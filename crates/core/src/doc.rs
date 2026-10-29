//! CRDT document model (yrs, binary-compatible with Yjs update format v1).
//!
//! Every vault has exactly two kinds of objects:
//!
//! * the **index** (object id [`INDEX_OBJECT_ID`]): a map of entry id → entry
//!   (path, kind, timestamps). Renames, moves, folder creation and trash are
//!   CRDT map edits, so they merge offline like everything else.
//! * one **note** object per Markdown document, holding the source text in a
//!   single `Y.Text`. Markdown stays the canonical representation.
//!
//! Attachments are entries in the index whose bytes live in encrypted blobs.
//!
//! [`INDEX_OBJECT_ID`]: crate::protocol::INDEX_OBJECT_ID

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use yrs::updates::decoder::Decode;
use yrs::{
    Any, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Text, TextRef,
    Transact, Update,
};

use crate::textdiff::{self, Edit};
use crate::{order, paths};

/// Transaction origin used for edits made by the local user.
pub const LOCAL_ORIGIN: &str = "local";
/// Transaction origin used when integrating updates received from elsewhere.
/// Everything else (local edits, undo/redo) is reported as a local update.
pub const REMOTE_ORIGIN: &str = "remote";
const NOTE_TEXT: &str = "content";
const INDEX_FILES: &str = "files";

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("invalid CRDT update: {0}")]
    BadUpdate(String),
}

pub type Result<T> = std::result::Result<T, DocError>;

/// Collects the binary updates produced by local transactions.
#[derive(Clone, Default)]
struct LocalUpdates(Arc<Mutex<Vec<Vec<u8>>>>);

impl LocalUpdates {
    fn attach(&self, doc: &Doc) {
        let sink = self.0.clone();
        doc.observe_update_v1("notekpr-local", move |txn, e| {
            if !is_remote(txn.origin()) {
                sink.lock().unwrap().push(e.update.clone());
            }
        })
        .expect("observer registration on a fresh doc");
    }

    fn take(&self) -> Option<Vec<u8>> {
        let mut v = std::mem::take(&mut *self.0.lock().unwrap());
        match v.len() {
            0 => None,
            1 => v.pop(),
            _ => Some(
                yrs::merge_updates_v1(v.iter().map(|u| u.as_slice())).expect("merging own updates"),
            ),
        }
    }
}

fn is_remote(origin: Option<&yrs::Origin>) -> bool {
    origin.is_some_and(|o| o.as_ref() == REMOTE_ORIGIN.as_bytes())
}

fn decode(update: &[u8]) -> Result<Update> {
    check_plausible(update)?;
    Update::decode_v1(update).map_err(|e| DocError::BadUpdate(e.to_string()))
}

/// Refuse an update whose header claims more clients than its bytes could
/// hold. yrs reserves a hash table for the claimed count before reading
/// them, so a 7-byte update declaring a billion clients made it allocate and
/// initialise ~2 GB (found by fuzzing; any member could have crashed the
/// others' tabs). Each client entry takes at least three bytes (block
/// count, client id, clock).
fn check_plausible(update: &[u8]) -> Result<()> {
    let mut clients: u64 = 0;
    let mut used = 0;
    for (i, b) in update.iter().take(5).enumerate() {
        clients |= u64::from(b & 0x7f) << (7 * i);
        used = i + 1;
        if b & 0x80 == 0 {
            break;
        }
    }
    let rest = (update.len() - used) as u64;
    if clients > rest / 3 {
        return Err(DocError::BadUpdate(format!(
            "update declares {clients} clients in {} bytes",
            update.len()
        )));
    }
    Ok(())
}

fn apply(doc: &Doc, update: &[u8]) -> Result<()> {
    let u = decode(update)?;
    doc.transact_mut_with(REMOTE_ORIGIN)
        .apply_update(u)
        .map_err(|e| DocError::BadUpdate(e.to_string()))
}

/// Merge several updates (e.g. before encrypting a batch).
pub fn merge_updates(updates: &[Vec<u8>]) -> Result<Vec<u8>> {
    yrs::merge_updates_v1(updates.iter().map(|u| u.as_slice()))
        .map_err(|e| DocError::BadUpdate(e.to_string()))
}

/// A Markdown note.
pub struct NoteDoc {
    doc: Doc,
    text: TextRef,
    local: LocalUpdates,
    undo: Option<yrs::UndoManager>,
}

impl Default for NoteDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteDoc {
    pub fn new() -> Self {
        let doc = Doc::new();
        let text = doc.get_or_insert_text(NOTE_TEXT);
        let local = LocalUpdates::default();
        local.attach(&doc);
        NoteDoc {
            doc,
            text,
            local,
            undo: None,
        }
    }

    /// Build from a full state (snapshot) and/or a series of updates.
    pub fn from_updates<'a>(updates: impl IntoIterator<Item = &'a [u8]>) -> Result<Self> {
        let d = Self::new();
        for u in updates {
            d.apply_update(u)?;
        }
        Ok(d)
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Apply a remote update. Idempotent: re-applying a known update is a no-op.
    pub fn apply_update(&self, update: &[u8]) -> Result<()> {
        apply(&self.doc, update)
    }

    pub fn text(&self) -> String {
        self.text.get_string(&self.doc.transact())
    }

    /// Text length in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.text.len(&self.doc.transact()) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Replace the content by diffing against the current text. Returns the
    /// resulting update, or `None` if nothing changed.
    pub fn set_text(&self, new: &str) -> Option<Vec<u8>> {
        let old = self.text();
        if old == new {
            return None;
        }
        self.apply_edits(&textdiff::diff(&old, new))
    }

    /// Apply a list of splices as one local transaction.
    pub fn apply_edits(&self, edits: &[Edit]) -> Option<Vec<u8>> {
        if edits.is_empty() {
            return None;
        }
        {
            let mut txn = self.doc.transact_mut_with(LOCAL_ORIGIN);
            for e in edits {
                match e {
                    Edit::Delete { at, len } => {
                        self.text.remove_range(&mut txn, *at as u32, *len as u32)
                    }
                    Edit::Insert { at, text } => self.text.insert(&mut txn, *at as u32, text),
                }
            }
        }
        self.take_local_update()
    }

    /// Delete `delete` bytes at `at` and insert `insert` there.
    pub fn splice(&self, at: usize, delete: usize, insert: &str) -> Option<Vec<u8>> {
        let mut edits = Vec::with_capacity(2);
        if delete > 0 {
            edits.push(Edit::Delete { at, len: delete });
        }
        if !insert.is_empty() {
            edits.push(Edit::Insert {
                at,
                text: insert.to_string(),
            });
        }
        self.apply_edits(&edits)
    }

    /// Updates produced locally since the last call (edits, undo, redo).
    pub fn take_local_update(&self) -> Option<Vec<u8>> {
        self.local.take()
    }

    /// Full state encoded as a single update (used for snapshots and caches).
    ///
    /// Never contains deleted text. The undo manager keeps deleted items
    /// alive in this document so they can be restored, and encoding them as
    /// they are would hand text the user deleted to whoever reads the result,
    /// e.g. people a note is shared with later. Re-encoding through a fresh
    /// document lets yrs garbage-collect them.
    pub fn encode_state(&self) -> Vec<u8> {
        let raw = self
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        if self.undo.is_none() {
            return raw;
        }
        let fresh = Doc::new();
        match apply(&fresh, &raw) {
            Ok(()) => fresh
                .transact()
                .encode_state_as_update_v1(&StateVector::default()),
            Err(_) => {
                debug_assert!(false, "own state must re-apply");
                raw
            }
        }
    }

    pub fn state_vector(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    /// Everything the holder of `sv` is missing.
    pub fn encode_diff(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(sv)
    }

    /// Track local edits for undo/redo.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub fn enable_undo(&mut self) {
        self.enable_undo_with_clock(|| crate::now_ms() as u64);
    }

    /// Track local edits for undo/redo; `now_ms` groups edits made in quick succession.
    pub fn enable_undo_with_clock(&mut self, now_ms: impl Fn() -> u64 + Send + Sync + 'static) {
        let opts = yrs::undo::Options {
            capture_timeout_millis: 600,
            tracked_origins: Default::default(),
            capture_transaction: None,
            timestamp: Arc::new(now_ms),
            init_undo_stack: Vec::new(),
            init_redo_stack: Vec::new(),
        };
        let mut mgr = yrs::UndoManager::with_options(opts);
        mgr.expand_scope(&self.doc, &self.text);
        mgr.include_origin(LOCAL_ORIGIN);
        self.undo = Some(mgr);
    }

    /// Track local edits of a drawing (its elements, settings and pictures)
    /// for undo/redo, instead of a note's text.
    pub fn enable_drawing_undo_with_clock(
        &mut self,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) {
        let opts = yrs::undo::Options {
            // A gesture is one step: the app ends each with a boundary.
            capture_timeout_millis: 300,
            tracked_origins: Default::default(),
            capture_transaction: None,
            timestamp: Arc::new(now_ms),
            init_undo_stack: Vec::new(),
            init_redo_stack: Vec::new(),
        };
        let mut mgr = yrs::UndoManager::with_options(opts);
        for root in crate::drawing::roots(&self.doc) {
            mgr.expand_scope(&self.doc, &root);
        }
        mgr.include_origin(LOCAL_ORIGIN);
        self.undo = Some(mgr);
    }

    pub fn undo_enabled(&self) -> bool {
        self.undo.is_some()
    }

    /// End the current undo group so the next edit starts a new one.
    pub fn undo_boundary(&mut self) {
        if let Some(m) = &mut self.undo {
            m.reset();
        }
    }

    pub fn undo(&mut self) -> Option<Vec<u8>> {
        let m = self.undo.as_mut()?;
        if !m.undo_blocking() {
            return None;
        }
        self.take_local_update()
    }

    pub fn redo(&mut self) -> Option<Vec<u8>> {
        let m = self.undo.as_mut()?;
        if !m.redo_blocking() {
            return None;
        }
        self.take_local_update()
    }

    /// A position that survives concurrent edits (for cursors).
    pub fn sticky(&self, index: usize, assoc_after: bool) -> Option<yrs::StickyIndex> {
        use yrs::IndexedSequence;
        let assoc = if assoc_after {
            yrs::Assoc::After
        } else {
            yrs::Assoc::Before
        };
        self.text
            .sticky_index(&self.doc.transact(), index as u32, assoc)
    }

    pub fn resolve_sticky(&self, s: &yrs::StickyIndex) -> Option<usize> {
        s.get_offset(&self.doc.transact()).map(|o| o.index as usize)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Note,
    Attachment,
    Folder,
    /// A drawing: its object holds elements, not text (see
    /// [`crate::drawing`]).
    Drawing,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Note => "note",
            EntryKind::Attachment => "attachment",
            EntryKind::Folder => "folder",
            EntryKind::Drawing => "drawing",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "note" => Some(Self::Note),
            "attachment" => Some(Self::Attachment),
            "folder" => Some(Self::Folder),
            "drawing" => Some(Self::Drawing),
            _ => None,
        }
    }
}

/// One file-tree entry of a vault.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub kind: EntryKind,
    pub created: i64,
    pub modified: i64,
    pub trashed: Option<i64>,
    /// When it was deleted for good (emptying the trash): the server keeps
    /// its data for a while, so this can still be undone (back to the
    /// trash) until then. Such entries show nowhere but in the trash's list
    /// of what was deleted for good; the entry goes once the server let go
    /// of the data.
    #[serde(default)]
    pub purged: Option<i64>,
    pub size: Option<u64>,
    pub mime: Option<String>,
    /// A note's or folder's icon, usually an emoji ([`clean_icon`]).
    #[serde(default)]
    pub icon: Option<String>,
    /// Its place among its siblings when put there by hand
    /// ([`order`](crate::order)).
    #[serde(default)]
    pub order: Option<String>,
}

/// Longest icon (of a vault, note or folder), in characters: one emoji can
/// be several.
pub const MAX_ICON_CHARS: usize = 16;

/// An icon as stored: trimmed, without control characters. `Ok(None)` for
/// a blank one; `Err` if longer than [`MAX_ICON_CHARS`].
pub fn clean_icon(icon: &str) -> std::result::Result<Option<String>, String> {
    let icon: String = icon.trim().chars().filter(|c| !c.is_control()).collect();
    if icon.chars().count() > MAX_ICON_CHARS {
        return Err("an icon is one emoji or a few characters".into());
    }
    Ok((!icon.is_empty()).then_some(icon))
}

impl Entry {
    /// A note's or drawing's name: its file name without `.md` or
    /// `.drawing`.
    pub fn title(&self) -> &str {
        match self.kind {
            EntryKind::Drawing => paths::drawing_title(&self.path),
            _ => paths::title(&self.path),
        }
    }

    pub fn is_live(&self) -> bool {
        self.trashed.is_none() && self.purged.is_none()
    }

    /// In the trash: trashed, not deleted for good.
    pub fn in_trash(&self) -> bool {
        self.trashed.is_some() && self.purged.is_none()
    }
}

/// The vault index document.
pub struct IndexDoc {
    doc: Doc,
    files: MapRef,
    local: LocalUpdates,
}

impl Default for IndexDoc {
    fn default() -> Self {
        Self::new()
    }
}

fn any_str(v: Option<Out>) -> Option<String> {
    match v? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn any_num(v: Option<Out>) -> Option<f64> {
    match v? {
        Out::Any(Any::Number(yrs::any::Number::Float(n))) => Some(n),
        Out::Any(Any::Number(yrs::any::Number::Int(n))) => Some(n as f64),
        _ => None,
    }
}

impl IndexDoc {
    pub fn new() -> Self {
        let doc = Doc::new();
        let files = doc.get_or_insert_map(INDEX_FILES);
        let local = LocalUpdates::default();
        local.attach(&doc);
        IndexDoc { doc, files, local }
    }

    pub fn from_updates<'a>(updates: impl IntoIterator<Item = &'a [u8]>) -> Result<Self> {
        let d = Self::new();
        for u in updates {
            d.apply_update(u)?;
        }
        Ok(d)
    }

    pub fn apply_update(&self, update: &[u8]) -> Result<()> {
        apply(&self.doc, update)
    }

    pub fn encode_state(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    pub fn take_local_update(&self) -> Option<Vec<u8>> {
        self.local.take()
    }

    fn read_entry<T: ReadTxn>(&self, txn: &T, id: &str, m: &MapRef) -> Option<Entry> {
        let path = any_str(m.get(txn, "path"))?;
        let kind = EntryKind::parse(&any_str(m.get(txn, "kind"))?)?;
        let created = any_num(m.get(txn, "created")).unwrap_or(0.0) as i64;
        Some(Entry {
            id: id.to_string(),
            path,
            kind,
            created,
            modified: any_num(m.get(txn, "modified"))
                .map(|n| n as i64)
                .unwrap_or(created),
            trashed: any_num(m.get(txn, "trashed")).map(|n| n as i64),
            purged: any_num(m.get(txn, "purged")).map(|n| n as i64),
            size: any_num(m.get(txn, "size")).map(|n| n as u64),
            mime: any_str(m.get(txn, "mime")),
            // Anyone who can write sets it: only a valid one is shown.
            icon: any_str(m.get(txn, "icon")).and_then(|i| clean_icon(&i).ok().flatten()),
            order: any_str(m.get(txn, "order")).filter(|k| order::valid_key(k)),
        })
    }

    /// All entries, including trashed ones, sorted by path.
    pub fn entries(&self) -> Vec<Entry> {
        let txn = self.doc.transact();
        let mut out: Vec<Entry> = self
            .files
            .iter(&txn)
            .filter_map(|(id, v)| match v {
                Out::YMap(m) => self.read_entry(&txn, id, &m),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.created.cmp(&b.created))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    pub fn get(&self, id: &str) -> Option<Entry> {
        let txn = self.doc.transact();
        match self.files.get(&txn, id)? {
            Out::YMap(m) => self.read_entry(&txn, id, &m),
            _ => None,
        }
    }

    /// Live entry at exactly `path`, preferring the oldest one if there are duplicates.
    pub fn find_by_path(&self, path: &str) -> Option<Entry> {
        self.entries()
            .into_iter()
            .find(|e| e.is_live() && e.path == path)
    }

    /// Unique display paths for live entries. Concurrent offline creation can
    /// produce two entries with the same path; the older keeps it and later ones
    /// get a numeric suffix. Nothing is renamed in the shared state, so this
    /// never causes edit wars between clients.
    pub fn resolved_paths(&self) -> BTreeMap<String, Entry> {
        let mut taken: HashMap<String, ()> = HashMap::new();
        let mut out = BTreeMap::new();
        let mut live: Vec<Entry> = self.entries().into_iter().filter(|e| e.is_live()).collect();
        live.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.created.cmp(&b.created))
                .then(a.id.cmp(&b.id))
        });
        for e in live {
            let p = paths::unique(&e.path, |p| taken.contains_key(p));
            taken.insert(p.clone(), ());
            out.insert(p, e);
        }
        out
    }

    fn mutate(&self, f: impl FnOnce(&mut yrs::TransactionMut)) -> Option<Vec<u8>> {
        {
            let mut txn = self.doc.transact_mut_with(LOCAL_ORIGIN);
            f(&mut txn);
        }
        self.take_local_update()
    }

    fn entry_map<T: ReadTxn>(&self, txn: &T, id: &str) -> Option<MapRef> {
        match self.files.get(txn, id)? {
            Out::YMap(m) => Some(m),
            _ => None,
        }
    }

    /// Insert a new entry. `now` is Unix milliseconds.
    pub fn create(&self, id: &str, path: &str, kind: EntryKind, now: i64) -> Option<Vec<u8>> {
        self.create_with(id, path, kind, now, None, None)
    }

    pub fn create_with(
        &self,
        id: &str,
        path: &str,
        kind: EntryKind,
        now: i64,
        size: Option<u64>,
        mime: Option<&str>,
    ) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            let mut fields: HashMap<String, Any> = HashMap::new();
            fields.insert("path".into(), Any::from(path));
            fields.insert("kind".into(), Any::from(kind.as_str()));
            fields.insert("created".into(), Any::from(now as f64));
            fields.insert("modified".into(), Any::from(now as f64));
            if let Some(s) = size {
                fields.insert("size".into(), Any::from(s as f64));
            }
            if let Some(m) = mime {
                fields.insert("mime".into(), Any::from(m));
            }
            let prelim: MapPrelim = fields
                .into_iter()
                .map(|(k, v)| (k, yrs::In::from(v)))
                .collect();
            self.files.insert(txn, id, prelim);
        })
    }

    /// Rename or move an entry. An order key only means something among
    /// siblings, so one moved to another folder loses it.
    pub fn rename(&self, id: &str, new_path: &str, now: i64) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id) {
                let old = any_str(m.get(txn, "path")).unwrap_or_default();
                if paths::parent(&old) != paths::parent(new_path) {
                    m.remove(txn, "order");
                }
                m.try_update(txn, "path", new_path);
                m.try_update(txn, "modified", now as f64);
            }
        })
    }

    /// Every entry as `(id, path, live)` — also those of kinds this release
    /// doesn't know (a newer one added them): left out of
    /// [`entries`](Self::entries), they still hold their folders, move and go
    /// to the trash with them.
    fn all_paths(&self) -> Vec<(String, String, bool)> {
        let txn = self.doc.transact();
        self.files
            .iter(&txn)
            .filter_map(|(id, v)| {
                let Out::YMap(m) = v else { return None };
                let path = any_str(m.get(&txn, "path"))?;
                let live = m.get(&txn, "trashed").is_none() && m.get(&txn, "purged").is_none();
                Some((id.to_string(), path, live))
            })
            .collect()
    }

    /// [`all_paths`](Self::all_paths) at `folder` or in it.
    fn within(&self, folder: &str) -> Vec<(String, String, bool)> {
        self.all_paths()
            .into_iter()
            .filter(|(_, path, _)| path == folder || paths::is_within(path, folder))
            .collect()
    }

    /// An entry of a kind a newer release added, as that release writes it.
    #[cfg(test)]
    pub(crate) fn create_unknown_kind(&self, id: &str, path: &str) {
        self.mutate(|txn| {
            let prelim: MapPrelim = [
                ("path".to_string(), yrs::In::from(Any::from(path))),
                ("kind".to_string(), yrs::In::from(Any::from("board"))),
                ("created".to_string(), yrs::In::from(Any::from(1.0))),
            ]
            .into_iter()
            .collect();
            self.files.insert(txn, id, prelim);
        });
    }

    /// Whether `folder` holds entries of kinds this release doesn't know
    /// (see [`within`](Self::within)).
    pub fn holds_unknown(&self, folder: &str) -> bool {
        let known: HashSet<String> = self.entries().into_iter().map(|e| e.id).collect();
        self.within(folder)
            .into_iter()
            .any(|(id, _, live)| live && !known.contains(&id))
    }

    /// Rename a folder: moves every entry beneath it (and the folder entry
    /// itself, which loses its order key if it moves to another folder).
    pub fn rename_folder(&self, old_folder: &str, new_folder: &str, now: i64) -> Option<Vec<u8>> {
        let moves: Vec<(String, String)> = self
            .within(old_folder)
            .into_iter()
            .map(|(id, path, _)| {
                let rest = &path[old_folder.len()..];
                (id, format!("{new_folder}{rest}"))
            })
            .collect();
        if moves.is_empty() {
            return None;
        }
        self.mutate(|txn| {
            let moved_away = paths::parent(old_folder) != paths::parent(new_folder);
            for (id, p) in &moves {
                if let Some(m) = self.entry_map(txn, id) {
                    if moved_away && p == new_folder {
                        m.remove(txn, "order");
                    }
                    m.try_update(txn, "path", p.as_str());
                    m.try_update(txn, "modified", now as f64);
                }
            }
        })
    }

    /// Record that an entry's content changed (drives "recent" sorting).
    pub fn touch(&self, id: &str, now: i64, size: Option<u64>) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id) {
                m.try_update(txn, "modified", now as f64);
                if let Some(s) = size {
                    m.try_update(txn, "size", s as f64);
                }
            }
        })
    }

    /// Soft delete; recoverable from the trash.
    pub fn trash(&self, id: &str, now: i64) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id) {
                m.try_update(txn, "trashed", now as f64);
            }
        })
    }

    /// Trash a folder and everything in it.
    pub fn trash_folder(&self, folder: &str, now: i64) -> Option<Vec<u8>> {
        let ids: Vec<String> = self
            .within(folder)
            .into_iter()
            .filter(|(_, _, live)| *live)
            .map(|(id, _, _)| id)
            .collect();
        self.trash_all(ids.iter().map(String::as_str), now)
    }

    /// Trash several entries in one change.
    pub fn trash_all<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
        now: i64,
    ) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            for id in ids {
                if let Some(m) = self.entry_map(txn, id) {
                    m.try_update(txn, "trashed", now as f64);
                }
            }
        })
    }

    /// Set an entry's icon (cleaned with [`clean_icon`] first), or remove it
    /// with `None`. Not a change of its content: `modified` stays.
    pub fn set_icon(&self, id: &str, icon: Option<&str>) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id) {
                match icon {
                    Some(i) => {
                        m.try_update(txn, "icon", i);
                    }
                    None => {
                        m.remove(txn, "icon");
                    }
                }
            }
        })
    }

    /// Set or remove (`None`) the order keys of entries ([`order`]); invalid
    /// keys are skipped. Not a change of their content: `modified` stays.
    pub fn set_order<'a>(
        &self,
        keys: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            for (id, key) in keys {
                let Some(m) = self.entry_map(txn, id) else {
                    continue;
                };
                match key {
                    Some(k) if order::valid_key(k) => {
                        m.try_update(txn, "order", k);
                    }
                    Some(_) => {}
                    None => {
                        m.remove(txn, "order");
                    }
                }
            }
        })
    }

    /// Take an entry out of the trash. Not one deleted for good: its data is
    /// gone from the server, or held there only for [`unmark_purged`].
    ///
    /// [`unmark_purged`]: Self::unmark_purged
    pub fn restore(&self, id: &str) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id)
                && m.get(txn, "purged").is_none()
            {
                m.remove(txn, "trashed");
            }
        })
    }

    /// Mark trashed entries deleted for good (their data was purged on the
    /// server, which keeps it a while, see [`Entry::purged`]). `now` is Unix
    /// milliseconds.
    pub fn mark_purged<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
        now: i64,
    ) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            for id in ids {
                if let Some(m) = self.entry_map(txn, id) {
                    if m.get(txn, "trashed").is_none() {
                        m.try_update(txn, "trashed", now as f64);
                    }
                    m.try_update(txn, "purged", now as f64);
                }
            }
        })
    }

    /// Undoing a deletion for good: the entry is back in the trash.
    pub fn unmark_purged(&self, id: &str) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            if let Some(m) = self.entry_map(txn, id) {
                m.remove(txn, "purged");
            }
        })
    }

    /// Permanently remove an entry from the index (after its data was purged).
    pub fn remove(&self, id: &str) -> Option<Vec<u8>> {
        self.mutate(|txn| {
            self.files.remove(txn, id);
        })
    }

    /// Folders: explicit folder entries plus every parent of a live entry
    /// (of any kind, see [`all_paths`](Self::all_paths)).
    pub fn folders(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for e in self.entries().into_iter().filter(|e| e.is_live()) {
            if e.kind == EntryKind::Folder {
                set.insert(e.path.clone());
            }
        }
        for (_, path, _) in self.all_paths().into_iter().filter(|(_, _, live)| *live) {
            let mut p = paths::parent(&path);
            while !p.is_empty() {
                set.insert(p.to_string());
                p = paths::parent(p);
            }
        }
        set.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crafted_updates_found_by_fuzzing_do_not_panic() {
        // Divided by zero in yrs's block lookup while committing (patched in
        // vendor/yrs).
        let crash: &[u8] = &[
            1, 6, 0, 0, 50, 1, 0, 0, 0, 0, 0, 0, 4, 0, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 253,
        ];
        let _ = NoteDoc::new().apply_update(crash);
        let _ = IndexDoc::new().apply_update(crash);
        // Spliced a second run of blocks into one client's list; panicked
        // while encoding the commit (refused while decoding now).
        let crash: &[u8] = &[
            3, 3, 3, 45, 0, 1, 1, 1, 1, 1, 1, 1, 0, 3, 0, 23, 3, 1, 255, 255, 255, 255, 255, 255,
            255, 147, 3, 3, 1, 3, 3, 1, 1, 1, 1, 1, 1, 1, 0, 3, 0, 0, 2, 0, 23, 3, 129, 3, 3, 1, 3,
            3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 17, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let _ = NoteDoc::new().apply_update(crash);
        let _ = IndexDoc::new().apply_update(crash);
        // A map value declaring a huge entry count made yrs reserve ~2 GB.
        let bomb: &[u8] = &[
            1, 247, 95, 44, 90, 72, 25, 9, 165, 255, 44, 118, 255, 255, 255, 255, 5, 255, 255, 255,
            147, 25, 9, 165, 254, 255,
        ];
        let t = std::time::Instant::now();
        let _ = NoteDoc::new().apply_update(bomb);
        assert!(t.elapsed() < std::time::Duration::from_millis(50));
        // Unwrapped a failed lookup when filling a gap (patched in vendor/yrs).
        let crash: &[u8] = &[
            3, 3, 3, 4, 0, 0, 0, 0, 0, 0, 0, 44, 187, 187, 187, 187, 187, 187, 187, 187, 187, 3, 3,
            3, 1, 0, 24, 0, 0, 0, 0, 0, 0,
        ];
        let _ = NoteDoc::new().apply_update(crash);
        let _ = IndexDoc::new().apply_update(crash);
    }

    #[test]
    fn an_update_claiming_a_billion_clients_is_refused_at_once() {
        // Found by fuzzing: this made yrs reserve ~2 GB before failing.
        let bomb: &[u8] = &[0x83, 0x83, 0x83, 0xff, 0x83, 0x2d, 0xff];
        let t = std::time::Instant::now();
        assert!(NoteDoc::new().apply_update(bomb).is_err());
        assert!(IndexDoc::new().apply_update(bomb).is_err());
        assert!(t.elapsed() < std::time::Duration::from_millis(50));
        // Real updates still apply.
        let n = NoteDoc::new();
        let u = n.set_text("hello").unwrap();
        let m = NoteDoc::new();
        m.apply_update(&u).unwrap();
        assert_eq!(m.text(), "hello");
        let empty = NoteDoc::new().encode_state();
        NoteDoc::new().apply_update(&empty).unwrap();
    }

    #[test]
    fn note_concurrent_edits_merge() {
        let a = NoteDoc::new();
        let u0 = a.set_text("Hello world").unwrap();
        let b = NoteDoc::from_updates([u0.as_slice()]).unwrap();
        let ua = a.set_text("Hello brave world").unwrap();
        let ub = b.set_text("Hello world!").unwrap();
        a.apply_update(&ub).unwrap();
        b.apply_update(&ua).unwrap();
        assert_eq!(a.text(), "Hello brave world!");
        assert_eq!(a.text(), b.text());
        // Replaying is harmless.
        a.apply_update(&ua).unwrap();
        a.apply_update(&u0).unwrap();
        assert_eq!(a.text(), "Hello brave world!");
        // Snapshot reproduces state.
        let c = NoteDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        assert_eq!(c.text(), a.text());
    }

    #[test]
    fn remote_updates_are_not_reported_as_local() {
        let a = NoteDoc::new();
        let u = a.set_text("x").unwrap();
        let b = NoteDoc::new();
        b.apply_update(&u).unwrap();
        assert!(b.take_local_update().is_none());
    }

    #[test]
    fn undo_redo() {
        let mut a = NoteDoc::new();
        a.enable_undo();
        a.set_text("one");
        a.undo_boundary();
        a.set_text("one two");
        assert!(a.undo().is_some());
        assert_eq!(a.text(), "one");
        assert!(a.redo().is_some());
        assert_eq!(a.text(), "one two");
    }

    #[test]
    fn encoded_state_never_contains_deleted_text() {
        let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        let mut a = NoteDoc::new();
        a.enable_undo();
        a.set_text("hello SECRETPASSWORD world");
        a.undo_boundary();
        a.set_text("hello world");
        let state = a.encode_state();
        assert!(!contains(&state, b"SECRETPASSWORD"));
        // The state is still complete: it rebuilds the text and merges with
        // replicas that saw the deletion.
        let b = NoteDoc::from_updates([state.as_slice()]).unwrap();
        assert_eq!(b.text(), "hello world");
        let c = NoteDoc::new();
        c.apply_update(&a.encode_state()).unwrap();
        c.set_text("hello world!");
        a.apply_update(&c.encode_state()).unwrap();
        assert_eq!(a.text(), "hello world!");
        // Undo still restores the deleted text locally.
        assert!(a.undo().is_some());
    }

    #[test]
    fn entry_icons() {
        let a = IndexDoc::new();
        let u = a.create("n", "Trip.md", EntryKind::Note, 5).unwrap();
        let b = IndexDoc::from_updates([u.as_slice()]).unwrap();
        assert_eq!(a.get("n").unwrap().icon, None);
        let u = a.set_icon("n", Some("🏖️")).unwrap();
        b.apply_update(&u).unwrap();
        let e = b.get("n").unwrap();
        assert_eq!((e.icon.as_deref(), e.modified), (Some("🏖️"), 5));
        let u = b.set_icon("n", None).unwrap();
        a.apply_update(&u).unwrap();
        assert_eq!(a.get("n").unwrap().icon, None);
        // One written by another client without the rules isn't shown.
        let u = a
            .set_icon("n", Some(&"x".repeat(MAX_ICON_CHARS + 1)))
            .unwrap();
        b.apply_update(&u).unwrap();
        assert_eq!(b.get("n").unwrap().icon, None);
        assert_eq!(clean_icon(" 📁\n").unwrap().as_deref(), Some("📁"));
        assert_eq!(clean_icon("  ").unwrap(), None);
        assert!(clean_icon(&"x".repeat(MAX_ICON_CHARS + 1)).is_err());
    }

    #[test]
    fn entry_order() {
        let a = IndexDoc::new();
        a.create("f", "Trips", EntryKind::Folder, 1);
        a.create("n", "Trips/Rome.md", EntryKind::Note, 1);
        a.create("m", "Trips/Oslo.md", EntryKind::Note, 1);
        let b = IndexDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        let u = a
            .set_order([("n", Some("V")), ("m", Some("k")), ("f", Some("G"))])
            .unwrap();
        b.apply_update(&u).unwrap();
        let e = b.get("n").unwrap();
        assert_eq!((e.order.as_deref(), e.modified), (Some("V"), 1));
        // A rename within the folder keeps it, a move elsewhere drops it.
        b.rename("n", "Trips/Roma.md", 2);
        assert_eq!(b.get("n").unwrap().order.as_deref(), Some("V"));
        b.rename("n", "Roma.md", 3);
        assert_eq!(b.get("n").unwrap().order, None);
        // Moving a folder: its own key goes, those of its contents stay.
        b.rename_folder("Trips", "Archive/Trips", 4);
        assert_eq!(b.get("f").unwrap().order, None);
        assert_eq!(b.get("m").unwrap().order.as_deref(), Some("k"));
        // Removing it; invalid keys are neither written nor shown.
        b.set_order([("m", None), ("f", Some("a0"))]);
        assert_eq!(
            (b.get("m").unwrap().order, b.get("f").unwrap().order),
            (None, None)
        );
        {
            let mut txn = a.doc.transact_mut();
            let m = a.entry_map(&txn, "m").unwrap();
            m.insert(&mut txn, "order", "not a key");
        }
        assert_eq!(a.get("m").unwrap().order, None);
    }

    #[test]
    fn index_ops_merge() {
        let a = IndexDoc::new();
        let u = a.create("n1", "Inbox/Idea.md", EntryKind::Note, 1).unwrap();
        let b = IndexDoc::from_updates([u.as_slice()]).unwrap();
        // Concurrent rename on A and trash on B both apply.
        let ra = a.rename("n1", "Projects/Idea.md", 2).unwrap();
        let tb = b.trash("n1", 3).unwrap();
        a.apply_update(&tb).unwrap();
        b.apply_update(&ra).unwrap();
        for d in [&a, &b] {
            let e = d.get("n1").unwrap();
            assert_eq!(e.path, "Projects/Idea.md");
            assert_eq!(e.trashed, Some(3));
        }
        let r = a.restore("n1").unwrap();
        b.apply_update(&r).unwrap();
        assert!(b.get("n1").unwrap().is_live());
        assert_eq!(b.folders(), vec!["Projects".to_string()]);
    }

    #[test]
    fn deleted_for_good_leaves_the_trash_until_undone() {
        let a = IndexDoc::new();
        a.create("n", "Old.md", EntryKind::Note, 1);
        a.create("m", "Other.md", EntryKind::Note, 1);
        a.trash("n", 2);
        let u = a.mark_purged(["n", "m"], 3).unwrap();
        let b = IndexDoc::from_updates([a.encode_state().as_slice(), u.as_slice()]).unwrap();
        for d in [&a, &b] {
            let e = d.get("n").unwrap();
            assert_eq!((e.trashed, e.purged), (Some(2), Some(3)));
            assert!(!e.is_live() && !e.in_trash());
            // Deleted for good straight away (never trashed): trashed too.
            let m = d.get("m").unwrap();
            assert_eq!((m.trashed, m.purged), (Some(3), Some(3)));
        }
        // Can't come out of the trash while deleted for good.
        assert!(a.restore("n").is_none());
        assert!(!a.get("n").unwrap().is_live());
        assert!(a.find_by_path("Old.md").is_none());
        // Undone: back in the trash, from where it can be restored.
        b.apply_update(&a.unmark_purged("n").unwrap()).unwrap();
        let e = b.get("n").unwrap();
        assert!(e.in_trash() && e.purged.is_none());
        b.restore("n").unwrap();
        assert!(b.get("n").unwrap().is_live());
    }

    #[test]
    fn duplicate_paths_resolve_deterministically() {
        let a = IndexDoc::new();
        let b = IndexDoc::new();
        let ua = a.create("x", "Note.md", EntryKind::Note, 10).unwrap();
        let ub = b.create("y", "Note.md", EntryKind::Note, 20).unwrap();
        a.apply_update(&ub).unwrap();
        b.apply_update(&ua).unwrap();
        let ra = a.resolved_paths();
        let rb = b.resolved_paths();
        assert_eq!(ra.get("Note.md").unwrap().id, "x");
        assert_eq!(ra.get("Note 2.md").unwrap().id, "y");
        assert_eq!(ra.keys().collect::<Vec<_>>(), rb.keys().collect::<Vec<_>>());
    }

    #[test]
    fn kinds_from_newer_releases_stay_hidden_but_go_with_their_folder() {
        let a = IndexDoc::new();
        a.create("f", "A", EntryKind::Folder, 1);
        a.create("n", "A/c.md", EntryKind::Note, 1);
        a.create_unknown_kind("b", "A/plan");
        a.create_unknown_kind("solo", "Only/board");
        assert_eq!(
            a.entries()
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["f", "n"]
        );
        assert!(a.get("b").is_none());
        assert!(a.holds_unknown("A") && !a.holds_unknown("B"));
        // A folder holding only such an entry is still a folder.
        assert!(a.folders().contains(&"Only".to_string()));

        let raw = |id: &str| {
            a.all_paths()
                .into_iter()
                .find(|(i, _, _)| i == id)
                .map(|(_, p, live)| (p, live))
                .unwrap()
        };
        a.rename_folder("A", "Z", 2).unwrap();
        assert_eq!(raw("b"), ("Z/plan".to_string(), true));
        assert_eq!(a.get("n").unwrap().path, "Z/c.md");
        a.trash_folder("Z", 3).unwrap();
        assert_eq!(raw("b"), ("Z/plan".to_string(), false));
        assert!(a.get("n").unwrap().in_trash());
        assert!(!a.holds_unknown("Z"));
    }

    #[test]
    fn folder_rename() {
        let a = IndexDoc::new();
        a.create("f", "A", EntryKind::Folder, 1);
        a.create("n", "A/B/c.md", EntryKind::Note, 1);
        a.create("m", "AB/d.md", EntryKind::Note, 1);
        a.rename_folder("A", "Z", 2).unwrap();
        assert_eq!(a.get("f").unwrap().path, "Z");
        assert_eq!(a.get("n").unwrap().path, "Z/B/c.md");
        assert_eq!(a.get("m").unwrap().path, "AB/d.md");
    }
}
