# syntax=docker/dockerfile:1

FROM rust:1-trixie AS builder
WORKDIR /app

# Copy source and build the release binary.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim AS runtime

# TLS certificates are needed for fetching HTTPS pages.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root user.
RUN useradd --system --uid 10001 --create-home --home-dir /home/rsspls rsspls

WORKDIR /work
COPY --from=builder /app/target/release/rsspls /usr/local/bin/rsspls

USER rsspls
ENTRYPOINT ["rsspls"]
CMD ["--help"]
