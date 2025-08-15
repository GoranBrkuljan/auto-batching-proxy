# --- Build stage
FROM rust:1.89 as builder
WORKDIR /app

# Cache deps
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs \
    cargo build --release \
    rm -rf src

# Copy real sources and build
COPY src ./src
RUN cargo build --release

# --- Runtime stage
FROM ubuntu:24.10
RUN useradd -m app
WORKDIR /home/app
COPY --from=builder /app/target/release/auto-batching-proxy /usr/local/bin/auto-batching-proxy
USER app
EXPOSE 3000
ENV RUST_LOG=info
CMD ["/usr/local/bin/auto-batching-proxy"]
