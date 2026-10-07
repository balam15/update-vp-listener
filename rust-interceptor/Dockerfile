FROM repo.maybank.co.id/docker-proxy/rust:1.97-alpine AS build

WORKDIR /src

ARG EXTRA_CA_CERT=config.toml
COPY ${EXTRA_CA_CERT} /tmp/extra-ca-cert

RUN if grep -q "BEGIN CERTIFICATE" /tmp/extra-ca-cert; then \
        cat /tmp/extra-ca-cert >> /etc/ssl/certs/ca-certificates.crt; \
    fi; \
    rm -f /tmp/extra-ca-cert

RUN apk add --no-cache \
    ca-certificates \
    musl-dev \
    pkgconfig \
    openssl-dev \
    openssl-libs-static \
    curl-dev \
    zlib-dev \
    build-base

COPY rust-interceptor/Cargo.toml rust-interceptor/Cargo.lock ./
COPY rust-interceptor/src ./src

RUN cargo build --release --locked \
    && strip target/release/vp-fms-interceptor


FROM repo.maybank.co.id/docker-proxy/alpine:3.21

ARG EXTRA_CA_CERT=config.toml
COPY ${EXTRA_CA_CERT} /tmp/extra-ca-cert

RUN if grep -q "BEGIN CERTIFICATE" /tmp/extra-ca-cert; then \
        cat /tmp/extra-ca-cert >> /etc/ssl/certs/ca-certificates.crt; \
    fi; \
    rm -f /tmp/extra-ca-cert

RUN apk add --no-cache ca-certificates libcurl zlib openssl libgcc \
    && adduser --system --uid 65532 --no-create-home interceptor \
    && mkdir -p /etc/vp-fms \
    && chmod 755 /etc/vp-fms

COPY --from=build /src/target/release/vp-fms-interceptor /usr/local/bin/vp-fms-interceptor

USER 65532:65532

EXPOSE 9100 9101

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -qO- http://127.0.0.1:9101/healthz || exit 1

ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/vp-fms-interceptor"]
CMD ["/etc/vp-fms/config.toml"]