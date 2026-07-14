ARG RUST_IMAGE=rust:1.96.0-bullseye
ARG RUNTIME_IMAGE=ubuntu:22.04
FROM ${RUST_IMAGE} AS builder

WORKDIR /app
COPY . .
RUN sed -i '0,/^version = ".*"/s//version = "0.0.1"/' Cargo.toml \
    && cargo build --release --bin sysg \
    && cp target/release/sysg /sysg-old
RUN sed -i '0,/^version = ".*"/s//version = "0.0.2"/' Cargo.toml \
    && cargo build --release --bin sysg \
    && cp target/release/sysg /sysg-new

FROM ${RUNTIME_IMAGE}

RUN apt-get update && apt-get install -y \
    procps \
    python3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /sysg-old /usr/local/bin/sysg-old
COPY --from=builder /sysg-new /usr/local/bin/sysg-new
RUN chmod +x /usr/local/bin/sysg-old /usr/local/bin/sysg-new

COPY tests/docker/repro/stale-daemon/sysg.config.yaml /repro/sysg.config.yaml
COPY tests/docker/repro/stale-daemon/run_repro.sh /repro/run_repro.sh
RUN chmod +x /repro/run_repro.sh

WORKDIR /repro
ENTRYPOINT ["/repro/run_repro.sh"]
