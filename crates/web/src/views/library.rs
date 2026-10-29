//! The saved shapes' panel of a drawing's page, as Excalidraw's library
//! sidebar (`LibraryMenu.tsx`, `LibraryMenuItems.tsx`, `LibraryUnit.tsx`,
//! `LibraryMenuHeaderContent.tsx`): the shapes as small pictures with their
//! names, newest first, each put into the drawing with a click or by
//! dragging it there; what is selected shown first, to add it; importing
//! and exporting `.excalidrawlib` files. Shift-click (a range from the
//! shape picked last), Ctrl/⌘-click or a shape's checkbox picks shapes:
//! clicking or dragging a picked one puts them all in, side by side, and
//! the panel's head then removes or exports just them (Escape lets them
//! go). A shape is renamed from its pencil, or by double-clicking its name.
//! The shapes themselves are `crate::drawing::library`.

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use leptos::prelude::*;
use notekpr_core::library::LibraryItem;
use serde_json::json;
use wasm_bindgen::JsCast;

use super::focus_on_mount;
use super::icons::Icon;
use crate::drawing::Board;
use crate::drawing::library;
use crate::state::ui;
use crate::util;

/// Excalidraw's `onItemSelectToggle`: `id` is picked, or let go if it was.
/// Picked with `range` (Shift) after another one, everything between the
/// two (in `order`, the shapes as shown) is picked too. Returns the picked
/// shapes and the one picked last (none once one was let go: a range then
/// starts afresh).
fn toggle(
    order: &[String],
    picked: &[String],
    last: Option<&str>,
    id: &str,
    range: bool,
) -> (Vec<String>, Option<String>) {
    if picked.iter().any(|p| p == id) {
        let rest = picked.iter().filter(|p| *p != id).cloned().collect();
        return (rest, None);
    }
    let pos = |x: &str| order.iter().position(|o| o == x);
    let ends = last
        .filter(|_| range)
        .and_then(|l| Some((pos(l)?, pos(id)?)));
    let next = match ends {
        Some((a, b)) => {
            let (from, to) = (a.min(b), a.max(b));
            order
                .iter()
                .enumerate()
                .filter(|(i, o)| (from..=to).contains(i) || picked.contains(o))
                .map(|(_, o)| o.clone())
                .collect()
        }
        None => {
            let mut p = picked.to_vec();
            p.push(id.to_string());
            p
        }
    };
    (next, Some(id.to_string()))
}

#[component]
pub fn LibraryPanel(
    board: StoredValue<Rc<Board>, LocalStorage>,
    rev: RwSignal<u64>,
    open: RwSignal<bool>,
) -> impl IntoView {
    let ui = ui();
    let items = Memo::new(move |_| {
        ui.library_rev.track();
        // The Personal storage may load (or be made) after the page.
        ui.vaults.track();
        library::items()
    });
    let writable = Memo::new(move |_| {
        ui.vaults.track();
        board.with_value(|b| b.writable())
    });
    // The shapes picked in the panel (Excalidraw's `selectedItems`), and the
    // one picked last (where a Shift range starts).
    let picked = RwSignal::new(Vec::<String>::new());
    let last = RwSignal::new(None::<String>);
    // Shapes removed meanwhile (here or elsewhere) are let go.
    Effect::new(move |_| {
        let list = items.get();
        let mine = picked.get_untracked();
        let keep: Vec<String> = mine
            .iter()
            .filter(|id| list.iter().any(|i| &i.id == *id))
            .cloned()
            .collect();
        if keep.len() != mine.len() {
            picked.set(keep);
        }
    });
    let pick = move |id: String, range: bool| {
        let order: Vec<String> = items.with_untracked(|l| l.iter().map(|i| i.id.clone()).collect());
        let (next, l) = toggle(
            &order,
            &picked.get_untracked(),
            last.get_untracked().as_deref(),
            &id,
            range,
        );
        picked.set(next);
        last.set(l);
    };
    let let_go = move || {
        picked.set(Vec::new());
        last.set(None);
    };
    // What is selected, to add (Excalidraw's "pending" item): its picture
    // is made once the selection holds still.
    let selected = Memo::new(move |_| {
        rev.get();
        board.with_value(|b| {
            let s = b.scene();
            let ed = b.editor.borrow();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for e in ed.selected(&s) {
                (e.id(), e.version()).hash(&mut h);
            }
            (!ed.selection.is_empty()).then(|| h.finish())
        })
    });
    let pending = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        ui.ui_prefs.track();
        let Some(key) = selected.get() else {
            pending.set(None);
            return;
        };
        leptos::task::spawn_local(async move {
            util::sleep(150).await;
            // (The panel may have closed meanwhile.)
            if selected.try_get_untracked() != Some(Some(key)) {
                return;
            }
            let item = board.try_with_value(|b| b.library_item().ok()).flatten();
            let url = match item {
                Some(i) => library::picture("selection", &i.elements).await,
                None => None,
            };
            let _ = pending.try_set(url);
        });
    });
    let add = move |_| board.with_value(|b| b.add_to_library());
    let import = move |_| {
        crate::actions::pick_files(
            false,
            ".excalidrawlib,application/vnd.excalidrawlib+json,application/json",
            move |files| {
                library::import_files(files, open);
            },
        )
    };
    let remove_picked = move |_| {
        let ids = picked.get_untracked();
        let what = util::count(ids.len(), "shape", "shapes");
        if util::confirm(&format!("Remove {what} from your saved shapes?")) {
            library::remove(&ids);
            let_go();
        }
    };
    let count = move || picked.with(Vec::len);
    view! {
        <aside class="library-panel" aria-label="Saved shapes"
            on:keydown=move |ev: web_sys::KeyboardEvent| {
                // Escape lets the picked shapes go (and stays in the panel).
                if ev.key() == "Escape" && count() > 0 {
                    ev.stop_propagation();
                    let_go();
                }
            }>
            <header class="library-head">
                <Show when=move || count() == 0 fallback=move || view! {
                    <span class="library-title grow library-count">{move || format!("{} selected", count())}</span>
                    <button class="icon-btn" title="Export the selected shapes (.excalidrawlib)" aria-label="Export the selected shapes"
                        on:click=move |_| library::export(&picked.get_untracked())><Icon name="download"/></button>
                    <button class="icon-btn" title="Remove the selected shapes" aria-label="Remove the selected shapes" on:click=remove_picked><Icon name="trash"/></button>
                    <button class="icon-btn" title="Clear the selection (Esc)" aria-label="Clear the selection" on:click=move |_| let_go()><Icon name="minus"/></button>
                }>
                    <span class="library-title grow">"Saved shapes"</span>
                    <button class="icon-btn" title="Import shapes (.excalidrawlib)" aria-label="Import shapes" on:click=import><Icon name="upload"/></button>
                    <button class="icon-btn" title="Export the saved shapes (.excalidrawlib)" aria-label="Export the saved shapes" on:click=move |_| library::export(&[])><Icon name="download"/></button>
                </Show>
                <button class="icon-btn" title="Close" aria-label="Close the saved shapes" on:click=move |_| open.set(false)><Icon name="x"/></button>
            </header>
            <div class="library-items">
                <Show when=move || selected.get().is_some() && writable.get()>
                    <button class="library-unit library-pending" title="Add to saved shapes" aria-label="Add to saved shapes" on:click=add>
                        <span class="library-pic">{move || pending.get().map(|src| view! { <img src=src alt=""/> })}</span>
                        <span class="library-adder"><Icon name="plus"/></span>
                    </button>
                </Show>
                {move || {
                    let list = items.get();
                    if list.is_empty() && selected.get().is_none() {
                        return view! {
                            <p class="library-empty muted">"No saved shapes yet. Select something on the canvas to add it here, or import shapes (.excalidrawlib)."</p>
                        }
                        .into_any();
                    }
                    list.into_iter()
                        .map(|item| view! { <LibraryUnit board=board item=item writable=writable picked=picked pick=pick/> })
                        .collect_view()
                        .into_any()
                }}
            </div>
        </aside>
    }
}

/// One shape: its picture and name. A click (or Enter) puts it — or, when
/// it is picked, every picked shape — in the middle of the view, a drag
/// where it is dropped; Shift/Ctrl/⌘-click picks it.
#[component]
fn LibraryUnit(
    board: StoredValue<Rc<Board>, LocalStorage>,
    item: LibraryItem,
    writable: Memo<bool>,
    picked: RwSignal<Vec<String>>,
    pick: impl Fn(String, bool) + Copy + 'static,
) -> impl IntoView {
    let ui = ui();
    let src = RwSignal::new(None::<String>);
    let id = StoredValue::new(item.id.clone());
    let name = item.name.clone().unwrap_or_default();
    let label = if name.is_empty() {
        "Saved shape".to_string()
    } else {
        name.clone()
    };
    let item = StoredValue::new(item);
    Effect::new(move |_| {
        ui.ui_prefs.track();
        leptos::task::spawn_local(async move {
            let Some((key, els)) = item.try_with_value(|i| (i.id.clone(), i.elements.clone()))
            else {
                return;
            };
            if let Some(url) = library::picture(&key, &els).await {
                let _ = src.try_set(Some(url));
            }
        });
    });
    let is_picked = move || id.with_value(|id| picked.with(|p| p.contains(id)));
    let insert = move || {
        if !writable.get_untracked() {
            return;
        }
        let mine = id.get_value();
        let chosen = picked.get_untracked();
        let mut list = library::items();
        if chosen.contains(&mine) {
            list.retain(|i| chosen.contains(&i.id));
        } else {
            list.retain(|i| i.id == mine);
        }
        board.with_value(|b| b.insert_library_items(list, None));
    };
    let drag = move |ev: web_sys::DragEvent| {
        let mine = id.get_value();
        let chosen = picked.get_untracked();
        let ids = if chosen.contains(&mine) {
            chosen
        } else {
            vec![mine]
        };
        if let Some(d) = ev.data_transfer() {
            let _ = d.set_data(library::DRAG_TYPE, &json!({"itemIds": ids}).to_string());
            d.set_effect_allowed("copy");
        }
    };
    let remove = move |ev: web_sys::MouseEvent| {
        ev.stop_propagation();
        if util::confirm("Remove this shape from your saved shapes?") {
            library::remove(&[id.get_value()]);
        }
    };
    // Renaming: the name becomes a field (Enter keeps it, Escape or
    // clicking elsewhere doesn't).
    let renaming = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let start_rename = {
        let name = name.clone();
        move || {
            draft.set(name.clone());
            renaming.set(true);
        }
    };
    let start = start_rename.clone();
    let name_view = move || {
        if renaming.get() {
            view! {
                <form class="library-rename" on:submit=move |ev| {
                    ev.prevent_default();
                    renaming.set(false);
                    library::rename(&id.get_value(), &draft.get_untracked());
                }>
                    <input class="input small" aria-label="Name of the shape" use:focus_on_mount
                        prop:value=move || draft.get()
                        // The whole name, to type over.
                        on:focus=|ev| {
                            if let Some(i) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) {
                                i.select();
                            }
                        }
                        on:input=move |ev| draft.set(event_target_value(&ev))
                        on:click=|ev| ev.stop_propagation()
                        // Keys typed here are the name's, not the drawing's.
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            ev.stop_propagation();
                            if ev.key() == "Escape" {
                                renaming.set(false);
                            }
                        }
                        on:keyup=|ev: web_sys::KeyboardEvent| ev.stop_propagation()
                        on:blur=move |_| renaming.set(false)/>
                </form>
            }
            .into_any()
        } else {
            let start = start.clone();
            view! {
                <span class="library-name" title=name.clone()
                    on:dblclick=move |ev| { ev.stop_propagation(); start(); }>{name.clone()}</span>
            }
            .into_any()
        }
    };
    view! {
        <div class="library-unit" class:picked=is_picked role="button" tabindex="0" title=label.clone() aria-label=label
            aria-pressed=move || is_picked().to_string()
            draggable=move || (writable.get() && !renaming.get()).to_string()
            on:click=move |ev: web_sys::MouseEvent| {
                if renaming.get_untracked() {
                    return;
                }
                if ev.shift_key() || ev.ctrl_key() || ev.meta_key() {
                    ev.prevent_default();
                    pick(id.get_value(), ev.shift_key());
                } else {
                    insert();
                }
            }
            on:keydown=move |ev: web_sys::KeyboardEvent| {
                // On the shape itself, not its buttons.
                let on_item = ev
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|t| t.class_list().contains("library-unit"));
                if ev.key() == "Enter" && on_item {
                    ev.prevent_default();
                    ev.stop_propagation();
                    insert();
                }
            }
            on:dragstart=drag>
            <span class="library-pic">{move || src.get().map(|s| view! { <img src=s alt="" draggable="false"/> })}</span>
            {name_view}
            <button class="library-check icon-btn tiny" title="Select (Shift-click: a range)" aria-label="Select this shape"
                aria-pressed=move || is_picked().to_string()
                on:click=move |ev: web_sys::MouseEvent| {
                    ev.stop_propagation();
                    pick(id.get_value(), ev.shift_key());
                }>
                <Show when=is_picked><Icon name="check"/></Show>
            </button>
            <span class="library-actions">
                <button class="icon-btn tiny" title="Rename" aria-label="Rename the shape"
                    on:click=move |ev: web_sys::MouseEvent| { ev.stop_propagation(); start_rename(); }><Icon name="edit"/></button>
                <button class="library-remove icon-btn tiny" title="Remove from saved shapes" aria-label="Remove from saved shapes" on:click=remove><Icon name="trash"/></button>
            </span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::toggle;

    fn ids(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    /// As Excalidraw's `onItemSelectToggle` (and its tests: ranges both
    /// ways, keeping what was picked, letting go).
    #[test]
    fn shapes_are_picked_one_by_one_or_in_ranges() {
        let order = ids(&["a", "b", "c", "d", "e"]);
        let (p, last) = toggle(&order, &[], None, "b", false);
        assert_eq!((p.clone(), last.as_deref()), (ids(&["b"]), Some("b")));
        // Shift: from the last one picked, either way.
        let (p, last) = toggle(&order, &p, last.as_deref(), "d", true);
        assert_eq!(p, ids(&["b", "c", "d"]));
        let (p2, _) = toggle(&order, &ids(&["d"]), Some("d"), "a", true);
        assert_eq!(p2, ids(&["a", "b", "c", "d"]));
        // Ranges keep what was picked outside them.
        let (p3, _) = toggle(&order, &ids(&["e", "a"]), Some("a"), "b", true);
        assert_eq!(p3, ids(&["a", "b", "e"]));
        // Without Shift, one more.
        let (p, last) = toggle(&order, &p, last.as_deref(), "a", false);
        assert_eq!(p, ids(&["b", "c", "d", "a"]));
        assert_eq!(last.as_deref(), Some("a"));
        // Picked again: let go; a range then has nowhere to start.
        let (p, last) = toggle(&order, &p, last.as_deref(), "c", false);
        assert_eq!((p.clone(), last.clone()), (ids(&["b", "d", "a"]), None));
        let (p, _) = toggle(&order, &p, last.as_deref(), "e", true);
        assert_eq!(p, ids(&["b", "d", "a", "e"]));
        // A last one no longer shown: just this one.
        let (p, _) = toggle(&order, &[], Some("gone"), "c", true);
        assert_eq!(p, ids(&["c"]));
    }
}
