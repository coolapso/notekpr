//! Small filesystem helpers: crash-safe writes and private (0600/0700) files.
//!
//! Every persistent file the client owns is written with write-then-rename, so
//! a crash leaves either the old or the new version on disk, never a torn one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

/// Prefix and suffix of our temporary files. Short and fixed-length, so a
/// file whose own name is close to the 255-byte limit can still be replaced
/// (`ENAMETOOLONG`), and dot-prefixed, so the mirror watcher and editors
/// ignore them.
const TMP_PREFIX: &str = ".notekpr-";
const TMP_SUFFIX: &str = ".tmp";
/// Marker of the temporary names used by older versions (`.{name}.cpn-tmp-{hex}`).
const LEGACY_TMP_MARK: &str = ".cpn-tmp-";

/// Whether a file name is one of our temporary files (current or legacy form).
pub fn is_temp_name(name: &str) -> bool {
    let current = name
        .strip_prefix(TMP_PREFIX)
        .and_then(|r| r.strip_suffix(TMP_SUFFIX))
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    let legacy = name.starts_with('.')
        && name
            .rsplit_once(LEGACY_TMP_MARK)
            .is_some_and(|(_, hex)| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    current || legacy
}

pub(crate) fn temp_path(dir: &Path) -> std::path::PathBuf {
    let suffix = data_encoding::HEXLOWER.encode(&notekpr_core::crypto::random_bytes::<6>());
    dir.join(format!("{TMP_PREFIX}{suffix}{TMP_SUFFIX}"))
}

/// Write `data` to `path` atomically. With `private`, the file is created with
/// mode 0600 (owner read/write only) on Unix.
pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> io::Result<()> {
    write_atomic_if(path, data, private, || Ok(true)).map(|_| ())
}

/// Like [`write_atomic`], but `check` runs after the new content is safely on
/// disk and immediately before the rename. If it returns `false` the
/// temporary file is removed, `path` is left alone and `Ok(false)` returned.
/// This keeps the window in which a concurrent writer (an editor saving the
/// same file) can be overwritten as small as possible.
pub fn write_atomic_if(
    path: &Path,
    data: &[u8],
    private: bool,
    check: impl FnOnce() -> io::Result<bool>,
) -> io::Result<bool> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = temp_path(dir);
    let result = (|| {
        let mut f = open_new(&tmp, private)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if !check()? {
            return Ok(false);
        }
        fs::rename(&tmp, path)?;
        sync_dir(dir);
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Remove temporary files of ours left behind by a crash, below `dir` (at
/// most `max_depth` levels deep, not following symlinks), if they are older
/// than `min_age`. Returns how many were removed.
pub fn remove_stale_temp_files(dir: &Path, max_depth: usize, min_age: Duration) -> usize {
    let mut n = 0;
    let walker = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        // Never descend into other dot-directories (`.git`, `.obsidian`, …).
        .filter_entry(|e| {
            e.file_type().is_file() || e.file_name().to_str().is_some_and(|n| !n.starts_with('.'))
        });
    for e in walker.filter_map(|e| e.ok()) {
        if !e.file_type().is_file() || !e.file_name().to_str().is_some_and(is_temp_name) {
            continue;
        }
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if old && fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

fn open_new(path: &Path, private: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = private;
    o.open(path)
}

/// Create a new file that must not exist yet (used for key files).
pub fn create_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = open_new(path, true)?;
    f.write_all(data)?;
    f.sync_all()?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// `mkdir -p` with mode 0700 for the directories created.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)
}

pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether a file can be read by users other than its owner.
pub fn is_shared_readable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o077 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        write_atomic(&p, b"one", true).unwrap();
        write_atomic(&p, b"two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert!(!is_shared_readable(&p));
        // No temporary files are left behind.
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn temp_names_are_short_and_recognised() {
        let d = tempfile::tempdir().unwrap();
        // A name at the file-system limit can still be replaced atomically.
        let long = "x".repeat(250) + ".md";
        let p = d.path().join(&long);
        write_atomic(&p, b"one", true).unwrap();
        write_atomic(&p, b"two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let t = temp_path(d.path());
        let name = t.file_name().unwrap().to_str().unwrap();
        assert!(name.len() < 32 && is_temp_name(name), "{name}");
        assert!(is_temp_name(".note.md.cpn-tmp-0a1b2c"));
        for n in [
            "note.md",
            ".notekpr-.tmp",
            ".notekpr-xyz.tmp",
            "a.cpn-tmp-00",
        ] {
            assert!(!is_temp_name(n), "{n}");
        }
    }

    #[test]
    fn checked_write_leaves_the_file_alone_when_the_check_fails() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.md");
        fs::write(&p, "theirs").unwrap();
        assert!(!write_atomic_if(&p, b"ours", true, || Ok(false)).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"theirs");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
        assert!(write_atomic_if(&p, b"ours", true, || Ok(true)).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"ours");
    }

    #[test]
    fn stale_temp_files_are_removed() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a/b")).unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        for p in [
            "a/.notekpr-0123456789ab.tmp",
            "a/b/.Note.md.cpn-tmp-abcdef",
            ".git/.notekpr-0123456789ab.tmp",
            "a/keep.md",
        ] {
            fs::write(d.path().join(p), "x").unwrap();
        }
        assert_eq!(remove_stale_temp_files(d.path(), 8, Duration::ZERO), 2);
        assert!(d.path().join("a/keep.md").exists());
        assert!(d.path().join(".git/.notekpr-0123456789ab.tmp").exists());
        // Young files are kept (a write may be in progress).
        fs::write(d.path().join("a/.notekpr-0123456789ab.tmp"), "x").unwrap();
        assert_eq!(
            remove_stale_temp_files(d.path(), 8, Duration::from_secs(3600)),
            0
        );
    }
}
