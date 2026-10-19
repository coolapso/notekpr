//! Encrypted local copy of each vault in IndexedDB, so a page load shows the
//! notes this browser already had right away and only fetches what changed
//! since, instead of downloading and replaying the whole vault every time.
//!
//! Every stored object is a note's (or the vault index's) full CRDT state,
//! encrypted with the key its server copy is encrypted with (purpose
//! `LocalCache`, bound to vault and object). The copy is therefore no more
//! readable than the server's: without the vault keys, which this browser only
//! has after unlocking the account, it is noise.
//!
//! The stored copy is complete up to its cursor (`Meta::seq`); a restored
//! vault subscribes from there. The cursor only moves when everything changed
//! in memory has been written, and never past content still waiting for a key.
//! Notes with changes the server hasn't confirmed yet are not written: those
//! changes live in local storage until sent (see `storage::pending`), and a
//! stored state could outlive them there and never reach the server.
//!
//! One tab per account writes, for as long as it is open (a Web Lock): a tab
//! that is behind could otherwise overwrite a note with an older state while
//! another tab's cursor already counts the newer one. Others only read. The
//! meta record also counts the stored objects, so a copy that lost some of
//! them (deleted by another tab while this one wrote) is not trusted.
//!
//! Layout (one object store, out-of-line keys):
//! * `{user}\u{1}{vault}` → encrypted [`Meta`]
//! * `{user}\u{1}{vault}\u{1}{object}` → encrypted state

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use coolpsonotes_core::crypto::Purpose;
use coolpsonotes_core::doc::{IndexDoc, NoteDoc};
use coolpsonotes_core::engine::{Applied, Deferred, VaultReplica};
use coolpsonotes_core::protocol::INDEX_OBJECT_ID;
use js_sys::{Array, Function, Promise, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbKeyRange, IdbRequest, IdbTransaction, IdbTransactionMode};

use crate::state::with;
use crate::util;

const DB_NAME: &str = "coolpsonotes";
const DB_VERSION: u32 = 1;
const STORE: &str = "cache";
/// Bump to discard copies written in an older layout.
const FORMAT: u32 = 1;
/// Object id the meta record is encrypted under (not a valid note id).
const META_OBJECT: &str = "~cache";
const SEP: char = '\u{1}';
const SAVE_DELAY_MS: i32 = 1500;

#[derive(Serialize, Deserialize)]
struct Meta {
    format: u32,
    /// The stored objects contain every change up to this sequence number.
    seq: i64,
    /// Number of stored objects.
    objects: usize,
    since_snapshot: HashMap<String, u32>,
}

/// What is known about the stored copies, kept in `Core`.
#[derive(Default)]
pub struct Tracker {
    /// Objects changed in memory since they were last written, by vault.
    dirty: BTreeMap<String, BTreeSet<String>>,
    /// Objects deleted for good since the last write, by vault.
    removed: BTreeMap<String, BTreeSet<String>>,
    /// Cursor of each vault's stored copy.
    cursor: HashMap<String, i64>,
    /// Objects of each vault's stored copy (known once this tab writes).
    stored: HashMap<String, BTreeSet<String>>,
    listed: bool,
    /// Vaults not cached this session (writing failed).
    off: HashSet<String>,
    scheduled: bool,
    saving: bool,
    again: bool,
    pruned: bool,
}

/// A vault's stored copy as read from IndexedDB, still encrypted.
pub struct Stored {
    meta: Vec<u8>,
    objects: Vec<(String, Vec<u8>)>,
}

/// One vault's write.
pub struct Job {
    vault: String,
    meta: Vec<u8>,
    puts: Vec<(String, Vec<u8>)>,
    deletes: Vec<String>,
    cursor: i64,
}

// ------------------------------------------------------------------ replica <-> stored copy

/// Highest sequence number the replica's documents are complete up to:
/// content waiting for a key is counted by `seq` but not in the documents.
fn complete_up_to(r: &VaultReplica) -> i64 {
    r.deferred
        .iter()
        .map(|d| match d {
            Deferred::Update(u) => u.seq - 1,
            Deferred::Snapshot(s) => s.covers_seq - 1,
        })
        .fold(r.seq, i64::min)
        .max(0)
}

/// Build the write of a vault's changed objects; `dirty` keeps what couldn't
/// be written (unconfirmed local changes, key unavailable), and while anything
/// is kept the cursor stays where it was (`cursor`, `None` if nothing is
/// stored yet). `stored` (the objects stored) is updated for the write. `None`
/// when there is nothing to write.
fn prepare(
    r: &VaultReplica,
    dirty: &mut BTreeSet<String>,
    removed: BTreeSet<String>,
    cursor: Option<i64>,
    stored: &mut BTreeSet<String>,
) -> Option<Job> {
    let mut puts = Vec::new();
    let mut kept = BTreeSet::new();
    for o in std::mem::take(dirty) {
        if removed.contains(&o) {
            continue;
        }
        let state = if o == INDEX_OBJECT_ID {
            r.index.encode_state()
        } else {
            match r.notes.get(&o) {
                Some(n) => n.encode_state(),
                None => continue,
            }
        };
        let sealed = r.encrypt_object(Purpose::LocalCache, &o, &state);
        match sealed {
            Ok(data) if !r.pending.iter().any(|p| p.object_id == o) => puts.push((o, data)),
            _ => {
                kept.insert(o);
            }
        }
    }
    *dirty = kept;
    let seq = if dirty.is_empty() {
        complete_up_to(r)
    } else {
        cursor.unwrap_or(0)
    };
    if puts.is_empty() && removed.is_empty() && cursor == Some(seq) {
        return None;
    }
    stored.extend(puts.iter().map(|(o, _)| o.clone()));
    stored.retain(|o| !removed.contains(o));
    let meta = Meta {
        format: FORMAT,
        seq,
        objects: stored.len(),
        since_snapshot: r.since_snapshot.clone(),
    };
    let meta = serde_json::to_vec(&meta).ok()?;
    let meta = r
        .encrypt_object(Purpose::LocalCache, META_OBJECT, &meta)
        .ok()?;
    Some(Job {
        vault: r.id.clone(),
        meta,
        puts,
        deletes: removed.into_iter().collect(),
        cursor: seq,
    })
}

/// Fill a new, empty replica from its stored copy and return the cursor.
/// Nothing is changed if any part can't be read.
fn restore(r: &mut VaultReplica, stored: Stored) -> Result<i64, String> {
    let meta = r
        .decrypt_object(Purpose::LocalCache, META_OBJECT, &stored.meta)
        .map_err(|e| e.to_string())?;
    let meta: Meta = serde_json::from_slice(&meta).map_err(|e| e.to_string())?;
    if meta.format != FORMAT {
        return Err("stored in an older format".into());
    }
    if meta.objects != stored.objects.len() {
        return Err("incomplete".into());
    }
    let index = IndexDoc::new();
    let mut notes = HashMap::new();
    for (o, data) in stored.objects {
        let state = r
            .decrypt_object(Purpose::LocalCache, &o, &data)
            .map_err(|e| e.to_string())?;
        if o == INDEX_OBJECT_ID {
            index.apply_update(&state).map_err(|e| e.to_string())?;
        } else {
            let n = NoteDoc::new();
            n.apply_update(&state).map_err(|e| e.to_string())?;
            notes.insert(o, n);
        }
    }
    r.index = index;
    r.notes = notes;
    r.seq = meta.seq;
    r.since_snapshot = meta.since_snapshot;
    Ok(meta.seq)
}

// ------------------------------------------------------------------ session glue

/// Read a vault's stored copy (before creating its replica).
pub async fn fetch(user: &str, vault: &str) -> Option<Stored> {
    match load(user, vault).await {
        Ok(s) => s,
        Err(e) => {
            util::warn(&format!("reading the local copy failed: {e}"));
            None
        }
    }
}

/// Fill the replica just created for `vault` (inside `with`). Returns whether
/// it now has content to show; an unreadable copy is deleted.
pub fn restore_into(
    tracker: &mut Tracker,
    user: &str,
    r: &mut VaultReplica,
    stored: Option<Stored>,
) -> bool {
    let Some(stored) = stored else { return false };
    let has_content = !stored.objects.is_empty();
    match restore(r, stored) {
        Ok(cursor) => {
            util::log(&format!(
                "vault {}: {} notes from this browser's copy, catching up from {cursor}",
                r.id,
                r.notes.len()
            ));
            tracker.cursor.insert(r.id.clone(), cursor);
            has_content
        }
        Err(e) => {
            util::warn(&format!("discarding the local copy of a vault: {e}"));
            spawn_remove(user, &r.id);
            false
        }
    }
}

/// Remember changed content of a vault for the next write.
pub fn note_applied(vault: &str, applied: &Applied) {
    with(|c| {
        let t = &mut c.cache;
        let dirty = t.dirty.entry(vault.to_string()).or_default();
        dirty.extend(applied.objects.iter().cloned());
        if applied.index_changed {
            dirty.insert(INDEX_OBJECT_ID.to_string());
        }
        if !applied.purged.is_empty() {
            t.removed
                .entry(vault.to_string())
                .or_default()
                .extend(applied.purged.iter().cloned());
        }
    });
    schedule();
}

pub fn note_local(vault: &str, object: &str) {
    with(|c| {
        c.cache
            .dirty
            .entry(vault.to_string())
            .or_default()
            .insert(object.to_string())
    });
    schedule();
}

/// Write soon (sync progress or confirmations may let the cursor move).
pub fn schedule() {
    if with(|c| std::mem::replace(&mut c.cache.scheduled, true)) {
        return;
    }
    util::set_timeout(SAVE_DELAY_MS, || leptos::task::spawn_local(save_now()));
}

async fn save_now() {
    let (uid, listed) = with(|c| {
        c.cache.scheduled = false;
        if c.cache.saving {
            c.cache.again = true;
            return (String::new(), false);
        }
        (c.user_id.clone(), c.cache.listed)
    });
    // Changes stay marked until this tab may write.
    if uid.is_empty() || !writer(&uid) {
        return;
    }
    if !listed {
        match stored_objects(&uid).await {
            Ok(m) => with(|c| {
                if c.user_id == uid {
                    c.cache.stored = m;
                    c.cache.listed = true;
                }
            }),
            Err(e) => {
                util::warn(&format!("listing local copies failed: {e}"));
                return;
            }
        }
    }
    let jobs = with(|c| {
        if c.user_id != uid || c.cache.saving || !c.cache.listed {
            return vec![];
        }
        let mut jobs = Vec::new();
        let t = &mut c.cache;
        for (id, r) in &c.replicas {
            if !r.member || t.off.contains(id) {
                continue;
            }
            let dirty = t.dirty.entry(id.clone()).or_default();
            let removed = t.removed.remove(id).unwrap_or_default();
            let stored = t.stored.entry(id.clone()).or_default();
            if let Some(job) = prepare(r, dirty, removed, t.cursor.get(id).copied(), stored) {
                jobs.push(job);
            }
        }
        t.saving = !jobs.is_empty();
        jobs
    });
    if jobs.is_empty() {
        return;
    }
    for job in jobs {
        // Not for a vault dropped meanwhile (its copy is being deleted).
        if !with(|c| c.user_id == uid && c.replicas.contains_key(&job.vault)) {
            continue;
        }
        match save(&uid, &job).await {
            Ok(()) => {
                with(|c| c.cache.cursor.insert(job.vault.clone(), job.cursor));
            }
            Err(e) => {
                util::warn(&format!("writing the local copy failed: {e}"));
                with(|c| {
                    c.cache.off.insert(job.vault.clone());
                    c.cache.cursor.remove(&job.vault);
                    c.cache.stored.remove(&job.vault);
                });
                spawn_remove(&uid, &job.vault);
            }
        }
    }
    let again = with(|c| {
        c.cache.saving = false;
        std::mem::take(&mut c.cache.again)
    });
    if again {
        schedule();
    }
}

/// Forget a vault's copy (vault deleted, access lost).
pub fn forget_vault(vault: &str) {
    let uid = with(|c| {
        c.cache.dirty.remove(vault);
        c.cache.removed.remove(vault);
        c.cache.cursor.remove(vault);
        c.cache.stored.remove(vault);
        c.user_id.clone()
    });
    spawn_remove(&uid, vault);
}

/// Delete copies of vaults this account can no longer open (access lost while
/// this browser was away). `vaults` is the server's current list.
pub fn prune(vaults: &BTreeSet<String>) {
    let (uid, first) = with(|c| {
        (
            c.user_id.clone(),
            !std::mem::replace(&mut c.cache.pruned, true),
        )
    });
    if !first {
        return;
    }
    let keep = vaults.clone();
    leptos::task::spawn_local(async move {
        match stored_objects(&uid).await {
            Ok(stored) => {
                for v in stored.keys().filter(|v| !keep.contains(*v)) {
                    spawn_remove(&uid, v);
                }
            }
            Err(e) => util::warn(&format!("listing local copies failed: {e}")),
        }
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Lock {
    Asking,
    Held,
    Busy,
}

thread_local! {
    static LOCKS: std::cell::RefCell<HashMap<String, Lock>> = std::cell::RefCell::new(HashMap::new());
}

/// Whether this tab writes `user`'s copies. The first tab to ask holds the
/// lock until it closes; the others ask again on their next write.
fn writer(user: &str) -> bool {
    let set = |l: Lock| LOCKS.with(|m| m.borrow_mut().insert(user.to_string(), l));
    match LOCKS.with(|m| m.borrow().get(user).copied()) {
        Some(Lock::Held) => return true,
        Some(Lock::Asking) => return false,
        Some(Lock::Busy) | None => {}
    }
    let locks = js_sys::Reflect::get(&util::window().navigator(), &"locks".into())
        .ok()
        .filter(|l| l.is_object());
    let request = locks.as_ref().and_then(|l| {
        js_sys::Reflect::get(l, &"request".into())
            .ok()?
            .dyn_into::<Function>()
            .ok()
    });
    let (Some(locks), Some(request)) = (locks, request) else {
        // No Web Locks in this browser: assume a single tab.
        set(Lock::Held);
        return true;
    };
    set(Lock::Asking);
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"ifAvailable".into(), &JsValue::TRUE);
    let u = user.to_string();
    let granted = Closure::once_into_js(move |lock: JsValue| -> JsValue {
        let held = !lock.is_null();
        LOCKS.with(|m| {
            m.borrow_mut()
                .insert(u, if held { Lock::Held } else { Lock::Busy })
        });
        if !held {
            return JsValue::UNDEFINED;
        }
        schedule();
        // Never settles: the lock is held until the page goes away.
        Promise::new(&mut |_, _| {}).into()
    });
    let name = JsValue::from_str(&format!("coolpsonotes-cache-{user}"));
    if request.call3(&locks, &name, &opts, &granted).is_err() {
        set(Lock::Held);
        return true;
    }
    false
}

fn spawn_remove(user: &str, vault: &str) {
    let (user, vault) = (user.to_string(), vault.to_string());
    leptos::task::spawn_local(async move {
        if let Err(e) = remove_vault(&user, &vault).await {
            util::warn(&format!("deleting a local copy failed: {e}"));
        }
    });
}

// ------------------------------------------------------------------ IndexedDB

thread_local! {
    static DB: std::cell::RefCell<Option<IdbDatabase>> = const { std::cell::RefCell::new(None) };
}

fn err(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

/// A promise and its resolve / reject functions.
fn deferred() -> (Promise, Function, Function) {
    let mut slot = None;
    let p = Promise::new(&mut |res, rej| slot = Some((res, rej)));
    let (res, rej) = slot.expect("the executor runs synchronously");
    (p, res, rej)
}

async fn wait(req: &IdbRequest) -> Result<JsValue, String> {
    let (p, res, rej) = deferred();
    let r = req.clone();
    let ok = Closure::<dyn FnMut()>::new(move || {
        let _ = res.call1(&JsValue::NULL, &r.result().unwrap_or(JsValue::UNDEFINED));
    });
    let r = req.clone();
    let fail = Closure::<dyn FnMut()>::new(move || {
        let e = r
            .error()
            .ok()
            .flatten()
            .map(JsValue::from)
            .unwrap_or(JsValue::NULL);
        let _ = rej.call1(&JsValue::NULL, &e);
    });
    req.set_onsuccess(Some(ok.as_ref().unchecked_ref()));
    req.set_onerror(Some(fail.as_ref().unchecked_ref()));
    let out = JsFuture::from(p).await;
    req.set_onsuccess(None);
    req.set_onerror(None);
    out.map_err(err)
}

/// Wait until a write transaction is committed.
async fn committed(tx: &IdbTransaction) -> Result<(), String> {
    let (p, res, rej) = deferred();
    let ok = Closure::<dyn FnMut()>::new(move || {
        let _ = res.call0(&JsValue::NULL);
    });
    let t = tx.clone();
    let fail = Closure::<dyn FnMut()>::new(move || {
        let e = t.error().map(JsValue::from).unwrap_or(JsValue::NULL);
        let _ = rej.call1(&JsValue::NULL, &e);
    });
    tx.set_oncomplete(Some(ok.as_ref().unchecked_ref()));
    tx.set_onerror(Some(fail.as_ref().unchecked_ref()));
    tx.set_onabort(Some(fail.as_ref().unchecked_ref()));
    let out = JsFuture::from(p).await;
    tx.set_oncomplete(None);
    tx.set_onerror(None);
    tx.set_onabort(None);
    out.map(|_| ()).map_err(err)
}

async fn db() -> Result<IdbDatabase, String> {
    if let Some(d) = DB.with(|d| d.borrow().clone()) {
        return Ok(d);
    }
    let factory = util::window()
        .indexed_db()
        .map_err(err)?
        .ok_or("IndexedDB is not available")?;
    let open = factory.open_with_u32(DB_NAME, DB_VERSION).map_err(err)?;
    let o = open.clone();
    let upgrade = Closure::<dyn FnMut()>::new(move || {
        if let Ok(db) = o.result().and_then(|r| r.dyn_into::<IdbDatabase>())
            && !db.object_store_names().contains(STORE)
        {
            let _ = db.create_object_store(STORE);
        }
    });
    open.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let db = wait(&open).await;
    open.set_onupgradeneeded(None);
    let db: IdbDatabase = db?.dyn_into().map_err(err)?;
    DB.with(|d| *d.borrow_mut() = Some(db.clone()));
    Ok(db)
}

fn write_tx(db: &IdbDatabase) -> Result<(IdbTransaction, web_sys::IdbObjectStore), String> {
    let tx = db
        .transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)
        .map_err(err)?;
    let store = tx.object_store(STORE).map_err(err)?;
    Ok((tx, store))
}

fn meta_key(user: &str, vault: &str) -> String {
    format!("{user}{SEP}{vault}")
}

fn object_key(user: &str, vault: &str, object: &str) -> String {
    format!("{user}{SEP}{vault}{SEP}{object}")
}

/// Keys starting with `prefix`: `prefix` up to `prefix` with its last
/// character (always [`SEP`]) replaced by the next one.
fn prefix_range(prefix: &str) -> Result<JsValue, String> {
    let hi = format!("{}\u{2}", &prefix[..prefix.len() - SEP.len_utf8()]);
    IdbKeyRange::bound(&prefix.into(), &hi.into())
        .map(JsValue::from)
        .map_err(err)
}

async fn load(user: &str, vault: &str) -> Result<Option<Stored>, String> {
    let db = db().await?;
    let tx = db.transaction_with_str(STORE).map_err(err)?;
    let store = tx.object_store(STORE).map_err(err)?;
    let prefix = format!("{}{SEP}", meta_key(user, vault));
    let range = prefix_range(&prefix)?;
    // All requests up front: the transaction ends once none are outstanding.
    let meta = store.get(&meta_key(user, vault).into()).map_err(err)?;
    let keys = store.get_all_keys_with_key(&range).map_err(err)?;
    let values = store.get_all_with_key(&range).map_err(err)?;
    let meta = wait(&meta).await?;
    let keys: Array = wait(&keys).await?.unchecked_into();
    let values: Array = wait(&values).await?.unchecked_into();
    let Ok(meta) = meta.dyn_into::<Uint8Array>() else {
        return Ok(None);
    };
    let objects = keys
        .iter()
        .zip(values.iter())
        .filter_map(|(k, v)| {
            let object = k.as_string()?.strip_prefix(&prefix)?.to_string();
            Some((object, v.dyn_into::<Uint8Array>().ok()?.to_vec()))
        })
        .collect();
    Ok(Some(Stored {
        meta: meta.to_vec(),
        objects,
    }))
}

async fn save(user: &str, job: &Job) -> Result<(), String> {
    let db = db().await?;
    let (tx, store) = write_tx(&db)?;
    for (object, data) in &job.puts {
        let key = object_key(user, &job.vault, object);
        store
            .put_with_key(&Uint8Array::from(data.as_slice()), &key.into())
            .map_err(err)?;
    }
    for object in &job.deletes {
        store
            .delete(&object_key(user, &job.vault, object).into())
            .map_err(err)?;
    }
    store
        .put_with_key(
            &Uint8Array::from(job.meta.as_slice()),
            &meta_key(user, &job.vault).into(),
        )
        .map_err(err)?;
    committed(&tx).await
}

async fn remove_vault(user: &str, vault: &str) -> Result<(), String> {
    let db = db().await?;
    let (tx, store) = write_tx(&db)?;
    let prefix = format!("{}{SEP}", meta_key(user, vault));
    store.delete(&prefix_range(&prefix)?).map_err(err)?;
    store.delete(&meta_key(user, vault).into()).map_err(err)?;
    committed(&tx).await
}

/// The stored objects of each vault this browser has a copy of for `user`.
async fn stored_objects(user: &str) -> Result<HashMap<String, BTreeSet<String>>, String> {
    let db = db().await?;
    let tx = db.transaction_with_str(STORE).map_err(err)?;
    let store = tx.object_store(STORE).map_err(err)?;
    let prefix = format!("{user}{SEP}");
    let keys = store
        .get_all_keys_with_key(&prefix_range(&prefix)?)
        .map_err(err)?;
    let keys: Array = wait(&keys).await?.unchecked_into();
    let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
    for k in keys.iter().filter_map(|k| k.as_string()) {
        let Some(rest) = k.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once(SEP) {
            Some((v, o)) => out.entry(v.to_string()).or_default().insert(o.to_string()),
            None => out.entry(rest.to_string()).or_default().is_empty(),
        };
    }
    Ok(out)
}

/// Delete everything this browser stored for `user` ("forget this browser").
pub async fn forget_user(user: &str) {
    let run = async {
        let db = db().await?;
        let (tx, store) = write_tx(&db)?;
        store
            .delete(&prefix_range(&format!("{user}{SEP}"))?)
            .map_err(err)?;
        committed(&tx).await
    };
    if let Err(e) = run.await {
        util::warn(&format!("deleting local copies failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolpsonotes_core::engine::VaultMeta;
    use coolpsonotes_core::keys::{VaultKey, VaultKeyring};
    use coolpsonotes_core::protocol::{Role, Update};

    fn ring(epoch: u32) -> VaultKeyring {
        let mut ring = VaultKeyring::new("v1");
        ring.insert(epoch, VaultKey::generate());
        ring
    }

    fn replica(ring: &VaultKeyring) -> VaultReplica {
        VaultReplica::new("v1", Role::Owner, ring.clone(), VaultMeta::named("Test"))
    }

    /// Someone else's change of `note` to `text`, as the server delivers it.
    fn remote(from: &mut VaultReplica, note: &str, text: &str, seq: i64) -> Update {
        let n = from.notes.entry(note.to_string()).or_default();
        n.set_text(text);
        let state = n.encode_state();
        Update {
            object_id: note.into(),
            seq,
            author_id: "u2".into(),
            payload: from.encrypt_object(Purpose::Update, note, &state).unwrap(),
            created_at: 0,
        }
    }

    fn stored(job: &Job) -> Stored {
        Stored {
            meta: job.meta.clone(),
            objects: job.puts.clone(),
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_restored_vault_has_its_notes_and_cursor() {
        let k = ring(1);
        let (mut r, mut other) = (replica(&k), replica(&k));
        r.apply_update(&remote(&mut other, "n1", "hello", 5))
            .unwrap();
        let mut dirty = set(&["n1"]);
        let job = prepare(&r, &mut dirty, BTreeSet::new(), None, &mut BTreeSet::new()).unwrap();
        assert!(dirty.is_empty());
        assert_eq!(job.cursor, 5);

        let mut fresh = replica(&k);
        assert_eq!(restore(&mut fresh, stored(&job)).unwrap(), 5);
        assert_eq!(fresh.note_text("n1"), "hello");
        assert_eq!(fresh.seq, 5);
    }

    #[test]
    fn unconfirmed_local_changes_hold_the_cursor() {
        let k = ring(1);
        let (mut r, mut other) = (replica(&k), replica(&k));
        r.apply_update(&remote(&mut other, "n1", "a", 3)).unwrap();
        let mut stored = BTreeSet::new();
        let first = prepare(&r, &mut set(&["n1"]), BTreeSet::new(), None, &mut stored).unwrap();
        assert_eq!(first.cursor, 3);

        // A local edit the server hasn't confirmed, then someone else's change.
        let u = r
            .notes
            .entry("n2".into())
            .or_default()
            .set_text("mine")
            .unwrap();
        r.local_change("n2", u).unwrap();
        r.apply_update(&remote(&mut other, "n1", "ab", 4)).unwrap();
        let mut dirty = set(&["n1", "n2"]);
        let job = prepare(&r, &mut dirty, BTreeSet::new(), Some(3), &mut stored).unwrap();
        assert_eq!(dirty, set(&["n2"]));
        assert!(job.puts.iter().all(|(o, _)| o != "n2"));
        assert_eq!(
            job.cursor, 3,
            "n2 isn't stored, so the copy isn't complete past 3"
        );

        // Confirmed: everything is written and the cursor moves.
        r.pending.clear();
        r.seq = 5;
        let job = prepare(&r, &mut dirty, BTreeSet::new(), Some(3), &mut stored).unwrap();
        assert!(dirty.is_empty());
        assert_eq!(job.cursor, 5);
    }

    #[test]
    fn content_waiting_for_a_key_is_fetched_again() {
        let mut r = replica(&ring(1));
        let mut newer = replica(&ring(2));
        // Under a key we don't have yet: kept aside, but seq moves past it.
        assert!(
            r.apply_update(&remote(&mut newer, "n1", "secret", 7))
                .is_err()
        );
        r.seq = 9;
        assert!(r.needs_keys());
        let job = prepare(
            &r,
            &mut BTreeSet::new(),
            BTreeSet::new(),
            None,
            &mut BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(job.cursor, 6);
    }

    #[test]
    fn nothing_new_means_no_write() {
        let r = replica(&ring(1));
        assert!(
            prepare(
                &r,
                &mut BTreeSet::new(),
                BTreeSet::new(),
                Some(0),
                &mut BTreeSet::new()
            )
            .is_none()
        );
        assert!(
            prepare(
                &r,
                &mut BTreeSet::new(),
                BTreeSet::new(),
                None,
                &mut BTreeSet::new()
            )
            .is_some()
        );
    }

    #[test]
    fn deleted_notes_are_removed_from_the_copy() {
        let k = ring(1);
        let (mut r, mut other) = (replica(&k), replica(&k));
        r.apply_update(&remote(&mut other, "n1", "bye", 2)).unwrap();
        r.apply_purge("n1", 3);
        let job = prepare(
            &r,
            &mut set(&["n1"]),
            set(&["n1"]),
            Some(2),
            &mut set(&["n1"]),
        )
        .unwrap();
        assert!(job.puts.is_empty());
        assert_eq!(job.deletes, vec!["n1".to_string()]);
        assert_eq!(job.cursor, 3);
    }

    #[test]
    fn a_copy_missing_objects_is_refused() {
        let k = ring(1);
        let (mut r, mut other) = (replica(&k), replica(&k));
        r.apply_update(&remote(&mut other, "n1", "one", 1)).unwrap();
        r.apply_update(&remote(&mut other, "n2", "two", 2)).unwrap();
        let job = prepare(
            &r,
            &mut set(&["n1", "n2"]),
            BTreeSet::new(),
            None,
            &mut BTreeSet::new(),
        )
        .unwrap();
        let mut partial = stored(&job);
        partial.objects.pop();
        assert!(restore(&mut replica(&k), partial).is_err());
        assert!(restore(&mut replica(&k), stored(&job)).is_ok());
    }

    #[test]
    fn a_copy_under_another_key_is_refused_and_changes_nothing() {
        let r = replica(&ring(1));
        let job = prepare(
            &r,
            &mut set(&[INDEX_OBJECT_ID]),
            BTreeSet::new(),
            None,
            &mut BTreeSet::new(),
        )
        .unwrap();
        let mut stranger = replica(&ring(1));
        stranger.seq = 4;
        assert!(restore(&mut stranger, stored(&job)).is_err());
        assert_eq!(stranger.seq, 4);
    }
}
