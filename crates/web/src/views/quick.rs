//! Quick notes: the sidebar section, the overview page (`/quick`) with its
//! capture box, and the "promote" dialog. The logic is in `crate::quick`.

use leptos::html;
use leptos::prelude::*;

use crate::quick::{self, QuickNote};
use crate::state::{self, Modal, Route, step, ui};
use crate::util;
use crate::views::SyncStatus;
use crate::views::dialogs::Dialog;
use crate::views::focus_on_mount;
use crate::views::icons::Icon;

/// The quick-notes vault and its newest quick notes, recomputed when notes,
/// their text, the preferences or the vaults change.
fn latest(archived: bool, limit: usize) -> (Option<String>, Vec<QuickNote>) {
    let ui = ui();
    ui.tree_rev.track();
    ui.content_rev.track();
    ui.prefs_rev.track();
    ui.vaults.track();
    match quick::vault() {
        Some(v) => {
            let list = quick::notes(&v, archived, limit);
            (Some(v), list)
        }
        None => (None, vec![]),
    }
}

/// Sidebar section: "new quick note" and the latest five.
#[component]
pub fn QuickSection() -> impl IntoView {
    let ui = ui();
    let data = Memo::new(move |_| latest(false, 5));
    view! {
        <Show when=move || ui.ui_prefs.get().quick_notes>
            <div class="sb-section sb-quick">
                <div class="sb-label">
                    <button class="sb-label-link" class:active=move || ui.route.get() == Route::Quick
                        title="All quick notes" on:click=move |_| state::navigate(Route::Quick)>"Quick notes"</button>
                    {move || data.get().0.is_some().then(|| view! {
                        <button class="icon-btn tiny qn-add" title=format!("New quick note ({}+Alt+Q)", util::mod_key()) aria-label="New quick note"
                            on:click=move |_| quick::new_and_open()><Icon name="plus"/></button>
                    })}
                </div>
                {move || {
                    let (v, items) = data.get();
                    if v.is_none() {
                        return view! { <p class="sb-hint muted small">"Quick notes need a vault you can edit."</p> }.into_any();
                    }
                    if items.is_empty() {
                        return view! {
                            <button class="sb-link" on:click=move |_| quick::new_and_open()>
                                <Icon name="plus"/><span class="grow ellipsis">"New quick note"</span></button>
                        }.into_any();
                    }
                    items.into_iter().map(|q| {
                        let (v, id) = (q.vault.clone(), q.id.clone());
                        let (v2, id2) = (q.vault.clone(), q.id.clone());
                        let active = move || ui.route.get() == Route::Note(v2.clone(), id2.clone());
                        view! {
                            <button class="sb-link" class:active=active title=q.title.clone()
                                on:click=move |_| state::navigate(Route::Note(v.clone(), id.clone()))>
                                <Icon name="zap"/><span class="grow ellipsis">{q.preview.clone()}</span></button>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </Show>
    }
}

/// `/quick`: a capture box and all quick notes as cards, newest first.
#[component]
pub fn QuickPage() -> impl IntoView {
    let ui = ui();
    let show_archived = RwSignal::new(false);
    let data = Memo::new(move |_| latest(show_archived.get(), usize::MAX));
    let vault_name = Memo::new(move |_| data.get().0.map(|v| state::vault_name(&v)));
    let text = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let area = NodeRef::<html::Textarea>::new();
    let save = move || {
        let t = text.get_untracked();
        if t.trim().is_empty() || busy.get_untracked() {
            if let Some(a) = area.get_untracked() {
                let _ = a.focus();
            }
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            if quick::create(&t).await.is_some() {
                // Only what was saved: anything typed meanwhile stays.
                text.update(|c| {
                    if *c == t {
                        c.clear();
                    }
                });
                if let Some(a) = area.get_untracked() {
                    let _ = a.focus();
                }
            }
            busy.set(false);
        });
    };
    // Ready to type (on phones only when tapped: no keyboard popping up).
    Effect::new(move |_| {
        if let Some(a) = area.get()
            && !util::is_touch()
        {
            util::set_timeout(0, move || {
                let _ = a.focus();
            });
        }
    });
    let hint = format!("{}+Enter to save", util::mod_key());
    view! {
        <div class="page quick-page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow ellipsis">"Quick notes"</h1>
                {move || vault_name.get().map(|n| view! { <span class="muted small qn-vault desktop-only" title="Change this in Settings → Editor">{format!("in {n}")}</span> })}
                <span class="mobile-only"><SyncStatus/></span>
            </header>
            <div class="page-body">
                {move || data.get().0.is_none().then(|| view! {
                    <p class="banner warn qn-novault"><Icon name="alert"/><span>{quick::NEEDS_VAULT}</span></p>
                })}
                <form class="quick-capture" on:submit=move |ev| { ev.prevent_default(); save(); }>
                    <textarea class="input" rows="3" node_ref=area placeholder="What's on your mind?" aria-label="New quick note"
                        disabled=move || data.get().0.is_none()
                        prop:value=move || text.get()
                        on:input=move |ev| text.set(event_target_value(&ev))
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) && !ev.is_composing() {
                                ev.prevent_default();
                                save();
                            }
                        }></textarea>
                    <div class="qc-bar">
                        <span class="muted small grow desktop-only">{hint}</span>
                        <button class="btn primary" type="submit" disabled=move || busy.get() || data.get().0.is_none()>
                            <Icon name="check"/>" Save"</button>
                    </div>
                </form>
                <div class="qn-head">
                    <h2 class="grow">{move || {
                        let n = data.get().1.iter().filter(|q| !q.archived).count();
                        match n { 0 => "No quick notes yet".to_string(), 1 => "1 quick note".to_string(), n => format!("{n} quick notes") }
                    }}</h2>
                    <label class="check small">
                        <input type="checkbox" class="qn-archived-toggle" prop:checked=move || show_archived.get()
                            on:change=move |ev| show_archived.set(event_target_checked(&ev))/>
                        " Show archived"
                    </label>
                </div>
                <div class="qn-cards">
                    {move || data.get().1.into_iter().map(|q| view! { <QuickCard q=q/> }).collect_view()}
                </div>
            </div>
        </div>
    }
}

#[component]
fn QuickCard(q: QuickNote) -> impl IntoView {
    let ui = ui();
    let (v, id) = (q.vault.clone(), q.id.clone());
    let (v2, id2) = (q.vault.clone(), q.id.clone());
    let (v3, id3) = (q.vault.clone(), q.id.clone());
    let archived = q.archived;
    let writable = state::can_write(&q.vault);
    view! {
        <div class="qn-card" class:archived=archived>
            <button class="qn-open" title=format!("Open {}", q.title) on:click=move |_| state::navigate(Route::Note(v.clone(), id.clone()))>
                <span class="qn-text">{q.preview.clone()}</span>
                {(!q.rest.is_empty()).then(|| view! { <span class="qn-rest">{q.rest.clone()}</span> })}
            </button>
            <div class="qn-foot">
                <span class="qn-time grow" title=util::absolute_time(q.created)>{util::relative_time(q.created)}</span>
                {archived.then(|| view! { <span class="pill">"archived"</span> })}
                {writable.then(move || view! {
                    <button class="btn ghost tiny qn-promote" title="Move into a folder as a regular note"
                        on:click=move |_| ui.modal.set(Some(Modal::Promote { vault: v2.clone(), id: id2.clone() }))>
                        <Icon name="file-plus"/>" Promote"</button>
                    {if archived {
                        view! { <button class="btn ghost tiny qn-unarchive" on:click=move |_| quick::unarchive(&v3, &id3)>
                            <Icon name="archive"/>" Unarchive"</button> }.into_any()
                    } else {
                        view! { <button class="btn ghost tiny qn-archive" on:click=move |_| quick::archive(&v3, &id3)>
                            <Icon name="archive"/>" Archive"</button> }.into_any()
                    }}
                })}
            </div>
        </div>
    }
}

/// Promote a quick note: a name (default: its first line) and a folder.
#[component]
pub fn PromoteDialog(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let text = step(|c| c.replicas.get(&vault).map(|r| r.note_text(&id))).unwrap_or_default();
    let suggested = quick::promote_title(&text);
    let name = RwSignal::new(if suggested.is_empty() {
        state::entry(&vault, &id)
            .map(|e| e.title().to_string())
            .unwrap_or_default()
    } else {
        suggested
    });
    let folder = RwSignal::new(String::new());
    let folders: Vec<String> = step(|c| c.replicas.get(&vault).map(|r| r.index.folders()))
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f != quick::FOLDER && !coolpsonotes_core::paths::is_within(f, quick::FOLDER))
        .collect();
    view! {
        <Dialog title="Promote to a note">
            <form class="stack promote" on:submit=move |ev| {
                ev.prevent_default();
                match quick::promote(&vault, &id, &name.get_untracked(), &folder.get_untracked()) {
                    Ok(()) => { ui.modal.set(None); state::success("Promoted to a regular note."); }
                    Err(e) => state::error(e),
                }
            }>
                <label class="field">
                    <span class="small muted">"Name"</span>
                    <input class="input promote-name" use:focus_on_mount prop:value=move || name.get()
                        on:input=move |ev| name.set(event_target_value(&ev))/>
                </label>
                <label class="field">
                    <span class="small muted">"Folder (pick one or type a new one; empty: the vault root)"</span>
                    <input class="input promote-folder" list="promote-folders" placeholder="(vault root)"
                        prop:value=move || folder.get() on:input=move |ev| folder.set(event_target_value(&ev))/>
                </label>
                <datalist id="promote-folders">
                    {folders.into_iter().map(|f| view! { <option value=f.clone()></option> }).collect_view()}
                </datalist>
                <button class="btn primary" type="submit">"Promote"</button>
            </form>
        </Dialog>
    }
}
