ARG RUST_IMAGE=rust:1.96.0-bullseye
ARG RUNTIME_IMAGE=ubuntu:22.04
FROM ${RUST_IMAGE} AS builder

WORKDIR /app
COPY . .
RUN cargo build --release --bin sysg

FROM ${RUNTIME_IMAGE}

RUN apt-get update && apt-get install -y \
    procps \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/sysg /usr/local/bin/sysg
RUN chmod +x /usr/local/bin/sysg

COPY tests/docker/zombie-repro/sysg.config.yaml /repro/sysg.config.yaml
COPY tests/docker/zombie-repro/run_repro.sh /repro/run_repro.sh
RUN chmod +x /repro/run_repro.sh

WORKDIR /repro
ENTRYPOINT ["/repro/run_repro.sh"]
