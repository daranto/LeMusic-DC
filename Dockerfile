# syntax=docker/dockerfile:1

# --- Abhängigkeiten vorbauen (gecacht, solange Cargo.lock gleich bleibt) ---
FROM rust:1-slim-bookworm AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin lemusic

# --- Statisches ffmpeg (deutlich kleiner als das Debian-Paket) ---
FROM mwader/static-ffmpeg:9.0.2-1 AS ffmpeg

# --- Laufzeit-Image ---
FROM python:3.13-slim-bookworm AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data lemusic \
    && mkdir -p /data /opt/yt-dlp \
    && chown lemusic /data /opt/yt-dlp

# yt-dlp in eigenem venv, das dem Bot-Nutzer gehört → Update ohne Root möglich.
# Das Extra „deno“ bringt die JS-Runtime mit, die YouTube inzwischen braucht.
USER lemusic
RUN python -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir --upgrade "yt-dlp[default,deno]"
ENV PATH="/opt/yt-dlp/bin:${PATH}" \
    HOME=/data \
    DATA_DIR=/data \
    WEB_BIND=0.0.0.0:8080 \
    YTDLP_AUTO_UPDATE=true

COPY --from=ffmpeg /ffmpeg /usr/local/bin/ffmpeg
COPY --from=builder /build/target/release/lemusic /usr/local/bin/lemusic
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080
# Großzügige Startphase: Der Entrypoint aktualisiert vorher yt-dlp.
HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
    CMD ["/usr/local/bin/lemusic", "healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/entrypoint.sh"]
