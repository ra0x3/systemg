ARG RUST_IMAGE=rust:1.96.0-bullseye

FROM ${RUST_IMAGE} AS versions
WORKDIR /app
COPY . .
RUN sed -i '0,/^version = .*/s//version = "0.56.0"/' Cargo.toml \
 && cargo build --release --bin sysg \
 && cp target/release/sysg /sysg-0.56.0
RUN sed -i '0,/^version = .*/s//version = "0.56.1"/' Cargo.toml \
 && cargo build --release --bin sysg \
 && cp target/release/sysg /sysg-0.56.1

FROM sysg-usecase-base
RUN apt-get update \
 && apt-get install -y curl \
 && rm -rf /var/lib/apt/lists/*
COPY --from=versions /sysg-0.56.0 /release/pkg-0.56.0/sysg
COPY --from=versions /sysg-0.56.1 /release/pkg-0.56.1/sysg
RUN mkdir -p /release/latest \
 && printf '0.56.1\n' > /release/latest/VERSION \
 && case "$(uname -m)" in \
      x86_64) TARGET=x86_64-unknown-linux-gnu-debian ;; \
      aarch64) TARGET=aarch64-unknown-linux-gnu ;; \
      *) exit 1 ;; \
    esac \
 && tar -czf "/release/sysg-0.56.0-${TARGET}.tar.gz" -C /release/pkg-0.56.0 sysg \
 && tar -czf "/release/sysg-0.56.1-${TARGET}.tar.gz" -C /release/pkg-0.56.1 sysg
COPY scripts/index.sh /usecase/index.sh
COPY tests/docker/usecase/foreground-follow-reconnects/fgcap.py /usecase/fgcap.py
COPY tests/docker/usecase/live-reexec-upgrade/pipeline.yaml /usecase/pipeline.yaml
COPY tests/docker/usecase/live-reexec-upgrade/surface.yaml /usecase/surface.yaml
COPY tests/docker/usecase/live-reexec-upgrade/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh
ENTRYPOINT ["/usecase/run.sh"]
