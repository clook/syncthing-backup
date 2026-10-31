# 0002 — Source folder as the durable queue, drop Redis
- Status: accepted
- Date: 2026-10-30
- Type: technical
- Decided by: proposed by Claude, approved by user ("Folder as queue, no Redis (Recommended)").

## Context
Redis held the event cursor (`last_event_id`) and a `retry_queue`. That came from the Python version: after a
crash, the program had to know what was done and what was left. The user wants crash recovery, protection
against Syncthing's limited event buffer, and strictly sequential HDD access.

## Options considered
1. Keep Redis and fix the bugs.
2. Keep Redis as the work queue and add a source scan.
3. **No external state**: every file still in the source folder is pending. A scan rebuilds the queue, and an
   in-memory FIFO feeds one worker.

## Decision
Option 3. A file is removed from the source only once it is durably in the destination (0006). So after a
crash, the next startup scan finds everything that is left, and nothing else needs to survive.

## Evidence
- Old cursor bug: `last_id` is read back from Redis at startup (src/main.rs:255-256 @ 1057e25) and used as
  `since`. Event IDs only live in memory in Syncthing: `nextSubscriptionIDs` starts every subscription at 1,
  and nothing persists it (https://github.com/syncthing/syncthing/blob/ed38528d7fba3dd6a42c9c9fc6780af432dc15d2/lib/events/events.go, functions `Subscribe`/`sendEvent`). After a Syncthing
  restart, `since=<old id>` matches nothing until the new IDs catch up, so files are skipped silently.
- Retry entries are keyed by event ID (src/main.rs:274 @ 1057e25). Because IDs restart, a new failure can
  overwrite an older entry in the `retry_queue` hash.
- The retry queue is only replayed at startup (src/main.rs:240-252 @ 1057e25).
- Syncthing's event buffer is finite: `EventSubBufferSize = 1000` (https://github.com/syncthing/syncthing/blob/ed38528d7fba3dd6a42c9c9fc6780af432dc15d2/lib/api/api.go). The docs
  (https://docs.syncthing.net/rest/events-get.html, retrieved 2026-10-30) say: "There is a limit to the
  number of events buffered."
- `scripts/review.sh` enforces "no Redis dependency".

## Consequences
- One less component in K3s. No state to back up or migrate. `REDIS_HOST` and `START_ID` become obsolete,
  and the porter warns at startup if they are set.
- Files that fail stay in the source and are retried by the next scan. A file stuck for more than 24h is
  logged as a warning.
