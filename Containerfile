# syntax=docker/dockerfile:1

FROM rust:1-trixie AS builder
WORKDIR /app

# Copy source and build the release binary.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates rsync rclone  \
    && rm -rf /var/lib/apt/lists/*

ENV XDG_CONFIG_HOME=/rsspls/
ENV XDG_CACHE_HOME=/rsspls/cache

COPY --from=builder /app/target/release/rsspls /usr/local/bin/rsspls

ENTRYPOINT ["rsspls"]
