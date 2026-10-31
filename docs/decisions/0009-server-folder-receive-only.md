# 0009 — Server folder type: Receive Only instead of Send & Receive
- Status: accepted (supersedes the folder-type part of 0005)
- Date: 2026-10-30
- Type: product
- Decided by: user, 2026-10-30: "I approve 0008 and 0009, I'll switch to Receive Only". Claude proposed it, based on the T9 end-to-end tests.

## Context
Decision 0005 kept the user's setup: phone folders Send Only + Ignore Delete, server folders **Send &
Receive**. The T9 end-to-end tests ran real Syncthing v2.1.7 instances against the porter, and found a gap
in backup coverage that depends only on the server folder type.

## Finding
Scenario `edit`: a photo is backed up and moved off the server, then **edited on the phone**.
- **Send & Receive server**: the edited version is **never transferred**, so it is never backed up. The
  server's deletion `{phone:…219, server:…221}` and the phone's edit `{phone:…221}` are concurrent versions,
  and Syncthing keeps the **deletion** as the global version. The phone just shows "out of sync".
- **Receive Only server**: the deletion stays local (it is never announced), the edit is transferred, and
  the porter stores it as `edit_1.jpg` next to the untouched original (decision 0004).

## Decision
Option 1: the server folders switch to Receive Only. The user changes the Syncthing configuration. The
porter needs no code change, and the end-to-end suite now defaults to `receiveonly`.

## Options
1. **Switch the server folders to Receive Only (chosen).** Edits on the phone are backed up, and
   deletions caused by the porter can never leave the server, even towards another device that shares the
   folder (the risk behind the allowlist in 0005). What to watch for: the server GUI shows "Revert Local
   Changes". Clicking it would download everything again. That is harmless, because the porter deduplicates
   identical files (0004), but it is wasted transfer.
2. Keep Send & Receive. Accept that a file edited on the phone after its first backup is not backed up again.

## Evidence
- Version vectors observed with Send & Receive (diagnostics from `/rest/db/file?folder=photos&file=edit.jpg`
  on both instances, recorded in `docs/reviews/0001-R1-e2e-sendreceive-edit.log`):
  phone local `deleted=False size=12000 version=[phone:1793398221]`; global on both sides
  `deleted=True version=[phone:1793398219, server:1793398221]`.
- End-to-end logs (Syncthing v2.1.7, `tests/e2e/run.sh`):
  - `docs/reviews/0001-R1-e2e-receiveonly.log`: all 7 scenarios pass, including `edit`.
  - `docs/reviews/0001-R1-e2e-sendreceive.log`: every scenario except `edit` passes.
  - `docs/reviews/0001-R1-e2e-sendreceive-edit.log`: `edit` fails, with diagnostics.
- Folder type docs (https://docs.syncthing.net/users/foldertypes.html, retrieved 2026-10-30), Receive Only:
  "Local changes are however not distributed to other devices"; "Revert Local Changes" re-syncs
  "modified or deleted files … from the cluster".
- **Unverified**: why Syncthing picks the deletion in a delete/modify conflict. This was observed, not
  traced in the source. Behaviour may differ in other Syncthing versions, so re-run `tests/e2e/run.sh edit`
  after upgrading Syncthing.
