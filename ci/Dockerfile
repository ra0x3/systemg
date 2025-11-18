# Stage 1: Build
FROM lukemathwalker/cargo-chef:latest-rust-1.90.0 AS chef

WORKDIR /build/

# hadolint ignore=DL3008
RUN apt-get update -y && \
    apt-get install -y --no-install-recommends \
    build-essential \
    clang \
    cmake \
    gcc \
    git \
    libclang-dev \
    lld \
    llvm \
    libssl-dev \
    pkg-config \
    && apt-get clean

FROM chef AS planner
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: build binaries
FROM chef AS builder
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV RUSTFLAGS="-C strip=symbols -C opt-level=z -C codegen-units=1 -C lto=yes"
COPY --from=planner /build/recipe.json recipe.json

COPY . .
RUN cargo build --release --all-targets

# Stage 3: Run
FROM ubuntu:22.04 AS run

WORKDIR /root/

RUN DEBIAN_FRONTEND=noninteractive apt-get update -y \
    && apt-get install -y --no-install-recommends \
    tzdata \
    ca-certificates \
    wget \
    && apt-get autoremove -y \
    && apt-get clean -y

COPY --from=builder /build/target/release/sysg .
COPY --from=builder /build/target/release/sysg.d .
