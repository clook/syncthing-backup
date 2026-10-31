# 0003 — Event handling: gap/restart detection and drain-then-rescan
- Status: accepted
- Date: 2026-10-30
- Type: technical
- Decided by: proposed by Claude, approved by user (plan 0001). This design was prompted by the user's
  burst scenario.

## Context
The user's scenario, which was hard in the Python version: one 1 GB file arrives, then 100 files of 200 KB.
During the long copy, events for the small files must not be lost.

## Decision
- The event poller is its own task. It only adds to the in-memory queue and does no disk I/O. Copies run
  in `spawn_blocking`, so polling continues while the disk is busy.
- Subscribe with `events=ItemFinished,ConfigSaved`. This gives a dedicated subscription with its own
  sequence of IDs.
- **Gap detection**: if the first returned ID is greater than `since + 1`, events were dropped, so trigger a
  full rescan.
- **Restart detection**: on any error or empty long-poll, compare `/rest/system/status` `startTime`. If it
  changed, set `since = 0` and trigger a rescan.
- **Drain-then-rescan**: when the worker empties the queue after at least one successful item, it rescans
  the source. Failed items alone do not trigger a rescan, so a failing file can't cause a hot loop.
- A periodic scan (`SCAN_INTERVAL_SECS`, default 900) is the final safety net.

## Evidence
- IDs are per subscription and only count matching events: `e.SubscriptionID = l.nextSubscriptionIDs[i]`
  is assigned inside the mask check (https://github.com/syncthing/syncthing/blob/ed38528d7fba3dd6a42c9c9fc6780af432dc15d2/lib/events/events.go, `sendEvent`). So with a filter, IDs are
  consecutive and a gap means a loss.
- The docs (https://docs.syncthing.net/rest/events-get.html, retrieved 2026-10-30) say a loss "can be
  detected by noting a discontinuity in the event IDs".
- A subscription for a custom mask is created lazily on the first request with that mask
  (https://github.com/syncthing/syncthing/blob/ed38528d7fba3dd6a42c9c9fc6780af432dc15d2/lib/api/api.go, `getEventSub`). Events between a Syncthing (re)start and the porter's first request
  are not buffered, which is why a restart triggers a rescan.
- Change made during planning: the first draft said gaps could not be detected with a filter. Reading the
  source showed they can, and gap detection was added. See plan 0001's progress log.
- Tests: `main::tests::queue_grows_while_worker_blocked` (T6). End-to-end burst check: plan 0001, T9.

## Consequences
Each rescan walks the source tree (readdir + stat). That is cheap on an SSD and touches the HDD not at all.
