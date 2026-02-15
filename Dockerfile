FROM rust:1.93-alpine AS builder

RUN apk add --no-cache musl-dev gcc

WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release
COPY . .
RUN touch src/main.rs && cargo build --release


FROM alpine:latest

RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/app/target/release/syncthing-backup .

ENV REDIS_HOST=localhost
ENV SYNCTHING_PORT=8384

CMD ["./syncthing-backup"]
