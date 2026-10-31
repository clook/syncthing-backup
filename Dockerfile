# syntax=docker/dockerfile:1
# The build stage runs on the CI machine's own architecture and cross-compiles for the target
# (tonistiigi/xx), instead of compiling under QEMU emulation. See docs/decisions/0011.
FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.10.0 AS xx

FROM --platform=$BUILDPLATFORM rust:1.99.0-alpine3.24 AS builder
COPY --from=xx / /
RUN apk add --no-cache clang lld
ARG TARGETPLATFORM
# Target C toolchain and libc, for crates with C code (ring, used by rustls).
RUN xx-apk add --no-cache musl-dev gcc

WORKDIR /usr/src/app
# Dependencies first, in their own layer: rebuilt only when Cargo.toml or Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && xx-cargo build --release --locked
COPY src ./src
RUN touch src/main.rs && xx-cargo build --release --locked \
 && cp "target/$(xx-cargo --print-target-triple)/release/syncthing-backup" /syncthing-backup \
 && xx-verify --static /syncthing-backup


FROM alpine:3.24
# No ca-certificates: rustls ships its own roots, and Syncthing is reached over plain HTTP.
# No USER: run with Syncthing's uid/gid (see README), so received files can be removed.
COPY --from=builder /syncthing-backup /app/syncthing-backup
# CMD, not ENTRYPOINT: `docker run <image> sh` must open a shell, not start a second porter
# (two workers would break the single-worker invariant).
CMD ["/app/syncthing-backup"]
