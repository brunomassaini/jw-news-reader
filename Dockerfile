FROM rust:1.85-slim AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Cache dependencies by building a dummy binary first.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release
RUN rm -f target/release/deps/jw_news_reader_api*

# Build the real source.
COPY src ./src
RUN touch src/main.rs
RUN cargo build --release

# ── Runtime image ──────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/jw-news-reader-api /usr/local/bin/jw-news-reader-api

EXPOSE 8000

CMD ["jw-news-reader-api"]
