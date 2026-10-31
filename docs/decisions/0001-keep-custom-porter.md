# 0001 — Keep the custom porter instead of switching to a dedicated tool
- Status: accepted
- Date: 2026-10-30
- Type: product
- Decided by: user. They chose to harden the tool (answered "Folder as queue, no Redis" and approved plan 0001).

## Context
The user asked whether to keep this project or switch to a free, open-source, self-hosted tool that runs on
K3s. Their requirements: arbitrary phone folders; files moved off the server to an HDD; files never re-sent
after they are moved; resilient, chunked transfers.

## Options considered
- **Immich / Ente** (self-hosted photo backup). Photos and videos only. The server is the photo library, so
  they are not built to "move files away and forget them". *Unverified*: how they behave when assets are
  deleted on the server was not tested.
- **Nextcloud auto-upload**. Heavy to run, and needs the same "move off" logic anyway. Not evaluated in depth.
- **Syncthing + this porter**: what runs today.

## Decision
Keep Syncthing + the porter. Harden it (plan 0001).

## Evidence
- Never re-sending comes from Syncthing itself, not from the porter. The Syncthing docs on folder types
  (https://docs.syncthing.net/users/foldertypes.html, retrieved 2026-10-30) say: "In send only mode, all
  changes from other devices in the cluster are ignored". So the phone (send-only) ignores the deletion the
  server records when the porter moves a file away.
- The porter is small: `src/main.rs` was 309 lines at commit 1057e25. That makes a full review practical.
- The user confirmed the current behaviour works ("even if the files are removed from the Syncthing
  instance on a server … they are not retried for sending").

## Consequences
- We own the safety of the move logic. Hence the review harness (0007) and the invariants in `CLAUDE.md`.
- The HDD holds a single copy. An offsite backup (restic/Kopia) is still recommended in the README.
