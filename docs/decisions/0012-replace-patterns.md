# 0012 — Replace rules: newer versions of matching paths replace the backup copy
- Status: proposed
- Date: 2026-10-31
- Type: product
- Decided by: user. Asked what files matching a rule should do, they chose "Replace latest on NAS" ("A
  matching file replaces its NAS copy with the same name instead of getting a `_N` suffix"). Asked
  whether the rule should also clean up older copies already in the backup, they chose "No, I'll do it
  by hand". Technical details proposed by Claude, approved with plan 0003.

## Context
Decision 0004 never overwrites: a new version of a file already in the backup is saved as `name_N.ext`.
That is right for photos and documents. Some apps, though, rewrite their own backup file under the same
name every day (for example `Backups/app.db`). Each version becomes one more `_N` copy, and the backup
grows with copies nobody restores.

## Options considered
1. Keep the last N copies, deleting older ones in the backup.
2. Discard matching files on the server without backing them up.
3. **Replace the backup copy with the same name** (chosen by the user).

## Decision
- New setting `REPLACE_PATTERNS`: comma-separated globs on the path relative to `DESTINATION_DIRECTORY`
  (the same as relative to `SOURCE_DIRECTORY`). Empty or unset: off, behaviour unchanged. `*` does not
  cross `/`, `**` does. An invalid pattern stops the startup.
- For a matching file, the destination is the same name only:
  - missing: normal move;
  - regular file with identical content: duplicate, source removed (as in 0004);
  - regular file with different content: **replaced**;
  - anything else (directory, symlink): keep both, as in 0004, with a warning.
- Replacing is a copy to the mover's temporary file, fsynced and **always verified** (even with
  `VERIFY_COPY=false`, since the previous good copy is about to be discarded), then `rename(2)` of the
  temporary file over the previous copy, then the usual directory fsync, identity, mount and source
  removal steps (0006, 0008, 0010).
- Invariant 1 becomes: never overwrite or modify an existing destination file, except paths matching
  `REPLACE_PATTERNS`, where a verified, fsynced newer version atomically replaces the previous one.
- Nothing is cleaned up retroactively: existing `_N` copies stay.

## Evidence
- `rename(2)` replaces atomically. man-pages 6.18 (`man 2 rename`, Arch package man-pages 6.18-1,
  read 2026-10-31): "If newpath already exists, it will be atomically replaced, so that there is no point
  at which another process attempting to access newpath will find it missing." So at every moment the path
  holds either the complete previous copy or the complete new one.
- `RENAME_EXCHANGE` (swap, then inspect what was replaced) is not used: the same page lists the
  filesystems that support the rename flags, and NFS is not among them. The live destination is an NFS
  share (k3s-cluster-gitops `apps/syncthing-backup/deployment.yaml`). That NFS rejects these flags is
  **unverified** here.
- The target is checked to be a regular file just before the rename. Between the check and the rename,
  only the porter's single worker writes to the destination (invariant 3), the same reasoning as the
  existing comment at src/mover.rs:110 @ 687d043.
- Old copies piling up were seen on the live system on 2026-10-31: the first live run created several
  `name_1.ext` copies of files rewritten daily by an app.
- Tests: added with R3 and R5 (named in the review record 0004).

## Consequences
- For matching paths, the backup keeps only the newest version: an older version is gone once replaced.
  That is the point, but a rule that is too broad (`**`) would turn the backup into a mirror for those
  paths. The README warns about it.
- Files whose name changes on every version (for example `backup-YYYY-MM-DD.db`) never collide and still
  accumulate. Out of scope (the user chose not to add pruning).
- A crash after the rename but before the source is removed leaves the new version in the backup and the
  source in place; the next pass sees identical content and removes the source.
