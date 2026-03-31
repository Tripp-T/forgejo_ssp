
FROM rust:1.94-slim AS builder
RUN apt-get update && apt-get install -y pkg-config openssl libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /usr/src/app
COPY . .
RUN cargo build --release

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/forgejo_ssp /usr/local/bin/forgejo_ssp

EXPOSE 3000
ENV RUST_LOG="info"
CMD ["forgejo_ssp"]