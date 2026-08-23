# Stage 1: Compilador estático
FROM rust:1.80-slim as builder

WORKDIR /usr/src/dtn-core
RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

COPY . .

# Compilar binarios totalmente estáticos con strip
RUN cargo build --release --target x86_64-unknown-linux-musl --bin dtnd --bin dtn-cli

# Stage 2: Contenedor runtime ultra-ligero (Scratch)
FROM scratch

# Copiar binarios compilados
COPY --from=builder /usr/src/dtn-core/target/x86_64-unknown-linux-musl/release/dtnd /dtnd
COPY --from=builder /usr/src/dtn-core/target/x86_64-unknown-linux-musl/release/dtn-cli /dtn-cli

EXPOSE 4556/udp

ENTRYPOINT ["/dtnd"]
