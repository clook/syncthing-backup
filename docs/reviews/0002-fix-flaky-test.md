# 0002 — Review of the flaky-test fix

Reviewed-Commit: 7aa3dac
Range: 3b7f17a..7aa3dac
Plan: docs/plans/0001-folder-as-queue.md

## Context
CI failed on PR #6 (run 41220724984, pull_request event, commit 3b7f17a) in
`mover::tests::remove_ours_refuses_foreign_file`; the same commit passed on the push event (run
41220320727) and on main after the merge (run 41220938479). The PR was merged before this was noticed.

## Mechanical checks
scripts/review.sh at 7aa3dac: passed (34 tests). Full output: `0002-fix-flaky-test.log`.

## Findings
| # | Severity | File:line | Finding | Resolution |
|---|----------|-----------|---------|------------|
| 1 | low (test only) | src/mover.rs:910 | The test deleted "our" file before writing the "foreign" one at the same path; ext4 reused the inode number, so `remove_ours` correctly saw the same (device, inode) and removed it | fixed in 7aa3dac: the original is kept alive under another name, so its inode cannot be reused |

Production code is unchanged. In production, an inode at the same path with the same (device, inode)
can only be reused after our file was deleted, and only the porter deletes in the destination; another
disk mounted at that path has another device number. The other deletions in the tests were checked: the
directory test is rejected by the "regular file" check whatever its inode, and the witness test compares
no inodes. Reviewed inline by Claude (one test, 4 lines); no `/code-review` run.

## Invariants
1–5 from CLAUDE.md: unaffected (test-only change).
