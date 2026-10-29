//! Home (all vaults), a vault's overview page, and the trash.

use leptos::prelude::*;
use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::protocol::{PurgedItem, Role};

use crate::actions;
use crate::api;
use crate::state::{self, Modal, Route, step, ui};
use crate::util;
use crate::views::icons::Icon;
use crate::{knowledge, views::SyncStatus};

#[component]
fn PageHead(title: String) -> impl IntoView {
    let ui = ui();
    view! {
        <header class="page-head">
            <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
            <h1 class="grow ellipsis">{title}</h1>
            <span class="mobile-only"><SyncStatus/></span>
        </header>
    }
}

#[component]
pub fn Home() -> impl IntoView {
    let ui = ui();
    let name = ui
        .user
        .get_untracked()
        .map(|u| u.display_name)
        .unwrap_or_default();
    view! {
        <div class="page">
            <PageHead title=format!("Hi {name}")/>
            <div class="page-body">
                <div class="cards">
                    {move || ui.vaults.get().into_iter().map(|v| {
                        let id = v.id.clone();
                        let (label, icon) = match (&v.role, v.shared) {
                            (Role::Owner, false) => ("Private", "lock"),
                            (Role::Owner, true) => ("Shared by you", "users"),
                            (Role::Admin, _) => ("Shared with you · admin", "users"),
                            (Role::Editor, _) => ("Shared with you", "users"),
                            (Role::Viewer, _) => ("Shared with you · read only", "eye"),
                            (Role::Other(_), _) => ("Shared with you · read only until you reload", "eye"),
                        };
                        let count = {
                            let id = id.clone();
                            move || { ui.tree_rev.get(); state::step(|c| c.notes_of(&id)).len() }
                        };
                        view! {
                            <button class="card" class:archived=v.archived on:click=move |_| state::navigate(Route::Vault(id.clone()))>
                                <div class="card-icon">{super::vault_icon(Some(&v))}</div>
                                <div class="card-title">{v.name.clone()}</div>
                                <div class="card-meta"><Icon name=icon/>" "{label}</div>
                                <div class="card-meta muted">{move || if v.loaded { util::count(count(), "note", "notes") } else { v.error.clone().unwrap_or_else(|| "Loading…".into()) }}</div>
                            </button>
                        }
                    }).collect_view()}
                    <button class="card add" on:click=move |_| ui.modal.set(Some(Modal::NewVault))>
                        <div class="card-icon"><Icon name="plus"/></div>
                        <div class="card-title">"New vault"</div>
                        <div class="card-meta muted">"A separate space with its own sharing and keys"</div>
                    </button>
                </div>
                <Show when=move || !ui.shared.get().is_empty()>
                    <h2>"Shared with me"</h2>
                    <div class="cards">
                        {move || ui.shared.get().into_iter().map(|s| {
                            let (v, n) = (s.vault.clone(), s.note.clone());
                            view! {
                                <button class="card" on:click=move |_| state::navigate(Route::Shared(v.clone(), n.clone()))>
                                    <div class="card-icon">{super::entry_icon(s.icon.clone(), "file")}</div>
                                    <div class="card-title">{s.title.clone()}</div>
                                    <div class="card-meta"><Icon name="users"/>" Shared by "{s.owner.clone()}</div>
                                    <div class="card-meta muted">{if s.role.can_write() { "Can edit" } else { "Read only" }}</div>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                </Show>
            </div>
        </div>
    }
}

#[component]
pub fn VaultHome(vault: String) -> impl IntoView {
    let ui = ui();
    let vid = StoredValue::new(vault.clone());
    let title = Memo::new(move |_| {
        ui.vaults.get();
        state::step(|c| c.vault_name(&vid.get_value()))
    });
    let recent = Memo::new(move |_| {
        ui.tree_rev.get();
        let mut e: Vec<_> = state::step(|c| c.live_entries(&vid.get_value()))
            .into_iter()
            .filter(|e| e.kind == EntryKind::Note)
            .collect();
        e.sort_by_key(|x| std::cmp::Reverse(x.modified));
        e.truncate(12);
        e
    });
    let tags = Memo::new(move |_| {
        ui.content_rev.get();
        knowledge::tags(&vid.get_value())
            .into_iter()
            .take(30)
            .collect::<Vec<_>>()
    });
    let loaded = Memo::new(move |_| {
        ui.vaults
            .get()
            .iter()
            .any(|x| x.id == vid.get_value() && x.loaded)
    });
    let writable = Memo::new(move |_| {
        ui.vaults.get();
        state::step(|c| c.can_write(&vid.get_value()))
    });
    view! {
        <div class="page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow ellipsis">{move || title.get()}</h1>
                <button class="btn ghost small" on:click=move |_| ui.modal.set(Some(Modal::VaultSettings(vid.get_value())))><Icon name="settings"/>" Vault settings"</button>
            </header>
            <div class="page-body">
                {move || {
                    if !loaded.get() {
                        return view! { <div class="center-fill"><div class="spinner"></div><p class="muted">"Decrypting your notes…"</p></div> }.into_any();
                    }
                    view! {
                        {move || writable.get().then(|| view! {
                            <div class="quick-actions">
                                <button class="qa" on:click=move |_| actions::new_note(&vid.get_value(), "")><Icon name="file-plus"/><span>"New note"</span></button>
                                <button class="qa" on:click=move |_| actions::new_drawing(&vid.get_value(), "")><Icon name="drawing"/><span>"New drawing"</span></button>
                                <button class="qa" on:click=move |_| state::navigate(Route::Drawings(vid.get_value()))><Icon name="image"/><span>"Drawings"</span></button>
                                <button class="qa" on:click=move |_| ui.modal.set(Some(Modal::Import(vid.get_value())))><Icon name="upload"/><span>"Import notes"</span></button>
                                <button class="qa" on:click=move |_| ui.modal.set(Some(Modal::QuickSwitch))><Icon name="search"/><span>"Find a note"</span></button>
                                <button class="qa" on:click=move |_| state::navigate(Route::Graph(vid.get_value()))><Icon name="graph"/><span>"Graph"</span></button>
                            </div>
                        })}
                        <h2>"Recently edited"</h2>
                        {move || {
                            let items = recent.get();
                            if items.is_empty() {
                                return view! { <p class="muted">"No notes yet. Create your first one!"</p> }.into_any();
                            }
                            let v = vid.get_value();
                            view! {
                                <div class="note-list">
                                    {items.into_iter().map(|e| {
                                        let text = step(|c| c.replicas.get(&v).map(|r| r.note_text(&e.id))).unwrap_or_default();
                                        note_card(v.clone(), e, &text, None)
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }}
                        {move || (!tags.get().is_empty()).then(|| view! {
                            <h2>"Tags"</h2>
                            <div class="tags">
                                {tags.get().into_iter().map(|(t, n)| {
                                    let q = format!("tag:{t}");
                                    view! { <button class="chip" on:click=move |_| ui.modal.set(Some(Modal::Search(q.clone())))>{format!("#{t}")}<span class="count">{n}</span></button> }
                                }).collect_view()}
                            </div>
                        })}
                    }.into_any()
                }}
            </div>
        </div>
    }
}

/// A note as a card (a vault's recently edited notes, the quick notes): its
/// icon and title, the start of its `text`, where it is and when it
/// changed, then `badge` (a pill: its text and what it means), if any.
pub fn note_card(
    vault: String,
    e: Entry,
    text: &str,
    badge: Option<(&'static str, &'static str)>,
) -> impl IntoView + use<> {
    let id = e.id.clone();
    let folder = notekpr_core::paths::parent(&e.path).to_string();
    // As shown: without HTML tags (colors).
    let text = notekpr_core::markdown::shown_text(text);
    let snippet: String = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#') && !l.starts_with("---"))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(140)
        .collect();
    view! {
        <button class="note-card" on:click=move |_| state::navigate(Route::Note(vault.clone(), id.clone()))>
            <div class="nc-title">{e.icon.clone().map(|i| view! { <span class="entry-icon" aria-hidden="true">{i}</span> })}{e.title().to_string()}</div>
            <div class="nc-snip">{snippet}</div>
            <div class="nc-meta">
                {if folder.is_empty() { String::new() } else { format!("{folder} · ") }}
                <span title=util::absolute_time(e.modified)>{util::relative_time(e.modified)}</span>
                {badge.map(|(t, why)| view! { <span class="pill" title=why>{t}</span> })}
            </div>
        </button>
    }
}

#[component]
pub fn Trash(vault: String) -> impl IntoView {
    let ui = ui();
    let v = vault.clone();
    let entries = move || {
        ui.tree_rev.get();
        step(|c| {
            c.replicas
                .get(&v)
                .map(|r| r.index.entries())
                .unwrap_or_default()
        })
    };
    let all = entries.clone();
    let items = move || {
        let mut e: Vec<_> = all().into_iter().filter(|e| e.in_trash()).collect();
        e.sort_by_key(|x| std::cmp::Reverse(x.trashed));
        e
    };
    let v = vault.clone();
    let writable = move || {
        ui.vaults.get();
        state::step(|c| c.can_write(&v))
    };
    // Deleting for good is the owner's and admins' (editors only restore).
    let v = vault.clone();
    let admin = move || {
        ui.vaults.get();
        state::step(|c| c.can_administer(&v))
    };
    // What was deleted for good and can still be undone: the server's list,
    // named by the entries it left in the index (owner and admins).
    let kept = RwSignal::new(Vec::<PurgedItem>::new());
    let (vk, ak) = (vault.clone(), admin.clone());
    Effect::new(move |_| {
        ui.tree_rev.get();
        if !ak() {
            kept.set(vec![]);
            return;
        }
        let v = vk.clone();
        leptos::task::spawn_local(async move {
            if let Ok(l) = api::get::<Vec<PurgedItem>>(&format!("/vaults/{v}/purged")).await {
                kept.set(l);
            }
        });
    });
    let undoable = move || {
        let list = kept.get();
        let mut out: Vec<(Entry, i64)> = entries()
            .into_iter()
            .filter(|e| e.purged.is_some())
            .filter_map(|e| {
                let until = list.iter().find(|k| k.id == e.id)?.kept_until;
                Some((e, until))
            })
            .collect();
        out.sort_by_key(|(e, _)| std::cmp::Reverse(e.purged));
        out
    };
    let period = actions::purge_undo_period();
    let items2 = items.clone();
    let ve = vault.clone();
    let vi = vault.clone();
    let vu = vault.clone();
    let (w2, a2, a3) = (writable.clone(), admin.clone(), admin.clone());
    let (p1, p2) = (period.clone(), period.clone());
    view! {
        <div class="page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow">"Trash"</h1>
                <Show when=move || admin() && !items2().is_empty()>
                    {
                        let ve = ve.clone();
                        let p1 = p1.clone();
                        view! {
                            <button class="btn danger small" on:click=move |_| {
                                let ask = match &p1 {
                                    Some(p) => format!("Delete everything in the trash for good? You can still undo it here for {p}."),
                                    None => "Permanently delete everything in the trash? This cannot be undone.".into(),
                                };
                                if util::confirm(&ask) {
                                    let v = ve.clone();
                                    leptos::task::spawn_local(async move { actions::empty_trash(&v).await });
                                }
                            }><Icon name="trash"/>" Empty trash"</button>
                        }
                    }
                </Show>
            </header>
            <div class="page-body">
                <p class="muted small">"Deleted notes stay here until the trash is emptied. Older versions of notes are also kept in each note's history."</p>
                {move || (w2() && !a2()).then(|| view! {
                    <p class="muted small trash-admins">"Only the vault's owner and its admins can delete things for good."</p>
                })}
                {move || {
                    let list = items();
                    if list.is_empty() {
                        return view! { <p class="muted">"The trash is empty."</p> }.into_any();
                    }
                    let writable = writable();
                    let admin = a3();
                    view! {
                        <div class="list">
                            {list.into_iter().map(|e| {
                                let (v1, id1) = (vi.clone(), e.id.clone());
                                let (v2, id2) = (vi.clone(), e.id.clone());
                                let p2 = p2.clone();
                                view! {
                                    <div class="list-row">
                                        <Icon name=if e.kind == EntryKind::Folder { "folder" } else { "file" }/>
                                        <div class="grow">
                                            <div>{e.path.clone()}</div>
                                            <div class="muted small">{format!("deleted {}", util::relative_time(e.trashed.unwrap_or(0)))}</div>
                                        </div>
                                        {writable.then(|| view! {
                                            <button class="btn small" on:click=move |_| actions::restore(&v1, &id1)>"Restore"</button>
                                        })}
                                        {admin.then(|| view! {
                                            <button class="btn small danger" on:click=move |_| {
                                                let ask = match &p2 {
                                                    Some(p) => format!("Delete this for good? You can still undo it here for {p}."),
                                                    None => "Delete this permanently? This cannot be undone.".into(),
                                                };
                                                if util::confirm(&ask) {
                                                    let (v, id) = (v2.clone(), id2.clone());
                                                    leptos::task::spawn_local(async move { actions::purge(&v, &id).await; });
                                                }
                                            }>"Delete forever"</button>
                                        })}
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    }.into_any()
                }}
                {move || {
                    let list = undoable();
                    (!list.is_empty()).then(|| view! {
                        <h2 class="trash-gone">"Deleted for good"</h2>
                        <p class="muted small">"Kept a while longer: until then, undoing it puts it back in the trash."</p>
                        <div class="list trash-gone-list">
                            {list.into_iter().map(|(e, until)| {
                                let (v, id) = (vu.clone(), e.id.clone());
                                view! {
                                    <div class="list-row">
                                        <Icon name=if e.kind == EntryKind::Folder { "folder" } else { "file" }/>
                                        <div class="grow">
                                            <div>{e.path.clone()}</div>
                                            <div class="muted small">
                                                {format!("deleted for good {} · ", util::relative_time(e.purged.unwrap_or(0)))}
                                                <span title=util::absolute_time(until)>{format!("kept until {}", util::date(until))}</span>
                                            </div>
                                        </div>
                                        <button class="btn small" on:click=move |_| {
                                            let (v, id) = (v.clone(), id.clone());
                                            leptos::task::spawn_local(async move { actions::undo_purge(&v, &id).await; });
                                        }>"Undo"</button>
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    })
                }}
            </div>
        </div>
    }
}
