use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

const MIN_SCAN_INTERVAL_SECS: u64 = 10;

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
    /// Paths whose backup copy a newer version replaces instead of getting a `_N` name.
    pub replace: Option<ReplaceRules>,
}

/// `REPLACE_PATTERNS` (decision 0012): globs on the path relative to `DESTINATION_DIRECTORY`. `*`
/// stays within one directory, `**` crosses directories.
#[derive(Debug, Clone)]
pub struct ReplaceRules {
    pub patterns: Vec<String>,
    set: GlobSet,
}

impl ReplaceRules {
    /// `None` when `spec` holds no pattern. A pattern that can never match a relative path, or does
    /// not parse, is an error: a rule silently matching nothing would hide a misconfiguration.
    pub fn parse(spec: &str) -> Result<Option<Self>> {
        let patterns: Vec<String> = spec
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = GlobSetBuilder::new();
        for p in &patterns {
            if p.starts_with('/') {
                anyhow::bail!(
                    "REPLACE_PATTERNS: {:?} must be relative to DESTINATION_DIRECTORY",
                    p
                );
            }
            let glob = GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .with_context(|| format!("REPLACE_PATTERNS: invalid pattern {:?}", p))?;
            builder.add(glob);
        }
        let set = builder.build().context("REPLACE_PATTERNS")?;
        Ok(Some(Self { patterns, set }))
    }

    /// `rel`: destination path relative to `DESTINATION_DIRECTORY`.
    pub fn matches(&self, rel: &Path) -> bool {
        self.set.is_match(rel)
    }
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
            scan_interval: Duration::from_secs(scan_interval_secs()?),
            verify_copy: env_bool("VERIFY_COPY", true)?,
            min_age: Duration::from_secs(env_u64("MIN_AGE_SECS", 30)?),
            replace: ReplaceRules::parse(&std::env::var("REPLACE_PATTERNS").unwrap_or_default())?,
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

/// 0 would rescan the whole source tree in a tight loop.
fn scan_interval_secs() -> Result<u64> {
    let secs = env_u64("SCAN_INTERVAL_SECS", 900)?;
    if secs < MIN_SCAN_INTERVAL_SECS {
        anyhow::bail!(
            "Env var SCAN_INTERVAL_SECS must be at least {}, got {}",
            MIN_SCAN_INTERVAL_SECS,
            secs
        );
    }
    Ok(secs)
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(v) => v
            .parse()
            .with_context(|| format!("Env var {} must be an integer, got {:?}", name, v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(spec: &str) -> ReplaceRules {
        ReplaceRules::parse(spec).unwrap().unwrap()
    }

    #[test]
    fn replace_patterns_off_when_empty() {
        assert!(ReplaceRules::parse("").unwrap().is_none());
        assert!(ReplaceRules::parse(" , ,").unwrap().is_none());
    }

    #[test]
    fn replace_patterns_list() {
        let r = rules(" Apps/Backups/** , **/*.db");
        assert_eq!(r.patterns, vec!["Apps/Backups/**", "**/*.db"]);
        assert!(r.matches(Path::new("Apps/Backups/app.bak")));
        assert!(r.matches(Path::new("Apps/Backups/deep/x.bin")));
        assert!(r.matches(Path::new("Other/settings.db")));
        assert!(r.matches(Path::new("settings.db")));
        assert!(!r.matches(Path::new("Apps/Media/photo.jpg")));
        assert!(!r.matches(Path::new("Apps/Backups")));
    }

    #[test]
    fn replace_star_stays_in_one_directory() {
        let r = rules("Apps/*.db");
        assert!(r.matches(Path::new("Apps/app.db")));
        assert!(!r.matches(Path::new("Apps/sub/app.db")));
        let r = rules("Apps/**/*.db");
        assert!(r.matches(Path::new("Apps/app.db")));
        assert!(r.matches(Path::new("Apps/sub/app.db")));
    }

    #[test]
    fn replace_patterns_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let r = rules("Apps/**");
        assert!(r.matches(Path::new(std::ffi::OsStr::from_bytes(b"Apps/\xff.db"))));
    }

    #[test]
    fn replace_patterns_invalid() {
        assert!(ReplaceRules::parse("Apps/[a").is_err());
        assert!(ReplaceRules::parse("/Apps/**").is_err());
    }
}
