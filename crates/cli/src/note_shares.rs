//! Sharing single notes: the owner's side (share/unshare a note of a vault)
//! and the recipient's side (read and edit notes shared with you).
//!
//! Sharing starts with a fresh document key that has no link to the note's
//! earlier keys (`engine::start_sharing_keyed`), so recipients get the note
//! from then on only. They never receive the vault key or the vault index.
//!
//! Recipients comment too (docs/comments.md): they know no membership log,
//! so commenters' keys are the owner's and those pinned on first use, and
//! they may mention only the people who commented on the note and its owner.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::comments::{self, Person};
use notekpr_core::engine::{self, ObjectMeta, PinStatus, VaultReplica};
use notekpr_core::keys::{AccountPublic, VaultKeyring};
use notekpr_core::memberlog::LogBody;
use notekpr_core::protocol::*;
use notekpr_sync::members;

use crate::api::ApiError;
use crate::manage::{self, ShareOutcome};
use crate::ops::{CommentSent, NoteComments};
use crate::session::{Session, SharedVault};
use crate::ui;
use crate::vault::{self, CommentAct};

/// Give a note its own key (first time) and share it with one person.
pub async fn share_note(
    session: &Session,
    vault: &SharedVault,
    path: &str,
    user_ref: &str,
    role: Role,
    accept_new_key: bool,
) -> Result<ShareOutcome> {
    if !matches!(role, Role::Editor | Role::Viewer) {
        bail!("notes can be shared as editor or viewer");
    }
    let (id, note_id) = {
        let v = vault.lock().await;
        manage::require_owner(&v)?;
        let (_, e) = v.find_note(path)?;
        (v.id().to_string(), e.id)
    };
    let user = manage::find_user(session, user_ref).await?;
    if user.id == session.user_id {
        bail!("you already have access to your own notes");
    }
    let verified = session.verify_user(&user)?.ok_or_else(|| {
        anyhow!(
            "{} has not set up encryption yet; ask them to sign in once, then share again",
            user.display_name
        )
    })?;
    if verified.pin == PinStatus::Changed {
        if !accept_new_key {
            bail!(
                "WARNING: the account key of {} CHANGED since you first shared with them (now {}).\n\
                 Compare the fingerprint with them over a trusted channel; if it matches, re-run with --accept-new-key",
                user.display_name,
                verified.fingerprint
            );
        }
        session.accept_pin(&user.id, &verified.fingerprint)?;
    }
    ensure_document_key(session, vault, &id, &note_id).await?;
    let envelope = {
        let v = vault.lock().await;
        let ring = &v.replica.doc_keys[&note_id];
        let mut env = [engine::object_share_envelope(ring, &note_id, &user)?];
        // Signed by us, so they can tell the key is really ours.
        members::sign_grants(session.account(), &id, &note_id, ring, &mut env);
        let [env] = env;
        env
    };
    // The owner signs who the note is shared with into the vault's log.
    let key = user
        .account_key
        .as_ref()
        .expect("verified above")
        .verify()?;
    let req = AddObjectShareRequest {
        user_id: user.id.clone(),
        role: role.clone(),
        envelope,
        log_entry: session
            .log_entry(
                vault,
                members::share(&note_id, &user.id, role.clone(), &key),
            )
            .await,
    };
    let mut updated_role = false;
    match session
        .api
        .post::<_, ObjectShareInfo>(&format!("/vaults/{id}/objects/{note_id}/shares"), &req)
        .await
    {
        Ok(_) => {}
        Err(e) if e.status() == Some(409) && e.to_string().contains("already shared") => {
            session
                .api
                .patch::<_, ()>(
                    &format!("/vaults/{id}/objects/{note_id}/shares/{}", user.id),
                    &UpdateMemberRequest {
                        role: role.clone(),
                        log_entry: session
                            .log_entry(
                                vault,
                                members::share(&note_id, &user.id, role.clone(), &key),
                            )
                            .await,
                    },
                )
                .await?;
            updated_role = true;
        }
        Err(e) => return Err(anyhow!(e).context("sharing the note")),
    }
    session.reload_member_log(vault).await?;
    // Our keys as the server has them now: also where sharing started.
    let keys = session.keys(&id).await?;
    {
        let mut v = vault.lock().await;
        v.set_keys(keys);
        v.replica.shared.insert(note_id.clone());
        v.mark_state_dirty();
        v.save()?;
    }
    Ok(ShareOutcome {
        user,
        fingerprint: Some(verified.fingerprint),
        pin: Some(verified.pin),
        key_shared: true,
        updated_role,
    })
}

/// After sharing the note at `path`: send its comments people it's shared
/// with can't read yet again, under its own key (docs/comments.md §5).
/// Returns how many records go out.
pub async fn carry_comments(vault: &SharedVault, path: &str) -> Result<usize> {
    let mut v = vault.lock().await;
    let (_, e) = v.find_note(path)?;
    let pushes = v.replica.carry_comments(&e.id)?;
    v.mark_comments_dirty(&e.id);
    v.save()?;
    Ok(pushes.len())
}

/// Make sure a note can be shared: if nobody has it yet, move it to a fresh
/// document key (with a covering snapshot under it) that its earlier keys
/// can't be derived from.
async fn ensure_document_key(
    session: &Session,
    vault: &SharedVault,
    id: &str,
    note_id: &str,
) -> Result<()> {
    for attempt in 0..3 {
        if vault.lock().await.replica.is_shared_object(note_id) {
            return Ok(());
        }
        // The snapshot must cover every update of the note: catch up first.
        session
            .catch_up(vault)
            .await
            .map_err(|e| anyhow!(e).context("catching up before sharing"))?;
        let (ring, req) = {
            let v = vault.lock().await;
            let meta = v
                .replica
                .index
                .get(note_id)
                .map(|e| ObjectMeta::of(&e))
                .unwrap_or_default();
            let state = v
                .replica
                .note(note_id)
                .map(|n| n.encode_state())
                .unwrap_or_default();
            let keyring = &v.replica.keyring;
            match v.replica.doc_keys.get(note_id) {
                Some(doc) if !v.outbox.waits_for_key(note_id) => engine::start_sharing_keyed(
                    keyring,
                    doc,
                    note_id,
                    &meta,
                    &state,
                    v.replica.seq,
                )?,
                // No key yet, or a new note's key the server doesn't know
                // (sharing starts with a key of its own instead).
                _ => engine::start_sharing(keyring, note_id, &meta, &state, v.replica.seq)?,
            }
        };
        let mut req = req;
        members::sign_key_update(session.account(), id, note_id, &mut req);
        match session
            .api
            .put::<_, ()>(&format!("/vaults/{id}/objects/{note_id}/key"), &req)
            .await
        {
            Ok(()) => {
                let mut v = vault.lock().await;
                v.outbox.key_registered(note_id);
                v.set_note_keyring(note_id, ring);
                v.save()?;
                return Ok(());
            }
            // Someone else started sharing it, or new changes arrived: reload and retry.
            Err(e) if matches!(e.status(), Some(409) | Some(412)) && attempt < 2 => {
                session.refresh_keys(vault).await?;
            }
            Err(e) => return Err(anyhow!(e).context("giving the note its own key")),
        }
    }
    bail!("could not start sharing the note; try again")
}

/// Stop sharing a note with someone, with the note's new key for everyone
/// else in the same request: nothing is written under a key they know.
pub async fn unshare_note(
    session: &Session,
    vault: &SharedVault,
    path: &str,
    user_ref: &str,
) -> Result<PublicUser> {
    let (id, note_id) = {
        let v = vault.lock().await;
        manage::require_owner(&v)?;
        let (_, e) = v.find_note(path)?;
        (v.id().to_string(), e.id)
    };
    let shares = note_shares(session, &id, &note_id).await?;
    let user = shares
        .iter()
        .map(|s| &s.user)
        .find(|u| {
            u.id == user_ref
                || u.display_name.eq_ignore_ascii_case(user_ref)
                || u.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(user_ref))
        })
        .cloned()
        .ok_or_else(|| anyhow!("the note is not shared with '{user_ref}'"))?;
    for attempt in 0..2 {
        let log_entry = session
            .log_entry(
                vault,
                LogBody::Unshare {
                    object_id: note_id.clone(),
                    user_id: user.id.clone(),
                },
            )
            .await;
        let r = note_rotation(session, vault, &id, &note_id, Some(&user.id)).await?;
        let req = RemoveShareRequest {
            log_entry,
            rotation: Some(r.req.clone()),
        };
        match session
            .api
            .delete_with(
                &format!("/vaults/{id}/objects/{note_id}/shares/{}", user.id),
                &req,
            )
            .await
        {
            Ok(()) => {
                session.reload_member_log(vault).await?;
                r.store(vault, &note_id).await?;
                return Ok(user);
            }
            // Recipients, keys or the log changed meanwhile: retry once.
            Err(e) if matches!(e.status(), Some(409) | Some(412)) && attempt == 0 => {
                session.refresh_note_keys(vault, &note_id).await?;
                session.reload_member_log(vault).await?;
            }
            Err(e) => return Err(anyhow!(e).context("removing them from the note")),
        }
    }
    bail!("could not stop sharing the note (its recipients keep changing); try again")
}

/// New document key for the note's remaining recipients.
pub async fn rotate_note_key(
    session: &Session,
    vault: &SharedVault,
    id: &str,
    note_id: &str,
) -> Result<()> {
    let r = note_rotation(session, vault, id, note_id, None).await?;
    session
        .api
        .put::<_, ()>(&format!("/vaults/{id}/objects/{note_id}/key"), &r.req)
        .await
        .context("rotating the note key")?;
    r.store(vault, note_id).await
}

/// A note's new key, prepared for the server.
struct NoteRotation {
    next: VaultKeyring,
    req: ObjectKeyUpdate,
    /// Whether anyone keeps the note (or is left out of its key for now).
    shared: bool,
}

impl NoteRotation {
    /// The server took it: keep the key.
    async fn store(self, vault: &SharedVault, note_id: &str) -> Result<()> {
        let mut v = vault.lock().await;
        v.set_note_keyring(note_id, self.next);
        if !self.shared {
            v.replica.shared.remove(note_id);
        }
        v.save()
    }
}

/// The note's new key for its recipients but `removing`.
async fn note_rotation(
    session: &Session,
    vault: &SharedVault,
    id: &str,
    note_id: &str,
    removing: Option<&str>,
) -> Result<NoteRotation> {
    // Never to a key that changed since it was pinned: such people are left
    // out of the new key until verified (`share-note --accept-new-key`).
    let (users, changed) = session.split_changed_keys(
        note_shares(session, id, note_id)
            .await?
            .into_iter()
            .map(|s| s.user)
            .filter(|u| Some(u.id.as_str()) != removing)
            .collect(),
    );
    let (next, req) = {
        let v = vault.lock().await;
        let ring = v
            .replica
            .doc_keys
            .get(note_id)
            .ok_or_else(|| anyhow!("this note is not shared"))?;
        let meta = v
            .replica
            .index
            .get(note_id)
            .map(|e| ObjectMeta::of(&e))
            .unwrap_or_default();
        let (next, mut req) =
            engine::rotate_object_key(&v.replica.keyring, ring, note_id, &meta, &users)?;
        members::sign_grants(session.account(), id, note_id, &next, &mut req.envelopes);
        req.without = changed.iter().map(|u| u.id.clone()).collect();
        (next, req)
    };
    if !changed.is_empty() {
        let names: Vec<&str> = changed.iter().map(|u| u.display_name.as_str()).collect();
        ui::warn(&format!(
            "the note's new key went to everyone except {}: their account key changed. Verify their \
             fingerprint with them, then share the note with them again with --accept-new-key",
            names.join(", ")
        ));
    }
    Ok(NoteRotation {
        next,
        req,
        shared: !users.is_empty() || !changed.is_empty(),
    })
}

pub async fn note_shares(
    session: &Session,
    vault_id: &str,
    note_id: &str,
) -> Result<Vec<ObjectShareInfo>> {
    Ok(session
        .api
        .get(&format!("/vaults/{vault_id}/objects/{note_id}/shares"))
        .await?)
}

/// A note shared with us, with its decrypted title and icon.
pub struct SharedNoteView {
    pub info: SharedNote,
    pub title: String,
    pub icon: Option<String>,
}

/// Notes shared with the signed-in user individually.
pub async fn shared_with_me(session: &Session) -> Result<Vec<SharedNoteView>> {
    let list: Vec<SharedNote> = session.api.get("/shares").await?;
    let mut out = Vec::with_capacity(list.len());
    for info in list {
        let (title, icon) = match recipient_ring(session, &info).await {
            Ok((ring, _)) => match info
                .encrypted_meta
                .as_ref()
                .and_then(|m| ObjectMeta::decrypt(&ring, &info.object_id, m).ok())
            {
                Some(m) => (m.title, m.icon),
                None => ("(untitled)".into(), None),
            },
            Err(_) => ("(key not available yet)".into(), None),
        };
        out.push(SharedNoteView { info, title, icon });
    }
    Ok(out)
}

/// The keys of a note shared with us, confirmed by an owner of its vault
/// (their key as pinned here): a server can't hand us a key of its own.
async fn recipient_ring(
    session: &Session,
    info: &SharedNote,
) -> Result<(notekpr_core::keys::VaultKeyring, Role)> {
    let (vault_id, note_id) = (&info.vault_id, &info.object_id);
    let keys: ObjectKeysResponse = session
        .api
        .get(&format!("/vaults/{vault_id}/objects/{note_id}/keys"))
        .await?;
    // Who signed giving us its newest key: the vault's owner we know, or
    // another owner, looked up.
    let signer = match engine::grant_signer(&keys, &session.user_id) {
        Some(id) if id != info.owner.id => session.api.user(id).await?,
        _ => info.owner.clone(),
    };
    let verified = session
        .verify_user(&signer)?
        .ok_or_else(|| anyhow!("the note's owner has no encryption key"))?;
    if verified.pin == PinStatus::Changed {
        bail!(
            "SECURITY: the account key of {}, who shared this note, changed since it was pinned (now {}); \
             verify it with them before using the note",
            signer.display_name,
            verified.fingerprint
        );
    }
    let signer_key = signer.account_key.as_ref().expect("verified").verify()?;
    Ok((
        engine::object_keyring_for_recipient(
            &session.account,
            &session.user_id,
            vault_id,
            &keys,
            Some(&signer_key),
        )?,
        keys.role,
    ))
}

/// Pick a shared note by object id or (unique, case-insensitive) title.
pub async fn find_shared(session: &Session, reference: &str) -> Result<SharedNoteView> {
    let mut all = shared_with_me(session).await?;
    if let Some(i) = all.iter().position(|s| s.info.object_id == reference) {
        return Ok(all.remove(i));
    }
    let matches: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, s)| s.title.eq_ignore_ascii_case(reference))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [i] => Ok(all.remove(*i)),
        [] => {
            bail!("no note named '{reference}' is shared with you (see `notekpr shared list`)")
        }
        _ => bail!(
            "several shared notes are called '{reference}'; use the id from `notekpr shared list`"
        ),
    }
}

/// People a note is shared with see its title and icon from its own
/// metadata (they can't read the vault's index): send them again after a
/// rename or an icon change. Nothing for notes that aren't shared.
pub async fn refresh_meta(session: &Session, vault: &SharedVault, note_id: &str) -> Result<()> {
    for attempt in 0..2 {
        let (id, encrypted_meta) = {
            let v = vault.lock().await;
            let (Some(ring), Some(e)) = (
                v.replica.doc_keys.get(note_id),
                v.replica.index.get(note_id),
            ) else {
                return Ok(());
            };
            if !v.replica.is_shared_object(note_id) {
                return Ok(());
            }
            (
                v.id().to_string(),
                ObjectMeta::of(&e).encrypt(ring, note_id)?,
            )
        };
        match session
            .api
            .put::<_, ()>(
                &format!("/vaults/{id}/objects/{note_id}/meta"),
                &ObjectMetaUpdate { encrypted_meta },
            )
            .await
        {
            Ok(()) => return Ok(()),
            // Its key changed meanwhile: fresh keys, then once more.
            Err(e) if e.status() == Some(412) && attempt == 0 => {
                session.refresh_keys(vault).await?
            }
            Err(e) => return Err(anyhow!(e).context("updating the shared note's title")),
        }
    }
    Ok(())
}

/// Load a shared note into a recipient-only replica.
async fn load(session: &Session, s: &SharedNoteView) -> Result<VaultReplica> {
    let (ring, role) = recipient_ring(session, &s.info).await?;
    let mut r = VaultReplica::for_shared_notes(&s.info.vault_id);
    r.role = role;
    r.doc_keys.insert(s.info.object_id.clone(), ring);
    let mut since = 0;
    let mut first = true;
    loop {
        let ch: ChangesResponse = session
            .api
            .get(&format!(
                "/vaults/{}/objects/{}/changes?since={since}&snapshots={first}",
                s.info.vault_id, s.info.object_id
            ))
            .await?;
        first = false;
        since = ch.next_since;
        let more = ch.has_more;
        let (_, errors) = r.apply_changes(&ch);
        if !errors.is_empty() {
            tracing::debug!(
                "{} items of the shared note could not be decrypted (older than the share)",
                errors.len()
            );
        }
        if !more {
            break;
        }
    }
    Ok(r)
}

pub async fn read_shared(session: &Session, reference: &str) -> Result<(SharedNoteView, String)> {
    let s = find_shared(session, reference).await?;
    let r = load(session, &s).await?;
    let text = r.note_text(&s.info.object_id);
    Ok((s, text))
}

/// Push one change of a shared note. `Ok(false)`: refused for an outdated
/// key (the note's key was rotated meanwhile: reload and redo it).
async fn push_shared(session: &Session, vault_id: &str, mut push: PushUpdate) -> Result<bool> {
    members::sign_push(session.account(), vault_id, &mut push);
    match session.api.push(vault_id, vec![push]).await {
        Ok(r) => match r.rejected.first() {
            None => Ok(true),
            Some(x) if x.code == REJECT_STALE_KEY => Ok(false),
            Some(x) => bail!("the server refused the change: {}", x.message),
        },
        Err(ApiError::Status { status: 412, .. }) => Ok(false),
        Err(e) => Err(anyhow!(e).context("uploading the change")),
    }
}

/// Replace (or append to) a shared note's text. Changes are merged as text
/// diffs, so concurrent edits by others are preserved.
pub async fn write_shared(
    session: &Session,
    reference: &str,
    text: &str,
    append: bool,
) -> Result<SharedNoteView> {
    let s = find_shared(session, reference).await?;
    if !s.info.role.can_write() {
        bail!("this note is shared with you read-only");
    }
    let text = &*crate::ops::with_dates(text);
    for _ in 0..2 {
        let mut r = load(session, &s).await?;
        let oid = s.info.object_id.clone();
        let note = r.notes.entry(oid.clone()).or_default();
        let new = if append {
            let cur = note.text();
            let sep = if cur.is_empty() || cur.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{cur}{sep}{text}")
        } else {
            text.to_string()
        };
        let Some(update) = note.set_text(&new) else {
            return Ok(s);
        };
        let push = r.local_change(&oid, update)?;
        // Refused for a rotated key: reload keys and redo the edit.
        if push_shared(session, &s.info.vault_id, push).await? {
            return Ok(s);
        }
    }
    bail!("the note's key keeps changing; try again")
}

/// The keys of the people who commented on a shared note, as far as we
/// can trust them: the owner's (as pinned when the note's keys were
/// checked), ours, and others' pinned on first use. A key that changed
/// since it was pinned isn't used: their comments show as unverified.
async fn commenter_keys(
    session: &Session,
    s: &SharedNoteView,
    r: &VaultReplica,
) -> HashMap<String, AccountPublic> {
    let mut keys = HashMap::new();
    keys.insert(session.user_id.clone(), session.account().public());
    let authors: Vec<String> = r
        .comments
        .get(&s.info.object_id)
        .map(|log| {
            log.entries()
                .iter()
                .map(|e| e.record.author.clone())
                .collect()
        })
        .unwrap_or_default();
    for id in authors {
        if keys.contains_key(&id) {
            continue;
        }
        let user = if id == s.info.owner.id {
            s.info.owner.clone()
        } else {
            match session.api.user(&id).await {
                Ok(u) => u,
                Err(_) => continue,
            }
        };
        let trusted =
            matches!(session.verify_user(&user), Ok(Some(v)) if v.pin != PinStatus::Changed);
        if let (true, Some(k)) = (trusted, &user.account_key)
            && let Ok(key) = k.verify()
        {
            keys.insert(id, key);
        }
    }
    keys
}

fn shared_threads(
    s: &SharedNoteView,
    r: &VaultReplica,
    keys: &HashMap<String, AccountPublic>,
) -> Vec<comments::Thread> {
    r.comment_threads(
        &s.info.object_id,
        |u| keys.get(u).cloned(),
        |u| u == s.info.owner.id,
    )
}

/// The comment threads of a note shared with us (by title or id).
pub async fn shared_comments(session: &Session, reference: &str) -> Result<NoteComments> {
    let s = find_shared(session, reference).await?;
    let r = load(session, &s).await?;
    let keys = commenter_keys(session, &s, &r).await;
    let doc = r.note(&s.info.object_id);
    let raw = shared_threads(&s, &r, &keys)
        .into_iter()
        .map(|t| {
            let on = doc.is_some_and(|d| comments::locate(d, &t.anchor).is_some());
            (t, on)
        })
        .collect();
    Ok(NoteComments {
        vault_id: s.info.vault_id.clone(),
        path: s.title.clone(),
        threads: crate::ops::thread_items(session, raw).await,
    })
}

/// A new comment on a shared note, or something done to one of its comments.
pub enum SharedCommentAct {
    /// Comment on the text `on` (it must be in the note once).
    Add { on: String, text: String },
    /// Reply to, edit, delete, resolve or reopen comment `id` (or the start
    /// of it).
    On { id: String, act: CommentAct },
}

/// The people someone a note is shared with may mention: those who
/// commented on it, and its owner (docs/comments.md §1).
async fn shared_people(session: &Session, s: &SharedNoteView, r: &VaultReplica) -> Vec<Person> {
    let mut ids: Vec<String> = r
        .comments
        .get(&s.info.object_id)
        .map(|log| {
            log.entries()
                .iter()
                .map(|e| e.record.author.clone())
                .collect()
        })
        .unwrap_or_default();
    ids.retain(|u| *u != session.user_id && *u != s.info.owner.id);
    ids.sort();
    ids.dedup();
    let names = crate::ops::user_names(session, ids.clone()).await;
    let mut people = vec![(s.info.owner.id.clone(), s.info.owner.display_name.clone())];
    people.extend(
        ids.into_iter()
            .filter_map(|u| names.get(&u).map(|n| (u.clone(), n.clone()))),
    );
    people
}

/// Comment on a note shared with us (by title or id), or act on one of its
/// comments. Sent at once (recipients keep no local copy).
pub async fn shared_comment(
    session: &Session,
    reference: &str,
    act: SharedCommentAct,
) -> Result<CommentSent> {
    let s = find_shared(session, reference).await?;
    if !s.info.can_comment {
        bail!(
            "you can't comment on '{}': its owner doesn't let viewers comment",
            s.title
        );
    }
    let note = s.info.object_id.clone();
    for _ in 0..2 {
        let mut r = load(session, &s).await?;
        let text = match &act {
            SharedCommentAct::Add { text, .. } => Some(text.as_str()),
            SharedCommentAct::On { act, .. } => act.text(),
        };
        let mentioned = match text {
            Some(t) if t.contains('@') => {
                let people = shared_people(session, &s, &r).await;
                crate::mentions::resolve(t, Some(&people))
            }
            _ => Default::default(),
        };
        let action = match &act {
            SharedCommentAct::Add { on, text } => {
                let doc = r
                    .note(&note)
                    .ok_or_else(|| anyhow!("the note's text isn't available"))?;
                comments::Action::Comment {
                    body: text.clone(),
                    anchor: vault::anchor_on(doc, on, &s.title)?,
                }
            }
            SharedCommentAct::On { id, act: a } => {
                let keys = commenter_keys(session, &s, &r).await;
                let threads = shared_threads(&s, &r, &keys);
                let (target, thread) = vault::find_comment(
                    &threads,
                    id,
                    &s.title,
                    a,
                    &session.user_id,
                    &s.info.owner.id,
                )?;
                match a {
                    CommentAct::Reply(body) => comments::Action::Reply {
                        thread,
                        body: body.clone(),
                    },
                    CommentAct::Edit(body) => comments::Action::Edit {
                        target,
                        body: body.clone(),
                    },
                    CommentAct::Delete => comments::Action::Delete { target },
                    CommentAct::Resolve => comments::Action::Resolve { thread },
                    CommentAct::Reopen => comments::Action::Reopen { thread },
                }
            }
        };
        let record = comments::Record::new(&session.user_id, notekpr_core::now_ms(), action)
            .mentioning(mentioned.mentions.clone());
        let signed = comments::Signed::sign(session.account(), &s.info.vault_id, &note, &record)
            .map_err(|e| anyhow!(e))?;
        let push = r.local_comment(&note, &signed)?;
        if push_shared(session, &s.info.vault_id, push).await? {
            return Ok(CommentSent {
                id: record.id,
                vault_id: s.info.vault_id.clone(),
                note_id: note,
                mentioned,
            });
        }
    }
    bail!("the note's key keeps changing; try again")
}
