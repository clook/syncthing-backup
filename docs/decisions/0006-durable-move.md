# 0006 — Durable cross-device move
- Status: accepted, refined by 0008 (temp name, verification source)
- Date: 2026-10-30
- Type: technical
- Decided by: proposed by Claude, approved by user (plan 0001).

## Context
Source (Syncthing, SSD) and destination (HDD) are normally on different filesystems, so the copy path
is the main path.

## Decision
- Same device: `hard_link(src, target)`, then remove the source.
- Different device: copy to `.<name>.porter-tmp` in the target directory, copy mtime and permissions,
  `fsync`, `hard_link(tmp, target)`, remove tmp, `fsync` the directory. If `VERIFY_COPY` is on, re-compare
  the target with the source. Then check that the source path still points to the same inode that was
  copied, and only then remove the source.
- A failed verification removes only the file this attempt just created. The source is kept.

## Evidence
- Old code: `fs::rename`, then on any error `fs::copy` straight to the final name with no fsync, then
  `remove_file(src)` (src/main.rs:187-189 @ 1057e25). A crash or power loss leaves a truncated destination,
  or no durable copy at all. mtime is not preserved.
- `link(2)` refuses to replace an existing name: "EEXIST newpath already exists" (Linux man-pages, checked
  locally 2026-10-30). That makes "never overwrite" (0004) atomic.
- The old comparison (src/main.rs:107-112 @ 1057e25) compared raw `read()` counts from two readers. A
  short read makes identical files look different. The new comparison fills whole buffers.
- **Limit (stated honestly)**: right after a copy, the verification reads mostly from the page cache. It
  catches logic bugs and a source that changed during the copy, but not media errors on the HDD. Use a
  checksumming filesystem or scrubbing, or restic's `check`, for those.
- Tests (T4): `mover::tests::copy_path_preserves_content_and_mtime`, `never_overwrites_existing`,
  `witness_missing_moves_nothing`.
