# 0011 — Docker image: cross-compiled build, smaller image, leaner publish workflow
- Status: accepted
- Date: 2026-10-31
- Type: technical
- Decided by: proposed by Claude, approved by the user: "OK go ahead with the proposed fixes"
  (items 1–5 and 7–10 of the proposal; 6, a scratch base image, and 11, gating publishing on CI, were not
  taken).

## Context
- Docker Build and Publish took 36 and 34 min on 2026-02-16 (`gh run list --workflow
  docker-publish.yml`, retrieved 2026-10-31). The arm64 image was built under QEMU, so the whole Rust
  compile ran emulated. That this is the main cost is **unverified**: the logs of those runs have expired.
- The workflow built and pushed the `builder` stage for both platforms as `:build-cache`, before
  building the real image.
- No `.dockerignore`: a local build sent the whole working copy (`target/` was 6.2 GB, 4.1 GB of it e2e
  data, measured with `du -sh`), and `COPY . .` put the host's `target/` into the build stage.
- `rust:1.93-alpine` was last updated on 2026-02-13 and is based on Alpine 3.23 (Docker Hub tags API,
  retrieved 2026-10-31).

## Options considered
- Cross-compile with `tonistiigi/xx` (chosen: standard BuildKit helper, keeps `rust:alpine` and musl).
- `cargo-zigbuild`: also works, adds Zig to the build; kept as the fallback.
- Keep QEMU and only cache better: compile time under emulation stays.
- Scratch or distroless runtime image: smaller, but no shell to inspect a running pod (not taken).

## Decision
1. Build stage on `$BUILDPLATFORM`, cross-compiling to the target with `xx-cargo`; `xx-verify --static`
   checks the binary. The runtime stage only copies the binary, so the workflow needs no QEMU.
2. `.dockerignore` lets only `Cargo.toml`, `Cargo.lock` and `src/` reach the build.
3. Release profile: `strip = true`, `lto = "thin"`, `codegen-units = 1`. `panic` stays `unwind`, because
   the worker catches a mover panic (`run_worker` in `src/main.rs`).
4. Runtime `alpine:3.24` without `ca-certificates` (reqwest uses `rustls-tls`, which bundles
   `webpki-roots`, in `Cargo.lock`; Syncthing is reached over HTTP). `CMD ["/app/syncthing-backup"]` (amended before merge after review 0003: `ENTRYPOINT` made `docker run <image> sh` start a second porter).
   No `USER` in the image: the porter must run with Syncthing's uid/gid, set by the deployment (README).
5. Pinned images: `rust:1.99.0-alpine3.24` (2026-10-04) and `tonistiigi/xx:1.10.0` (2026-10-12), the
   newest releases at least two weeks old. CI (`ci.yml`) already builds with the latest stable Rust.
6. Workflow: no builder image push; `concurrency` cancels a superseded build; actions at their current
   majors (`setup-buildx` v4, `metadata-action` v6, `login` v4, `build-push` v7); the major-version
   release notes only change the Node runtime and remove inputs this workflow does not use (GitHub
   releases, retrieved 2026-10-31). Tags and OCI labels from `metadata-action`: `latest` and the
   `Cargo.toml` version on `main`, semver tags on `v*.*.*`. Pull requests that touch the build build both
   platforms without pushing.

## Evidence
- Release binary on the dev machine (x86_64 gnu, `cargo build --release`): 11.4 MB with the default
  profile, 4.0 MB with this one.
- Image build, size and duration on CI: see `docs/reviews/0003-docker-image.md`.

## Consequences
- An arm64 build failure can no longer hide behind emulation slowness, and a PR proves both platforms.
- Deployments must set the uid/gid (README "Deployment").

## Amendments before merge (review 0003, 2026-10-31)
This record was amended on its branch before it was merged, after review 0003:
- `CMD` instead of `ENTRYPOINT` (see Decision).
- `metadata-action` gets `flavor: latest=false`: by default, `type=semver` tag pushes also move `latest`,
  which contradicts "`latest` on `main`" above (docker/metadata-action README @ v6.2.0, "Latest tag",
  retrieved 2026-10-31, per the review agent; **unverified** by Claude directly).
- `cache-to` gets `ignore-error=true`, so a fork PR's read-only cache token cannot fail the build.
