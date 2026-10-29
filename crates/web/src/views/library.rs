//! The saved shapes' panel of a drawing's page, as Excalidraw's library
//! sidebar (`LibraryMenu.tsx`, `LibraryMenuItems.tsx`, `LibraryUnit.tsx`):
//! the shapes as small pictures, newest first, each put into the drawing
//! with a click or by dragging it there; what is selected shown first, to
//! add it; importing and exporting `.excalidrawlib` files. The shapes
//! themselves are `crate::drawing::library`.

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use leptos::prelude::*;
use notekpr_core::library::LibraryItem;
use serde_json::json;
use wasm_bindgen::JsCast;

use super::icons::Icon;
use crate::drawing::Board;
use crate::drawing::library;
use crate::state::ui;
use crate::util;

#[component]
pub fn LibraryPanel(
    board: StoredValue<Rc<Board>, LocalStorage>,
    rev: RwSignal<u64>,
    open: RwSignal<bool>,
) -> impl IntoView {
    let ui = ui();
    let items = Memo::new(move |_| {
        ui.library_rev.track();
        // The Drawings section may load (or be made) after the page.
        ui.vaults.track();
        library::items()
    });
    let writable = Memo::new(move |_| {
        ui.vaults.track();
        board.with_value(|b| b.writable())
    });
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
    view! {
        <aside class="library-panel" aria-label="Saved shapes">
            <header class="library-head">
                <span class="library-title grow">"Saved shapes"</span>
                <button class="icon-btn" title="Import shapes (.excalidrawlib)" aria-label="Import shapes" on:click=import><Icon name="upload"/></button>
                <button class="icon-btn" title="Export the saved shapes (.excalidrawlib)" aria-label="Export the saved shapes" on:click=move |_| library::export()><Icon name="download"/></button>
                <button class="icon-btn" title="Close" aria-label="Close the saved shapes" on:click=move |_| open.set(false)><Icon name="x"/></button>
            </header>
            <div class="library-items">
                <Show when=move || selected.get().is_some() && writable.get()>
                    <button class="library-unit library-pending" title="Add to saved shapes" aria-label="Add to saved shapes" on:click=add>
                        {move || pending.get().map(|src| view! { <img src=src alt=""/> })}
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
                        .map(|item| view! { <LibraryUnit board=board item=item writable=writable/> })
                        .collect_view()
                        .into_any()
                }}
            </div>
        </aside>
    }
}

/// One item: its picture; a click (or Enter) puts it in the middle of the
/// view, a drag where it is dropped.
#[component]
fn LibraryUnit(
    board: StoredValue<Rc<Board>, LocalStorage>,
    item: LibraryItem,
    writable: Memo<bool>,
) -> impl IntoView {
    let ui = ui();
    let src = RwSignal::new(None::<String>);
    let id = item.id.clone();
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
    let insert = move || {
        if writable.get_untracked() {
            item.with_value(|i| board.with_value(|b| b.insert_library_item(i, None)));
        }
    };
    let drag = {
        let id = id.clone();
        move |ev: web_sys::DragEvent| {
            if let Some(d) = ev.data_transfer() {
                let _ = d.set_data(library::DRAG_TYPE, &json!({"itemIds": [id]}).to_string());
                d.set_effect_allowed("copy");
            }
        }
    };
    let remove = move |ev: web_sys::MouseEvent| {
        ev.stop_propagation();
        if util::confirm("Remove this shape from your saved shapes?") {
            library::remove(&id);
        }
    };
    view! {
        <div class="library-unit" role="button" tabindex="0" title=name aria-label=label
            draggable=move || writable.get().to_string()
            on:click=move |_| insert()
            on:keydown=move |ev: web_sys::KeyboardEvent| {
                // On the item itself, not its remove button.
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
            {move || src.get().map(|s| view! { <img src=s alt="" draggable="false"/> })}
            <button class="library-remove icon-btn tiny" title="Remove from saved shapes" aria-label="Remove from saved shapes" on:click=remove><Icon name="trash"/></button>
        </div>
    }
}
