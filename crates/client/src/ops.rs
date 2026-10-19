//! Plaintext document operations on a [`Hub`], shared by the CLI, the local
//! HTTP gateway and the MCP server. Enforces the hub's vault allow-list and
//! read-only flag; errors carry a category that front ends map to HTTP status
//! codes or MCP tool errors.

use std::collections::BTreeSet;

use coolpsonotes_core::doc::{Entry, EntryKind};
use coolpsonotes_core::protocol::{INDEX_OBJECT_ID, Snapshot, SnapshotInfo};
use coolpsonotes_core::{paths, search};
use serde::Serialize;

use crate::hub::{Hub, VaultSummary};
use crate::session::SharedVault;
use crate::vault::{Vault, WriteResult, content_hash};

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
    /// The target already exists (or is of another kind).
    #[error("{0}")]
    Conflict(String),
    /// `If-Match` did not match: the document changed since it was read.
    #[error("{0}")]
    Precondition(String),
    #[error("{0}")]
    BadRequest(String),
    /// Needed data is not available offline.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0:#}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for OpError {
    fn from(e: anyhow::Error) -> Self {
        OpError::Internal(e)
    }
}

pub type OpResult<T> = Result<T, OpError>;

#[derive(Serialize, Clone, Debug)]
pub struct DocInfo {
    pub vault: String,
    pub path: String,
    pub id: String,
    pub title: String,
    pub kind: EntryKind,
    pub tags: Vec<String>,
    pub created: i64,
    pub modified: i64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Document {
    #[serde(flatten)]
    pub info: DocInfo,
    pub hash: String,
    pub links: Vec<String>,
    pub content: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct PutOutcome {
    pub vault: String,
    pub path: String,
    pub id: String,
    pub created: bool,
    pub changed: bool,
    pub hash: String,
    pub bytes: usize,
    /// Local changes not uploaded yet (offline); they sync automatically.
    pub pending_changes: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct SearchResult {
    pub vault: String,
    pub vault_name: String,
    pub path: String,
    pub id: String,
    pub title: String,
    pub score: f32,
    pub snippet: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Revision {
    pub id: String,
    pub created_at: i64,
    pub covers_seq: i64,
    pub author_id: String,
    pub size: i64,
}

fn info(vault_id: &str, path: &str, e: &Entry, tags: Vec<String>) -> DocInfo {
    DocInfo {
        vault: vault_id.to_string(),
        path: path.to_string(),
        id: e.id.clone(),
        title: paths::title(path).to_string(),
        kind: e.kind,
        tags,
        created: e.created,
        modified: e.modified,
        size: e.size.unwrap_or(0),
        mime: e.mime.clone(),
    }
}

pub fn doc_info(v: &Vault, path: &str, e: &Entry) -> DocInfo {
    let tags = if e.kind == EntryKind::Note { v.tags(&e.id) } else { vec![] };
    let mut i = info(v.id(), path, e, tags);
    if e.kind == EntryKind::Note {
        i.size = v.note_text(&e.id).len() as u64;
    }
    i
}

async fn vault(hub: &Hub, reference: &str) -> OpResult<SharedVault> {
    hub.resolve(reference).await.map_err(|e| OpError::NotFound(format!("{e:#}")))
}

fn check_write(hub: &Hub, v: &Vault) -> OpResult<()> {
    if hub.read_only {
        return Err(OpError::Forbidden("this endpoint is read-only".into()));
    }
    v.require_writable().map_err(|e| OpError::Forbidden(format!("{e:#}")))
}

fn bad(e: anyhow::Error) -> OpError {
    OpError::BadRequest(format!("{e:#}"))
}

pub async fn list_vaults(hub: &Hub) -> Vec<VaultSummary> {
    hub.summaries().await
}

/// Notes (and with `all`, attachments) below `folder`.
pub async fn list_documents(hub: &Hub, vref: &str, folder: Option<&str>, all: bool) -> OpResult<Vec<DocInfo>> {
    let v = vault(hub, vref).await?;
    let g = v.lock().await;
    let folder = folder.unwrap_or("");
    let listing = g.list(folder, true).map_err(|e| OpError::NotFound(format!("{e:#}")))?;
    Ok(listing
        .into_iter()
        .filter_map(|l| l.entry.map(|e| (l.path, e)))
        .filter(|(_, e)| e.kind == EntryKind::Note || (all && e.kind == EntryKind::Attachment))
        .map(|(p, e)| doc_info(&g, &p, &e))
        .collect())
}

pub async fn read_document(hub: &Hub, vref: &str, path: &str) -> OpResult<Document> {
    let v = vault(hub, vref).await?;
    let g = v.lock().await;
    let (p, e) = g.find(path).ok_or_else(|| OpError::NotFound(format!("no document '{path}'")))?;
    if e.kind != EntryKind::Note {
        return Err(OpError::BadRequest(format!("'{p}' is an attachment; use the attachments endpoint")));
    }
    let content = g.note_text(&e.id);
    let links = coolpsonotes_core::markdown::analyze(&content).links.into_iter().map(|l| l.target).collect();
    Ok(Document { info: doc_info(&g, &p, &e), hash: content_hash(content.as_bytes()), links, content })
}

/// Create or diff-update a note. `if_match` is a content hash (`*` = must
/// exist); a mismatch means someone changed the note since it was read.
pub async fn put_document(
    hub: &Hub,
    vref: &str,
    path: &str,
    content: &str,
    if_match: Option<&str>,
) -> OpResult<PutOutcome> {
    let v = vault(hub, vref).await?;
    let (vault_id, res) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let existing = g.find(path);
        if let Some(expected) = if_match {
            match &existing {
                Some((_, e)) if e.kind == EntryKind::Note => {
                    let current = content_hash(g.note_text(&e.id).as_bytes());
                    if expected != "*" && expected.trim_start_matches("W/").trim_matches('"') != current {
                        return Err(OpError::Precondition(format!(
                            "the document changed since you read it (current hash {current}); re-read and retry"
                        )));
                    }
                }
                _ => return Err(OpError::Precondition("the document does not exist".into())),
            }
        }
        if let Some((p, e)) = &existing
            && e.kind != EntryKind::Note
        {
            return Err(OpError::Conflict(format!("'{p}' exists and is not a note")));
        }
        let res = g.write_note(path, content).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), res)
    };
    Ok(finish(hub, &vault_id, res, content.as_bytes()).await)
}

/// Create a note that must not exist yet.
pub async fn create_document(hub: &Hub, vref: &str, path: &str, content: &str) -> OpResult<PutOutcome> {
    let v = vault(hub, vref).await?;
    let (vault_id, res) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        if let Ok(p) = paths::normalize_note(path)
            && g.paths().contains_key(&p)
        {
            return Err(OpError::Conflict(format!("'{p}' already exists")));
        }
        let res = g.create_note(path, content).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), res)
    };
    Ok(finish(hub, &vault_id, res, content.as_bytes()).await)
}

pub async fn append_document(hub: &Hub, vref: &str, path: &str, text: &str) -> OpResult<PutOutcome> {
    let v = vault(hub, vref).await?;
    let (vault_id, res, full) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let res = g.append_note(path, text).map_err(bad)?;
        g.save()?;
        let full = g.note_text(&res.id);
        (g.id().to_string(), res, full)
    };
    let mut out = finish(hub, &vault_id, res, full.as_bytes()).await;
    out.bytes = text.len();
    Ok(out)
}

async fn finish(hub: &Hub, vault_id: &str, res: WriteResult, content: &[u8]) -> PutOutcome {
    let push =
        if res.changed { hub.local_changed(vault_id, vec![res.id.clone()], true).await } else { Default::default() };
    PutOutcome {
        vault: vault_id.to_string(),
        path: res.path,
        id: res.id,
        created: res.created,
        changed: res.changed,
        hash: content_hash(content),
        bytes: content.len(),
        pending_changes: push.remaining,
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct MoveOutcome {
    pub vault: String,
    pub id: Option<String>,
    pub from: String,
    pub to: String,
}

pub async fn move_document(hub: &Hub, vref: &str, from: &str, to: &str, update_links: bool) -> OpResult<MoveOutcome> {
    let v = vault(hub, vref).await?;
    let (vault_id, id, new_path) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let id = g.find(from).map(|(_, e)| e.id);
        if id.is_none() && !g.is_folder(from) {
            return Err(OpError::NotFound(format!("no document or folder '{from}'")));
        }
        let new_path = g.move_path(from, to, update_links).map_err(|e| {
            let msg = format!("{e:#}");
            if msg.contains("already exists") { OpError::Conflict(msg) } else { OpError::BadRequest(msg) }
        })?;
        g.save()?;
        (g.id().to_string(), id, new_path)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    Ok(MoveOutcome { vault: vault_id, id, from: from.to_string(), to: new_path })
}

/// Move a document (or folder) to the trash. Returns the trashed entry ids.
pub async fn delete_document(hub: &Hub, vref: &str, path: &str) -> OpResult<(String, Vec<String>)> {
    let v = vault(hub, vref).await?;
    let (vault_id, ids) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        if g.find(path).is_none() && !g.is_folder(path) {
            return Err(OpError::NotFound(format!("no document or folder '{path}'")));
        }
        let ids = g.trash_path(path).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), ids)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    Ok((vault_id, ids))
}

pub async fn search(hub: &Hub, query: &str, vault_ref: Option<&str>, limit: usize) -> OpResult<Vec<SearchResult>> {
    let q = search::parse_query(query);
    if q.is_empty() {
        return Err(OpError::BadRequest("empty search query".into()));
    }
    let vaults = match vault_ref {
        Some(r) => vec![vault(hub, r).await?],
        None => hub.vaults(),
    };
    let mut out = Vec::new();
    for v in vaults {
        let g = v.lock().await;
        if let Some(want) = &q.vault
            && !g.name().to_lowercase().starts_with(want.as_str())
            && g.id() != want
        {
            continue;
        }
        for h in g.search(&q) {
            out.push(SearchResult {
                vault: g.id().to_string(),
                vault_name: g.name().to_string(),
                title: paths::title(&h.path).to_string(),
                path: h.path,
                id: h.entry.id,
                score: h.hit.score,
                snippet: h.hit.snippet,
            });
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(limit.max(1));
    Ok(out)
}

pub async fn backlinks(hub: &Hub, vref: &str, path: &str) -> OpResult<Vec<DocInfo>> {
    let v = vault(hub, vref).await?;
    let g = v.lock().await;
    let (_, e) = g.find(path).ok_or_else(|| OpError::NotFound(format!("no document '{path}'")))?;
    Ok(g.backlinks(&e.id).into_iter().map(|(p, e)| doc_info(&g, &p, &e)).collect())
}

/// Stored revisions (encrypted snapshots on the server), newest first.
pub async fn history(hub: &Hub, vref: &str, path: &str) -> OpResult<Vec<Revision>> {
    let v = vault(hub, vref).await?;
    let (vid, oid) = {
        let g = v.lock().await;
        let (_, e) = g.find_note(path).map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        (g.id().to_string(), e.id)
    };
    let list: Vec<SnapshotInfo> =
        hub.session.api.get(&format!("/vaults/{vid}/objects/{oid}/snapshots")).await.map_err(|e| {
            if e.is_network() { OpError::Unavailable(e.to_string()) } else { OpError::Internal(e.into()) }
        })?;
    Ok(list
        .into_iter()
        .map(|s| Revision {
            id: s.id,
            created_at: s.created_at,
            covers_seq: s.covers_seq,
            author_id: s.author_id,
            size: s.size,
        })
        .collect())
}

/// Decrypt one revision of a note.
pub async fn revision(hub: &Hub, vref: &str, path: &str, snapshot_id: &str) -> OpResult<String> {
    let v = vault(hub, vref).await?;
    let (vid, oid) = {
        let g = v.lock().await;
        let (_, e) = g.find_note(path).map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        (g.id().to_string(), e.id)
    };
    if !crate::store::valid_id(snapshot_id) {
        return Err(OpError::BadRequest("invalid revision id".into()));
    }
    let snap: Snapshot =
        hub.session.api.get(&format!("/vaults/{vid}/objects/{oid}/snapshots/{snapshot_id}")).await.map_err(
            |e| match e.status() {
                Some(404) => OpError::NotFound("no such revision".into()),
                _ if e.is_network() => OpError::Unavailable(e.to_string()),
                _ => OpError::Internal(e.into()),
            },
        )?;
    if snap.object_id != oid {
        return Err(OpError::Internal(anyhow::anyhow!("the server returned a revision of another document")));
    }
    let g = v.lock().await;
    g.replica.decrypt_revision(&snap).map_err(|e| OpError::Internal(e.into()))
}

pub async fn get_attachment(hub: &Hub, vref: &str, path: &str) -> OpResult<(DocInfo, Vec<u8>)> {
    let v = vault(hub, vref).await?;
    let (p, e) = {
        let g = v.lock().await;
        g.find(path).ok_or_else(|| OpError::NotFound(format!("no attachment '{path}'")))?
    };
    if e.kind != EntryKind::Attachment {
        return Err(OpError::BadRequest(format!("'{p}' is not an attachment")));
    }
    let data = hub.session.attachment(&v, &e.id).await.map_err(|e| OpError::Unavailable(format!("{e:#}")))?;
    let g = v.lock().await;
    Ok((doc_info(&g, &p, &e), data))
}

pub async fn put_attachment(
    hub: &Hub,
    vref: &str,
    path: &str,
    data: &[u8],
    mime: Option<&str>,
) -> OpResult<PutOutcome> {
    let v = vault(hub, vref).await?;
    let max = hub.session.server_info().await.map(|i| i.max_attachment_bytes).unwrap_or(25 * 1024 * 1024);
    if data.len() as u64 > max {
        return Err(OpError::BadRequest(format!("attachment is larger than the server limit of {max} bytes")));
    }
    if paths::is_note(path) {
        return Err(OpError::BadRequest("Markdown files are documents, not attachments".into()));
    }
    let (vault_id, res) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let res = g.add_attachment(path, data, mime).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), res)
    };
    Ok(finish(hub, &vault_id, res, data).await)
}

/// Permanently delete everything in a vault's trash (server and index).
pub async fn empty_trash(hub: &Hub, vref: &str) -> OpResult<usize> {
    let v = vault(hub, vref).await?;
    let (vid, entries) = {
        let g = v.lock().await;
        check_write(hub, &g)?;
        (g.id().to_string(), g.trashed())
    };
    let api = &hub.session.api;
    let mut purged = BTreeSet::new();
    for e in &entries {
        let res = match e.kind {
            EntryKind::Note => api.delete(&format!("/vaults/{vid}/objects/{}", e.id)).await,
            EntryKind::Attachment => api.delete(&format!("/vaults/{vid}/attachments/{}", e.id)).await,
            EntryKind::Folder => Ok(()),
        };
        match res {
            Ok(()) => {}
            Err(err) if err.is_not_found() => {}
            Err(err) if err.is_network() => return Err(OpError::Unavailable(err.to_string())),
            Err(err) => return Err(OpError::Internal(anyhow::anyhow!(err).context("deleting from the server"))),
        }
        purged.insert(e.id.clone());
    }
    {
        let mut g = v.lock().await;
        for id in &purged {
            if let Some(e) = entries.iter().find(|e| &e.id == id) {
                if e.kind == EntryKind::Attachment {
                    let _ = g.store().delete_blob(id);
                    g.pending_blobs.remove(id);
                }
                if e.kind == EntryKind::Note && id != INDEX_OBJECT_ID {
                    g.apply_purge(id, 0);
                }
            }
            g.remove_entry(id)?;
        }
        g.save()?;
    }
    hub.local_changed(&vid, vec![], true).await;
    Ok(purged.len())
}
