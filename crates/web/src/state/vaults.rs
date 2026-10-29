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
    step(|c| {
        crate::cache::prune(c, &ids);
        offline::note(c, |b| {
            b.vaults = infos.clone();
            b.keys.retain(|k, _| ids.contains(k));
        });
        let removed: Vec<String> = c
            .replicas
            .iter()
            .filter(|(k, r)| r.member && !ids.contains(*k))
            .map(|(k, _)| k.clone())
            .collect();
        for v in &removed {
            drop_vault(c, v);
        }
    });
    // In parallel: each needs its own key request (and local copy read).
    futures::future::join_all(infos.into_iter().map(load_vault)).await;
    step(|c| {
        publish_vaults(c);
        // A vault's limits may have changed: drawings and quick notes
        // waiting for room try again.
        room_freed(c);
    });
    // Quick notes are there from the start (once per person); ones deleted
    // offline go for good now.
    leptos::task::spawn_local(async {
        if crate::quick::ensure().await.is_some() {
            crate::quick::purge_deleted().await;
        }
    });
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
            step(|c| publish_vaults(c));
        }
        let done = step(|c| {
            let again = c.reload_again.remove(&vault_id);
            if !again {
                c.reloading.remove(&vault_id);
            }
            !again
        });
        if done {
            break;
        }
    }
}

/// Start reloading the keys behind `tag`. If a reload is already running,
/// it may have fetched the keys before they changed: ask it to run once
/// more, wait until it is done and return `false`.
pub(super) async fn begin_reload(tag: &str) -> bool {
    let first = step(|c| {
        if c.reloading.insert(tag.to_string()) {
            c.reload_again.remove(tag);
            true
        } else {
            c.reload_again.insert(tag.to_string());
            false
        }
    });
    if first {
        return true;
    }
    while step(|c| c.reloading.contains(tag)) {
        util::sleep(50).await;
    }
    false
}

/// Load a vault's keys (fetched, or `keys` from the offline data) and metadata.
pub(super) async fn load_vault_with(info: VaultInfo, force: bool, keys: Option<VaultKeysResponse>) {
    let (needs_keys, uid) = step(|c| {
        let needs = match c.replicas.get(&info.id) {
            None => true,
            Some(r) => force || !r.member || r.keyring.current_epoch() < Some(info.key_epoch),
        };
        (needs, c.user_id.clone())
    });
    let id = info.id.clone();
    if needs_keys {
        let stored = if step(|c| c.replicas.get(&id).is_none_or(|r| !r.member)) {
            crate::cache::fetch(&uid, &id).await
        } else {
            None
        };
        let keys: VaultKeysResponse = match keys {
            Some(k) => k,
            None => match api::get::<VaultKeysResponse>(&format!("/vaults/{id}/keys")).await {
                Ok(k) => {
                    step(|c| {
                        offline::note(c, |b| {
                            b.keys.insert(id.clone(), k.clone());
                        })
                    });
                    k
                }
                Err(e) => {
                    step(|c| c.vault_errors.insert(id.clone(), e.message.clone()));
                    return;
                }
            },
        };
        let ring = step(|c| {
            c.account
                .as_ref()
                .map(|a| engine::keyring_from_response(a, &uid, &id, &keys))
        });
        match ring {
            Some(Ok(ring)) => {
                // The owner's signed membership log, and our keys against it.
                match super::members::load_log(&id, &info.owner_id, &ring).await {
                    Ok(Some(log)) => {
                        step(|c| c.member_logs.insert(id.clone(), log));
                    }
                    Ok(None) => {}
                    Err(msg) => {
                        step(|c| c.vault_errors.insert(id.clone(), msg.clone()));
                        error(msg);
                        return;
                    }
                }
                let meta = VaultMeta::decrypt(&ring, &info.encrypted_meta)
                    .unwrap_or_else(|_| VaultMeta::named("(unreadable)"));
                let docs = engine::object_keyrings(&ring, &keys);
                let shared = engine::shared_objects(&keys);
                let share_from = engine::share_from(&keys);
                let note_keys = keys.shared.is_some();
                let (is_new, applied, docs_changed, restored, unsigned, offline_start) =
                    step(|c| {
                        // Notes shared with us individually are now reachable through the vault.
                        if c.replicas.get(&id).is_some_and(|r| !r.member) {
                            let notes: Vec<(String, String)> =
                                c.shares.keys().filter(|(v, _)| *v == id).cloned().collect();
                            for (v, o) in notes {
                                send(
                                    c,
                                    &ClientMessage::Unsubscribe {
                                        vault_id: v.clone(),
                                        object_id: Some(o.clone()),
                                    },
                                );
                                c.shares.remove(&(v, o));
                            }
                            c.replicas.remove(&id);
                            publish_shares(c);
                        }
                        c.vault_errors.remove(&id);
                        let (is_new, mut applied, docs_changed, restored, adopted) = match c
                            .replicas
                            .get_mut(&id)
                        {
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
                                r.role = info.role.clone();
                                r.doc_keys = docs;
                                r.shared = shared;
                                r.share_from = share_from;
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
                                let mut r = VaultReplica::new(&id, info.role.clone(), ring, meta);
                                r.doc_keys = docs;
                                r.shared = shared;
                                r.share_from = share_from;
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
                        };
                        if let Some(adopted) = &adopted {
                            persist_new_keys(c, &id, adopted);
                        }
                        applied.merge(super::members::apply_authors(c, &id));
                        if is_new {
                            subscribe(c, &id);
                        } else if docs_changed && c.loaded.contains(&id) {
                            // Unsent edits of a note that just got its own key
                            // must be encrypted again.
                            resend_pending(c, &id);
                        }
                        // Owners start the log of a vault from before it, and
                        // require signed changes.
                        let unsigned = c
                            .member_logs
                            .get(&id)
                            .is_none_or(|l| !l.has_log() || l.needs_signatures());
                        (
                            is_new,
                            applied,
                            docs_changed,
                            restored,
                            unsigned,
                            c.offline_start,
                        )
                    });
                if info.role == Role::Owner && !offline_start {
                    if unsigned {
                        leptos::task::spawn_local(super::members::sign_vault(id.clone()));
                    }
                    if is_new {
                        leptos::task::spawn_local(super::members::sign_missing_grants(id.clone()));
                    }
                    // Someone who left still knows a key: renew it.
                    if info.rotation_pending || !info.notes_rotation_pending.is_empty() {
                        let id = id.clone();
                        leptos::task::spawn_local(async move {
                            crate::actions::renew_pending_keys(&id).await
                        });
                    }
                }
                after_apply(&id, applied);
                if is_new && restored {
                    step(|c| show_loaded(c, &id));
                }
                if docs_changed {
                    ui().tree_rev.update(|v| *v += 1);
                    ui().shares_rev.update(|v| *v += 1);
                }
            }
            Some(Err(e)) => {
                step(|c| c.vault_errors.insert(id.clone(), e.to_string()));
            }
            None => {}
        }
    } else {
        step(|c| {
            if let Some(r) = c.replicas.get_mut(&id) {
                r.role = info.role.clone();
                if let Ok(m) = VaultMeta::decrypt(&r.keyring, &info.encrypted_meta) {
                    r.meta = m;
                }
            }
        });
    }
    step(|c| {
        // A changed limit may have made room for quick notes waiting here.
        let room = info.is_own_quick_notes(&c.user_id)
            && c.vault_infos
                .get(&id)
                .is_some_and(|old| old.note_limit != info.note_limit);
        c.vault_infos.insert(id.clone(), info);
        if room
            && let (Some(r), Some(o)) = (c.replicas.get_mut(&id), c.outboxes.get_mut(&id))
            && o.room_freed(r) > 0
        {
            persist_pending(c, &id);
            send_planned(c, &id);
        }
    });
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

pub(super) fn drop_vault(c: &mut Core, v: &str) {
    c.replicas.remove(v);
    c.vault_infos.remove(v);
    c.loaded.remove(v);
    c.synced.remove(v);
    c.outboxes.remove(v);
    revoke_urls(take_attachment_urls(c, Some(v)));
    crate::cache::forget_vault(c, v);
    if ui().route.get_untracked().vault() == Some(v) {
        navigate(Route::Home);
    }
}

pub fn publish_vaults(c: &Core) {
    let views = {
        // The quick notes are no vault to the person (`crate::quick`);
        // kinds from newer releases aren't shown.
        let mut v: Vec<VaultView> = c
            .vault_infos
            .values()
            .filter(|i| !i.is_own_quick_notes(&c.user_id) && i.kind != VaultKind::Other)
            .map(|i| {
                let r = c.replicas.get(&i.id);
                let meta = r.map(|r| &r.meta);
                VaultView {
                    id: i.id.clone(),
                    name: meta
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| "Locked vault".into()),
                    icon: meta.and_then(|m| m.icon.clone()),
                    picture: meta
                        .and_then(|m| m.picture.as_ref())
                        .filter(|p| p.is_valid())
                        .map(|p| {
                            format!(
                                "data:{};base64,{}",
                                p.mime,
                                notekpr_core::b64::encode(&p.data)
                            )
                        }),
                    role: i.role.clone(),
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
    };
    ui().vaults.set(views);
}
