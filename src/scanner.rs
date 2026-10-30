//! Rebuilds the queue from the source folders: every regular file left there is pending
//! (docs/decisions/0002).

use crate::mover::{self, Job};
use crate::syncthing::FolderMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

#[derive(Debug, Default)]
pub struct ScanResult {
    pub jobs: Vec<Job>,
    /// Files skipped because they changed less than `min_age` ago.
    pub deferred: usize,
    /// Age of the oldest pending file (by inode change time).
    pub oldest: Option<Duration>,
    /// Directories that could not be read.
    pub errors: usize,
    /// Files left under the mover's private removal name (see `mover::REMOVING_PREFIX`).
    pub leftovers: usize,
}

/// Syncthing's own bookkeeping and in-progress files.
pub fn is_ignored(name: &OsStr) -> bool {
    let n = name.as_bytes();
    n == b".stfolder"
        || n == b".stignore"
        || n == b".stversions"
        || n.starts_with(b".stfolder.removed-")
        || (n.starts_with(b".syncthing.") && n.ends_with(b".tmp"))
        || (n.starts_with(b"~syncthing~") && n.ends_with(b".tmp"))
}

pub fn scan(
    src_root: &Path,
    dst_root: &Path,
    folders: &FolderMap,
    min_age: Duration,
    now: SystemTime,
) -> ScanResult {
    let mut result = ScanResult::default();
    for rel in folders.values() {
        let root = src_root.join(rel);
        if !root.is_dir() {
            warn!("Folder {:?} does not exist under SOURCE_DIRECTORY", root);
            result.errors += 1;
            continue;
        }
        walk(
            &root,
            &src_root.join(rel),
            &dst_root.join(rel),
            min_age,
            now,
            &mut result,
        );
    }
    result
}

fn walk(
    dir: &Path,
    src_base: &Path,
    dst_base: &Path,
    min_age: Duration,
    now: SystemTime,
    out: &mut ScanResult,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Cannot read {:?}: {}", dir, e);
            out.errors += 1;
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("Cannot read an entry of {:?}: {}", dir, e);
                out.errors += 1;
                continue;
            }
        };
        if is_ignored(&entry.file_name()) {
            continue;
        }
        if entry
            .file_name()
            .as_bytes()
            .starts_with(mover::REMOVING_PREFIX.as_bytes())
        {
            // Never moved automatically: it is either an already backed-up copy (crash during
            // removal) or a newer version that could not be put back. A human decides.
            warn!(
                "Leftover of an interrupted removal: {:?}. Compare it with the backup and move it by hand",
                entry.path()
            );
            out.leftovers += 1;
            continue;
        }
        if entry.file_name() == mover::TMP_NAME {
            warn!("Skipping {:?}: reserved name", entry.path());
            continue;
        }
        let path = entry.path();
        // symlink_metadata: symlinks are neither followed nor moved.
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue, // vanished meanwhile
        };
        if meta.is_dir() {
            walk(&path, src_base, dst_base, min_age, now, out);
        } else if meta.file_type().is_file() {
            let changed = UNIX_EPOCH
                + Duration::new(
                    meta.ctime().max(0) as u64,
                    meta.ctime_nsec().clamp(0, 999_999_999) as u32,
                );
            let age = now.duration_since(changed).unwrap_or_default();
            if age < min_age {
                out.deferred += 1;
                continue;
            }
            out.oldest = out.oldest.max(Some(age));
            if let Ok(rel) = path.strip_prefix(src_base) {
                out.jobs.push(Job {
                    src: path.clone(),
                    dst: dst_base.join(rel),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ignore_rules() {
        for name in [
            ".stfolder",
            ".stignore",
            ".stversions",
            ".syncthing.a.jpg.tmp",
            "~syncthing~a.jpg.tmp",
            ".stfolder.removed-123",
        ] {
            assert!(is_ignored(OsStr::new(name)), "{}", name);
        }
        for name in ["a.jpg", ".hidden", "syncthing.tmp", ".syncthing.notes"] {
            assert!(!is_ignored(OsStr::new(name)), "{}", name);
        }
    }

    #[test]
    fn scans_files_and_maps_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("st");
        let dst = dir.path().join("hdd");
        let folder = src.join("phone/photos");
        fs::create_dir_all(folder.join("DCIM/.stversions")).unwrap();
        fs::create_dir_all(folder.join(".stfolder")).unwrap();
        fs::write(folder.join("DCIM/a.jpg"), b"a").unwrap();
        fs::write(folder.join("DCIM/.syncthing.b.jpg.tmp"), b"partial").unwrap();
        fs::write(folder.join("DCIM/.stversions/old.jpg"), b"v").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", folder.join("link")).unwrap();

        let folders = FolderMap::from([("photos".to_string(), PathBuf::from("phone/photos"))]);
        let later = SystemTime::now() + Duration::from_secs(3600);
        let r = scan(&src, &dst, &folders, Duration::from_secs(60), later);
        assert_eq!(
            r.jobs,
            vec![Job {
                src: folder.join("DCIM/a.jpg"),
                dst: dst.join("phone/photos/DCIM/a.jpg")
            }]
        );
        assert_eq!(r.errors, 0);
        assert_eq!(r.leftovers, 0);

        // Same files seen "now" are too young.
        let r = scan(
            &src,
            &dst,
            &folders,
            Duration::from_secs(60),
            SystemTime::now(),
        );
        assert!(r.jobs.is_empty());
        assert_eq!(r.deferred, 1);
    }

    #[test]
    fn leftovers_are_reported_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("st/photos");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(format!("{}123", mover::REMOVING_PREFIX)), b"x").unwrap();
        fs::write(folder.join(mover::TMP_NAME), b"x").unwrap();
        let folders = FolderMap::from([("photos".to_string(), PathBuf::from("photos"))]);
        let later = SystemTime::now() + Duration::from_secs(3600);
        let r = scan(
            &dir.path().join("st"),
            dir.path(),
            &folders,
            Duration::ZERO,
            later,
        );
        assert!(r.jobs.is_empty());
        assert_eq!(r.leftovers, 1);
    }
}
