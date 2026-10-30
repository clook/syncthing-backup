# 0007 — Plans, review harness and decision records live in the repo
- Status: accepted
- Date: 2026-10-30
- Type: process
- Decided by: user, in three requests during the session that produced plan 0001.

## Context
This is a backup tool, so changes must be traceable. Work also has to carry over between Claude sessions.

## Decision
- Plans: `docs/plans/NNNN-*.md`, with task IDs, a checklist and a progress log. Commits are prefixed with
  the task ID.
- Review harness: `scripts/review.sh` (mechanical checks), a code review per set of tasks recorded in
  `docs/reviews/`, and a pre-push hook plus CI that block code which has not been reviewed.
- Decisions: `docs/decisions/` (this folder), with evidence.
- Everything is described in `CLAUDE.md`.

## Evidence
- The user's requests: "write this plan so we are able to track the changes … and survive the Claude
  sessions"; "build a harness: for each task or set of tasks completed, you are able to run a review (at
  least before pushing to GitHub)"; "I need evidences to be recorded as well so the decision taken (product
  or technical) must be tracked."
- Harness self-test: `scripts/review.sh` run against the old code (1057e25) flagged unwraps (main.rs:139-140),
  file operations outside the mover (main.rs:181-189), the Redis dependency, and fmt/clippy failures.
  `check-reviewed.sh` refused a tree that had no review record. Both results are recorded in plan 0001's
  progress log, 2026-10-30.
