# syncthing-backup

Rust service that moves files received by a server-side Syncthing instance (phone backups) to an HDD.
Phone folders are **send-only + ignore delete**; server folders are **Send & Receive**. Once a file is moved
away, Syncthing records the deletion and the phone never re-sends it.

This is the user's backup path: **correctness beats features and speed.**

## Safety invariants (every change must preserve them)
1. Never overwrite or modify an existing destination file. Conflicts get a `_N` suffix; identical content is
   deduplicated (source deleted, nothing written).
2. Delete the source only after the destination is fsynced (and verified when `VERIFY_COPY=true`).
3. A single sequential worker does all HDD I/O. The event poller never touches the disk.
4. No external state: the source folder *is* the queue; a scan must always be able to rebuild it.
5. Never write when the witness file is missing.

## Layout
- `src/config.rs` — env configuration
- `src/syncthing.rs` — REST client (folders, events, status)
- `src/scanner.rs` — walks source folders, applies Syncthing ignore rules
- `src/mover.rs` — `move_file`: conflict resolution, durable copy, verification (blocking, unit-tested)
- `src/main.rs` — pending set, worker, event task, scan task

## Commands
- `cargo build` / `cargo test` / `cargo clippy --all-targets`
- Local run without touching files: `DRY_RUN=true SYNCTHING_KEY=… SOURCE_DIRECTORY=… DESTINATION_DIRECTORY=… cargo run`

## Workflow
- Plans live in `docs/plans/NNNN-<slug>.md`. Start a session by reading the active one (Status: in progress)
  and resume at the first unchecked task.
- Tick the task's checkbox and add a dated line to its **Progress log** in the same commit as the code.
- Prefix commit messages with the task ID (e.g. `T4: …`).

## Review harness (mandatory before any push)
Every task, or set of tasks, goes through this loop before it is pushed to GitHub:

1. **Mechanical review:** `scripts/review.sh` must pass. It runs rustfmt, clippy with `-D warnings`, the tests,
   invariant greps (no `unwrap`/`expect` in production code, only `mover.rs` deletes, renames or copies
   files, no Redis), and `cargo audit` when it is installed. Never weaken a check to make it pass. If a check
   is wrong, change it in its own commit and say why.
2. **Code review:** run the `/code-review` skill (effort `high`) on the code changed since the last
   `Reviewed-Commit` (`git diff <last>..HEAD`). Review it explicitly against the five safety invariants
   above. Also look for: data-loss paths, crash or power-loss windows, races between the event poller,
   scans and the worker, and error paths that drop a file silently.
3. **Fix:** fix every confirmed finding in `T<n>: fix review …` commits, then re-run `scripts/review.sh`. A
   finding that is not fixed needs a written reason.
4. **Record:** add `docs/reviews/NNNN-<tasks>.md` (template in `docs/reviews/README.md`), with
   `Reviewed-Commit:` set to the last code commit, the findings table with their resolutions, and the
   review.sh result. Commit it, and add a line to the plan's progress log.
5. **Push:** the `pre-push` hook (`.githooks/pre-push`) runs `scripts/check-reviewed.sh` and
   `scripts/review.sh`. It refuses a push if the code changed since the last review record, if any check
   fails, or if the working tree is dirty. CI (`.github/workflows/ci.yml`) runs the same two scripts.

Setup once per clone: `git config core.hooksPath .githooks`. `git push --no-verify` is for emergencies
only, and the user must decide to use it. Claude never uses it on its own.
