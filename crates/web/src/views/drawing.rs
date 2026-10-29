//! A drawing's page. A document route (`/v/<vault>/<id>`) shows a note or
//! a drawing: which one is only known once the vault's index is there, so
//! [`Document`] picks the view then (the note view never opens an editor on
//! anything but a note).

use leptos::html;
use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::paths;
use wasm_bindgen::JsCast;

use super::icons::Icon;
use crate::drawing::Board;
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
    let board = Board::new(&vault, &id);
    let canvas_ref = NodeRef::<html::Canvas>::new();
    let b = board.clone();
    Effect::new(move |_| {
        if let Some(c) = canvas_ref.get() {
            b.attach(c.unchecked_into());
        }
    });
    // Drawn again when it changes elsewhere, or the theme does.
    let b = board.clone();
    Effect::new(move |_| {
        ui.content_rev.get();
        ui.ui_prefs.track();
        ui.vaults.track();
        b.reload();
    });
    let kept = StoredValue::new_local(board.clone());
    on_cleanup(move || {
        kept.try_with_value(|b| b.close());
    });
    let zoom = RwSignal::new(1.0);
    board.on_zoom(move |z| {
        if zoom.get_untracked() != z {
            zoom.set(z);
        }
    });
    let shown_zoom = move || format!("{}%", (zoom.get() * 100.0).round());
    // Dragging scrolls the view (the editor's tools come next).
    let (bd, bm, bu) = (board.clone(), board.clone(), board.clone());
    let on_down = move |ev: web_sys::PointerEvent| {
        if let Some(t) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = t.set_pointer_capture(ev.pointer_id());
        }
        bd.pan.set(Some((
            ev.pointer_id(),
            ev.client_x() as f64,
            ev.client_y() as f64,
        )));
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        if let Some((pid, x, y)) = bm.pan.get()
            && pid == ev.pointer_id()
        {
            let (nx, ny) = (ev.client_x() as f64, ev.client_y() as f64);
            bm.view.set(bm.view.get().scrolled(nx - x, ny - y));
            bm.pan.set(Some((pid, nx, ny)));
            bm.schedule();
        }
    };
    let on_up = move |_: web_sys::PointerEvent| bu.pan.set(None);
    let (bi, bo, bf) = (board.clone(), board.clone(), board.clone());
    view! {
        <div class="note-page drawing-page">
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{move || crumbs.get()}</div>
            </header>
            <div class="drawing-stage">
                <canvas class="drawing-canvas" node_ref=canvas_ref
                    on:pointerdown=on_down on:pointermove=on_move on:pointerup=on_up.clone() on:pointercancel=on_up></canvas>
                <div class="drawing-zoom" role="group" aria-label="Zoom">
                    <button title="Zoom out" aria-label="Zoom out" on:click=move |_| { bo.zoom_by(1.0 / 1.1) }>"−"</button>
                    <button class="zoom-value" title="Show all of the drawing" on:click=move |_| { bf.fit() }>{shown_zoom}</button>
                    <button title="Zoom in" aria-label="Zoom in" on:click=move |_| { bi.zoom_by(1.1) }>"+"</button>
                </div>
            </div>
        </div>
    }
}
