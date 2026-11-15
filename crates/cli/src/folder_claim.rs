//! Who keeps a Markdown folder in sync (docs/cli.md §4, "One keeper per
//! folder"): a hidden file at its top, [`MARKER`], naming the keeper by a
//! random id its profile remembers too (`SyncSettings::folder_id`), and a
//! lock on the folder itself while a sync runs. Every notekpr on the
//! computer reads it — the CLI, the desktop app however installed, a copy
//! with settings of its own — so none needs to know where the others keep
//! theirs.
//!
//! The marker is in a folder anything running as the person can write, and
//! that cloud tools may sync: it is read without following links or
//! blocking, only as a small regular file, and only ever replaced by a
//! rename or removed — never written through.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::mirror::{is_ignored_name, resolve_lenient};
use crate::profile::ProfileStore;
use crate::setup;

/// The marker's name at a Markdown folder's top. A dot name: the mirror
/// never syncs it.
pub const MARKER: &str = ".notekpr-folder";

/// Where a folder that can't be locked itself (NFS) is locked instead.
const LOCK_FILE: &str = ".notekpr-folder.lock";

/// Whether `name` (at a Markdown folder's top) is the folder's own file,
/// never moved or copied with the notes.
pub fn is_own_file(name: &std::ffi::OsStr) -> bool {
    name == MARKER || name == LOCK_FILE
}

/// More than any marker notekpr writes.
const MAX_MARKER: u64 = 4096;

/// What the marker holds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct Marker {
    v: u32,
    id: String,
    /// Who, for messages: "the CLI, profile 'default'".
    by: String,
}

/// What is at a folder's top.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Absent,
    Marker {
        id: String,
        by: String,
    },
    /// Not a marker notekpr wrote: a link, a folder, someone else's file,
    /// too big, or not a marker's JSON.
    Strange,
}

/// A label from a marker, safe to show in a terminal or the app's window.
/// Only letters, digits, spaces and ASCII punctuation: no control or
/// direction-changing characters that could make a message say something
/// else than it does.
fn shown_label(by: &str) -> String {
    let s: String = by
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || c.is_ascii_punctuation())
        .take(100)
        .collect();
    if s.trim().is_empty() {
        "another notekpr".to_string()
    } else {
        s
    }
}

/// Open `path` to read it without following a link at its name or waiting
/// on a pipe.
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    o.open(path)
}

/// Whether an open file is one notekpr could have made: a regular file
/// with no other name, the person's own.
fn ours_to_read(f: &File) -> bool {
    let Ok(m) = f.metadata() else {
        return false;
    };
    if !m.is_file() || m.len() > MAX_MARKER {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and can't fail.
        let me = unsafe { libc::geteuid() };
        if m.nlink() != 1 || m.uid() != me {
            return false;
        }
    }
    true
}

fn read_marker(root: &Path) -> Found {
    let path = root.join(MARKER);
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Found::Absent,
        Err(_) => return Found::Strange,
        Ok(_) => {}
    }
    let Ok(f) = open_no_follow(&path) else {
        return Found::Strange;
    };
    if !ours_to_read(&f) {
        return Found::Strange;
    }
    let mut text = String::new();
    if f.take(MAX_MARKER).read_to_string(&mut text).is_err() {
        return Found::Strange;
    }
    match serde_json::from_str::<Marker>(&text) {
        Ok(m) if !m.id.is_empty() => Found::Marker { id: m.id, by: m.by },
        _ => Found::Strange,
    }
}

/// Whether a folder holds a marker (anything by that name): it is a
/// Markdown folder, or was.
pub fn holds_marker(dir: &Path) -> bool {
    dir.join(MARKER).symlink_metadata().is_ok()
}

/// A Markdown folder in or around `dir` (but `dir` itself): where it is.
pub fn nested(dir: &Path) -> Option<PathBuf> {
    let dir = resolve_lenient(dir);
    if let Some(p) = dir.ancestors().skip(1).find(|a| holds_marker(a)) {
        return Some(p.to_path_buf());
    }
    below(&dir)
}

/// A folder below `dir` holding a marker — another Markdown folder — the
/// way the mirror walks (dot names and links aside).
pub fn below(dir: &Path) -> Option<PathBuf> {
    walkdir::WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str().is_some_and(|n| !is_ignored_name(n)))
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().is_dir() && holds_marker(e.path()))
        .map(|e| e.into_path())
}

fn nested_error(dir: &Path, other: &Path) -> anyhow::Error {
    anyhow!(
        "{} is a Markdown folder (it holds {MARKER}), and {} is {} it: a folder can't be kept in sync \
         inside another. Choose another folder, or stop keeping that one in sync (and remove its {MARKER} if \
         nothing keeps it any more)",
        setup::shown(other),
        setup::shown(dir),
        if other.starts_with(dir) {
            "around"
        } else {
            "inside"
        }
    )
}

/// Who, among the profiles the CLI and the app know of, remembers `id` as
/// their folder's — and whether they still keep `root`.
fn known_keeper(id: &str, root: &Path, mine: &ProfileStore) -> Option<(String, bool)> {
    for k in setup::keepers(mine.dirs()) {
        for name in k.dirs().profiles() {
            let Ok(Some(p)) = ProfileStore::new(k.dirs(), &name).and_then(|s| s.load()) else {
                continue;
            };
            if p.sync.folder_id.as_deref() != Some(id) {
                continue;
            }
            let keeps = p
                .sync
                .mirror_dir
                .as_deref()
                .is_some_and(|d| resolve_lenient(d) == root);
            return Some((k.who(&name), keeps));
        }
    }
    None
}

/// What `root`'s marker says to the profile in `mine` (whose id is
/// `my_id`): free (absent, its own, or left behind by a profile known here
/// that stopped keeping it), or refused.
fn judge(root: &Path, mine: &ProfileStore, my_id: Option<&str>, take_over: bool) -> Result<()> {
    let unknown = |what: String| {
        anyhow!(
            "{} {what}. If nothing keeps it in sync any more, run again with --take-over, or remove that file",
            setup::shown(root)
        )
    };
    match read_marker(root) {
        Found::Absent => Ok(()),
        Found::Marker { id, .. } if Some(id.as_str()) == my_id => Ok(()),
        Found::Marker { id, by } => match known_keeper(&id, root, mine) {
            Some((who, true)) => bail!(
                "{} is kept in sync already, by {who}: choose another folder",
                setup::shown(root)
            ),
            Some((_, false)) => Ok(()),
            None if take_over => Ok(()),
            None => Err(unknown(format!(
                "is kept in sync by {} (its {MARKER} says so; not a profile notekpr knows here)",
                shown_label(&by)
            ))),
        },
        Found::Strange if take_over => Ok(()),
        Found::Strange => Err(unknown(format!("holds a {MARKER} notekpr can't read"))),
    }
}

/// The lock held while a folder is kept in sync.
enum Lock {
    /// On the folder itself (any local disk).
    Folder(File),
    /// On [`LOCK_FILE`] in it, where the folder can't be locked (NFS).
    File(File),
}

/// `(device, inode)`: the same folder or file, not just the same name.
#[cfg(unix)]
fn ident(m: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

#[cfg(not(unix))]
fn ident(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn in_use(root: &Path) -> anyhow::Error {
    anyhow!(
        "{} is being kept in sync by another notekpr right now: stop it there first, or choose another folder",
        setup::shown(root)
    )
}

impl Lock {
    fn take(root: &Path) -> Result<Lock> {
        let dir = File::open(root).with_context(|| format!("opening {}", root.display()))?;
        match dir.try_lock() {
            Ok(()) => return Ok(Lock::Folder(dir)),
            Err(std::fs::TryLockError::WouldBlock) => return Err(in_use(root)),
            Err(std::fs::TryLockError::Error(_)) => {}
        }
        // A file system that locks files only.
        let path = root.join(LOCK_FILE);
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let f = o
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        if !ours_to_read(&f) {
            bail!("{} isn't notekpr's: remove it", path.display());
        }
        match f.try_lock() {
            Ok(()) => Ok(Lock::File(f)),
            Err(std::fs::TryLockError::WouldBlock) => Err(in_use(root)),
            Err(std::fs::TryLockError::Error(e)) => Err(anyhow!(e)).with_context(|| {
                format!(
                    "{}'s disk doesn't let notekpr lock it, so it can't make sure nothing else keeps it in sync: \
                     keep the Markdown folder on another disk",
                    setup::shown(root)
                )
            }),
        }
    }

    /// The name still names the locked folder (or file).
    fn holds(&self, root: &Path) -> bool {
        let (f, path) = match self {
            Lock::Folder(f) => (f, root.to_path_buf()),
            Lock::File(f) => (f, root.join(LOCK_FILE)),
        };
        match (f.metadata(), path.symlink_metadata()) {
            (Ok(a), Ok(b)) => ident(&a) == ident(&b),
            // The folder gone (moved away, unmounted): nothing is written
            // meanwhile, and the same folder put back is still the one
            // locked. A lock file gone is a lock lost.
            (Ok(_), Err(e)) => {
                matches!(self, Lock::Folder(_)) && e.kind() == std::io::ErrorKind::NotFound
            }
            _ => false,
        }
    }
}

/// Shared locks on the folders above `root`, held while it is checked and
/// its marker written. A folder kept in sync is locked for good
/// ([`Lock::take`]), so a claim of a folder around this one — kept, or
/// being taken this instant — and this one never both pass their checks.
fn lock_above(root: &Path) -> Result<Vec<File>> {
    let mut held = vec![];
    for a in root.ancestors().skip(1) {
        // One that can't be opened, or locked (some network disks), can't
        // take part.
        let Ok(f) = File::open(a) else {
            continue;
        };
        match f.try_lock_shared() {
            Ok(()) => held.push(f),
            Err(std::fs::TryLockError::WouldBlock) => bail!(
                "{} is inside {}, which another notekpr keeps in sync (or is taking) right now: a folder can't \
                 be kept in sync inside another",
                setup::shown(root),
                setup::shown(a)
            ),
            Err(std::fs::TryLockError::Error(_)) => {}
        }
    }
    Ok(held)
}

/// A Markdown folder taken (see the module docs), held until dropped.
pub struct FolderClaim {
    root: PathBuf,
    lock: Lock,
    /// Whose (its profile), under which id.
    store: ProfileStore,
    id: String,
}

impl Drop for FolderClaim {
    /// Its profile gave the folder back while the sync ran (`sync
    /// --no-dir`, [`release`]): the marker goes as it stops.
    fn drop(&mut self) {
        let kept = self
            .store
            .load()
            .ok()
            .flatten()
            .is_some_and(|p| p.sync.folder_id.as_deref() == Some(self.id.as_str()));
        if !kept
            && self.lock.holds(&self.root)
            && matches!(read_marker(&self.root), Found::Marker { ref id, .. } if *id == self.id)
        {
            let _ = std::fs::remove_file(self.root.join(MARKER));
        }
    }
}

impl std::fmt::Debug for FolderClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderClaim")
            .field("root", &self.root)
            .finish()
    }
}

/// How a keeper is named in its marker: "the CLI, profile 'default'".
pub fn cli_label(profile: &str) -> String {
    format!("the CLI, profile '{profile}'")
}

impl FolderClaim {
    /// Take `root` (which must exist) for the profile in `store`, named
    /// `by` in the marker. `take_over`: the person said the marker's keeper
    /// — one no profile the CLI or the app knows here — keeps it no more.
    pub fn take(
        root: &Path,
        store: &ProfileStore,
        by: &str,
        take_over: bool,
    ) -> Result<FolderClaim> {
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        if !root.is_dir() {
            bail!("{} isn't a folder", root.display());
        }
        let _above = lock_above(&root)?;
        let lock = Lock::take(&root)?;
        if let Some(other) = nested(&root) {
            return Err(nested_error(&root, &other));
        }
        let profile = store
            .load()?
            .ok_or_else(|| anyhow!("no profile '{}'", store.name()))?;
        // Every other profile the CLI and the app know of, as when choosing
        // (an older release's keeper writes no marker).
        setup::check_new_folder_among(
            &root,
            &[],
            &setup::keepers(store.dirs()),
            store.dirs(),
            store.name(),
        )?;
        judge(&root, store, profile.sync.folder_id.as_deref(), take_over)?;
        // The profile remembers its id before the marker names it.
        let id = match profile.sync.folder_id.clone() {
            Some(id) => id,
            None => {
                let id = notekpr_core::random_id();
                store.update(|p| p.sync.folder_id = Some(id.clone()))?;
                id
            }
        };
        let m = Marker {
            v: 1,
            id: id.clone(),
            by: shown_label(by),
        };
        let path = root.join(MARKER);
        crate::fsutil::write_atomic(&path, serde_json::to_string(&m)?.as_bytes(), true)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(FolderClaim {
            root,
            lock,
            store: store.clone(),
            id,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder (or the lock file) is still the one locked: not replaced
    /// by another of the same name (moved away and another made, a
    /// remount). If not, the sync must stop.
    pub fn holds(&self) -> bool {
        self.lock.holds(&self.root)
    }

    /// Taken, but nothing came of it (what was to follow failed, before the
    /// profile remembered the folder): the marker goes at once, while the
    /// folder is still locked, so that choosing it again isn't refused as
    /// someone else's.
    pub fn abandon(self) {
        if self.lock.holds(&self.root)
            && matches!(read_marker(&self.root), Found::Marker { ref id, .. } if *id == self.id)
        {
            let _ = std::fs::remove_file(self.root.join(MARKER));
        }
    }
}

/// Give a folder back (the profile in `store` stops keeping it, or goes):
/// its marker goes, if it is this profile's. A sync still running there
/// gives it back as it stops: the profile forgets its id now
/// ([`FolderClaim`]'s drop).
pub fn release(store: &ProfileStore) {
    let Ok(Some(profile)) = store.load() else {
        return;
    };
    let (Some(dir), Some(id)) = (&profile.sync.mirror_dir, &profile.sync.folder_id) else {
        return;
    };
    let Ok(root) = dir.canonicalize() else {
        return;
    };
    match Lock::take(&root) {
        Ok(_lock) => {
            if matches!(read_marker(&root), Found::Marker { id: ref m, .. } if m == id) {
                let _ = std::fs::remove_file(root.join(MARKER));
            }
        }
        Err(_) => {
            let _ = store.update(|p| p.sync.folder_id = None);
        }
    }
}

/// The marker's say when choosing `dir` for the profile `mine` (whose
/// folder id that is, if any): refused when another keeps it, or it is in
/// or around another Markdown folder. The claim at start decides; this
/// tells early.
pub fn check(dir: &Path, mine: &ProfileStore, take_over: bool) -> Result<()> {
    let root = resolve_lenient(dir);
    if let Some(other) = nested(&root) {
        return Err(nested_error(&root, &other));
    }
    let my_id = mine.load().ok().flatten().and_then(|p| p.sync.folder_id);
    judge(&root, mine, my_id.as_deref(), take_over)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Dirs, Profile};
    use notekpr_core::protocol::PrincipalKind;

    /// A profile `name` under `dirs`, keeping nothing yet.
    fn profile(dirs: &Dirs, name: &str) -> ProfileStore {
        let s = ProfileStore::new(dirs, name).unwrap();
        s.save(&Profile::new(
            "https://n.example.com",
            PrincipalKind::Human,
            false,
        ))
        .unwrap();
        s
    }

    /// Take `root` the way a sync does, and remember it as the profile's.
    fn keep(root: &Path, s: &ProfileStore) -> Result<FolderClaim> {
        let c = FolderClaim::take(root, s, &cli_label(s.name()), false)?;
        s.update(|p| p.sync.mirror_dir = Some(root.to_path_buf()))?;
        Ok(c)
    }

    fn setup() -> (tempfile::TempDir, Dirs, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(&tmp.path().join("cfg"));
        let root = tmp.path().join("Notes");
        std::fs::create_dir(&root).unwrap();
        (tmp, dirs, root)
    }

    fn err(r: Result<FolderClaim>) -> String {
        format!("{:#}", r.unwrap_err())
    }

    #[test]
    fn one_keeper_at_a_time_and_while_it_keeps_the_folder() {
        let (_tmp, dirs, root) = setup();
        let (alice, bob) = (profile(&dirs, "alice"), profile(&dirs, "bob"));
        let held = keep(&root, &alice).unwrap();
        let id = alice.load().unwrap().unwrap().sync.folder_id.unwrap();
        assert_eq!(
            read_marker(&root),
            Found::Marker {
                id: id.clone(),
                by: "the CLI, profile 'alice'".into()
            }
        );
        // Running: the lock says so, whoever asks.
        assert!(err(keep(&root, &bob)).contains("right now"));
        assert!(err(keep(&root, &alice)).contains("right now"));
        drop(held);
        // Stopped, still its folder: the profile says so.
        let e = err(keep(&root, &bob));
        assert!(
            e.contains("kept in sync already") && e.contains("'alice'"),
            "{e}"
        );
        assert!(check(&root, &bob, false).is_err());
        // Its own again, the same id.
        drop(keep(&root, &alice).unwrap());
        assert_eq!(alice.load().unwrap().unwrap().sync.folder_id, Some(id));
        // Let go (`sync --no-dir` while it ran: the marker stayed): free.
        alice.update(|p| p.sync.mirror_dir = None).unwrap();
        check(&root, &bob, false).unwrap();
        drop(keep(&root, &bob).unwrap());
        assert!(matches!(read_marker(&root), Found::Marker { by, .. } if by.contains("'bob'")));
    }

    #[test]
    fn a_keeper_unknown_here_is_taken_over_only_when_asked() {
        let (_tmp, dirs, root) = setup();
        let bob = profile(&dirs, "bob");
        std::fs::write(
            root.join(MARKER),
            r#"{"v":1,"id":"0123","by":"the desktop app (development 1a2b\u001b[31m\u202e)"}"#,
        )
        .unwrap();
        let e = err(keep(&root, &bob));
        assert!(
            e.contains("development 1a2b[31m") && e.contains("--take-over"),
            "{e}"
        );
        assert!(!e.contains('\u{1b}') && !e.contains('\u{202e}'));
        assert!(check(&root, &bob, false).is_err());
        check(&root, &bob, true).unwrap();
        drop(FolderClaim::take(&root, &bob, "bob", true).unwrap());
        assert!(matches!(read_marker(&root), Found::Marker { by, .. } if by == "bob"));
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_marker_is_never_written_through() {
        let (tmp, dirs, root) = setup();
        let bob = profile(&dirs, "bob");
        let precious = tmp.path().join("precious");
        std::fs::write(&precious, "keep me").unwrap();
        // A link to another file, then another name for it.
        std::os::unix::fs::symlink(&precious, root.join(MARKER)).unwrap();
        assert_eq!(read_marker(&root), Found::Strange);
        assert!(err(keep(&root, &bob)).contains("can't read"));
        drop(FolderClaim::take(&root, &bob, "bob", true).unwrap());
        assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep me");
        assert!(!root.join(MARKER).symlink_metadata().unwrap().is_symlink());
        std::fs::remove_file(root.join(MARKER)).unwrap();
        std::fs::hard_link(&precious, root.join(MARKER)).unwrap();
        assert_eq!(read_marker(&root), Found::Strange);
        drop(FolderClaim::take(&root, &bob, "bob", true).unwrap());
        assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep me");
        // A pipe: read without waiting for a writer.
        std::fs::remove_file(root.join(MARKER)).unwrap();
        let fifo = std::ffi::CString::new(root.join(MARKER).to_str().unwrap()).unwrap();
        // SAFETY: a valid C string; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read_marker(&root), Found::Strange);
    }

    #[test]
    fn never_in_or_around_another_markdown_folder() {
        let (_tmp, dirs, root) = setup();
        let (alice, bob) = (profile(&dirs, "alice"), profile(&dirs, "bob"));
        let inner = root.join("Family").join("Sub");
        std::fs::create_dir_all(&inner).unwrap();
        drop(keep(&inner, &alice).unwrap());
        let e = err(keep(&root, &bob));
        assert!(e.contains("can't be kept in sync inside another"), "{e}");
        assert!(check(&root, &bob, true).is_err());
        let deeper = inner.join("Deeper");
        std::fs::create_dir(&deeper).unwrap();
        assert!(err(keep(&deeper, &bob)).contains("inside another"));
        // Dot folders are never synced: one there doesn't count.
        release(&alice);
        alice.update(|p| p.sync.mirror_dir = None).unwrap();
        assert!(!inner.join(MARKER).exists());
        let hidden = root.join(".trash");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join(MARKER), "{}").unwrap();
        drop(keep(&root, &bob).unwrap());
    }

    /// A folder and one inside it, taken at the same instant by two
    /// profiles that don't know each other: never both.
    #[test]
    fn a_folder_and_one_inside_it_are_never_taken_together() {
        let (_tmp, dirs, root) = setup();
        let (alice, bob) = (profile(&dirs, "alice"), profile(&dirs, "bob"));
        let inner = root.join("Sub");
        std::fs::create_dir(&inner).unwrap();
        // The outer one being taken (locked, no marker yet): not the inner.
        let outer = File::open(&root).unwrap();
        outer.lock().unwrap();
        let e = err(keep(&inner, &bob));
        assert!(e.contains("right now") && e.contains("inside"), "{e}");
        outer.unlock().unwrap();
        // The inner one being taken (its claim holding the folders above
        // it): not the outer.
        outer.lock_shared().unwrap();
        assert!(err(keep(&root, &alice)).contains("right now"));
        drop(outer);
        drop(keep(&inner, &bob).unwrap());
        assert!(err(keep(&root, &alice)).contains("inside another"));
    }

    #[test]
    fn given_back_by_its_keeper_at_once_or_as_its_sync_stops() {
        let (_tmp, dirs, root) = setup();
        let (alice, bob) = (profile(&dirs, "alice"), profile(&dirs, "bob"));
        let held = keep(&root, &alice).unwrap();
        // Someone else's profile doesn't give it back.
        bob.update(|p| {
            p.sync.mirror_dir = Some(root.clone());
            p.sync.folder_id = Some("other".into());
        })
        .unwrap();
        release(&bob);
        assert!(root.join(MARKER).exists());
        bob.update(|p| p.sync.mirror_dir = None).unwrap();
        // Its own, while its sync runs (`sync --no-dir` then): as that
        // stops.
        release(&alice);
        assert!(root.join(MARKER).exists());
        drop(held);
        assert!(!root.join(MARKER).exists());
        // Not running: at once.
        drop(keep(&root, &alice).unwrap());
        assert!(root.join(MARKER).exists());
        release(&alice);
        assert!(!root.join(MARKER).exists());
    }

    #[test]
    fn a_claim_nothing_came_of_leaves_nothing_behind() {
        let (_tmp, dirs, root) = setup();
        let alice = profile(&dirs, "alice");
        let claim = FolderClaim::take(&root, &alice, &cli_label("alice"), false).unwrap();
        assert!(root.join(MARKER).exists());
        claim.abandon();
        assert!(!root.join(MARKER).exists());
        // Another profile may take it then.
        let bob = profile(&dirs, "bob");
        check(&root, &bob, false).unwrap();
        drop(FolderClaim::take(&root, &bob, &cli_label("bob"), false).unwrap());
    }

    /// Where the folder itself can't be locked: the lock file gone (a cloud
    /// tool, a person) is the lock lost.
    #[test]
    fn a_lock_file_gone_is_a_lock_lost() {
        let (_tmp, _dirs, root) = setup();
        let path = root.join(LOCK_FILE);
        let f = File::create(&path).unwrap();
        f.try_lock().unwrap();
        let lock = Lock::File(f);
        assert!(lock.holds(&root));
        std::fs::remove_file(&path).unwrap();
        assert!(!lock.holds(&root));
        File::create(&path).unwrap();
        assert!(!lock.holds(&root));
    }

    #[test]
    fn a_folder_replaced_while_kept_isn_t_the_one_held() {
        let (tmp, dirs, root) = setup();
        let alice = profile(&dirs, "alice");
        let held = keep(&root, &alice).unwrap();
        assert!(held.holds());
        // Moved away and back: the same folder.
        let away = tmp.path().join("away");
        std::fs::rename(&root, &away).unwrap();
        assert!(held.holds());
        std::fs::rename(&away, &root).unwrap();
        assert!(held.holds());
        // Another made in its place.
        std::fs::rename(&root, &away).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(!held.holds());
    }
}
