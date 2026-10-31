//! Rebuilds the queue from the source folders: every regular file left there is pending
//! (docs/decisions/0002).

use crate::mover::{self, Job};
use crate::syncthing::{FolderMap, FolderRoots};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

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

/// Syncthing's own bookkeeping (only at the folder root, deeper they are ordinary synced files)
/// and in-progress files (anywhere).
pub fn is_ignored(name: &OsStr, at_root: bool) -> bool {
    let n = name.as_bytes();
    (at_root
        && (n == b".stfolder"
            || n == b".stignore"
            || n == b".stversions"
            || n.starts_with(b".stfolder.removed-")))
        || (n.starts_with(b".syncthing.") && n.ends_with(b".tmp"))
        || (n.starts_with(b"~syncthing~") && n.ends_with(b".tmp"))
}

pub fn scan(
    src_root: &Path,
    dst_root: &Path,
    folders: &FolderMap,
    roots: &FolderRoots,
    min_age: Duration,
    now: SystemTime,
) -> ScanResult {
    let mut result = ScanResult::default();
    let nested: HashSet<_> = roots.iter().map(|r| src_root.join(r)).collect();
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
            &nested,
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
    nested: &HashSet<PathBuf>,
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
    let at_root = dir == src_base;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("Cannot read an entry of {:?}: {}", dir, e);
                out.errors += 1;
                continue;
            }
        };
        if is_ignored(&entry.file_name(), at_root) {
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
            // Another Syncthing folder nested in this one (paused, not in FOLDER_IDS, or mapped on
            // its own) is never processed as part of it. Only configured folders count: a nested
            // `.stfolder` can also be ordinary synced content.
            if nested.contains(&path) {
                debug!("Skipping {:?}: root of another Syncthing folder", path);
                continue;
            }
            walk(&path, src_base, dst_base, nested, min_age, now, out);
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
            assert!(is_ignored(OsStr::new(name), true), "{}", name);
        }
        for name in ["a.jpg", ".hidden", "syncthing.tmp", ".syncthing.notes"] {
            assert!(!is_ignored(OsStr::new(name), true), "{}", name);
        }
        // Below the root, only temporary files are Syncthing's.
        for name in [
            ".stfolder",
            ".stignore",
            ".stversions",
            ".stfolder.removed-1",
        ] {
            assert!(!is_ignored(OsStr::new(name), false), "{}", name);
        }
        assert!(is_ignored(OsStr::new(".syncthing.a.jpg.tmp"), false));
    }

    #[test]
    fn scans_files_and_maps_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("st");
        let dst = dir.path().join("hdd");
        let folder = src.join("phone/photos");
        fs::create_dir_all(folder.join(".stversions")).unwrap();
        fs::create_dir_all(folder.join(".stfolder")).unwrap();
        fs::create_dir_all(folder.join("DCIM")).unwrap();
        fs::write(folder.join("DCIM/a.jpg"), b"a").unwrap();
        fs::write(folder.join("DCIM/.syncthing.b.jpg.tmp"), b"partial").unwrap();
        fs::write(folder.join(".stversions/old.jpg"), b"v").unwrap();
        // A nested Syncthing folder is left alone; a nested marker alone is ordinary content.
        fs::create_dir_all(folder.join("shared/.stfolder")).unwrap();
        fs::write(folder.join("shared/laptop.txt"), b"l").unwrap();
        fs::create_dir_all(folder.join("Camera/.stfolder")).unwrap();
        fs::write(folder.join("Camera/c.jpg"), b"c").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", folder.join("link")).unwrap();

        let folders = FolderMap::from([("photos".to_string(), PathBuf::from("phone/photos"))]);
        let roots = FolderRoots::from([
            PathBuf::from("phone/photos"),
            PathBuf::from("phone/photos/shared"),
        ]);
        let later = SystemTime::now() + Duration::from_secs(3600);
        let r = scan(&src, &dst, &folders, &roots, Duration::from_secs(60), later);
        let mut jobs = r.jobs.clone();
        jobs.sort_by(|a, b| a.src.cmp(&b.src));
        assert_eq!(
            jobs,
            vec![
                Job {
                    src: folder.join("Camera/c.jpg"),
                    dst: dst.join("phone/photos/Camera/c.jpg")
                },
                Job {
                    src: folder.join("DCIM/a.jpg"),
                    dst: dst.join("phone/photos/DCIM/a.jpg")
                },
            ]
        );
        assert_eq!(r.errors, 0);
        assert_eq!(r.leftovers, 0);

        // Below the root, Syncthing's names are ordinary synced files.
        fs::write(folder.join("DCIM/.stignore"), b"n").unwrap();
        let r = scan(&src, &dst, &folders, &roots, Duration::from_secs(60), later);
        assert_eq!(r.jobs.len(), 3);
        fs::remove_file(folder.join("DCIM/.stignore")).unwrap();

        // Same files seen "now" are too young.
        let r = scan(
            &src,
            &dst,
            &folders,
            &roots,
            Duration::from_secs(60),
            SystemTime::now(),
        );
        assert!(r.jobs.is_empty());
        assert_eq!(r.deferred, 2);
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
            &FolderRoots::new(),
            Duration::ZERO,
            later,
        );
        assert!(r.jobs.is_empty());
        assert_eq!(r.leftovers, 1);
    }
}
