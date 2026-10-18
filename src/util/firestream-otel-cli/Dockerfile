# Multi-stage build for otel-cli (Rust port).
# Build:  docker build -t otel-cli:dev .
# Run:    docker run --rm otel-cli:dev status

FROM rust:1.82-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/otel-cli /usr/local/bin/otel-cli
ENTRYPOINT ["otel-cli"]
CMD ["--help"]
