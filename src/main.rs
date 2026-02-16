use anyhow::{anyhow, Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tracing::{info, error, warn};

// --- Data Structures ---

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Event {
    id: u64,
    #[serde(rename = "globalID")]
    global_id: u64,
    #[serde(rename = "type")]
    event_type: String,
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ItemFinishedData {
    item: String,
    folder: String,
    action: String,
    #[serde(rename = "type")]
    item_type: String,
    error: Option<Value>, // Syncthing met null ou une chaîne si erreur
}

#[derive(Debug, Deserialize)]
struct SyncthingConfig {
    folders: Vec<FolderConfig>,
}

#[derive(Debug, Deserialize)]
struct FolderConfig {
    id: String,
    path: String,
}

#[derive(Clone)]
enum ExistingFileStrategy {
    DoNothing,
    Overwrite,
    OverwriteIfDifferent,
    RenameWithSuffix,
}

enum BackupAction {
    Skip,
    DeleteSource,
    MoveTo(PathBuf),
}

struct AppConfig {
    st_url: String,
    st_api_key: String,
    src_dir: PathBuf,
    dst_dir: PathBuf,
    st_storage_dir: PathBuf,
    dry_run: bool,
    witness_file: Option<String>,
    strategy: ExistingFileStrategy,
}

// --- Logic Layer (The "Brain") ---

fn files_are_identical_sync(path_a: &Path, path_b: &Path) -> Result<bool> {
    let meta_a = std::fs::metadata(path_a)?;
    let meta_b = std::fs::metadata(path_b)?;

    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }

    let mut r1 = BufReader::new(File::open(path_a)?);
    let mut r2 = BufReader::new(File::open(path_b)?);
    let mut buf1 = [0u8; 1024 * 512]; // 512KB for HDD sequential access
    let mut buf2 = [0u8; 1024 * 512];

    loop {
        let n1 = r1.read(&mut buf1)?;
        let n2 = r2.read(&mut buf2)?;
        if n1 != n2 || buf1[..n1] != buf2[..n2] {
            return Ok(false);
        }
        if n1 == 0 { break; }
    }
    Ok(true)
}

async fn determine_action(src: &Path, dst: &Path, strategy: &ExistingFileStrategy) -> Result<BackupAction> {
    if !dst.exists() {
        return Ok(BackupAction::MoveTo(dst.to_path_buf()));
    }

    match strategy {
        ExistingFileStrategy::DoNothing => Ok(BackupAction::Skip),
        ExistingFileStrategy::Overwrite => Ok(BackupAction::MoveTo(dst.to_path_buf())),
        ExistingFileStrategy::OverwriteIfDifferent | ExistingFileStrategy::RenameWithSuffix => {
            let s = src.to_path_buf();
            let d = dst.to_path_buf();
            let identical = tokio::task::spawn_blocking(move || files_are_identical_sync(&s, &d)).await??;

            if identical {
                return Ok(BackupAction::DeleteSource);
            }

            if let ExistingFileStrategy::RenameWithSuffix = strategy {
                let mut count = 1;
                let mut new_dst = dst.to_path_buf();
                while new_dst.exists() {
                    let stem = dst.file_stem().unwrap().to_str().unwrap();
                    let ext = dst.extension().map(|e| format!(".{}", e.to_str().unwrap())).unwrap_or_default();
                    new_dst = dst.with_file_name(format!("{}_{}{}", stem, count, ext));
                    count += 1;
                }
                Ok(BackupAction::MoveTo(new_dst))
            } else {
                Ok(BackupAction::MoveTo(dst.to_path_buf()))
            }
        }
    }
}

// --- Execution Layer ---

async fn process_event(
    event: &Event, 
    cfg: &AppConfig, 
    folders: &HashMap<String, PathBuf>, 
    redis: &mut redis::aio::ConnectionManager
) -> Result<()> {
    if event.event_type == "ItemFinished" {
        if let Some(raw_data) = &event.data {
            if let Ok(data) = serde_json::from_value::<ItemFinishedData>(raw_data.clone()) {
                if data.item_type == "file" && data.action == "update" && data.error.is_none() {
                    let rel_path = folders.get(&data.folder).context("Folder ID mapping missing")?;
                        let src = cfg.src_dir.join(rel_path).join(&data.item);
                        let dst = cfg.dst_dir.join(rel_path).join(&data.item);

                        if let Some(w) = &cfg.witness_file {
                            if !cfg.dst_dir.join(w).exists() {
                                return Err(anyhow!("Mount witness missing: {:?}", w));
                            }
                        }

                    if src.exists() {
                        let action = determine_action(&src, &dst, &cfg.strategy).await?;
        
                        match action {
                            BackupAction::Skip => info!("Skipped: {:?}", data.item),
                            BackupAction::DeleteSource => {
                                info!("File identical at destination. Purging source: {:?}", data.item);
                                if !cfg.dry_run { fs::remove_file(&src).await?; }
                            },
                            BackupAction::MoveTo(final_dst) => {
                                info!("Moving {:?} to {:?}", data.item, final_dst);
                                if !cfg.dry_run {
                                    if let Some(p) = final_dst.parent() { fs::create_dir_all(p).await?; }
                                    if fs::rename(&src, &final_dst).await.is_err() {
                                        fs::copy(&src, &final_dst).await?;
                                        fs::remove_file(&src).await?;
                                    }
                                }
                            }
                        }
                        let _: () = redis.hdel("retry_queue", event.id.to_string()).await?;
                    }
                }
            }
        }
    }

    Ok(())
}

// --- Main Runner ---

async fn run() -> Result<()> {
    info!("Starting Syncthing Backup Porter...");

    let dry_run = std::env::var("DRY_RUN").unwrap_or_default() == "true";
    let strategy = match std::env::var("EXISTING_STRATEGY").as_deref() {
        Ok("overwrite") => ExistingFileStrategy::Overwrite,
        Ok("different") => ExistingFileStrategy::OverwriteIfDifferent,
        Ok("suffix") => ExistingFileStrategy::RenameWithSuffix,
        _ => ExistingFileStrategy::DoNothing,
    };

    let cfg = AppConfig {
        st_url: std::env::var("SYNCTHING_URL").unwrap_or_else(|_| "http://localhost:8384".into()),
        st_api_key: std::env::var("SYNCTHING_KEY").context("Env var SYNCTHING_KEY is required")?,
        src_dir: PathBuf::from(std::env::var("SOURCE_DIRECTORY").context("Env var SOURCE_DIRECTORY is required")?),
        dst_dir: PathBuf::from(std::env::var("DESTINATION_DIRECTORY").context("Env var DESTINATION_DIRECTORY is required")?),
        st_storage_dir: PathBuf::from(std::env::var("SYNCTHING_STORAGE_DIRECTORY").unwrap_or_else(|_| "/".into())),
        witness_file: std::env::var("WITNESS_FILE").ok(),
        dry_run,
        strategy,
    };

    let redis_host = std::env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".into());
    let redis_client = redis::Client::open(format!("redis://{}/", redis_host))?;

    // ConnectionManager setup for Redis 1.0.3
    let mut redis_conn = redis::aio::ConnectionManager::new(redis_client).await
        .context("Failed to create Redis connection manager")?;
    
    let http_client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;

    info!("Fetching folder configuration from Syncthing...");
    let st_cfg: SyncthingConfig = http_client.get(format!("{}/rest/system/config", cfg.st_url))
        .header("X-API-Key", &cfg.st_api_key).send().await?.json().await?;
    
    let mut folders_map = HashMap::new();
    for f in st_cfg.folders {
        if let Ok(rel) = PathBuf::from(&f.path).strip_prefix(&cfg.st_storage_dir) {
            folders_map.insert(f.id, rel.to_path_buf());
        }
    }

    // 1. Retry Queue
    let retry_map: HashMap<String, String> = redis_conn.hgetall("retry_queue").await.unwrap_or_default();
    if !retry_map.is_empty() {
        info!("Found {} failed events in retry queue", retry_map.len());
        for (id, json) in retry_map {
            if let Ok(ev) = serde_json::from_str::<Event>(&json) {
                info!("Retrying event ID: {}", id);
                let _ = process_event(&ev, &cfg, &folders_map, &mut redis_conn).await;
            }
        }
    }

    // 2. Main Loop
    let mut last_id: u64 = std::env::var("START_ID").ok().and_then(|v| v.parse().ok())
        .unwrap_or(redis_conn.get("last_event_id").await.unwrap_or(0));

    info!("Entering main loop at Event ID: {}", last_id);

    loop {
        let url = format!("{}/rest/events?since={}&timeout=25", cfg.st_url, last_id);
        let res = http_client.get(&url).header("X-API-Key", &cfg.st_api_key).send().await;

        match res {
            Ok(response) => {
                if let Ok(events) = response.json::<Vec<Event>>().await {
                    for event in events {
                        let current_local_id = event.id;
                        if let Err(e) = process_event(&event, &cfg, &folders_map, &mut redis_conn).await {
                            error!("Event {} (GlobalID: {}) error: {}. Moving to retry queue.", current_local_id, event.global_id, e);
                            let serialized = serde_json::to_string(&event)?;
                            let _: () = redis_conn.hset("retry_queue", current_local_id.to_string(), serialized).await?;
                        }
                        last_id = current_local_id;
                        let _: () = redis_conn.set("last_event_id", last_id).await
                            .context("Can't update last event id on Redis")?;
                    }
                }
            },
            Err(e) => {
                warn!("Syncthing connection lost: {}. Retrying in 5s...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize structured logging
    tracing_subscriber::fmt::init();

    if let Err(e) = run().await {
        error!("FATAL: {:?}", e);
        std::process::exit(1);
    }
    Ok(())
}
