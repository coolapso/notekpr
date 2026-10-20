//! Presence: who is in which note, cursors.

use super::*;

// ------------------------------------------------------------------ presence

pub fn send_presence() {
    let d = step(|c| c.presence_debounce.clone());
    d.run(200, || {
        let ui = ui();
        let route = ui.route.get_untracked();
        let name = ui
            .user
            .get_untracked()
            .map(|u| u.display_name)
            .unwrap_or_default();
        let uid = step(|c| c.user_id.clone());
        let cursor = step(|c| c.cursor);
        let vaults: Vec<String> = step(|c| c.loaded.iter().cloned().collect());
        for v in vaults {
            let here = route.vault() == Some(v.as_str());
            let p = Presence {
                name: name.clone(),
                color: util::user_color(&uid),
                object_id: if here {
                    route.note().map(|(_, n)| n.to_string())
                } else {
                    None
                },
                cursor: if here { cursor } else { None },
                at: now_ms(),
            };
            announce(&v, "", &p);
        }
        // On individually shared notes, also announce ourselves on the note
        // itself, so people it is shared with (who see nothing else) see us.
        let target = match &route {
            Route::Note(v, n) if is_shared_note(v, n) => Some((v.clone(), n.clone())),
            Route::Shared(v, n)
                if step(|c| {
                    c.shares
                        .get(&(v.clone(), n.clone()))
                        .is_some_and(|s| s.loaded)
                }) =>
            {
                Some((v.clone(), n.clone()))
            }
            _ => None,
        };
        let prev = step(|c| std::mem::replace(&mut c.presence_note, target.clone()));
        let base = Presence {
            name,
            color: util::user_color(&uid),
            object_id: None,
            cursor: None,
            at: now_ms(),
        };
        if let Some((v, n)) = prev.filter(|p| Some(p) != target.as_ref()) {
            announce(&v, &n, &base);
        }
        if let Some((v, n)) = target {
            announce(
                &v,
                &n,
                &Presence {
                    object_id: Some(n.clone()),
                    cursor,
                    ..base
                },
            );
        }
    });
}

pub(super) fn announce(vault: &str, object_id: &str, p: &Presence) {
    let payload = step(|c| {
        let r = c.replicas.get(vault)?;
        r.encrypt_awareness(object_id, &serde_json::to_vec(p).ok()?)
            .ok()
    });
    if let Some(payload) = payload {
        send(&ClientMessage::Awareness {
            vault_id: vault.to_string(),
            object_id: object_id.to_string(),
            payload,
        });
    }
}

pub(super) fn on_awareness(
    vault_id: &str,
    object_id: &str,
    from_user: &str,
    conn: u64,
    payload: &[u8],
) {
    // Vault members announce themselves vault-wide and, on shared notes, on the
    // note too; once we know a connection vault-wide, its note-level messages
    // add nothing (and may arrive out of order), so they are ignored.
    let scope = (!object_id.is_empty()).then(|| object_id.to_string());
    let known_vault_wide = step(|c| {
        c.presence
            .get(&conn)
            .is_some_and(|e| e.scope.is_none() && now_ms() - e.seen < 45_000)
    });
    if scope.is_some() && known_vault_wide {
        return;
    }
    if payload.is_empty() {
        step(|c| c.presence.remove(&conn));
    } else {
        let p = step(|c| {
            c.replicas
                .get(vault_id)
                .and_then(|r| r.decrypt_awareness(object_id, payload).ok())
        });
        let Some(mut p) = p.and_then(|pt| serde_json::from_slice::<Presence>(&pt).ok()) else {
            return;
        };
        if let Some(o) = &scope {
            // A note-level announcement can only be about that note.
            if p.object_id.as_ref().is_some_and(|x| x != o) {
                p.object_id = None;
            }
        }
        let is_new = step(|c| {
            let new = !c.presence.contains_key(&conn);
            c.presence.insert(
                conn,
                PresenceEntry {
                    vault: vault_id.to_string(),
                    scope,
                    user_id: from_user.to_string(),
                    presence: p,
                    seen: now_ms(),
                },
            );
            new
        });
        if is_new {
            // Let the newcomer see us too.
            send_presence();
        }
    }
    publish_presence();
}

pub fn publish_presence() {
    let ui = ui();
    let route = ui.route.get_untracked();
    let now = now_ms();
    let (views, cursors, ed) = step(|c| {
        c.presence.retain(|_, e| now - e.seen < 45_000);
        let v = route
            .vault()
            .or(route.shared().map(|(v, _)| v))
            .map(str::to_string);
        // On a note shared with us, only people on that note are relevant.
        let only = route.shared().map(|(_, n)| n.to_string());
        let views: Vec<PresenceView> = c
            .presence
            .iter()
            .filter(|(_, e)| Some(&e.vault) == v.as_ref())
            .filter(|(_, e)| only.is_none() || e.presence.object_id == only)
            .map(|(conn, e)| PresenceView {
                conn: *conn,
                user_id: e.user_id.clone(),
                name: e.presence.name.clone(),
                color: e.presence.color.clone(),
                note: e.presence.object_id.clone(),
            })
            .collect();
        let open = route.any_note().map(|(_, n)| n.to_string());
        let cursors: Vec<RemoteCursor> = c
            .presence
            .values()
            .filter(|e| {
                Some(&e.vault) == v.as_ref()
                    && e.presence.object_id.is_some()
                    && e.presence.object_id == open
            })
            .filter_map(|e| {
                let (a, h) = e.presence.cursor?;
                Some(RemoteCursor {
                    name: e.presence.name.clone(),
                    color: e.presence.color.clone(),
                    anchor: a,
                    head: h,
                })
            })
            .collect();
        (views, cursors, c.editor.as_ref().map(|(_, _, e)| e.clone()))
    });
    ui.presence.set(views);
    if let Some(ed) = ed {
        ed.set_remote_cursors(cursors);
    }
}

pub fn start_presence_heartbeat() {
    util::set_interval(15_000, || {
        if ui().phase.get_untracked() == Phase::Ready {
            send_presence();
            publish_presence();
        }
    });
}
