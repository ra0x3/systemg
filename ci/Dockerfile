# Stage 1: Build
FROM --platform=$BUILDPLATFORM tonistiigi/xx AS xx
FROM --platform=$BUILDPLATFORM rust:1.90.0-bookworm AS chef

ARG TARGETPLATFORM
RUN cargo install cargo-chef
WORKDIR /build/

COPY --from=xx / /

# hadolint ignore=DL3008
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    lld \
    clang \
    libclang-dev \
    && xx-apt-get update  \
    && xx-apt-get install -y libc6-dev g++ binutils \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*


FROM chef AS planner
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
COPY . .
RUN cargo chef prepare --recipe-path recipe.json


FROM chef AS builder
ARG DEBUG_SYMBOLS=false
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_PROFILE_RELEASE_DEBUG=$DEBUG_SYMBOLS
COPY --from=planner /build/recipe.json recipe.json
RUN echo $CARGO_PROFILE_RELEASE_DEBUG
# Build our project dependencies, not our application!
RUN \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/build/target \
  xx-cargo chef cook --release --recipe-path recipe.json
# Up to this point, if our dependency tree stays the same,
# all layers should be cached.
COPY . .
# build application
# note this puts the builds outside of the cache dirs so the run image can copy them
RUN \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/build/target \
  xx-cargo build --release \
    && xx-verify ./target/$(xx-cargo --print-target-triple)/release/sysg \
    && cp ./target/$(xx-cargo --print-target-triple)/release/sysg /root/sysg \
    && cp ./target/$(xx-cargo --print-target-triple)/release/sysg.d /root/sysg.d

# Stage 3: Run
FROM debian:bookworm-slim AS run

WORKDIR /root/

RUN apt-get update -y \
    && apt-get install -y --no-install-recommends ca-certificates \
    # Clean up
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /root/sysg .
COPY --from=builder /root/sysg.d .
