//! Mentions of us in comments (docs/comments.md §1): found in the comments
//! this browser synced — the server never learns who is mentioned — listed
//! in the sidebar's inbox, announced as they arrive (a toast, and a system
//! notification if asked for), and marked read in the encrypted
//! preferences, so that what is read on one device is read on all.

use leptos::prelude::*;
use notekpr_core::comments::{self, Mention};
use wasm_bindgen::JsCast;

use super::{Core, Route, save_prefs, step, ui, user_name};
use crate::util;

/// A mention of us, as the inbox shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MentionView {
    pub vault: String,
    pub note: String,
    pub note_title: String,
    pub vault_name: String,
    /// Who mentioned us, and whether we know their name yet.
    pub name: String,
    pub name_known: bool,
    pub unread: bool,
    pub mention: Mention,
}

/// Our mentions in the vaults we are a member of (people a note is shared
/// with aren't mentioned), newest first.
pub fn my_mentions(c: &Core) -> Vec<MentionView> {
    let mut out = Vec::new();
    for (vault, r) in &c.replicas {
        if !r.member {
            continue;
        }
        for (note, log) in &r.comments {
            if !comments::mentions_anyone(log, &c.user_id) {
                continue;
            }
            let Some(e) = r.index.get(note).filter(|e| e.is_live()) else {
                continue;
            };
            let threads = super::comments::folded(c, vault, note);
            for m in comments::mentions_of(log, &threads, &c.user_id) {
                out.push(MentionView {
                    vault: vault.clone(),
                    note: note.clone(),
                    note_title: e.title().to_string(),
                    vault_name: c.vault_name(vault),
                    name: user_name(c, &m.author),
                    name_known: c.users.contains_key(&m.author),
                    unread: !c.prefs.mentions.is_read(vault, &m),
                    mention: m,
                });
            }
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse((m.mention.created, m.mention.seq)));
    out
}

/// Mark the mentions of us in a thread read (it was opened).
pub fn mark_thread_read(c: &mut Core, vault: &str, note: &str, thread: &str) {
    let mut changed = false;
    for m in my_mentions(c) {
        if m.vault == vault && m.note == note && m.mention.thread == thread {
            changed |= c.prefs.mentions.mark(vault, &m.mention);
        }
    }
    if changed {
        save_prefs(c);
        ui().mentions_rev.update(|n| *n += 1);
    }
}

/// Mark all our mentions read.
pub fn mark_all_mentions_read(c: &mut Core) {
    let all = my_mentions(c);
    let mut by_vault: std::collections::BTreeMap<&str, Vec<Mention>> = Default::default();
    for m in &all {
        by_vault
            .entry(m.vault.as_str())
            .or_default()
            .push(m.mention.clone());
    }
    let mut changed = false;
    for (v, ms) in by_vault {
        changed |= c.prefs.mentions.mark_all(v, &ms);
    }
    if changed {
        save_prefs(c);
        ui().mentions_rev.update(|n| *n += 1);
    }
}

/// A vault finished its first catch-up: the mentions in it are not news.
pub(super) fn mentions_seen_at_start(c: &mut Core, vault: &str) {
    let ids: Vec<String> = my_mentions(c)
        .into_iter()
        .filter(|m| m.vault == vault)
        .map(|m| m.mention.comment)
        .collect();
    c.mentions_announced.extend(ids);
}

/// Look up the names of those who mentioned us that we don't know yet.
pub async fn learn_mention_authors() {
    let unknown: Vec<String> = step(|c| {
        let mut ids: Vec<String> = my_mentions(c)
            .into_iter()
            .map(|m| m.mention.author)
            .filter(|a| !c.users.contains_key(a))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    });
    let mut learned = false;
    for id in unknown {
        if let Ok(u) = crate::api::get::<notekpr_core::protocol::PublicUser>(&format!(
            "/users/{}",
            crate::api::q(&id)
        ))
        .await
        {
            step(|c| c.users.insert(u.id.clone(), u));
            learned = true;
        }
    }
    if learned {
        ui().mentions_rev.update(|n| *n += 1);
    }
}

/// Comments arrived (an edge): announce new mentions of us — in vaults
/// past their first catch-up, not read, not announced yet — by name.
pub fn announce_mentions() {
    let fresh: Vec<MentionView> = step(|c| {
        let fresh: Vec<MentionView> = my_mentions(c)
            .into_iter()
            .filter(|m| {
                m.unread
                    && c.synced.contains(&m.vault)
                    && !c.mentions_announced.contains(&m.mention.comment)
            })
            .collect();
        c.mentions_announced
            .extend(fresh.iter().map(|m| m.mention.comment.clone()));
        fresh
    });
    ui().mentions_rev.update(|n| *n += 1);
    // One in the thread open on screen needs no announcing (it is read
    // there).
    let open = match ui().comment_ui.get_untracked() {
        super::CommentUi::Thread { id, .. } => super::current_route()
            .note()
            .map(|(v, n)| (v.to_string(), n.to_string(), id)),
        _ => None,
    };
    let fresh: Vec<MentionView> = fresh
        .into_iter()
        .filter(|m| {
            open.as_ref()
                .is_none_or(|(v, n, t)| (v, n, t) != (&m.vault, &m.note, &m.mention.thread))
        })
        .collect();
    if fresh.is_empty() {
        return;
    }
    leptos::task::spawn_local(async move {
        learn_mention_authors().await;
        for mut m in fresh {
            m.name = step(|c| user_name(c, &m.mention.author));
            super::info(format!("{} mentioned you in “{}”", m.name, m.note_title));
            notify(&m);
        }
    });
}

/// Open the thread a mention is in (an edge): its note, then the thread
/// by its text, once the note is shown.
pub fn open_mention(vault: &str, note: &str, thread: &str) {
    if super::current_route().note() != Some((vault, note)) {
        super::navigate(Route::Note(vault.to_string(), note.to_string()));
    }
    ui().thread_to_open.set(Some((
        vault.to_string(),
        note.to_string(),
        thread.to_string(),
    )));
}

/// The start of a comment, for a notification.
fn snippet(body: &str) -> String {
    let one: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(140) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

/// Whether the browser lets us show system notifications.
pub fn notifications_allowed() -> bool {
    js_sys::Reflect::has(&util::window(), &"Notification".into()).unwrap_or(false)
        && web_sys::Notification::permission() == web_sys::NotificationPermission::Granted
}

/// Whether this browser has system notifications at all.
pub fn notifications_supported() -> bool {
    js_sys::Reflect::has(&util::window(), &"Notification".into()).unwrap_or(false)
}

/// Turn system notifications for mentions on (asking the browser first) or
/// off, for this browser.
pub async fn set_mention_notifications(on: bool) {
    if on && !notifications_allowed() {
        let granted = match web_sys::Notification::request_permission() {
            Ok(p) => wasm_bindgen_futures::JsFuture::from(p)
                .await
                .ok()
                .and_then(|v| v.as_string())
                .is_some_and(|s| s == "granted"),
            Err(_) => false,
        };
        if !granted {
            super::error(
                "The browser doesn't allow notifications from notekpr. Allow them in its site settings, then try again.",
            );
            super::set_ui_prefs(|p| p.mention_notifications = false);
            return;
        }
    }
    super::set_ui_prefs(|p| p.mention_notifications = on);
}

/// A system notification for a mention, when asked for and the app isn't
/// in front (otherwise the toast is enough). Clicking it opens the thread.
fn notify(m: &MentionView) {
    let doc = util::document();
    if !ui().ui_prefs.with_untracked(|p| p.mention_notifications)
        || !notifications_allowed()
        || (!doc.hidden() && doc.has_focus().unwrap_or(true))
    {
        return;
    }
    let opts = web_sys::NotificationOptions::new();
    opts.set_body(&snippet(&m.mention.body));
    opts.set_tag(&m.mention.comment);
    let title = format!("{} mentioned you in {}", m.name, m.note_title);
    let Ok(n) = web_sys::Notification::new_with_options(&title, &opts) else {
        return;
    };
    let (vault, note, thread) = (m.vault.clone(), m.note.clone(), m.mention.thread.clone());
    let n2 = n.clone();
    let click = wasm_bindgen::closure::Closure::once(move || {
        let _ = util::window().focus();
        n2.close();
        open_mention(&vault, &note, &thread);
    });
    n.set_onclick(Some(click.as_ref().unchecked_ref()));
    click.forget();
}
