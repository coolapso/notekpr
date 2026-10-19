//! Sharing single notes: the owner's side (share/unshare a note of a vault)
//! and the recipient's side (read and edit notes shared with you).
//!
//! A shared note gets its own document key; see `engine::start_sharing`.
//! Recipients never receive the vault key or the vault index.

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::engine::{self, ObjectMeta, PinStatus, VaultReplica};
use coolpsonotes_core::protocol::*;

use crate::api::ApiError;
use crate::manage::{self, ShareOutcome};
use crate::session::{Session, SharedVault};

/// Give a note its own key (first time) and share it with one person.
pub async fn share_note(
    session: &Session,
    vault: &SharedVault,
    path: &str,
    user_ref: &str,
    role: Role,
    accept_new_key: bool,
) -> Result<ShareOutcome> {
    if role == Role::Owner {
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
        engine::object_share_envelope(&v.replica.doc_keys[&note_id], &note_id, &user)?
    };
    let req = AddObjectShareRequest {
        user_id: user.id.clone(),
        role,
        envelope,
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
                    &UpdateMemberRequest { role },
                )
                .await?;
            updated_role = true;
        }
        Err(e) => return Err(anyhow!(e).context("sharing the note")),
    }
    Ok(ShareOutcome {
        user,
        fingerprint: Some(verified.fingerprint),
        pin: Some(verified.pin),
        key_shared: true,
        updated_role,
    })
}

/// Make sure a note has a document key, creating it (with a covering
/// snapshot under the new key) if needed.
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
            let title = v
                .replica
                .index
                .get(note_id)
                .map(|e| e.title().to_string())
                .unwrap_or_default();
            let state = v
                .replica
                .note(note_id)
                .map(|n| n.encode_state())
                .unwrap_or_default();
            engine::start_sharing(&v.replica.keyring, note_id, &title, &state, v.replica.seq)?
        };
        match session
            .api
            .put::<_, ()>(&format!("/vaults/{id}/objects/{note_id}/key"), &req)
            .await
        {
            Ok(()) => {
                let mut v = vault.lock().await;
                v.replica.doc_keys.insert(note_id.to_string(), ring);
                v.mark_state_dirty();
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

/// Stop sharing a note with someone and rotate the note's key.
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
    session
        .api
        .delete(&format!(
            "/vaults/{id}/objects/{note_id}/shares/{}",
            user.id
        ))
        .await?;
    rotate_note_key(session, vault, &id, &note_id).await?;
    Ok(user)
}

/// New document key for the note's remaining recipients.
pub async fn rotate_note_key(
    session: &Session,
    vault: &SharedVault,
    id: &str,
    note_id: &str,
) -> Result<()> {
    let users: Vec<PublicUser> = note_shares(session, id, note_id)
        .await?
        .into_iter()
        .map(|s| s.user)
        .collect();
    let (next, req) = {
        let v = vault.lock().await;
        let ring = v
            .replica
            .doc_keys
            .get(note_id)
            .ok_or_else(|| anyhow!("this note is not shared"))?;
        let title = v
            .replica
            .index
            .get(note_id)
            .map(|e| e.title().to_string())
            .unwrap_or_default();
        engine::rotate_object_key(&v.replica.keyring, ring, note_id, &title, &users)?
    };
    session
        .api
        .put::<_, ()>(&format!("/vaults/{id}/objects/{note_id}/key"), &req)
        .await
        .context("rotating the note key")?;
    let mut v = vault.lock().await;
    v.replica.doc_keys.insert(note_id.to_string(), next);
    v.mark_state_dirty();
    v.save()
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

/// A note shared with us, with its decrypted title.
pub struct SharedNoteView {
    pub info: SharedNote,
    pub title: String,
}

/// Notes shared with the signed-in user individually.
pub async fn shared_with_me(session: &Session) -> Result<Vec<SharedNoteView>> {
    let list: Vec<SharedNote> = session.api.get("/shares").await?;
    let mut out = Vec::with_capacity(list.len());
    for info in list {
        let title = match recipient_ring(session, &info.vault_id, &info.object_id).await {
            Ok((ring, _)) => info
                .encrypted_meta
                .as_ref()
                .and_then(|m| ObjectMeta::decrypt(&ring, &info.object_id, m).ok())
                .map(|m| m.title)
                .unwrap_or_else(|| "(untitled)".into()),
            Err(_) => "(key not available yet)".into(),
        };
        out.push(SharedNoteView { info, title });
    }
    Ok(out)
}

async fn recipient_ring(
    session: &Session,
    vault_id: &str,
    note_id: &str,
) -> Result<(coolpsonotes_core::keys::VaultKeyring, Role)> {
    let keys: ObjectKeysResponse = session
        .api
        .get(&format!("/vaults/{vault_id}/objects/{note_id}/keys"))
        .await?;
    Ok((
        engine::object_keyring_for_recipient(&session.account, &session.user_id, vault_id, &keys)?,
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
            bail!("no note named '{reference}' is shared with you (see `coolpsonotes shared list`)")
        }
        _ => bail!(
            "several shared notes are called '{reference}'; use the id from `coolpsonotes shared list`"
        ),
    }
}

/// Load a shared note into a recipient-only replica.
async fn load(session: &Session, s: &SharedNoteView) -> Result<VaultReplica> {
    let (ring, role) = recipient_ring(session, &s.info.vault_id, &s.info.object_id).await?;
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
    for attempt in 0..2 {
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
        match session.api.push(&s.info.vault_id, vec![push]).await {
            Ok(_) => return Ok(s),
            // The note's key was rotated meanwhile: reload keys and redo the edit.
            Err(ApiError::Status { status: 412, .. }) if attempt == 0 => continue,
            Err(e) => return Err(anyhow!(e).context("uploading the change")),
        }
    }
    bail!("the note's key keeps changing; try again")
}
