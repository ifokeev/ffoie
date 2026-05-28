# Stage 1: builder
# Uses rust:1.95-slim (Debian-based) which runs as root — installs musl-tools cleanly.
# Build context is the workspace root (docker build -f crates/ffoie-chat-server/Dockerfile .)
# so COPY . . captures all workspace members needed for the cargo build.
FROM rust:1.95-slim AS builder

# Install musl cross-compile toolchain + pkg-config for any build scripts.
RUN apt-get update && \
    apt-get install --no-install-recommends -y musl-tools musl-dev ca-certificates pkg-config && \
    rm -rf /var/lib/apt/lists/*

# Add the musl target so rustup can link the stdlib for it.
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build

# Copy the entire workspace (filtered by .dockerignore at the workspace root).
COPY . .

# Build the server crate only (-p flag); the engine and wasm targets are excluded.
# panic=abort is already set in [profile.release] in the workspace Cargo.toml.
RUN cargo build --release --target x86_64-unknown-linux-musl -p ffoie-chat-server

# Strip debug symbols from the binary (belt-and-suspenders: workspace profile already sets strip="symbols").
RUN strip target/x86_64-unknown-linux-musl/release/ffoie-chat-server

# ──────────────────────────────────────────────────────────────────────────────
# Stage 2: runtime
# Alpine 3 is ~8 MB; final image with the static binary targets < 25 MB.
FROM alpine:3

# tini  — PID 1 init; forwards SIGTERM to the server for graceful shutdown.
# curl  — used by healthcheck probes outside the container (also useful for manual checks).
# ca-certificates — future-proof for outbound TLS (Postgres TLS, v1.1.x).
RUN apk add --no-cache tini curl ca-certificates

# Run as a non-root user (threat T-05-02).
RUN addgroup -S ffoie && adduser -S ffoie -G ffoie

# Copy the statically-linked binary from the builder stage.
COPY --from=builder --chown=ffoie:ffoie \
    /build/target/x86_64-unknown-linux-musl/release/ffoie-chat-server \
    /usr/local/bin/ffoie-chat-server

USER ffoie

# Chat server listens on 8080 by default (overridable via FFOIE_CHAT_BIND).
EXPOSE 8080

# tini as PID 1 so SIGTERM is forwarded properly.
ENTRYPOINT ["/sbin/tini", "--"]
CMD ["/usr/local/bin/ffoie-chat-server"]
