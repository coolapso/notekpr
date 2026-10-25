//! The file tree of a vault (folders, notes, attachments).

use super::focus_on_mount;
use std::collections::{BTreeMap, BTreeSet};

use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::moving::What;
use notekpr_core::{order, paths};
use wasm_bindgen::JsCast;

use crate::actions::{self, Spot};
use crate::state::{self, Modal, Route, ui};
use crate::views::icons::Icon;

#[derive(Clone, Debug, PartialEq)]
struct Row {
    key: String,
    id: Option<String>,
    kind: EntryKind,
    path: String,
    name: String,
    depth: usize,
    expanded: bool,
    has_children: bool,
    /// A note shared individually with people outside the vault.
    shared: bool,
    icon: Option<String>,
}

fn build_rows(vault: &str, expanded: &BTreeSet<String>) -> Vec<Row> {
    let (entries, shared): (_, BTreeSet<String>) = state::step(|c| {
        let shared = c
            .replicas
            .get(vault)
            .map(|r| r.shared.clone())
            .unwrap_or_default();
        (c.live_entries(vault), shared)
    });
    let children = order::tree(&entries);
    let mut out = Vec::new();
    let walk = Walk {
        children: &children,
        expanded,
        shared: &shared,
    };
    walk.rows("", 0, &mut out);
    out
}

/// The rows of the open part of the tree, each folder's children in order.
struct Walk<'a> {
    children: &'a BTreeMap<String, Vec<order::Child>>,
    expanded: &'a BTreeSet<String>,
    shared: &'a BTreeSet<String>,
}

impl Walk<'_> {
    fn rows(&self, parent: &str, depth: usize, out: &mut Vec<Row>) {
        for c in self.children.get(parent).into_iter().flatten() {
            let folder = c.is_folder();
            let e = c.entry.as_ref();
            let expanded = folder && self.expanded.contains(&c.path);
            out.push(Row {
                key: match e {
                    Some(e) if !folder => format!("f:{}", e.id),
                    _ => format!("d:{}", c.path),
                },
                id: e.map(|e| e.id.clone()),
                kind: e.map_or(EntryKind::Folder, |e| e.kind),
                path: c.path.clone(),
                name: c.name().to_string(),
                depth,
                expanded,
                has_children: folder && self.children.get(&c.path).is_some_and(|l| !l.is_empty()),
                shared: !folder && e.is_some_and(|e| self.shared.contains(&e.id)),
                icon: e.and_then(|e| e.icon.clone()),
            });
            if expanded {
                self.rows(&c.path, depth + 1, out);
            }
        }
    }
}

/// What is being dragged in the tree.
#[derive(Clone, Debug, PartialEq)]
struct Dragged {
    path: String,
    folder: bool,
}

/// Where `d` lands when dropped `f` of the way down the row of `path`
/// (`open`: an expanded folder showing its contents below it). Folders stay
/// above notes, so each only goes between its own kind; otherwise into the
/// folder. `None`: not on itself or into itself.
fn spot_for(d: &Dragged, path: &str, folder: bool, open: bool, f: f64) -> Option<Spot> {
    if d.path == path || (d.folder && paths::is_within(path, &d.path)) {
        return None;
    }
    Some(match (d.folder, folder) {
        (true, false) => Spot::Into(paths::parent(path).to_string()),
        (false, true) => Spot::Into(path.to_string()),
        (true, true) if f < 0.25 => Spot::Before(path.to_string()),
        (true, true) if f > 0.75 && !open => Spot::After(path.to_string()),
        (true, true) => Spot::Into(path.to_string()),
        (false, false) if f < 0.5 => Spot::Before(path.to_string()),
        (false, false) => Spot::After(path.to_string()),
    })
}

/// The element of the event's target matching `css` (Leptos delegates
/// events, so `current_target` isn't the element with the handler).
fn target_within(ev: &web_sys::Event, css: &str) -> Option<web_sys::Element> {
    ev.target()?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest(css)
        .ok()
        .flatten()
}

/// How far down the row under the pointer it is, from 0 to 1.
fn fraction_down(ev: &web_sys::DragEvent) -> f64 {
    let Some(row) = target_within(ev, ".tree-row") else {
        return 0.5;
    };
    let r = row.get_bounding_client_rect();
    if r.height() <= 0.0 {
        return 0.5;
    }
    ((f64::from(ev.client_y()) - r.top()) / r.height()).clamp(0.0, 1.0)
}

/// `aria-level`, which Leptos has no typed attribute for.
fn aria_level(el: web_sys::Element, level: usize) {
    let _ = el.set_attribute("aria-level", &level.to_string());
}

#[component]
pub fn Tree(vault: String) -> impl IntoView {
    let ui = ui();
    let expanded = RwSignal::new(BTreeSet::<String>::new());
    let menu = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        ui.close_menus.track();
        menu.set(None);
    });
    let renaming = RwSignal::new(None::<String>);
    let rename_text = RwSignal::new(String::new());
    let dragging = RwSignal::new(None::<Dragged>);
    let drop_target = RwSignal::new(None::<Spot>);
    // Set on every `dragover` (many a second): only notify on a change.
    let aim = move |s: Option<Spot>| {
        if drop_target.with_untracked(|t| *t != s) {
            drop_target.set(s);
        }
    };
    let v0 = vault.clone();

    // Folders open and close only when clicked, so nothing moves by itself.
    // A closed folder holding the open note is marked instead.
    let v_open = vault.clone();
    let open_path = Memo::new(move |_| {
        ui.tree_rev.track();
        match ui.route.get() {
            Route::Note(v, n) if v == v_open => state::step(|c| c.entry(&v, &n)).map(|e| e.path),
            _ => None,
        }
    });

    let v_rows = vault.clone();
    let rows = Memo::new(move |_| {
        ui.tree_rev.get();
        build_rows(&v_rows, &expanded.get())
    });
    let can_write = {
        let v = vault.clone();
        move || {
            ui.vaults.get();
            state::step(|c| c.can_write(&v))
        }
    };
    let can_write2 = can_write.clone();
    let vroot = vault.clone();

    view! {
        <div class="tree" role="tree"
            class:drop=move || drop_target.with(|t| matches!(t, Some(Spot::Into(f)) if f.is_empty()))
            // Rows say where a drop over them lands; elsewhere it's the top level.
            on:dragover=move |ev: web_sys::DragEvent| {
                if dragging.with_untracked(Option::is_some) {
                    ev.prevent_default();
                    aim(Some(Spot::Into(String::new())));
                }
            }
            on:dragleave=move |ev: web_sys::DragEvent| {
                // Only when leaving the tree, not when moving between rows.
                let tree = target_within(&ev, ".tree");
                let to = ev.related_target().and_then(|t| t.dyn_into::<web_sys::Node>().ok());
                if !tree.is_some_and(|t| t.contains(to.as_ref())) {
                    aim(None);
                }
            }
            on:drop={
                let v = vroot.clone();
                move |ev: web_sys::DragEvent| {
                    ev.prevent_default();
                    let (d, spot) = (dragging.get_untracked(), drop_target.get_untracked());
                    dragging.set(None);
                    drop_target.set(None);
                    let (Some(d), Some(spot)) = (d, spot) else { return };
                    if let Spot::Into(f) = &spot && !f.is_empty() {
                        expanded.update(|s| { s.insert(f.clone()); });
                    }
                    if let Err(e) = actions::place_entry(&v, &d.path, &spot) { state::error(e); }
                }
            }>
            // Not `<For>`: tachys 0.2's keyed diff can put a row in the wrong
            // place when rows move and others appear or go in one update.
            {move || {
                // Only those who can change the vault move things.
                let draggable = if can_write2() { "true" } else { "false" };
                rows.get().into_iter().map(|row| {
                    let vault = v0.clone();
                    let row2 = row.clone();
                    let is_folder = row.kind == EntryKind::Folder;
                    let id = row.id.clone();
                    let path = row.path.clone();
                    let key = row.key.clone();
                    let key2 = key.clone();
                    let key3 = key.clone();
                    let active = {
                        let id = id.clone();
                        move || matches!(ui.route.get(), Route::Note(_, n) if Some(&n) == id.as_ref())
                    };
                    let holds_open = {
                        let path = path.clone();
                        let closed = is_folder && !row.expanded;
                        move || closed && open_path.get().is_some_and(|p| paths::is_within(&p, &path))
                    };
                    let icon = match row.kind {
                        EntryKind::Folder => "folder",
                        EntryKind::Note => "file",
                        EntryKind::Attachment => if crate::util::is_image(&row.name) { "image" } else { "paperclip" },
                    };
                    let indent = 8 + row.depth * 14;
                    let indent = format!("padding-left:{indent}px;--indent:{indent}px");
                    let on_click = {
                        let vault = vault.clone();
                        let id = id.clone();
                        let path = path.clone();
                        let kind = row.kind;
                        move |_| {
                            match kind {
                                EntryKind::Folder => expanded.update(|s| { if !s.remove(&path) { s.insert(path.clone()); } }),
                                EntryKind::Note => if let Some(id) = &id { state::navigate(Route::Note(vault.clone(), id.clone())); },
                                EntryKind::Attachment => if let Some(id) = &id {
                                    let (v, i) = (vault.clone(), id.clone());
                                    leptos::task::spawn_local(async move { actions::open_attachment(&v, &i).await });
                                },
                            }
                        }
                    };
                    let open = row.expanded && row.has_children;
                    let (p1, p2, p3, p4, p5) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
                    view! {
                        <div class="tree-row" role="treeitem" use:aria_level={row.depth + 1}
                            aria-expanded=is_folder.then(|| row.expanded.to_string())
                            class:active=active class:holds-open=holds_open class:folder=is_folder
                            class:drop=move || drop_target.with(|t| matches!(t, Some(Spot::Into(f)) if *f == p1))
                            class:drop-before=move || drop_target.with(|t| matches!(t, Some(Spot::Before(p)) if *p == p2))
                            class:drop-after=move || drop_target.with(|t| matches!(t, Some(Spot::After(p)) if *p == p3))
                            style=indent draggable=draggable
                            on:dragstart=move |ev: web_sys::DragEvent| {
                                if let Some(dt) = ev.data_transfer() {
                                    dt.set_effect_allowed("move");
                                    let _ = dt.set_data("text/x-notekpr-entry", &p4);
                                }
                                dragging.set(Some(Dragged { path: p4.clone(), folder: is_folder }));
                            }
                            on:dragend=move |_| {
                                dragging.set(None);
                                aim(None);
                            }
                            on:dragover=move |ev: web_sys::DragEvent| {
                                let Some(spot) = dragging.with_untracked(|d| {
                                    d.as_ref().map(|d| spot_for(d, &p5, is_folder, open, fraction_down(&ev)))
                                }) else { return };
                                // Here, not the tree's top level (nowhere when on itself).
                                ev.stop_propagation();
                                if spot.is_some() {
                                    ev.prevent_default();
                                }
                                aim(spot);
                            }>
                            {move || if renaming.get().as_deref() == Some(key3.as_str()) {
                                let row = row2.clone();
                                let vault = vault.clone();
                                view! {
                                    <form class="tree-rename" on:submit=move |ev| {
                                        ev.prevent_default();
                                        let name = rename_text.get_untracked();
                                        let res = match &row.id {
                                            Some(id) => actions::rename_entry(&vault, id, &name),
                                            None => actions::rename_implicit_folder(&vault, &row.path, &name),
                                        };
                                        match res { Ok(()) => renaming.set(None), Err(e) => state::error(e) }
                                    }>
                                        <input class="input small" use:focus_on_mount prop:value=move || rename_text.get()
                                            on:input=move |ev| rename_text.set(event_target_value(&ev))
                                            on:keydown=move |ev: web_sys::KeyboardEvent| if ev.key() == "Escape" { renaming.set(None); }
                                            on:blur=move |_| renaming.set(None)/>
                                    </form>
                                }.into_any()
                            } else {
                                let name = row2.name.clone();
                                let caret = if is_folder { if row2.expanded { "chevron-down" } else { "chevron-right" } } else { "" };
                                view! {
                                    <button class="tree-item" on:click=on_click.clone() title=row2.path.clone()>
                                        {(!caret.is_empty()).then(|| view! { <Icon name=caret class="caret"/> })}
                                        {super::entry_icon(row2.icon.clone(), icon)}
                                        <span class="name">{name}</span>
                                        {row2.shared.then(|| view! { <span class="shared-mark" title="Shared individually with people outside the vault"><Icon name="users"/></span> })}
                                    </button>
                                }.into_any()
                        }}
                        <button class="tree-more" title="More" aria-label="More" on:click={
                            let key = key.clone();
                            move |ev: web_sys::MouseEvent| { ev.stop_propagation(); menu.update(|m| *m = if m.as_deref() == Some(key.as_str()) { None } else { Some(key.clone()) }); }
                        }><Icon name="more"/></button>
                        {
                            let row = row.clone();
                            let can_write = can_write2.clone();
                            let vault = v0.clone();
                            move || (menu.get().as_deref() == Some(key2.as_str())).then(|| {
                                let row = row.clone();
                                let writable = can_write();
                                view! { <RowMenu vault=vault.clone() row_id=row.id.clone() kind=row.kind path=row.path.clone() name=row.name.clone()
                                    writable=writable menu=menu renaming=renaming rename_text=rename_text expanded=expanded row_key=row.key.clone()/> }
                            })
                        }
                    </div>
                }
                }).collect_view()
            }}
            {
                let v = vault.clone();
                move || (rows.get().is_empty() && ui.vaults.get().iter().any(|x| x.id == v && x.loaded)).then(|| view! {
                    <div class="tree-empty muted small">"No notes yet."</div>
                })
            }
            {
                let can_write = can_write.clone();
                let v = vault.clone();
                move || can_write().then(|| {
                    let v = v.clone();
                    view! { <button class="tree-add" on:click=move |_| actions::new_note(&v, "")><Icon name="plus"/>" New note"</button> }
                })
            }
        </div>
    }
}

#[component]
fn RowMenu(
    vault: String,
    row_id: Option<String>,
    kind: EntryKind,
    path: String,
    name: String,
    writable: bool,
    menu: RwSignal<Option<String>>,
    renaming: RwSignal<Option<String>>,
    rename_text: RwSignal<String>,
    expanded: RwSignal<BTreeSet<String>>,
    row_key: String,
) -> impl IntoView {
    let ui = ui();
    let close = move || menu.set(None);
    let is_folder = kind == EntryKind::Folder;
    let v1 = vault.clone();
    let v2 = vault.clone();
    let v3 = vault.clone();
    let v4 = vault.clone();
    let v5 = vault.clone();
    let p1 = path.clone();
    let p2 = path.clone();
    let p3 = path.clone();
    let id1 = row_id.clone();
    let id2 = row_id.clone();
    let id3 = row_id.clone();
    let id4 = row_id.clone();
    view! {
        <div class="menu" on:click=|ev| ev.stop_propagation()>
            {(is_folder && writable).then(|| view! {
                <button on:click=move |_| { close(); actions::new_note(&v1, &p1); expanded.update(|s| { s.insert(p1.clone()); }); }>
                    <Icon name="file-plus"/>" New note here"</button>
                <button on:click=move |_| { close(); ui.modal.set(Some(Modal::NewFolder { vault: v2.clone(), parent: p2.clone() })); }>
                    <Icon name="folder-plus"/>" New folder here"</button>
            })}
            {(is_folder && writable && actions::has_custom_order(&vault, &path)).then(|| {
                let (v, p) = (vault.clone(), path.clone());
                view! { <button on:click=move |_| { close(); actions::sort_folder(&v, &p); }>
                    <Icon name="sort"/>" Sort A–Z"</button> }
            })}
            {(kind == EntryKind::Note).then(|| {
                let (v, id) = (v3.clone(), id1.clone().unwrap_or_default());
                let fav = actions::is_favorite(&v, &id);
                // Owners share single notes, as from the note's header.
                let owner = state::step(|c| c.is_owner(&v));
                let share = (v.clone(), id.clone());
                view! {
                    <button on:click=move |_| { close(); actions::toggle_favorite(&v, &id); }>
                        <Icon name="star"/>{if fav { " Remove from favorites" } else { " Add to favorites" }}</button>
                    {owner.then(|| view! {
                        <button on:click=move |_| { close(); ui.modal.set(Some(Modal::ShareNote(share.0.clone(), share.1.clone()))); }>
                            <Icon name="share"/>" Share…"</button>
                    })}
                }
            })}
            {(kind == EntryKind::Attachment).then(|| {
                let (v, id) = (v4.clone(), id2.clone().unwrap_or_default());
                view! { <button on:click=move |_| { close(); let (v, id) = (v.clone(), id.clone()); leptos::task::spawn_local(async move { actions::download_attachment(&v, &id).await }); }>
                    <Icon name="download"/>" Download"</button> }
            })}
            {(writable && kind != EntryKind::Attachment).then(|| {
                let (v, id, p) = (vault.clone(), row_id.clone(), path.clone());
                view! { <button on:click=move |_| {
                    close();
                    ui.modal.set(Some(Modal::EntryIcon { vault: v.clone(), id: id.clone(), path: p.clone(), folder: is_folder }));
                }><Icon name="smile"/>" Set icon…"</button> }
            })}
            {writable.then(|| {
                let name = name.clone();
                let key = row_key.clone();
                let vault = v5.clone();
                let vault2 = v5.clone();
                let id_move = id3.clone();
                let id_del = id4.clone();
                let p_del = p3.clone();
                // Notes and folders; files go with the notes linking to them.
                let to_vault = match kind {
                    EntryKind::Note => id3.clone().map(What::Note),
                    EntryKind::Folder => Some(What::Folder(path.clone())),
                    EntryKind::Attachment => None,
                };
                let vault3 = v5.clone();
                view! {
                    <button on:click=move |_| { close(); rename_text.set(name.clone()); renaming.set(Some(key.clone())); }>
                        <Icon name="edit"/>" Rename"</button>
                    {id_move.map(|id| view! {
                        <button on:click=move |_| { close(); ui.modal.set(Some(Modal::Move { vault: vault.clone(), id: id.clone() })); }>
                            <Icon name="move"/>" Move to…"</button>
                    })}
                    {to_vault.map(|what| view! {
                        <button class="move-to-vault" on:click=move |_| { close(); ui.modal.set(Some(Modal::MoveToVault { vault: vault3.clone(), what: what.clone() })); }>
                            <Icon name="file-plus"/>" Move to vault…"</button>
                    })}
                    <button class="danger" on:click=move |_| {
                        close();
                        match (&id_del, is_folder) {
                            (_, true) => actions::trash_folder(&vault2, &p_del),
                            (Some(id), false) => actions::trash(&vault2, id),
                            _ => {}
                        }
                    }><Icon name="trash"/>" Move to trash"</button>
                }
            })}
        </div>
    }
}
