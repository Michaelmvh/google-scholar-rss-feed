# ---- Build stage ----
FROM rust:1-slim-bookworm AS builder
WORKDIR /app

# Pre-build dependencies against a stub so they are cached in their own layer
# and only recompiled when Cargo.toml / Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build the actual application.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- Runtime stage ----
FROM debian:bookworm-slim
WORKDIR /app

# TLS trust for HTTPS calls to the OpenAlex API is provided by rustls' bundled
# Mozilla roots, so no ca-certificates package is required.
COPY --from=builder /app/target/release/google-scholar-rss-feed /usr/local/bin/google-scholar-rss-feed

# Bake the feed definitions into the image so the repo is the single source of
# truth: edit feeds.toml, push, and the NAS just pulls the updated image. A host
# mount at /config/feeds.toml can still override this without a rebuild.
COPY feeds.toml /config/feeds.toml

EXPOSE 3005

# feeds.toml is mounted at /config/feeds.toml (see docker-compose.yml).
ENV GSRF_CONFIG=/config/feeds.toml

# Bind to all interfaces so the feed is reachable outside the container.
CMD ["google-scholar-rss-feed", "0.0.0.0:3005"]
