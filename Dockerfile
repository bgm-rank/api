FROM rust:slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# 编译 sqlx-cli
RUN cargo install sqlx-cli --no-default-features --features rustls,postgres

# 缓存依赖层
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main(){}" > src/main.rs && cargo build --release && rm src/main.rs

COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/bgm-rank-api /usr/local/bin/bgm-rank-api
EXPOSE 3000
CMD ["bgm-rank-api"]
