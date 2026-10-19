//! The WebSocket: catch-up, live updates, acknowledgements, pushing local
//! edits, unsent changes kept in local storage, compaction.

use super::*;

// ------------------------------------------------------------------ socket

pub(super) fn connect_socket() {
    let Some(token) = api::token() else { return };
    let on_message = Rc::new(|m: ServerMessage| on_server_message(m));
    let on_state = Rc::new(|s: ConnState| {
        let ui = ui();
        ui.conn.set(s);
    });
    let sock = Socket::new(token, on_message, on_state);
    with(|c| c.socket = Some(sock));
    // Reconnect promptly when the browser comes back online.
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(|| {
        if let Some(s) = with(|c| c.socket.clone()) {
            s.kick();
        }
    }) as Box<dyn FnMut()>);
    use wasm_bindgen::JsCast;
    let _ = util::window().add_event_listener_with_callback("online", cb.as_ref().unchecked_ref());
    cb.forget();
}

pub(super) fn send(m: &ClientMessage) -> bool {
    match with(|c| c.socket.clone()) {
        Some(s) if s.is_ready() => s.send(m),
        _ => false,
    }
}

pub(super) fn subscribe(vault_id: &str) {
    let since = with(|c| c.replicas.get(vault_id).map(|r| r.seq));
    if let Some(since) = since {
        send(&ClientMessage::Subscribe {
            vault_id: vault_id.to_string(),
            since: Some(since),
            object_id: None,
        });
    }
}

pub(super) fn on_server_message(m: ServerMessage) {
    match m {
        ServerMessage::Ready { conn_id, .. } => {
            with(|c| {
                c.conn_id = conn_id;
                c.presence_note = None;
            });
            let vaults: Vec<String> = with(|c| {
                c.replicas
                    .iter()
                    .filter(|(_, r)| r.member)
                    .map(|(k, _)| k.clone())
                    .collect()
            });
            for v in vaults {
                subscribe(&v);
            }
            let shares: Vec<(String, String)> = with(|c| {
                c.shares
                    .iter()
                    .filter(|(_, s)| s.error.is_none())
                    .map(|(k, _)| k.clone())
                    .collect()
            });
            for (v, n) in shares {
                subscribe_share(&v, &n);
            }
            send_presence();
        }
        ServerMessage::Changes { vault_id, changes } => {
            let res = with(|c| {
                let r = c.replicas.get_mut(&vault_id)?;
                let res = r.apply_changes(&changes);
                Some((res, r.needs_keys()))
            });
            if let Some(((applied, errors), needs_keys)) = res {
                if !errors.is_empty() {
                    util::warn(&format!(
                        "{} items in vault could not be decrypted",
                        errors.len()
                    ));
                }
                after_apply(&vault_id, applied);
                if needs_keys {
                    keys_missing(&vault_id);
                }
            }
        }
        ServerMessage::Synced {
            vault_id,
            head_seq,
            object_id,
        } => match object_id {
            Some(o) => on_share_synced(&vault_id, &o, head_seq),
            None => on_synced(&vault_id, head_seq),
        },
        ServerMessage::Update { vault_id, update } => {
            with(|c| {
                if let Some(s) = c
                    .shares
                    .get_mut(&(vault_id.clone(), update.object_id.clone()))
                {
                    s.seq = s.seq.max(update.seq);
                }
            });
            let res = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_update(&update))
            });
            match res {
                Some(Ok(applied)) => after_apply(&vault_id, applied),
                Some(Err(e)) if engine::missing_key(&e) => keys_missing(&vault_id),
                Some(Err(e)) => util::warn(&format!("could not apply update: {e}")),
                None => {}
            }
        }
        ServerMessage::Ack { vault_id, results } => {
            with(|c| {
                if let Some(r) = c.replicas.get_mut(&vault_id) {
                    r.ack(&results, true);
                }
            });
            persist_pending(&vault_id);
            update_pending_count();
            maybe_compact(&vault_id);
            crate::cache::schedule();
        }
        ServerMessage::SnapshotCreated {
            vault_id,
            object_id,
            ..
        } => {
            with(|c| {
                if let Some(r) = c.replicas.get_mut(&vault_id) {
                    r.snapshot_created(&object_id);
                }
            });
        }
        ServerMessage::Purged {
            vault_id,
            object_id,
            seq,
        } => {
            // Someone permanently deleted a note we still had unsent edits
            // for: keep our version as a new note rather than losing it.
            let rescue = with(|c| {
                let r = c.replicas.get(&vault_id)?;
                if !r.pending.iter().any(|p| p.object_id == object_id) {
                    return None;
                }
                let title = r
                    .index
                    .get(&object_id)
                    .map(|e| e.title().to_string())
                    .unwrap_or_else(|| "Untitled".into());
                Some((title, r.note_text(&object_id)))
            });
            let had_pending = rescue.is_some();
            if let Some((title, text)) = rescue {
                let v = vault_id.clone();
                leptos::task::spawn_local(async move {
                    let name = format!("{title} (recovered)");
                    crate::actions::create_note_with(&v, "", &name, &text).await;
                    info(format!(
                        "“{title}” was deleted by someone else; your unsent changes were saved as “{name}”."
                    ));
                });
            }
            let a = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_purge(&object_id, seq))
            });
            if let Some(a) = a {
                after_apply(&vault_id, a);
            }
            if had_pending {
                // A push with updates of the purged note is refused as a whole:
                // send the other updates again, without the dropped ones.
                persist_pending(&vault_id);
                resend_pending(&vault_id);
            }
        }
        ServerMessage::Snapshot { vault_id, snapshot } => {
            let res = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_snapshot(&snapshot))
            });
            if let Some(Ok(a)) = res {
                after_apply(&vault_id, a);
            }
        }
        ServerMessage::Awareness {
            vault_id,
            object_id,
            from_user,
            from_conn,
            payload,
        } => on_awareness(&vault_id, &object_id, &from_user, from_conn, &payload),
        ServerMessage::VaultEvent { vault_id, event } => on_vault_event(vault_id, event),
        ServerMessage::ObjectEvent {
            vault_id,
            object_id,
            event,
        } => on_object_event(vault_id, object_id, event),
        ServerMessage::UserEvent { event } => match event {
            UserEventKind::DevicePending | UserEventKind::DevicesChanged => {
                leptos::task::spawn_local(refresh_pending_devices());
            }
            UserEventKind::VaultsChanged => leptos::task::spawn_local(async {
                refresh_vaults().await;
                refresh_shares().await;
            }),
            UserEventKind::SharesChanged => leptos::task::spawn_local(refresh_shares_announcing()),
            UserEventKind::SessionRevoked => {
                sign_out_local(Some("This session was signed out.".into()))
            }
        },
        ServerMessage::Error {
            code,
            message,
            vault_id,
        } => {
            if code == "stale_key"
                && let Some(v) = vault_id
            {
                // A note got its own key (or a new one): reload keys and re-send.
                let v2 = v.clone();
                leptos::task::spawn_local(async move {
                    let before = key_epochs(&v2);
                    if is_member(&v2) {
                        reload_vault_keys(v2.clone()).await;
                    } else {
                        let notes: Vec<String> = with(|c| {
                            c.shares
                                .keys()
                                .filter(|(x, _)| *x == v2)
                                .map(|(_, n)| n.clone())
                                .collect()
                        });
                        for n in notes {
                            reload_share_keys(&v2, &n).await;
                        }
                    }
                    if key_epochs(&v2) != before {
                        resend_pending(&v2);
                    } else {
                        util::warn("server refused our key, and no newer key is available");
                    }
                });
            } else if code == "not_found" && vault_id.is_some() {
                leptos::task::spawn_local(async {
                    refresh_vaults().await;
                    refresh_shares().await;
                });
            } else if code == "too_large" {
                if let Some(v) = vault_id {
                    leptos::task::spawn_local(on_too_large(v));
                }
            } else if code == "forbidden" {
                error(format!("Not allowed: {message}"));
            } else if code == "conflict" {
                // e.g. a note was permanently deleted elsewhere while we edited it.
                error(message);
                // The whole push was refused: send the rest of it again.
                if let Some(v) = vault_id
                    && drop_rejected(&v)
                {
                    resend_pending(&v);
                }
            } else {
                util::warn(&format!("server error {code}: {message}"));
            }
        }
        ServerMessage::Pong { .. } => {}
    }
}

/// Current key epochs of a replica (vault key and every document key).
pub(super) fn key_epochs(vault: &str) -> Vec<(String, Option<u32>)> {
    with(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        let mut v: Vec<(String, Option<u32>)> = r
            .doc_keys
            .iter()
            .map(|(o, k)| (o.clone(), k.current_epoch()))
            .collect();
        v.push((String::new(), r.keyring.current_epoch()));
        v.sort();
        v
    })
}

/// Pending updates for purged objects can never succeed (the text is kept
/// in a new note by the `Purged` handler). Returns whether any were dropped.
pub(super) fn drop_rejected(vault_id: &str) -> bool {
    let dropped = with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return false;
        };
        let before = r.pending.len();
        let purged = r.purged.clone();
        r.pending.retain(|p| !purged.contains(&p.object_id));
        r.pending.len() != before
    });
    persist_pending(vault_id);
    update_pending_count();
    dropped
}

pub(super) fn on_synced(vault_id: &str, head: i64) {
    let (rolled_back, uid, first) = with(|c| {
        let uid = c.user_id.clone();
        c.loaded.insert(vault_id.to_string());
        let first = c.synced.insert(vault_id.to_string());
        let rb = c
            .replicas
            .get_mut(vault_id)
            .map(|r| r.synced(head))
            .unwrap_or(false);
        (rb, uid, first)
    });
    storage::set_head_seen(
        &uid,
        vault_id,
        with(|c| {
            c.replicas
                .get(vault_id)
                .map(|r| r.max_head_seen)
                .unwrap_or(head)
        }),
    );
    if first {
        restore_stored_pending(vault_id);
        show_loaded(vault_id);
    }
    if rolled_back {
        ui().warning.set(Some(
            "The server's copy of a vault is older than what this browser saw before (restored backup or a server problem). Your local changes are being re-sent.".into(),
        ));
        repush_everything(vault_id);
    }
    resend_pending(vault_id);
    maybe_compact(vault_id);
    crate::cache::schedule();
}

/// A vault's content became available: show it.
pub(super) fn show_loaded(vault_id: &str) {
    publish_vaults();
    let ui = ui();
    if ui.route.get_untracked().vault() == Some(vault_id) {
        ui.tree_rev.update(|v| *v += 1);
        ui.content_rev.update(|v| *v += 1);
    }
}

/// Updates left unsent by a previous session (tab closed while offline):
/// apply them locally (they are ours) and send them again.
pub(super) fn restore_stored_pending(vault_id: &str) {
    let uid = with(|c| c.user_id.clone());
    let stored = storage::pending(&uid, vault_id);
    if stored.is_empty() {
        return;
    }
    let mut applied = engine::Applied::default();
    with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return;
        };
        for p in stored {
            if r.pending
                .iter()
                .any(|x| x.client_update_id == p.client_update_id)
            {
                continue;
            }
            if let Ok(plain) = r.decrypt_object(Purpose::Update, &p.object_id, &p.payload) {
                if p.object_id == INDEX_OBJECT_ID {
                    let _ = r.index.apply_update(&plain);
                    applied.index_changed = true;
                } else {
                    let _ = r
                        .notes
                        .entry(p.object_id.clone())
                        .or_default()
                        .apply_update(&plain);
                    applied.objects.insert(p.object_id.clone());
                }
                r.pending.push(engine::Pending {
                    client_update_id: p.client_update_id,
                    object_id: p.object_id,
                    update: plain,
                });
            }
        }
    });
    after_apply(vault_id, applied);
    info("Sending changes saved while you were offline.");
}

pub(super) fn repush_everything(vault_id: &str) {
    with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return;
        };
        if !r.can_write() || !r.member {
            return;
        }
        let mut items = vec![(INDEX_OBJECT_ID.to_string(), r.index.encode_state())];
        for (id, n) in &r.notes {
            items.push((id.clone(), n.encode_state()));
        }
        for (id, state) in items {
            let _ = r.local_change(&id, state);
        }
    });
    resend_pending(vault_id);
}

pub(super) fn resend_pending(vault_id: &str) {
    let ups = with(|c| {
        c.replicas
            .get(vault_id)
            .map(|r| {
                encrypt_pending(r)
                    .into_iter()
                    .filter_map(|(_, u)| u)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    push_all(vault_id, ups);
    update_pending_count();
}

/// Every unacknowledged update with its id, encrypted with the current keys.
/// One that can't be encrypted right now (`None`, e.g. its note's key is
/// being reloaded) doesn't keep the others from being sent or stored.
pub(super) fn encrypt_pending(r: &VaultReplica) -> Vec<(String, Option<PushUpdate>)> {
    r.pending
        .iter()
        .map(|p| {
            let u = r
                .encrypt_object(Purpose::Update, &p.object_id, &p.update)
                .ok()
                .map(|payload| PushUpdate {
                    object_id: p.object_id.clone(),
                    client_update_id: p.client_update_id.clone(),
                    payload,
                });
            (p.client_update_id.clone(), u)
        })
        .collect()
}

/// The server's limit for one update (from `/info`).
pub fn max_update_bytes() -> usize {
    ui().server
        .get_untracked()
        .map(|s| s.max_update_bytes as usize)
        .unwrap_or(sizes::DEFAULT_MAX_UPDATE)
}

/// Send updates in pushes the server accepts. Updates too large to ever be
/// stored are held back (they stay pending on this device) and reported,
/// instead of making every push of the vault fail.
pub(super) fn push_all(vault_id: &str, ups: Vec<PushUpdate>) {
    let (pushes, oversized) = sizes::batches(ups, max_update_bytes());
    for updates in pushes {
        // If the socket is down, updates stay in the replica's pending
        // queue and are re-sent after reconnecting.
        send(&ClientMessage::Push {
            vault_id: vault_id.to_string(),
            updates,
        });
    }
    if !oversized.is_empty() {
        report_oversized(vault_id, &oversized);
    }
}

pub(super) fn report_oversized(vault_id: &str, ups: &[PushUpdate]) {
    let new: Vec<&PushUpdate> = with(|c| {
        ups.iter()
            .filter(|u| c.oversized_reported.insert(u.client_update_id.clone()))
            .collect()
    });
    let Some(biggest) = new.iter().max_by_key(|u| u.payload.len()) else {
        return;
    };
    let title = with(|c| {
        c.replicas
            .get(vault_id)
            .and_then(|r| r.index.get(&biggest.object_id))
            .map(|e| e.title().to_string())
            .or_else(|| {
                c.shares
                    .get(&(vault_id.to_string(), biggest.object_id.clone()))
                    .map(|s| s.title.clone())
            })
    });
    let what = match title {
        Some(t) => format!("A change to “{t}”"),
        None => "A change".to_string(),
    };
    util::warn(&format!(
        "holding back {} update(s) larger than the server limit",
        new.len()
    ));
    error(format!(
        "{what} is too large to sync ({}; the server accepts {} at a time). It is kept on this device only.",
        util::human_bytes(biggest.payload.len() as u64),
        util::human_bytes(max_update_bytes() as u64)
    ));
}

/// The server refused a push as too large: its limit may have gone down
/// since we asked. Refresh it and re-send what still fits.
pub(super) async fn on_too_large(vault_id: String) {
    let before = max_update_bytes();
    if let Ok(info) = api::get::<ServerInfo>("/info").await {
        ui().server.set(Some(info));
    }
    if max_update_bytes() < before {
        resend_pending(&vault_id);
    } else {
        error("The server refused some changes as too large; they are kept on this device.");
    }
}

/// Total payload bytes waiting in the outbox of a vault.
pub fn outbox_bytes(vault_id: &str) -> usize {
    with(|c| {
        c.outbox
            .get(vault_id)
            .map(|v| v.iter().map(|u| u.payload.len()).sum())
            .unwrap_or(0)
    })
}

pub(super) fn on_vault_event(vault_id: String, event: VaultEventKind) {
    match event {
        VaultEventKind::KeyRotated
        | VaultEventKind::MetaChanged
        | VaultEventKind::RoleChanged
        | VaultEventKind::MembersChanged => {
            leptos::task::spawn_local(async move {
                // From the list rather than `/vaults/{id}`: someone just removed
                // may get this before their `AccessRevoked`, and is handled by that.
                let Ok(infos) = api::get::<Vec<VaultInfo>>("/vaults").await else {
                    return;
                };
                if let Some(info) = infos.into_iter().find(|i| i.id == vault_id) {
                    let owner = info.role == Role::Owner;
                    load_vault(info).await;
                    publish_vaults();
                    if owner && event == VaultEventKind::MembersChanged {
                        crate::actions::grant_missing_keys(&vault_id).await;
                    }
                }
            });
        }
        VaultEventKind::Deleted | VaultEventKind::AccessRevoked => {
            let name = with(|c| c.replicas.get(&vault_id).map(|r| r.meta.name.clone()));
            drop_vault(&vault_id);
            publish_vaults();
            if let Some(n) = name {
                info(if event == VaultEventKind::Deleted {
                    format!("Vault “{n}” was deleted.")
                } else {
                    format!("You no longer have access to “{n}”.")
                });
            }
        }
    }
}

/// React to newly applied content: refresh the tree, the open note, panels.
pub fn after_apply(vault_id: &str, applied: engine::Applied) {
    if applied.is_empty() {
        return;
    }
    crate::cache::note_applied(vault_id, &applied);
    let ui = ui();
    let current = ui.route.get_untracked();
    if current.vault() == Some(vault_id) || current.shared().is_some_and(|(v, _)| v == vault_id) {
        if applied.index_changed || !applied.purged.is_empty() {
            ui.tree_rev.update(|v| *v += 1);
        }
        let d = with(|c| c.content_debounce.clone());
        d.run(250, move || ui.content_rev.update(|v| *v += 1));
    }
    // Push remote content into the open editor.
    let open = with(|c| {
        c.editor
            .as_ref()
            .map(|(v, n, e)| (v.clone(), n.clone(), e.clone()))
    });
    if let Some((v, n, ed)) = open {
        if v == vault_id && applied.objects.contains(&n) {
            let text = with(|c| c.replicas.get(&v).map(|r| r.note_text(&n)));
            if let Some(t) = text {
                ed.set_text(t, false);
            }
        }
        if v == vault_id && applied.purged.contains(&n) {
            info("This note was permanently deleted by someone else.");
            navigate(if is_member(&v) {
                Route::Vault(v.clone())
            } else {
                Route::Home
            });
        }
        if v == vault_id && applied.index_changed {
            // Link targets may have appeared or disappeared.
            ed.refresh();
        }
    }
}

// ------------------------------------------------------------------ local edits

/// Record a local CRDT update and schedule sending it.
pub fn local_update(vault_id: &str, object_id: &str, update: Vec<u8>) {
    let ok = with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return false;
        };
        match r.local_change(object_id, update) {
            Ok(p) => {
                c.outbox.entry(vault_id.to_string()).or_default().push(p);
                true
            }
            Err(e) => {
                util::warn(&format!("encrypting update failed: {e}"));
                false
            }
        }
    });
    if ok {
        crate::cache::note_local(vault_id, object_id);
        schedule_flush();
    }
}

pub fn schedule_flush() {
    let d = with(|c| c.flush.clone());
    d.run(120, flush_now);
    update_pending_count();
}

pub fn flush_now() {
    let batches: Vec<(String, Vec<PushUpdate>)> =
        with(|c| std::mem::take(&mut c.outbox).into_iter().collect());
    for (v, ups) in batches {
        push_all(&v, ups);
        persist_pending(&v);
    }
    update_pending_count();
}

pub(super) fn persist_pending(vault_id: &str) {
    let (uid, items) = with(|c| {
        (
            c.user_id.clone(),
            c.replicas.get(vault_id).map(encrypt_pending),
        )
    });
    let Some(items) = items else {
        // The replica is gone (e.g. the last note shared with us from this vault).
        let _ = storage::set_pending(&uid, vault_id, &[]);
        return;
    };
    let failed = items.iter().filter(|(_, u)| u.is_none()).count();
    let ups: Vec<PushUpdate> = if failed == 0 {
        items.into_iter().filter_map(|(_, u)| u).collect()
    } else {
        // Keep what was stored before for updates we can't encrypt now.
        util::warn(&format!("{failed} unsent update(s) could not be encrypted"));
        let mut stored = storage::pending(&uid, vault_id);
        let ups: Vec<PushUpdate> = items
            .into_iter()
            .filter_map(|(id, u)| {
                u.or_else(|| {
                    let i = stored.iter().position(|s| s.client_update_id == id)?;
                    Some(stored.swap_remove(i))
                })
            })
            .collect();
        if ups.is_empty() {
            // Never replace stored changes with nothing because of an error.
            return;
        }
        ups
    };
    if let Err(e) = storage::set_pending(&uid, vault_id, &ups) {
        util::warn(&format!("could not store unsent changes: {e}"));
        storage_full();
    }
}

/// Tell the user (at most once a minute) that unsent changes can't be kept.
pub(super) fn storage_full() {
    let now = now_ms();
    if with(|c| std::mem::replace(&mut c.storage_warned_at, now)) > now - 60_000 {
        return;
    }
    error(
        "This browser's storage is full, so changes that are not synced yet would be lost if you close or reload this tab. Stay online until they are synced.",
    );
}

/// Replace a note's text through its CRDT (diffing against the current
/// text) and queue the change, split into updates small enough for the
/// server. Returns how many updates were made (0: unchanged or no such note).
pub fn set_note_text(vault: &str, note: &str, text: &str) -> usize {
    let budget = sizes::insert_budget(max_update_bytes());
    let ups = with(|c| {
        let n = c.replicas.get_mut(vault)?.notes.get_mut(note)?;
        let edits = textdiff::diff(&n.text(), text);
        Some(
            sizes::split_edits(edits, budget)
                .iter()
                .filter_map(|g| n.apply_edits(g))
                .collect::<Vec<_>>(),
        )
    })
    .unwrap_or_default();
    let n = ups.len();
    for u in ups {
        local_update(vault, note, u);
    }
    n
}

pub(super) fn update_pending_count() {
    let n = with(|c| c.pending_count());
    ui().pending.set(n);
}

/// Index "modified" timestamps, throttled per note.
pub fn touch(vault_id: &str, note_id: &str, size: usize) {
    let key = (vault_id.to_string(), note_id.to_string());
    let now = now_ms();
    let due = with(|c| {
        let last = c.last_touch.get(&key).copied().unwrap_or(0);
        if now - last > 30_000 {
            c.last_touch.insert(key.clone(), now);
            true
        } else {
            false
        }
    });
    if due {
        // People a single note is shared with can't see (or change) the vault index.
        let up = with(|c| {
            c.replicas
                .get(vault_id)
                .filter(|r| r.member)
                .and_then(|r| r.index.touch(note_id, now, Some(size as u64)))
        });
        if let Some(u) = up {
            local_update(vault_id, INDEX_OBJECT_ID, u);
        }
    }
    with(|c| c.dirty.insert(key));
}

/// Compact objects with long update logs (and give history a revision).
pub fn maybe_compact(vault_id: &str) {
    let candidates = with(|c| {
        c.replicas
            .get(vault_id)
            .map(|r| r.compaction_candidates())
            .unwrap_or_default()
    });
    for o in candidates {
        snapshot(vault_id.to_string(), o);
    }
}

pub fn snapshot(vault_id: String, object_id: String) {
    let key = (vault_id.clone(), object_id.clone());
    let req = with(|c| {
        if c.snapshotting.contains(&key) {
            return None;
        }
        let r = c.replicas.get(&vault_id)?;
        if !r.can_write() || r.seq == 0 || r.pending.iter().any(|p| p.object_id == object_id) {
            return None;
        }
        let mut req = r.snapshot_request(&object_id).ok()?;
        if !r.member {
            // Our sequence number only means something for the note itself.
            let s = c.shares.get(&key)?;
            if !s.loaded || !s.info.role.can_write() {
                return None;
            }
            req.covers_seq = s.seq;
        }
        c.snapshotting.insert(key.clone());
        Some(req)
    });
    let Some(req) = req else { return };
    leptos::task::spawn_local(async move {
        // Jitter so several clients don't compact the same object at once.
        util::sleep((js_sys::Math::random() * 1500.0) as i32).await;
        let r = api::post::<_, SnapshotInfo>(
            &format!("/vaults/{vault_id}/objects/{object_id}/snapshots"),
            &req,
        )
        .await;
        with(|c| {
            c.snapshotting.remove(&key);
            if r.is_ok() {
                c.dirty.remove(&key);
                if let Some(rep) = c.replicas.get_mut(&vault_id) {
                    rep.snapshot_created(&object_id);
                }
            }
        });
    });
}

/// Called when leaving a note: store a revision if it was edited.
pub fn note_closed(vault_id: &str, note_id: &str) {
    flush_now();
    let dirty = with(|c| {
        c.dirty
            .contains(&(vault_id.to_string(), note_id.to_string()))
    });
    if dirty {
        let (v, n) = (vault_id.to_string(), note_id.to_string());
        // Wait for acks so the snapshot can cover our own updates.
        util::set_timeout(2500, move || snapshot(v, n));
    }
}
