# Syncthing Backup Porter

A robust service to move Syncthing-synchronized files to persistent storage (HDD/NAS). Designed for reliability, low memory footprint, and mechanical drive health.

> [!IMPORTANT]  
> **AI Collaboration Disclosure**: This entire project, including the Rust implementation, Docker optimization, and CI/CD workflows, was generated and refined during an interactive pair-programming session with **Gemini (Google AI)**.

## 🚀 Features

- **Sequential HDD Optimization**: Uses a Command pattern to ensure only one heavy I/O operation happens at a time, preventing disk thrashing.
- **Smart Comparison**: Block-by-block file comparison (512KB chunks) to avoid redundant writes.
- **Resilient State**: Uses Redis to track the event cursor and manage a `retry_queue` for failed operations.
- **Multi-Arch**: Native support for `amd64` and `arm64` (Raspberry Pi compatible).
- **Mount Safety**: Optional witness file check to prevent writing to the root partition if the drive is disconnected.

## 🛠 Configuration

| Env Variable | Description |
|--------------|-------------|
| `SYNCTHING_URL` | API URL (default: `http://localhost:8384`) |
| `SYNCTHING_KEY` | **Required** API Key |
| `SOURCE_DIRECTORY` | Local path where Syncthing downloads files |
| `DESTINATION_DIRECTORY` | Local path to move files to |
| `EXISTING_STRATEGY` | `nothing`, `overwrite`, `different`, `suffix` |
| `WITNESS_FILE` | Filename to check for on destination (e.g. `.mounted`) |
| `REDIS_HOST` | Redis server address |

## 📦 Deployment

### Docker Compose
```yaml
services:
  backup-porter:
    image: clook/syncthing-backup:latest
    environment:
      - SYNCTHING_KEY=YOUR_API_KEY
      - SOURCE_DIRECTORY=/syncthing
      - DESTINATION_DIRECTORY=/backup
      - REDIS_HOST=redis
      - EXISTING_STRATEGY=different
      - WITNESS_FILE=.mounted
    volumes:
      - /path/to/st_data:/syncthing
      - /path/to/hdd:/backup
    restart: unless-stopped

  redis:
    image: redis:alpine
