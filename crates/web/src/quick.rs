//! Quick notes: a capture-first place for thoughts that don't have a home yet.
//!
//! Each person's quick notes are a vault of that kind (`notekpr_core::quick`):
//! made the first time, kept out of the vault lists, never shared, flat, and
//! holding at most `VaultInfo::note_limit` notes. A quick note leaves by
//! moving to a vault (`actions::move_to_vault`) or by being deleted for good
//! — they keep no trash. Files are kept only for the notes linking to them
//! (`drop_unlinked_files`). The CLI (`notekpr quick`) works the same way.

use std::cell::Cell;

use leptos::prelude::*;
use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::protocol::{Limit, VaultKind};
use notekpr_core::quick as rules;

use crate::actions;
use crate::state::{self, Route, ViewMode, step};

pub use notekpr_core::quick::{NAME, suggested_title};

/// A quick note for the views.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickNote {
    pub vault: String,
    pub entry: Entry,
    pub text: String,
    /// Made here while there was no room: on this device only, for now.
    pub waiting: bool,
}

/// The quick notes' vault, once they exist.
pub fn vault() -> Option<String> {
    step(|c| c.quick_notes())
}

/// How many quick notes there may be.
pub fn limit() -> Limit {
    step(|c| {
        c.quick_notes()
            .and_then(|v| c.vault_infos.get(&v).map(|i| i.note_limit))
            .unwrap_or(Limit::Max(
                notekpr_core::protocol::DEFAULT_QUICK_NOTES_PER_USER,
            ))
    })
}

/// How many quick notes there are.
pub fn count() -> usize {
    step(|c| {
        c.quick_notes()
            .map(|v| {
                c.live_entries(&v)
                    .iter()
                    .filter(|e| e.kind == EntryKind::Note)
                    .count()
            })
            .unwrap_or(0)
    })
}

/// Whether one more fits.
pub fn has_room() -> bool {
    rules::has_room(count(), limit())
}

thread_local! {
    /// The quick notes are being made (once, not by two tasks at once).
    static MAKING: Cell<bool> = const { Cell::new(false) };
}

/// The quick notes' vault, made the first time (`None`: they can't be made
/// now, e.g. offline).
pub async fn ensure() -> Option<String> {
    while MAKING.get() {
        crate::util::sleep(50).await;
    }
    if let Some(v) = vault() {
        return Some(v);
    }
    if step(|c| c.account.is_none()) {
        return None;
    }
    MAKING.set(true);
    let made = actions::create_vault_of(NAME, VaultKind::QuickNotes).await;
    let v = match made {
        Ok(Some(v)) => Some(v),
        // Another app made them meanwhile.
        Ok(None) => {
            state::refresh_vaults().await;
            vault()
        }
        Err(e) => {
            crate::util::warn(&format!("quick notes could not be set up: {e}"));
            None
        }
    };
    MAKING.set(false);
    v
}

/// The quick notes, newest first, at most `limit`.
pub fn notes(limit: usize) -> Vec<QuickNote> {
    let Some(vault) = vault() else {
        return vec![];
    };
    let list = step(|c| {
        let mut list: Vec<_> = c
            .live_entries(&vault)
            .into_iter()
            .filter(|e| e.kind == EntryKind::Note)
            .collect();
        list.sort_by(|a, b| {
            b.created
                .cmp(&a.created)
                .then(b.modified.cmp(&a.modified))
                .then(a.id.cmp(&b.id))
        });
        list.truncate(limit);
        let r = c.replicas.get(&vault);
        let o = c.outboxes.get(&vault);
        list.into_iter()
            .map(|e| {
                let text = r.map(|r| r.note_text(&e.id)).unwrap_or_default();
                let waiting = o.is_some_and(|o| o.waits_for_room(&e.id));
                (e, text, waiting)
            })
            .collect::<Vec<_>>()
    });
    list.into_iter()
        .map(|(entry, text, waiting)| QuickNote {
            vault: vault.clone(),
            entry,
            text,
            waiting,
        })
        .collect()
}

fn local_stamp() -> String {
    let d = js_sys::Date::new_0();
    rules::stamp_name(
        d.get_full_year() as i32,
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
    )
}

/// Add a quick note with `text`; returns its vault and id.
pub async fn create(text: &str) -> Option<(String, String)> {
    let Some(v) = ensure().await else {
        state::error(NOT_SET_UP);
        return None;
    };
    if !has_room() {
        state::error(rules::full_message(limit()));
        return None;
    }
    let text = text.trim_end().trim_start_matches(['\n', '\r']);
    let id = actions::create_note_with(&v, "", &local_stamp(), text).await?;
    Some((v, id))
}

/// Create an empty quick note and open it, ready to type.
pub fn new_and_open() {
    leptos::task::spawn_local(async {
        if let Some((v, id)) = create("").await {
            let ui = state::ui();
            ui.modal.set(None);
            if ui.mode.get_untracked() == ViewMode::Reading {
                ui.mode.set(ViewMode::Live);
            }
            state::navigate(Route::Note(v, id));
        }
    });
}

/// Delete a quick note for good, with the attachments only it uses. Offline
/// it is gone here at once, and from the server once online.
pub async fn delete(id: &str) -> bool {
    let Some(v) = vault() else {
        return false;
    };
    let own: Vec<String> = step(|c| actions::linked_attachments(c, &v, id))
        .into_iter()
        .filter(|(_, _, only_here)| *only_here)
        .map(|(a, _, _)| a)
        .collect();
    actions::trash_quietly(&v, id);
    for a in &own {
        actions::trash_quietly(&v, a);
    }
    if state::ui().route.get_untracked() == Route::Note(v.clone(), id.to_string()) {
        state::navigate(Route::Quick);
    }
    purge_deleted().await;
    true
}

/// Delete for good what is still in the quick notes' trash (deleted while
/// offline, or a purge that failed).
pub async fn purge_deleted() {
    let Some(v) = vault() else {
        return;
    };
    let ids: Vec<String> = step(|c| {
        c.replicas
            .get(&v)
            .map(|r| {
                r.index
                    .entries()
                    .into_iter()
                    .filter(|e| !e.is_live())
                    .map(|e| e.id)
                    .collect()
            })
            .unwrap_or_default()
    });
    for id in ids {
        if !actions::purge_quietly(&v, &id).await {
            return;
        }
    }
}

/// Delete for good the files no quick note links to any more (unchanged for
/// `UNLINKED_FILE_GRACE_MS`): the quick notes keep files only for their
/// notes. Run once they are caught up — before, the edit with a file's link
/// may not have arrived.
pub async fn drop_unlinked_files() {
    let Some(v) = vault() else {
        return;
    };
    let before = crate::util::now_ms() - rules::UNLINKED_FILE_GRACE_MS;
    let ids = step(|c| {
        c.replicas
            .get(&v)
            .and_then(|r| rules::unlinked_files(r, before))
            .unwrap_or_default()
    });
    if ids.is_empty() {
        return;
    }
    for id in &ids {
        actions::trash_quietly(&v, id);
    }
    purge_deleted().await;
}

pub const NOT_SET_UP: &str =
    "Quick notes are set up the first time you're online with this account.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_are_the_core_ones() {
        assert_eq!(NAME, "Quick notes");
        assert_eq!(suggested_title("Garden plan\nmore"), "Garden plan");
    }
}
