# rust-interceptor

Rust + Tokio implementation of the VP→FMS interceptor. **This is the primary
build** — see `../BENCHMARK.md` for the measured reasons.

Reads `../config.toml`, the same file the Java build uses.

## Build and run

```bash
# rustup.rs returns 403 on this network; Homebrew is the working path
brew install rust cmake openssl@3

cargo test                # 19 unit tests
cargo build --release     # ~40s, librdkafka builds from source via cmake
./target/release/vp-fms-interceptor ../config.toml
```

Produces a 3.4 MB static-ish binary with no runtime dependencies.

## Admin endpoints

Served by `src/admin.rs` on the address in `[admin] addr` (default
`127.0.0.1:9101`). Open any of these in a browser while the process is running:

| URL | Content-Type | Body |
|---|---|---|
| <http://127.0.0.1:9101/healthz> | `text/plain` | `ok` — liveness |
| <http://127.0.0.1:9101/> | `application/json` | every counter as one JSON object |
| <http://127.0.0.1:9101/metrics> | `text/plain; version=0.0.4` | Prometheus text format |

```bash
curl -s localhost:9101/healthz                        # ok
curl -s localhost:9101/ | python3 -m json.tool        # counters, pretty-printed
```

Three things to know:

- **Loopback-only by default, and there is no auth.** `127.0.0.1` means a browser
  on this host can reach it and another machine cannot. If you need it scraped
  remotely, change `config.toml` rather than the code, and put something in front
  of it — these endpoints expose operational internals next to a payments path.
- **A bind failure here does not stop the proxy.** `serve` logs and returns; you
  lose metrics, not availability. That is deliberate, and the function returns
  `()` so it *cannot* report failure upward.
- **Your browser will also request `/favicon.ico`.** Unknown paths fall through
  to the catch-all arm, so it gets served the JSON body. Harmless, but it is why
  you may see two hits per page load.

Counters worth reading first: `tee_dropped` (Kafka is degraded — and it proves
the hot path was not), `framer_desyncs` (wire framing does not match
`[framing]`), and `upstream_connect_failed`.

### Metrics reference

Every metric is prefixed `vp_interceptor_`. All are counters except the six
marked **gauge**. Timing metrics come as `_count` / `_sum` / `_max` triples, so
average = `sum / count` over whatever window you query; there are no histograms.

| Hop | Metric | Meaning |
|---|---|---|
| VP → listener | `conns_accepted` | VP connections accepted |
| | `conns_active` **gauge** | connections open now |
| | `conns_rejected` | closed immediately because `max_connections` was hit |
| | `accept_errors` | `accept()` itself failed (e.g. out of file descriptors) |
| | `conns_closed_clean` | both directions ended cleanly |
| | `conns_closed_error` | a direction ended with an IO error or panic |
| | `conns_idle_closed` | directions closed by `idle_timeout_ms` |
| | `pump_panics` | pump task panicked (a bug) |
| | `conn_duration_{count,sum_ms,max_ms}` | connection lifetime, accept → close |
| | `bytes_vp_to_fms` | bytes forwarded VP → FMS |
| listener → FMS | `upstream_connect_attempts` | FMS dials |
| | `upstream_connect_ok` | dials that succeeded |
| | `upstream_connect_refused` | dial returned an error |
| | `upstream_connect_timeout` | dial exceeded `connect_timeout_ms` |
| | `upstream_connect_failed` | refused + timeout |
| | `upstream_connect_{count,sum_us,max_us}` | connect time, successful dials only |
| | `upstream_conns_active` **gauge** | FMS connections open now |
| | `upstream_last_connect_ok` **gauge** | 1 if the latest dial succeeded, 0 if it failed |
| | `bytes_fms_to_vp` | bytes forwarded FMS → VP |
| | `rtt_{count,sum_us,max_us}` | request forwarded → matching response seen |
| | `rtt_unmatched` | requests closed with no response |
| tee | `tee_accepted` / `tee_dropped` | chunks queued / dropped because a shard queue was full |
| | `frames_emitted` | complete messages reassembled |
| | `framer_desyncs` | framing lost sync |
| listener → Kafka | `kafka_enqueued` / `kafka_enqueue_failed` | handed to librdkafka / its queue was full |
| | `kafka_delivered` / `kafka_delivery_failed` | broker ack / delivery failed |
| | `kafka_connected` **gauge** | 1 if at least one broker is UP |
| | `kafka_brokers_up` **gauge** | brokers UP |
| | `kafka_brokers_total` **gauge** | brokers librdkafka knows of |
| | `kafka_errors` | librdkafka error callbacks (transport, resolve, auth…) |
| | `kafka_all_brokers_down` | subset of `kafka_errors`: every broker down |

Two caveats on connection state:

- **FMS state is passive.** FMS is only dialed when a VP connection arrives, so
  `upstream_last_connect_ok` reflects the last real attempt and reads 0 before
  the first one. Read it together with `upstream_connect_attempts`.
- **Kafka state is refreshed every 5 s.** The producer sets
  `statistics.interval.ms=5000` to feed the `kafka_brokers_*` gauges. A
  `[kafka.properties]` entry overrides it; `0` disables the callback and the
  gauges stay at 0.

`[kafka.filter]` can drop exact-match frames from Kafka only. The current rule
is echo traffic: MTI `0800`/`0810` with `de70 = "301"`. Frames that cannot be
parsed for filtering are still published.

## Structure

| file | role |
|---|---|
| `src/main.rs` | accept loop, connection limit, graceful drain |
| `src/proxy.rs` | **the hot path** — two pump tasks per connection |
| `src/tee.rs` | bounded queue, shard workers, per-connection timing |
| `src/framing.rs` | length-prefix reassembler (unit-tested) |
| `src/parse.rs` | `key=value` field extraction (unit-tested) |
| `src/kafka.rs` | fire-and-forget producer, JSON envelope, encodings |
| `src/stats.rs` | atomic counters and gauges |
| `src/admin.rs` | `/metrics`, `/healthz` — hand-rolled, no web framework |

No web framework on purpose: this process sits inline on an authorization path,
and every dependency is one more thing that can allocate, block, or spawn threads
next to the hot path.

Every source file is annotated with `// LEARN:` (Rust semantics) and `// JAVA:`
(the equivalent, and the trade-off) comments. `RUST_EXPLAINED.md` in this
directory is the same material as prose. To read the code without them:
`grep -v "LEARN:\|JAVA:" src/tee.rs`

## The rule this code serves

> **Kafka must never be able to affect VP↔FMS.**

Check any change against that sentence first. The four mechanisms enforcing it
are documented in `../README.md`; the one that is easiest to break by accident is
in `src/proxy.rs` — bytes must reach the far socket **before** anything is handed
to the tee, and the tee call must never `.await`.

## One trap in `src/proxy.rs` worth knowing

`BytesMut::split().freeze()` is zero-copy, but the resulting `Bytes` keeps the
**whole** read allocation alive. A 122-byte message queued for Kafka pinned its
entire 16 KiB read buffer — 409 MB RSS at 256 connections before this was found.

Small reads are now copied into an exact-size `Bytes` instead (peak RSS 409 MB →
43 MB, no latency cost). If you touch that code, do not "optimise" the copy away
without re-measuring RSS under concurrency. See `../BENCHMARK.md`.

## Testing

```bash
cargo test                                    # unit tests only

# end-to-end -- run these from the PROJECT ROOT, not from here.
# See ../RUNBOOK.md and ../testkit/README.md.
(cd testkit && python3 mock_fms.py --port 8583 &)
./rust-interceptor/target/release/vp-fms-interceptor config.toml &
./scripts/status.sh                           # exit 0 = healthy
```
