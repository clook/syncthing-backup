use crate::config::AppConfig;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, warn};

/// Long-poll timeout requested from Syncthing; the HTTP timeout must be longer.
const EVENTS_TIMEOUT_SECS: u64 = 60;

/// Syncthing folder ID -> folder path relative to the storage root.
pub type FolderMap = HashMap<String, PathBuf>;

#[derive(Debug, Deserialize, Clone)]
pub struct Event {
    pub id: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ItemFinishedData {
    pub item: String,
    pub folder: String,
    pub action: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub error: Option<Value>, // null, or a string when the item failed
}

#[derive(Debug, Deserialize)]
struct SyncthingConfig {
    folders: Vec<FolderConfig>,
}

#[derive(Debug, Deserialize)]
pub struct FolderConfig {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Deserialize)]
struct SystemStatus {
    #[serde(rename = "startTime")]
    start_time: String,
}

pub struct Client {
    http: reqwest::Client,
    url: String,
    api_key: String,
}

impl Client {
    pub fn new(cfg: &AppConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(EVENTS_TIMEOUT_SECS + 30))
            .build()?;
        Ok(Self {
            http,
            url: cfg.st_url.trim_end_matches('/').to_string(),
            api_key: cfg.st_api_key.clone(),
        })
    }

    async fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> Result<T> {
        let res = self
            .http
            .get(format!("{}{}", self.url, path_and_query))
            .header("X-API-Key", &self.api_key)
            .send()
            .await?
            .error_for_status()?;
        res.json()
            .await
            .with_context(|| format!("Invalid JSON from {}", path_and_query))
    }

    /// Fetches the folder list from Syncthing and maps IDs to paths relative to the storage root.
    pub async fn folders(&self, cfg: &AppConfig) -> Result<FolderMap> {
        let st_cfg: SyncthingConfig = self.get("/rest/system/config").await?;
        Ok(map_folders(st_cfg.folders, cfg))
    }

    /// Changes whenever Syncthing restarts (event IDs then start again from 1).
    pub async fn start_time(&self) -> Result<String> {
        let status: SystemStatus = self.get("/rest/system/status").await?;
        Ok(status.start_time)
    }

    /// Long-polls for events with an ID greater than `since`.
    pub async fn events(&self, since: u64) -> Result<Vec<Event>> {
        self.get(&format!(
            "/rest/events?since={}&timeout={}&events=ItemFinished,ConfigSaved",
            since, EVENTS_TIMEOUT_SECS
        ))
        .await
    }
}

/// Keeps unpaused folders that live under the storage root and, when set, are in `FOLDER_IDS`.
pub fn map_folders(folders: Vec<FolderConfig>, cfg: &AppConfig) -> FolderMap {
    let mut map = FolderMap::new();
    for f in folders {
        if let Some(allowed) = &cfg.folder_ids
            && !allowed.contains(&f.id)
        {
            continue;
        }
        if f.paused {
            debug!("Skipping paused folder {}", f.id);
            continue;
        }
        match PathBuf::from(&f.path).strip_prefix(&cfg.st_storage_dir) {
            Ok(rel) => {
                map.insert(f.id, rel.to_path_buf());
            }
            Err(_) => warn!(
                "Skipping folder {}: path {:?} is not under SYNCTHING_STORAGE_DIRECTORY {:?}",
                f.id, f.path, cfg.st_storage_dir
            ),
        }
    }
    if let Some(allowed) = &cfg.folder_ids {
        for id in allowed.iter().filter(|id| !map.contains_key(*id)) {
            warn!(
                "FOLDER_IDS contains {} but it is not an active, mappable folder",
                id
            );
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cfg(folder_ids: Option<&[&str]>) -> AppConfig {
        AppConfig {
            st_url: String::new(),
            st_api_key: String::new(),
            src_dir: PathBuf::from("/src"),
            dst_dir: PathBuf::from("/dst"),
            st_storage_dir: PathBuf::from("/var/syncthing"),
            witness_file: None,
            dry_run: false,
            folder_ids: folder_ids
                .map(|ids| ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>()),
            scan_interval: Duration::from_secs(1),
            verify_copy: true,
            min_age: Duration::from_secs(0),
        }
    }

    fn folder(id: &str, path: &str, paused: bool) -> FolderConfig {
        FolderConfig {
            id: id.into(),
            path: path.into(),
            paused,
        }
    }

    #[test]
    fn maps_folders_under_storage_root_only() {
        let map = map_folders(
            vec![
                folder("photos", "/var/syncthing/phone/photos", false),
                folder("outside", "/home/user/docs", false),
                folder("paused", "/var/syncthing/phone/paused", true),
            ],
            &cfg(None),
        );
        assert_eq!(map.len(), 1);
        assert_eq!(map["photos"], PathBuf::from("phone/photos"));
    }

    #[test]
    fn allowlist_filters_folders() {
        let map = map_folders(
            vec![
                folder("photos", "/var/syncthing/photos", false),
                folder("laptop", "/var/syncthing/laptop", false),
            ],
            &cfg(Some(&["photos"])),
        );
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["photos"]);
    }

    #[test]
    fn parses_item_finished_event() {
        let ev: Event = serde_json::from_str(
            r#"{"id":12,"globalID":40,"type":"ItemFinished","time":"2026-01-01T00:00:00Z",
                "data":{"item":"DCIM/a.jpg","folder":"photos","error":null,"type":"file","action":"update"}}"#,
        )
        .unwrap();
        let data: ItemFinishedData = serde_json::from_value(ev.data.unwrap()).unwrap();
        assert_eq!(data.item, "DCIM/a.jpg");
        assert!(data.error.is_none());
    }
}
