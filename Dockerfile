FROM lukemathwalker/cargo-chef:latest-rust-1.94-slim AS chef
WORKDIR /usr/src/app
# Install system dependencies needed for both planning and building
RUN apt-get update && apt-get install -y pkg-config openssl libssl-dev && rm -rf /var/lib/apt/lists/*

# Planner stage: Prepare the recipe for caching dependencies
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Builder stage
FROM chef AS builder
COPY --from=planner /usr/src/app/recipe.json recipe.json
# Build dependencies - this layer is cached unless recipe.json changes
RUN cargo chef cook --release --recipe-path recipe.json
# Build the application
COPY . .
RUN cargo build --release

# Runtime stage
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/forgejo_ssp /usr/local/bin/forgejo_ssp
EXPOSE 3000
ENV RUST_LOG="info"
CMD ["forgejo_ssp"]