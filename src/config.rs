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
    /// `None` when `spec` holds no pattern. Commas inside `{a,b}` belong to the pattern. A pattern
    /// that does not parse, or whose form can never match a destination path (absolute, `.` or
    /// `..` components, trailing `/`), is an error. Patterns that are well formed but match no
    /// configured folder are reported by `outside_folders`.
    pub fn parse(spec: &str) -> Result<Option<Self>> {
        let patterns: Vec<String> = split_patterns(spec)
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = GlobSetBuilder::new();
        for p in &patterns {
            // An empty component also rejects a leading or trailing `/` and `//`.
            if p.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
                anyhow::bail!(
                    "REPLACE_PATTERNS: {:?} must be a relative path like `folder/dir/**`, without `.`, `..`, `//` or a trailing `/`",
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

    /// Patterns whose literal leading directories lie outside every folder path (relative to the
    /// storage root): they can never match, e.g. when the folder's own path was left out.
    pub fn outside_folders<'a>(&self, folders: impl IntoIterator<Item = &'a Path>) -> Vec<&str> {
        let folders: Vec<&Path> = folders.into_iter().collect();
        self.patterns
            .iter()
            .filter(|p| {
                let literal: Vec<&str> = p
                    .split('/')
                    .take_while(|c| !c.contains(['*', '?', '[', '{', '\\']))
                    .collect();
                // The last component may be a file name: only directories are compared.
                let dirs = &literal[..literal.len().min(p.split('/').count() - 1)];
                let prefix: PathBuf = dirs.iter().collect();
                !dirs.is_empty()
                    && !folders
                        .iter()
                        .any(|f| f.starts_with(&prefix) || prefix.starts_with(f))
            })
            .map(String::as_str)
            .collect()
    }
}

/// Splits on commas outside `{…}` alternations. Braces inside `[…]` classes or escaped with `\\`
/// are literal and do not count.
fn split_patterns(spec: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0usize, 0);
    let (mut escaped, mut class) = (false, false);
    for (i, c) in spec.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' if !class => class = true,
            ']' if class => class = false,
            _ if class => {}
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&spec[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&spec[start..]);
    parts
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        for (var, reason) in OBSOLETE_VARS {
            if std::env::var_os(var).is_some() {
                warn!("{} is set but ignored since 0.3.0: {}", var, reason);
            }
        }

        let folder_ids = Some(env_string("FOLDER_IDS")?)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<HashSet<_>>()
            })
            .filter(|s| !s.is_empty());

        Ok(Self {
            st_url: env_or("SYNCTHING_URL", "http://localhost:8384")?,
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
            st_storage_dir: PathBuf::from(env_or("SYNCTHING_STORAGE_DIRECTORY", "/")?),
            witness_file: Some(env_string("WITNESS_FILE")?).filter(|s| !s.is_empty()),
            dry_run: env_bool("DRY_RUN", false)?,
            folder_ids,
            scan_interval: Duration::from_secs(scan_interval_secs()?),
            verify_copy: env_bool("VERIFY_COPY", true)?,
            min_age: Duration::from_secs(env_u64("MIN_AGE_SECS", 30)?),
            replace: ReplaceRules::parse(&env_string("REPLACE_PATTERNS")?)?,
        })
    }
}

/// Empty when unset; an error (not a silent default) when not valid UTF-8.
fn env_string(name: &str) -> Result<String> {
    env_or(name, "")
}

/// `default` when unset; an error (not a silent default) when not valid UTF-8.
fn env_or(name: &str, default: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) => Ok(v),
        Err(std::env::VarError::NotPresent) => Ok(default.to_string()),
        Err(e) => Err(e).with_context(|| format!("Env var {}", name)),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e).with_context(|| format!("Env var {}", name)),
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
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e).with_context(|| format!("Env var {}", name)),
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
        for bad in [
            "Apps/[a",
            "/Apps/**",
            "./Apps/**",
            "Apps/../x/**",
            "Apps/",
            "Apps//x",
        ] {
            assert!(ReplaceRules::parse(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn replace_patterns_alternation_keeps_commas() {
        let r = rules("Apps/**/*.{db,bak}, Other/x");
        assert_eq!(r.patterns, vec!["Apps/**/*.{db,bak}", "Other/x"]);
        assert!(r.matches(Path::new("Apps/a/app.bak")));
    }

    #[test]
    fn replace_patterns_literal_braces_do_not_join() {
        let r = rules(r"Apps/[{]x, Apps/\{y, phone/Backups/**");
        assert_eq!(r.patterns.len(), 3, "{:?}", r.patterns);
        assert!(r.matches(Path::new("phone/Backups/a.db")));
    }

    #[test]
    fn replace_patterns_outside_folders() {
        let r = rules("phone/Apps/**,replace/**,**/*.db,phone/notes.db,other/x/*.db");
        let folders = [Path::new("phone"), Path::new("tablet/docs")];
        assert_eq!(
            r.outside_folders(folders),
            vec!["replace/**", "other/x/*.db"]
        );
        // A pattern above the folder path can still match inside it.
        let r = rules("tablet/**");
        assert!(r.outside_folders([Path::new("tablet/docs")]).is_empty());
    }
}
