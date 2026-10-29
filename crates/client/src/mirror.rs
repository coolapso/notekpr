//! Plaintext file mirror: keeps `DIR/<vault name>/<vault path>` in sync with
//! the encrypted replicas, in both directions.
//!
//! **Remote → disk.** When a note changes, its file is replaced atomically —
//! but only if the file still matches what was last written or read. If it
//! was edited locally in the meantime, the local edit is ingested first and
//! the merged text written afterwards.
//!
//! **Disk → CRDT.** A file changed by an external editor is never copied over
//! the document. Each note keeps its last few *base* versions (CRDT state +
//! text hash): what we wrote to disk, and what we last read from it. When a
//! file changes, the base whose text is closest to the file — the version the
//! editor most likely loaded — is forked into a fresh document (new client
//! id), the textual diff `base → file` is applied there, and the resulting
//! update is merged into the live document. Remote edits made after that base
//! are therefore preserved, never overwritten.
//!
//! New files create entries; deleted files move entries to the trash
//! (recoverable) — in the quick notes, which keep no trash, they are deleted
//! for good; a delete plus a create of identical content within a short
//! window is a rename. The quick notes' folder is flat and holds at most as
//! many notes as they may: other new files wait on disk, as do new
//! drawings (`.drawing` files) while their vault holds as many as it may.
//! Dotfiles, editor swap/backup files and our own
//! temporary files are ignored. Change detection compares size + mtime and
//! falls back to content hashes, so our own writes are recognised.
//!
//! **Saved shapes.** With the Personal storage comes [`SHAPES_FILE`] at the
//! top level, Excalidraw's library file: written when the shapes change,
//! and an edit of it merged in shape by shape against what it last held
//! ([`sync_shapes`]). A file that isn't a library is left alone, and one
//! deleted is written again: neither removes any shape.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use notekpr_core::b64;
use notekpr_core::doc::{EntryKind, NoteDoc};
use notekpr_core::protocol::INDEX_OBJECT_ID;
use notekpr_core::{paths, textdiff};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use crate::fsutil;
use crate::hub::{Hub, HubEvent};
use crate::ui;
use crate::vault::{Vault, apply_edits_chunked, content_hash, fork_note};

/// Versions kept per note for base selection.
const MAX_BASES: usize = 4;
/// A file that disappears and reappears elsewhere within this window is a move.
pub const RENAME_WINDOW: Duration = Duration::from_millis(1500);
const DEBOUNCE: Duration = Duration::from_millis(300);
const REMOTE_DEBOUNCE: Duration = Duration::from_millis(150);
const FULL_SCAN_EVERY: Duration = Duration::from_secs(30);
/// Scan interval while the directory cannot be watched.
const POLL_EVERY: Duration = Duration::from_secs(5);
/// Files whose mtime is this close to when we recorded them are re-hashed,
/// since a same-size edit within the timestamp granularity is invisible to stat.
const RACY_NS: i64 = 2_000_000_000;
/// Temporary files of ours older than this are leftovers of a crash.
const STALE_TEMP_AGE: Duration = Duration::from_secs(60);
/// More than this many mirrored files of a vault missing at the same time —
/// and more than half of them — looks like a moved or unmounted folder, not
/// like deletions: nothing is trashed until confirmed.
pub const MASS_DELETE_MIN_FILES: usize = 10;
/// In the quick notes, deleting a file deletes the note for good: more than
/// this many missing at once are held the same way, however few they are.
pub const QUICK_MASS_DELETE_MIN_FILES: usize = 3;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Base {
    #[serde(with = "b64")]
    state: Vec<u8>,
    hash: String,
    /// Read from disk (saved by an editor) rather than written by us.
    #[serde(default)]
    read: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct FileRecord {
    /// Vault path the file was written to / read from.
    path: String,
    /// Where the file is, relative to the vault folder, if not at `path`: on
    /// a case-insensitive file system, entries whose paths differ only in
    /// case get distinct names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disk: Option<String>,
    kind: EntryKind,
    /// Hash of the content last written or ingested.
    hash: String,
    size: u64,
    mtime_ns: i64,
    checked_ns: i64,
    #[serde(default)]
    bases: Vec<Base>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct MirrorState {
    root: PathBuf,
    dir_name: String,
    #[serde(default)]
    files: BTreeMap<String, FileRecord>,
    /// Explicit folder entries materialised as directories: id -> path.
    #[serde(default)]
    folders: BTreeMap<String, String>,
    /// The saved shapes' file ([`SHAPES_FILE`]), kept with the Personal
    /// storage's state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shapes: Option<ShapesRecord>,
}

/// The saved shapes' file, at the mirror's top level beside the vault
/// folders (not in `Quick notes/`, whose files must be linked by a note):
/// Excalidraw's library file, as account exports have it.
pub const SHAPES_FILE: &str = notekpr_core::account_export::SAVED_SHAPES_FILE;

/// What the mirror knows of the saved shapes' file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct ShapesRecord {
    /// Hash of the content last written or read.
    hash: String,
    size: u64,
    mtime_ns: i64,
    checked_ns: i64,
    /// The library's entries when the file was last written
    /// ([`shapes_fingerprint`]): while they stay so, there's nothing to
    /// write. Empty: check.
    #[serde(default)]
    entries: String,
    /// The shapes as the file held them when last written or read, by id
    /// (each one's JSON, without its id): what an edit of the file is
    /// merged against.
    #[serde(default)]
    base: BTreeMap<String, serde_json::Value>,
}

struct MirrorVault {
    dir: PathBuf,
    state: MirrorState,
    dirty: bool,
    /// Recorded files missing on disk, and since when (rename detection).
    missing_since: BTreeMap<String, Instant>,
    /// Deletions are held: too many files (or the whole folder) vanished.
    held: bool,
    /// What the file system ignores when comparing names.
    fold: Fold,
    warned: BTreeSet<String>,
    /// Notes renamed on disk in this pass: people they're shared with get
    /// the new name once the vault is released.
    renamed: Vec<String>,
}

/// What one reconciliation pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub ingested: usize,
    pub created: usize,
    pub renamed: usize,
    pub trashed: usize,
    pub written: usize,
    pub removed: usize,
    pub conflicts: usize,
    /// Missing files *not* moved to the trash because too many vanished at
    /// once (see [`MASS_DELETE_MIN_FILES`]).
    pub held: usize,
    /// Missing files that are written again ([`MirrorOptions::restore_missing`]).
    pub restored: usize,
}

impl Report {
    fn add(&mut self, o: Report) {
        self.ingested += o.ingested;
        self.created += o.created;
        self.renamed += o.renamed;
        self.trashed += o.trashed;
        self.written += o.written;
        self.removed += o.removed;
        self.conflicts += o.conflicts;
        self.held += o.held;
        self.restored += o.restored;
    }

    pub fn local_changes(&self) -> usize {
        self.ingested + self.created + self.renamed + self.trashed
    }

    pub fn is_empty(&self) -> bool {
        *self == Report::default()
    }
}

/// Which remote changes to write out.
#[derive(Debug, Clone)]
pub enum Scope {
    All,
    Objects(BTreeSet<String>),
}

/// How the mirror treats files that vanished in bulk.
#[derive(Debug, Clone, Copy, Default)]
pub struct MirrorOptions {
    /// Move them to the trash anyway (the user confirmed the deletion).
    pub allow_mass_delete: bool,
    /// Write them again from the replica (and re-create a missing mirror
    /// directory), instead of holding.
    pub restore_missing: bool,
    /// Treat the mirror directory as case-insensitive (`None` = detect).
    #[doc(hidden)]
    pub case_insensitive: Option<bool>,
    /// Treat it as ignoring how accents are written (`None` = detect).
    #[doc(hidden)]
    pub unicode_insensitive: Option<bool>,
}

pub struct Mirror {
    hub: Arc<Hub>,
    root: PathBuf,
    vaults: BTreeMap<String, MirrorVault>,
    warned: BTreeSet<String>,
    opts: MirrorOptions,
    /// What the mirror directory ignores when comparing names.
    fold: Fold,
}

/// What a file system ignores when it compares names: case (macOS and
/// Windows defaults), and how accents are written (macOS: "é" composed or as
/// "e" plus a separate mark is one name, and listings may give either).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fold {
    pub case: bool,
    pub unicode: bool,
}

/// Comparison key of a path on a file system ignoring `fold`.
fn fold_key(fold: Fold, p: &str) -> String {
    let p = if fold.unicode {
        paths::nfc(p)
    } else {
        p.to_string()
    };
    if fold.case { p.to_lowercase() } else { p }
}

/// Whether `dir` is on a case-insensitive file system: create a lower-case
/// probe file and look for it under its upper-case name.
pub fn is_case_insensitive(dir: &Path) -> std::io::Result<bool> {
    let probe = fsutil::temp_path(dir);
    fsutil::create_private_file(&probe, b"")?;
    let upper = probe
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| dir.join(n.to_uppercase()));
    let found = upper.is_some_and(|u| u.symlink_metadata().is_ok());
    let _ = std::fs::remove_file(&probe);
    Ok(found)
}

/// Whether `dir` treats differently written accents as one name (macOS):
/// create a probe with a composed "é" and look for it decomposed.
pub fn is_unicode_insensitive(dir: &Path) -> std::io::Result<bool> {
    let base = fsutil::temp_path(dir);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("probe");
    let probe = dir.join(format!("{name}-\u{e9}"));
    fsutil::create_private_file(&probe, b"")?;
    let found = dir
        .join(format!("{name}-e\u{301}"))
        .symlink_metadata()
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    Ok(found)
}

/// Where each live note and attachment goes on disk, relative to the vault
/// folder. Normally its path; on a case-insensitive file system, entries
/// whose paths differ only in case would share one file (and overwrite each
/// other), so all but one get a distinct name (`idea 2.md`). Entries already
/// on disk stay where they are.
fn assign_disk_paths(
    live: &BTreeMap<String, (String, notekpr_core::doc::Entry)>,
    files: &BTreeMap<String, FileRecord>,
    fold: Fold,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut entries: Vec<(&String, &String)> = live
        .iter()
        .filter(|(_, (_, e))| e.kind != EntryKind::Folder)
        .map(|(id, (p, _))| (id, p))
        .collect();
    entries.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
    for (id, p) in &entries {
        if let Some(r) = files.get(*id)
            && r.path == **p
            && used.insert(fold_key(fold, r.disk()))
        {
            out.insert((*id).clone(), r.disk().to_string());
        }
    }
    let all: BTreeSet<String> = entries.iter().map(|(_, p)| fold_key(fold, p)).collect();
    let mut clashing = Vec::new();
    for (id, p) in &entries {
        if out.contains_key(*id) {
            continue;
        }
        if used.insert(fold_key(fold, p)) {
            out.insert((*id).clone(), (*p).clone());
        } else {
            clashing.push((*id, *p));
        }
    }
    for (id, p) in clashing {
        let d = paths::unique(p, |q| {
            let k = fold_key(fold, q);
            used.contains(&k) || all.contains(&k)
        });
        used.insert(fold_key(fold, &d));
        out.insert(id.clone(), d);
    }
    out
}

/// Live entries that may be mirrored (safe paths), by id.
fn mirrored_entries(v: &Vault) -> BTreeMap<String, (String, notekpr_core::doc::Entry)> {
    v.paths()
        .into_iter()
        .filter(|(p, _)| is_safe_vault_path(p))
        .map(|(p, e)| (e.id.clone(), (p, e)))
        .collect()
}

/// Marker in each vault folder naming the vault it mirrors, so a folder left
/// behind by a deleted or left vault is never imported into another one.
const VAULT_MARKER: &str = ".notekpr-vault";

fn vault_marker(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(VAULT_MARKER))
        .ok()
        .map(|s| s.trim().to_string())
}

/// The vault folders below a mirror directory that mirror one of `vault_ids`
/// (by their `.notekpr-vault` marker). Folders without a marker (or of
/// other vaults) are never included.
pub fn mirrored_folders(root: &Path, vault_ids: &BTreeSet<String>) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|d| vault_marker(d).is_some_and(|id| vault_ids.contains(&id)))
        .collect();
    out.sort();
    out
}

/// Whether a directory contains anything the mirror would sync.
fn has_mirrorable_content(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.filter_map(|e| e.ok())
            .any(|e| e.file_name().to_str().is_none_or(|n| !is_ignored_name(n)))
    })
}

/// `p` made absolute, with its longest existing ancestor canonicalised (works
/// for paths that do not exist).
fn resolve_lenient(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut base = abs.as_path();
    let mut rest = Vec::new();
    while base.symlink_metadata().is_err() {
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return abs,
        }
    }
    let mut out = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    for n in rest.iter().rev() {
        out.push(n);
    }
    out
}

/// Identity of a directory (device, inode), to notice it being replaced.
/// `Some(_)` while it exists.
fn dir_ident(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok().filter(|m| m.is_dir())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        Some((0, 0))
    }
}

async fn stored_state(v: &crate::session::SharedVault) -> Result<Option<MirrorState>> {
    let g = v.lock().await;
    Ok(g.store().load_aux("mirror")?.and_then(|b| {
        serde_json::from_slice(&b)
            .map_err(|e| tracing::warn!("mirror state unreadable: {e}"))
            .ok()
    }))
}

// ---------------------------------------------------------------- pure helpers

/// Names the mirror never syncs: dotfiles and dot-directories (`.git`,
/// `.obsidian`, `.trash`, our temporary files), editor swap/backup/lock files.
pub fn is_ignored_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.')
        || name.ends_with('~')
        || name == "4913"
        || name.starts_with("~$")
        || (name.starts_with('#') && name.ends_with('#'))
        || [
            ".swp",
            ".swo",
            ".swx",
            ".swn",
            ".tmp",
            ".part",
            ".crdownload",
            ".kate-swp",
            ".bak",
        ]
        .iter()
        .any(|s| lower.ends_with(s))
        || lower == "thumbs.db"
        || lower == "desktop.ini"
}

/// Whether a vault path may be materialised on disk: canonical, and no
/// hidden segment (a collaborator must not be able to plant `.git/hooks/...`).
/// On Windows, segments must also be valid Win32 file names (no drive
/// prefixes, reserved characters or device names), so a vault path can never
/// escape the mirror or export directory.
pub fn is_safe_vault_path(p: &str) -> bool {
    is_safe_vault_path_for(p, cfg!(windows))
}

/// [`is_safe_vault_path`] with the Windows rules switched on or off (tests).
pub fn is_safe_vault_path_for(p: &str, windows: bool) -> bool {
    // Normalizing may only change how accents are written (macOS names).
    paths::normalize(p).is_ok_and(|n| n == paths::nfc(p))
        && p.split('/').all(|s| {
            !is_ignored_name(s) && !s.contains('\0') && (!windows || is_windows_safe_name(s))
        })
}

/// A valid Win32 file name: none of `\ / : * ? " < > |` or control characters
/// (`:` would also allow drive prefixes such as `C:` and NTFS alternate data
/// streams), no trailing dot or space (Windows silently strips them), and not
/// a reserved device name (`CON`, `NUL`, `COM1`, …, also with an extension).
pub fn is_windows_safe_name(s: &str) -> bool {
    !s.is_empty()
        && !s.chars().any(|c| {
            matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
        })
        && !s.ends_with(['.', ' '])
        && !is_reserved_windows_name(s)
}

fn is_reserved_windows_name(s: &str) -> bool {
    let stem = s
        .split('.')
        .next()
        .unwrap_or(s)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let Some(rest) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = rest.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// Directory name for a vault: its name with path separators and characters
/// that are invalid on common filesystems replaced.
pub fn sanitize_dir_name(name: &str, fallback: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .to_string();
    if s.is_empty() || s == ".." {
        s = fallback.to_string();
    }
    if is_reserved_windows_name(&s) {
        s = format!("_{s}");
    }
    if s.len() > 200 {
        let mut cut = 200;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// Character-level edit cost (bytes deleted + inserted).
fn char_cost(from: &str, to: &str) -> usize {
    textdiff::diff(from, to)
        .iter()
        .map(|e| match e {
            textdiff::Edit::Delete { len, .. } => *len,
            textdiff::Edit::Insert { text, .. } => text.len(),
        })
        .sum()
}

/// Line-level edit cost (lines deleted + inserted). Edits by different people
/// usually touch different lines, so going from a base that already contains
/// a remote edit to the editor's file costs at least the lines of that edit —
/// unlike a character diff, which may cheaply "align" two unrelated lines.
fn line_cost(from: &str, to: &str) -> usize {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let a: Vec<&str> = from.split_inclusive('\n').collect();
    let b: Vec<&str> = to.split_inclusive('\n').collect();
    capture_diff_slices(Algorithm::Myers, &a, &b)
        .iter()
        .map(|op| match *op {
            DiffOp::Equal { .. } => 0,
            DiffOp::Delete { old_len, .. } => old_len,
            DiffOp::Insert { new_len, .. } => new_len,
            DiffOp::Replace {
                old_len, new_len, ..
            } => old_len + new_len,
        })
        .sum()
}

/// Index of the base text the editor most likely started from: the fewest
/// changed lines to reach `disk`, then the fewest changed bytes; ties go to
/// the newest (highest index). `bases` is ordered oldest first.
pub fn choose_base(bases: &[String], disk: &str) -> Option<usize> {
    let mut best: Option<(usize, (usize, usize))> = None;
    for (i, b) in bases.iter().enumerate() {
        let c = (line_cost(b, disk), char_cost(b, disk));
        if best.is_none_or(|(_, bc)| c <= bc) {
            best = Some((i, c));
        }
    }
    best.map(|(i, _)| i)
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// What `stat` saw. `ino` and `ctime_ns` (Unix) make a replaced or rewritten
/// file visible even when size and mtime happen to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime_ns: i64,
    ino: u64,
    ctime_ns: i64,
}

impl Stat {
    fn of(m: &std::fs::Metadata) -> Stat {
        #[cfg(unix)]
        let (ino, ctime_ns) = {
            use std::os::unix::fs::MetadataExt;
            (m.ino(), m.ctime() * 1_000_000_000 + m.ctime_nsec())
        };
        #[cfg(not(unix))]
        let (ino, ctime_ns) = (0, 0);
        Stat {
            size: m.len(),
            mtime_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            ino,
            ctime_ns,
        }
    }
}

/// What is at a path, without following a final symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    File(Stat),
    Missing,
    /// A symlink, directory or special file: never read, written or deleted.
    Other,
}

fn stat(path: &Path) -> std::io::Result<Node> {
    use std::io::ErrorKind;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(Node::File(Stat::of(&m))),
        Ok(_) => Ok(Node::Other),
        // A parent directory replaced by a file is as good as missing.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(Node::Missing)
        }
        Err(e) => Err(e),
    }
}

enum DiskState {
    Missing,
    /// Replaced by a symlink, directory or special file.
    NotAFile,
    /// Content as recorded; the stat taken before any read.
    Unchanged(Stat),
    /// Changed content, and the stat taken *before* it was read.
    Modified(Vec<u8>, Stat),
}

/// What a record remembers of its file, for [`disk_state`].
trait Seen {
    /// Hash, size, mtime and when it was checked.
    fn seen(&self) -> (&str, u64, i64, i64);
    fn set_stat(&mut self, st: Stat);
}

impl Seen for FileRecord {
    fn seen(&self) -> (&str, u64, i64, i64) {
        (&self.hash, self.size, self.mtime_ns, self.checked_ns)
    }
    fn set_stat(&mut self, st: Stat) {
        FileRecord::set_stat(self, st)
    }
}

impl Seen for ShapesRecord {
    fn seen(&self) -> (&str, u64, i64, i64) {
        (&self.hash, self.size, self.mtime_ns, self.checked_ns)
    }
    fn set_stat(&mut self, st: Stat) {
        self.size = st.size;
        self.mtime_ns = st.mtime_ns;
        self.checked_ns = now_ns();
    }
}

/// Compare the file with its record (cheap stat first, then content).
fn disk_state(path: &Path, rec: &mut impl Seen) -> std::io::Result<DiskState> {
    let st = match stat(path)? {
        Node::File(st) => st,
        Node::Missing => return Ok(DiskState::Missing),
        Node::Other => return Ok(DiskState::NotAFile),
    };
    let (hash, size, mtime_ns, checked_ns) = rec.seen();
    let racy = checked_ns - mtime_ns < RACY_NS;
    if st.size == size && st.mtime_ns == mtime_ns && !racy {
        return Ok(DiskState::Unchanged(st));
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DiskState::Missing),
        Err(e) => return Err(e),
    };
    if content_hash(&data) == hash {
        rec.set_stat(st);
        return Ok(DiskState::Unchanged(st));
    }
    Ok(DiskState::Modified(data, st))
}

/// Disk path of a `/`-separated relative path below `dir`. Every segment must
/// be one plain path component (no drive prefix, root, `..`, or a separator
/// of the platform), and the result must stay below `dir`.
pub(crate) fn disk_path(dir: &Path, rel: &str) -> Result<PathBuf> {
    use std::path::Component;
    let mut p = dir.to_path_buf();
    for seg in rel.split('/') {
        let mut comps = Path::new(seg).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(c)), None) if c == std::ffi::OsStr::new(seg) => p.push(c),
            _ => bail!("a path segment is not supported on this system"),
        }
    }
    if p == dir || !p.starts_with(dir) {
        bail!("a path does not stay inside its directory");
    }
    Ok(p)
}

/// Files (and empty directories) below `dir`, by vault path.
fn scan_dir(dir: &Path) -> (BTreeMap<String, Stat>, BTreeSet<String>) {
    let mut files = BTreeMap::new();
    let mut nonempty: BTreeSet<String> = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    let walker = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str().is_some_and(|n| !is_ignored_name(n)));
    for e in walker.filter_map(|e| e.ok()) {
        let Ok(rel) = e.path().strip_prefix(dir) else {
            continue;
        };
        let Some(rel) = rel
            .to_str()
            .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
        else {
            continue;
        };
        if !is_safe_vault_path(&rel) {
            continue;
        }
        let ft = e.file_type();
        if ft.is_dir() {
            dirs.insert(rel);
        } else if ft.is_file()
            && let Ok(Node::File(st)) = stat(e.path())
        {
            let mut p = paths::parent(&rel);
            while !p.is_empty() {
                nonempty.insert(p.to_string());
                p = paths::parent(p);
            }
            files.insert(rel, st);
        }
    }
    let empty = dirs.into_iter().filter(|d| !nonempty.contains(d)).collect();
    (files, empty)
}

/// Outcome of [`replace_if_unchanged`].
#[derive(Debug, PartialEq, Eq)]
enum Replaced {
    Written,
    /// The file changed since it was observed; it was left alone (the next
    /// pass ingests it).
    Changed,
    /// Written, but an editor wrote into the old file in the instant between
    /// the last check and the rename: this is what it wrote (to be ingested).
    Recovered(Vec<u8>),
}

/// Replace `path` atomically, unless it changed since `expected` was observed
/// (`None` = it must not exist). `expected` must be a stat taken *before* the
/// content it describes was read. The check is repeated right before the
/// rename; an in-place write that still slips in is recovered from the old
/// file (Unix).
fn replace_if_unchanged(path: &Path, data: &[u8], expected: Option<Stat>) -> Result<Replaced> {
    replace_with_hook(path, data, expected, || {})
}

/// [`replace_if_unchanged`] with a hook that runs between the final check and
/// the rename (tests simulate a racing editor there).
fn replace_with_hook(
    path: &Path,
    data: &[u8],
    expected: Option<Stat>,
    after_check: impl FnOnce(),
) -> Result<Replaced> {
    let unchanged = |n: Node| match (n, expected) {
        (Node::Missing, None) => true,
        (Node::File(a), Some(b)) => a == b,
        _ => false,
    };
    if !unchanged(stat(path)?) {
        return Ok(Replaced::Changed);
    }
    if let Some(parent) = path.parent() {
        fsutil::create_private_dir(parent)?;
    }
    // Keep the old file open: if an editor writes into it after our last
    // check, its content is still readable after the rename.
    #[cfg(unix)]
    let old = expected.and_then(|_| std::fs::File::open(path).ok());
    let written = fsutil::write_atomic_if(path, data, true, || {
        let ok = unchanged(stat(path)?);
        if ok {
            after_check();
        }
        Ok(ok)
    })
    .with_context(|| "writing a mirrored file")?;
    if !written {
        return Ok(Replaced::Changed);
    }
    #[cfg(unix)]
    if let (Some(mut f), Some(exp)) = (old, expected)
        && let Ok(m) = f.metadata()
    {
        let now = Stat::of(&m);
        // (The rename itself changes the old file's ctime, not its content.)
        if now.size != exp.size || now.mtime_ns != exp.mtime_ns {
            use std::io::{Read, Seek};
            let mut buf = Vec::new();
            if f.seek(std::io::SeekFrom::Start(0)).is_ok() && f.read_to_end(&mut buf).is_ok() {
                return Ok(Replaced::Recovered(buf));
            }
        }
    }
    Ok(Replaced::Written)
}

fn conflict_name(path: &str, taken: impl Fn(&str) -> bool) -> String {
    let stamp = ui::fmt_time(notekpr_core::now_ms())
        .replace(':', "")
        .replace('T', " ")
        .replace('Z', "");
    let parent = paths::parent(path);
    let name = paths::file_name(path);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    paths::unique(
        &paths::join(parent, &format!("{stem} (conflict {stamp}){ext}")),
        taken,
    )
}

/// Remove now-empty parent directories of `path` up to (excluding) `stop`.
fn prune_empty_parents(path: &Path, stop: &Path) {
    let mut cur = path.parent();
    while let Some(d) = cur {
        if d == stop || !d.starts_with(stop) || std::fs::remove_dir(d).is_err() {
            break;
        }
        cur = d.parent();
    }
}

/// Remember a version of the file. `read` = it was read from disk (the
/// editor saved it), otherwise we wrote it.
fn push_base(rec: &mut FileRecord, state: Vec<u8>, hash: &str, read: bool) {
    let mut read = read;
    if let Some(last) = rec.bases.last()
        && last.hash == hash
    {
        // Same text: the editor has it if either version says so.
        read |= last.read;
        rec.bases.pop();
    }
    rec.bases.push(Base {
        state,
        hash: hash.to_string(),
        read,
    });
    let excess = rec.bases.len().saturating_sub(MAX_BASES);
    rec.bases.drain(..excess);
}

/// The bases an editor can have started from: the newest version it saved
/// (read from disk), and what we wrote after that (it may have reloaded
/// those). Anything older is history it has moved past — a file equal to
/// such a version is a deliberate revert (undo, `git checkout`), and must be
/// diffed against what the editor saved last, not treated as "no change".
fn candidate_bases(bases: &[Base]) -> &[Base] {
    let from = bases.iter().rposition(|b| b.read).unwrap_or(0);
    &bases[from..]
}

/// What the folder holds for a note or drawing: its text, or the drawing as
/// a `.drawing` file (Excalidraw's JSON, the pictures inside).
fn file_text(v: &Vault, id: &str, kind: EntryKind) -> String {
    match kind {
        EntryKind::Drawing => v.drawing_file(id),
        _ => v.note_text(id),
    }
}

/// Apply an external edit of a note or drawing relative to the best base.
/// Returns the state of the fork (a valid base for what is on disk now).
fn ingest_note(v: &mut Vault, id: &str, rec: &FileRecord, disk: &str) -> Result<Vec<u8>> {
    match rec.kind {
        EntryKind::Drawing => ingest_drawing(v, id, rec, disk),
        _ => ingest_text(v, id, rec, disk),
    }
}

/// A drawing read from its file in the folder; `None` if it isn't one (yet:
/// an editor may be halfway through saving).
fn drawing_from_file(text: &str) -> Option<notekpr_draw::scene::Scene> {
    let mut ids = notekpr_draw::restore::Ids::new(now_ns() as u64);
    notekpr_draw::file::read_drawing(text, &mut ids)
        .ok()
        .map(|d| d.scene)
}

/// [`ingest_note`] for a drawing: what changed between the base and the
/// file, element by element and property by property, is applied to the
/// drawing — changes made elsewhere meanwhile stay.
fn ingest_drawing(v: &mut Vault, id: &str, rec: &FileRecord, disk: &str) -> Result<Vec<u8>> {
    let live_state = v
        .replica
        .notes
        .entry(id.to_string())
        .or_default()
        .encode_state();
    let Some(scene) = drawing_from_file(disk) else {
        bail!("a .drawing file in the folder is not a drawing (any more); it is not synced");
    };
    // Pictures new in the file become files of the vault.
    let current = v.drawing(id);
    let path = v
        .paths()
        .into_iter()
        .find(|(_, e)| e.id == id)
        .map_or_else(String::new, |(p, _)| p);
    let scene = v.adopt_pictures(&path, &scene, Some(&current))?;
    if current == scene {
        return Ok(live_state);
    }
    // The base sharing the most elements with the file is the one the
    // editor started from.
    let mut candidates: Vec<NoteDoc> = candidate_bases(&rec.bases)
        .iter()
        .filter_map(|b| fork_note(&b.state).ok())
        .collect();
    let shared = |doc: &NoteDoc| {
        let base = doc.drawing().scene();
        scene
            .elements
            .iter()
            .filter(|e| base.element(e.id()) == Some(*e))
            .count()
    };
    let best = (0..candidates.len()).max_by_key(|&i| (shared(&candidates[i]), i));
    let fork = match best {
        Some(i) => candidates.swap_remove(i),
        None => fork_note(&live_state)?,
    };
    let updates = fork.drawing().set_scene_split(&scene, v.update_budget());
    v.merge_note_updates(id, updates)?;
    Ok(fork.encode_state())
}

/// [`ingest_note`] for a note.
fn ingest_text(v: &mut Vault, id: &str, rec: &FileRecord, disk: &str) -> Result<Vec<u8>> {
    let live_state = v
        .replica
        .notes
        .entry(id.to_string())
        .or_default()
        .encode_state();
    if v.note_text(id) == disk {
        return Ok(live_state);
    }
    let mut candidates: Vec<NoteDoc> = candidate_bases(&rec.bases)
        .iter()
        .filter_map(|b| fork_note(&b.state).ok())
        .collect();
    let texts: Vec<String> = candidates.iter().map(|f| f.text()).collect();
    let fork = match choose_base(&texts, disk) {
        Some(i) => candidates.swap_remove(i),
        None => fork_note(&live_state)?,
    };
    let base_text = fork.text();
    // A huge edit (a big paste) becomes several updates the server accepts.
    let updates = apply_edits_chunked(&fork, &textdiff::diff(&base_text, disk), v.update_budget());
    v.merge_note_updates(id, updates)?;
    Ok(fork.encode_state())
}

/// Remove a file only if it still has the recorded content: it is first moved
/// aside (atomic), then checked, so an editor saving at that moment is never
/// lost — its version is put back instead. Returns whether it was removed.
fn remove_if_unchanged(path: &Path, hash: &str) -> Result<bool> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let aside = fsutil::temp_path(dir);
    match std::fs::rename(path, &aside) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("removing a mirrored file"),
    }
    let same = std::fs::read(&aside).is_ok_and(|d| content_hash(&d) == hash);
    if same {
        std::fs::remove_file(&aside).context("removing a mirrored file")?;
        return Ok(true);
    }
    // Changed at the last moment: put it back, without clobbering a file
    // created there in the meantime.
    let back = std::fs::hard_link(&aside, path)
        .and_then(|_| std::fs::remove_file(&aside))
        .or_else(|_| match stat(path)? {
            Node::Missing => std::fs::rename(&aside, path),
            _ => Err(std::io::Error::other("a file was created in its place")),
        });
    if back.is_err() {
        // Last resort: keep it next to the original name as a conflict copy.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let copy = conflict_name(name, |q| dir.join(q).symlink_metadata().is_ok());
            std::fs::rename(&aside, dir.join(copy)).context("keeping a changed file")?;
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------- saved shapes

/// The saved shapes as kept, in short: whether they changed since the file
/// was written.
fn shapes_fingerprint(v: &Vault) -> String {
    let mut entries = v.replica.index.library_entries();
    entries.sort();
    let mut all = Vec::new();
    for (id, json) in entries {
        all.extend_from_slice(id.as_bytes());
        all.push(0);
        all.extend_from_slice(json.as_bytes());
        all.push(0);
    }
    content_hash(&all)
}

/// The shapes of a library file; `None` if it isn't one (yet: an editor
/// may be halfway through saving).
fn shapes_from_file(data: &[u8]) -> Option<Vec<notekpr_core::library::LibraryItem>> {
    let text = std::str::from_utf8(data).ok()?;
    let mut ids = notekpr_draw::restore::Ids::new(now_ns() as u64);
    notekpr_draw::file::read_library(text, notekpr_core::now_ms() as f64, &mut ids).ok()
}

/// Shapes by id, as a [`ShapesRecord`] keeps them.
fn shapes_base(
    items: &[notekpr_core::library::LibraryItem],
) -> BTreeMap<String, serde_json::Value> {
    items
        .iter()
        .map(|i| (i.id.clone(), notekpr_draw::library::item_json(i)))
        .collect()
}

/// [`shapes_base`] read back.
fn base_shapes(
    base: &BTreeMap<String, serde_json::Value>,
) -> Vec<notekpr_core::library::LibraryItem> {
    let mut ids = notekpr_draw::restore::Ids::new(now_ns() as u64);
    base.iter()
        .filter_map(|(id, v)| notekpr_draw::library::read_item(v, Some(id), 0.0, &mut ids))
        .collect()
}

/// What [`sync_shapes`] did.
#[derive(Debug, Default, Clone, Copy)]
struct ShapesDone {
    /// An edit of the file was merged into the saved shapes.
    ingested: bool,
    /// The file was written.
    written: bool,
}

/// The saved shapes' file at the mirror's top level (`root`), kept with the
/// Personal storage (`v`, mirrored by `mv`). An edit of the file is merged
/// in shape by shape against what it last held (`notekpr_core::library::
/// merge`): shapes added, changed or taken out there are added, changed or
/// removed, and changes made elsewhere meanwhile to other shapes stay. A
/// file that isn't a library (any more) is left alone with a warning and
/// changes nothing. With `write`, the file is then written when the shapes
/// changed — unless it was edited meanwhile (the next pass reads it) — or
/// written again when it is gone: deleting it removes no shapes. With no
/// shapes and no file, there's none.
fn sync_shapes(
    mv: &mut MirrorVault,
    root: &Path,
    v: &mut Vault,
    writable: bool,
    write: bool,
) -> Result<ShapesDone> {
    let mut done = ShapesDone::default();
    let path = root.join(SHAPES_FILE);
    // 1. An edit on disk, merged in.
    let on_disk = match stat(&path)? {
        Node::Other => {
            mv.warn_once(
                "shapes:other",
                &format!(
                    "mirror: {SHAPES_FILE} is not a file; the saved shapes are not mirrored until it is"
                ),
            );
            return Ok(done);
        }
        Node::Missing => None,
        Node::File(st) => {
            let edited = match mv.state.shapes.as_mut() {
                Some(rec) => match disk_state(&path, rec)? {
                    DiskState::Modified(data, st) => Some((data, st)),
                    DiskState::Unchanged(_) => None,
                    // (Gone or replaced this very moment: the next pass.)
                    DiskState::Missing | DiskState::NotAFile => return Ok(done),
                },
                // Not ours yet: one made here, or kept from before.
                None => Some((std::fs::read(&path)?, st)),
            };
            match edited {
                None => Some(st),
                // Left alone: neither read nor written over.
                Some(_) if !writable => return Ok(done),
                Some((data, st)) => {
                    let Some(file) = shapes_from_file(&data) else {
                        mv.warn_once(
                            "shapes:broken",
                            &format!(
                                "mirror: {SHAPES_FILE} is not an Excalidraw library (any more); it is left \
                                 alone, and your saved shapes stay as they are until it is one again"
                            ),
                        );
                        return Ok(done);
                    };
                    mv.warned.remove("shapes:broken");
                    let base = mv
                        .state
                        .shapes
                        .as_ref()
                        .map(|r| base_shapes(&r.base))
                        .unwrap_or_default();
                    let merged = notekpr_core::library::merge(
                        &v.replica.index,
                        &base,
                        &file,
                        v.update_budget(),
                    );
                    for u in &merged.updates {
                        v.record(INDEX_OBJECT_ID, u.clone())?;
                    }
                    if merged.too_large > 0 {
                        ui::warn(&format!(
                            "mirror: {} shape(s) in {SHAPES_FILE} are too large for this server and were not saved",
                            merged.too_large
                        ));
                    }
                    done.ingested = merged.any();
                    mv.state.shapes = Some(ShapesRecord {
                        hash: content_hash(&data),
                        size: st.size,
                        mtime_ns: st.mtime_ns,
                        checked_ns: now_ns(),
                        entries: String::new(),
                        base: shapes_base(&file),
                    });
                    mv.dirty = true;
                    Some(st)
                }
            }
        }
    };
    if !write {
        return Ok(done);
    }
    // 2. The shapes, written when they changed. (Twice at most: what an
    // editor saved at the very last moment is merged in, and written.)
    let mut expected = on_disk;
    for _ in 0..2 {
        let fingerprint = shapes_fingerprint(v);
        if expected.is_some()
            && mv
                .state
                .shapes
                .as_ref()
                .is_some_and(|r| r.entries == fingerprint)
        {
            break;
        }
        let items = notekpr_core::library::items(&v.replica.index);
        if expected.is_none() && items.is_empty() {
            // No shapes, no file.
            if mv.state.shapes.take().is_some() {
                mv.dirty = true;
            }
            break;
        }
        let text = notekpr_draw::file::write_library(&items);
        let hash = content_hash(text.as_bytes());
        if let (Some(rec), Some(_)) = (mv.state.shapes.as_mut(), expected)
            && rec.hash == hash
        {
            rec.entries = fingerprint;
            mv.dirty = true;
            break;
        }
        let before = mv.state.shapes.as_ref().map(|r| base_shapes(&r.base));
        let replaced = replace_if_unchanged(&path, text.as_bytes(), expected)?;
        if replaced == Replaced::Changed {
            // Edited meanwhile: the next pass reads it.
            break;
        }
        let st = match stat(&path)? {
            Node::File(st) => st,
            _ => break,
        };
        done.written = true;
        mv.state.shapes = Some(ShapesRecord {
            hash,
            size: st.size,
            mtime_ns: st.mtime_ns,
            checked_ns: now_ns(),
            entries: fingerprint,
            base: shapes_base(&items),
        });
        mv.dirty = true;
        let Replaced::Recovered(saved) = replaced else {
            break;
        };
        // What the editor saved, against what the file held before.
        if writable && let Some(file) = shapes_from_file(&saved) {
            let merged = notekpr_core::library::merge(
                &v.replica.index,
                &before.unwrap_or_default(),
                &file,
                v.update_budget(),
            );
            for u in &merged.updates {
                v.record(INDEX_OBJECT_ID, u.clone())?;
            }
            done.ingested |= merged.any();
        }
        expected = Some(st);
    }
    Ok(done)
}

impl FileRecord {
    /// Location of the file relative to the vault folder.
    fn disk(&self) -> &str {
        self.disk.as_deref().unwrap_or(&self.path)
    }

    fn set_stat(&mut self, st: Stat) {
        self.size = st.size;
        self.mtime_ns = st.mtime_ns;
        self.checked_ns = now_ns();
    }
}

impl MirrorVault {
    fn record_stat(&mut self, id: &str, path: &Path) {
        if let (Some(rec), Ok(Node::File(st))) = (self.state.files.get_mut(id), stat(path)) {
            rec.set_stat(st);
        }
        self.dirty = true;
    }

    fn warn_once(&mut self, key: &str, msg: &str) {
        if self.warned.insert(key.to_string()) {
            ui::warn(msg);
        }
    }

    /// One entry could not be processed: say so (once) and carry on with the
    /// others. The path is only logged at debug level.
    fn entry_failed(&mut self, vault_name: &str, key: &str, e: &anyhow::Error) {
        tracing::debug!(entry = key, "mirror: skipped an entry: {e:#}");
        self.warn_once(
            &format!("err:{key}"),
            &format!(
                "vault '{vault_name}': a file could not be synced and was skipped ({e:#}); \
                 NOTEKPR_LOG=debug shows which"
            ),
        );
    }

    fn key(&self, p: &str) -> String {
        fold_key(self.fold, p)
    }

    /// Whether `q` is taken: by a live entry (`live_keys`, folded), by a
    /// recorded file, or on disk (anything at all is there).
    fn taken(&self, q: &str, live_keys: &BTreeSet<String>) -> bool {
        let k = self.key(q);
        live_keys.contains(&k)
            || self.state.files.values().any(|r| self.key(r.disk()) == k)
            || disk_path(&self.dir, q).map_or(true, |p| p.symlink_metadata().is_ok())
    }
}

/// Pass step 1 for one recorded file: modified, missing or replaced?
fn ingest_record(
    mv: &mut MirrorVault,
    v: &mut Vault,
    id: &str,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let Some(mut rec) = mv.state.files.get(id).cloned() else {
        return Ok(());
    };
    let path = disk_path(&mv.dir, rec.disk())?;
    match disk_state(&path, &mut rec)? {
        DiskState::Unchanged(_) => {
            mv.missing_since.remove(id);
        }
        DiskState::Missing => {
            mv.missing_since
                .entry(id.to_string())
                .or_insert_with(Instant::now);
            return Ok(());
        }
        DiskState::NotAFile => {
            // Not a deletion: the note is kept, the path is just not synced.
            mv.missing_since.remove(id);
            mv.warn_once(
                &format!("notfile:{id}"),
                &format!(
                    "vault '{}': a synced file was replaced by a symlink, folder or special file; it is not \
                     synced any more (the note itself is kept)",
                    v.name()
                ),
            );
            return Ok(());
        }
        DiskState::Modified(data, st) => {
            mv.missing_since.remove(id);
            if !writable {
                mv.warn_once(
                    &format!("ro:{id}"),
                    &format!(
                        "vault '{}' is read-only for you; a local edit to one of its files is kept on disk but \
                         not synced",
                        v.name()
                    ),
                );
                return Ok(());
            }
            // Edited here while trashed or deleted elsewhere: `write_remote`
            // keeps the file (and the edit) instead of deleting it.
            if !v.entry(id).is_some_and(|e| e.is_live()) {
                return Ok(());
            }
            let hash = content_hash(&data);
            match rec.kind {
                EntryKind::Note | EntryKind::Drawing => {
                    let Ok(text) = String::from_utf8(data) else {
                        mv.warn_once(
                            &format!("utf8:{id}"),
                            "a Markdown file is not valid UTF-8; it is not synced",
                        );
                        return Ok(());
                    };
                    let base_state = ingest_note(v, id, &rec, &text)?;
                    push_base(&mut rec, base_state, &hash, true);
                }
                EntryKind::Attachment => {
                    // Blobs are immutable: a new version is a new entry
                    // (the previous one goes to the trash).
                    let res = v.add_attachment(&rec.path, &data, None)?;
                    mv.state.files.remove(id);
                    rec.hash = hash;
                    rec.set_stat(st);
                    mv.state.files.insert(res.id.clone(), rec);
                    mv.dirty = true;
                    touched.insert(res.id);
                    report.ingested += 1;
                    return Ok(());
                }
                EntryKind::Folder => return Ok(()),
            }
            rec.hash = hash;
            rec.set_stat(st);
            touched.insert(id.to_string());
            report.ingested += 1;
        }
    }
    if mv.state.files.get(id) != Some(&rec) {
        mv.state.files.insert(id.to_string(), rec);
        mv.dirty = true;
    }
    Ok(())
}

/// Pass step 2 for one unknown file: a rename of a missing file, or new.
fn ingest_new_file(
    mv: &mut MirrorVault,
    v: &mut Vault,
    path: &str,
    st: Stat,
    max_attachment: u64,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let full = disk_path(&mv.dir, path)?;
    let data = match std::fs::read(&full) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("reading a new file"),
    };
    let hash = content_hash(&data);
    let is_note = paths::is_note(path);
    // A .drawing file becomes a drawing (anything else by that name, or
    // one in the quick notes, which hold notes, stays a file).
    let drawing = if paths::is_drawing(path) && !v.is_quick_notes() {
        std::str::from_utf8(&data).ok().and_then(drawing_from_file)
    } else {
        None
    };
    let kind = if is_note {
        EntryKind::Note
    } else if drawing.is_some() {
        EntryKind::Drawing
    } else {
        EntryKind::Attachment
    };
    let moved = mv
        .missing_since
        .keys()
        .find(|id| {
            mv.state
                .files
                .get(*id)
                .is_some_and(|r| r.hash == hash && r.kind == kind)
        })
        .cloned();
    if let Some(id) = moved
        && v.entry(&id).is_some_and(|e| e.is_live())
    {
        match v.move_path(&id, path, false) {
            Ok(new_path) => {
                mv.missing_since.remove(&id);
                if let Some(r) = mv.state.files.get_mut(&id) {
                    r.disk = (new_path != path).then(|| path.to_string());
                    r.path = new_path;
                    r.set_stat(st);
                }
                mv.dirty = true;
                mv.renamed.push(id.clone());
                touched.insert(id);
                report.renamed += 1;
                return Ok(());
            }
            Err(e) => tracing::debug!("rename not applied: {e:#}"),
        }
    }
    // The quick notes are flat, hold a limited number of notes and keep
    // files only for the notes linking to them: other files wait on disk
    // (tried again on every pass).
    if v.is_quick_notes() {
        if !notekpr_core::quick::is_top_level(path) {
            mv.warn_once(
                "quick:folders",
                "quick notes have no folders: files in folders inside 'Quick notes' are not synced",
            );
            return Ok(());
        }
        if is_note && let Err(e) = v.require_room() {
            mv.warn_once(
                &format!("quick:full:{path}"),
                &format!("{e:#} A new file in 'Quick notes' is synced once there's room."),
            );
            return Ok(());
        }
        if !is_note && v.require_linked(path).is_err() {
            mv.warn_once(
                &format!("quick:unlinked:{path}"),
                "quick notes keep files only for their notes: a file in 'Quick notes' is synced once a quick note links to it",
            );
            return Ok(());
        }
    }
    // A vault holds so many drawings: a new one waits on disk until there's
    // room (tried again on every pass).
    if drawing.is_some()
        && !v.is_quick_notes()
        && let Err(e) = v.require_drawing_room()
    {
        mv.warn_once(
            &format!("drawings:full:{path}"),
            &format!("{e:#} '{path}' is synced once there's room."),
        );
        return Ok(());
    }
    let (r, state) = if let Some(scene) = &drawing {
        let r = v.create_drawing(path, scene)?;
        let state = v
            .replica
            .notes
            .get(&r.id)
            .map(|n| n.encode_state())
            .unwrap_or_default();
        (r, Some(state))
    } else if is_note {
        let Ok(text) = String::from_utf8(data) else {
            mv.warn_once(
                &format!("utf8:{path}"),
                "a Markdown file is not valid UTF-8; it is not synced",
            );
            return Ok(());
        };
        let r = v.create_note(path, &text)?;
        let state = v
            .replica
            .notes
            .get(&r.id)
            .map(|n| n.encode_state())
            .unwrap_or_default();
        (r, Some(state))
    } else {
        if data.len() as u64 > max_attachment {
            mv.warn_once(
                &format!("big:{path}"),
                &format!(
                    "a file is larger than the server limit ({max_attachment} bytes); not synced"
                ),
            );
            return Ok(());
        }
        (v.add_attachment(path, &data, None)?, None)
    };
    let mut rec = FileRecord {
        disk: (r.path != path).then(|| path.to_string()),
        path: r.path.clone(),
        kind,
        hash: hash.clone(),
        size: st.size,
        mtime_ns: st.mtime_ns,
        checked_ns: now_ns(),
        bases: vec![],
    };
    if let Some(s) = state {
        push_base(&mut rec, s, &hash, true);
    }
    mv.state.files.insert(r.id.clone(), rec);
    mv.dirty = true;
    touched.insert(r.id);
    report.created += 1;
    Ok(())
}

/// Write step a for one recorded entry that is gone (trashed, removed) or
/// became a folder: remove the file, unless it was edited locally.
fn remove_gone(
    mv: &mut MirrorVault,
    v: &mut Vault,
    id: &str,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let Some(mut rec) = mv.state.files.get(id).cloned() else {
        return Ok(());
    };
    let path = disk_path(&mv.dir, rec.disk())?;
    let state = disk_state(&path, &mut rec)?;
    mv.state.files.remove(id);
    mv.dirty = true;
    mv.missing_since.remove(id);
    let edited = match state {
        DiskState::Missing | DiskState::NotAFile => return Ok(()),
        DiskState::Unchanged(_) => {
            if remove_if_unchanged(&path, &rec.hash)? {
                prune_empty_parents(&path, &mv.dir);
                report.removed += 1;
                return Ok(());
            }
            // Saved at the last moment: it stays (and becomes a new note).
            None
        }
        DiskState::Modified(data, _) => Some(data),
    };
    // Edited here while trashed elsewhere: keep the edit in the (trashed)
    // note and leave the file in place, where the next scan re-creates it as
    // a new note.
    if let Some(data) = edited
        && writable
        && matches!(rec.kind, EntryKind::Note | EntryKind::Drawing)
        && v.entry(id).is_some()
        && let Ok(text) = String::from_utf8(data)
    {
        ingest_note(v, id, &rec, &text)?;
        touched.insert(id.to_string());
    }
    mv.warn_once(
        &format!("trashed:{id}"),
        "a file you edited locally was deleted or moved to the trash elsewhere; your copy is kept",
    );
    report.conflicts += 1;
    Ok(())
}

/// Write step b for one entry: follow a move or rename done elsewhere.
#[allow(clippy::too_many_arguments)]
fn apply_move(
    mv: &mut MirrorVault,
    v: &mut Vault,
    id: &str,
    target: &str,
    target_disk: &str,
    live_keys: &BTreeSet<String>,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let Some(rec) = mv.state.files.get(id) else {
        return Ok(());
    };
    if rec.path == target && rec.disk() == target_disk {
        return Ok(());
    }
    let mut rec = rec.clone();
    if rec.path == target {
        // Not moved, but its file is shared with another entry whose path
        // differs only in case (recorded before case-insensitivity was
        // handled): leave that file to the other entry and write this one
        // afresh under its own name.
        mv.state.files.remove(id);
        mv.dirty = true;
        return Ok(());
    }
    let old = disk_path(&mv.dir, rec.disk())?;
    let new = disk_path(&mv.dir, target_disk)?;
    // A rename that only changes case is the same file on a case-insensitive
    // file system: nothing is "in the way".
    if mv.key(rec.disk()) != mv.key(target_disk) {
        let recorded = mv
            .state
            .files
            .iter()
            .any(|(other, r)| other != id && mv.key(r.disk()) == mv.key(target_disk));
        match stat(&new)? {
            Node::Missing => {}
            Node::File(_) if !recorded => {
                // An unknown local file is in the way: keep it as a conflict copy.
                let copy = conflict_name(target_disk, |q| mv.taken(q, live_keys));
                std::fs::rename(&new, disk_path(&mv.dir, &copy)?)
                    .context("keeping a conflicting local file")?;
                ui::warn(&format!(
                    "vault '{}': a local file was in the way of a note moved elsewhere; it was kept as a \
                     conflict copy next to it",
                    v.name()
                ));
                report.conflicts += 1;
            }
            // Another synced file or a folder is in the way: try again later.
            _ => return Ok(()),
        }
    }
    match disk_state(&old, &mut rec)? {
        DiskState::Missing | DiskState::NotAFile => {
            // Written afresh at the new place by step c.
            mv.state.files.remove(id);
        }
        state => {
            if let DiskState::Modified(data, _) = state
                && writable
                && matches!(rec.kind, EntryKind::Note | EntryKind::Drawing)
                && let Ok(text) = String::from_utf8(data)
            {
                let hash = content_hash(text.as_bytes());
                let base = ingest_note(v, id, &rec, &text)?;
                push_base(&mut rec, base, &hash, true);
                rec.hash = hash;
                touched.insert(id.to_string());
            }
            if let Some(parent) = new.parent() {
                fsutil::create_private_dir(parent)?;
            }
            std::fs::rename(&old, &new).context("moving a mirrored file")?;
            prune_empty_parents(&old, &mv.dir);
            rec.path = target.to_string();
            rec.disk = (target_disk != target).then(|| target_disk.to_string());
            mv.state.files.insert(id.to_string(), rec);
            mv.record_stat(id, &new);
            report.renamed += 1;
        }
    }
    mv.dirty = true;
    Ok(())
}

/// Write step c for an explicit folder.
fn write_folder(mv: &mut MirrorVault, id: &str, p: &str) -> Result<()> {
    let path = disk_path(&mv.dir, p)?;
    if stat(&path)? == Node::Missing {
        // Deleted here: reading the disk takes it to the trash once what
        // was in it went.
        if mv.state.folders.get(id).map(String::as_str) == Some(p) {
            return Ok(());
        }
        fsutil::create_private_dir(&path)?;
    }
    if path.is_dir() && mv.state.folders.get(id).map(String::as_str) != Some(p) {
        mv.state.folders.insert(id.to_string(), p.to_string());
        mv.dirty = true;
    }
    Ok(())
}

/// Write step c for a note: bring the file up to date with the document,
/// merging a local edit first if there is one.
#[allow(clippy::too_many_arguments)]
fn write_note(
    mv: &mut MirrorVault,
    v: &mut Vault,
    id: &str,
    kind: EntryKind,
    p: &str,
    disk: &str,
    live_keys: &BTreeSet<String>,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let path = disk_path(&mv.dir, disk)?;
    let text = file_text(v, id, kind);
    let hash = content_hash(text.as_bytes());
    let Some(mut rec) = mv.state.files.get(id).cloned() else {
        // Not written yet. A file already there is adopted if identical,
        // otherwise kept as a conflict copy.
        let written_before = report.written;
        match stat(&path)? {
            Node::Other => {
                mv.warn_once(
                    &format!("notfile:{id}"),
                    &format!(
                        "vault '{}': a symlink, folder or special file is where a note would be written; the \
                         note is not mirrored",
                        v.name()
                    ),
                );
                return Ok(());
            }
            Node::File(_) => {
                let data = std::fs::read(&path)?;
                if content_hash(&data) != hash {
                    let copy = conflict_name(disk, |q| mv.taken(q, live_keys));
                    std::fs::rename(&path, disk_path(&mv.dir, &copy)?)
                        .context("keeping a conflicting local file")?;
                    ui::warn(&format!(
                        "vault '{}': a local file differed from the synced document at the same path; it was \
                         kept as a conflict copy next to it",
                        v.name()
                    ));
                    report.conflicts += 1;
                    if replace_if_unchanged(&path, text.as_bytes(), None)? != Replaced::Written {
                        return Ok(());
                    }
                    report.written += 1;
                }
            }
            Node::Missing => {
                if replace_if_unchanged(&path, text.as_bytes(), None)? != Replaced::Written {
                    return Ok(());
                }
                report.written += 1;
            }
        }
        // An identical file that was already there is adopted: read, not written.
        let adopted = report.written == written_before;
        let state = v
            .replica
            .notes
            .get(id)
            .map(|n| n.encode_state())
            .unwrap_or_default();
        let mut rec = FileRecord {
            path: p.to_string(),
            disk: (disk != p).then(|| disk.to_string()),
            kind,
            hash: hash.clone(),
            size: 0,
            mtime_ns: 0,
            checked_ns: 0,
            bases: vec![],
        };
        push_base(&mut rec, state, &hash, adopted);
        mv.state.files.insert(id.to_string(), rec);
        mv.record_stat(id, &path);
        return Ok(());
    };
    // A move that step b could not apply yet: the file at `disk` is not ours.
    if rec.path != p || rec.disk() != disk || rec.hash == hash {
        return Ok(());
    }
    let expected = match disk_state(&path, &mut rec)? {
        // Deleted locally; ingest() trashes it.
        DiskState::Missing | DiskState::NotAFile => return Ok(()),
        DiskState::Modified(data, st) => {
            if !writable {
                return Ok(());
            }
            let Ok(disk_text) = String::from_utf8(data) else {
                return Ok(());
            };
            let disk_hash = content_hash(disk_text.as_bytes());
            let base = ingest_note(v, id, &rec, &disk_text)?;
            push_base(&mut rec, base, &disk_hash, true);
            rec.hash = disk_hash;
            rec.set_stat(st);
            touched.insert(id.to_string());
            report.ingested += 1;
            st
        }
        DiskState::Unchanged(st) => st,
    };
    let text = file_text(v, id, kind);
    let hash = content_hash(text.as_bytes());
    let state = v
        .replica
        .notes
        .get(id)
        .map(|n| n.encode_state())
        .unwrap_or_default();
    if hash != rec.hash {
        match replace_if_unchanged(&path, text.as_bytes(), Some(expected))? {
            Replaced::Written => {}
            Replaced::Changed => {
                // Changed under us: the next pass ingests it.
                mv.state.files.insert(id.to_string(), rec);
                mv.dirty = true;
                return Ok(());
            }
            Replaced::Recovered(data) => {
                // An editor saved into the old file at the very last moment:
                // merge that save (written out by the next pass).
                if writable && let Ok(saved) = String::from_utf8(data) {
                    let saved_hash = content_hash(saved.as_bytes());
                    let base = ingest_note(v, id, &rec, &saved)?;
                    push_base(&mut rec, base, &saved_hash, true);
                    touched.insert(id.to_string());
                    report.ingested += 1;
                }
            }
        }
        rec.hash = hash.clone();
        report.written += 1;
    }
    push_base(&mut rec, state, &hash, false);
    mv.state.files.insert(id.to_string(), rec);
    mv.record_stat(id, &path);
    Ok(())
}

/// Write an attachment that was downloaded (outside the vault lock).
fn write_download(
    mv: &mut MirrorVault,
    id: &str,
    p: &str,
    disk: &str,
    data: &[u8],
    report: &mut Report,
) -> Result<()> {
    let path = disk_path(&mv.dir, disk)?;
    let hash = content_hash(data);
    match stat(&path)? {
        Node::Other => return Ok(()),
        Node::File(_) => {
            let current = std::fs::read(&path)?;
            if content_hash(&current) != hash {
                let copy = conflict_name(disk, |q| mv.taken(q, &BTreeSet::new()));
                std::fs::rename(&path, disk_path(&mv.dir, &copy)?)
                    .context("keeping a conflicting local file")?;
                report.conflicts += 1;
                if replace_if_unchanged(&path, data, None)? != Replaced::Written {
                    return Ok(());
                }
                report.written += 1;
            }
        }
        Node::Missing => {
            if replace_if_unchanged(&path, data, None)? != Replaced::Written {
                return Ok(());
            }
            report.written += 1;
        }
    }
    mv.state.files.insert(
        id.to_string(),
        FileRecord {
            path: p.to_string(),
            disk: (disk != p).then(|| disk.to_string()),
            kind: EntryKind::Attachment,
            hash,
            size: 0,
            mtime_ns: 0,
            checked_ns: 0,
            bases: vec![],
        },
    );
    mv.record_stat(id, &path);
    Ok(())
}

impl Mirror {
    /// Prepare the mirror for every vault open on the hub.
    pub async fn new(hub: Arc<Hub>, root: &Path) -> Result<Mirror> {
        Self::with_options(hub, root, MirrorOptions::default()).await
    }

    /// [`Mirror::new`] with options. A mirror directory that is missing
    /// although files were mirrored into it before (moved, renamed, an
    /// unmounted drive) is not re-created — every note would look deleted —
    /// unless `restore_missing` asks for a fresh copy.
    pub async fn with_options(hub: Arc<Hub>, root: &Path, opts: MirrorOptions) -> Result<Mirror> {
        if root.symlink_metadata().is_err() && !opts.restore_missing {
            let want = resolve_lenient(root);
            for v in hub.vaults() {
                if let Some(s) = stored_state(&v).await?
                    && s.root == want
                    && !s.files.is_empty()
                {
                    bail!(
                        "the mirror directory {} is missing, but notes were mirrored into it before (moved, \
                         renamed or not mounted?). Nothing was changed. Put it back and start again; to write a \
                         fresh copy there instead, add --restore-missing",
                        root.display()
                    );
                }
            }
        }
        fsutil::create_private_dir(root)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        // (Left over from writing the saved shapes' file.)
        fsutil::remove_stale_temp_files(&root, 1, STALE_TEMP_AGE);
        let fold = Fold {
            case: match opts.case_insensitive {
                Some(f) => f,
                None => is_case_insensitive(&root)
                    .with_context(|| format!("checking {}", root.display()))?,
            },
            unicode: match opts.unicode_insensitive {
                Some(f) => f,
                None => is_unicode_insensitive(&root)
                    .with_context(|| format!("checking {}", root.display()))?,
            },
        };
        tracing::debug!(?fold, "mirror: what the directory ignores in names");
        let mut m = Mirror {
            hub,
            root,
            vaults: BTreeMap::new(),
            warned: BTreeSet::new(),
            opts,
            fold,
        };
        m.attach_all().await;
        Ok(m)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn set_options(&mut self, opts: MirrorOptions) {
        self.opts = opts;
    }

    /// Directory of a vault's files.
    pub fn vault_dir(&self, vault_id: &str) -> Option<&Path> {
        self.vaults.get(vault_id).map(|m| m.dir.as_path())
    }

    /// Attach a vault; a vault that cannot be mirrored is reported (once) and
    /// skipped, it does not stop the others.
    /// Mirror every open vault, the quick notes first: the folder named
    /// after them is theirs.
    async fn attach_all(&mut self) {
        if let Some(q) = self.hub.quick_notes().await {
            let id = q.lock().await.id().to_string();
            self.attach_or_warn(&id).await;
        }
        for id in self.hub.vault_ids() {
            self.attach_or_warn(&id).await;
        }
    }

    async fn attach_or_warn(&mut self, vault_id: &str) {
        if let Err(e) = self.attach(vault_id).await
            && self.warned.insert(format!("attach:{vault_id}"))
        {
            ui::warn(&format!("mirror: vault {vault_id} is not mirrored: {e:#}"));
        }
    }

    async fn attach(&mut self, vault_id: &str) -> Result<()> {
        if self.vaults.contains_key(vault_id) {
            return Ok(());
        }
        let Some(v) = self.hub.get(vault_id) else {
            return Ok(());
        };
        if !self.root.is_dir() {
            bail!("the mirror directory is missing");
        }
        let stored = stored_state(&v).await?;
        let name = v.lock().await.name().to_string();
        // Vault folders must differ even ignoring case where the file system
        // does ("Notes" and "notes" would share one folder), and from the
        // saved shapes' file.
        let taken: BTreeSet<String> = self
            .vaults
            .values()
            .map(|m| fold_key(self.fold, &m.state.dir_name))
            .chain([fold_key(self.fold, SHAPES_FILE)])
            .collect();
        let state = match stored {
            Some(s)
                if s.root == self.root && !taken.contains(&fold_key(self.fold, &s.dir_name)) =>
            {
                s
            }
            _ => {
                let dir_name = self.fresh_dir_name(&name, vault_id, &taken)?;
                MirrorState {
                    root: self.root.clone(),
                    dir_name,
                    ..MirrorState::default()
                }
            }
        };
        let dir = disk_path(&self.root, &state.dir_name)?;
        // A vault folder that vanished is not re-created (that would make
        // every note look deleted): the passes hold and report it instead.
        if state.files.is_empty() || dir.is_dir() || self.opts.restore_missing {
            fsutil::create_private_dir(&dir)?;
            fsutil::remove_stale_temp_files(&dir, 64, STALE_TEMP_AGE);
            if vault_marker(&dir).as_deref() != Some(vault_id)
                && let Err(e) = fsutil::write_atomic(
                    &dir.join(VAULT_MARKER),
                    format!("{vault_id}\n").as_bytes(),
                    true,
                )
            {
                tracing::debug!("mirror: cannot write the vault marker: {e}");
            }
        }
        self.vaults.insert(
            vault_id.to_string(),
            MirrorVault {
                dir,
                state,
                dirty: true,
                missing_since: BTreeMap::new(),
                held: false,
                fold: self.fold,
                warned: BTreeSet::new(),
                renamed: Vec::new(),
            },
        );
        Ok(())
    }

    /// Folder name for a vault without mirror state (first sync, or the state
    /// was lost). A folder that already holds files not mirrored for *this*
    /// vault — left behind by a deleted or left vault with the same name, or
    /// someone else's files — is never adopted: its files would be imported
    /// into this vault. A suffixed name is used instead.
    fn fresh_dir_name(
        &self,
        name: &str,
        vault_id: &str,
        taken: &BTreeSet<String>,
    ) -> Result<String> {
        let base = sanitize_dir_name(name, vault_id);
        let short = &vault_id[..vault_id.len().min(6)];
        for n in 1..100 {
            let cand = match n {
                1 => base.clone(),
                2 => format!("{base} ({short})"),
                k => format!("{base} ({short}) {}", k - 1),
            };
            if taken.contains(&fold_key(self.fold, &cand)) {
                continue;
            }
            let d = disk_path(&self.root, &cand)?;
            let foreign =
                has_mirrorable_content(&d) && vault_marker(&d).as_deref() != Some(vault_id);
            if foreign {
                continue;
            }
            if n > 1 {
                ui::status(&format!(
                    "mirror: vault '{name}' goes into the folder '{cand}' ('{base}' already holds other files, \
                     which are left alone)"
                ));
            }
            return Ok(cand);
        }
        bail!("no free folder name for vault '{name}' in the mirror directory")
    }

    /// The mirror directory itself vanished while running (unmounted, moved):
    /// say so once and do nothing until it is back.
    fn root_missing(&mut self) -> bool {
        if self.root.is_dir() {
            if self.warned.remove("root-missing") {
                ui::status(&format!(
                    "mirror: {} is back; syncing files again",
                    self.root.display()
                ));
            }
            return false;
        }
        if self.warned.insert("root-missing".into()) {
            ui::warn(&format!(
                "the mirror directory {} disappeared (moved, renamed or unmounted?). Nothing is deleted or \
                 written while it is gone; put it back to resume",
                self.root.display()
            ));
        }
        true
    }

    async fn save_state(&mut self, vault_id: &str) -> Result<()> {
        let (Some(mv), Some(v)) = (self.vaults.get_mut(vault_id), self.hub.get(vault_id)) else {
            return Ok(());
        };
        if !mv.dirty {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&mv.state)?;
        v.lock().await.store().save_aux("mirror", &bytes)?;
        mv.dirty = false;
        Ok(())
    }

    /// Full two-way pass over every vault: ingest disk changes, write remote
    /// changes, then pick up anything created along the way (conflict
    /// copies). With `final_pass`, pending deletions are applied immediately.
    /// A vault that fails is reported and does not stop the others.
    pub async fn reconcile_all(&mut self, final_pass: bool) -> Result<Report> {
        let mut total = Report::default();
        self.attach_all().await;
        let ids: Vec<String> = self.vaults.keys().cloned().collect();
        for id in ids {
            match self.reconcile_vault(&id, final_pass).await {
                Ok(r) => total.add(r),
                Err(e) => ui::warn(&format!("mirror: {e:#}")),
            }
        }
        Ok(total)
    }

    async fn reconcile_vault(&mut self, id: &str, final_pass: bool) -> Result<Report> {
        let mut r = self.ingest(id, false).await?;
        r.add(self.write_remote(id, &Scope::All).await?);
        let second = self.ingest(id, final_pass).await?;
        r.add(second);
        r.held = second.held;
        r.add(self.write_remote(id, &Scope::All).await?);
        self.save_state(id).await?;
        Ok(r)
    }

    /// Disk → CRDT for one vault.
    pub async fn ingest(&mut self, vault_id: &str, final_pass: bool) -> Result<Report> {
        let mut report = Report::default();
        if self.root_missing() {
            return Ok(report);
        }
        let Some(shared) = self.hub.get(vault_id) else {
            return Ok(report);
        };
        let opts = self.opts;
        let root = self.root.clone();
        // Never ask the network while holding the vault lock.
        let max_attachment = self.hub.limits().await.max_attachment_bytes;
        let Some(mv) = self.vaults.get_mut(vault_id) else {
            return Ok(report);
        };
        let (disk, empty_dirs) = scan_dir(&mv.dir);
        let mut v = shared.lock().await;
        let name = v.name().to_string();
        let writable = !self.hub.read_only && v.require_writable().is_ok();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let live = v.paths();
        // Disk paths that belong to a recorded file, or to a live entry not
        // written yet (write_remote adopts or resolves what is there).
        let assigned = assign_disk_paths(&mirrored_entries(&v), &mv.state.files, mv.fold);
        let claimed: BTreeSet<String> = mv
            .state
            .files
            .values()
            .map(|r| mv.key(r.disk()))
            .chain(assigned.values().map(|d| mv.key(d)))
            .collect();

        // 1. Recorded files: modified or missing?
        let ids: Vec<String> = mv.state.files.keys().cloned().collect();
        for id in &ids {
            if let Err(e) = ingest_record(mv, &mut v, id, writable, &mut touched, &mut report) {
                mv.entry_failed(&name, id, &e);
            }
        }

        // 2. New files: renames (same content as a missing file) or creations.
        let mut new_files: Vec<(String, Stat)> = disk
            .iter()
            .filter(|(p, _)| !claimed.contains(&mv.key(p)))
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        // Notes first: a file in the quick notes needs a note linking to it.
        new_files.sort_by_key(|(p, _)| !paths::is_note(p));
        if writable {
            for (path, st) in new_files {
                if let Err(e) = ingest_new_file(
                    mv,
                    &mut v,
                    &path,
                    st,
                    max_attachment,
                    &mut touched,
                    &mut report,
                ) {
                    mv.entry_failed(&name, &path, &e);
                }
            }
        }

        // 3. Missing long enough (or final pass): move to the trash — unless
        // so many vanished at once that it looks like a moved or unmounted
        // folder rather than deletions (never applied without confirmation,
        // not even in a final pass).
        let total = mv.state.files.len();
        let missing = mv.missing_since.len();
        let dir_gone = total > 0 && !mv.dir.is_dir();
        let quick = v.is_quick_notes();
        let many = if quick {
            missing > QUICK_MASS_DELETE_MIN_FILES
        } else {
            missing > MASS_DELETE_MIN_FILES && missing * 2 > total
        };
        let mass = missing > 0 && (dir_gone || many);
        let mut expired: Vec<String> = mv
            .missing_since
            .iter()
            .filter(|(_, t)| final_pass || t.elapsed() >= RENAME_WINDOW)
            .map(|(id, _)| id.clone())
            .collect();
        if opts.restore_missing && missing > 0 {
            // Forget them all: write_remote writes them again.
            for id in std::mem::take(&mut mv.missing_since).into_keys() {
                mv.state.files.remove(&id);
                report.restored += 1;
            }
            mv.dirty = true;
            mv.held = false;
            expired.clear();
        } else if mass && !opts.allow_mass_delete {
            if !mv.held {
                let what = if dir_gone {
                    format!("its folder {} is gone", mv.dir.display())
                } else {
                    format!("{missing} of {total} mirrored files disappeared at once")
                };
                let (nothing, gone) = if quick {
                    ("Nothing was deleted", "deletes those quick notes for good")
                } else {
                    ("Nothing was moved to the trash", "moves them to the trash")
                };
                ui::warn(&format!(
                    "vault '{name}': {what}. {nothing}. If the folder was moved, renamed or \
                     unmounted, put it back. If you deleted the files on purpose, stop `notekpr sync` and run \
                     `notekpr sync --once --allow-mass-delete` ({gone}), or \
                     `--restore-missing` to get the files back"
                ));
            }
            mv.held = true;
            report.held = missing;
            expired.clear();
        } else if mv.held {
            mv.held = false;
            ui::status(&format!(
                "vault '{name}': the missing files are back; deletions are synced again"
            ));
        }
        for id in expired {
            mv.missing_since.remove(&id);
            if writable && v.entry(&id).is_some_and(|e| e.is_live()) {
                if let Err(e) = v.trash_id(&id) {
                    mv.entry_failed(&name, &id, &e);
                    continue;
                }
                report.trashed += 1;
                touched.insert(id.clone());
            }
            // Read-only vaults: forgetting the record makes write_remote restore the file.
            mv.state.files.remove(&id);
            mv.dirty = true;
        }

        // 4. Explicit folders: new empty directories, removed folder entries.
        if writable && !mv.held && mv.dir.is_dir() {
            for d in &empty_dirs {
                let dk = mv.key(d);
                let known = mv.state.folders.values().any(|p| mv.key(p) == dk)
                    || live.keys().any(|p| {
                        let pk = mv.key(p);
                        pk == dk || paths::is_within(&pk, &dk)
                    });
                if !known && let Ok(id) = v.create_folder(d) {
                    mv.state.folders.insert(id.clone(), d.clone());
                    mv.dirty = true;
                    touched.insert(id);
                    report.created += 1;
                }
            }
            let gone: Vec<(String, String)> = mv
                .state
                .folders
                .iter()
                .filter(|(_, p)| disk_path(&mv.dir, p).is_ok_and(|d| !d.is_dir()))
                .map(|(id, p)| (id.clone(), p.clone()))
                .collect();
            for (id, p) in gone {
                // What's in it goes to the trash first (or comes back): until
                // then the folder stays, and isn't made again on disk.
                if !opts.restore_missing && v.paths().keys().any(|q| paths::is_within(q, &p)) {
                    continue;
                }
                mv.state.folders.remove(&id);
                mv.dirty = true;
                // Restoring: written again.
                if opts.restore_missing {
                    continue;
                }
                if v.entry(&id).is_some_and(|e| e.is_live()) {
                    // With what's left in it that a newer release added (not
                    // on disk: this one doesn't know its kind).
                    if let Err(e) = v.trash_path(&p) {
                        mv.entry_failed(&name, &id, &e);
                        continue;
                    }
                    report.trashed += 1;
                    touched.insert(id);
                }
            }
        }

        // 5. With the Personal storage: the saved shapes' file, edited here.
        let mut shapes_changed = false;
        if v.is_quick_notes() {
            match sync_shapes(mv, &root, &mut v, writable, false) {
                Ok(done) => {
                    shapes_changed = done.ingested;
                    report.ingested += usize::from(done.ingested);
                }
                Err(e) => mv.entry_failed(&name, SHAPES_FILE, &e),
            }
        }

        let changed = !touched.is_empty() || shapes_changed;
        if changed {
            v.save()?;
        }
        let quick = v.is_quick_notes();
        drop(v);
        let renamed = std::mem::take(&mut mv.renamed);
        if changed {
            self.hub
                .local_changed(vault_id, touched.into_iter().collect(), true)
                .await;
        }
        // The quick notes keep no trash: what was deleted goes for good.
        if quick
            && report.trashed > 0
            && let Err(e) = crate::ops::purge_deleted_quick_notes(&self.hub).await
        {
            ui::warn(&format!("quick notes: {e:#}"));
        }
        for id in renamed {
            if let Err(e) = crate::note_shares::refresh_meta(&self.hub.session, &shared, &id).await
            {
                ui::warn(&format!("{e:#}"));
            }
        }
        if report.local_changes() > 0 {
            ui::status(&format!(
                "vault '{name}': {} local edit(s), {} new, {} moved, {} {}",
                report.ingested,
                report.created,
                report.renamed,
                report.trashed,
                if quick { "deleted" } else { "trashed" }
            ));
        }
        Ok(report)
    }

    /// CRDT → disk for one vault.
    pub async fn write_remote(&mut self, vault_id: &str, scope: &Scope) -> Result<Report> {
        let mut report = Report::default();
        if self.root_missing() {
            return Ok(report);
        }
        let Some(shared) = self.hub.get(vault_id) else {
            return Ok(report);
        };
        let restore = self.opts.restore_missing;
        let root = self.root.clone();
        let Some(mv) = self.vaults.get_mut(vault_id) else {
            return Ok(report);
        };
        if !mv.dir.is_dir() {
            // A vault folder that vanished is not re-created behind the
            // user's back (ingest holds its deletions and says why).
            if !mv.state.files.is_empty() && !restore {
                return Ok(report);
            }
            fsutil::create_private_dir(&mv.dir)?;
        }
        let mut downloads: Vec<(String, String, String)> = Vec::new();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let name;
        let mut shapes_changed = false;
        {
            let mut v = shared.lock().await;
            if v.read_only.is_some() {
                return Ok(report);
            }
            name = v.name().to_string();
            let writable = !self.hub.read_only && v.require_writable().is_ok();
            let mut live: BTreeMap<String, (String, notekpr_core::doc::Entry)> = BTreeMap::new();
            for (p, e) in v.paths() {
                if !is_safe_vault_path(&p) {
                    mv.warn_once(
                        &format!("unsafe:{}", e.id),
                        &format!(
                            "vault '{name}' contains an entry with a hidden or unsupported path; it is not mirrored"
                        ),
                    );
                    continue;
                }
                live.insert(e.id.clone(), (p, e));
            }
            let live_keys: BTreeSet<String> = live.values().map(|(p, _)| mv.key(p)).collect();

            // a. Entries that are gone (trashed, removed) or became folders.
            let gone: Vec<String> = mv
                .state
                .files
                .keys()
                .filter(|id| {
                    live.get(*id)
                        .is_none_or(|(_, e)| e.kind == EntryKind::Folder)
                })
                .cloned()
                .collect();
            for id in gone {
                if let Err(e) = remove_gone(mv, &mut v, &id, writable, &mut touched, &mut report) {
                    mv.entry_failed(&name, &id, &e);
                }
            }

            // Where each entry lives on disk (distinct even ignoring case).
            let assigned = assign_disk_paths(&live, &mv.state.files, mv.fold);
            let disk_of = |id: &str, p: &String| assigned.get(id).unwrap_or(p).clone();

            // b. Moves/renames done elsewhere.
            for (id, (p, _)) in &live {
                if let Err(e) = apply_move(
                    mv,
                    &mut v,
                    id,
                    p,
                    &disk_of(id, p),
                    &live_keys,
                    writable,
                    &mut touched,
                    &mut report,
                ) {
                    mv.entry_failed(&name, id, &e);
                }
            }

            // c. Content.
            for (id, (p, e)) in &live {
                let in_scope = match scope {
                    Scope::All => true,
                    Scope::Objects(set) => set.contains(id) || !mv.state.files.contains_key(id),
                };
                if !in_scope {
                    continue;
                }
                let res = match e.kind {
                    EntryKind::Folder => write_folder(mv, id, p),
                    EntryKind::Attachment => {
                        if !mv.state.files.contains_key(id) {
                            downloads.push((id.clone(), p.clone(), disk_of(id, p)));
                        }
                        Ok(())
                    }
                    EntryKind::Note | EntryKind::Drawing => write_note(
                        mv,
                        &mut v,
                        id,
                        e.kind,
                        p,
                        &disk_of(id, p),
                        &live_keys,
                        writable,
                        &mut touched,
                        &mut report,
                    ),
                };
                if let Err(err) = res {
                    mv.entry_failed(&name, id, &err);
                }
            }

            // d. Folder entries removed elsewhere: drop their directory if empty.
            let live_folders: BTreeSet<&String> = live
                .iter()
                .filter(|(_, (_, e))| e.kind == EntryKind::Folder)
                .map(|(id, _)| id)
                .collect();
            let stale: Vec<(String, String)> = mv
                .state
                .folders
                .iter()
                .filter(|(id, _)| !live_folders.contains(id))
                .map(|(i, p)| (i.clone(), p.clone()))
                .collect();
            for (id, p) in stale {
                mv.state.folders.remove(&id);
                mv.dirty = true;
                if let Ok(d) = disk_path(&mv.dir, &p)
                    && std::fs::remove_dir(&d).is_ok()
                {
                    prune_empty_parents(&d, &mv.dir);
                    report.removed += 1;
                }
            }

            // e. With the Personal storage: the saved shapes' file.
            if v.is_quick_notes() {
                match sync_shapes(mv, &root, &mut v, writable, true) {
                    Ok(done) => {
                        shapes_changed = done.ingested;
                        report.ingested += usize::from(done.ingested);
                        report.written += usize::from(done.written);
                    }
                    Err(e) => mv.entry_failed(&name, SHAPES_FILE, &e),
                }
            }
            if !touched.is_empty() || shapes_changed {
                v.save()?;
            }
        }

        // Attachments are downloaded without holding the vault lock.
        for (id, p, disk) in downloads {
            let data = match self.hub.session.attachment(&shared, &id).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!("attachment not available yet: {e:#}");
                    continue;
                }
            };
            if let Err(e) = write_download(mv, &id, &p, &disk, &data, &mut report) {
                mv.entry_failed(&name, &id, &e);
            }
        }

        if !touched.is_empty() || shapes_changed {
            self.hub
                .local_changed(vault_id, touched.into_iter().collect(), true)
                .await;
        }
        let written = report.written + report.removed + report.renamed;
        if written > 0 {
            ui::status(&format!(
                "vault '{name}': {} file(s) updated, {} moved, {} removed",
                report.written, report.renamed, report.removed
            ));
        }
        Ok(report)
    }

    /// Start (or restart) watching the mirror directory. On failure (the
    /// inotify watch limit, an unsupported file system, the directory is
    /// gone) the mirror keeps working by scanning every few seconds.
    fn watch_root(&mut self, watcher: &mut Option<notify::RecommendedWatcher>) -> bool {
        use notify::{RecursiveMode, Watcher};
        let Some(w) = watcher.as_mut() else {
            return false;
        };
        if !self.root.is_dir() {
            return false;
        }
        let _ = w.unwatch(&self.root);
        match w.watch(&self.root, RecursiveMode::Recursive) {
            Ok(()) => {
                if self.warned.remove("watch") {
                    ui::status("mirror: watching the directory for changes again");
                }
                true
            }
            Err(e) => {
                self.watch_failed(&e.to_string());
                false
            }
        }
    }

    fn watch_failed(&mut self, e: &str) {
        if self.warned.insert("watch".into()) {
            ui::warn(&format!(
                "mirror: cannot watch {} for changes ({e}); scanning it every {}s instead. On Linux, raising \
                 fs.inotify.max_user_watches usually helps",
                self.root.display(),
                POLL_EVERY.as_secs()
            ));
        }
    }

    async fn local_pass(&mut self) {
        for id in self.vaults.keys().cloned().collect::<Vec<_>>() {
            if let Err(e) = self.ingest(&id, false).await {
                ui::warn(&format!("mirror: {e:#}"));
            }
            if let Err(e) = self
                .write_remote(&id, &Scope::Objects(BTreeSet::new()))
                .await
            {
                ui::warn(&format!("mirror: {e:#}"));
            }
            if let Err(e) = self.save_state(&id).await {
                ui::warn(&format!("mirror: saving the mirror state: {e:#}"));
            }
        }
    }

    /// Watch the directory and the hub until `shutdown`.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let (tx, mut fs_rx) = mpsc::unbounded_channel::<std::result::Result<(), String>>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    let relevant = ev.paths.iter().any(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| !is_ignored_name(n))
                    });
                    if relevant && !matches!(ev.kind, notify::EventKind::Access(_)) {
                        let _ = tx.send(Ok(()));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                }
            }) {
                Ok(w) => Some(w),
                Err(e) => {
                    self.watch_failed(&e.to_string());
                    None
                }
            };
        let mut watching = self.watch_root(&mut watcher);
        let mut watched = dir_ident(&self.root);
        let mut events = self.hub.subscribe();
        // Close the gap between the caller's initial pass and the watch starting.
        if let Err(e) = self.reconcile_all(false).await {
            ui::warn(&format!("mirror: {e:#}"));
        }
        let mut full = tokio::time::interval(FULL_SCAN_EVERY);
        full.tick().await;
        let mut poll = tokio::time::interval(POLL_EVERY);
        poll.tick().await;
        let mut pending_remote: BTreeMap<String, Scope> = BTreeMap::new();
        let mut remote_deadline: Option<tokio::time::Instant> = None;
        let mut deletion_check = tokio::time::interval(RENAME_WINDOW);
        loop {
            let remote_sleep = async {
                match remote_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(change) = fs_rx.recv() => {
                    if let Err(e) = change {
                        // Events may be missing from now on: scan instead.
                        self.watch_failed(&e);
                        watching = false;
                        continue;
                    }
                    tracing::debug!("mirror: file system change");
                    tokio::time::sleep(DEBOUNCE).await;
                    while let Ok(Ok(())) = fs_rx.try_recv() {}
                    self.local_pass().await;
                }
                ev = events.recv() => match ev {
                    Ok(HubEvent::Changed { vault_id, objects, index_changed, purged, .. }) => {
                        let entry = pending_remote.entry(vault_id).or_insert_with(|| Scope::Objects(BTreeSet::new()));
                        if index_changed || !purged.is_empty() {
                            *entry = Scope::All;
                        } else if let Scope::Objects(set) = entry {
                            set.extend(objects);
                        }
                        remote_deadline.get_or_insert_with(|| tokio::time::Instant::now() + REMOTE_DEBOUNCE);
                    }
                    Ok(HubEvent::VaultAdded { vault_id }) => {
                        self.attach_or_warn(&vault_id).await;
                        pending_remote.insert(vault_id, Scope::All);
                        remote_deadline.get_or_insert_with(|| tokio::time::Instant::now() + REMOTE_DEBOUNCE);
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        for id in self.vaults.keys() {
                            pending_remote.insert(id.clone(), Scope::All);
                        }
                        remote_deadline.get_or_insert_with(tokio::time::Instant::now);
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                _ = remote_sleep => {
                    remote_deadline = None;
                    for (id, scope) in std::mem::take(&mut pending_remote) {
                        if let Err(e) = self.write_remote(&id, &scope).await {
                            ui::warn(&format!("mirror: {e:#}"));
                        }
                        if let Err(e) = self.save_state(&id).await {
                            ui::warn(&format!("mirror: saving the mirror state: {e:#}"));
                        }
                    }
                }
                _ = deletion_check.tick() => {
                    // Held vaults (mass disappearance) are re-checked by the full scan.
                    let has_missing = self.vaults.values().any(|m| !m.held && !m.missing_since.is_empty());
                    if has_missing {
                        for id in self.vaults.keys().cloned().collect::<Vec<_>>() {
                            if let Err(e) = self.ingest(&id, false).await {
                                ui::warn(&format!("mirror: {e:#}"));
                            }
                            let _ = self.save_state(&id).await;
                        }
                    }
                }
                _ = poll.tick() => {
                    if !watching {
                        watching = self.watch_root(&mut watcher);
                        watched = dir_ident(&self.root);
                        self.local_pass().await;
                    }
                }
                _ = full.tick() => {
                    // The directory may have been removed and re-created
                    // (remounted): its old watch is gone.
                    let ident = dir_ident(&self.root);
                    if ident.is_none() {
                        watching = false;
                    } else if !watching || ident != watched {
                        watching = self.watch_root(&mut watcher);
                        watched = ident;
                    }
                    if let Err(e) = self.reconcile_all(false).await {
                        ui::warn(&format!("mirror: {e:#}"));
                    }
                }
                _ = shutdown.changed() => {
                    for id in self.vaults.keys().cloned().collect::<Vec<_>>() {
                        let _ = self.save_state(&id).await;
                    }
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_rules() {
        for n in [
            ".git",
            ".obsidian",
            ".trash",
            ".#note.md",
            "note.md~",
            "note.md.swp",
            ".note.md.swp",
            "4913",
            "#x#",
            "~$doc.docx",
            ".x.cpn-tmp-abc",
        ] {
            assert!(is_ignored_name(n), "{n}");
        }
        for n in ["note.md", "image.png", "Ideas", "4914", "a~b.md"] {
            assert!(!is_ignored_name(n), "{n}");
        }
    }

    #[test]
    fn path_safety_and_dir_names() {
        assert!(is_safe_vault_path("Ideas/First.md"));
        assert!(!is_safe_vault_path(".git/hooks/post-commit"));
        assert!(!is_safe_vault_path("a/.hidden.md"));
        assert!(!is_safe_vault_path("a//b.md"));
        assert!(!is_safe_vault_path("../x.md"));
        assert!(!is_safe_vault_path("/abs.md"));
        assert_eq!(sanitize_dir_name("Family / Home", "id"), "Family _ Home");
        assert_eq!(sanitize_dir_name("..", "id"), "id");
        assert_eq!(sanitize_dir_name("  .hidden ", "id"), "hidden");
        assert_eq!(sanitize_dir_name("a:b*c?", "id"), "a_b_c_");
        assert_eq!(sanitize_dir_name("tab\there", "id"), "tab_here");
        assert_eq!(sanitize_dir_name("", "fallback"), "fallback");
        assert_eq!(sanitize_dir_name("con", "id"), "_con");
        let d = disk_path(Path::new("/m/v"), "a/b.md").unwrap();
        assert_eq!(d, Path::new("/m/v/a/b.md"));
        for bad in ["", "a//b", "..", "a/../b", "/abs", "a/./b"] {
            assert!(disk_path(Path::new("/m/v"), bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn windows_path_rules() {
        // Fine on Unix, rejected for Windows: drive prefixes and streams,
        // reserved characters, device names, trailing dots and spaces.
        for p in [
            "C:/x.md",
            "C:",
            "a/C:evil.md",
            "notes/x.md:stream",
            "a/b?.md",
            "a/<b>.md",
            "a/b|c.md",
            "a/\"q\".md",
            "a/b*.md",
            "CON",
            "con.md",
            "a/NUL.txt",
            "a/com1.md",
            "LPT9.md",
            "a/COM\u{b9}.md",
            "dir./x.md",
            "a/x.md.",
        ] {
            assert!(is_safe_vault_path_for(p, false), "{p}");
            assert!(!is_safe_vault_path_for(p, true), "{p}");
        }
        for p in [
            "Notes/Meeting 10-30.md",
            "a/console.md",
            "a/COM10.md",
            "a/CONFIG.md",
            "ä/ö ü.md",
        ] {
            assert!(is_safe_vault_path_for(p, true), "{p}");
        }
        // Path separators of any platform never survive normalisation.
        assert!(!is_safe_vault_path_for("a\\..\\..\\x.md", false));
        assert!(!is_safe_vault_path_for("a\\b.md", true));
    }

    fn st(p: &Path) -> Option<Stat> {
        match stat(p).unwrap() {
            Node::File(s) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn replace_refuses_when_the_file_changed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("n.md");
        std::fs::write(&p, "v1").unwrap();
        let seen = st(&p);
        // Changed after it was observed (an editor saved): left alone.
        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&p, "v1 + local").unwrap();
        assert_eq!(
            replace_if_unchanged(&p, b"remote", seen).unwrap(),
            Replaced::Changed
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v1 + local");
        // Changed right before the rename: also left alone.
        let seen = st(&p);
        let r = replace_with_hook(&p, b"remote", seen, || {}).unwrap();
        assert_eq!(r, Replaced::Written);
        let seen = st(&p);
        let p2 = p.clone();
        let mut fired = false;
        let r = fsutil::write_atomic_if(&p, b"x", true, || {
            std::fs::write(&p2, "editor").unwrap();
            fired = true;
            Ok(st(&p2) == seen)
        })
        .unwrap();
        assert!(fired && !r);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "editor");
        // Must not exist: refuses when something appeared.
        assert_eq!(
            replace_if_unchanged(&p, b"x", None).unwrap(),
            Replaced::Changed
        );
        // Only temporary-file-free results.
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn an_in_place_save_racing_the_rename_is_recovered() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("n.md");
        std::fs::write(&p, "v1").unwrap();
        let seen = st(&p);
        let p2 = p.clone();
        // The editor writes into the old file after our last check.
        let r = replace_with_hook(&p, b"remote", seen, move || {
            use std::io::Write;
            std::thread::sleep(Duration::from_millis(5));
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&p2)
                .unwrap();
            f.write_all(b"v1 + saved at the last moment").unwrap();
        })
        .unwrap();
        assert_eq!(
            r,
            Replaced::Recovered(b"v1 + saved at the last moment".to_vec())
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "remote");
    }

    #[test]
    fn removal_keeps_a_file_saved_at_the_last_moment() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("n.md");
        std::fs::write(&p, "v1").unwrap();
        assert!(remove_if_unchanged(&p, &content_hash(b"v1")).unwrap());
        assert!(!p.exists());
        std::fs::write(&p, "v2").unwrap();
        assert!(!remove_if_unchanged(&p, &content_hash(b"v1")).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn base_selection_prefers_what_the_editor_loaded() {
        let v1 = "line one\nline two\n".to_string();
        let v2 = "line one\nline two\nremote addition\n".to_string();
        // The editor loaded v1 and appended a line; v1 is the closest base.
        let disk = "line one\nline two\nlocal addition\n";
        assert_eq!(choose_base(&[v1.clone(), v2.clone()], disk), Some(0));
        // The editor reloaded v2 and edited it.
        let disk = "line one\nline two\nremote addition\nmore\n";
        assert_eq!(choose_base(&[v1.clone(), v2.clone()], disk), Some(1));
        // A remote edit of another line.
        let v3 = "line ONE\nline two\n".to_string();
        assert_eq!(
            choose_base(&[v1.clone(), v3.clone()], "line one\nline 2\n"),
            Some(0)
        );
        assert_eq!(
            choose_base(&[v1.clone(), v3.clone()], "line ONE\nline 2\n"),
            Some(1)
        );
        // Ties go to the newest.
        assert_eq!(choose_base(&[v1.clone(), v1.clone()], "x"), Some(1));
        assert_eq!(choose_base(&[], "x"), None);
    }

    #[test]
    fn versions_before_the_last_save_are_not_starting_points() {
        let b = |h: &str, read| Base {
            state: vec![],
            hash: h.into(),
            read,
        };
        let hashes = |bs: &[Base]| bs.iter().map(|b| b.hash.clone()).collect::<Vec<_>>();
        // Only our writes: the editor may hold any of them.
        assert_eq!(
            hashes(candidate_bases(&[b("w1", false), b("w2", false)])),
            ["w1", "w2"]
        );
        // It saved r2: w1 is history (a file equal to it is a revert), w3 may
        // have been reloaded.
        let bases = [b("w1", false), b("r2", true), b("w3", false)];
        assert_eq!(hashes(candidate_bases(&bases)), ["r2", "w3"]);
        // The same text read and written keeps the "read" mark.
        let mut rec = FileRecord {
            path: "a.md".into(),
            disk: None,
            kind: EntryKind::Note,
            hash: String::new(),
            size: 0,
            mtime_ns: 0,
            checked_ns: 0,
            bases: vec![],
        };
        push_base(&mut rec, vec![], "h", true);
        push_base(&mut rec, vec![], "h", false);
        assert_eq!(rec.bases.len(), 1);
        assert!(rec.bases[0].read);
    }

    fn entry(id: &str, path: &str) -> (String, (String, notekpr_core::doc::Entry)) {
        let e = notekpr_core::doc::Entry {
            id: id.into(),
            path: path.into(),
            kind: EntryKind::Note,
            created: 0,
            modified: 0,
            size: None,
            mime: None,
            trashed: None,
            purged: None,
            icon: None,
            order: None,
        };
        (id.to_string(), (path.to_string(), e))
    }

    #[test]
    fn case_variants_get_distinct_files_where_case_is_ignored() {
        let live: BTreeMap<_, _> = [
            entry("b", "idea.md"),
            entry("a", "Idea.md"),
            entry("c", "idea 2.md"),
            entry("d", "Other.md"),
        ]
        .into_iter()
        .collect();
        let none = BTreeMap::new();
        // Case-sensitive: everything at its own path.
        let plain = assign_disk_paths(&live, &none, Fold::default());
        assert!(plain.iter().all(|(id, d)| *d == live[id].0), "{plain:?}");
        // Case-insensitive: "Idea.md" keeps the name, "idea.md" moves aside
        // without taking the real "idea 2.md".
        let folded = assign_disk_paths(&live, &none, CASE);
        assert_eq!(folded["a"], "Idea.md");
        assert_eq!(folded["c"], "idea 2.md");
        assert_eq!(folded["b"], "idea 3.md");
        assert_eq!(folded["d"], "Other.md");
        let keys: BTreeSet<String> = folded.values().map(|d| d.to_lowercase()).collect();
        assert_eq!(keys.len(), 4);
        // A file already on disk stays where it is.
        let mut files = BTreeMap::new();
        files.insert(
            "b".to_string(),
            FileRecord {
                path: "idea.md".into(),
                disk: None,
                kind: EntryKind::Note,
                hash: String::new(),
                size: 0,
                mtime_ns: 0,
                checked_ns: 0,
                bases: vec![],
            },
        );
        let kept = assign_disk_paths(&live, &files, CASE);
        assert_eq!(kept["b"], "idea.md");
        assert_eq!(kept["a"], "Idea 3.md");
    }

    /// A case-insensitive file system (Windows; macOS ignores accents too).
    const CASE: Fold = Fold {
        case: true,
        unicode: false,
    };

    #[test]
    fn names_compare_as_the_file_system_does() {
        let (decomposed, composed) = ("Notes/Cafe\u{301}.md", "Notes/Caf\u{e9}.md");
        let mac = Fold {
            case: true,
            unicode: true,
        };
        assert_eq!(
            fold_key(mac, decomposed),
            fold_key(mac, "notes/caf\u{e9}.md")
        );
        // Elsewhere both are real, different names.
        assert_ne!(
            fold_key(Fold::default(), decomposed),
            fold_key(Fold::default(), composed)
        );
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("x\u{e9}"), "").unwrap();
        let expected = d.path().join("xe\u{301}").exists();
        assert_eq!(is_unicode_insensitive(d.path()).unwrap(), expected);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn case_probe_matches_the_file_system() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("probe"), "").unwrap();
        let expected = d.path().join("PROBE").exists();
        assert_eq!(is_case_insensitive(d.path()).unwrap(), expected);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn stale_editor_save_preserves_remote_edit() {
        // Live document, then a remote edit lands after the file was written.
        let base_doc = NoteDoc::new();
        base_doc.set_text("alpha\nbeta\n");
        let base_state = base_doc.encode_state();
        let live = NoteDoc::from_updates([base_state.as_slice()]).unwrap();
        let remote = NoteDoc::from_updates([base_state.as_slice()]).unwrap();
        let u = remote.set_text("alpha\nbeta\ngamma (remote)\n").unwrap();
        live.apply_update(&u).unwrap();
        // The editor saves its stale buffer with a local change.
        let disk = "alpha (local)\nbeta\n";
        let bases = vec![base_doc.text(), live.text()];
        let i = choose_base(&bases, disk).unwrap();
        assert_eq!(i, 0);
        let fork = fork_note(&base_state).unwrap();
        let up = fork
            .apply_edits(&textdiff::diff(&fork.text(), disk))
            .unwrap();
        live.apply_update(&up).unwrap();
        assert_eq!(live.text(), "alpha (local)\nbeta\ngamma (remote)\n");
    }

    #[test]
    fn conflict_names_are_unique() {
        let n = conflict_name("Ideas/Note.md", |_| false);
        assert!(n.starts_with("Ideas/Note (conflict "));
        assert!(n.ends_with(").md"));
    }
}

/// Property tests for the path rules that keep mirrored files inside their
/// folder and apart from each other [C-H6, C-H7].
#[cfg(test)]
mod path_properties {
    use super::*;
    use notekpr_core::doc::Entry;
    use proptest::prelude::*;

    const ALPHABET: &[char] = &[
        'a', 'B', 'n', 'N', '1', ' ', '.', '.', '/', '/', '\\', ':', '*', '?', '"', '<', '>', '|',
        '-', '_', '~', '\0', '\t', 'é', 'É', 'ß', 'İ', '€', '😀', '\u{202e}',
    ];
    const WORDS: &[&str] = &[
        "con", "CON", "nul.md", "Com1", "lpt9.txt", "..", ".", "a", "A", "Notes", "notes",
    ];

    fn text(max: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                4 => prop::sample::select(ALPHABET).prop_map(|c| c.to_string()),
                1 => prop::sample::select(WORDS).prop_map(str::to_string),
            ],
            0..max,
        )
        .prop_map(|v| v.concat())
    }

    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512)
    }

    fn entry(id: &str, path: &str) -> (String, (String, Entry)) {
        let e = Entry {
            id: id.into(),
            path: path.into(),
            kind: EntryKind::Note,
            created: 0,
            modified: 0,
            trashed: None,
            purged: None,
            size: None,
            mime: None,
            icon: None,
            order: None,
        };
        (id.to_string(), (path.to_string(), e))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// A path accepted for mirroring maps below the mirror folder, one
        /// plain component per segment, also under Windows' rules.
        #[test]
        fn safe_paths_stay_inside(p in text(40)) {
            let base = Path::new("/mirror/Vault");
            for windows in [false, true] {
                if !is_safe_vault_path_for(&p, windows) {
                    continue;
                }
                let d = disk_path(base, &p).unwrap();
                prop_assert!(d.starts_with(base) && d != base);
                prop_assert_eq!(
                    d.strip_prefix(base).unwrap().components().count(),
                    p.split('/').count()
                );
                prop_assert!(p.split('/').all(|s| s != ".." && s != "." && !s.is_empty()));
                if windows {
                    prop_assert!(p.split('/').all(is_windows_safe_name));
                }
            }
        }

        /// Whatever disk_path accepts stays below its directory.
        #[test]
        fn disk_paths_never_escape(p in text(40)) {
            let base = Path::new("/mirror/Vault");
            if let Ok(d) = disk_path(base, &p) {
                prop_assert!(d.starts_with(base) && d != base);
                prop_assert!(!d.components().any(|c| matches!(c, std::path::Component::ParentDir)));
            }
        }

        /// Vault folder names are always usable on every common file system.
        #[test]
        fn vault_folder_names_are_safe(name in text(60)) {
            let n = sanitize_dir_name(&name, "Vault");
            prop_assert!(is_windows_safe_name(&n), "{n:?}");
            prop_assert!(!n.starts_with('.') && n.len() <= 200 && !n.contains('/'));
        }

        /// On a case-insensitive file system every note still gets its own
        /// file; paths that don't clash keep their name. On a case-sensitive
        /// one nothing is renamed.
        #[test]
        fn case_clashes_get_distinct_files(paths in prop::collection::btree_set(
            prop::collection::vec(prop::sample::select(&["a", "A", "b", "Ä", "ä", "ß", "SS"][..]), 1..4)
                .prop_map(|v| v.concat() + ".md"),
            1..8,
        )) {
            let live: BTreeMap<_, _> = paths
                .iter()
                .enumerate()
                .map(|(i, p)| entry(&format!("n{i}"), p))
                .collect();
            let plain = assign_disk_paths(&live, &BTreeMap::new(), Fold::default());
            prop_assert!(live.iter().all(|(id, (p, _))| plain[id] == *p));

            let case = Fold { case: true, unicode: false };
            let folded = assign_disk_paths(&live, &BTreeMap::new(), case);
            prop_assert_eq!(folded.len(), live.len());
            let keys: BTreeSet<String> = folded.values().map(|d| fold_key(case, d)).collect();
            prop_assert_eq!(keys.len(), folded.len(), "two notes share a file: {:?}", folded);
            for (id, (p, _)) in &live {
                let clashes = live.values().filter(|(q, _)| fold_key(case, q) == fold_key(case, p)).count();
                if clashes == 1 {
                    prop_assert_eq!(&folded[id], p);
                }
            }
        }
    }
}
