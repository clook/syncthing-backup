# 0005 — Folder scope: Send & Receive server folders, optional `FOLDER_IDS` allowlist
- Status: accepted; folder type superseded by 0009 (server folders become Receive Only)
- Date: 2026-10-30
- Type: product
- Decided by: user. Server folders are "Send & Receive". The instance has "Only phone folders", so the
  allowlist is optional.

## Context
The porter moves files out of every Syncthing folder under `SYNCTHING_STORAGE_DIRECTORY`. On a Send &
Receive folder, a local deletion is sent to every device that shares the folder. For the phone this is
harmless, because it is send-only. For any other device it would delete data.

## Decision
Keep the current behaviour: process every folder under the storage root. Skip paused folders. Add an
optional `FOLDER_IDS` allowlist and warn about IDs in it that cannot be mapped. Reconsider making the
allowlist mandatory if the instance ever shares a folder with a non-send-only device.

## Evidence
- Folder mapping without filtering: src/main.rs:84-89 @ 1057e25.
- https://docs.syncthing.net/users/foldertypes.html (retrieved 2026-10-30), send-only: "all changes from
  other devices in the cluster are ignored". So the deletion does not reach the phone.
- Tests (T3): `syncthing::tests::allowlist_filters_folders`, `maps_folders_under_storage_root_only`.
