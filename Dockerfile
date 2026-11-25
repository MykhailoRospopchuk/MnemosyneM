# syntax=docker/dockerfile:1

FROM rust:1-trixie AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    g++ \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency compilation
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --bin m_nemosyne_m \
    && rm -rf src

# Build actual application
COPY src ./src
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --bin m_nemosyne_m

FROM debian:trixie-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    libstdc++6 \
    sqlite3 \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN mkdir -p /data

COPY --from=builder /app/target/release/m_nemosyne_m /usr/local/bin/m_nemosyne_m

ENV DB_PATH=/data/mnemosynem.db \
    FASTEMBED_CACHE_DIR=/data/fastembed_cache \
    RUST_LOG=info,m_nemosyne_m=info,teloxide=warn

CMD ["m_nemosyne_m"]
