# 0003 — Replace rules for files rewritten in place

Status: **in progress**, branch `feat/replace-patterns`
Decision: [0012](../decisions/0012-replace-patterns.md)

## Context
Some apps write their backup to the same file name every day, for example `Backups/app.db` or
`settings.bak`. The phone re-sends each new version, and invariant 1 ("never overwrite", decision 0004)
saves every version as a new `name_N.ext` in the backup. These copies pile up and nobody ever restores
them.

The user chose to replace, for matching paths only, the backup copy with the newer version instead of
adding `_N`. Copies already in the backup are left alone; the user removes them by hand.

## Tasks
- [x] **R1** This plan, branch, decision 0012.
- [ ] **R2** `REPLACE_PATTERNS` (globs, `globset`) parsed in `src/config.rs`, with unit tests.
- [ ] **R3** `src/mover.rs`: replace path (verified copy, atomic `rename` over the previous regular
      file), `Replaced` and `WouldReplace` outcomes, unit tests.
- [ ] **R4** `src/main.rs`: matcher passed to the mover, new outcomes logged.
- [ ] **R5** e2e `replace` scenario.
- [ ] **R6** README and CLAUDE.md: invariant 1, env var, example.
- [ ] **R7** Review harness, review record 0004, PR (merge only with the user's approval).
- [ ] **R8** Release 0.4.0 and GitOps change (each step with the user's approval).

## Verification
1. `cargo test` and `scripts/review.sh` pass.
2. `tests/e2e/run.sh` (all scenarios, `replace` included) passes against local Syncthing.
3. Live: a `Replaced …` log line once a matching file is re-sent, and no new `_N` copy for it.

## Progress log
- 2026-10-31 — Plan approved. Design choices recorded in 0012 (proposed).
