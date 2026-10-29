//! Plaintext document operations on a [`Hub`], shared by the CLI, the local
//! HTTP gateway. Enforces the hub's vault allow-list and
//! read-only flag; errors carry a category that front ends map to HTTP status
//! codes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::moving::{self, Unmovable, What};
use notekpr_core::protocol::{
    INDEX_OBJECT_ID, Limit, ObjectRef, PurgedItem, Snapshot, SnapshotInfo,
};
use notekpr_core::{markdown, paths, quick, search};
use serde::Serialize;

use crate::hub::{Hub, VaultSummary};
use crate::session::{Session, SharedVault};
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
    /// Its icon (an emoji), if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
    /// They wait because the server is read-only for now.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// They wait because the vault is full, until it has room.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vault_full: bool,
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
        icon: e.icon.clone(),
    }
}

pub fn doc_info(v: &Vault, path: &str, e: &Entry) -> DocInfo {
    let tags = if e.kind == EntryKind::Note {
        v.tags(&e.id)
    } else {
        vec![]
    };
    let mut i = info(v.id(), path, e, tags);
    if e.kind == EntryKind::Note {
        i.size = v.note_text(&e.id).len() as u64;
    }
    i
}

async fn vault(hub: &Hub, reference: &str) -> OpResult<SharedVault> {
    hub.resolve(reference)
        .await
        .map_err(|e| OpError::NotFound(format!("{e:#}")))
}

fn check_write(hub: &Hub, v: &Vault) -> OpResult<()> {
    if hub.read_only {
        return Err(OpError::Forbidden("this endpoint is read-only".into()));
    }
    v.require_writable()
        .map_err(|e| OpError::Forbidden(format!("{e:#}")))
}

fn bad(e: anyhow::Error) -> OpError {
    OpError::BadRequest(format!("{e:#}"))
}

pub async fn list_vaults(hub: &Hub) -> Vec<VaultSummary> {
    hub.summaries().await
}

/// Notes (and with `all`, attachments) below `folder`.
pub async fn list_documents(
    hub: &Hub,
    vref: &str,
    folder: Option<&str>,
    all: bool,
) -> OpResult<Vec<DocInfo>> {
    let v = vault(hub, vref).await?;
    let g = v.lock().await;
    let folder = folder.unwrap_or("");
    let listing = g
        .list(folder, true)
        .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
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
    let (p, e) = g
        .find(path)
        .ok_or_else(|| OpError::NotFound(format!("no document '{path}'")))?;
    if e.kind != EntryKind::Note {
        return Err(OpError::BadRequest(format!(
            "'{p}' is an attachment; use the attachments endpoint"
        )));
    }
    let content = g.note_text(&e.id);
    let links = notekpr_core::markdown::analyze(&content)
        .links
        .into_iter()
        .map(|l| l.target)
        .collect();
    Ok(Document {
        info: doc_info(&g, &p, &e),
        hash: content_hash(content.as_bytes()),
        links,
        content,
    })
}

/// Create or diff-update a note. `if_match` is a content hash (`*` = must
/// exist); a mismatch means someone changed the note since it was read.
/// The day it is here (in the local time zone).
pub fn local_today() -> notekpr_core::dates::Date {
    let (y, mo, d, _, _) = local_time(notekpr_core::now_ms());
    notekpr_core::dates::Date::new(y, mo as u8, d as u8)
        .unwrap_or(notekpr_core::dates::Date::from_days(0))
}

/// `text` with `@today`, `@tomorrow` and `@yesterday` turned into those
/// days' dates (`@2026-10-23`): what a note means by them when it's written.
/// The Markdown folder's files are left as they are.
pub fn with_dates(text: &str) -> std::borrow::Cow<'_, str> {
    match notekpr_core::dates::expand_keywords(text, local_today()) {
        Some(t) => t.into(),
        None => text.into(),
    }
}

pub async fn put_document(
    hub: &Hub,
    vref: &str,
    path: &str,
    content: &str,
    if_match: Option<&str>,
) -> OpResult<PutOutcome> {
    let content = &*with_dates(content);
    let v = vault(hub, vref).await?;
    let (vault_id, res) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let existing = g.find(path);
        if let Some(expected) = if_match {
            match &existing {
                Some((_, e)) if e.kind == EntryKind::Note => {
                    let current = content_hash(g.note_text(&e.id).as_bytes());
                    if expected != "*"
                        && expected.trim_start_matches("W/").trim_matches('"') != current
                    {
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
pub async fn create_document(
    hub: &Hub,
    vref: &str,
    path: &str,
    content: &str,
) -> OpResult<PutOutcome> {
    let content = &*with_dates(content);
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

pub async fn append_document(
    hub: &Hub,
    vref: &str,
    path: &str,
    text: &str,
) -> OpResult<PutOutcome> {
    let text = &*with_dates(text);
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
    let push = if res.changed {
        hub.local_changed(vault_id, vec![res.id.clone()], true)
            .await
    } else {
        Default::default()
    };
    PutOutcome {
        vault: vault_id.to_string(),
        path: res.path,
        id: res.id,
        created: res.created,
        changed: res.changed,
        hash: content_hash(content),
        bytes: content.len(),
        pending_changes: push.remaining,
        read_only: push.read_only,
        vault_full: push.vault_full,
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct MoveOutcome {
    pub vault: String,
    pub id: Option<String>,
    pub from: String,
    pub to: String,
}

pub async fn move_document(
    hub: &Hub,
    vref: &str,
    from: &str,
    to: &str,
    update_links: bool,
) -> OpResult<MoveOutcome> {
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
            if msg.contains("already exists") {
                OpError::Conflict(msg)
            } else {
                OpError::BadRequest(msg)
            }
        })?;
        g.save()?;
        (g.id().to_string(), id, new_path)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    // People a moved note is shared with see its new name (the move itself
    // stands if that can't be sent now).
    if let Some(id) = &id
        && let Err(e) = crate::note_shares::refresh_meta(&hub.session, &v, id).await
    {
        crate::ui::warn(&format!("{e:#}"));
    }
    Ok(MoveOutcome {
        vault: vault_id,
        id,
        from: from.to_string(),
        to: new_path,
    })
}

/// Set (or with `None` remove) a document's or folder's icon. Returns the
/// vault's and the entry's ids.
pub async fn set_icon(
    hub: &Hub,
    vref: &str,
    path: &str,
    icon: Option<&str>,
) -> OpResult<(String, String)> {
    let v = vault(hub, vref).await?;
    let (vault_id, id) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        if g.find(path).is_none() && !g.is_folder(path) {
            return Err(OpError::NotFound(format!("no document or folder '{path}'")));
        }
        let id = g.set_icon(path, icon).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), id)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    Ok((vault_id, id))
}

/// Put a document or folder just before (or `after`) another in its
/// folder's order. Returns the vault's and the entry's id.
pub async fn place(
    hub: &Hub,
    vref: &str,
    path: &str,
    other: &str,
    after: bool,
) -> OpResult<(String, String)> {
    let v = vault(hub, vref).await?;
    let (vault_id, id) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        for p in [path, other] {
            if g.find(p).is_none() && !g.is_folder(p) {
                return Err(OpError::NotFound(format!("no document or folder '{p}'")));
            }
        }
        let id = g.place(path, other, after).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), id)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    Ok((vault_id, id))
}

/// Put the contents of `folder` ("" or "/" = the top) back in A–Z order.
/// Returns how many entries had been put in place by hand.
pub async fn sort_folder(hub: &Hub, vref: &str, folder: &str) -> OpResult<usize> {
    let v = vault(hub, vref).await?;
    let (vault_id, n) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        if !folder.trim_matches('/').is_empty() && !g.is_folder(folder) {
            return Err(OpError::NotFound(format!("no folder '{folder}'")));
        }
        let n = g.sort_folder(folder).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), n)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    Ok(n)
}

/// One comment or reply, as handed out.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommentItem {
    pub id: String,
    /// The author's user id, and name when the server could be asked.
    pub author: String,
    pub author_name: Option<String>,
    pub body: String,
    /// The people it mentions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<Mentioned>,
    pub created: i64,
    pub edited: bool,
    /// Written here, not stored on the server yet.
    pub pending: bool,
    /// Signed with a key known for its author.
    pub verified: bool,
}

/// Someone a comment mentions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Mentioned {
    pub id: String,
    pub name: Option<String>,
}

/// A comment with its replies.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommentThread {
    pub id: String,
    /// The text it is about, as it was.
    pub quote: String,
    /// The text is still in the note.
    pub on_text: bool,
    pub resolved_by: Option<String>,
    /// The comment, then its replies.
    pub comments: Vec<CommentItem>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteComments {
    pub vault_id: String,
    pub path: String,
    pub threads: Vec<CommentThread>,
}

/// The comments on a note (docs/comments.md). Authors' names are looked up
/// on the server when it can be reached.
pub async fn comments(hub: &Hub, vref: &str, path: &str) -> OpResult<NoteComments> {
    let v = vault(hub, vref).await?;
    let (vault_id, p, raw) = {
        let g = v.lock().await;
        let (p, note, threads) = g
            .comments(path)
            .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        let doc = g.replica.notes.get(&note);
        let raw: Vec<(notekpr_core::comments::Thread, bool)> = threads
            .into_iter()
            .map(|t| {
                let on =
                    doc.is_some_and(|d| notekpr_core::comments::locate(d, &t.anchor).is_some());
                (t, on)
            })
            .collect();
        (g.id().to_string(), p, raw)
    };
    Ok(NoteComments {
        vault_id,
        path: p,
        threads: thread_items(&hub.session, raw).await,
    })
}

/// Threads as handed out, with their authors' names (and who they
/// mention) as far as the server tells them. `raw`: each thread and
/// whether its text is still in the note.
pub(crate) async fn thread_items(
    session: &Session,
    raw: Vec<(notekpr_core::comments::Thread, bool)>,
) -> Vec<CommentThread> {
    let all = || {
        raw.iter()
            .flat_map(|(t, _)| std::iter::once(&t.root).chain(&t.replies))
    };
    let people: Vec<String> = all()
        .flat_map(|c| std::iter::once(&c.author).chain(&c.mentions).cloned())
        .collect();
    let names = user_names(session, people).await;
    let item = |c: &notekpr_core::comments::Comment| CommentItem {
        id: c.id.clone(),
        author: c.author.clone(),
        author_name: names.get(&c.author).cloned(),
        body: c.body.clone(),
        mentions: c
            .mentions
            .iter()
            .map(|u| Mentioned {
                id: u.clone(),
                name: names.get(u).cloned(),
            })
            .collect(),
        created: c.created,
        edited: c.edited,
        pending: c.pending,
        verified: c.trust == notekpr_core::comments::Trust::Verified,
    };
    raw.iter()
        .map(|(t, on)| CommentThread {
            id: t.root.id.clone(),
            quote: t.anchor.quote.clone(),
            on_text: *on,
            resolved_by: t.resolved_by.clone(),
            comments: std::iter::once(&t.root)
                .chain(&t.replies)
                .map(item)
                .collect(),
        })
        .collect()
}

/// Names of users by id, as far as the server tells them (best effort:
/// nothing for those it doesn't, or while offline).
pub async fn user_names(
    session: &Session,
    ids: Vec<String>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut asked = std::collections::HashSet::new();
    for id in ids {
        if !asked.insert(id.clone()) {
            continue;
        }
        match session.api.user(&id).await {
            Ok(u) => {
                out.insert(id, u.display_name);
            }
            Err(e) if e.is_offline() => break,
            Err(_) => {}
        }
    }
    out
}

/// A comment record sent: its id, the vault's and note's ids, and whom it
/// mentions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommentSent {
    pub id: String,
    pub vault_id: String,
    pub note_id: String,
    #[serde(flatten)]
    pub mentioned: crate::mentions::Resolved,
}

/// Whom `text` mentions: the vault's members only (docs/comments.md §1).
async fn mentions_in(hub: &Hub, v: &SharedVault, text: Option<&str>) -> crate::mentions::Resolved {
    match text {
        Some(t) if t.contains('@') => {
            let people = crate::mentions::people(&hub.session, v).await;
            crate::mentions::resolve(t, people.as_deref())
        }
        _ => Default::default(),
    }
}

fn check_comment(hub: &Hub, v: &Vault) -> OpResult<()> {
    if hub.read_only {
        return Err(OpError::Forbidden("this endpoint is read-only".into()));
    }
    v.require_commenting()
        .map_err(|e| OpError::Forbidden(format!("{e:#}")))
}

/// Comment on the text `on` of a note (it must appear there once).
pub async fn add_comment(
    hub: &Hub,
    vref: &str,
    path: &str,
    on: &str,
    body: &str,
) -> OpResult<CommentSent> {
    let v = vault(hub, vref).await?;
    check_comment(hub, &*v.lock().await)?;
    let mentioned = mentions_in(hub, &v, Some(body)).await;
    let sent = {
        let mut g = v.lock().await;
        check_comment(hub, &g)?;
        let s = &hub.session;
        let (note_id, id) = g
            .add_comment(
                s.account(),
                &s.user_id,
                path,
                on,
                body,
                mentioned.mentions.clone(),
            )
            .map_err(bad)?;
        g.save()?;
        CommentSent {
            id,
            vault_id: g.id().to_string(),
            note_id,
            mentioned,
        }
    };
    hub.local_changed(&sent.vault_id, vec![], true).await;
    Ok(sent)
}

/// Reply to, edit, delete, resolve or reopen a comment (`id`, or the start
/// of it). The new record is what's sent.
pub async fn comment_action(
    hub: &Hub,
    vref: &str,
    path: &str,
    id: &str,
    act: crate::vault::CommentAct,
) -> OpResult<CommentSent> {
    let v = vault(hub, vref).await?;
    check_comment(hub, &*v.lock().await)?;
    let mentioned = mentions_in(hub, &v, act.text()).await;
    let sent = {
        let mut g = v.lock().await;
        check_comment(hub, &g)?;
        let s = &hub.session;
        let (note_id, id) = g
            .comment_action(
                s.account(),
                &s.user_id,
                path,
                id,
                act,
                mentioned.mentions.clone(),
            )
            .map_err(bad)?;
        g.save()?;
        CommentSent {
            id,
            vault_id: g.id().to_string(),
            note_id,
            mentioned,
        }
    };
    hub.local_changed(&sent.vault_id, vec![], true).await;
    Ok(sent)
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
    // The quick notes keep no trash.
    if hub
        .quick_notes()
        .await
        .is_some_and(|q| q.try_lock().is_ok_and(|g| g.id() == vault_id))
    {
        purge_deleted_quick_notes(hub).await?;
    }
    Ok((vault_id, ids))
}

pub async fn search(
    hub: &Hub,
    query: &str,
    vault_ref: Option<&str>,
    limit: usize,
) -> OpResult<Vec<SearchResult>> {
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
    let (_, e) = g
        .find(path)
        .ok_or_else(|| OpError::NotFound(format!("no document '{path}'")))?;
    Ok(g.backlinks(&e.id)
        .into_iter()
        .map(|(p, e)| doc_info(&g, &p, &e))
        .collect())
}

/// Stored revisions (encrypted snapshots on the server), newest first.
pub async fn history(hub: &Hub, vref: &str, path: &str) -> OpResult<Vec<Revision>> {
    let v = vault(hub, vref).await?;
    let (vid, oid) = {
        let g = v.lock().await;
        let (_, e) = g
            .find_note(path)
            .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        (g.id().to_string(), e.id)
    };
    let list: Vec<SnapshotInfo> = hub
        .session
        .api
        .get(&format!("/vaults/{vid}/objects/{oid}/snapshots"))
        .await
        .map_err(|e| {
            if e.is_offline() {
                OpError::Unavailable(e.to_string())
            } else {
                OpError::Internal(e.into())
            }
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
        let (_, e) = g
            .find_note(path)
            .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        (g.id().to_string(), e.id)
    };
    if !crate::store::valid_id(snapshot_id) {
        return Err(OpError::BadRequest("invalid revision id".into()));
    }
    let snap: Snapshot = hub
        .session
        .api
        .get(&format!(
            "/vaults/{vid}/objects/{oid}/snapshots/{snapshot_id}"
        ))
        .await
        .map_err(|e| match e.status() {
            Some(404) => OpError::NotFound("no such revision".into()),
            _ if e.is_offline() => OpError::Unavailable(e.to_string()),
            _ => OpError::Internal(e.into()),
        })?;
    if snap.object_id != oid {
        return Err(OpError::Internal(anyhow::anyhow!(
            "the server returned a revision of another document"
        )));
    }
    let g = v.lock().await;
    g.replica
        .decrypt_revision(&snap)
        .map_err(|e| OpError::Internal(e.into()))
}

pub async fn get_attachment(hub: &Hub, vref: &str, path: &str) -> OpResult<(DocInfo, Vec<u8>)> {
    let v = vault(hub, vref).await?;
    let (p, e) = {
        let g = v.lock().await;
        g.find(path)
            .ok_or_else(|| OpError::NotFound(format!("no attachment '{path}'")))?
    };
    if e.kind != EntryKind::Attachment {
        return Err(OpError::BadRequest(format!("'{p}' is not an attachment")));
    }
    let data = hub
        .session
        .attachment(&v, &e.id)
        .await
        .map_err(|e| OpError::Unavailable(format!("{e:#}")))?;
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
    let max = hub.limits().await.max_attachment_bytes;
    if data.len() as u64 > max {
        return Err(OpError::BadRequest(format!(
            "attachment is larger than the server limit of {max} bytes"
        )));
    }
    if paths::is_note(path) {
        return Err(OpError::BadRequest(
            "Markdown files are documents, not attachments".into(),
        ));
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

/// Delete everything in a vault's trash for good: the vault's owner and
/// admins only. The server keeps it for a while (`purge_undo_days`), so the
/// index keeps the entries, marked, for [`undo_purge`]; entries with nothing
/// on the server (folders, what is gone already) are removed.
pub async fn empty_trash(hub: &Hub, vref: &str) -> OpResult<usize> {
    let v = vault(hub, vref).await?;
    let (vid, entries) = {
        let g = v.lock().await;
        check_write(hub, &g)?;
        if !g.role().can_administer() {
            return Err(OpError::Forbidden(
                "only the vault's owner and its admins can empty the trash".into(),
            ));
        }
        (g.id().to_string(), g.trashed())
    };
    let api = &hub.session.api;
    let mut purged = BTreeSet::new();
    // Kept by the server (marked), or nothing there (removed).
    let mut kept = Vec::new();
    for e in &entries {
        let res = match e.kind {
            EntryKind::Note | EntryKind::Drawing => {
                api.delete(&format!("/vaults/{vid}/objects/{}", e.id)).await
            }
            EntryKind::Attachment => {
                api.delete(&format!("/vaults/{vid}/attachments/{}", e.id))
                    .await
            }
            // Nothing on the server.
            EntryKind::Folder => {
                purged.insert(e.id.clone());
                continue;
            }
        };
        match res {
            Ok(()) => kept.push(e.id.clone()),
            Err(err) if err.is_not_found() => {}
            Err(err) if err.is_offline() => return Err(OpError::Unavailable(err.to_string())),
            Err(err) => {
                return Err(OpError::Internal(
                    anyhow::anyhow!(err).context("deleting from the server"),
                ));
            }
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
            if !kept.contains(id) {
                g.remove_entry(id)?;
            }
        }
        g.mark_purged(&kept)?;
        g.save()?;
    }
    hub.local_changed(&vid, vec![], true).await;
    Ok(purged.len())
}

/// What of a vault was deleted for good and can still be undone, with how
/// long it is kept (the vault's owner and admins).
pub async fn deleted_for_good(hub: &Hub, vref: &str) -> OpResult<Vec<(Entry, i64)>> {
    let v = vault(hub, vref).await?;
    let (vid, entries) = {
        let g = v.lock().await;
        if !g.role().can_administer() {
            return Ok(vec![]);
        }
        (g.id().to_string(), g.deleted_for_good())
    };
    if entries.is_empty() {
        return Ok(vec![]);
    }
    let kept: Vec<PurgedItem> = match hub.session.api.get(&format!("/vaults/{vid}/purged")).await {
        Ok(k) => k,
        Err(e) if e.is_offline() => return Err(OpError::Unavailable(e.to_string())),
        Err(e) => return Err(OpError::Internal(anyhow::anyhow!(e))),
    };
    Ok(entries
        .into_iter()
        .filter_map(|e| {
            let until = kept.iter().find(|k| k.id == e.id)?.kept_until;
            Some((e, until))
        })
        .collect())
}

/// Undo deleting an entry for good (by path or id), while the server keeps
/// it: back in the trash. The vault's owner and admins only.
pub async fn undo_purge(hub: &Hub, vref: &str, reference: &str) -> OpResult<Entry> {
    let v = vault(hub, vref).await?;
    let (vid, e) = {
        let g = v.lock().await;
        check_write(hub, &g)?;
        if !g.role().can_administer() {
            return Err(OpError::Forbidden(
                "only the vault's owner and its admins can undo deleting for good".into(),
            ));
        }
        let e = g
            .find_deleted_for_good(reference)
            .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        (g.id().to_string(), e)
    };
    let path = match e.kind {
        EntryKind::Note | EntryKind::Drawing => {
            format!("/vaults/{vid}/objects/{}/restore", e.id)
        }
        EntryKind::Attachment => format!("/vaults/{vid}/attachments/{}/restore", e.id),
        EntryKind::Folder => {
            return Err(OpError::NotFound("a folder can't be brought back".into()));
        }
    };
    match hub.session.api.post_empty(&path).await {
        Ok(()) => {}
        Err(err) if err.is_not_found() => {
            return Err(OpError::NotFound(format!(
                "'{}' can't be brought back any more: it is gone for good",
                e.path
            )));
        }
        Err(err) if err.is_offline() => return Err(OpError::Unavailable(err.to_string())),
        Err(err) => return Err(OpError::Internal(anyhow::anyhow!(err))),
    }
    {
        let mut g = v.lock().await;
        g.unmark_purged(&e.id)?;
        g.save()?;
    }
    hub.local_changed(&vid, vec![], true).await;
    // The note's content comes back with the server's `restored`.
    Ok(e)
}

/// Entries deleted for good stay in the index (marked) while the server
/// keeps their data; once their undo period is over and the server let go
/// of it, the vault's owner or an admin drops them. Asks the server only
/// when there are such entries. Returns how many were dropped.
pub async fn forget_erased(hub: &Hub, vault_id: &str, undo_days: u32) -> OpResult<usize> {
    let Some(v) = hub.get(vault_id) else {
        return Ok(0);
    };
    let over = notekpr_core::now_ms() - undo_days as i64 * 24 * 3600 * 1000;
    let due: Vec<String> = {
        let g = v.lock().await;
        if !g.role().can_administer() || g.read_only.is_some() || hub.read_only {
            return Ok(0);
        }
        g.deleted_for_good()
            .into_iter()
            .filter(|e| e.purged.is_some_and(|at| at <= over))
            .map(|e| e.id)
            .collect()
    };
    if due.is_empty() {
        return Ok(0);
    }
    let kept: Vec<PurgedItem> = hub
        .session
        .api
        .get(&format!("/vaults/{vault_id}/purged"))
        .await
        .map_err(|e| OpError::Unavailable(e.to_string()))?;
    let gone: Vec<String> = due
        .into_iter()
        .filter(|id| !kept.iter().any(|k| &k.id == id))
        .collect();
    if gone.is_empty() {
        return Ok(0);
    }
    {
        let mut g = v.lock().await;
        for id in &gone {
            g.remove_entry(id)?;
        }
        g.save()?;
    }
    hub.local_changed(vault_id, vec![], true).await;
    Ok(gone.len())
}

// ---------------------------------------------------------------- quick notes
//
// Each person's quick notes are a vault of that kind (`notekpr_core::quick`):
// flat, never shared, a limited number of notes, deleted for good. A quick
// note leaves them by moving to a vault ([`move_to_vault`]).

/// Name of a quick note made at `ms`, in local time: `2026-10-19 14.32`
/// (no `:`, which Windows file names don't allow).
pub fn quick_note_name(ms: i64) -> String {
    let (y, mo, d, h, mi) = local_time(ms);
    quick::stamp_name(y, mo, d, h, mi)
}

/// Year, month, day, hour and minute of `ms` in the local time zone (UTC
/// where that is not available).
fn local_time(ms: i64) -> (i32, u32, u32, u32, u32) {
    #[cfg(unix)]
    {
        let secs = ms.div_euclid(1000) as libc::time_t;
        // SAFETY: localtime_r only writes the struct it is given; this
        // process never changes TZ while running.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return (
                tm.tm_year + 1900,
                (tm.tm_mon + 1) as u32,
                tm.tm_mday as u32,
                tm.tm_hour as u32,
                tm.tm_min as u32,
            );
        }
    }
    let t = time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    (
        t.year(),
        u8::from(t.month()) as u32,
        t.day() as u32,
        t.hour() as u32,
        t.minute() as u32,
    )
}

/// The person's quick notes, made the first time.
async fn quick_vault(hub: &Hub) -> OpResult<SharedVault> {
    hub.ensure_quick_notes()
        .await
        .map_err(|e| OpError::Unavailable(format!("{e:#}")))
}

/// Add a quick note with `text`, named after the minute it is made.
pub async fn create_quick_note(hub: &Hub, text: &str) -> OpResult<PutOutcome> {
    let text = text.trim_end().trim_start_matches(['\n', '\r']);
    let name = quick_note_name(notekpr_core::now_ms());
    let v = quick_vault(hub).await?;
    let (vault_id, res) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        g.require_room()
            .map_err(|e| OpError::Conflict(format!("{e:#}")))?;
        let taken: BTreeSet<String> = g.paths().keys().map(|p| p.to_lowercase()).collect();
        let path = paths::unique(&format!("{name}.md"), |p| taken.contains(&p.to_lowercase()));
        let res = g.create_note(&path, text).map_err(bad)?;
        g.save()?;
        (g.id().to_string(), res)
    };
    Ok(finish(hub, &vault_id, res, text.as_bytes()).await)
}

#[derive(Serialize, Clone, Debug)]
pub struct QuickNote {
    pub path: String,
    pub id: String,
    pub title: String,
    pub preview: String,
    pub created: i64,
    pub modified: i64,
    /// Made on this device and waiting for room in the quick notes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub waiting_for_room: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct QuickNotes {
    /// The vault holding them (absent before the first one).
    pub vault: Option<String>,
    /// How many there are, and may be.
    pub count: usize,
    pub limit: Option<Limit>,
    /// Newest first.
    pub notes: Vec<QuickNote>,
}

/// The quick notes, newest first (at most `limit` of them).
pub async fn list_quick_notes(hub: &Hub, limit: usize) -> OpResult<QuickNotes> {
    let Some(v) = hub.quick_notes().await else {
        return Ok(QuickNotes {
            vault: None,
            count: 0,
            limit: None,
            notes: vec![],
        });
    };
    let g = v.lock().await;
    let mut notes = g.notes();
    let count = notes.len();
    notes.sort_by(|(_, a), (_, b)| {
        b.created
            .cmp(&a.created)
            .then(b.modified.cmp(&a.modified))
            .then(a.id.cmp(&b.id))
    });
    notes.truncate(limit);
    Ok(QuickNotes {
        vault: Some(g.id().to_string()),
        count,
        limit: Some(g.note_limit),
        notes: notes
            .into_iter()
            .map(|(p, e)| {
                let title = paths::title(&p).to_string();
                QuickNote {
                    preview: quick::preview(&g.note_text(&e.id), &title),
                    waiting_for_room: g.outbox.waits_for_room(&e.id),
                    title,
                    path: p,
                    id: e.id,
                    created: e.created,
                    modified: e.modified,
                }
            })
            .collect(),
    })
}

/// The attachments the note `id` links to (`(id, path)`), and whether no
/// other note of the vault does.
fn linked_attachments(g: &Vault, id: &str) -> Vec<(String, String, bool)> {
    let cands = g.link_candidates();
    let all = g.paths();
    let attachments: BTreeMap<&str, &str> = all
        .iter()
        .filter(|(_, e)| e.kind == EntryKind::Attachment)
        .map(|(p, e)| (e.id.as_str(), p.as_str()))
        .collect();
    let path_of = |n: &str| g.display_path(n).unwrap_or_default();
    let mine = markdown::linked_ids(&g.note_text(id), &path_of(id), &cands);
    let others: BTreeSet<String> = g
        .notes()
        .into_iter()
        .filter(|(_, e)| e.id != id)
        .flat_map(|(p, e)| markdown::linked_ids(&g.note_text(&e.id), &p, &cands))
        .collect();
    mine.into_iter()
        .filter_map(|a| {
            let path = attachments.get(a.as_str())?;
            Some((a.clone(), path.to_string(), !others.contains(&a)))
        })
        .collect()
}

/// Delete a quick note for good, with the attachments only it uses. Offline
/// it is gone from this device at once and from the server once online.
pub async fn delete_quick_note(hub: &Hub, reference: &str) -> OpResult<String> {
    let v = hub
        .quick_notes()
        .await
        .ok_or_else(|| OpError::NotFound("you have no quick notes yet".into()))?;
    let (vault_id, path) = {
        let mut g = v.lock().await;
        check_write(hub, &g)?;
        let (path, e) = g
            .find_note(reference)
            .map_err(|e| OpError::NotFound(format!("{e:#}")))?;
        let own: Vec<String> = linked_attachments(&g, &e.id)
            .into_iter()
            .filter(|(_, _, only_here)| *only_here)
            .map(|(id, _, _)| id)
            .collect();
        g.trash_id(&e.id).map_err(bad)?;
        for a in own {
            g.trash_id(&a).map_err(bad)?;
        }
        g.save()?;
        (g.id().to_string(), path)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    purge_deleted_quick_notes(hub).await?;
    Ok(path)
}

/// Delete for good what was deleted from the quick notes (they keep no
/// trash). Without the server it waits: deleted ones stay out of sight.
pub async fn purge_deleted_quick_notes(hub: &Hub) -> OpResult<usize> {
    let Some(v) = hub.quick_notes().await else {
        return Ok(0);
    };
    let (id, any) = {
        let g = v.lock().await;
        (
            g.id().to_string(),
            !g.trashed().is_empty() && g.read_only.is_none(),
        )
    };
    if !any || hub.read_only {
        return Ok(0);
    }
    match empty_trash(hub, &id).await {
        Err(OpError::Unavailable(_)) => Ok(0),
        r => r,
    }
}

/// Delete for good the files no quick note links to any more (unchanged for
/// [`quick::UNLINKED_FILE_GRACE_MS`]): the quick notes keep files only for
/// their notes. Call it once they are caught up with the server — before,
/// the edit with a file's link may not have arrived. `now`: the time (ms).
pub async fn drop_unlinked_quick_files(hub: &Hub, now: i64) -> OpResult<usize> {
    let Some(v) = hub.quick_notes().await else {
        return Ok(0);
    };
    if hub.read_only {
        return Ok(0);
    }
    let vault_id = {
        let mut g = v.lock().await;
        if g.read_only.is_some() {
            return Ok(0);
        }
        let before = now - quick::UNLINKED_FILE_GRACE_MS;
        let ids = quick::unlinked_files(&g.replica, before).unwrap_or_default();
        if ids.is_empty() {
            return Ok(0);
        }
        for id in &ids {
            g.trash_id(id).map_err(bad)?;
        }
        g.save()?;
        g.id().to_string()
    };
    hub.local_changed(&vault_id, vec![], true).await;
    purge_deleted_quick_notes(hub).await
}

#[derive(Serialize, Clone, Debug)]
pub struct Moved {
    pub from_vault: String,
    pub from_path: String,
    pub vault: String,
    pub path: String,
    /// A note, a drawing or a folder.
    pub kind: EntryKind,
    /// The note's or folder's id there (`None`: a folder that only exists
    /// as the parent of what is in it).
    pub id: Option<String>,
    /// How many notes moved (with a folder, all in it).
    pub notes: usize,
    /// Files copied along: those in the folder and those its notes link to.
    pub attachments: usize,
    /// The originals are gone: deleted for good from the quick notes, to
    /// the trash of another vault (except files a note staying behind links
    /// to). `false` if the target vault doesn't have the copies yet (the
    /// server couldn't be reached): both are kept.
    pub removed: bool,
}

/// Move a note or drawing, or a folder with everything in it, to another
/// vault ([`moving`]): the copies there, then — once the server has them —
/// the originals go: deleted for good from the quick notes, to the trash of
/// a vault. A drawing's copy replaces its original, which the server
/// deletes for good once the copy is stored (so the copy needs no room under
/// a drawings limit). History, comments and shares stay with the
/// originals. `from` `None`: the quick notes. The copy is `folder/name`
/// (`name` `None`: the note's title or the folder's name, for a quick note a
/// title from its first line).
pub async fn move_to_vault(
    hub: &Hub,
    from: Option<&str>,
    reference: &str,
    to: &str,
    folder: &str,
    name: Option<&str>,
) -> OpResult<Moved> {
    let src = match from {
        Some(r) => vault(hub, r).await?,
        None => hub
            .quick_notes()
            .await
            .ok_or_else(|| OpError::NotFound("you have no quick notes yet".into()))?,
    };
    let dst = vault(hub, to).await?;
    if Arc::ptr_eq(&src, &dst) {
        return Err(OpError::BadRequest(
            "that is the same vault: use `notekpr mv` to move within it".into(),
        ));
    }
    if !hub.is_online() {
        return Err(OpError::Unavailable(
            "moving to another vault needs the server".into(),
        ));
    }
    // What goes.
    let (from_vault, plan, name) = {
        let g = src.lock().await;
        check_write(hub, &g)?;
        let what = match g.find(reference) {
            Some((_, e)) if matches!(e.kind, EntryKind::Note | EntryKind::Drawing) => {
                What::Note(e.id)
            }
            Some((p, e)) if e.kind == EntryKind::Folder => What::Folder(p),
            Some((p, _)) => {
                return Err(OpError::BadRequest(format!(
                    "'{p}' is a file: files move with the notes that link to them"
                )));
            }
            None if g.is_folder(reference) => What::Folder(reference.to_string()),
            None => {
                return Err(OpError::NotFound(format!(
                    "no note or folder '{reference}' in vault '{}'",
                    g.name()
                )));
            }
        };
        let plan = moving::plan(&g.replica, &what).map_err(|e| match e {
            Unmovable::Gone => OpError::NotFound(format!("'{reference}' is gone")),
            Unmovable::Loading => OpError::Unavailable(
                "some of it hasn't arrived from the server yet: try again once synced".into(),
            ),
            Unmovable::NeedsNewerApp => OpError::Conflict(
                "it holds things a newer version of notekpr added: update notekpr to move it"
                    .into(),
            ),
        })?;
        let name = match (name.map(str::trim).filter(|n| !n.is_empty()), &what) {
            (Some(n), _) => n.to_string(),
            (None, What::Note(id)) if g.is_quick_notes() => {
                Some(quick::suggested_title(&g.note_text(id)))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| plan.name().to_string())
            }
            (None, _) => plan.name().to_string(),
        };
        (g.id().to_string(), plan, name)
    };
    if name.contains(['/', '\\']) {
        return Err(OpError::BadRequest("give it a name without '/'".into()));
    }
    let mut files = BTreeMap::new();
    for id in plan.files() {
        let data = hub
            .session
            .attachment(&src, &id)
            .await
            .map_err(|e| OpError::Unavailable(format!("{e:#}")))?;
        files.insert(id, data);
    }
    // The copies.
    let (vault_id, placed, ids) = {
        let mut g = dst.lock().await;
        check_write(hub, &g)?;
        if g.is_quick_notes() {
            return Err(OpError::BadRequest(
                "notes move from the quick notes to a vault, not into them".into(),
            ));
        }
        if g.is_drawings() && plan.notes() > 0 {
            return Err(OpError::BadRequest(
                "your Drawings hold drawings and files only: move notes to a vault".into(),
            ));
        }
        let placed = plan
            .place(&g.replica, folder, &name)
            .map_err(|e| OpError::BadRequest(e.to_string()))?;
        let mut ids = BTreeMap::new();
        for c in &placed.copies {
            let id = match c.kind {
                EntryKind::Folder => g.create_folder(&c.path).map_err(bad)?,
                EntryKind::Attachment => {
                    let data = files.get(&c.from).map(Vec::as_slice).unwrap_or_default();
                    g.add_attachment(&c.path, data, None).map_err(bad)?.id
                }
                EntryKind::Note => g.create_note(&c.path, &c.text).map_err(bad)?.id,
                EntryKind::Drawing => {
                    let scene = c.drawing.clone().unwrap_or_default();
                    let id = g.create_drawing(&c.path, &scene).map_err(bad)?.id;
                    // The original goes once the copy is stored: the copy
                    // replaces it, and needs no room for another drawing.
                    if plan.trash.contains(&c.from) {
                        g.outbox.set_replaces(
                            &id,
                            ObjectRef {
                                vault_id: from_vault.clone(),
                                object_id: c.from.clone(),
                            },
                        );
                    }
                    id
                }
            };
            g.copy_looks(&id, c.icon.as_deref(), c.order.as_deref())
                .map_err(bad)?;
            ids.insert(c.from.clone(), (c.kind, id));
        }
        g.save()?;
        (g.id().to_string(), placed, ids)
    };
    // Notes and drawings: objects whose content the server must have.
    let notes: Vec<String> = ids
        .values()
        .filter(|(k, _)| matches!(k, EntryKind::Note | EntryKind::Drawing))
        .map(|(_, id)| id.clone())
        .collect();
    let pushed = hub.local_changed(&vault_id, notes.clone(), true).await;
    let stored = !pushed.offline && !pushed.read_only && {
        let g = dst.lock().await;
        ids.values().all(|(_, id)| !g.pending_blobs.contains(id))
            && notes.iter().all(|n| !g.outbox.unconfirmed(&g.replica, n))
            && !g.outbox.unconfirmed(&g.replica, INDEX_OBJECT_ID)
    };
    // The originals, once the vault has the copies.
    if stored {
        let quick_notes = {
            let mut g = src.lock().await;
            g.trash_ids(&plan.trash).map_err(bad)?;
            g.save()?;
            g.is_quick_notes()
        };
        hub.local_changed(&from_vault, vec![], true).await;
        if quick_notes {
            purge_deleted_quick_notes(hub).await?;
        }
    }
    let root = placed.copies.iter().find(|c| c.path == placed.path);
    Ok(Moved {
        from_vault,
        from_path: plan.from_path().to_string(),
        vault: vault_id,
        path: placed.path.clone(),
        kind: if plan.is_folder() {
            EntryKind::Folder
        } else if plan.is_drawing() {
            EntryKind::Drawing
        } else {
            EntryKind::Note
        },
        id: root
            .and_then(|c| ids.get(&c.from))
            .map(|(_, id)| id.clone()),
        notes: plan.notes(),
        attachments: plan.files().len(),
        removed: stored,
    })
}

#[cfg(test)]
mod quick_tests {
    use super::*;

    #[test]
    fn quick_note_names_are_local_minutes() {
        let n = quick_note_name(notekpr_core::now_ms());
        let b = n.as_bytes();
        assert_eq!(n.len(), 16, "{n}");
        assert!(
            b[4] == b'-' && b[7] == b'-' && b[10] == b' ' && b[13] == b'.',
            "{n}"
        );
        assert_eq!(n.chars().filter(|c| c.is_ascii_digit()).count(), 12, "{n}");
        assert!(!n.contains(':'));
    }
}
