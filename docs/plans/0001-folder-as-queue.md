# 0001 — Folder as queue (hardening, drop Redis)

Status: **in progress**, branch `feat/folder-as-queue`


## Context
The porter moves files that Syncthing receives from the phone (send-only + ignore delete) to an HDD; the
server Syncthing folders are Send & Receive. It is used as the "cloud backup" path, so it must never lose
or overwrite a file and must never silently leave a file behind. Review of `src/main.rs` found:

- **Silent gaps**: Syncthing event IDs restart at 1 after a Syncthing restart, but the stored `last_event_id`
  is reused → `since=<old id>` returns nothing for a long time. Events are also lost when the porter is down
  longer than Syncthing's event buffer. Retry queue only replayed at startup.
- **Unsafe cross-disk move** (the normal case SSD→HDD): `fs::copy` straight to final name, no fsync, then
  delete source → truncated/lost file on crash/power loss. mtime not preserved.
- **Overwrites backups**: `overwrite`/`different` replace existing backup files; default `nothing` means an
  edited photo is never backed up and silently stays in source.
- Retry hash keyed by event ID (collides after Syncthing restart); busy loop with no log on non-JSON/401
  responses; `to_str().unwrap()` panics on non-UTF8 names; dry-run consumes events; retry entries for
  vanished sources never removed; ConfigSaved refresh errors ignored.

Decision records (with evidence): `docs/decisions/0001`–`0007`.

Decisions made with the user: drop Redis and use the **source folder as the durable queue** (scan + events
→ single sequential worker); conflicts = **keep both (suffix), never overwrite**; only phone folders on the
instance, add an **optional `FOLDER_IDS` allowlist**.

## Tracking
The plan lives **in the repo**, not only in this session's plan file:
- `docs/plans/0001-folder-as-queue.md`: this plan (context, decisions, design, verification), plus a
  **task checklist** (IDs T1…T10 below) and a **Progress log** section. Each session ticks off the tasks
  it finished and adds a dated log line with the result, anything that differed from the plan, and what
  comes next.
- Work happens on branch `feat/folder-as-queue`. Roughly one commit per task, with the message prefixed by
  the task ID (e.g. `T4: mover with durable cross-device copy`). Each commit updates the checklist, so
  `git log` and the plan file stay in sync.
- `CLAUDE.md` points to `docs/plans/`, so a new session starts by reading the active plan and resumes at the
  first unchecked task.
- Later redesigns get new plan files (`0002-…`). Old ones stay as a record of decisions.

### Task checklist
- [x] **T1** Create `docs/plans/0001-folder-as-queue.md` (this plan + checklist + progress log), create
      `CLAUDE.md`, create the branch, and commit the existing uncommitted `fetch_folders` refactor as the
      baseline.
- [x] **T1b** Review harness (user request): `scripts/review.sh`, `scripts/check-reviewed.sh`,
      `.githooks/pre-push`, `.github/workflows/ci.yml`, `docs/reviews/`, process described in `CLAUDE.md`.
      Review sets: **R1** = T2–T6 (core rework), **R2** = T7–T8. Each set is reviewed before it is pushed.
- [x] **T1c** Decision records with evidence (user request): `docs/decisions/` 0001–0007, rules in
      `CLAUDE.md`. Removing the Redis/`config` deps moves from T7 to T6 (review.sh checks "no Redis", and R1
      covers T2–T6).
- [x] **T2** `config.rs`: new and removed env vars, warnings for obsolete ones.
- [x] **T3** `syncthing.rs`: folders (+ `paused`, `FOLDER_IDS`), event polling with status checks and
      backoff, `startTime`.
- [x] **T4** `mover.rs` + unit tests: suffix conflicts, dedup, no-replace link, durable copy, verify, mtime,
      witness, dry run.
- [x] **T5** `scanner.rs` + tests for the ignore rules.
- [x] **T6** `main.rs`: pending set, single worker, event task, scan task, drain-then-rescan, test with a
      slow fake mover.
- [x] **T7** Remove Redis/`config` deps (done in T6), update the Dockerfile, bump to 0.3.0.
- [x] **T8** Rewrite `README.md`.
- [x] **T9** Local end-to-end checks (Syncthing container, crash/restart, 1 GB + 100 small files burst).
- [ ] **T10** k3s deploy in dry run, then live. Mark the plan as done.

### CLAUDE.md content (concise)
- What the project is: a Syncthing → HDD porter for phone backups. Phone folders are send-only with ignore
  delete; server folders are Send & Receive.
- **Safety invariants**. Every change must keep them:
  1. Never overwrite or modify an existing destination file (conflicts get a `_N` suffix, identical files
     are deduplicated).
  2. Delete the source only after the destination is fsynced, and verified when `VERIFY_COPY` is on.
  3. One sequential worker for HDD I/O. The event poller never does disk I/O.
  4. No external state: the source folder is the queue, and scans must always be able to rebuild it.
  5. Never write when the witness file is missing.
- Commands: `cargo build`, `cargo test`, `cargo clippy`, and how to run locally with `DRY_RUN=true`.
- Module map (`config`, `syncthing`, `scanner`, `mover`, `main`).
- Workflow: read the active plan in `docs/plans/`, update its checklist and progress log in the same
  commit as the code, and prefix commit messages with the task ID.

## Design

```
startup scan ─┐
periodic scan ├─► pending set (dedup, ordered) ─► ONE worker (sequential HDD I/O) ─► move_file()
events       ─┘        (in-memory)                 failure → stays in source, next scan retries
```
Crash at any point is safe: the source file is only deleted after the destination is durable, and the next
startup scan re-discovers anything not finished. No external state.

### Modules (split the 300-line `main.rs`, keep the existing style: anyhow, tracing, tokio)
- `src/config.rs` — `AppConfig` from env. Keep `SYNCTHING_URL`, `SYNCTHING_KEY`, `SOURCE_DIRECTORY`,
  `DESTINATION_DIRECTORY`, `SYNCTHING_STORAGE_DIRECTORY`, `WITNESS_FILE`, `DRY_RUN`. New: `FOLDER_IDS`
  (comma list, optional), `SCAN_INTERVAL_SECS` (default 900), `VERIFY_COPY` (default true),
  `MIN_AGE_SECS` (default 30). Removed: `REDIS_HOST`, `START_ID`, `EXISTING_STRATEGY` → log a **warning** if
  still set (user may currently run with `overwrite`/`different`).
- `src/syncthing.rs` — HTTP client: reuse `fetch_folders` (from current uncommitted diff), extend
  `FolderConfig` with `paused` and filter by `FOLDER_IDS`; `system_start_time()` via `/rest/system/status`;
  `poll_events(since)` using `events=ItemFinished,ConfigSaved&timeout=60`, checking HTTP status and logging
  + backing off on any error (fixes busy loop).
- `src/scanner.rs` — walk each mapped folder (`src_dir/rel`), yield regular files only; skip `.stfolder`,
  `.stignore`, `.stversions/`, `.syncthing.*.tmp`, `~syncthing~*.tmp`, symlinks, and files whose ctime is
  younger than `MIN_AGE_SECS` (Syncthing renames temp→final atomically; age guard covers its post-rename
  steps). Use `std::fs::read_dir` recursion in `spawn_blocking` (no new dep).
- `src/mover.rs` — blocking, fully unit-tested `move_file(src, dst, cfg) -> Result<Outcome>`:
  1. Witness check (`dst_dir/WITNESS_FILE`); missing → `Err(WitnessMissing)` → worker pauses (sleep +
     recheck), item kept.
  2. Resolve target: `name.ext`, then `name_1.ext`, `name_2.ext`… For each existing candidate: identical
     (reuse `files_are_identical_sync`) → `Outcome::Duplicate` (delete source); first free name → target.
     Build names with `OsString` (no unwrap → non-UTF8 safe; handle no-extension/dotfiles).
  3. Same device (`st_dev` equal): `hard_link(src, target)` (atomic, fails if target exists = never
     overwrites) then `remove_file(src)`.
  4. Cross device: copy to `target_dir/.<name>.porter-tmp`, `set_modified`/permissions from src,
     `sync_all()`, `hard_link(tmp, target)` (no-replace), remove tmp, fsync parent dir; if `VERIFY_COPY`
     re-read and compare target vs src; only then `remove_file(src)` and fsync source dir.
  5. `DRY_RUN`: log the decided outcome, touch nothing.
  Stale `*.porter-tmp` files are ignored by the scanner and overwritten/removed on next attempt.
- `src/main.rs` — wiring:
  - `pending: BTreeSet<PathBuf>` behind a `tokio::sync::Mutex` + `Notify`; producers insert, the single
    worker pops one at a time and runs `move_file` in `spawn_blocking`. Failures are logged with error and
    dropped from the set (source still exists → next scan re-queues); count consecutive failures per path
    for an `error!` escalation.
  - Event task: on ItemFinished (`type=file`, `action=update`, no error, folder mapped) enqueue path;
    on ConfigSaved refresh folders (log failures). Track `since` in memory only. On connection error or
    when returned IDs go backwards: compare `startTime`; if changed → `since=0` and trigger a full scan.
  - **Events keep flowing during long copies** (1 GB file + 100 × 200 KB burst): the event poller is its
    own tokio task and only does an in-memory insert (no disk I/O, never awaits the worker); the copy runs
    in `spawn_blocking`, so polling continues while the HDD is busy. The 100 small files are queued as
    their events arrive and handled one by one after the 1 GB file.
  - **Gap detection** (decision 0003): a filtered subscription gets its own consecutive IDs, so if the
    first returned ID is greater than `since + 1`, events were dropped and a rescan is triggered.
  - **Missed events are caught anyway (drain-then-rescan)**: whenever the worker
    empties the queue after processing ≥1 item, it starts a source scan (readdir + stat on the SSD only,
    cheap). Any file whose event was lost is picked up right after the burst, not 15 min later.
  - Scan task: at startup, after each drained burst, and every `SCAN_INTERVAL_SECS`; log `pending N, oldest Xh` and `warn!` if any
    file has been pending > 24h (backup-gap alert visible in logs).

### Other files
- `Cargo.toml`: remove `redis`, `config`; add dev-dep `tempfile`; bump to `0.3.0`.
- `Dockerfile`: pin `alpine:3.x`, drop `REDIS_HOST`/`SYNCTHING_PORT` envs.
- `README.md` (full rewrite, T8):
  - How it works: the recommended phone and server Syncthing settings, why files are never sent again, and
    a diagram of the folder-as-queue design.
  - Safety guarantees: the invariants above, in user terms.
  - Configuration table: the new env vars, plus a "migrating from 0.2.x" note (Redis removed;
    `EXISTING_STRATEGY`/`START_ID` ignored; conflicts now keep both files).
  - Deployment: Docker Compose without Redis, and an example k3s Deployment (hostPath/PVC mounts, a
    `WITNESS_FILE` that is strongly recommended, the API key in a Secret).
  - Limits: the HDD is a single copy, so pair it with restic or Kopia to an offsite target (a k3s CronJob)
    for a real 3-2-1 backup.
  - Keep the AI disclosure, updated to mention Claude.

## Verification
Run checks after each task (and record the result in the plan's progress log):
1. `cargo build`, `cargo clippy`, `cargo test` — mover unit tests (tempfile, same-fs and copy path forced):
   new file; identical → source removed; differing → `_1` created, original untouched; identical to existing
   `_1` → dedup; non-UTF8 filename; mtime preserved; witness missing → nothing moved; dry-run → nothing
   changed. Scanner tests for ignore patterns.
2. Local end-to-end: run a Syncthing container + porter with `DRY_RUN=true`, drop files from another
   device, check logs; then real run, restart Syncthing mid-way and kill the porter mid-copy, confirm every
   file ends up in the destination exactly once and no `.porter-tmp` remains after next scan.
   Burst case: send one 1 GB file and then 100 × 200 KB files. Check that the logs show the small files'
   events queued *during* the big copy, and that all 101 files reach the destination. Run it again with
   the poller artificially stopped during the copy, so the events are lost, and check the drain-then-rescan
   still moves all of them.
   Add a unit test with a fake slow mover: the pending set must keep growing while the mover is blocked.
3. Deploy with `DRY_RUN=true` first on k3s, check the startup scan picks up any files left behind by the old
   version, then switch off dry-run.

## Progress log

- 2026-10-30 — Plan agreed. T1: plan + CLAUDE.md added, baseline commit 1057e25 (fetch_folders refactor).
- 2026-10-30 — T1b: review harness added (user asked for a review gate before any push).
- 2026-10-30 — Harness self-test: review.sh on the old code failed on fmt, clippy, unwraps (main.rs:139-140),
  file operations outside the mover (main.rs:181-189) and Redis. check-reviewed.sh refused a tree with no
  review record. Both behave as intended.
- 2026-10-30 — T1c: decision records added. Syncthing source checked (pinned ed38528): event IDs are
  per-subscription, in memory only, with a 1000-event buffer. This corrects the plan draft, which said gaps
  could not be detected with a filter. Gap detection added (decision 0003).
- 2026-10-30 — T2–T6 implemented: 22 unit tests, and scripts/review.sh passes (clippy collapsible_if fixed
  with clippy --fix). Differences from the plan: the source dir is not fsynced after the source is removed
  (if a crash brings the file back, the next pass sees it as a duplicate, which is harmless); before
  removing the source, the mover checks it is still the same inode, size and mtime that was copied, so a
  newer version renamed in by Syncthing is never deleted; SIGTERM lets the current file finish. Next: R1
  review.
- 2026-10-30 — T7: Alpine pinned to 3.24 (latest stable is 3.24.3, checked on dl-cdn.alpinelinux.org), obsolete ENV removed, version 0.3.0. T8: README rewritten. The Docker build itself is checked in T9.
- 2026-10-30 — R1 review: 10 findings, all fixed. A second review of those fixes found 10 more (including
  regressions that could stall the worker), all fixed, and a third review is running. T9: the e2e suite uses
  two local Syncthing v2.1.7 instances. With Receive Only, 7/7 scenarios pass, and in the burst scenario all
  100 small files were queued during the 2 GB copy. With Send & Receive, `edit` fails because of how
  Syncthing resolves conflicts, which led to decision 0009. The user approved 0008 and 0009: "I approve 0008 and 0009, I'll switch to Receive Only".
- 2026-10-30 — Final full review (round 4, 1057e25..19435b6): 10 findings; fixed in 2957bb3 except #7
  (path component exists as a file: rare, never silent) and #10 (extra status request: correctness over
  speed). Round 5 reviewed 2957bb3 and found a regression (a `.stfolder` marker alone skipped a subtree)
  and that put-back via `link` fails under `fs.protected_hardlinks`; fixed: nested folders come from the
  Syncthing config and apply to events too, put-back uses `renameat2(RENAME_NOREPLACE)` and is re-queued
  at once, link fallback warns once. e2e at 2957bb3: Receive Only 7/7 pass. Decision 0010 extended,
  still proposed.
- 2026-10-30 — Round 6 (review of 139b163): an immediate re-queue of a file modified in place (same inode)
  could loop and write a new `_N` copy per pass. Fixed: only a replaced inode is re-queued at once, a
  modified file waits for a scan (MIN_AGE_SECS). Creation-time comment corrected (not available on musl).
- 2026-10-30 — Review record docs/reviews/0001-R1-T2-T8.md (rounds 1–6, Reviewed-Commit 6b5ea5b) with
  review.sh and e2e logs. T9 done: e2e Receive Only 7/7 at 139b163. Next: user approval of 0010, push,
  T10 (k3s dry run).
- 2026-10-31 — The user approved 0010 ("1. OK") and the push ("OK for pushing"). Destination is NFS
  ("The HDD is with NFS"): to check during T10 that hard links and the witness work on the export.
