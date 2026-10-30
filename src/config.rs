use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

/// Variables from 0.2.x that are no longer used. They are reported at startup so a stale
/// deployment (e.g. `EXISTING_STRATEGY=overwrite`) is visible in the logs.
const OBSOLETE_VARS: &[(&str, &str)] = &[
    (
        "REDIS_HOST",
        "Redis is no longer used: the source folder is the queue",
    ),
    ("START_ID", "the event cursor is no longer persisted"),
    (
        "EXISTING_STRATEGY",
        "conflicts always keep both files (name_N.ext), nothing is overwritten",
    ),
];

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub st_url: String,
    pub st_api_key: String,
    pub src_dir: PathBuf,
    pub dst_dir: PathBuf,
    pub st_storage_dir: PathBuf,
    pub witness_file: Option<String>,
    pub dry_run: bool,
    /// When set, only these Syncthing folder IDs are processed.
    pub folder_ids: Option<HashSet<String>>,
    pub scan_interval: Duration,
    pub verify_copy: bool,
    /// Files changed more recently than this are left for a later pass.
    pub min_age: Duration,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        for (var, reason) in OBSOLETE_VARS {
            if std::env::var_os(var).is_some() {
                warn!("{} is set but ignored since 0.3.0: {}", var, reason);
            }
        }

        let folder_ids = std::env::var("FOLDER_IDS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<HashSet<_>>()
            })
            .filter(|s| !s.is_empty());

        Ok(Self {
            st_url: std::env::var("SYNCTHING_URL")
                .unwrap_or_else(|_| "http://localhost:8384".into()),
            st_api_key: std::env::var("SYNCTHING_KEY")
                .context("Env var SYNCTHING_KEY is required")?,
            src_dir: PathBuf::from(
                std::env::var("SOURCE_DIRECTORY")
                    .context("Env var SOURCE_DIRECTORY is required")?,
            ),
            dst_dir: PathBuf::from(
                std::env::var("DESTINATION_DIRECTORY")
                    .context("Env var DESTINATION_DIRECTORY is required")?,
            ),
            st_storage_dir: PathBuf::from(
                std::env::var("SYNCTHING_STORAGE_DIRECTORY").unwrap_or_else(|_| "/".into()),
            ),
            witness_file: std::env::var("WITNESS_FILE").ok().filter(|s| !s.is_empty()),
            dry_run: env_bool("DRY_RUN", false)?,
            folder_ids,
            scan_interval: Duration::from_secs(env_u64("SCAN_INTERVAL_SECS", 900)?),
            verify_copy: env_bool("VERIFY_COPY", true)?,
            min_age: Duration::from_secs(env_u64("MIN_AGE_SECS", 30)?),
        })
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            _ => anyhow::bail!("Env var {} must be true/false, got {:?}", name, v),
        },
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(v) => v
            .parse()
            .with_context(|| format!("Env var {} must be an integer, got {:?}", name, v)),
    }
}
