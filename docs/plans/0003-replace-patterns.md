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
- [x] **R2** `REPLACE_PATTERNS` (globs, `globset`) parsed in `src/config.rs`, with unit tests.
- [x] **R3** `src/mover.rs`: replace path (verified copy, atomic `rename` over the previous regular
      file), `Replaced` and `WouldReplace` outcomes, unit tests.
- [x] **R4** `src/main.rs`: matcher passed to the mover, new outcomes logged.
- [x] **R5** e2e `replace` scenario.
- [x] **R6** README and CLAUDE.md: invariant 1, env var, example.
- [x] **R7** Review harness, review record 0004, PR (merge only with the user's approval).
- [ ] **R8** Release 0.4.0 and GitOps change (each step with the user's approval).

## Verification
1. `cargo test` and `scripts/review.sh` pass.
2. `tests/e2e/run.sh` (all scenarios, `replace` included) passes against local Syncthing.
3. Live: a `Replaced …` log line once a matching file is re-sent, and no new `_N` copy for it.

## Progress log
- 2026-10-31 — Plan approved. Design choices recorded in 0012 (proposed).
- 2026-10-31 — R2–R4: `globset` 0.4.20 (2026-08-04; 0.4.21 is four days old, so not used). 46 unit tests
  pass, 12 of them new (5 config, 7 mover). The forced verification on replace is visible only in the code
  (`copy_durable(…, true, Place::Replace)`); no test can make a correct copy fail verification.
- 2026-10-31 — R5–R6: e2e `replace` scenario added. The first run used `replace/**`, which never matched,
  because patterns include the Syncthing folder's path (`phone/photos/replace/**`); the README now says so.
  The full suite (8 scenarios) passes against Syncthing v2.1.7. Earlier runs failed in `burst` only because
  `/tmp` (tmpfs) was short of space for the 2 GB file. README and CLAUDE.md updated (invariant 1).
- 2026-10-31 — R7: review 0004, two rounds (20 findings: 16 fixed in c7d8088 and e6b1dab, 1 partly
  fixed, 3 won't fix with reasons). review.sh passes (51 tests); e2e passes at e6b1dab. Next: PR, merge
  only with the user's approval.
- 2026-10-31 — The user approved the merge ("Yes, go ahead"): 0012 accepted, version 0.4.0.
