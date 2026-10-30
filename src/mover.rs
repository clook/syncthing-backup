//! The only module allowed to delete, link or copy files (enforced by scripts/review.sh).
//! See docs/decisions/0004 (conflicts), 0006 and 0008 (durable move).

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Upper bound on `name_N.ext` candidates, to fail loudly instead of looping forever.
const MAX_SUFFIX: u32 = 10_000;
/// Reserved name of the in-progress copy in each destination directory. Fixed and short, so long
/// source names cannot overflow NAME_MAX, and only one exists per directory (single worker).
pub const TMP_NAME: &str = ".syncthing-backup.tmp";
const NAME_MAX: usize = 255;

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
            "mount witness {:?} is missing or on another filesystem, destination not mounted?",
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
///
/// The source is opened once: copying, comparing and the final "still the same file" check all
/// use that inode, so a newer version renamed into place by Syncthing meanwhile is never deleted
/// and never makes a correct copy of the previous version look wrong (decision 0008).
pub fn move_file(job: &Job, opts: &MoveOptions) -> Result<Outcome> {
    check_witness(opts, None)?;
    if job.dst.file_name() == Some(OsStr::new(TMP_NAME)) {
        bail!("{:?} uses the reserved name {}", job.src, TMP_NAME);
    }

    let link_meta = match fs::symlink_metadata(&job.src) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Outcome::Gone),
        r => r.with_context(|| format!("stat {:?}", job.src))?,
    };
    if !link_meta.file_type().is_file() {
        bail!("{:?} is not a regular file", job.src);
    }
    let mut src_file = match File::open(&job.src) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Outcome::Gone),
        r => r.with_context(|| format!("open {:?}", job.src))?,
    };
    let src_meta = src_file.metadata()?;
    if src_meta.ino() != link_meta.ino() || src_meta.dev() != link_meta.dev() {
        bail!("{:?} was replaced while being opened", job.src);
    }

    // A concurrent writer is impossible (single worker), but link() still reports EEXIST
    // atomically, so re-resolve instead of ever replacing a file.
    for _ in 0..3 {
        let target = match resolve_target(&mut src_file, &job.dst)? {
            Target::Identical(existing) => {
                if opts.dry_run {
                    return Ok(Outcome::WouldDeduplicate(existing));
                }
                // The existing copy may never have been made durable (e.g. a crash after link).
                sync_file(&existing)?;
                sync_dir(parent_of(&existing)?)?;
                check_witness(opts, Some(&existing))?;
                remove_source(&job.src, &src_meta)?;
                return Ok(Outcome::Duplicate(existing));
            }
            Target::Free(target) => target,
        };
        if opts.dry_run {
            return Ok(Outcome::WouldMove(target));
        }

        let parent = parent_of(&target)?;
        create_dirs_durable(parent)?;

        let same_device = fs::metadata(parent)?.dev() == src_meta.dev();
        let placed = if opts.allow_link && same_device {
            match link_no_replace(&job.src, &target) {
                // Same st_dev but different mounts (bind mounts): fall back to copying.
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    copy_durable(&mut src_file, &src_meta, &target)?
                }
                r => {
                    let placed =
                        r.with_context(|| format!("link {:?} -> {:?}", job.src, target))?;
                    if placed {
                        ensure_linked_same_inode(&target, &src_meta)?;
                        sync_file(&target)?;
                    }
                    placed
                }
            }
        } else {
            copy_durable(&mut src_file, &src_meta, &target)?
        };
        if !placed {
            continue; // target name appeared meanwhile: pick another
        }
        sync_dir(parent)?;

        if opts.verify && !same_content(&mut src_file, &target)? {
            // The copy was read from the very inode we compare against, so it is really wrong.
            // Only the file this attempt created is removed; the source is kept for a retry.
            fs::remove_file(&target).with_context(|| format!("remove unverified {:?}", target))?;
            sync_dir(parent)?;
            bail!(
                "verification failed for {:?}: destination differs from source",
                job.src
            );
        }

        check_witness(opts, Some(&target))?;
        remove_source(&job.src, &src_meta)?;
        return Ok(Outcome::Moved(target));
    }
    bail!("could not find a free destination name for {:?}", job.src)
}

/// Fails with `WitnessMissing` if the witness is absent or, when `written` is given, if that file
/// is not on the witness's filesystem (destination unmounted during the copy).
fn check_witness(opts: &MoveOptions, written: Option<&Path>) -> Result<()> {
    let Some(w) = &opts.witness else {
        return Ok(());
    };
    let Ok(witness_meta) = fs::metadata(w) else {
        return Err(WitnessMissing(w.clone()).into());
    };
    if let Some(path) = written
        && fs::metadata(path)?.dev() != witness_meta.dev()
    {
        return Err(WitnessMissing(w.clone()).into());
    }
    Ok(())
}

fn parent_of(path: &Path) -> Result<&Path> {
    path.parent().context("destination has no parent directory")
}

/// Like `create_dir_all`, but fsyncs the parent of every directory it creates, so a new subtree
/// survives a power loss once the source is gone.
fn create_dirs_durable(dir: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cur = dir;
    loop {
        match fs::symlink_metadata(cur) {
            Ok(m) if m.is_dir() => break,
            Ok(_) => bail!("{:?} exists and is not a directory", cur),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(cur);
                cur = cur.parent().context("no existing ancestor directory")?;
            }
            Err(e) => return Err(e).with_context(|| format!("stat {:?}", cur)),
        }
    }
    for d in missing.into_iter().rev() {
        match fs::create_dir(d) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("create {:?}", d)),
        }
        sync_dir(parent_of(d)?)?;
    }
    Ok(())
}

/// First free name among `dst`, `dst_1`, `dst_2`…, or an existing file with identical content.
fn resolve_target(src: &mut File, dst: &Path) -> Result<Target> {
    for n in 0..=MAX_SUFFIX {
        let candidate = if n == 0 {
            dst.to_path_buf()
        } else {
            suffixed(dst, n)
        };
        match fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Target::Free(candidate)),
            Err(e) => return Err(e).with_context(|| format!("stat {:?}", candidate)),
            Ok(m) if m.file_type().is_file() && same_content(src, &candidate)? => {
                return Ok(Target::Identical(candidate));
            }
            Ok(_) => {}
        }
    }
    bail!("more than {} name conflicts for {:?}", MAX_SUFFIX, dst)
}

/// `dir/name.ext` -> `dir/name_N.ext`; works on raw bytes so non-UTF-8 names are safe.
/// A leading dot is not an extension separator (`.bashrc` -> `.bashrc_1`). The stem is shortened
/// when needed to stay within NAME_MAX (at a UTF-8 boundary when the name is UTF-8).
fn suffixed(path: &Path, n: u32) -> PathBuf {
    let name = path.file_name().unwrap_or_default().as_bytes();
    let (stem, mut ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, &b""[..]),
    };
    let suffix = format!("_{}", n);
    if ext.len() + suffix.len() >= NAME_MAX {
        ext = b"";
    }
    let mut keep = stem.len().min(NAME_MAX - suffix.len() - ext.len());
    if let Ok(text) = std::str::from_utf8(stem) {
        while !text.is_char_boundary(keep) {
            keep -= 1;
        }
    }
    let mut new_name = stem[..keep].to_vec();
    new_name.extend_from_slice(suffix.as_bytes());
    new_name.extend_from_slice(ext);
    path.with_file_name(OsStr::from_bytes(&new_name))
}

/// Creates `target` as a new name for `existing`. `Ok(false)` if `target` already exists.
fn link_no_replace(existing: &Path, target: &Path) -> io::Result<bool> {
    match fs::hard_link(existing, target) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// After linking by path, makes sure the link points to the inode we opened. If the source was
/// replaced in between, the new link (a second name of the newer source) is removed again.
fn ensure_linked_same_inode(target: &Path, src_meta: &fs::Metadata) -> Result<()> {
    let linked = fs::symlink_metadata(target)?;
    if linked.ino() != src_meta.ino() || linked.dev() != src_meta.dev() {
        fs::remove_file(target).with_context(|| format!("remove mislinked {:?}", target))?;
        bail!("source was replaced during the move; retrying on the next pass");
    }
    Ok(())
}

/// Copies through the reserved temporary file, fsynced before it gets its final name.
/// `Ok(false)` if `target` already exists.
fn copy_durable(src: &mut File, src_meta: &fs::Metadata, target: &Path) -> Result<bool> {
    let tmp = target.with_file_name(TMP_NAME);
    // A leftover from an interrupted attempt is ours; anything else under that name is not touched.
    match fs::symlink_metadata(&tmp) {
        Ok(m) if m.file_type().is_file() => {
            fs::remove_file(&tmp).with_context(|| format!("remove stale {:?}", tmp))?
        }
        Ok(_) => bail!("{:?} exists and is not a regular file", tmp),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {:?}", tmp)),
    }

    let result = (|| -> Result<bool> {
        // create_new = O_EXCL: never follows a symlink, never truncates an existing file.
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {:?}", tmp))?;
        src.seek(SeekFrom::Start(0))?;
        io::copy(src, &mut output).with_context(|| format!("copy to {:?}", tmp))?;
        output.set_permissions(src_meta.permissions())?;
        output.set_modified(src_meta.modified()?)?;
        output
            .sync_all()
            .with_context(|| format!("fsync {:?}", tmp))?;
        link_no_replace(&tmp, target).with_context(|| format!("link {:?} -> {:?}", tmp, target))
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

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("fsync {:?}", path))
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("fsync directory {:?}", dir))
}

/// Compares the opened source with a file at `other`, size first, byte for byte. Reads full
/// buffers so short reads cannot cause a false "different".
fn same_content(src: &mut File, other: &Path) -> Result<bool> {
    let mut other_file = File::open(other).with_context(|| format!("open {:?}", other))?;
    if src.metadata()?.len() != other_file.metadata()?.len() {
        return Ok(false);
    }
    src.seek(SeekFrom::Start(0))?;
    let mut buf1 = vec![0u8; 1024 * 512]; // 512KB for HDD sequential access
    let mut buf2 = vec![0u8; 1024 * 512];
    loop {
        let n1 = fill(src, &mut buf1)?;
        let n2 = fill(&mut other_file, &mut buf2)?;
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
                    .starts_with(".syncthing-backup")
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
    fn same_content_detects_difference_and_equality() {
        let env = env();
        let a = env.src.join("a");
        let b = env.src.join("b");
        fs::write(&a, vec![1u8; 2_000_000]).unwrap();
        fs::write(&b, vec![1u8; 2_000_000]).unwrap();
        let mut fa = File::open(&a).unwrap();
        assert!(same_content(&mut fa, &b).unwrap());
        let mut other = vec![1u8; 2_000_000];
        other[1_999_999] = 2;
        fs::write(&b, other).unwrap();
        assert!(!same_content(&mut fa, &b).unwrap());
    }

    #[test]
    fn stale_tmp_is_replaced() {
        let env = env();
        let j = job(&env, "a.jpg", b"fresh");
        fs::write(env.dst.join(TMP_NAME), b"stale partial copy").unwrap();
        move_file(&j, &opts(false)).unwrap();
        assert_eq!(fs::read(&j.dst).unwrap(), b"fresh");
        assert!(!env.dst.join(TMP_NAME).exists());
    }

    #[test]
    fn tmp_symlink_is_never_followed() {
        let env = env();
        let victim = env.dst.join("victim.jpg");
        fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(&victim, env.dst.join(TMP_NAME)).unwrap();
        let j = job(&env, "a.jpg", b"new");
        assert!(move_file(&j, &opts(false)).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert!(j.src.exists());
    }

    #[test]
    fn reserved_name_is_rejected() {
        let env = env();
        let j = job(&env, TMP_NAME, b"x");
        assert!(move_file(&j, &opts(false)).is_err());
        assert!(j.src.exists());
    }

    #[test]
    fn long_names_stay_within_name_max() {
        let stem = "é".repeat(126); // 252 bytes
        let p = suffixed(&Path::new("/d").join(format!("{}.jpg", stem)), 12);
        let name = p.file_name().unwrap();
        assert!(name.len() <= NAME_MAX, "{}", name.len());
        let text = name.to_str().expect("still valid UTF-8");
        assert!(text.ends_with("_12.jpg"));
    }

    #[test]
    fn creates_nested_directories() {
        let env = env();
        let j = job(&env, "a/b/c/d.jpg", b"x");
        assert_eq!(
            move_file(&j, &opts(false)).unwrap(),
            Outcome::Moved(env.dst.join("a/b/c/d.jpg"))
        );
    }
}
