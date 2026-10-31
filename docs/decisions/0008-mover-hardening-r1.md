# 0008 — Mover hardening after review R1
- Status: accepted (refines 0006: the temp file name, the verification source and how the source is
  removed all change)
- Date: 2026-10-30
- Type: technical
- Decided by: proposed by Claude after review R1 and its re-review. It implements invariants 1, 2 and 5
  from `CLAUDE.md`, which the user already approved, and changes no behaviour visible to the user. Approved by
  the user on 2026-10-30: "I approve 0008 and 0009, I'll switch to Receive Only". (It was first marked accepted without an approval; the second review caught
  this.)

## Context
Review R1 (`docs/reviews/0001-R1-T2-T8.md`) found ten ways the 0006 design could break the invariants in
edge cases.

## Decision
1. **Open the source once.** The copy, the verification, the dedup comparisons and the final "still the
   same file" check all use that file descriptor and inode. A newer version that Syncthing renames into place
   during a copy no longer makes a correct copy of the previous version fail verification and get deleted.
   Both versions end up in the backup (keep-both, 0004).
2. **Reserved temp name `.syncthing-backup-7f3a9c2e.tmp`**, created with `O_EXCL` (`create_new`). It never follows a
   symlink and never truncates a file it did not create. It is short, so it cannot hit `ENAMETOOLONG`. A
   stale copy is removed only if it is a regular file. A source file with that exact name is refused.
   The random token makes a clash with a real file practically impossible, and the scanner skips that name.
3. **Durability on every path.** The destination is fsynced on the hard-link and dedup paths too (not only
   the copy path). Every directory from the target up to `DESTINATION_DIRECTORY` is fsynced. That also
   covers directories created, but never synced, by earlier runs or by 0.2.x.
4. **Witness check before removing the source.** The witness must still exist, and the device (`st_dev`) of
   `DESTINATION_DIRECTORY` must be the same as when the move started. If it is not, the file just written
   (probably onto the bare mount point) is removed. Subvolumes and mounts below the root are allowed.
   (The first version compared the written file with the witness, which broke on btrfs subvolumes. The
   second review found this.)
4b. **Removing the source without a race window.** The source is renamed (atomically, whatever it holds at
   that moment) to `.syncthing-backup-removing-<inode>`, checked, then unlinked. If it turns out to be a
   newer version, it is linked back. A leftover from a crash is handled the same way on the next attempt.
   The private name is deliberately *not* a Syncthing temp name: if it is ever stranded, it gets backed up
   instead of being cleaned up.
5. **EXDEV falls back to a copy.** This covers bind mounts of the same filesystem, which share an `st_dev`.
6. Conflict names (`name_N.ext`) are shortened to stay within NAME_MAX (255 bytes), cutting at a UTF-8
   character boundary.
7. Worker and event loop:
   - a file that a Syncthing **event** announces again while it is being processed is re-queued afterwards
     (scans do not do this, so a failing file cannot loop);
   - the folder mapping is retried every 10 s while no mapping is known;
   - the redundant verification of hard links (the same inode) is skipped;
   - Syncthing's `startTime` is checked on every events response.

## Evidence
- Findings (R1: 10, re-review: 10) and how each was resolved: `docs/reviews/0001-R1-T2-T8.md`.
- Tests:
  - `mover::tests::stale_tmp_is_replaced`, `tmp_symlink_is_never_followed`, `reserved_name_is_rejected`,
    `long_names_stay_within_name_max`, `creates_nested_directories`, `same_content_detects_difference_and_equality`,
    `replaced_source_is_kept`, `interrupted_removal_is_completed`;
  - `tests::queue_deduplicates` (re-queue after a re-announcement).
- End-to-end runs with real Syncthing v2.1.7 (scenarios burst, resend, edit, st_restart, kill_copy, offline,
  witness): logs in `docs/reviews/`.
- **Not covered by automated tests (unverified by test, reasoned only)**: the EXDEV fallback needs a bind
  mount; a source replaced in the middle of a copy cannot be triggered from a unit test without a hook;
  the witness `st_dev` mismatch needs a real unmount.
