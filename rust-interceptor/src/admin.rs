// ============================================================================
// admin.rs -- tiny hand-rolled HTTP server for /metrics, /healthz, /.
//
// Comment tags: // LEARN: Rust semantics   // JAVA: the comparison
// Everything else is original design rationale. See ../RUST_EXPLAINED.md Part 9.
// ============================================================================

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::stats::Stats;

/// Minimal admin surface: `/metrics` (Prometheus text), `/healthz`, `/` (JSON).
///
/// Hand-rolled rather than pulling in a web framework. This process sits inline
/// on an authorization path, so every dependency is one more thing that can
/// allocate, block, or spawn threads next to the hot path.
//
// LEARN: this rationale is a genuine Rust-ecosystem consideration. Adding `axum`
//   or `actix-web` pulls in hyper, tower, http and a hundred transitive crates,
//   some of which spawn their own threads. For three endpoints returning static
//   text, 40 lines of hand-rolled HTTP is the right call.
// LEARN: `addr: String` BY VALUE -- takes ownership. That is why main.rs had to
//   write `cfg.admin.addr.clone()` at the call site.
pub async fn serve(addr: String, stats: Arc<Stats>) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            // LEARN: BIND FAILURE RETURNS INSTEAD OF PROPAGATING. Losing metrics
            //   must not stop the proxy. Note the function returns `()`, so it
            //   STRUCTURALLY CANNOT report failure to its caller -- the design
            //   decision is in the signature, same trick as Tee::offer.
            tracing::error!(%addr, error = %e, "admin bind failed; continuing without metrics");
            return;
        }
    };
    tracing::info!(%addr, "admin listening (/metrics, /healthz)");

    loop {
        // LEARN: `(mut sock, _)` binds the socket as mutable and discards the
        //   peer address entirely. The `_` does not just ignore the value -- it
        //   DROPS it immediately.
        let (mut sock, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "admin accept failed");
                continue;
            }
        };
        let stats = Arc::clone(&stats);
        tokio::spawn(async move {
            // LEARN: `[0u8; 1024]` is a FIXED-SIZE ARRAY of type [u8; 1024],

            // Helm cart for rust
            // service.yaml
            // deployment.yaml
            // alphine -> os
            // Logging -> mouting, volume existing
            // 512 MB
            // 0.5 core
            //
            //   allocated ON THE STACK (strictly: inside the task's state-machine
            //   struct). THE SIZE IS PART OF THE TYPE.
            // JAVA: `new byte[1024]` is ALWAYS a heap allocation with a header
            //   and a length field, and always becomes garbage.
            // LEARN: this is a small but very representative example of "cheap
            //   memory" -- in Rust, FIXED-SIZE BUFFERS ARE FREE. In Java, every
            //   array is a heap object.
            let mut buf = [0u8; 1024];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            // LEARN: `String::from_utf8_lossy` returns a Cow<str> -- "Clone on
            //   Write": either a BORROW of the original bytes (if already valid
            //   UTF-8, NO ALLOCATION) or an owned String (if replacement chars
            //   had to be inserted). For a valid ASCII HTTP request this BORROWS
            //   THE STACK BUFFER with no allocation at all.
            // JAVA: HAS NO Cow. `new String(bytes, UTF_8)` always allocates and
            //   always copies, even when the input was already valid. Cow is a
            //   genuinely useful type with no Java counterpart -- it lets an API
            //   be zero-copy in the common case without changing its signature.
            let req = String::from_utf8_lossy(&buf[..n]);
            // LEARN: parse "GET /metrics HTTP/1.1" by taking the second
            //   whitespace-separated token. Option<&str>, defaulting to "/".
            //   Two combinators, and a MALFORMED REQUEST CANNOT PANIC.
            // LEARN: deliberately minimal -- one read, first packet only, no
            //   Content-Length handling, no keep-alive. Adequate for a
            //   localhost-bound admin endpoint, and the doc comment is honest
            //   about it.
            let path = req.split_whitespace().nth(1).unwrap_or("/");

            // LEARN: `match` on a &str against literal patterns, returning a
            //   TUPLE destructured into two bindings.
            // JAVA: Java 21's pattern-matching switch is now equivalent; before
            //   14 you needed a statement switch with mutable locals.
            // LEARN: BOTH ARMS MUST HAVE THE SAME TYPE, which is why "ok\n" gets
            //   .to_string() to match the String returned by prometheus(). The
            //   compiler catches the mismatch.
            let (content_type, body) = match path {
                "/healthz" => ("text/plain", "ok\n".to_string()),
                "/metrics" => ("text/plain; version=0.0.4", prometheus(&stats)),
                _ => ("application/json", json(&stats)),
            };

            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

fn prometheus(stats: &Stats) -> String {
    let mut out = String::with_capacity(2048);
    // LEARN: consumes the Vec by value and destructures each tuple IN THE LOOP
    //   PATTERN.
    for (name, value) in stats.snapshot() {
        // LEARN: with_capacity(2048) pre-sizes correctly, but then each format!
        //   allocates a TEMPORARY String that is immediately copied and dropped
        //   -- 42 pointless allocations per scrape. `write!(out, "...")` with
        //   std::fmt::Write would append in place with zero temporaries.
        //   It runs once per scrape interval so it genuinely does not matter;
        //   it is worth noticing as the general idiom: FORMAT! ALLOCATES,
        //   WRITE! APPENDS.
        let kind = if crate::stats::GAUGES.contains(&name) {
            "gauge"
        } else {
            "counter"
        };
        out.push_str(&format!("# TYPE vp_interceptor_{name} {kind}\n"));
        out.push_str(&format!("vp_interceptor_{name} {value}\n"));
    }
    perf_test_metrics(stats, &mut out);
    out
}

/// Performance-test (TPS) metrics, exposed with their literal public names (no
/// `vp_interceptor_` prefix) so the load-test PromQL can reference them directly,
/// e.g. `rate(vp_rest_success_total[1m])`.
///
/// "rest" is the Kafka leg; "iso8583" is the FMS leg. All seven are monotonic
/// counters. Only `vp_reports_received_total` is a dedicated counter; the others
/// are derived here from the counters already maintained on the hot and tee
/// paths, so adding them changes no existing behaviour.
///
///   vp_reports_received_total  reports received from VP (VP -> FMS frames)
///   vp_rest_requests_total     publish attempts to Kafka (enqueued + enqueue-failed)
///   vp_rest_success_total      frames delivered to a broker
///   vp_rest_failed_total       enqueue failures + delivery failures
///   vp_iso8583_requests_total  requests forwarded to FMS (= reports received)
///   vp_iso8583_success_total   FMS responses matched to a request
///   vp_iso8583_failed_total    requests that closed without a matching response
///
/// Note: the iso8583 success/failed split comes from the request/response
/// pairing, so it requires `[timing] enabled = true` and `pair_request_response
/// = true` (both on by default). With timing off, requests still counts but
/// success/failed stay at 0.
fn perf_test_metrics(stats: &Stats, out: &mut String) {
    let reports = Stats::get(&stats.vp_reports_received);

    let enqueued = Stats::get(&stats.kafka_enqueued);
    let enqueue_failed = Stats::get(&stats.kafka_enqueue_failed);
    let delivered = Stats::get(&stats.kafka_delivered);
    let delivery_failed = Stats::get(&stats.kafka_delivery_failed);

    let rest_requests = enqueued + enqueue_failed;
    let rest_success = delivered;
    let rest_failed = enqueue_failed + delivery_failed;

    let iso_requests = reports;
    let iso_success = Stats::get(&stats.rtt_count);
    let iso_failed = Stats::get(&stats.rtt_unmatched);

    for (name, help, value) in [
        (
            "vp_reports_received_total",
            "Reports received from VP (one per VP->FMS frame reassembled)",
            reports,
        ),
        (
            "vp_rest_requests_total",
            "Publish attempts to Kafka (enqueued + enqueue-failed)",
            rest_requests,
        ),
        (
            "vp_rest_success_total",
            "Frames delivered to a Kafka broker",
            rest_success,
        ),
        (
            "vp_rest_failed_total",
            "Kafka enqueue failures plus delivery failures",
            rest_failed,
        ),
        (
            "vp_iso8583_requests_total",
            "Requests forwarded to FMS (equals reports received from VP)",
            iso_requests,
        ),
        (
            "vp_iso8583_success_total",
            "FMS responses matched to a forwarded request",
            iso_success,
        ),
        (
            "vp_iso8583_failed_total",
            "Requests that closed without a matching FMS response",
            iso_failed,
        ),
    ] {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} counter\n"));
        out.push_str(&format!("{name} {value}\n"));
    }
}

fn json(stats: &Stats) -> String {
    // LEARN: an ITERATOR CHAIN. The Java Streams equivalent is near-identical:
    //     stats.snapshot().entrySet().stream()
    //          .map(e -> "\"" + e.getKey() + "\":" + e.getValue())
    //          .collect(Collectors.joining(","));
    //   DIFFERENCES:
    //     .into_iter() CONSUMES the Vec, yielding owned items. Java streams
    //       always borrow.
    //     .map(|(k, v)| ...) DESTRUCTURES THE TUPLE IN THE CLOSURE PARAMETER.
    //       Java needs e.getKey() / e.getValue().
    //     .collect() is GENERIC OVER THE TARGET COLLECTION, chosen by the type
    //       annotation `Vec<String>` on the left -- return-type-driven dispatch.
    //       Java requires you to name a Collector explicitly.
    //     Rust's chain COMPILES TO A SINGLE LOOP with no intermediate objects.
    //       Java's stream creates a pipeline of Spliterator objects and virtual
    //       accept() calls, which the JIT often but not always flattens.
    let fields: Vec<String> = stats
        .snapshot()
        .into_iter()
        .map(|(k, v)| format!("\"{k}\":{v}"))
        .collect();
    // LEARN: `{{` and `}}` are ESCAPED LITERAL BRACES, so this produces {...}.
    // JAVA: same escaping rule as MessageFormat.
    // LEARN: hand-rolling JSON is safe HERE only because every key is a fixed
    //   identifier and every value is a u64. Contrast kafka.rs, where a test
    //   exists specifically to stop anyone "optimising" the envelope into string
    //   concatenation -- that data is attacker-influenced.
    format!("{{{}}}\n", fields.join(","))
}
