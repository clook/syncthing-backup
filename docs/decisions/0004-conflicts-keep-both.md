# 0004 — Name conflicts: keep both files, never overwrite
- Status: accepted
- Date: 2026-10-30
- Type: product
- Decided by: user ("Keep both (suffix)"). The `overwrite` and `different` modes are removed.

## Context
A file can arrive whose name already exists in the backup with different content. For example: an edited
photo, a filename reused after a phone reset, or every file re-sent after someone clicks "Override Changes"
on the phone.

## Decision
Never overwrite. Try `name.ext`, then `name_1.ext`, `name_2.ext`… If one of them has identical content, the
file is a duplicate: delete the source and write nothing. Otherwise write to the first free name.
`EXISTING_STRATEGY` is ignored, and the porter warns if it is set.

## Evidence
- Old behaviour: `overwrite` and `different` both end in `MoveTo(dst)` onto the existing file
  (src/main.rs:125, 146 @ 1057e25). The previous backup is replaced, so data is lost.
- The old default `nothing` returns `Skip` (src/main.rs:124 @ 1057e25), and the source file is never moved.
  An edited photo was therefore never backed up, and nothing reported it.
- The old suffix mode only compared against the unsuffixed name (src/main.rs:129-143 @ 1057e25). A second
  re-send therefore created `_2`, `_3`… duplicates. It also panicked on non-UTF-8 names (`to_str().unwrap()`,
  line 139-140).
- Mass re-sends are realistic. From https://docs.syncthing.net/users/foldertypes.html (retrieved 2026-10-30):
  "Override Changes" makes "any changes made to files … overwritten by the version on this host".
- Tests (T4): `mover::tests::conflict_creates_suffix`, `identical_suffix_is_deduplicated`,
  `non_utf8_name_suffix`.
