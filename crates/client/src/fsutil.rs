//! Small filesystem helpers: crash-safe writes and private (0600/0700) files.
//!
//! Every persistent file the client owns is written with write-then-rename, so
//! a crash leaves either the old or the new version on disk, never a torn one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Write `data` to `path` atomically. With `private`, the file is created with
/// mode 0600 (owner read/write only) on Unix.
pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let suffix = data_encoding::HEXLOWER.encode(&coolpsonotes_core::crypto::random_bytes::<6>());
    // Dot-prefixed so the mirror watcher and editors ignore it.
    let tmp = dir.join(format!(".{name}.cpn-tmp-{suffix}"));
    let result = (|| {
        let mut f = open_new(&tmp, private)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        sync_dir(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
        fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
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
        fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
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
}
