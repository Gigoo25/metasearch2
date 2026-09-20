FROM lukemathwalker/cargo-chef:latest-rust-1.93-slim AS chef
WORKDIR /app

RUN apt-get update && \
  apt-get install -y --no-install-recommends \
  build-essential=12.12 \
  cmake=3.31.6-2 \
  perl=5.40.1-6+deb13u1 \
  pkg-config=1.8.1-4 \
  clang=1:19.0-63 \
  libclang-dev=1:19.0-63 \
  llvm=1:19.0-63 \
  golang=2:1.24~2 \
  git=1:2.47.3-0+deb13u1 \
  ca-certificates=20250419 \
  curl=8.14.1-2+deb13u5 \
  && rm -rf /var/lib/apt/lists/*

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
ENV RUST_BACKTRACE=1
RUN cargo build --release

FROM debian:trixie-slim AS runtime
WORKDIR /app
RUN apt-get update && \
  apt-get install -y --no-install-recommends \
  kiwix-tools=3.7.0-1.1 \
  curl=8.14.1-2+deb13u5 \
  && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/metasearch /usr/local/bin/metasearch
COPY config-container.toml /usr/local/bin/config.toml
COPY kiwix-empty-library.xml /usr/local/share/kiwix-empty-library.xml
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ARG CONFIG
ENV CONFIG=${CONFIG}
EXPOSE 28019
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
