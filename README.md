# Syncthing Backup Porter

Moves files that your phone sends through Syncthing to long-term storage (HDD/NAS), one file at a time,
without ever overwriting an existing backup.

> [!IMPORTANT]
> **AI collaboration disclosure**: the first versions (up to 0.2.x) were written with **Gemini (Google AI)**.
> The 0.3 rework, review harness and documentation were written with **Claude (Anthropic)**. Decisions and
> their evidence are recorded in [`docs/decisions/`](docs/decisions/).

## How it works

```
Phone (Send Only, Ignore Delete) ──Syncthing──► server folder (Send & Receive, SSD)
                                                      │
          startup scan ─┐                             │ files waiting here = the queue
          periodic scan ├─► in-memory queue ─► ONE worker ─► HDD (durable copy, verified)
          events       ─┘                                      then source removed
```

- **Files are never sent again.** When the porter removes a file from the server folder, Syncthing records
  the deletion. The phone ignores it, because send-only folders ignore all remote changes.
- **The source folder is the queue.** A file stays there until it is safely on the HDD. Nothing else needs
  to survive a crash: the next scan finds whatever is left. No database or Redis is needed.
- **Events only add speed.** Syncthing `ItemFinished` events queue files as soon as they arrive. Scans
  catch anything the events missed: at startup, right after each burst of files is processed, after a
  Syncthing restart or a detected event gap, and every `SCAN_INTERVAL_SECS`.
- **Sequential HDD access.** A single worker copies one file at a time. Events keep being queued while a
  large file is being copied.

### Recommended Syncthing settings
| Device | Folder type | Option |
|--------|-------------|--------|
| Phone  | Send Only   | *Ignore Delete* enabled |
| Server | Send & Receive | (`Receive Only` also works) |

Do not share a folder that the porter processes with a device that is **not** send-only: on a Send & Receive
folder, the porter's deletions would reach that device. Use `FOLDER_IDS` to limit the porter to the phone
folders.

## Safety guarantees
1. **Nothing in the backup is ever overwritten.** If `photo.jpg` already exists with different content,
   the new file is saved as `photo_1.jpg` (then `_2`, …). If identical content is already there, the source
   is simply removed.
2. **The source is removed only after the copy is safe.** The file is copied under a temporary name and
   fsynced, then given its final name (an atomic operation that never replaces an existing file). It is
   then compared byte for byte with the source (`VERIFY_COPY`). Only then is the source removed, and only
   if it is still the same file that was copied.
3. **Destination not mounted, nothing written.** If `WITNESS_FILE` is missing, the porter pauses and logs
   an error.
4. **Failures are never silent.** A file that cannot be moved stays in the source and is retried on the
   next scan. Repeated failures are logged as errors. A file still waiting after 24h is reported on every
   scan.
5. **Modification times are preserved.**

Not covered: the HDD is a **single copy**. A copy verified right after writing is read back mostly from
memory, so later disk corruption is not detected. For a real 3-2-1 backup, back up the destination offsite
with [restic](https://restic.net/) or [Kopia](https://kopia.io/) (e.g. a K3s CronJob), and use a
checksumming filesystem or regular `restic check` runs.

## Configuration

| Env variable | Default | Description |
|--------------|---------|-------------|
| `SYNCTHING_URL` | `http://localhost:8384` | Syncthing API URL |
| `SYNCTHING_KEY` | **required** | Syncthing API key |
| `SOURCE_DIRECTORY` | **required** | Where the Syncthing storage root is mounted in this container |
| `DESTINATION_DIRECTORY` | **required** | Backup root (files keep their relative paths) |
| `SYNCTHING_STORAGE_DIRECTORY` | `/` | Storage root *as Syncthing sees it*. Folder paths are made relative to it |
| `WITNESS_FILE` | *(none, strongly recommended)* | File that must exist in `DESTINATION_DIRECTORY`, e.g. `.mounted` |
| `FOLDER_IDS` | *(all folders under the storage root)* | Comma-separated Syncthing folder IDs to process |
| `SCAN_INTERVAL_SECS` | `900` | Periodic full scan |
| `MIN_AGE_SECS` | `30` | Scans skip files changed more recently than this |
| `VERIFY_COPY` | `true` | Compare destination with source before removing the source |
| `DRY_RUN` | `false` | Log what would happen; write or remove nothing |

### Migrating from 0.2.x
- Redis is no longer used. Remove it, along with `REDIS_HOST`.
- `START_ID` and `EXISTING_STRATEGY` are ignored, and the porter logs a warning if they are set. Name
  conflicts now always keep both files.
- On first start, the porter scans the source folders and moves any files that 0.2.x left behind.
  Run it once with `DRY_RUN=true` to see what it would do.

## Deployment

### Docker Compose
```yaml
services:
  backup-porter:
    image: clook/syncthing-backup:0.3.0
    environment:
      - SYNCTHING_URL=http://syncthing:8384
      - SYNCTHING_KEY=YOUR_API_KEY
      - SYNCTHING_STORAGE_DIRECTORY=/var/syncthing
      - SOURCE_DIRECTORY=/syncthing
      - DESTINATION_DIRECTORY=/backup
      - WITNESS_FILE=.mounted
    volumes:
      - /path/to/st_data:/syncthing
      - /path/to/hdd:/backup
    stop_grace_period: 2m   # lets a large file finish copying
    restart: unless-stopped
```

### Kubernetes / K3s
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: syncthing-backup
spec:
  replicas: 1                 # never more than one: HDD access must stay sequential
  strategy:
    type: Recreate
  selector:
    matchLabels: { app: syncthing-backup }
  template:
    metadata:
      labels: { app: syncthing-backup }
    spec:
      terminationGracePeriodSeconds: 120
      containers:
        - name: porter
          image: clook/syncthing-backup:0.3.0
          env:
            - { name: SYNCTHING_URL, value: "http://syncthing:8384" }
            - name: SYNCTHING_KEY
              valueFrom: { secretKeyRef: { name: syncthing-api, key: key } }
            - { name: SYNCTHING_STORAGE_DIRECTORY, value: "/var/syncthing" }
            - { name: SOURCE_DIRECTORY, value: "/syncthing" }
            - { name: DESTINATION_DIRECTORY, value: "/backup" }
            - { name: WITNESS_FILE, value: ".mounted" }
          volumeMounts:
            - { name: st-data, mountPath: /syncthing }
            - { name: hdd, mountPath: /backup }
      volumes:
        - name: st-data
          persistentVolumeClaim: { claimName: syncthing-data }
        - name: hdd
          hostPath: { path: /mnt/hdd/backup, type: Directory }
```
Create the witness file once on the mounted disk: `touch /mnt/hdd/backup/.mounted`. If the disk is ever not
mounted, the directory underneath has no witness, so the porter writes nothing.

## Development
See [`CLAUDE.md`](CLAUDE.md) for the safety invariants and how changes are planned, reviewed and recorded:

- [`docs/plans/`](docs/plans/): plans with task checklists and progress logs
- [`docs/decisions/`](docs/decisions/): decision records with their evidence
- [`docs/reviews/`](docs/reviews/): review records

`scripts/review.sh` runs the mechanical checks (fmt, clippy, tests, invariant checks). The pre-push hook
(`git config core.hooksPath .githooks`) and CI require that every code change is covered by a review record.
