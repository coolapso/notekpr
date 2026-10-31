//! Notes shared with us individually (we are not members of their vault).

use super::*;

// ------------------------------------------------------------------ notes shared with us

/// Load the list of notes shared with us individually, their keys and titles.
pub async fn refresh_shares() {
    refresh_shares_with(false).await
}

/// Like [`refresh_shares`], and tell the user about notes newly shared with them.
pub(super) async fn refresh_shares_announcing() {
    refresh_shares_with(true).await
}

pub(super) async fn refresh_shares_with(announce: bool) {
    let list: Vec<SharedNote> = match api::get("/shares").await {
        Ok(l) => l,
        Err(e) => {
            if !e.is_offline() {
                util::warn(&format!("could not load shared notes: {e}"));
            }
            return;
        }
    };
    let (list, before) = step(|c| {
        // Notes of vaults we are a member of are reachable through the vault.
        let list: Vec<SharedNote> = list
            .into_iter()
            .filter(|s| !c.replicas.get(&s.vault_id).is_some_and(|r| r.member))
            .collect();
        let keep: BTreeSet<(String, String)> = list
            .iter()
            .map(|s| (s.vault_id.clone(), s.object_id.clone()))
            .collect();
        crate::cache::prune_shares(c, &keep);
        let removed: Vec<(String, String)> = c
            .shares
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect();
        for (v, o) in removed {
            drop_share(c, &v, &o);
        }
        let before: BTreeSet<(String, String)> = c.shares.keys().cloned().collect();
        (list, before)
    });
    for info in list {
        load_share(info).await;
    }
    let new: Vec<(String, String)> = step(|c| {
        publish_shares(c);
        c.shares
            .iter()
            .filter(|(k, s)| !before.contains(*k) && s.error.is_none())
            .map(|(_, s)| (s.info.owner.display_name.clone(), s.title.clone()))
            .collect()
    });
    if announce {
        for (owner, title) in new {
            info(format!(
                "{owner} shared “{title}” with you. It's under “Shared with me” in the sidebar."
            ));
        }
    }
}

pub(super) async fn load_share(info: SharedNote) {
    let key = (info.vault_id.clone(), info.object_id.clone());
    let current = step(|c| {
        c.shares
            .get(&key)
            .map(|s| s.error.is_none() && s.info.key_epoch == info.key_epoch)
    });
    if current == Some(true) {
        // Same key: only the title, icon or role may have changed.
        step(|c| {
            let meta = c
                .replicas
                .get(&key.0)
                .and_then(|r| r.doc_keys.get(&key.1))
                .and_then(|ring| {
                    info.encrypted_meta
                        .as_ref()
                        .and_then(|m| ObjectMeta::decrypt(ring, &key.1, m).ok())
                });
            if let Some(s) = c.shares.get_mut(&key) {
                if let Some(m) = meta {
                    (s.title, s.icon) = (m.title, m.icon);
                }
                s.info = info;
            }
        });
        return;
    }
    // Not subscribed yet: new, or its key could not be loaded before.
    let (unsubscribed, uid) = step(|c| {
        let unsubscribed = c.shares.get(&key).is_none_or(|s| s.error.is_some());
        if current.is_none() {
            c.shares.insert(
                key.clone(),
                ShareState {
                    info: info.clone(),
                    title: "Shared note".into(),
                    icon: None,
                    seq: 0,
                    loaded: false,
                    synced: false,
                    error: None,
                },
            );
        }
        (unsubscribed, c.user_id.clone())
    });
    reload_share_keys(&key.0, &key.1).await;
    if !unsubscribed || !step(|c| c.shares.get(&key).is_some_and(|s| s.error.is_none())) {
        return;
    }
    // This browser's copy first: shown right away, then caught up from.
    if current.is_none()
        && let Some(seq) = crate::cache::restore_share(&uid, &key.0, &key.1).await
    {
        step(|c| {
            if let Some(s) = c.shares.get_mut(&key) {
                s.seq = s.seq.max(seq);
                s.loaded = true;
            }
        });
        if ui().route.get_untracked().shared() == Some((key.0.as_str(), key.1.as_str())) {
            ui().content_rev.update(|v| *v += 1);
        }
    }
    step(|c| subscribe_share(c, &key.0, &key.1));
}

/// (Re)load the document key of a note shared with us.
pub async fn reload_share_keys(vault: &str, note: &str) {
    let tag = format!("{vault}/{note}");
    if !begin_reload(&tag).await {
        return;
    }
    loop {
        reload_share_keys_once(vault, note).await;
        let done = step(|c| {
            let again = c.reload_again.remove(&tag);
            if !again {
                c.reloading.remove(&tag);
            }
            !again
        });
        if done {
            break;
        }
    }
}

pub(super) async fn reload_share_keys_once(vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    let res = api::get::<ObjectKeysResponse>(&format!("/vaults/{vault}/objects/{note}/keys")).await;
    let keys = match res {
        Ok(k) => k,
        Err(e) if e.status == 404 => {
            step(|c| {
                drop_share(c, vault, note);
                publish_shares(c);
            });
            return;
        }
        Err(e) => {
            step(|c| {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e.message.clone());
                }
                publish_shares(c);
            });
            return;
        }
    };
    // Who signed giving us its newest key: the vault's owner we know, or
    // another owner, looked up.
    let known = step(|c| {
        let owner = c.shares.get(&key)?.info.owner.clone();
        Some(match engine::grant_signer(&keys, &c.user_id) {
            Some(id) if id != owner.id => Err(id.to_string()),
            _ => Ok(owner),
        })
    });
    let signer = match known {
        None => return,
        Some(Ok(owner)) => Ok(owner),
        Some(Err(id)) => api::get::<PublicUser>(&format!("/users/{id}"))
            .await
            .map_err(|e| e.message),
    };
    let applied = step(|c| {
        // Only a key an owner (as pinned) signed giving us.
        let ring = match signer.and_then(|s| share_keyring(c, vault, note, &s, &keys)) {
            Ok(r) => r,
            Err(e) => {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e);
                }
                publish_shares(c);
                return None;
            }
        };
        let meta = keys
            .encrypted_meta
            .as_ref()
            .and_then(|m| ObjectMeta::decrypt(&ring, note, m).ok());
        if let Some(s) = c.shares.get_mut(&key) {
            s.error = None;
            s.info.role = keys.role;
            s.info.key_epoch = keys.current_epoch;
            if let Some(m) = meta {
                (s.title, s.icon) = (m.title, m.icon);
            }
        }
        // The replica's role only gates sending; each note's own role is checked by the server.
        let role = if c
            .shares
            .iter()
            .any(|((v, _), s)| v == vault && s.info.role.can_write())
        {
            Role::Editor
        } else {
            Role::Viewer
        };
        let r = c
            .replicas
            .entry(vault.to_string())
            .or_insert_with(|| VaultReplica::for_shared_notes(vault));
        let applied = if r.member {
            engine::Applied::default()
        } else {
            r.doc_keys.insert(note.to_string(), ring);
            r.role = role;
            r.retry_deferred().0
        };
        publish_shares(c);
        Some(applied)
    });
    if let Some(a) = applied {
        after_apply(vault, a);
    }
}

/// The keyring of a note shared with us, if `signer` — an owner of its
/// vault, their key as pinned — signed giving us its newest key; `Err` says
/// why not.
fn share_keyring(
    c: &mut Core,
    vault: &str,
    note: &str,
    signer: &PublicUser,
    keys: &ObjectKeysResponse,
) -> Result<notekpr_core::keys::VaultKeyring, String> {
    if !c
        .shares
        .contains_key(&(vault.to_string(), note.to_string()))
    {
        return Err("the note is no longer shared with you".into());
    }
    let Some(key) = signer.account_key.as_ref().and_then(|k| k.verify().ok()) else {
        return Err("the note's owner has no valid encryption key".into());
    };
    match engine::check_pin(&mut c.prefs.pins, &signer.id, &key.fingerprint()) {
        engine::PinStatus::Changed => {
            return Err(format!(
                "Security warning: the encryption key of {}, who shared this note, changed since you last saw it. Compare fingerprints with them.",
                signer.display_name
            ));
        }
        engine::PinStatus::New => save_prefs(c),
        engine::PinStatus::Match => {}
    }
    let account = c.account.as_ref().ok_or("no account keys on this device")?;
    engine::object_keyring_for_recipient(account, &c.user_id, vault, keys, Some(&key))
        .map_err(|e| e.to_string())
}

pub(super) fn drop_share(c: &mut Core, vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    if c.shares.remove(&key).is_none() {
        return;
    }
    crate::cache::forget_share(c, vault, note);
    send(
        c,
        &ClientMessage::Unsubscribe {
            vault_id: vault.to_string(),
            object_id: Some(note.to_string()),
        },
    );
    let last = !c.shares.keys().any(|(v, _)| v == vault);
    if let Some(r) = c.replicas.get_mut(vault).filter(|r| !r.member) {
        r.doc_keys.remove(note);
        r.notes.remove(note);
        r.pending.retain(|p| p.object_id != note);
        r.deferred.clear();
        if last {
            c.replicas.remove(vault);
            c.shares_restored.remove(vault);
        }
    }
    c.outboxes.remove(vault);
    persist_pending(c, vault);
    update_pending_count(c);
    if ui().route.get_untracked().shared() == Some((vault, note)) {
        navigate(Route::Home);
    }
}

pub fn publish_shares(c: &Core) {
    let mut views: Vec<SharedView> = c
        .shares
        .iter()
        .map(|((v, n), s)| SharedView {
            vault: v.clone(),
            note: n.clone(),
            title: s.title.clone(),
            icon: s.icon.clone(),
            role: s.info.role.clone(),
            owner: s.info.owner.display_name.clone(),
            added_at: s.info.added_at,
            loaded: s.loaded,
            error: s.error.clone(),
        })
        .collect();
    views.sort_by_key(|v| v.title.to_lowercase());
    let ui = ui();
    if ui.shared.get_untracked() != views {
        ui.shared.set(views);
    }
}

pub(super) fn subscribe_share(c: &Core, vault: &str, note: &str) {
    if let Some(since) = c
        .shares
        .get(&(vault.to_string(), note.to_string()))
        .map(|s| s.seq)
    {
        send(
            c,
            &ClientMessage::Subscribe {
                vault_id: vault.to_string(),
                since: Some(since),
                object_id: Some(note.to_string()),
            },
        );
    }
}

/// A shared note's catch-up is complete (an edge).
pub(super) fn on_share_synced(vault: &str, note: &str, head: i64) {
    let (first, applied) = step(|c| {
        let Some(s) = c.shares.get_mut(&(vault.to_string(), note.to_string())) else {
            return (false, engine::Applied::default());
        };
        s.seq = s.seq.max(head);
        let first = !s.synced;
        s.loaded = true;
        s.synced = true;
        let mut applied = engine::Applied::default();
        if first {
            publish_shares(c);
            // Unsent changes from an earlier visit, once every shared note of
            // this vault is loaded.
            let all = c
                .shares
                .iter()
                .filter(|((v, _), _)| v == vault)
                .all(|(_, s)| s.synced || s.error.is_some());
            if all && c.shares_restored.insert(vault.to_string()) {
                applied = restore_stored_pending(c, vault);
            }
            if ui().route.get_untracked().shared() == Some((vault, note)) {
                ui().content_rev.update(|v| *v += 1);
                send_presence(c);
            }
            // Keep this browser's copy current.
            c.cache.mark(vault, note);
            crate::cache::schedule(c);
        }
        resend_pending(c, vault);
        (first, applied)
    });
    if first {
        after_apply(vault, applied);
    }
}

/// Whether we reach this vault as a member (not only through shared notes).
pub fn is_member(c: &Core, vault: &str) -> bool {
    c.replicas.get(vault).is_some_and(|r| r.member)
}

/// Whether a note of a vault we are a member of has its own key (is shared individually).
pub fn is_shared_note(c: &Core, vault: &str, note: &str) -> bool {
    c.replicas
        .get(vault)
        .is_some_and(|r| r.member && r.is_shared_object(note))
}

/// Whether we may edit a note (vault role, or the role of its individual share).
pub fn can_write_note(c: &Core, vault: &str, note: &str) -> bool {
    match c.replicas.get(vault) {
        Some(r) if r.member => r.can_write(),
        _ => c
            .shares
            .get(&(vault.to_string(), note.to_string()))
            .is_some_and(|s| s.info.role.can_write()),
    }
}

/// Reload whatever keys are missing for content of this vault.
pub(super) fn keys_missing(c: &Core, vault: &str) {
    if is_member(c, vault) {
        leptos::task::spawn_local(reload_vault_keys(vault.to_string()));
    } else {
        let notes: Vec<String> = c
            .shares
            .keys()
            .filter(|(v, _)| v == vault)
            .map(|(_, n)| n.clone())
            .collect();
        let v = vault.to_string();
        leptos::task::spawn_local(async move {
            for n in notes {
                reload_share_keys(&v, &n).await;
            }
            step(|c| resend_pending(c, &v));
        });
    }
}

/// Reload one note's keys (it got or rotated its own key), or the whole
/// vault's from servers that can't hand out one note's keys to members.
async fn reload_note_keys(vault: String, note: String) {
    let Some(ring) = step(|c| c.replicas.get(&vault).map(|r| r.keyring.clone())) else {
        return;
    };
    let r = match api::get::<ObjectKeysResponse>(&format!("/vaults/{vault}/objects/{note}/keys"))
        .await
    {
        Ok(r) => r,
        Err(e) if e.status == 404 => return,
        Err(_) => return reload_vault_keys(vault).await,
    };
    let Some(doc) = engine::object_keyring_for_member(&ring, &r) else {
        return reload_vault_keys(vault).await;
    };
    let shared = r.shared.unwrap_or(false);
    let res = step(|c| {
        let rep = c.replicas.get_mut(&vault)?;
        let changed = rep.doc_keys.get(&note).map(|k| k.current_epoch())
            != Some(doc.current_epoch())
            || rep.shared.contains(&note) != shared;
        c.outboxes
            .entry(vault.clone())
            .or_default()
            .note_keys_reloaded(rep, &note, doc, shared);
        let (applied, _) = rep.retry_deferred();
        persist_new_keys(c, &vault, &[]);
        // Unsent edits of the note are encrypted again with its new key.
        if changed && c.loaded.contains(&vault) {
            resend_pending(c, &vault);
        }
        Some((applied, changed))
    });
    let Some((applied, changed)) = res else {
        return;
    };
    after_apply(&vault, applied);
    if changed {
        ui().tree_rev.update(|v| *v += 1);
        ui().shares_rev.update(|v| *v += 1);
    }
}

/// An event about one note from the server (an edge).
pub(super) fn on_object_event(vault: String, note: String, event: ObjectEventKind) {
    if step(|c| is_member(c, &vault)) {
        match event {
            ObjectEventKind::Shared | ObjectEventKind::KeyRotated => {
                leptos::task::spawn_local(reload_note_keys(vault, note))
            }
            // An event from a newer release: reload what we know of it.
            ObjectEventKind::Other => {
                ui().shares_rev.update(|v| *v += 1);
                leptos::task::spawn_local(reload_note_keys(vault, note))
            }
            ObjectEventKind::SharesChanged => {
                ui().shares_rev.update(|v| *v += 1);
                // Someone it was shared with may have gone: the owner
                // renews its key.
                if step(|c| {
                    c.replicas
                        .get(&vault)
                        .is_some_and(|r| r.role == Role::Owner)
                }) {
                    leptos::task::spawn_local(async move {
                        crate::actions::renew_pending_keys(&vault).await
                    });
                }
            }
            // Members read titles from the vault index.
            ObjectEventKind::MetaChanged
            | ObjectEventKind::AccessRevoked
            | ObjectEventKind::Deleted => {}
        }
        return;
    }
    match event {
        ObjectEventKind::Shared | ObjectEventKind::KeyRotated => {
            leptos::task::spawn_local(async move {
                reload_share_keys(&vault, &note).await;
                step(|c| resend_pending(c, &vault));
            });
        }
        ObjectEventKind::MetaChanged | ObjectEventKind::SharesChanged => {
            leptos::task::spawn_local(refresh_shares())
        }
        // An event from a newer release: reload what we know of it.
        ObjectEventKind::Other => leptos::task::spawn_local(async move {
            reload_share_keys(&vault, &note).await;
            refresh_shares().await;
            step(|c| resend_pending(c, &vault));
        }),
        ObjectEventKind::AccessRevoked | ObjectEventKind::Deleted => {
            let title = step(|c| {
                let title = c
                    .shares
                    .get(&(vault.clone(), note.clone()))
                    .map(|s| s.title.clone());
                drop_share(c, &vault, &note);
                publish_shares(c);
                title
            });
            if let Some(t) = title {
                info(if event == ObjectEventKind::Deleted {
                    format!("“{t}” was deleted.")
                } else {
                    format!("“{t}” is no longer shared with you.")
                });
            }
        }
    }
}
