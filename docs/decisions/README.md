# Decision records

One file per product, technical or process decision: `NNNN-<slug>.md`. Records are never rewritten
after they are accepted. A later decision **supersedes** an earlier one, and both records link to each other.

## Rules
- **Who decided**: either `user` (quote their answer) or `proposed by Claude, approved by user`
  (name the approval: plan approval, a question answered, etc.).
- **Evidence is mandatory**. Each claim names its source: a code location pinned to a commit, a docs URL
  with a short verbatim quote and the date it was retrieved, upstream source pinned to a commit, a test
  name, or a review/log file. A claim that has not been verified is labelled **unverified**.
- Update the active plan and `CLAUDE.md` when a decision changes how we work.

## Template

```markdown
# NNNN — Title
- Status: proposed | accepted | superseded by NNNN
- Date: YYYY-MM-DD
- Type: product | technical | process
- Decided by: …

## Context
## Options considered
## Decision
## Evidence
## Consequences
```

## Index
| # | Decision | Type | Status |
|---|----------|------|--------|
| 0001 | Keep the custom porter instead of switching to a dedicated tool | product | accepted |
| 0002 | Source folder as the durable queue, drop Redis | technical | accepted |
| 0003 | Event handling: gap/restart detection and drain-then-rescan | technical | accepted |
| 0004 | Name conflicts: keep both files, never overwrite | product | accepted |
| 0005 | Folder scope: Send & Receive server folders, optional `FOLDER_IDS` allowlist | product | accepted, type superseded by 0009 |
| 0006 | Durable cross-device move | technical | accepted |
| 0007 | Plans, review harness and decision records live in the repo | process | accepted |
| 0008 | Mover hardening after review R1 | technical | accepted |
| 0009 | Server folder type: Receive Only instead of Send & Receive | product | accepted |
