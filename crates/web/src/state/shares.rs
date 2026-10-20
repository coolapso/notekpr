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
    // Notes of vaults we are a member of are reachable through the vault.
    let list: Vec<SharedNote> = step(|c| {
        list.into_iter()
            .filter(|s| !c.replicas.get(&s.vault_id).is_some_and(|r| r.member))
            .collect()
    });
    let keep: BTreeSet<(String, String)> = list
        .iter()
        .map(|s| (s.vault_id.clone(), s.object_id.clone()))
        .collect();
    crate::cache::prune_shares(&keep);
    let removed: Vec<(String, String)> = step(|c| {
        c.shares
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect()
    });
    for (v, o) in removed {
        drop_share(&v, &o);
    }
    let before: BTreeSet<(String, String)> = step(|c| c.shares.keys().cloned().collect());
    for info in list {
        load_share(info).await;
    }
    publish_shares();
    if announce {
        let new: Vec<(String, String)> = step(|c| {
            c.shares
                .iter()
                .filter(|(k, s)| !before.contains(*k) && s.error.is_none())
                .map(|(_, s)| (s.info.owner.display_name.clone(), s.title.clone()))
                .collect()
        });
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
        // Same key: only the title or role may have changed.
        step(|c| {
            let title = c
                .replicas
                .get(&key.0)
                .and_then(|r| r.doc_keys.get(&key.1))
                .and_then(|ring| {
                    info.encrypted_meta
                        .as_ref()
                        .and_then(|m| ObjectMeta::decrypt(ring, &key.1, m).ok())
                })
                .map(|m| m.title);
            if let Some(s) = c.shares.get_mut(&key) {
                if let Some(t) = title {
                    s.title = t;
                }
                s.info = info;
            }
        });
        return;
    }
    // Not subscribed yet: new, or its key could not be loaded before.
    let unsubscribed = step(|c| c.shares.get(&key).is_none_or(|s| s.error.is_some()));
    if current.is_none() {
        step(|c| {
            c.shares.insert(
                key.clone(),
                ShareState {
                    info: info.clone(),
                    title: "Shared note".into(),
                    seq: 0,
                    loaded: false,
                    synced: false,
                    error: None,
                },
            )
        });
    }
    reload_share_keys(&key.0, &key.1).await;
    if unsubscribed && step(|c| c.shares.get(&key).is_some_and(|s| s.error.is_none())) {
        // This browser's copy first: shown right away, then caught up from.
        if current.is_none() {
            let uid = step(|c| c.user_id.clone());
            if let Some(seq) = crate::cache::restore_share(&uid, &key.0, &key.1).await {
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
        }
        subscribe_share(&key.0, &key.1);
    }
}

/// (Re)load the document key of a note shared with us.
pub async fn reload_share_keys(vault: &str, note: &str) {
    let tag = format!("{vault}/{note}");
    if !begin_reload(&tag).await {
        return;
    }
    loop {
        reload_share_keys_once(vault, note).await;
        if !step(|c| c.reload_again.remove(&tag)) {
            break;
        }
    }
    step(|c| c.reloading.remove(&tag));
}

pub(super) async fn reload_share_keys_once(vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    let res = api::get::<ObjectKeysResponse>(&format!("/vaults/{vault}/objects/{note}/keys")).await;
    let keys = match res {
        Ok(k) => k,
        Err(e) if e.status == 404 => {
            drop_share(vault, note);
            publish_shares();
            return;
        }
        Err(e) => {
            step(|c| {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e.message.clone());
                }
            });
            publish_shares();
            return;
        }
    };
    // Only a key the note's owner (as pinned) signed giving us.
    let (ring, pinned) = step(|c| {
        let Some(owner) = c.shares.get(&key).map(|s| s.info.owner.clone()) else {
            return (None, false);
        };
        let Some(owner_key) = owner.account_key.as_ref().and_then(|k| k.verify().ok()) else {
            let e = "the note's owner has no valid encryption key".to_string();
            return (Some(Err(e)), false);
        };
        let pin = engine::check_pin(&mut c.prefs.pins, &owner.id, &owner_key.fingerprint());
        if pin == engine::PinStatus::Changed {
            let e = format!(
                "Security warning: the encryption key of {}, who shared this note, changed since you last saw it. Compare fingerprints with them.",
                owner.display_name
            );
            return (Some(Err(e)), false);
        }
        let ring = c.account.as_ref().map(|a| {
            engine::object_keyring_for_recipient(a, &c.user_id, vault, &keys, Some(&owner_key))
                .map_err(|e| e.to_string())
        });
        (ring, pin == engine::PinStatus::New)
    });
    if pinned {
        save_prefs();
    }
    let ring = match ring {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            step(|c| {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e.to_string());
                }
            });
            publish_shares();
            return;
        }
        None => return,
    };
    let applied = step(|c| {
        let title = keys
            .encrypted_meta
            .as_ref()
            .and_then(|m| ObjectMeta::decrypt(&ring, note, m).ok())
            .map(|m| m.title);
        if let Some(s) = c.shares.get_mut(&key) {
            s.error = None;
            s.info.role = keys.role;
            s.info.key_epoch = keys.current_epoch;
            if let Some(t) = title {
                s.title = t;
            }
        }
        let r = c
            .replicas
            .entry(vault.to_string())
            .or_insert_with(|| VaultReplica::for_shared_notes(vault));
        if r.member {
            return engine::Applied::default();
        }
        r.doc_keys.insert(note.to_string(), ring);
        // The replica's role only gates sending; each note's own role is checked by the server.
        r.role = if c
            .shares
            .iter()
            .any(|((v, _), s)| v == vault && s.info.role.can_write())
        {
            Role::Editor
        } else {
            Role::Viewer
        };
        r.retry_deferred().0
    });
    after_apply(vault, applied);
    publish_shares();
}

pub(super) fn drop_share(vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    let Some(_) = step(|c| c.shares.remove(&key)) else {
        return;
    };
    crate::cache::forget_share(vault, note);
    send(&ClientMessage::Unsubscribe {
        vault_id: vault.to_string(),
        object_id: Some(note.to_string()),
    });
    step(|c| {
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
    });
    persist_pending(vault);
    update_pending_count();
    if ui().route.get_untracked().shared() == Some((vault, note)) {
        navigate(Route::Home);
    }
}

pub fn publish_shares() {
    let mut views: Vec<SharedView> = step(|c| {
        c.shares
            .iter()
            .map(|((v, n), s)| SharedView {
                vault: v.clone(),
                note: n.clone(),
                title: s.title.clone(),
                role: s.info.role,
                owner: s.info.owner.display_name.clone(),
                added_at: s.info.added_at,
                loaded: s.loaded,
                error: s.error.clone(),
            })
            .collect()
    });
    views.sort_by_key(|v| v.title.to_lowercase());
    let ui = ui();
    if ui.shared.get_untracked() != views {
        ui.shared.set(views);
    }
}

pub(super) fn subscribe_share(vault: &str, note: &str) {
    let since = step(|c| {
        c.shares
            .get(&(vault.to_string(), note.to_string()))
            .map(|s| s.seq)
    });
    if let Some(since) = since {
        send(&ClientMessage::Subscribe {
            vault_id: vault.to_string(),
            since: Some(since),
            object_id: Some(note.to_string()),
        });
    }
}

pub(super) fn on_share_synced(vault: &str, note: &str, head: i64) {
    let first = step(|c| {
        let Some(s) = c.shares.get_mut(&(vault.to_string(), note.to_string())) else {
            return false;
        };
        s.seq = s.seq.max(head);
        let first = !s.synced;
        s.loaded = true;
        s.synced = true;
        first
    });
    if first {
        publish_shares();
        // Unsent changes from an earlier visit, once every shared note of this vault is loaded.
        let restore = step(|c| {
            c.shares
                .iter()
                .filter(|((v, _), _)| v == vault)
                .all(|(_, s)| s.synced || s.error.is_some())
                && c.shares_restored.insert(vault.to_string())
        });
        if restore {
            restore_stored_pending(vault);
        }
        if ui().route.get_untracked().shared() == Some((vault, note)) {
            ui().content_rev.update(|v| *v += 1);
            send_presence();
        }
        // Keep this browser's copy current.
        step(|c| {
            c.cache.mark(vault, note);
        });
        crate::cache::schedule();
    }
    resend_pending(vault);
}

/// Whether we reach this vault as a member (not only through shared notes).
pub fn is_member(vault: &str) -> bool {
    step(|c| c.replicas.get(vault).is_some_and(|r| r.member))
}

/// Whether a note of a vault we are a member of has its own key (is shared individually).
pub fn is_shared_note(vault: &str, note: &str) -> bool {
    step(|c| {
        c.replicas
            .get(vault)
            .is_some_and(|r| r.member && r.is_shared_object(note))
    })
}

/// Whether we may edit a note (vault role, or the role of its individual share).
pub fn can_write_note(vault: &str, note: &str) -> bool {
    step(|c| match c.replicas.get(vault) {
        Some(r) if r.member => r.can_write(),
        _ => c
            .shares
            .get(&(vault.to_string(), note.to_string()))
            .is_some_and(|s| s.info.role.can_write()),
    })
}

/// Reload whatever keys are missing for content of this vault.
pub(super) fn keys_missing(vault: &str) {
    if is_member(vault) {
        leptos::task::spawn_local(reload_vault_keys(vault.to_string()));
    } else {
        let notes: Vec<String> = step(|c| {
            c.shares
                .keys()
                .filter(|(v, _)| v == vault)
                .map(|(_, n)| n.clone())
                .collect()
        });
        let v = vault.to_string();
        leptos::task::spawn_local(async move {
            for n in notes {
                reload_share_keys(&v, &n).await;
            }
            resend_pending(&v);
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
        Some((applied, changed, c.loaded.contains(&vault)))
    });
    let Some((applied, changed, loaded)) = res else {
        return;
    };
    after_apply(&vault, applied);
    persist_new_keys(&vault, &[]);
    if changed {
        ui().tree_rev.update(|v| *v += 1);
        ui().shares_rev.update(|v| *v += 1);
        // Unsent edits of the note are encrypted again with its new key.
        if loaded {
            resend_pending(&vault);
        }
    }
}

pub(super) fn on_object_event(vault: String, note: String, event: ObjectEventKind) {
    if is_member(&vault) {
        match event {
            ObjectEventKind::Shared | ObjectEventKind::KeyRotated => {
                leptos::task::spawn_local(reload_note_keys(vault, note))
            }
            ObjectEventKind::SharesChanged => ui().shares_rev.update(|v| *v += 1),
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
                resend_pending(&vault);
            });
        }
        ObjectEventKind::MetaChanged | ObjectEventKind::SharesChanged => {
            leptos::task::spawn_local(refresh_shares())
        }
        ObjectEventKind::AccessRevoked | ObjectEventKind::Deleted => {
            let title = step(|c| {
                c.shares
                    .get(&(vault.clone(), note.clone()))
                    .map(|s| s.title.clone())
            });
            drop_share(&vault, &note);
            publish_shares();
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
