# Builder image to compile systemg for Linux
FROM rust:1.75 AS builder

WORKDIR /app
COPY . .
RUN cargo build --release

# Extract the binary
FROM scratch AS binary
COPY --from=builder /app/target/release/sysg /sysg