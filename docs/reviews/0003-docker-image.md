# 0003 — Review of D1–D6 (Docker image and publish workflow)

Reviewed-Commit: 2df17a2
Range: a2f594c..2df17a2
Plan: docs/plans/0002-docker-image.md
Decisions applied: 0011

## Mechanical checks
scripts/review.sh at 2df17a2: passed (34 tests, fmt, clippy `-D warnings`, invariant greps; cargo audit not
installed, skipped). Full output: `0003-docker-image.log`.

## Code review
`/code-review` (high) by a review agent on `a2f594c..de79507`. Verdict: nothing that breaks the build or
the image. Checked against upstream sources (tonistiigi/xx `xx-cargo`, `xx-verify` @ v1.10.0): rustup
target installation, `--print-target-triple`, the clang linker, `xx-apk add gcc musl-dev`, static-pie
acceptance; image tags exist on Docker Hub; `.dockerignore` keeps `src/`; reqwest 0.11 with `rustls-tls`
uses `webpki-roots` (no system certificates); `panic = "unwind"` kept for the worker's `JoinError`
handling. No Docker build was run: the build of both platforms, including `ring` for arm64, is
**unverified** until the PR's Docker check.

## Findings
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 1 | medium | Dockerfile:27 | `ENTRYPOINT` made `docker run <image> sh` (or `compose run … sh`) start a second porter with the full env; two workers break invariant 3 | fixed in 07221d9: back to `CMD` |
| 2 | low | .github/workflows/docker-publish.yml:47 | metadata-action's default `latest=auto` also tags `latest` on `v*.*.*` pushes, so `latest` could move to a non-main build | fixed in 07221d9: `flavor: latest=false` |
| 3 | low | README.md:92 | Tag list incomplete (`X.Y` missing); `X.Y.Z` is rebuilt on every main push until the version is bumped, so it is not immutable | fixed in 07221d9: documented, with digest pinning for an immutable image |
| 4 | low | .github/workflows/docker-publish.yml:66 | Fork PRs have a read-only cache token; a cache export error could fail the build (**unverified** for build-push v7) | fixed in 07221d9: `ignore-error=true` |

The fixes in 07221d9 (about 15 lines) were reviewed inline by Claude, not by a second `/code-review` run.

### Round 2 (PR #8 Docker check, run 41222208178)
The build passed for both platforms in 4 min 19 s (16:58:16 to 17:02:35 UTC). That verifies the `ring`
cross-compile and `xx-verify --static` for amd64 and arm64.
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 5 | low (build noise) | Dockerfile:16 | arm64 only: `warning: linker stderr: clang: argument unused during compilation: '-no-pie'` (reported by the user). rustc 1.99.0 sets `static_position_independent_executables` for `x86_64-unknown-linux-musl` but not for `aarch64-unknown-linux-musl` (`compiler/rustc_target/src/spec/targets/*_linux_musl.rs` @ 1.99.0), so it links with `-static -no-pie` (`compiler/rustc_codegen_ssa/src/back/linker.rs:532-538` @ 1.99.0). clang ignores `-no-pie` next to `-static` | fixed in 2df17a2: `-Wno-unused-command-line-argument` passed to the linker, for the aarch64 musl target only. The binary does not change: it was already static non-PIE |

## Invariants
1, 2, 4, 5: unaffected (no change under `src/`; the release profile only changes optimization and strips
symbols). 3: protected by finding 1's fix; the image still runs one porter per container.
