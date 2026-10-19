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

use anyhow::{Context, Result};
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
/// Files whose mtime is this close to when we recorded them are re-hashed,
/// since a same-size edit within the timestamp granularity is invisible to stat.
const RACY_NS: i64 = 2_000_000_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Base {
    #[serde(with = "b64")]
    state: Vec<u8>,
    hash: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
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

pub struct Mirror {
    hub: Arc<Hub>,
    root: PathBuf,
    vaults: BTreeMap<String, MirrorVault>,
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
pub fn is_safe_vault_path(p: &str) -> bool {
    paths::normalize(p).is_ok_and(|n| n == p)
        && p.split('/')
            .all(|s| !is_ignored_name(s) && !s.contains('\0'))
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

#[derive(Debug, Clone, Copy)]
struct Stat {
    size: u64,
    mtime_ns: i64,
}

fn stat(path: &Path) -> std::io::Result<Option<Stat>> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(Some(Stat {
            size: m.len(),
            mtime_ns: m
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
        })),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

enum DiskState {
    Missing,
    Unchanged,
    Modified(Vec<u8>),
}

/// Compare the file with its record (cheap stat first, then content).
fn disk_state(path: &Path, rec: &mut FileRecord) -> std::io::Result<DiskState> {
    let Some(st) = stat(path)? else {
        return Ok(DiskState::Missing);
    };
    let racy = rec.checked_ns - rec.mtime_ns < RACY_NS;
    if st.size == rec.size && st.mtime_ns == rec.mtime_ns && !racy {
        return Ok(DiskState::Unchanged);
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
        return Ok(DiskState::Unchanged);
    }
    Ok(DiskState::Modified(data))
}

/// Disk path of a vault path below `dir`.
fn disk_path(dir: &Path, vault_path: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for seg in vault_path.split('/') {
        p.push(seg);
    }
    p
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
            && let Ok(Some(st)) = stat(e.path())
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

/// Replace `path` atomically, unless it changed since `expected` was observed.
/// Returns false (and leaves the file alone) if it changed.
fn replace_if_unchanged(path: &Path, data: &[u8], expected: Option<Stat>) -> Result<bool> {
    if let Some(parent) = path.parent() {
        fsutil::create_private_dir(parent)?;
    }
    let current = stat(path)?;
    let same = match (current, expected) {
        (None, None) => true,
        (Some(a), Some(b)) => a.size == b.size && a.mtime_ns == b.mtime_ns,
        _ => false,
    };
    if !same {
        return Ok(false);
    }
    fsutil::write_atomic(path, data, true).with_context(|| "writing a mirrored file")?;
    Ok(true)
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

impl MirrorVault {
    fn record_stat(&mut self, id: &str, path: &Path) {
        if let (Some(rec), Ok(Some(st))) = (self.state.files.get_mut(id), stat(path)) {
            rec.size = st.size;
            rec.mtime_ns = st.mtime_ns;
            rec.checked_ns = now_ns();
        }
        self.dirty = true;
    }

    fn warn_once(&mut self, key: &str, msg: &str) {
        if self.warned.insert(key.to_string()) {
            ui::warn(msg);
        }
    }
}

impl Mirror {
    /// Prepare the mirror for every vault open on the hub.
    pub async fn new(hub: Arc<Hub>, root: &Path) -> Result<Mirror> {
        fsutil::create_private_dir(root)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        let mut m = Mirror {
            hub,
            root,
            vaults: BTreeMap::new(),
        };
        for id in m.hub.vault_ids() {
            m.attach(&id).await?;
        }
        Ok(m)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory of a vault's files.
    pub fn vault_dir(&self, vault_id: &str) -> Option<&Path> {
        self.vaults.get(vault_id).map(|m| m.dir.as_path())
    }

    async fn attach(&mut self, vault_id: &str) -> Result<()> {
        if self.vaults.contains_key(vault_id) {
            return Ok(());
        }
        let Some(v) = self.hub.get(vault_id) else {
            return Ok(());
        };
        let (name, stored) = {
            let g = v.lock().await;
            let stored: Option<MirrorState> = g.store().load_aux("mirror")?.and_then(|b| {
                serde_json::from_slice(&b)
                    .map_err(|e| tracing::warn!("mirror state unreadable: {e}"))
                    .ok()
            });
            (g.name().to_string(), stored)
        };
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
        let dir = self.root.join(&state.dir_name);
        fsutil::create_private_dir(&dir)?;
        self.vaults.insert(
            vault_id.to_string(),
            MirrorVault {
                dir,
                state,
                dirty: true,
                missing_since: BTreeMap::new(),
                warned: BTreeSet::new(),
            },
        );
        Ok(())
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
    pub async fn reconcile_all(&mut self, final_pass: bool) -> Result<Report> {
        let mut total = Report::default();
        for id in self.hub.vault_ids() {
            self.attach(&id).await?;
        }
        let ids: Vec<String> = self.vaults.keys().cloned().collect();
        for id in ids {
            let mut r = self.ingest(&id, false).await?;
            r.add(self.write_remote(&id, &Scope::All).await?);
            r.add(self.ingest(&id, final_pass).await?);
            r.add(self.write_remote(&id, &Scope::All).await?);
            self.save_state(&id).await?;
            total.add(r);
        }
        Ok(total)
    }

    /// Disk → CRDT for one vault.
    pub async fn ingest(&mut self, vault_id: &str, final_pass: bool) -> Result<Report> {
        let mut report = Report::default();
        let Some(shared) = self.hub.get(vault_id) else {
            return Ok(report);
        };
        let Some(mv) = self.vaults.get_mut(vault_id) else {
            return Ok(report);
        };
        let (disk, empty_dirs) = scan_dir(&mv.dir);
        let mut v = shared.lock().await;
        let writable = !self.hub.read_only && v.require_writable().is_ok();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let live = v.paths();
        let known_paths: BTreeMap<String, String> = mv
            .state
            .files
            .iter()
            .map(|(id, r)| (r.path.clone(), id.clone()))
            .collect();

        // 1. Recorded files: modified or missing?
        let ids: Vec<String> = mv.state.files.keys().cloned().collect();
        for id in &ids {
            let mut rec = mv.state.files[id].clone();
            let path = disk_path(&mv.dir, &rec.path);
            let state = disk_state(&path, &mut rec)?;
            match state {
                DiskState::Unchanged => {
                    mv.missing_since.remove(id);
                }
                DiskState::Missing => {
                    mv.missing_since
                        .entry(id.clone())
                        .or_insert_with(Instant::now);
                }
                DiskState::Modified(data) => {
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
                        continue;
                    }
                    let st = stat(&path)?;
                    match rec.kind {
                        EntryKind::Note => {
                            let Ok(text) = String::from_utf8(data.clone()) else {
                                mv.warn_once(
                                    &format!("utf8:{id}"),
                                    "a Markdown file is not valid UTF-8; it is not synced",
                                );
                                continue;
                            };
                            if v.entry(id).is_none() {
                                continue;
                            }
                            let base_state = ingest_note(&mut v, id, &rec, &text)?;
                            push_base(&mut rec, base_state, &content_hash(text.as_bytes()));
                            touched.insert(id.clone());
                            report.ingested += 1;
                        }
                        EntryKind::Attachment => {
                            // Blobs are immutable: a new version is a new entry
                            // (the previous one goes to the trash).
                            let res = v.add_attachment(&rec.path, &data, None)?;
                            mv.state.files.remove(id);
                            rec.hash = content_hash(&data);
                            if let Some(st) = st {
                                rec.size = st.size;
                                rec.mtime_ns = st.mtime_ns;
                                rec.checked_ns = now_ns();
                            }
                            mv.state.files.insert(res.id.clone(), rec);
                            mv.dirty = true;
                            touched.insert(res.id);
                            report.ingested += 1;
                            continue;
                        }
                        EntryKind::Folder => continue,
                    }
                    rec.hash = content_hash(&data);
                    if let Some(st) = st {
                        rec.size = st.size;
                        rec.mtime_ns = st.mtime_ns;
                        rec.checked_ns = now_ns();
                    }
                }
            }
            if mv.state.files.contains_key(id) {
                mv.state.files.insert(id.clone(), rec);
                mv.dirty = true;
            }
        }

        // 2. New files: renames (same content as a missing file) or creations.
        let new_files: Vec<(String, Stat)> = disk
            .iter()
            .filter(|(p, _)| !known_paths.contains_key(*p))
            // A live entry we have not written yet: write_remote adopts or resolves it.
            .filter(|(p, _)| !live.contains_key(*p))
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        if writable {
            for (path, st) in new_files {
                let full = disk_path(&mv.dir, &path);
                let Ok(data) = std::fs::read(&full) else {
                    continue;
                };
                let hash = content_hash(&data);
                let is_note = paths::is_note(&path);
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
                    match v.move_path(&id, &path, false) {
                        Ok(_) => {
                            mv.missing_since.remove(&id);
                            if let Some(r) = mv.state.files.get_mut(&id) {
                                r.path = path.clone();
                                r.size = st.size;
                                r.mtime_ns = st.mtime_ns;
                                r.checked_ns = now_ns();
                            }
                            mv.dirty = true;
                            touched.insert(id);
                            report.renamed += 1;
                            continue;
                        }
                        Err(e) => tracing::debug!("rename not applied: {e:#}"),
                    }
                }
                let res = if is_note {
                    let Ok(text) = String::from_utf8(data.clone()) else {
                        mv.warn_once(
                            &format!("utf8:{path}"),
                            "a Markdown file is not valid UTF-8; it is not synced",
                        );
                        continue;
                    };
                    match v.create_note(&path, &text) {
                        Ok(r) => {
                            let state = v
                                .replica
                                .notes
                                .get(&r.id)
                                .map(|n| n.encode_state())
                                .unwrap_or_default();
                            (r, Some(state))
                        }
                        Err(e) => {
                            mv.warn_once(
                                &format!("create:{path}"),
                                &format!("cannot sync a new file: {e:#}"),
                            );
                            continue;
                        }
                    }
                } else {
                    let max = self
                        .hub
                        .session
                        .server_info()
                        .await
                        .map(|i| i.max_attachment_bytes)
                        .unwrap_or(u64::MAX);
                    if data.len() as u64 > max {
                        mv.warn_once(
                            &format!("big:{path}"),
                            &format!(
                                "a file is larger than the server limit ({max} bytes); not synced"
                            ),
                        );
                        continue;
                    }
                    match v.add_attachment(&path, &data, None) {
                        Ok(r) => (r, None),
                        Err(e) => {
                            mv.warn_once(
                                &format!("create:{path}"),
                                &format!("cannot sync a new file: {e:#}"),
                            );
                            continue;
                        }
                    }
                };
                let (r, state) = res;
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
            }
        }

        // 3. Missing long enough (or final pass): move to the trash.
        let expired: Vec<String> = mv
            .missing_since
            .iter()
            .filter(|(_, t)| final_pass || t.elapsed() >= RENAME_WINDOW)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            mv.missing_since.remove(&id);
            if writable && v.entry(&id).is_some_and(|e| e.is_live()) {
                v.trash_id(&id)?;
                report.trashed += 1;
                touched.insert(id.clone());
            }
            // Read-only vaults: forgetting the record makes write_remote restore the file.
            mv.state.files.remove(&id);
            mv.dirty = true;
        }

        // 4. Explicit folders: new empty directories, removed folder entries.
        if writable {
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
                .filter(|(_, p)| !disk_path(&mv.dir, p).is_dir())
                .map(|(id, p)| (id.clone(), p.clone()))
                .collect();
            for (id, p) in gone {
                mv.state.folders.remove(&id);
                mv.dirty = true;
                let has_children = v.paths().keys().any(|q| paths::is_within(q, &p));
                if !has_children && v.entry(&id).is_some_and(|e| e.is_live()) {
                    v.trash_id(&id)?;
                    report.trashed += 1;
                    touched.insert(id);
                }
            }
        }

        let changed = !touched.is_empty();
        if changed {
            v.save()?;
        }
        let vault_name = v.name().to_string();
        drop(v);
        if changed {
            self.hub
                .local_changed(vault_id, touched.into_iter().collect(), true)
                .await;
        }
        if report.local_changes() > 0 {
            ui::status(&format!(
                "vault '{vault_name}': {} local edit(s), {} new, {} moved, {} trashed",
                report.ingested, report.created, report.renamed, report.trashed
            ));
        }
        Ok(report)
    }

    /// CRDT → disk for one vault.
    pub async fn write_remote(&mut self, vault_id: &str, scope: &Scope) -> Result<Report> {
        let mut report = Report::default();
        let Some(shared) = self.hub.get(vault_id) else {
            return Ok(report);
        };
        let Some(mv) = self.vaults.get_mut(vault_id) else {
            return Ok(report);
        };
        let mut downloads: Vec<(String, String)> = Vec::new();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        {
            let mut v = shared.lock().await;
            if v.read_only.is_some() {
                return Ok(report);
            }
            let writable = !self.hub.read_only && v.require_writable().is_ok();
            let mut live: BTreeMap<String, (String, coolpsonotes_core::doc::Entry)> =
                BTreeMap::new();
            for (p, e) in v.paths() {
                if !is_safe_vault_path(&p) {
                    mv.warn_once(
                        &format!("unsafe:{}", e.id),
                        &format!(
                            "vault '{}' contains an entry with a hidden or unsupported path; it is not mirrored",
                            v.name()
                        ),
                    );
                    continue;
                }
                live.insert(e.id.clone(), (p, e));
            }

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
                let Some(mut rec) = mv.state.files.remove(&id) else {
                    continue;
                };
                mv.dirty = true;
                mv.missing_since.remove(&id);
                let path = disk_path(&mv.dir, &rec.path);
                match disk_state(&path, &mut rec)? {
                    DiskState::Missing => {}
                    DiskState::Unchanged => {
                        let _ = std::fs::remove_file(&path);
                        prune_empty_parents(&path, &mv.dir);
                        report.removed += 1;
                    }
                    DiskState::Modified(data) => {
                        // Edited here while trashed elsewhere: keep the edit in
                        // the (trashed) note and leave the file in place, where
                        // the next scan re-creates it as a new note.
                        if writable
                            && rec.kind == EntryKind::Note
                            && v.entry(&id).is_some()
                            && let Ok(text) = String::from_utf8(data)
                        {
                            let _ = ingest_note(&mut v, &id, &rec, &text);
                            touched.insert(id.clone());
                        }
                        mv.warn_once(
                            &format!("trashed:{id}"),
                            "a file you edited locally was deleted or moved to the trash elsewhere; your copy is kept",
                        );
                        report.conflicts += 1;
                    }
                }
            }

            // b. Moves/renames done elsewhere.
            for (id, (p, _)) in &live {
                let Some(rec) = mv.state.files.get(id) else {
                    continue;
                };
                if rec.path == *p {
                    continue;
                }
                let mut rec = rec.clone();
                let old = disk_path(&mv.dir, &rec.path);
                let new = disk_path(&mv.dir, p);
                if stat(&new)?.is_some() {
                    continue;
                }
                match disk_state(&old, &mut rec)? {
                    DiskState::Missing => {
                        mv.state.files.remove(id);
                    }
                    state => {
                        if let DiskState::Modified(data) = state
                            && writable
                            && rec.kind == EntryKind::Note
                            && let Ok(text) = String::from_utf8(data.clone())
                        {
                            let base = ingest_note(&mut v, id, &rec, &text)?;
                            push_base(&mut rec, base, &content_hash(&data));
                            rec.hash = content_hash(&data);
                            touched.insert(id.clone());
                        }
                        if let Some(parent) = new.parent() {
                            fsutil::create_private_dir(parent)?;
                        }
                        std::fs::rename(&old, &new).with_context(|| "moving a mirrored file")?;
                        prune_empty_parents(&old, &mv.dir);
                        rec.path = p.clone();
                        mv.state.files.insert(id.clone(), rec);
                        mv.record_stat(id, &new);
                        report.renamed += 1;
                    }
                }
                mv.dirty = true;
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
                let path = disk_path(&mv.dir, p);
                match e.kind {
                    EntryKind::Folder => {
                        if !path.is_dir() && stat(&path)?.is_none() {
                            fsutil::create_private_dir(&path)?;
                        }
                        if path.is_dir() && mv.state.folders.get(id) != Some(p) {
                            mv.state.folders.insert(id.clone(), p.clone());
                            mv.dirty = true;
                        }
                    }
                    EntryKind::Attachment => {
                        if !mv.state.files.contains_key(id) {
                            downloads.push((id.clone(), p.clone()));
                        }
                    }
                    EntryKind::Note => {
                        let text = v.note_text(id);
                        let hash = content_hash(text.as_bytes());
                        match mv.state.files.get(id).cloned() {
                            Some(mut rec) => {
                                if rec.hash == hash {
                                    continue;
                                }
                                let before = stat(&path)?;
                                match disk_state(&path, &mut rec)? {
                                    // Deleted locally; ingest() trashes it.
                                    DiskState::Missing => continue,
                                    DiskState::Modified(data) => {
                                        if !writable {
                                            continue;
                                        }
                                        let Ok(disk_text) = String::from_utf8(data.clone()) else {
                                            continue;
                                        };
                                        let base = ingest_note(&mut v, id, &rec, &disk_text)?;
                                        push_base(&mut rec, base, &content_hash(&data));
                                        rec.hash = content_hash(&data);
                                        touched.insert(id.clone());
                                        report.ingested += 1;
                                    }
                                    DiskState::Unchanged => {}
                                }
                                let text = v.note_text(id);
                                let hash = content_hash(text.as_bytes());
                                if hash != rec.hash {
                                    let observed = stat(&path)?.or(before);
                                    if !replace_if_unchanged(&path, text.as_bytes(), observed)? {
                                        // Changed under us: picked up by the next scan.
                                        mv.state.files.insert(id.clone(), rec);
                                        continue;
                                    }
                                    rec.hash = hash.clone();
                                    report.written += 1;
                                }
                                let state = v
                                    .replica
                                    .notes
                                    .get(id)
                                    .map(|n| n.encode_state())
                                    .unwrap_or_default();
                                push_base(&mut rec, state, &hash);
                                mv.state.files.insert(id.clone(), rec);
                                mv.record_stat(id, &path);
                            }
                            None => {
                                // Not written yet. A file already there is adopted
                                // if identical, otherwise kept as a conflict copy.
                                if let Some(st) = stat(&path)? {
                                    let data = std::fs::read(&path)?;
                                    if content_hash(&data) != hash {
                                        let taken = |q: &str| {
                                            disk_path(&mv.dir, q).exists()
                                                || live.values().any(|(lp, _)| lp == q)
                                        };
                                        let cpath = conflict_name(p, taken);
                                        let cfull = disk_path(&mv.dir, &cpath);
                                        std::fs::rename(&path, &cfull)
                                            .with_context(|| "keeping a conflicting local file")?;
                                        ui::warn(&format!(
                                            "vault '{}': a local file differed from the synced document at the same path; \
                                             it was kept as a conflict copy next to it",
                                            v.name()
                                        ));
                                        report.conflicts += 1;
                                        if !replace_if_unchanged(&path, text.as_bytes(), None)? {
                                            continue;
                                        }
                                        report.written += 1;
                                    } else {
                                        let _ = st;
                                    }
                                } else {
                                    if !replace_if_unchanged(&path, text.as_bytes(), None)? {
                                        continue;
                                    }
                                    report.written += 1;
                                }
                                let state = v
                                    .replica
                                    .notes
                                    .get(id)
                                    .map(|n| n.encode_state())
                                    .unwrap_or_default();
                                let mut rec = FileRecord {
                                    path: p.clone(),
                                    kind: EntryKind::Note,
                                    hash: hash.clone(),
                                    size: 0,
                                    mtime_ns: 0,
                                    checked_ns: 0,
                                    bases: vec![],
                                };
                                push_base(&mut rec, state, &hash);
                                mv.state.files.insert(id.clone(), rec);
                                mv.record_stat(id, &path);
                            }
                        }
                    }
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
                let d = disk_path(&mv.dir, &p);
                if std::fs::remove_dir(&d).is_ok() {
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
            let path = disk_path(&mv.dir, &p);
            let hash = content_hash(&data);
            let existing = stat(&path)?;
            if existing.is_some() {
                let current = std::fs::read(&path)?;
                if content_hash(&current) != hash {
                    let taken = |q: &str| disk_path(&mv.dir, q).exists();
                    let cpath = conflict_name(&p, taken);
                    std::fs::rename(&path, disk_path(&mv.dir, &cpath))?;
                    report.conflicts += 1;
                    if !replace_if_unchanged(&path, &data, None)? {
                        continue;
                    }
                    report.written += 1;
                }
            } else {
                if !replace_if_unchanged(&path, &data, None)? {
                    continue;
                }
                report.written += 1;
            }
            mv.state.files.insert(
                id.clone(),
                FileRecord {
                    path: p,
                    kind: EntryKind::Attachment,
                    hash,
                    size: 0,
                    mtime_ns: 0,
                    checked_ns: 0,
                    bases: vec![],
                },
            );
            mv.record_stat(&id, &path);
        }

        if !touched.is_empty() {
            self.hub
                .local_changed(vault_id, touched.into_iter().collect(), true)
                .await;
        }
        let written = report.written + report.removed + report.renamed;
        if written > 0 {
            let name = shared.lock().await.name().to_string();
            ui::status(&format!(
                "vault '{name}': {} file(s) updated, {} moved, {} removed",
                report.written, report.renamed, report.removed
            ));
        }
        Ok(report)
    }

    /// Watch the directory and the hub until `shutdown`.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        use notify::{RecursiveMode, Watcher};
        let (tx, mut fs_rx) = mpsc::unbounded_channel::<()>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let relevant = ev.paths.iter().any(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| !is_ignored_name(n))
                    });
                    if relevant && !matches!(ev.kind, notify::EventKind::Access(_)) {
                        let _ = tx.send(());
                    }
                }
            })?;
        watcher.watch(&self.root, RecursiveMode::Recursive)?;
        let mut events = self.hub.subscribe();
        // Close the gap between the caller's initial pass and the watch starting.
        if let Err(e) = self.reconcile_all(false).await {
            ui::warn(&format!("mirror: {e:#}"));
        }
        let mut full = tokio::time::interval(FULL_SCAN_EVERY);
        full.tick().await;
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
                Some(()) = fs_rx.recv() => {
                    tracing::debug!("mirror: file system change");
                    tokio::time::sleep(DEBOUNCE).await;
                    while fs_rx.try_recv().is_ok() {}
                    for id in self.vaults.keys().cloned().collect::<Vec<_>>() {
                        if let Err(e) = self.ingest(&id, false).await {
                            ui::warn(&format!("mirror: {e:#}"));
                        }
                        if let Err(e) = self.write_remote(&id, &Scope::Objects(BTreeSet::new())).await {
                            ui::warn(&format!("mirror: {e:#}"));
                        }
                        let _ = self.save_state(&id).await;
                    }
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
                        if let Err(e) = self.attach(&vault_id).await {
                            ui::warn(&format!("mirror: {e:#}"));
                        }
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
                        let _ = self.save_state(&id).await;
                    }
                }
                _ = deletion_check.tick() => {
                    let has_missing = self.vaults.values().any(|m| !m.missing_since.is_empty());
                    if has_missing {
                        for id in self.vaults.keys().cloned().collect::<Vec<_>>() {
                            if let Err(e) = self.ingest(&id, false).await {
                                ui::warn(&format!("mirror: {e:#}"));
                            }
                            let _ = self.save_state(&id).await;
                        }
                    }
                }
                _ = full.tick() => {
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
        let d = disk_path(Path::new("/m/v"), "a/b.md");
        assert_eq!(d, Path::new("/m/v/a/b.md"));
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
