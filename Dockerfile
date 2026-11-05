# WeCal — build multi-stage: cargo-chef para cache de deps + binário oats.
# Estágio 1 — chef: prepara o "plano" de dependências (recipe.json)
FROM lukemathwalker/cargo-chef:latest-rust-1.89 AS chef
WORKDIR /app

# Estágio 2 — planner: gera o recipe.json a partir do Cargo.toml
FROM chef AS planner
COPY Cargo.toml ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# Estágio 3 — builder: compila só as deps (cacheável) e depois o binário
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml ./
COPY src ./src
COPY templates ./templates
COPY migrations ./migrations

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev cmake \
 && rm -rf /var/lib/apt/lists/*

RUN cargo build --release

# ---------------------------------------------------------------------------
# Estágio 4 — runtime: só o binário final em imagem leve.
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/wecal ./wecal

ENV PORT=3000
EXPOSE 3000

CMD ["./wecal"]
