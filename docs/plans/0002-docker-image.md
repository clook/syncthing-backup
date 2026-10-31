# 0002 — Docker image and publish workflow optimizations

Status: **in progress**, branch `feat/docker-image`
Decision: [0011](../decisions/0011-docker-build.md)

## Context
Docker Build and Publish runs take about 35 min (36 and 34 min on 2026-02-16; `gh run list --workflow
docker-publish.yml`). The likely cause (**unverified**: the old run logs have expired) is the arm64 Rust
compile under QEMU emulation. The workflow also builds and pushes an unused `build-cache` builder image.
There is no `.dockerignore`, so a local build sends 6.2 GB (`target/`, 4.1 GB of it e2e data) to Docker,
and `COPY . .` copies the host's `target/` into the build stage. The builder image `rust:1.93-alpine` was
last updated on 2026-02-13 (Alpine 3.23).

The user approved items 1–5 and 7–10 of the proposal: "OK go ahead with the proposed fixes" (2026-10-31).
Items 6 (scratch base image) and 11 (gate publishing on CI) are out of scope.

## Tasks
- [x] **D1** This plan, branch, decision 0011.
- [x] **D2** `.dockerignore`: only `Cargo.toml`, `Cargo.lock` and `src/` reach the build.
- [x] **D3** `Cargo.toml` release profile: `strip`, thin LTO, one codegen unit (keep `panic = "unwind"`:
      the worker catches a mover panic).
- [x] **D4** `Dockerfile`: build stage on `$BUILDPLATFORM`, cross-compile with `tonistiigi/xx`; drop
      `ca-certificates`; `CMD` with an absolute path (was `ENTRYPOINT`, changed after review 0003); builder `rust:1.99.0-alpine3.24`.
- [x] **D5** Publish workflow: no builder image push, no QEMU, `concurrency`, current action majors,
      `metadata-action` tags and labels, PR builds without push.
- [x] **D6** README: run with Syncthing's uid/gid (Compose `user:`, K3s `securityContext`), image tags.
- [ ] **D7** Review harness, review record 0003, PR (merge only with the user's approval).

## Verification
1. Local `docker buildx build` for amd64 and arm64: time, image size, context size, against the current
   Dockerfile.
2. Static binaries for both architectures (`xx-verify`, `file`).
3. `docker run` without env exits with "Env var SYNCTHING_KEY is required".
4. `scripts/review.sh` passes with the new release profile.
5. The PR run of the publish workflow builds both platforms; its duration is recorded in 0011.

## Progress log
- 2026-10-31 — Plan approved. The Docker daemon is not running on the dev machine, so local image
  measurements wait for it. `rust:1.93-alpine3.24` does not exist: pinned `rust:1.99.0-alpine3.24`
  (2026-10-04, newest release at least two weeks old; CI already builds with the latest stable).
- 2026-10-31 — D1–D6 written. Release binary 11.4 MB → 4.0 MB (host build). QEMU removed from the workflow:
  the runtime stage has no RUN step. The image itself is verified by the PR's Docker build (no local
  daemon). Next: D7 review.
- 2026-10-31 — D7: review 0003 (4 findings, all fixed in 07221d9; `ENTRYPOINT` back to `CMD`). review.sh
  passes. Next: PR and its Docker check (the multi-arch build is still **unverified**).
