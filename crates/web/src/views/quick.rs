//! Quick notes: the sidebar section, the overview page (`/quick`) with its
//! capture box, and the "move to a vault" dialog (for any note). The logic
//! is in `crate::quick`.

use leptos::html;
use leptos::prelude::*;

use crate::quick::{self, QuickNote};
use crate::state::{self, Modal, Route, step, ui};
use crate::util;
use crate::views::SyncStatus;
use crate::views::dialogs::Dialog;
use crate::views::focus_on_mount;
use crate::views::icons::Icon;

/// The quick notes (newest first) and how many there may be, recomputed when
/// notes, their text or the vaults change.
fn latest() -> (
    Option<String>,
    Vec<QuickNote>,
    notekpr_core::protocol::Limit,
) {
    let ui = ui();
    ui.tree_rev.track();
    ui.content_rev.track();
    ui.vaults.track();
    ui.pending.track();
    (quick::vault(), quick::notes(usize::MAX), quick::limit())
}

/// Sidebar entry: the Quick notes page, and "new quick note".
#[component]
pub fn QuickSection() -> impl IntoView {
    let ui = ui();
    view! {
        <Show when=move || ui.ui_prefs.get().quick_notes>
            <div class="sb-section sb-quick">
                <button class="sb-link" class:active=move || ui.route.get() == Route::Quick
                    title="All quick notes" on:click=move |_| state::navigate(Route::Quick)>
                    <Icon name="zap"/><span class="grow ellipsis">"Quick notes"</span></button>
                <button class="icon-btn tiny qn-add" title="New quick note (Alt+Q)" aria-label="New quick note"
                    on:click=move |_| quick::new_and_open()><Icon name="plus"/></button>
            </div>
        </Show>
    }
}

/// `/quick`: a capture box and all quick notes as cards, newest first.
#[component]
pub fn QuickPage() -> impl IntoView {
    let ui = ui();
    let data = Memo::new(move |_| latest());
    let full = Memo::new(move |_| {
        let (_, notes, limit) = data.get();
        !notekpr_core::quick::has_room(notes.len(), limit)
    });
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
    // How many there are, and may be: "4 of 20", or with no limit "4".
    let counter = move || {
        let (_, notes, limit) = data.get();
        match limit.max() {
            Some(max) => format!("{} of {max}", notes.len()),
            None => notes.len().to_string(),
        }
    };
    // Room for at most three more.
    let nearly_full = Memo::new(move |_| {
        let (_, notes, limit) = data.get();
        limit
            .max()
            .is_some_and(|max| max.saturating_sub(notes.len() as u64) <= 3)
    });
    view! {
        <div class="page quick-page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow ellipsis">"Quick notes"</h1>
                <span class="mobile-only"><SyncStatus/></span>
            </header>
            <div class="page-body">
                {move || (data.get().0.is_none() && ui.conn.get() != crate::ws::ConnState::Open).then(|| view! {
                    <p class="banner warn qn-novault"><Icon name="alert"/><span>{quick::NOT_SET_UP}</span></p>
                })}
                {move || full.get().then(|| view! {
                    <p class="banner warn qn-full"><Icon name="alert"/>
                        <span>{notekpr_core::quick::full_message(data.get().2)}</span></p>
                })}
                <form class="quick-capture" on:submit=move |ev| { ev.prevent_default(); save(); }>
                    <textarea class="input" rows="3" node_ref=area placeholder="What's on your mind?" aria-label="New quick note"
                        disabled=move || full.get()
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
                        <button class="btn primary" type="submit" disabled=move || busy.get() || full.get()>
                            <Icon name="check"/>" Save"</button>
                    </div>
                </form>
                <div class="qn-head">
                    <h2 class="grow">
                        <span class="qn-count" class:near=move || nearly_full.get() class:full=move || full.get()
                            title="How many quick notes there are, and may be">{counter}</span>
                        {move || {
                            let (_, notes, limit) = data.get();
                            if notes.len() == 1 && limit.max().is_none() { " quick note" } else { " quick notes" }
                        }}
                    </h2>
                </div>
                <p class="muted small qn-explain">"Quick notes are yours alone. Move one to a vault to keep it, share it or comment on it; deleting one deletes it for good."</p>
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
    let id3 = q.id.clone();
    view! {
        <div class="qn-card">
            <button class="qn-open" title=format!("Open {}", q.title) on:click=move |_| state::navigate(Route::Note(v.clone(), id.clone()))>
                <span class="qn-text">{q.preview.clone()}</span>
                {(!q.rest.is_empty()).then(|| view! { <span class="qn-rest">{q.rest.clone()}</span> })}
            </button>
            <div class="qn-foot">
                <span class="qn-meta">
                    <span class="qn-time" title=util::absolute_time(q.created)>{util::relative_time(q.created)}</span>
                    {q.waiting.then(|| view! {
                        <span class="pill qn-waiting" title="Made while the quick notes were full: on this device until there's room">"this device only"</span>
                    })}
                </span>
                <span class="qn-actions">
                    <button class="btn ghost tiny qn-move" title="Move to a vault as a regular note"
                        on:click=move |_| ui.modal.set(Some(Modal::MoveToVault { vault: v2.clone(), id: id2.clone() }))>
                        <Icon name="file-plus"/>" Move to vault…"</button>
                    <button class="btn ghost tiny danger qn-delete" title="Delete for good"
                        on:click=move |_| {
                            if util::confirm("Delete this quick note for good? Quick notes have no trash.") {
                                let id = id3.clone();
                                leptos::task::spawn_local(async move {
                                    if quick::delete(&id).await {
                                        state::success("Deleted.");
                                    }
                                });
                            }
                        }>
                        <Icon name="trash"/>" Delete"</button>
                </span>
            </div>
        </div>
    }
}

/// Move a note to another vault: which vault, which folder there, and its
/// name (a quick note: its first line). The original goes once the server
/// has the copy.
#[component]
pub fn MoveToVaultDialog(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let (text, title, quick_note) = step(|c| {
        (
            c.replicas
                .get(&vault)
                .map(|r| r.note_text(&id))
                .unwrap_or_default(),
            c.entry(&vault, &id)
                .map(|e| e.title().to_string())
                .unwrap_or_default(),
            c.is_quick_notes(&vault),
        )
    });
    let suggested = if quick_note {
        quick::suggested_title(&text)
    } else {
        String::new()
    };
    let name = RwSignal::new(if suggested.is_empty() {
        title
    } else {
        suggested
    });
    // Vaults one can write to, the one shown in the sidebar before first.
    let targets: Vec<(String, String)> = ui
        .vaults
        .get_untracked()
        .into_iter()
        .filter(|v| v.id != vault && v.role.can_write() && !v.archived && v.loaded)
        .map(|v| (v.id, v.name))
        .collect();
    let target = RwSignal::new(
        targets
            .first()
            .map(|(id, _)| id.clone())
            .unwrap_or_default(),
    );
    let folder = RwSignal::new(String::new());
    let folders = Memo::new(move |_| {
        let t = target.get();
        step(|c| {
            c.replicas
                .get(&t)
                .map(|r| r.index.folders())
                .unwrap_or_default()
        })
    });
    let busy = RwSignal::new(false);
    let no_target = targets.is_empty();
    let (from, note) = (StoredValue::new(vault), StoredValue::new(id));
    let submit = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let (from, note) = (from.get_value(), note.get_value());
        let (to, f, n) = (
            target.get_untracked(),
            folder.get_untracked(),
            name.get_untracked(),
        );
        leptos::task::spawn_local(async move {
            match crate::actions::move_to_vault(&from, &note, &to, &f, &n).await {
                Ok(new_id) => {
                    ui.modal.set(None);
                    let where_to = step(|c| c.vault_name(&to));
                    state::success(format!("Moved to {where_to}."));
                    // From the note itself: follow it.
                    if ui.route.get_untracked() == Route::Note(from, note) {
                        state::navigate(Route::Note(to, new_id));
                    }
                }
                Err(e) => state::error(e),
            }
            busy.set(false);
        });
    };
    view! {
        <Dialog title="Move to a vault">
            <form class="stack move-to-vault" on:submit=move |ev| { ev.prevent_default(); submit(); }>
                {no_target.then(|| view! { <p class="banner warn"><Icon name="alert"/><span>"There's no other vault you can write to. Create one first."</span></p> })}
                <label class="field">
                    <span class="small muted">"Vault"</span>
                    <select class="input mtv-vault" on:change=move |ev| { target.set(event_target_value(&ev)); folder.set(String::new()); }>
                        {targets.into_iter().map(|(id, n)| {
                            let sel = id == target.get_untracked();
                            view! { <option value=id selected=sel>{n}</option> }
                        }).collect_view()}
                    </select>
                </label>
                <label class="field">
                    <span class="small muted">"Folder (pick one or type a new one; empty: the vault's top level)"</span>
                    <input class="input mtv-folder" list="mtv-folders" placeholder="(top level)"
                        prop:value=move || folder.get() on:input=move |ev| folder.set(event_target_value(&ev))/>
                </label>
                <datalist id="mtv-folders">
                    {move || folders.get().into_iter().map(|f| view! { <option value=f.clone()></option> }).collect_view()}
                </datalist>
                <label class="field">
                    <span class="small muted">"Name"</span>
                    <input class="input mtv-name" use:focus_on_mount prop:value=move || name.get()
                        on:input=move |ev| name.set(event_target_value(&ev))/>
                </label>
                <p class="muted small">{if quick_note {
                    "It leaves your quick notes once the vault has it."
                } else {
                    "The original goes to this vault's trash once the other vault has the copy; its history, comments and shares stay with it."
                }}</p>
                <button class="btn primary" type="submit" disabled=move || busy.get() || no_target>"Move"</button>
            </form>
        </Dialog>
    }
}
