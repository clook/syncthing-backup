//! The only module allowed to delete, link or copy files (enforced by scripts/review.sh).
//! See docs/decisions/0004 (conflicts) and 0006 (durable move).

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Upper bound on `name_N.ext` candidates, to fail loudly instead of looping forever.
const MAX_SUFFIX: u32 = 10_000;
const TMP_SUFFIX: &str = ".porter-tmp";

/// A source file and where it belongs in the backup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Job {
    pub src: PathBuf,
    pub dst: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Source written to this destination and removed.
    Moved(PathBuf),
    /// Identical content already at this destination; source removed.
    Duplicate(PathBuf),
    /// Source no longer exists (already handled).
    Gone,
    /// Dry run: what would have happened.
    WouldMove(PathBuf),
    WouldDeduplicate(PathBuf),
}

#[derive(Debug)]
pub struct WitnessMissing(pub PathBuf);

impl std::fmt::Display for WitnessMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mount witness {:?} is missing, destination not mounted?",
            self.0
        )
    }
}

impl std::error::Error for WitnessMissing {}

#[derive(Debug, Clone)]
pub struct MoveOptions {
    /// Full path of the witness file; nothing is written while it is missing.
    pub witness: Option<PathBuf>,
    pub verify: bool,
    pub dry_run: bool,
    /// Use a hard link when source and destination share a filesystem (tests disable it to
    /// exercise the copy path).
    pub allow_link: bool,
}

enum Target {
    Free(PathBuf),
    Identical(PathBuf),
}

/// Moves `job.src` into the backup without ever overwriting an existing destination file.
/// The source is only removed once the destination is durable (and verified when enabled).
pub fn move_file(job: &Job, opts: &MoveOptions) -> Result<Outcome> {
    if let Some(w) = &opts.witness
        && !w.exists()
    {
        return Err(WitnessMissing(w.clone()).into());
    }

    let src_meta = match fs::symlink_metadata(&job.src) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Outcome::Gone),
        r => r.with_context(|| format!("stat {:?}", job.src))?,
    };
    if !src_meta.file_type().is_file() {
        bail!("{:?} is not a regular file", job.src);
    }

    // A concurrent writer is impossible (single worker), but link() still reports EEXIST
    // atomically, so re-resolve instead of ever replacing a file.
    for _ in 0..3 {
        let target = match resolve_target(&job.src, &job.dst)? {
            Target::Identical(existing) => {
                if opts.dry_run {
                    return Ok(Outcome::WouldDeduplicate(existing));
                }
                remove_source(&job.src, &src_meta)?;
                return Ok(Outcome::Duplicate(existing));
            }
            Target::Free(target) => target,
        };
        if opts.dry_run {
            return Ok(Outcome::WouldMove(target));
        }

        let parent = target
            .parent()
            .context("destination has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| format!("create {:?}", parent))?;

        let same_device = fs::metadata(parent)?.dev() == src_meta.dev();
        let placed = if opts.allow_link && same_device {
            link_no_replace(&job.src, &target)?
        } else {
            copy_durable(&job.src, &target, &src_meta)?
        };
        if !placed {
            continue; // target name appeared meanwhile: pick another
        }
        sync_dir(parent)?;

        if opts.verify && !files_are_identical(&job.src, &target)? {
            // Only the file this attempt created is removed; the source is kept for a retry.
            fs::remove_file(&target).with_context(|| format!("remove unverified {:?}", target))?;
            sync_dir(parent)?;
            bail!(
                "verification failed for {:?}: destination differs from source",
                job.src
            );
        }

        remove_source(&job.src, &src_meta)?;
        return Ok(Outcome::Moved(target));
    }
    bail!("could not find a free destination name for {:?}", job.src)
}

/// First free name among `dst`, `dst_1`, `dst_2`…, or an existing file with identical content.
fn resolve_target(src: &Path, dst: &Path) -> Result<Target> {
    for n in 0..=MAX_SUFFIX {
        let candidate = if n == 0 {
            dst.to_path_buf()
        } else {
            suffixed(dst, n)
        };
        match fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Target::Free(candidate)),
            Err(e) => return Err(e).with_context(|| format!("stat {:?}", candidate)),
            Ok(m) if m.file_type().is_file() && files_are_identical(src, &candidate)? => {
                return Ok(Target::Identical(candidate));
            }
            Ok(_) => {}
        }
    }
    bail!("more than {} name conflicts for {:?}", MAX_SUFFIX, dst)
}

/// `dir/name.ext` -> `dir/name_N.ext`; works on raw bytes so non-UTF-8 names are safe.
/// A leading dot is not an extension separator (`.bashrc` -> `.bashrc_1`).
fn suffixed(path: &Path, n: u32) -> PathBuf {
    let name = path.file_name().unwrap_or_default().as_bytes();
    let (stem, ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, &b""[..]),
    };
    let mut new_name = stem.to_vec();
    new_name.extend_from_slice(format!("_{}", n).as_bytes());
    new_name.extend_from_slice(ext);
    path.with_file_name(OsStr::from_bytes(&new_name))
}

/// Creates `target` as a new name for `existing`. Returns false if `target` already exists.
fn link_no_replace(existing: &Path, target: &Path) -> Result<bool> {
    match fs::hard_link(existing, target) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e).with_context(|| format!("link {:?} -> {:?}", existing, target)),
    }
}

/// Copies through a temporary file that is fsynced before it gets its final name.
/// Returns false if `target` already exists.
fn copy_durable(src: &Path, target: &Path, src_meta: &fs::Metadata) -> Result<bool> {
    let mut tmp_name = OsStr::new(".").to_os_string();
    tmp_name.push(src.file_name().unwrap_or_default());
    tmp_name.push(TMP_SUFFIX);
    let tmp = target.with_file_name(tmp_name);

    let result = (|| -> Result<bool> {
        let mut input = File::open(src).with_context(|| format!("open {:?}", src))?;
        if input.metadata()?.ino() != src_meta.ino() {
            bail!("{:?} was replaced before the copy started", src);
        }
        // Truncates a leftover from an interrupted attempt.
        let mut output = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("create {:?}", tmp))?;
        io::copy(&mut input, &mut output)
            .with_context(|| format!("copy {:?} -> {:?}", src, tmp))?;
        output.set_permissions(src_meta.permissions())?;
        output.set_modified(src_meta.modified()?)?;
        output
            .sync_all()
            .with_context(|| format!("fsync {:?}", tmp))?;
        link_no_replace(&tmp, target)
    })();

    // The temporary name is never needed after this point, whatever happened.
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) if result.is_ok() => return Err(e).with_context(|| format!("remove {:?}", tmp)),
        Err(_) => {}
    }
    result
}

/// Removes the source, unless the path now points to another file (e.g. Syncthing renamed a newer
/// version into place since we copied it).
fn remove_source(src: &Path, copied: &fs::Metadata) -> Result<()> {
    let now = fs::symlink_metadata(src).with_context(|| format!("stat {:?}", src))?;
    if now.ino() != copied.ino()
        || now.dev() != copied.dev()
        || now.len() != copied.len()
        || now.mtime() != copied.mtime()
    {
        bail!(
            "{:?} changed during the move; keeping it for the next pass",
            src
        );
    }
    fs::remove_file(src).with_context(|| format!("remove source {:?}", src))
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("fsync directory {:?}", dir))
}

/// Byte-for-byte comparison, size first. Reads full buffers so short reads cannot cause a
/// false "different".
pub fn files_are_identical(path_a: &Path, path_b: &Path) -> Result<bool> {
    let meta_a = fs::metadata(path_a)?;
    let meta_b = fs::metadata(path_b)?;
    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }

    let mut r1 = File::open(path_a)?;
    let mut r2 = File::open(path_b)?;
    let mut buf1 = vec![0u8; 1024 * 512]; // 512KB for HDD sequential access
    let mut buf2 = vec![0u8; 1024 * 512];
    loop {
        let n1 = fill(&mut r1, &mut buf1)?;
        let n2 = fill(&mut r2, &mut buf2)?;
        if n1 != n2 || buf1[..n1] != buf2[..n2] {
            return Ok(false);
        }
        if n1 == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or EOF.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    struct Env {
        _dir: TempDir,
        src: PathBuf,
        dst: PathBuf,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        Env {
            _dir: dir,
            src,
            dst,
        }
    }

    fn opts(allow_link: bool) -> MoveOptions {
        MoveOptions {
            witness: None,
            verify: true,
            dry_run: false,
            allow_link,
        }
    }

    fn job(env: &Env, rel: &str, content: &[u8]) -> Job {
        let src = env.src.join(rel);
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, content).unwrap();
        Job {
            src,
            dst: env.dst.join(rel),
        }
    }

    #[test]
    fn moves_new_file_both_paths() {
        for allow_link in [true, false] {
            let env = env();
            let j = job(&env, "DCIM/a.jpg", b"photo");
            assert_eq!(
                move_file(&j, &opts(allow_link)).unwrap(),
                Outcome::Moved(j.dst.clone())
            );
            assert_eq!(fs::read(&j.dst).unwrap(), b"photo");
            assert!(!j.src.exists());
        }
    }

    #[test]
    fn copy_path_preserves_content_and_mtime() {
        let env = env();
        let j = job(&env, "a.jpg", &vec![7u8; 3 * 1024 * 1024]);
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(&j.src)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        move_file(&j, &opts(false)).unwrap();
        assert_eq!(fs::read(&j.dst).unwrap(), vec![7u8; 3 * 1024 * 1024]);
        assert_eq!(fs::metadata(&j.dst).unwrap().modified().unwrap(), mtime);
        let leftovers: Vec<_> = fs::read_dir(&env.dst)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(TMP_SUFFIX)
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn identical_destination_removes_source_only() {
        let env = env();
        let j = job(&env, "a.jpg", b"same");
        fs::write(&j.dst, b"same").unwrap();
        assert_eq!(
            move_file(&j, &opts(false)).unwrap(),
            Outcome::Duplicate(j.dst.clone())
        );
        assert!(!j.src.exists());
        assert_eq!(fs::read(&j.dst).unwrap(), b"same");
    }

    #[test]
    fn conflict_creates_suffix() {
        let env = env();
        let j = job(&env, "a.jpg", b"new");
        fs::write(&j.dst, b"old").unwrap();
        assert_eq!(
            move_file(&j, &opts(false)).unwrap(),
            Outcome::Moved(env.dst.join("a_1.jpg"))
        );
        assert_eq!(fs::read(&j.dst).unwrap(), b"old");
        assert_eq!(fs::read(env.dst.join("a_1.jpg")).unwrap(), b"new");
    }

    #[test]
    fn identical_suffix_is_deduplicated() {
        let env = env();
        let j = job(&env, "a.jpg", b"v2");
        fs::write(&j.dst, b"v1").unwrap();
        fs::write(env.dst.join("a_1.jpg"), b"v2").unwrap();
        assert_eq!(
            move_file(&j, &opts(true)).unwrap(),
            Outcome::Duplicate(env.dst.join("a_1.jpg"))
        );
        assert!(!env.dst.join("a_2.jpg").exists());
        assert!(!j.src.exists());
    }

    #[test]
    fn never_overwrites_existing() {
        // A directory squatting the name must not be replaced either.
        let env = env();
        let j = job(&env, "a.jpg", b"new");
        fs::create_dir(&j.dst).unwrap();
        assert_eq!(
            move_file(&j, &opts(true)).unwrap(),
            Outcome::Moved(env.dst.join("a_1.jpg"))
        );
        assert!(j.dst.is_dir());
    }

    #[test]
    fn non_utf8_name_suffix() {
        let env = env();
        let name = OsStr::from_bytes(b"caf\xe9.jpg");
        let src = env.src.join(name);
        fs::write(&src, b"new").unwrap();
        let dst = env.dst.join(name);
        fs::write(&dst, b"old").unwrap();
        let j = Job { src, dst };
        let expected = env.dst.join(OsStr::from_bytes(b"caf\xe9_1.jpg"));
        assert_eq!(
            move_file(&j, &opts(false)).unwrap(),
            Outcome::Moved(expected)
        );
    }

    #[test]
    fn suffix_naming() {
        assert_eq!(
            suffixed(Path::new("/d/a.tar.gz"), 1),
            PathBuf::from("/d/a.tar_1.gz")
        );
        assert_eq!(
            suffixed(Path::new("/d/.bashrc"), 2),
            PathBuf::from("/d/.bashrc_2")
        );
        assert_eq!(
            suffixed(Path::new("/d/README"), 3),
            PathBuf::from("/d/README_3")
        );
    }

    #[test]
    fn witness_missing_moves_nothing() {
        let env = env();
        let j = job(&env, "a.jpg", b"x");
        let o = MoveOptions {
            witness: Some(env.dst.join(".mounted")),
            ..opts(false)
        };
        let err = move_file(&j, &o).unwrap_err();
        assert!(err.downcast_ref::<WitnessMissing>().is_some());
        assert!(j.src.exists());
        assert!(!j.dst.exists());
    }

    #[test]
    fn dry_run_changes_nothing() {
        let env = env();
        let j = job(&env, "a.jpg", b"x");
        let o = MoveOptions {
            dry_run: true,
            ..opts(false)
        };
        assert_eq!(
            move_file(&j, &o).unwrap(),
            Outcome::WouldMove(j.dst.clone())
        );
        assert!(j.src.exists());
        assert!(!j.dst.exists());
    }

    #[test]
    fn missing_source_is_gone() {
        let env = env();
        let j = Job {
            src: env.src.join("nope"),
            dst: env.dst.join("nope"),
        };
        assert_eq!(move_file(&j, &opts(false)).unwrap(), Outcome::Gone);
    }

    #[test]
    fn replaced_source_is_kept() {
        let env = env();
        let j = job(&env, "a.jpg", b"v1");
        let copied = fs::symlink_metadata(&j.src).unwrap();
        // Simulate Syncthing renaming a newer version into place after the copy.
        let newer = env.src.join(".tmp-newer");
        fs::write(&newer, b"v2-longer").unwrap();
        fs::rename(&newer, &j.src).unwrap();
        assert!(remove_source(&j.src, &copied).is_err());
        assert_eq!(fs::read(&j.src).unwrap(), b"v2-longer");
    }

    #[test]
    fn identical_detects_difference_and_equality() {
        let env = env();
        let a = env.src.join("a");
        let b = env.src.join("b");
        fs::write(&a, vec![1u8; 2_000_000]).unwrap();
        fs::write(&b, vec![1u8; 2_000_000]).unwrap();
        assert!(files_are_identical(&a, &b).unwrap());
        let mut other = vec![1u8; 2_000_000];
        other[1_999_999] = 2;
        fs::write(&b, other).unwrap();
        assert!(!files_are_identical(&a, &b).unwrap());
    }
}
