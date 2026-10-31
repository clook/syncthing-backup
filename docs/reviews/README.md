# Review records

Each `NNNN-<tasks>.md` file records one review of a set of tasks. `scripts/check-reviewed.sh` (used by the
pre-push hook and CI) reads the **newest** record. It only accepts the pushed code if nothing under
`src/`, `Cargo.*` or `Dockerfile` has changed since that record's `Reviewed-Commit`.

## Template

```markdown
# NNNN — Review of T2–T4

Reviewed-Commit: <sha of the last code commit reviewed, after fixes>
Range: <previous Reviewed-Commit>..<sha>
Plan: docs/plans/0001-folder-as-queue.md

## Mechanical checks
scripts/review.sh: passed (N tests)

## Findings
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 1 | high | src/mover.rs:42 | … | fixed in <sha> / won't fix: <reason> |

## Invariants
1–5 from CLAUDE.md checked: ok / notes
```
