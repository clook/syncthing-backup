# 0010 — Removal leftovers are reported, never moved automatically; cleanup checks identity; copies verified before naming
- Status: **accepted** (refines 0008 §2, §4 and §4b)
- Date: 2026-10-30
- Type: technical, with one visible behaviour (leftovers need a human)
- Decided by: proposed by Claude after the third review round, extended after the final review (round 4).
  Approved by the user on 2026-10-31: "1. OK" (answer to "Approve decision 0010?").

## Context
The third review (`docs/reviews/0001-R1-T2-T8.md`, round 3) found that the 0008 design could:
- delete a destination file that is **not ours** when a cleanup ran by path after a remount or disk swap;
- remove the source while the only copy sat hidden under a remounted mount point;
- back up crash leftovers (`.syncthing-backup-removing-*`) as junk-named duplicates, and confuse an old
  leftover with a new file that reuses the same inode number.

## Decision
1. **Identity-checked cleanup.** The mover only removes a destination file if its (device, inode) is still
   the one it just wrote. Before removing the source, the target must still be that file.
2. **Unique private removal name** (`.syncthing-backup-removing-<nanoseconds>`). A directory found at the
   source path (file, directory or symlink) is put back with `renameat2(RENAME_NOREPLACE)`, which needs
   no link permission (round 5: `fs.protected_hardlinks`); filesystems without it fall back to
   `link` + unlink for non-directories. If the original name was taken again in the meantime (a third
   version arrived), the entry is put back as `name_1.ext`, `name_2.ext`…, never replacing anything, and
   the worker queues it immediately (round 4, finding 4: a file stranded under the private name is a
   "local addition" that Syncthing's *Revert Local Changes* would delete). Residual risk, **unverified**:
   a phone file with that exact suffixed name arriving within seconds.
3. **Leftovers are reported, never moved.** A file left under the private name, by a crash in the
   microseconds between rename and unlink, or by a put-back that failed with an I/O error, is never
   deleted. Every scan logs it as a warning and counts it in the scan summary (`leftover(s)`), and a human
   compares it with the backup. This **replaces** 0008 §4b, which backed such files up automatically under
   their private name.
4. Directory fsync: the target directory and the parents of created directories are fsynced on every
   move, and each ancestor up to `DESTINATION_DIRECTORY` once per process run (instead of on every file).
   An ancestor is only remembered once its fsync succeeded, and the memory is per device of
   `DESTINATION_DIRECTORY` (round 4, finding 5).
5. **Copies are verified before they get their final name** (`VERIFY_COPY`): the temporary file is
   compared with the opened source, and only then linked to its backup name. A bad copy can no longer hold
   the primary name after a crash between linking and verifying (round 4, finding 8).
6. **Hard links.** Placing a copy without replacing anything uses `link(tmp, target)`, so the destination
   filesystem must support hard links (ext4, XFS, btrfs, ZFS, NFS: yes; exFAT/FAT and many SMB shares: no).
   The error message says so. On the same filesystem, any failure to link the source (EXDEV from bind
   mounts, EPERM from `fs.protected_hardlinks`) falls back to the copy path instead of failing (round 4,
   findings 2 and 3).
   The first fallback of a run is logged as a warning, since copying doubles HDD I/O (round 5).
7. **Nested Syncthing folders.** A subdirectory that is the root of another *configured* Syncthing folder
   (from `/rest/system/config`, paused and not allowlisted included) is skipped by scans and by events.
   A `.stfolder` marker alone does not count: it can be ordinary synced content (round 4 finding 1,
   round 5).

## Why leftovers need a human
Automatic handling cannot tell an already backed-up copy (a duplicate) from a newer version, because the
original name is not stored. Backing every leftover up would create junk-named duplicates after each crash.
Leftovers are expected to be extremely rare: they need a crash, or two Syncthing renames, within microseconds.

## Evidence
- Tests: `mover::tests::remove_ours_refuses_foreign_file`, `directory_replacing_source_is_put_back`,
  `witness_must_still_exist_before_source_removal`, `replaced_source_is_kept`,
  `put_back_never_replaces_a_newer_version`, `verified_copy_leaves_no_temporary_file`;
  `syncthing::tests::roots_include_paused_and_not_allowlisted_folders`, `tests::event_to_job`,
  `scanner::tests::scans_files_and_maps_destinations`;
  `scanner::tests::leftovers_are_reported_not_queued`.
- Review findings and resolutions: `docs/reviews/0001-R1-T2-T8.md`, rounds 3 to 5.
- Hard-link support per filesystem: **unverified** (general knowledge, not tested here). The user's
  destination is NFS ("The HDD is with NFS", 2026-10-31). NFS supports hard links when the exported
  filesystem does; the e2e suite ran on btrfs, so the NFS path is **unverified** until the T10 dry run
  and a first real move.
