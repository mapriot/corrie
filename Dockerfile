# syntax=docker/dockerfile:1
#
# Multi-stage build for corrie. The only system dependency is GDAL (SQLite is
# compiled in). Debian bookworm ships GDAL 3.6, within the 3.5–3.12 range the
# `gdal` crate has prebuilt bindings for.
#
#   docker build -t corrie .
#   docker run --rm -v "$PWD:/data" corrie tile config.json contours.pmtiles

# ── build ────────────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS build

# GDAL headers + a C toolchain (gdal-sys, bundled SQLite).
RUN apt-get update && apt-get install -y --no-install-recommends \
        libgdal-dev \
        build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release --locked && strip target/release/corrie

# ── runtime ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# GDAL runtime library only (same 3.6 the binary was built against).
RUN apt-get update && apt-get install -y --no-install-recommends \
        libgdal32 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/corrie /usr/local/bin/corrie

# Bind-mount your data here; relative paths (and a config's sources) resolve inside /data.
WORKDIR /data
ENTRYPOINT ["corrie"]
