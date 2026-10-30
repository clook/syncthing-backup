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
