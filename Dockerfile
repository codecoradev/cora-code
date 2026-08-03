# Cora Code — Docker image for self-hosted deployment.
#
# Multi-stage build:
#   1. Builder: rust:1.85 (glibc) — compile with tree-sitter + LTO
#   2. Runtime: debian:bookworm-slim — minimal glibc base, ~15MB final image
#
# Usage:
#   docker build -t cora .
#   docker run --rm cora --version
#   docker run --rm -e CORA_API_KEY=sk-xxx -v $(pwd):/workspace cora review --staged
#   docker run --rm cora mcp serve          # MCP server (stdio)

# ── Builder ────────────────────────────────────────────────────────────
FROM rust:1.85-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies — copy only Cargo files first.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release --features tree-sitter 2>/dev/null || true

# Copy real source and rebuild (cached deps reused).
COPY src/ src/
RUN touch src/main.rs && cargo build --release --features tree-sitter

# ── Runtime ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/cora /usr/local/bin/cora

# Default workdir for review targets.
WORKDIR /workspace

ENTRYPOINT ["cora"]
