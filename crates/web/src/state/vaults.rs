//! Vaults: loading keys and metadata, dropping lost ones, the vault list.

use super::*;

// ------------------------------------------------------------------ vaults

pub async fn refresh_vaults() {
    let infos: Vec<VaultInfo> = match api::get("/vaults").await {
        Ok(v) => v,
        Err(e) => {
            if !e.is_offline() {
                error(format!("Could not load vaults: {e}"));
            }
            return;
        }
    };
    let ids: BTreeSet<String> = infos.iter().map(|v| v.id.clone()).collect();
    crate::cache::prune(&ids);
    offline::note(|b| {
        b.vaults = infos.clone();
        b.keys.retain(|k, _| ids.contains(k));
    });
    let removed: Vec<String> = with(|c| {
        c.replicas
            .iter()
            .filter(|(k, r)| r.member && !ids.contains(*k))
            .map(|(k, _)| k.clone())
            .collect()
    });
    for v in &removed {
        drop_vault(v);
    }
    // In parallel: each needs its own key request (and local copy read).
    futures::future::join_all(infos.into_iter().map(load_vault)).await;
    publish_vaults();
}

/// Load (or refresh) one vault's keys and metadata.
pub async fn load_vault(info: VaultInfo) {
    load_vault_with(info, false, None).await
}

/// Reload a vault's keys, e.g. after a note in it got its own key.
pub async fn reload_vault_keys(vault_id: String) {
    if !begin_reload(&vault_id).await {
        return;
    }
    loop {
        if let Ok(info) = api::get::<VaultInfo>(&format!("/vaults/{vault_id}")).await {
            load_vault_with(info, true, None).await;
            publish_vaults();
        }
        if !with(|c| c.reload_again.remove(&vault_id)) {
            break;
        }
    }
    with(|c| c.reloading.remove(&vault_id));
}

/// Start reloading the keys behind `tag`. If a reload is already running,
/// it may have fetched the keys before they changed: ask it to run once
/// more, wait until it is done and return `false`.
pub(super) async fn begin_reload(tag: &str) -> bool {
    if with(|c| c.reloading.insert(tag.to_string())) {
        with(|c| c.reload_again.remove(tag));
        return true;
    }
    with(|c| c.reload_again.insert(tag.to_string()));
    while with(|c| c.reloading.contains(tag)) {
        util::sleep(50).await;
    }
    false
}

/// Load a vault's keys (fetched, or `keys` from the offline data) and metadata.
pub(super) async fn load_vault_with(info: VaultInfo, force: bool, keys: Option<VaultKeysResponse>) {
    let (needs_keys, uid) = with(|c| {
        let needs = match c.replicas.get(&info.id) {
            None => true,
            Some(r) => force || !r.member || r.keyring.current_epoch() < Some(info.key_epoch),
        };
        (needs, c.user_id.clone())
    });
    let id = info.id.clone();
    if needs_keys {
        let stored = if with(|c| c.replicas.get(&id).is_none_or(|r| !r.member)) {
            crate::cache::fetch(&uid, &id).await
        } else {
            None
        };
        let keys: VaultKeysResponse = match keys {
            Some(k) => k,
            None => match api::get::<VaultKeysResponse>(&format!("/vaults/{id}/keys")).await {
                Ok(k) => {
                    offline::note(|b| {
                        b.keys.insert(id.clone(), k.clone());
                    });
                    k
                }
                Err(e) => {
                    with(|c| c.vault_errors.insert(id.clone(), e.message.clone()));
                    return;
                }
            },
        };
        let ring = with(|c| {
            c.account
                .as_ref()
                .map(|a| engine::keyring_from_response(a, &uid, &id, &keys))
        });
        match ring {
            Some(Ok(ring)) => {
                let meta = VaultMeta::decrypt(&ring, &info.encrypted_meta)
                    .unwrap_or_else(|_| VaultMeta::named("(unreadable)"));
                let docs = engine::object_keyrings(&ring, &keys);
                // Notes shared with us individually are now reachable through the vault.
                let was_recipient = with(|c| c.replicas.get(&id).is_some_and(|r| !r.member));
                if was_recipient {
                    for (v, o) in with(|c| {
                        c.shares
                            .keys()
                            .filter(|(v, _)| *v == id)
                            .cloned()
                            .collect::<Vec<_>>()
                    }) {
                        send(&ClientMessage::Unsubscribe {
                            vault_id: v.clone(),
                            object_id: Some(o.clone()),
                        });
                        with(|c| c.shares.remove(&(v, o)));
                    }
                    with(|c| c.replicas.remove(&id));
                    publish_shares();
                }
                let shared = engine::shared_objects(&keys);
                let note_keys = keys.shared.is_some();
                let (is_new, applied, docs_changed, restored, adopted) = with(|c| {
                    c.vault_errors.remove(&id);
                    match c.replicas.get_mut(&id) {
                        Some(r) => {
                            let epochs = |r: &VaultReplica| {
                                r.doc_keys
                                    .iter()
                                    .map(|(o, k)| (o.clone(), k.current_epoch()))
                                    .collect::<Vec<_>>()
                            };
                            let before = (epochs(r), r.shared.clone());
                            r.keyring = ring;
                            r.meta = meta;
                            r.role = info.role;
                            r.doc_keys = docs;
                            r.shared = shared;
                            // New notes' keys the server doesn't have yet stay.
                            c.outboxes
                                .entry(id.clone())
                                .or_default()
                                .keys_reloaded(r, note_keys);
                            let docs_changed = before != (epochs(r), r.shared.clone());
                            let (applied, _) = r.retry_deferred();
                            (false, applied, docs_changed, false, None)
                        }
                        None => {
                            let mut r = VaultReplica::new(&id, info.role, ring, meta);
                            r.doc_keys = docs;
                            r.shared = shared;
                            r.max_head_seen = storage::head_seen(&uid, &id);
                            // Keys of new notes not registered yet, before the
                            // local copy and unsent changes that need them.
                            let adopted = adopt_stored_new_keys(c, &id);
                            c.outboxes
                                .entry(id.clone())
                                .or_default()
                                .keys_reloaded(&mut r, note_keys);
                            let restored =
                                crate::cache::restore_into(&mut c.cache, &uid, &mut r, stored);
                            if restored {
                                c.loaded.insert(id.clone());
                            }
                            c.replicas.insert(id.clone(), r);
                            (
                                true,
                                engine::Applied::default(),
                                false,
                                restored,
                                Some(adopted),
                            )
                        }
                    }
                });
                if let Some(adopted) = adopted {
                    persist_new_keys(&id, &adopted);
                }
                if is_new {
                    subscribe(&id);
                    if restored {
                        show_loaded(&id);
                    }
                } else {
                    after_apply(&id, applied);
                    if docs_changed {
                        ui().tree_rev.update(|v| *v += 1);
                        ui().shares_rev.update(|v| *v += 1);
                        // Unsent edits of a note that just got its own key must be re-encrypted.
                        if with(|c| c.loaded.contains(&id)) {
                            resend_pending(&id);
                        }
                    }
                }
            }
            Some(Err(e)) => {
                with(|c| c.vault_errors.insert(id.clone(), e.to_string()));
            }
            None => {}
        }
    } else {
        with(|c| {
            if let Some(r) = c.replicas.get_mut(&id) {
                r.role = info.role;
                if let Ok(m) = VaultMeta::decrypt(&r.keyring, &info.encrypted_meta) {
                    r.meta = m;
                }
            }
        });
    }
    with(|c| c.vault_infos.insert(id, info));
}

/// Remove the cached blob URLs of decrypted attachments (of one vault, or
/// all); the caller revokes them with [`revoke_urls`] outside the borrow.
pub(super) fn take_attachment_urls(c: &mut Core, vault: Option<&str>) -> Vec<String> {
    let prefix = vault.map(|v| format!("{v}:"));
    let keys: Vec<String> = c
        .attachment_urls
        .keys()
        .filter(|k| prefix.as_ref().is_none_or(|p| k.starts_with(p.as_str())))
        .cloned()
        .collect();
    keys.iter()
        .filter_map(|k| c.attachment_urls.remove(k))
        .collect()
}

pub(super) fn revoke_urls(urls: Vec<String>) {
    for u in urls {
        let _ = web_sys::Url::revoke_object_url(&u);
    }
}

pub(super) fn drop_vault(v: &str) {
    let urls = with(|c| {
        c.replicas.remove(v);
        c.vault_infos.remove(v);
        c.loaded.remove(v);
        c.synced.remove(v);
        c.outboxes.remove(v);
        take_attachment_urls(c, Some(v))
    });
    revoke_urls(urls);
    crate::cache::forget_vault(v);
    let ui = ui();
    if ui.route.get_untracked().vault() == Some(v) {
        navigate(Route::Home);
    }
}

pub fn publish_vaults() {
    let views = with(|c| {
        let mut v: Vec<VaultView> = c
            .vault_infos
            .values()
            .map(|i| {
                let r = c.replicas.get(&i.id);
                VaultView {
                    id: i.id.clone(),
                    name: r
                        .map(|r| r.meta.name.clone())
                        .unwrap_or_else(|| "Locked vault".into()),
                    role: i.role,
                    shared: i.member_count > 1,
                    archived: i.archived,
                    owner_id: i.owner_id.clone(),
                    loaded: c.loaded.contains(&i.id),
                    error: c.vault_errors.get(&i.id).cloned(),
                }
            })
            .collect();
        v.sort_by(|a, b| {
            a.archived
                .cmp(&b.archived)
                .then(a.shared.cmp(&b.shared))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        v
    });
    ui().vaults.set(views);
}
