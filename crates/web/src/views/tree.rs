//! The file tree of a vault (folders, notes, attachments).

use std::collections::{BTreeMap, BTreeSet};

use coolpsonotes_core::doc::EntryKind;
use coolpsonotes_core::paths;
use leptos::prelude::*;

use crate::actions;
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
}

fn build_rows(vault: &str, expanded: &BTreeSet<String>) -> Vec<Row> {
    let entries = state::live_entries(vault);
    let shared: BTreeSet<String> = state::with(|c| c.replicas.get(vault).map(|r| r.doc_keys.keys().cloned().collect()).unwrap_or_default());
    let folders: BTreeMap<String, Option<String>> = {
        let mut m: BTreeMap<String, Option<String>> = BTreeMap::new();
        for e in &entries {
            if e.kind == EntryKind::Folder {
                m.insert(e.path.clone(), Some(e.id.clone()));
            }
            let mut p = paths::parent(&e.path);
            while !p.is_empty() {
                m.entry(p.to_string()).or_insert(None);
                p = paths::parent(p);
            }
        }
        m
    };
    let mut children: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for (path, id) in &folders {
        let parent = paths::parent(path).to_string();
        children.entry(parent).or_default().push(Row {
            key: format!("d:{path}"),
            id: id.clone(),
            kind: EntryKind::Folder,
            path: path.clone(),
            name: paths::file_name(path).to_string(),
            depth: 0,
            expanded: expanded.contains(path),
            has_children: false,
            shared: false,
        });
    }
    for e in &entries {
        if e.kind == EntryKind::Folder {
            continue;
        }
        let name = if e.kind == EntryKind::Note { e.title().to_string() } else { paths::file_name(&e.path).to_string() };
        children.entry(paths::parent(&e.path).to_string()).or_default().push(Row {
            key: format!("f:{}", e.id),
            id: Some(e.id.clone()),
            kind: e.kind,
            path: e.path.clone(),
            name,
            depth: 0,
            expanded: false,
            has_children: false,
            shared: shared.contains(&e.id),
        });
    }
    for list in children.values_mut() {
        list.sort_by(|a, b| {
            let fa = a.kind != EntryKind::Folder;
            let fb = b.kind != EntryKind::Folder;
            fa.cmp(&fb).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    let mut out = Vec::new();
    fn walk(parent: &str, depth: usize, children: &BTreeMap<String, Vec<Row>>, out: &mut Vec<Row>) {
        let Some(list) = children.get(parent) else { return };
        for r in list {
            let mut r = r.clone();
            r.depth = depth;
            r.has_children = r.kind == EntryKind::Folder && children.get(&r.path).is_some_and(|c| !c.is_empty());
            let recurse = r.kind == EntryKind::Folder && r.expanded;
            let p = r.path.clone();
            out.push(r);
            if recurse {
                walk(&p, depth + 1, children, out);
            }
        }
    }
    walk("", 0, &children, &mut out);
    out
}

#[component]
pub fn Tree(vault: String) -> impl IntoView {
    let ui = ui();
    let expanded = RwSignal::new(BTreeSet::<String>::new());
    let menu = RwSignal::new(None::<String>);
    let renaming = RwSignal::new(None::<String>);
    let rename_text = RwSignal::new(String::new());
    let drop_target = RwSignal::new(None::<String>);
    let v0 = vault.clone();

    // Reveal the open note's folder.
    Effect::new(move |_| {
        if let Route::Note(v, n) = ui.route.get()
            && let Some(e) = state::entry(&v, &n) {
                let mut p = paths::parent(&e.path).to_string();
                expanded.update(|s| {
                    while !p.is_empty() {
                        s.insert(p.clone());
                        p = paths::parent(&p).to_string();
                    }
                });
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
            state::can_write(&v)
        }
    };
    let can_write2 = can_write.clone();
    let vroot = vault.clone();

    view! {
        <div class="tree" role="tree"
            class:drop=move || drop_target.get().as_deref() == Some("")
            on:dragover=move |ev| { ev.prevent_default(); }
            on:drop={
                let v = vroot.clone();
                move |ev: web_sys::DragEvent| {
                    ev.prevent_default();
                    drop_target.set(None);
                    if let Some(id) = ev.data_transfer().and_then(|d| d.get_data("text/x-cpn-entry").ok()).filter(|s| !s.is_empty())
                        && let Err(e) = actions::move_entry(&v, &id, "") { state::error(e); }
                }
            }>
            <For each=move || rows.get() key=|r| format!("{}|{}|{}|{}|{}", r.key, r.name, r.expanded, r.depth, r.shared) let:row>
                {
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
                    let icon = match row.kind {
                        EntryKind::Folder => "folder",
                        EntryKind::Note => "file",
                        EntryKind::Attachment => if crate::util::is_image(&row.name) { "image" } else { "paperclip" },
                    };
                    let indent = format!("padding-left:{}px", 8 + row.depth * 14);
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
                    let drag_id = id.clone();
                    let drop_path = path.clone();
                    let drop_vault = vault.clone();
                    let dp2 = path.clone();
                    let dp3 = path.clone();
                    view! {
                        <div class="tree-row" role="treeitem" class:active=active class:folder=is_folder
                            class:drop=move || is_folder && drop_target.get().as_deref() == Some(dp3.as_str())
                            style=indent draggable="true"
                            on:dragstart=move |ev: web_sys::DragEvent| {
                                if let (Some(dt), Some(id)) = (ev.data_transfer(), drag_id.clone()) {
                                    let _ = dt.set_data("text/x-cpn-entry", &id);
                                }
                            }
                            on:dragover=move |ev: web_sys::DragEvent| {
                                if is_folder { ev.prevent_default(); ev.stop_propagation(); drop_target.set(Some(dp2.clone())); }
                            }
                            on:dragleave=move |_| drop_target.set(None)
                            on:drop=move |ev: web_sys::DragEvent| {
                                if !is_folder { return; }
                                ev.prevent_default(); ev.stop_propagation();
                                drop_target.set(None);
                                if let Some(id) = ev.data_transfer().and_then(|d| d.get_data("text/x-cpn-entry").ok()).filter(|s| !s.is_empty()) {
                                    if let Err(e) = actions::move_entry(&drop_vault, &id, &drop_path) { state::error(e); }
                                    expanded.update(|s| { s.insert(drop_path.clone()); });
                                }
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
                                        <input class="input small" autofocus prop:value=move || rename_text.get()
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
                                        <Icon name=icon/>
                                        <span class="name">{name}</span>
                                        {row2.shared.then(|| view! { <span class="shared-mark" title="Shared individually with people outside the vault"><Icon name="users"/></span> })}
                                    </button>
                                }.into_any()
                            }}
                            <button class="tree-more" title="More" on:click={
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
                }
            </For>
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
            {(kind == EntryKind::Note).then(|| {
                let (v, id) = (v3.clone(), id1.clone().unwrap_or_default());
                let fav = actions::is_favorite(&v, &id);
                view! { <button on:click=move |_| { close(); actions::toggle_favorite(&v, &id); }>
                    <Icon name="star"/>{if fav { " Remove from favorites" } else { " Add to favorites" }}</button> }
            })}
            {(kind == EntryKind::Attachment).then(|| {
                let (v, id) = (v4.clone(), id2.clone().unwrap_or_default());
                view! { <button on:click=move |_| { close(); let (v, id) = (v.clone(), id.clone()); leptos::task::spawn_local(async move { actions::download_attachment(&v, &id).await }); }>
                    <Icon name="download"/>" Download"</button> }
            })}
            {writable.then(|| {
                let name = name.clone();
                let key = row_key.clone();
                let vault = v5.clone();
                let vault2 = v5.clone();
                let id_move = id3.clone();
                let id_del = id4.clone();
                let p_del = p3.clone();
                view! {
                    <button on:click=move |_| { close(); rename_text.set(name.clone()); renaming.set(Some(key.clone())); }>
                        <Icon name="edit"/>" Rename"</button>
                    {id_move.map(|id| view! {
                        <button on:click=move |_| { close(); ui.modal.set(Some(Modal::Move { vault: vault.clone(), id: id.clone() })); }>
                            <Icon name="move"/>" Move to…"</button>
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
