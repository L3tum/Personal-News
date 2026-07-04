# syntax=docker/dockerfile:1

# Build with musl for a fully static binary (no dynamic libc dependencies)
FROM rust:1.96 AS builder

ENV PATH="/root/.cargo/bin:${PATH}"

# Install musl target and the musl-gcc compiler needed by ring
RUN rustup target add x86_64-unknown-linux-musl \
    && apt-get update && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy source
COPY . .

# Build with caching and musl toolchain
RUN --mount=type=cache,id=personalnews-cargo-registry,target=/root/.cargo/registry \
    --mount=type=cache,id=personalnews-target,target=/build/target \
    cargo build --target x86_64-unknown-linux-musl --release && \
    cp target/x86_64-unknown-linux-musl/release/rss_digest /rss_digest

# Runtime: completely static binary with no external dependencies
FROM scratch

COPY --from=builder /rss_digest /rss_digest

# Environment variables — see .env.example for all options
ENV FRESHRSS_URL=http://freshrss:8080
ENV QDRANT_URL=http://qdrant:6333
ENV OLLAMA_URL=http://ollama:11434
ENV CRON_TIME=06:00
ENV CRON_TIMEZONE=UTC

# No exposed ports — the container runs as a cron-like scheduler
CMD ["/rss_digest"]
