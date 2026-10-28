//! A drawing's page. A document route (`/v/<vault>/<id>`) shows a note or
//! a drawing: which one is only known once the vault's index is there, so
//! [`Document`] picks the view then (the note view never opens an editor on
//! anything but a note).

use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::paths;

use super::icons::Icon;
use crate::state::{self, ui};

/// A note or a drawing, by id.
#[component]
pub fn Document(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let (v, n) = (vault.clone(), note.clone());
    let drawing = Memo::new(move |_| {
        ui.tree_rev.get();
        ui.vaults.track();
        state::step(|c| c.entry(&v, &n)).is_some_and(|e| e.kind == EntryKind::Drawing)
    });
    move || {
        let (vault, note) = (vault.clone(), note.clone());
        if drawing.get() {
            view! { <DrawingView vault=vault id=note/> }.into_any()
        } else {
            view! { <super::note::NoteView vault=vault note=note/> }.into_any()
        }
    }
}

#[component]
pub fn DrawingView(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let (v, i) = (vault.clone(), id.clone());
    state::step(|c| c.ensure_note_loaded(&v, &i));
    let (v, i) = (vault.clone(), id.clone());
    let crumbs = Memo::new(move |_| {
        ui.tree_rev.get();
        let path = state::step(|c| c.entry(&v, &i))
            .map(|e| e.path)
            .unwrap_or_default();
        let mut crumbs = vec![paths::drawing_title(&path).to_string()];
        let folder = paths::parent(&path);
        if !folder.is_empty() {
            crumbs.extend(folder.split('/').map(str::to_string));
        }
        crumbs.join(" / ")
    });
    view! {
        <div class="note-page drawing-page">
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{move || crumbs.get()}</div>
            </header>
            <div class="drawing-stage"></div>
        </div>
    }
}
