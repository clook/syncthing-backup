# 0004 — Review of R2–R6 (replace rules)

Reviewed-Commit: 5a4e75f
Range: 2df17a2..5a4e75f
Plan: docs/plans/0003-replace-patterns.md
Decisions applied: 0012 (proposed; amended by the fixes below before acceptance)

## Mechanical checks
scripts/review.sh at 5a4e75f: passed (51 tests, fmt, clippy `-D warnings`, invariant greps; cargo audit not
installed, skipped). Full output: `0004-replace-patterns.log`.

## End-to-end
`tests/e2e/run.sh` with two local Syncthing v2.1.7 instances (server folder Receive Only), all 8 scenarios
including the new `replace`:
- at f91e606 (before review): passed;
- at c7d8088 (round 1 fixes): passed;
- at e6b1dab (final): passed, `0004-e2e.log`.

## Code review
- Round 1: `/code-review` (high) by a review agent on `2df17a2..f91e606`, 10 findings.
- Round 2: `/code-review` (high) by a review agent on the round 1 fixes (`fd30036..c7d8088 -- src/`).
  Verdict: no data-loss path in the fixes; 10 hardening findings.
- The round 2 fixes (e6b1dab, about 80 lines) were reviewed inline by Claude, not by a third
  `/code-review` run. One edge checked: if the name is swapped for a symlink between the stat and the open
  in `resolve_replace`, `File::open` follows it, but the identity check before the rename uses
  `symlink_metadata`, so the move fails instead of replacing.
- 5a4e75f (test only, reviewed inline): CI on ext4 failed `replace_refuses_a_changed_target`. The test
  deleted and recreated the backup file, and ext4 reused the inode number, so the "other" file had the same
  identity. The test now moves the compared file aside. Limitation recorded: identity is device + inode,
  so a file deleted and recreated under the same inode number between the comparison and the rename is
  not detected. That needs another writer in the backup directory during a move (invariant 3).

Severity: high = can lose or hide a backup; medium = can stall backups or break an invariant without
loss; low = efficiency, docs, or a narrow edge case.

### Round 1 (2df17a2..f91e606)
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 1 | high | src/mover.rs:212 | A failed mount check after a replace made `remove_ours` delete the new copy; the previous one was already gone | fixed in c7d8088: after a replace the new copy is kept, the source stays and is deduplicated next pass |
| 2 | high | src/mover.rs:495 | A vanished replace target (unmount) returned `None`; the retry wrote a full copy onto the mountpoint (invariant 5) | fixed in c7d8088: a changed target is an error; the mount is re-checked before a retry |
| 3 | high | src/mover.rs:170 | A source modified in place during the copy could replace a good backup with a torn version | fixed in c7d8088: rename refused if size or mtime changed (ctime added in round 2) |
| 4 | low | src/main.rs:233 | A newer version put back as `app_1.db` is re-queued under that name and can replace a pre-rule `app_1.db` | won't fix: needs two new versions during one move and a pre-rule `_1`; both versions are older copies of a file the rule discards anyway. Recorded in 0012 consequences |
| 5 | medium | src/config.rs:66 | Never-matching forms accepted silently; `{a,b}` split at its comma; non-UTF-8 value silently disabled the feature | fixed in c7d8088: forms rejected, brace-aware split, UTF-8 error, warning for patterns outside every folder |
| 6 | process | docs/decisions/0012:41 | NFS atomicity stated as fact; tests not named | fixed in c7d8088: labelled **unverified**, tests named |
| 7 | low | README.md:50 | Safety item 2 still said the final name never replaces a file | fixed in c7d8088 |
| 8 | medium | src/mover.rs:488 | Rename checked "a regular file", not the file that was compared | fixed in c7d8088: `Target::Replace` carries the compared `FileId` |
| 9 | low | src/mover.rs:376 | A same-size replace reads the data about five times; unneeded `stat` | partly fixed in c7d8088 (`stat` only on the link path); extra reads accepted (correctness over speed), recorded in 0012 |
| 10 | low | src/mover.rs:224 | "No free destination name" error misleading for a replace | fixed in c7d8088 |

### Round 2 (fd30036..c7d8088, src/ only)
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 1 | medium | src/mover.rs:140 | The retry guard checked only that the witness exists, not that the backup root is the same filesystem | fixed in e6b1dab: `confirm_mount` |
| 2 | medium | src/mover.rs:503 | The stability check used size and mtime only; a writer can restore mtime | fixed in e6b1dab: ctime compared too |
| 3 | medium | src/mover.rs:559 | `remove_source` compared whole-second mtime (pre-existing, every move) | fixed in e6b1dab: nanoseconds compared |
| 4 | low | src/mover.rs:393 | Replace identity taken from a stat before the compared file was opened | fixed in e6b1dab: identity from the opened file |
| 5 | medium | src/config.rs:167 | Non-UTF-8 values of other variables (`WITNESS_FILE`, `DRY_RUN`, …) silently fell back to defaults (pre-existing) | fixed in e6b1dab: every optional variable errors on non-UTF-8 |
| 6 | low | src/config.rs:121 | Braces in `[…]` or escaped braces counted by the comma split | fixed in e6b1dab, test `replace_patterns_literal_braces_do_not_join` |
| 7 | low | src/mover.rs:139 | Stale comment on why the loop retries | fixed in e6b1dab |
| 8 | low | src/config.rs:66 | Redundant `/` checks | fixed in e6b1dab |
| 9 | low | src/mover.rs:215 | "Keep the new copy on mount failure" has no test | won't fix now: needs a test seam to change the mount mid-move; recorded as untested in 0012 |
| 10 | low | src/main.rs:415 | The outside-folders warning runs only when the folder mapping changes | won't fix: it runs at startup and on every mapping change, so a folder added later re-checks all patterns; repeating it every scan would only add noise |

## Invariants
1. Never overwrite: unchanged outside `REPLACE_PATTERNS`. Inside, only the compared regular file is
   replaced, by a verified and fsynced copy, from a stable source, through an atomic rename. ok
2. Source removed only after fsync (and verify): ok on the replace path (verification forced).
3. Single worker for HDD I/O: ok, no change to the queue or the poller.
4. Source folder is the queue: ok; every new error keeps the source for the next scan.
5. No write without witness: ok; the mount is re-checked before any retry.
