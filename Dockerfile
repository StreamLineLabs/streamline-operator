FROM rust:1.75-slim-bookworm AS builder

WORKDIR /build
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 1000 operator

COPY --from=builder /build/target/release/streamline-operator /usr/local/bin/streamline-operator

USER operator
ENTRYPOINT ["streamline-operator"]
