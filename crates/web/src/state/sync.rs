//! The WebSocket: catch-up, live updates, acknowledgements, pushing local
//! edits, unsent changes kept in local storage, compaction.

use super::*;
use coolpsonotes_sync::SnapshotAction;

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
            if with(|c| std::mem::take(&mut c.offline_start)) {
                leptos::task::spawn_local(offline::finish_online());
            }
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
                super::members::log_missing(&vault_id);
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
            super::members::log_missing(&vault_id);
        }
        ServerMessage::Ack {
            vault_id,
            results,
            rejected,
        } => {
            let limits = limits();
            let reaction = with(|c| {
                let r = c.replicas.get_mut(&vault_id)?;
                let o = c.outboxes.entry(vault_id.clone()).or_default();
                o.on_ack(r, &results, true);
                Some(o.on_rejected(r, &rejected, &limits))
            });
            persist_pending(&vault_id);
            update_pending_count();
            maybe_compact(&vault_id);
            crate::cache::schedule();
            if let Some(re) = reaction {
                react(&vault_id, re);
            }
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
                let r = c.replicas.get_mut(&vault_id)?;
                c.outboxes
                    .entry(vault_id.clone())
                    .or_default()
                    .drop_object(r, &object_id);
                Some(r.apply_purge(&object_id, seq))
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
            // (Older servers refuse whole pushes; newer ones list single
            // refused updates in the acknowledgement, see `react`.)
            if code == "stale_key"
                && let Some(v) = vault_id
            {
                leptos::task::spawn_local(reload_keys_and_resend(v));
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

/// What to do after the server's answer to a push (see `coolpsonotes_sync::Reaction`).
pub(super) fn react(vault_id: &str, re: coolpsonotes_sync::Reaction) {
    if !re.held.is_empty() {
        report_held(vault_id, &re.held);
    }
    for object in &re.purged {
        rescue_dropped(vault_id, object);
    }
    if re.reload_keys {
        leptos::task::spawn_local(reload_keys_and_resend(vault_id.to_string()));
    } else if re.resend {
        send_planned(vault_id);
    }
}

/// The server refused updates encrypted with an outdated key (a note got
/// its own key, or a key was rotated): reload keys, then send again.
pub(super) async fn reload_keys_and_resend(v: String) {
    let before = key_epochs(&v);
    if is_member(&v) {
        reload_vault_keys(v.clone()).await;
    } else {
        let notes: Vec<String> = with(|c| {
            c.shares
                .keys()
                .filter(|(x, _)| *x == v)
                .map(|(_, n)| n.clone())
                .collect()
        });
        for n in notes {
            reload_share_keys(&v, &n).await;
        }
    }
    if key_epochs(&v) != before {
        resend_pending(&v);
    } else {
        util::warn("server refused our key, and no newer key is available");
    }
}

/// Our unsent changes to a note were dropped because someone deleted it for
/// good: keep our version as a new note rather than losing it.
fn rescue_dropped(vault_id: &str, object_id: &str) {
    let rescue = with(|c| {
        let r = c.replicas.get(vault_id)?;
        if r.purged.contains(object_id) || !r.member {
            return None;
        }
        let text = r.note_text(object_id);
        let title = r
            .index
            .get(object_id)
            .map(|e| e.title().to_string())
            .unwrap_or_else(|| "Untitled".into());
        (!text.is_empty()).then_some((title, text))
    });
    if let Some((title, text)) = rescue {
        let v = vault_id.to_string();
        leptos::task::spawn_local(async move {
            let name = format!("{title} (recovered)");
            crate::actions::create_note_with(&v, "", &name, &text).await;
            info(format!(
                "“{title}” was deleted by someone else; your unsent changes were saved as “{name}”."
            ));
        });
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
        let o = c.outboxes.entry(vault_id.to_string()).or_default();
        for object in &purged {
            o.drop_object(r, object);
        }
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
            .map(|r| {
                // The server's history went backwards: queue our full state.
                let rb = head < r.max_head_seen;
                if rb {
                    coolpsonotes_sync::recover_from_rollback(r, head);
                }
                r.synced(head);
                rb
            })
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

/// Updates left unsent by earlier page loads or other tabs (closed while
/// offline): apply them locally (they are ours) and send them again. Another
/// tab still sending the same updates is harmless: the server keeps one copy
/// per update id. Their storage keys are removed once everything in them is
/// stored under this page's key.
pub(super) fn restore_stored_pending(vault_id: &str) {
    let uid = with(|c| c.user_id.clone());
    let stored = storage::all_pending(&uid, vault_id);
    if stored.is_empty() {
        return;
    }
    let mut applied = engine::Applied::default();
    let mut adopted = 0;
    let mut done_keys = Vec::new();
    with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return;
        };
        for (key, items) in stored {
            let mut complete = true;
            for p in items {
                if r.pending
                    .iter()
                    .any(|x| x.client_update_id == p.client_update_id)
                {
                    continue;
                }
                match r.decrypt_object(Purpose::Update, &p.object_id, &p.payload) {
                    Ok(plain) => {
                        adopted += 1;
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
                    // Kept where it is (e.g. its key isn't loaded yet).
                    Err(_) => complete = false,
                }
            }
            if complete && !storage::is_own_pending_key(&uid, vault_id, &key) {
                done_keys.push(key);
            }
        }
    });
    if adopted == 0 && done_keys.is_empty() {
        return;
    }
    after_apply(vault_id, applied);
    // Only after they are stored under this page's key.
    if persist_pending(vault_id) {
        for k in &done_keys {
            storage::remove_pending_key(k);
        }
    }
    if adopted > 0 {
        info("Sending changes saved while you were offline.");
    }
}

/// Send everything not acknowledged again (after a reconnect, new keys or a
/// refused push).
pub(super) fn resend_pending(vault_id: &str) {
    with(|c| {
        if let Some(o) = c.outboxes.get_mut(vault_id) {
            o.connection_reset();
        }
    });
    send_planned(vault_id);
}

/// Send what the sync engine plans for a vault. What can't be handed to the
/// socket (it is closed) is planned again later — and merged with later
/// edits meanwhile.
pub(super) fn send_planned(vault_id: &str) {
    let limits = limits();
    let plan = with(|c| {
        let r = c.replicas.get_mut(vault_id)?;
        let o = c.outboxes.entry(vault_id.to_string()).or_default();
        let mut plan = o.plan(r, &limits);
        // Every change carries its author's signature.
        if let Some(a) = &c.account {
            coolpsonotes_sync::members::sign_pushes(a, vault_id, &mut plan.pushes);
        }
        Some(plan)
    });
    let Some(plan) = plan else { return };
    if !plan.held.is_empty() {
        report_held(vault_id, &plan.held);
    }
    let register: Vec<coolpsonotes_sync::NewKey> = with(|c| {
        plan.register
            .into_iter()
            .filter(|k| {
                c.registering
                    .insert((vault_id.to_string(), k.object_id.clone()))
            })
            .collect()
    });
    if !register.is_empty() {
        leptos::task::spawn_local(register_keys(vault_id.to_string(), register));
    }
    for updates in plan.pushes {
        let ids: Vec<String> = updates.iter().map(|u| u.client_update_id.clone()).collect();
        let sent = send(&ClientMessage::Push {
            vault_id: vault_id.to_string(),
            updates,
        });
        if !sent {
            with(|c| {
                if let Some(o) = c.outboxes.get_mut(vault_id) {
                    o.not_sent(&ids);
                }
            });
        }
    }
    update_pending_count();
}

/// Give a note created here its own key, registered before any of its
/// content is sent (so none of it is ever under the vault key).
pub fn new_note_key(vault_id: &str, object_id: &str, title: &str) {
    let made = with(|c| {
        let r = c.replicas.get_mut(vault_id)?;
        let o = c.outboxes.entry(vault_id.to_string()).or_default();
        Some(o.new_document_key(r, object_id, title))
    });
    if made == Some(true) {
        persist_new_keys(vault_id, &[]);
    }
}

/// Store the vault's unregistered note keys under this page's key (and drop
/// the other pages' entries in `adopted`, now kept here).
pub fn persist_new_keys(vault_id: &str, adopted: &[String]) {
    let (uid, keys) = with(|c| {
        (
            c.user_id.clone(),
            c.outboxes
                .get(vault_id)
                .map(|o| o.new_keys.clone())
                .unwrap_or_default(),
        )
    });
    storage::set_new_keys(&uid, vault_id, &keys, adopted);
}

/// Take over the unregistered note keys stored by earlier page loads or
/// other tabs (before restoring the copies of unsent changes that need them).
/// Returns the storage keys taken over.
pub(super) fn adopt_stored_new_keys(c: &mut Core, vault_id: &str) -> Vec<String> {
    let stored = storage::all_new_keys(&c.user_id, vault_id);
    let o = c.outboxes.entry(vault_id.to_string()).or_default();
    let mut adopted = Vec::new();
    for (key, keys) in stored {
        for k in keys {
            if !o.new_keys.iter().any(|x| x.object_id == k.object_id) {
                o.new_keys.push(k);
            }
        }
        adopted.push(key);
    }
    adopted
}

/// Register new notes' keys, then send their content.
async fn register_keys(vault_id: String, keys: Vec<coolpsonotes_sync::NewKey>) {
    let mut decided = false;
    for k in keys {
        let path = format!("/vaults/{vault_id}/objects/{}/key", k.object_id);
        match api::put::<_, ()>(&path, &k.update).await {
            Ok(()) => {
                decided = true;
                with(|c| {
                    if let Some(o) = c.outboxes.get_mut(&vault_id) {
                        o.key_registered(&k.object_id);
                    }
                })
            }
            // Offline or a server problem: tried again with a later plan
            // (the next edit or reconnect).
            Err(e) if e.status == 0 || e.status >= 500 => {}
            // Wrapped under a replaced vault key (412), or refused; perhaps it
            // is registered and the answer got lost: fresh keys show
            // (`Outbox::keys_reloaded`).
            Err(e) => {
                decided = true;
                super::reload_vault_keys(vault_id.clone()).await;
                with(|c| {
                    let (Some(r), Some(o)) =
                        (c.replicas.get_mut(&vault_id), c.outboxes.get_mut(&vault_id))
                    else {
                        return;
                    };
                    if o.waits_for_key(&k.object_id) {
                        util::warn(&format!("a new note's key was refused: {}", e.message));
                        o.key_refused(r, &k.object_id);
                    }
                });
            }
        }
        with(|c| {
            c.registering
                .remove(&(vault_id.clone(), k.object_id.clone()))
        });
    }
    if decided {
        persist_new_keys(&vault_id, &[]);
        persist_pending(&vault_id);
        send_planned(&vault_id);
    }
}

/// The server's limit for one update (from `/info`).
pub fn max_update_bytes() -> usize {
    ui().server
        .get_untracked()
        .map(|s| s.max_update_bytes as usize)
        .unwrap_or(coolpsonotes_sync::Limits::DEFAULT_MAX_UPDATE)
}

/// The server's limits as far as known.
pub fn limits() -> coolpsonotes_sync::Limits {
    coolpsonotes_sync::Limits::new(max_update_bytes())
}

/// Tell the user (once per update) about changes held back as too large.
pub(super) fn report_held(vault_id: &str, held: &[coolpsonotes_sync::Held]) {
    let Some(biggest) = held.iter().max_by_key(|h| h.bytes) else {
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
        "holding back {} update(s) the server can't store",
        held.len()
    ));
    let size = if biggest.bytes > 0 {
        format!("{}; ", util::human_bytes(biggest.bytes as u64))
    } else {
        String::new()
    };
    error(format!(
        "{what} is too large to sync ({size}the server accepts {} at a time). It is kept on this device only.",
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

/// Bytes of local changes of a vault not handed to the server yet.
pub fn outbox_bytes(vault_id: &str) -> usize {
    with(|c| {
        let r = c.replicas.get(vault_id)?;
        Some(c.outboxes.get(vault_id).map_or_else(
            || r.pending.iter().map(|p| p.update.len()).sum(),
            |o| o.unsent_bytes(r),
        ))
    })
    .unwrap_or(0)
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
            Ok(_) => true,
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
    let vaults: Vec<String> = with(|c| {
        c.replicas
            .iter()
            .filter(|(_, r)| !r.pending.is_empty())
            .map(|(v, _)| v.clone())
            .collect()
    });
    for v in vaults {
        send_planned(&v);
        persist_pending(&v);
    }
    update_pending_count();
}

/// Store this page's unsent updates of a vault; false if that failed.
pub(super) fn persist_pending(vault_id: &str) -> bool {
    let uid = with(|c| c.user_id.clone());
    let previous = storage::pending(&uid, vault_id);
    let copies = with(|c| {
        let r = c.replicas.get(vault_id)?;
        let empty = coolpsonotes_sync::Outbox::default();
        let o = c.outboxes.get(vault_id).unwrap_or(&empty);
        Some(coolpsonotes_sync::stored_copies(r, o, &previous))
    });
    let ups = match copies {
        // The replica is gone (e.g. the last note shared with us from this vault).
        None => return storage::set_pending(&uid, vault_id, &[]).is_ok(),
        // Never replace stored changes with nothing because of an error.
        Some(None) => {
            util::warn("unsent updates could not be encrypted; keeping the stored copies");
            return false;
        }
        Some(Some(ups)) => ups,
    };
    if let Err(e) = storage::set_pending(&uid, vault_id, &ups) {
        util::warn(&format!("could not store unsent changes: {e}"));
        storage_full();
        return false;
    }
    true
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
    let budget = limits().edit_budget();
    let ups = with(|c| {
        let n = c.replicas.get_mut(vault)?.notes.get_mut(note)?;
        Some(coolpsonotes_sync::edits::set_text(n, text, budget))
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
    let action = with(|c| {
        if c.snapshotting.contains(&key) {
            return None;
        }
        let r = c.replicas.get(&vault_id)?;
        // Our sequence number only means something for a shared note itself.
        let covers = if r.member {
            None
        } else {
            let s = c.shares.get(&key)?;
            if !s.loaded || !s.info.role.can_write() {
                return None;
            }
            Some(s.seq)
        };
        let empty = coolpsonotes_sync::Outbox::default();
        let o = c.outboxes.get(&vault_id).unwrap_or(&empty);
        let action = match coolpsonotes_sync::snapshot_action(r, o, &object_id, covers)? {
            SnapshotAction::Snapshot(mut req) => {
                coolpsonotes_sync::members::sign_snapshot(
                    c.account.as_ref()?,
                    &vault_id,
                    &object_id,
                    &mut req,
                );
                SnapshotAction::Snapshot(req)
            }
            SnapshotAction::AdoptKey { ring, mut update } => {
                coolpsonotes_sync::members::sign_key_update(
                    c.account.as_ref()?,
                    &vault_id,
                    &object_id,
                    &mut update,
                );
                SnapshotAction::AdoptKey { ring, update }
            }
        };
        c.snapshotting.insert(key.clone());
        Some(action)
    });
    let Some(action) = action else { return };
    leptos::task::spawn_local(async move {
        // Jitter so several clients don't compact the same object at once.
        util::sleep((js_sys::Math::random() * 1500.0) as i32).await;
        let (res, ring) = match action {
            SnapshotAction::Snapshot(req) => (
                api::post::<_, SnapshotInfo>(
                    &format!("/vaults/{vault_id}/objects/{object_id}/snapshots"),
                    &req,
                )
                .await
                .map(|_| ()),
                None,
            ),
            // An older note moves to its own key, with this snapshot under it.
            SnapshotAction::AdoptKey { ring, update } => (
                api::put::<_, ()>(
                    &format!("/vaults/{vault_id}/objects/{object_id}/key"),
                    &update,
                )
                .await,
                Some(ring),
            ),
        };
        let adopted = ring.is_some();
        let refused = matches!(&res, Err(e) if e.status == 409 || e.status == 412);
        with(|c| {
            c.snapshotting.remove(&key);
            if res.is_ok() {
                c.dirty.remove(&key);
                if let Some(rep) = c.replicas.get_mut(&vault_id) {
                    if let Some(ring) = ring {
                        rep.doc_keys.insert(object_id.clone(), ring);
                    }
                    rep.snapshot_created(&object_id);
                }
            }
        });
        // Someone else gave it a key, or changes arrived meanwhile.
        if adopted && refused {
            super::reload_vault_keys(vault_id).await;
        }
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
