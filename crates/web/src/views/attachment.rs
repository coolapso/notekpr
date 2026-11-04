//! A file's page (an attachment): what it shows — a picture, a video, a
//! sound — or, for what can't be shown here, a card to download it; the
//! notes and drawings using it ("Used in"); and what a note's page offers
//! too: rename, move, download, the trash.

use leptos::html;
use leptos::prelude::*;
use notekpr_core::doc::Entry;
use notekpr_core::paths;
use wasm_bindgen::JsCast;

use super::SyncStatus;
use super::icons::Icon;
use crate::state::{self, Modal, Route, step, ui};
use crate::{actions, knowledge, util};

/// How a file shows on its page.
#[derive(Clone, Copy, PartialEq)]
enum Shows {
    Picture,
    Video,
    Audio,
    /// Not here: a card to download it (or open it in a tab: PDFs).
    Card,
}

fn shows(name: &str) -> Shows {
    match paths::media(name) {
        Some(paths::Media::Video) => Shows::Video,
        Some(paths::Media::Audio) => Shows::Audio,
        None if util::is_image(name) => Shows::Picture,
        None => Shows::Card,
    }
}

/// "12.3 KB · in notekpr/attachments".
fn about(e: &Entry) -> String {
    let folder = match paths::parent(&e.path) {
        "" => "at the top of the vault".to_string(),
        f => format!("in {f}"),
    };
    match e.size {
        Some(n) => format!("{} · {folder}", util::human_bytes(n)),
        None => folder,
    }
}

#[component]
pub fn AttachmentView(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let (v, i) = (vault.clone(), id.clone());
    let entry = Memo::new(move |_| {
        ui.tree_rev.get();
        step(|c| c.entry(&v, &i))
    });
    let v = vault.clone();
    let loaded = Memo::new(move |_| {
        ui.vaults.track();
        step(|c| c.loaded.contains(&v))
    });
    let v = vault.clone();
    let writable = Memo::new(move |_| {
        ui.vaults.get();
        step(|c| c.can_write(&v))
    });
    // The quick notes' files go with their notes: no folders, no trash.
    let quick = step(|c| c.is_quick_notes(&vault));

    // Its name, as typed: renamed on Enter or on leaving the field.
    let name_ref = NodeRef::<html::Input>::new();
    let name_text = RwSignal::new(String::new());
    Effect::new(move |_| {
        let Some(e) = entry.get() else {
            return;
        };
        let focused = name_ref.get().is_some_and(|t| {
            util::document()
                .active_element()
                .is_some_and(|a| a.is_same_node(Some(t.unchecked_ref())))
        });
        if !focused {
            name_text.set(paths::file_name(&e.path).to_string());
        }
    });
    let (vr, ir) = (vault.clone(), id.clone());
    let commit = StoredValue::new(move || {
        let Some(e) = entry.get_untracked() else {
            return;
        };
        let old = paths::file_name(&e.path).to_string();
        let new = name_text.get_untracked();
        if new.trim() == old {
            return;
        }
        if let Err(err) = actions::rename_entry(&vr, &ir, &new) {
            state::error(err);
            name_text.set(old);
        }
    });

    // The file, decrypted, for what shows here.
    let url = RwSignal::new(None::<String>);
    let (vl, il) = (vault.clone(), id.clone());
    Effect::new(move |_| {
        let Some(e) = entry.get() else {
            return;
        };
        if shows(&e.path) == Shows::Card || url.get_untracked().is_some() {
            return;
        }
        let (v, i) = (vl.clone(), il.clone());
        leptos::task::spawn_local(async move {
            if let Some(u) = actions::load_attachment(&v, &i).await {
                url.try_set(Some(u));
            }
        });
    });

    let (vu, iu) = (vault.clone(), id.clone());
    let used = Memo::new(move |_| {
        ui.tree_rev.get();
        ui.content_rev.get();
        knowledge::used_in(&vu, &iu)
    });

    let vb = vault.clone();
    let breadcrumb = move || {
        let path = entry.get().map(|e| e.path).unwrap_or_default();
        let mut crumbs = vec![step(|c| c.vault_name(&vb))];
        let folder = paths::parent(&path);
        if !folder.is_empty() {
            crumbs.extend(folder.split('/').map(str::to_string));
        }
        crumbs.join(" / ")
    };
    let download = {
        let (v, i) = (vault.clone(), id.clone());
        move || {
            let (v, i) = (v.clone(), i.clone());
            leptos::task::spawn_local(async move { actions::download_attachment(&v, &i).await });
        }
    };
    let download = StoredValue::new(download);
    let more = RwSignal::new(false);
    super::close_on_outside(more);
    let (vm, im) = (vault.clone(), id.clone());
    let (vo, io) = (vault.clone(), id.clone());
    let (vback, vrestore, irestore) = (vault.clone(), vault.clone(), id.clone());
    let vused = vault.clone();

    view! {
        <div class="note-page attachment-page">
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{breadcrumb}</div>
                <span class="mobile-only"><SyncStatus/></span>
                <button class="icon-btn" title="Download" aria-label="Download" on:click=move |_| download.with_value(|d| d())><Icon name="download"/></button>
                <div class="tb-drop">
                    <button class="icon-btn" title="More" aria-label="More" on:click=move |_| more.update(|o| *o = !*o)><Icon name="more"/></button>
                    <Show when=move || more.get()>
                        {
                            let (v, i) = (vm.clone(), im.clone());
                            let (v2, i2) = (vm.clone(), im.clone());
                            let (v3, i3) = (vm.clone(), im.clone());
                            let (v4, i4) = (vm.clone(), im.clone());
                            let inline = entry.get_untracked().is_some_and(|e| util::opens_inline(&e.path));
                            view! {
                                <div class="menu right" on:click=move |_| more.set(false)>
                                    <button on:click=move |_| download.with_value(|d| d())><Icon name="download"/>" Download"</button>
                                    {inline.then(|| view! {
                                        <button on:click=move |_| {
                                            let (v, i) = (v.clone(), i.clone());
                                            leptos::task::spawn_local(async move { actions::open_in_tab(&v, &i).await });
                                        }><Icon name="maximize"/>" Open in a new tab"</button>
                                    })}
                                    <button on:click=move |_| {
                                        let Some(e) = entry.get_untracked() else { return };
                                        let target = knowledge::link_target(&v2, &i2, "")
                                            .unwrap_or_else(|| paths::file_name(&e.path).to_string());
                                        let bang = if util::shows_in_note(&e.path) { "!" } else { "" };
                                        util::copy_to_clipboard(&format!("{bang}[[{target}]]"));
                                        state::success("Link copied — paste it into a note.");
                                    }><Icon name="link"/>" Copy link to file"</button>
                                    {move || (writable.get() && !quick).then(|| {
                                        let (v3, i3) = (v3.clone(), i3.clone());
                                        let (v4, i4) = (v4.clone(), i4.clone());
                                        view! {
                                            <button on:click=move |_| ui.modal.set(Some(Modal::Move { vault: v3.clone(), id: i3.clone() }))><Icon name="move"/>" Move to…"</button>
                                            <button class="danger" on:click=move |_| { actions::trash(&v4, &i4); state::navigate(Route::Vault(v4.clone())); }><Icon name="trash"/>" Move to trash"</button>
                                        }
                                    })}
                                </div>
                            }
                        }
                    </Show>
                </div>
            </header>
            <div class="note-body">
                <div class="note-main">
                    {move || {
                        if !loaded.get() {
                            return view! { <div class="center-fill"><div class="spinner"></div></div> }.into_any();
                        }
                        let Some(e) = entry.get() else {
                            let vback = vback.clone();
                            return view! {
                                <div class="empty">
                                    <p>"This file doesn't exist (anymore)."</p>
                                    <button class="btn ghost" on:click=move |_| state::navigate(Route::Vault(vback.clone()))>"Back to vault"</button>
                                </div>
                            }.into_any();
                        };
                        if !e.is_live() {
                            let (v, i) = (vrestore.clone(), irestore.clone());
                            return view! {
                                <div class="empty">
                                    <p>"This file is in the trash."</p>
                                    {move || writable.get().then(|| {
                                        let (v, i) = (v.clone(), i.clone());
                                        view! { <button class="btn" on:click=move |_| actions::restore(&v, &i)>"Restore it"</button> }
                                    })}
                                </div>
                            }.into_any();
                        }
                        let name = paths::file_name(&e.path).to_string();
                        let preview = match shows(&e.path) {
                            Shows::Picture => view! { <img class="att-view" alt=name.clone() src=move || url.get()/> }.into_any(),
                            Shows::Video => view! { <video class="att-view" controls=true src=move || url.get()></video> }.into_any(),
                            Shows::Audio => view! { <audio class="att-audio" controls=true src=move || url.get()></audio> }.into_any(),
                            Shows::Card => {
                                let inline = util::opens_inline(&e.path);
                                let (v, i) = (vo.clone(), io.clone());
                                view! {
                                    <div class="att-card">
                                        <Icon name="paperclip"/>
                                        <div class="grow">
                                            <b class="ellipsis">{name.clone()}</b>
                                            <span class="muted small">{if inline { "It opens in a new tab." } else { "It can't be shown here: download it to open it." }}</span>
                                        </div>
                                        {inline.then(|| view! {
                                            <button class="btn small" on:click=move |_| {
                                                let (v, i) = (v.clone(), i.clone());
                                                leptos::task::spawn_local(async move { actions::open_in_tab(&v, &i).await });
                                            }><Icon name="maximize"/>" Open"</button>
                                        })}
                                        <button class="btn small" on:click=move |_| download.with_value(|d| d())><Icon name="download"/>" Download"</button>
                                    </div>
                                }.into_any()
                            }
                        };
                        let vused = vused.clone();
                        view! {
                            <div class="note-title-row">
                                <input class="note-title" node_ref=name_ref aria-label="File name"
                                    readonly=move || !writable.get()
                                    prop:value=move || name_text.get()
                                    on:input=move |ev| name_text.set(event_target_value(&ev))
                                    on:blur=move |_| commit.with_value(|c| c())
                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                        if ev.key() == "Enter" {
                                            ev.prevent_default();
                                            commit.with_value(|c| c());
                                        }
                                    }/>
                            </div>
                            <p class="att-meta muted small">{about(&e)}</p>
                            <div class="att-preview">{preview}</div>
                            <section class="att-used">
                                <h2>"Used in"</h2>
                                {move || {
                                    let u = used.get();
                                    if u.is_empty() {
                                        return view! { <p class="muted small">"No note uses this file."</p> }.into_any();
                                    }
                                    let v = vused.clone();
                                    view! {
                                        <div class="list">
                                            {u.into_iter().map(|b| {
                                                let v = v.clone();
                                                let drawing = paths::is_drawing(&b.path);
                                                let title = if drawing { paths::drawing_title(&b.path) } else { paths::title(&b.path) }.to_string();
                                                let folder = paths::parent(&b.path).to_string();
                                                view! {
                                                    <button class="list-row used-in" on:click=move |_| state::navigate(Route::Note(v.clone(), b.id.clone()))>
                                                        <Icon name=if drawing { "drawing" } else { "file" }/>
                                                        <span class="grow">
                                                            <span class="ua-title ellipsis">{title}</span>
                                                            {(!b.snippet.is_empty()).then(|| view! { <span class="pi-snip">{b.snippet.clone()}</span> })}
                                                        </span>
                                                        {(!folder.is_empty()).then(|| view! { <span class="muted small ellipsis">{folder}</span> })}
                                                    </button>
                                                }
                                            }).collect_view()}
                                        </div>
                                    }.into_any()
                                }}
                            </section>
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
