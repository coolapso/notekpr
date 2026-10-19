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
//! (recoverable); a delete plus a create of identical content within a short
//! window is a rename. Dotfiles, editor swap/backup files and our own
//! temporary files are ignored. Change detection compares size + mtime and
//! falls back to content hashes, so our own writes are recognised.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use coolpsonotes_core::b64;
use coolpsonotes_core::doc::{EntryKind, NoteDoc};
use coolpsonotes_core::{paths, textdiff};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use crate::fsutil;
use crate::hub::{Hub, HubEvent};
use crate::ui;
use crate::vault::{Vault, content_hash, fork_note};

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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Base {
    #[serde(with = "b64")]
    state: Vec<u8>,
    hash: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct FileRecord {
    /// Vault path the file was written to / read from.
    path: String,
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
}

struct MirrorVault {
    dir: PathBuf,
    state: MirrorState,
    dirty: bool,
    /// Recorded files missing on disk, and since when (rename detection).
    missing_since: BTreeMap<String, Instant>,
    /// Deletions are held: too many files (or the whole folder) vanished.
    held: bool,
    warned: BTreeSet<String>,
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
}

pub struct Mirror {
    hub: Arc<Hub>,
    root: PathBuf,
    vaults: BTreeMap<String, MirrorVault>,
    warned: BTreeSet<String>,
    opts: MirrorOptions,
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
    paths::normalize(p).is_ok_and(|n| n == p)
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

/// Compare the file with its record (cheap stat first, then content).
fn disk_state(path: &Path, rec: &mut FileRecord) -> std::io::Result<DiskState> {
    let st = match stat(path)? {
        Node::File(st) => st,
        Node::Missing => return Ok(DiskState::Missing),
        Node::Other => return Ok(DiskState::NotAFile),
    };
    let racy = rec.checked_ns - rec.mtime_ns < RACY_NS;
    if st.size == rec.size && st.mtime_ns == rec.mtime_ns && !racy {
        return Ok(DiskState::Unchanged(st));
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DiskState::Missing),
        Err(e) => return Err(e),
    };
    if content_hash(&data) == rec.hash {
        rec.size = st.size;
        rec.mtime_ns = st.mtime_ns;
        rec.checked_ns = now_ns();
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
    let stamp = ui::fmt_time(coolpsonotes_core::now_ms())
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

fn push_base(rec: &mut FileRecord, state: Vec<u8>, hash: &str) {
    if let Some(last) = rec.bases.last()
        && last.hash == hash
    {
        rec.bases.pop();
    }
    rec.bases.push(Base {
        state,
        hash: hash.to_string(),
    });
    let excess = rec.bases.len().saturating_sub(MAX_BASES);
    rec.bases.drain(..excess);
}

/// Apply an external edit of a note relative to the best base. Returns the
/// state of the fork (a valid base for what is on disk now).
fn ingest_note(v: &mut Vault, id: &str, rec: &FileRecord, disk: &str) -> Result<Vec<u8>> {
    let live_state = v
        .replica
        .notes
        .entry(id.to_string())
        .or_default()
        .encode_state();
    if v.note_text(id) == disk {
        return Ok(live_state);
    }
    let mut candidates: Vec<NoteDoc> = rec
        .bases
        .iter()
        .filter_map(|b| fork_note(&b.state).ok())
        .collect();
    let texts: Vec<String> = candidates.iter().map(|f| f.text()).collect();
    let fork = match choose_base(&texts, disk) {
        Some(i) => candidates.swap_remove(i),
        None => fork_note(&live_state)?,
    };
    let base_text = fork.text();
    if let Some(update) = fork.apply_edits(&textdiff::diff(&base_text, disk)) {
        v.merge_note_update(id, update)?;
    }
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

impl FileRecord {
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
                 COOLPSONOTES_LOG=debug shows which"
            ),
        );
    }

    /// Whether `q` is taken on disk (anything at all is there) or by a live entry.
    fn taken(&self, q: &str, live_paths: &BTreeSet<String>) -> bool {
        live_paths.contains(q)
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
    let path = disk_path(&mv.dir, &rec.path)?;
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
                EntryKind::Note => {
                    let Ok(text) = String::from_utf8(data) else {
                        mv.warn_once(
                            &format!("utf8:{id}"),
                            "a Markdown file is not valid UTF-8; it is not synced",
                        );
                        return Ok(());
                    };
                    let base_state = ingest_note(v, id, &rec, &text)?;
                    push_base(&mut rec, base_state, &hash);
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
    let kind = if is_note {
        EntryKind::Note
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
            Ok(_) => {
                mv.missing_since.remove(&id);
                if let Some(r) = mv.state.files.get_mut(&id) {
                    r.path = path.to_string();
                    r.set_stat(st);
                }
                mv.dirty = true;
                touched.insert(id);
                report.renamed += 1;
                return Ok(());
            }
            Err(e) => tracing::debug!("rename not applied: {e:#}"),
        }
    }
    let (r, state) = if is_note {
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
        path: r.path.clone(),
        kind,
        hash: hash.clone(),
        size: st.size,
        mtime_ns: st.mtime_ns,
        checked_ns: now_ns(),
        bases: vec![],
    };
    if let Some(s) = state {
        push_base(&mut rec, s, &hash);
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
    let path = disk_path(&mv.dir, &rec.path)?;
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
        && rec.kind == EntryKind::Note
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
    live_paths: &BTreeSet<String>,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let Some(rec) = mv.state.files.get(id) else {
        return Ok(());
    };
    if rec.path == target {
        return Ok(());
    }
    let mut rec = rec.clone();
    let old = disk_path(&mv.dir, &rec.path)?;
    let new = disk_path(&mv.dir, target)?;
    match stat(&new)? {
        Node::Missing => {}
        Node::File(_) if !mv.state.files.values().any(|r| r.path == target) => {
            // An unknown local file is in the way: keep it as a conflict copy.
            let copy = conflict_name(target, |q| mv.taken(q, live_paths));
            std::fs::rename(&new, disk_path(&mv.dir, &copy)?)
                .context("keeping a conflicting local file")?;
            ui::warn(&format!(
                "vault '{}': a local file was in the way of a note moved elsewhere; it was kept as a conflict \
                 copy next to it",
                v.name()
            ));
            report.conflicts += 1;
        }
        // Another synced file or a folder is in the way: try again later.
        _ => return Ok(()),
    }
    match disk_state(&old, &mut rec)? {
        DiskState::Missing | DiskState::NotAFile => {
            // Written afresh at the new place by step c.
            mv.state.files.remove(id);
        }
        state => {
            if let DiskState::Modified(data, _) = state
                && writable
                && rec.kind == EntryKind::Note
                && let Ok(text) = String::from_utf8(data)
            {
                let hash = content_hash(text.as_bytes());
                let base = ingest_note(v, id, &rec, &text)?;
                push_base(&mut rec, base, &hash);
                rec.hash = hash;
                touched.insert(id.to_string());
            }
            if let Some(parent) = new.parent() {
                fsutil::create_private_dir(parent)?;
            }
            std::fs::rename(&old, &new).context("moving a mirrored file")?;
            prune_empty_parents(&old, &mv.dir);
            rec.path = target.to_string();
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
    p: &str,
    live_paths: &BTreeSet<String>,
    writable: bool,
    touched: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<()> {
    let path = disk_path(&mv.dir, p)?;
    let text = v.note_text(id);
    let hash = content_hash(text.as_bytes());
    let Some(mut rec) = mv.state.files.get(id).cloned() else {
        // Not written yet. A file already there is adopted if identical,
        // otherwise kept as a conflict copy.
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
                    let copy = conflict_name(p, |q| mv.taken(q, live_paths));
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
        let state = v
            .replica
            .notes
            .get(id)
            .map(|n| n.encode_state())
            .unwrap_or_default();
        let mut rec = FileRecord {
            path: p.to_string(),
            kind: EntryKind::Note,
            hash: hash.clone(),
            size: 0,
            mtime_ns: 0,
            checked_ns: 0,
            bases: vec![],
        };
        push_base(&mut rec, state, &hash);
        mv.state.files.insert(id.to_string(), rec);
        mv.record_stat(id, &path);
        return Ok(());
    };
    // A move that step b could not apply yet: the file at `p` is not ours.
    if rec.path != p || rec.hash == hash {
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
            push_base(&mut rec, base, &disk_hash);
            rec.hash = disk_hash;
            rec.set_stat(st);
            touched.insert(id.to_string());
            report.ingested += 1;
            st
        }
        DiskState::Unchanged(st) => st,
    };
    let text = v.note_text(id);
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
                    push_base(&mut rec, base, &saved_hash);
                    touched.insert(id.to_string());
                    report.ingested += 1;
                }
            }
        }
        rec.hash = hash.clone();
        report.written += 1;
    }
    push_base(&mut rec, state, &hash);
    mv.state.files.insert(id.to_string(), rec);
    mv.record_stat(id, &path);
    Ok(())
}

/// Write an attachment that was downloaded (outside the vault lock).
fn write_download(
    mv: &mut MirrorVault,
    id: &str,
    p: &str,
    data: &[u8],
    report: &mut Report,
) -> Result<()> {
    let path = disk_path(&mv.dir, p)?;
    let hash = content_hash(data);
    match stat(&path)? {
        Node::Other => return Ok(()),
        Node::File(_) => {
            let current = std::fs::read(&path)?;
            if content_hash(&current) != hash {
                let copy = conflict_name(p, |q| mv.taken(q, &BTreeSet::new()));
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
        let mut m = Mirror {
            hub,
            root,
            vaults: BTreeMap::new(),
            warned: BTreeSet::new(),
            opts,
        };
        for id in m.hub.vault_ids() {
            m.attach_or_warn(&id).await;
        }
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
        let taken: BTreeSet<String> = self
            .vaults
            .values()
            .map(|m| m.state.dir_name.clone())
            .collect();
        let state = match stored {
            Some(s) if s.root == self.root && !taken.contains(&s.dir_name) => s,
            _ => {
                let mut dir_name = sanitize_dir_name(&name, vault_id);
                if taken.contains(&dir_name) {
                    dir_name = format!("{dir_name} ({})", &vault_id[..vault_id.len().min(6)]);
                }
                MirrorState {
                    root: self.root.clone(),
                    dir_name,
                    files: BTreeMap::new(),
                    folders: BTreeMap::new(),
                }
            }
        };
        let dir = disk_path(&self.root, &state.dir_name)?;
        // A vault folder that vanished is not re-created (that would make
        // every note look deleted): the passes hold and report it instead.
        if state.files.is_empty() || dir.is_dir() || self.opts.restore_missing {
            fsutil::create_private_dir(&dir)?;
            fsutil::remove_stale_temp_files(&dir, 64, STALE_TEMP_AGE);
        }
        self.vaults.insert(
            vault_id.to_string(),
            MirrorVault {
                dir,
                state,
                dirty: true,
                missing_since: BTreeMap::new(),
                held: false,
                warned: BTreeSet::new(),
            },
        );
        Ok(())
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
        for id in self.hub.vault_ids() {
            self.attach_or_warn(&id).await;
        }
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
        let Some(mv) = self.vaults.get_mut(vault_id) else {
            return Ok(report);
        };
        let (disk, empty_dirs) = scan_dir(&mv.dir);
        let mut v = shared.lock().await;
        let name = v.name().to_string();
        let writable = !self.hub.read_only && v.require_writable().is_ok();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let live = v.paths();
        let known_paths: BTreeSet<String> =
            mv.state.files.values().map(|r| r.path.clone()).collect();

        // 1. Recorded files: modified or missing?
        let ids: Vec<String> = mv.state.files.keys().cloned().collect();
        for id in &ids {
            if let Err(e) = ingest_record(mv, &mut v, id, writable, &mut touched, &mut report) {
                mv.entry_failed(&name, id, &e);
            }
        }

        // 2. New files: renames (same content as a missing file) or creations.
        let new_files: Vec<(String, Stat)> = disk
            .iter()
            .filter(|(p, _)| !known_paths.contains(*p))
            // A live entry we have not written yet: write_remote adopts or resolves it.
            .filter(|(p, _)| !live.contains_key(*p))
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        if writable && !new_files.is_empty() {
            let max_attachment = if new_files.iter().any(|(p, _)| !paths::is_note(p)) {
                self.hub
                    .session
                    .server_info()
                    .await
                    .map(|i| i.max_attachment_bytes)
                    .unwrap_or(u64::MAX)
            } else {
                u64::MAX
            };
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
        let mass =
            missing > 0 && (dir_gone || (missing > MASS_DELETE_MIN_FILES && missing * 2 > total));
        let mut expired: Vec<String> = mv
            .missing_since
            .iter()
            .filter(|(_, t)| final_pass || t.elapsed() >= RENAME_WINDOW)
            .map(|(id, _)| id.clone())
            .collect();
        if mass && !opts.allow_mass_delete {
            if opts.restore_missing {
                // Forget them: write_remote writes them again.
                for id in std::mem::take(&mut mv.missing_since).into_keys() {
                    mv.state.files.remove(&id);
                    report.restored += 1;
                }
                mv.dirty = true;
            } else {
                if !mv.held {
                    let what = if dir_gone {
                        format!("its folder {} is gone", mv.dir.display())
                    } else {
                        format!("{missing} of {total} mirrored files disappeared at once")
                    };
                    ui::warn(&format!(
                        "vault '{name}': {what}. Nothing was moved to the trash. If the folder was moved, renamed \
                         or unmounted, put it back. If you deleted the files on purpose, stop `coolpsonotes sync` \
                         and run `coolpsonotes sync --once --allow-mass-delete` (moves them to the trash), or \
                         `--restore-missing` to get the files back"
                    ));
                }
                mv.held = true;
                report.held = missing;
            }
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
        if writable && !mv.held {
            for d in &empty_dirs {
                let known = mv.state.folders.values().any(|p| p == d)
                    || live.keys().any(|p| p == d || paths::is_within(p, d));
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
                mv.state.folders.remove(&id);
                mv.dirty = true;
                let has_children = v.paths().keys().any(|q| paths::is_within(q, &p));
                if !has_children && v.entry(&id).is_some_and(|e| e.is_live()) {
                    if let Err(e) = v.trash_id(&id) {
                        mv.entry_failed(&name, &id, &e);
                        continue;
                    }
                    report.trashed += 1;
                    touched.insert(id);
                }
            }
        }

        let changed = !touched.is_empty();
        if changed {
            v.save()?;
        }
        drop(v);
        if changed {
            self.hub
                .local_changed(vault_id, touched.into_iter().collect(), true)
                .await;
        }
        if report.local_changes() > 0 {
            ui::status(&format!(
                "vault '{name}': {} local edit(s), {} new, {} moved, {} trashed",
                report.ingested, report.created, report.renamed, report.trashed
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
        let mut downloads: Vec<(String, String)> = Vec::new();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let name;
        {
            let mut v = shared.lock().await;
            if v.read_only.is_some() {
                return Ok(report);
            }
            name = v.name().to_string();
            let writable = !self.hub.read_only && v.require_writable().is_ok();
            let mut live: BTreeMap<String, (String, coolpsonotes_core::doc::Entry)> =
                BTreeMap::new();
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
            let live_paths: BTreeSet<String> = live.values().map(|(p, _)| p.clone()).collect();

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

            // b. Moves/renames done elsewhere.
            for (id, (p, _)) in &live {
                if let Err(e) = apply_move(
                    mv,
                    &mut v,
                    id,
                    p,
                    &live_paths,
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
                            downloads.push((id.clone(), p.clone()));
                        }
                        Ok(())
                    }
                    EntryKind::Note => write_note(
                        mv,
                        &mut v,
                        id,
                        p,
                        &live_paths,
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
            if !touched.is_empty() {
                v.save()?;
            }
        }

        // Attachments are downloaded without holding the vault lock.
        for (id, p) in downloads {
            let data = match self.hub.session.attachment(&shared, &id).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!("attachment not available yet: {e:#}");
                    continue;
                }
            };
            if let Err(e) = write_download(mv, &id, &p, &data, &mut report) {
                mv.entry_failed(&name, &id, &e);
            }
        }

        if !touched.is_empty() {
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
