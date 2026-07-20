ARG RUST_IMAGE=rust:1.96.0-bullseye

FROM ${RUST_IMAGE} AS oldbuilder
WORKDIR /app
COPY . .
RUN sed -i '0,/^version = .*/s//version = "0.0.1"/' Cargo.toml \
 && cargo build --release --bin sysg \
 && cp target/release/sysg /sysg-old

FROM sysg-usecase-base

COPY --from=oldbuilder /sysg-old /usr/local/bin/sysg-old
RUN chmod +x /usr/local/bin/sysg-old

COPY tests/docker/usecase/restart-recycle-refused/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-recycle-refused/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
