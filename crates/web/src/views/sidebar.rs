//! Left sidebar: vault switcher, quick actions, favorites, recent, file tree.

use coolpsonotes_core::protocol::Role;
use leptos::prelude::*;

use crate::actions;
use crate::state::{self, Modal, Route, ui};
use crate::util;
use crate::views::icons::Icon;
use crate::views::tree::Tree;
use crate::views::{SyncStatus, avatar};

fn role_badge(role: Role, shared: bool) -> AnyView {
    match (role, shared) {
        (Role::Owner, false) => view! { <span class="badge" title="Private: only you"><Icon name="lock"/></span> }.into_any(),
        (Role::Owner, true) => view! { <span class="badge" title="Shared by you"><Icon name="users"/></span> }.into_any(),
        (Role::Editor, _) => view! { <span class="badge" title="Shared with you — you can edit"><Icon name="users"/></span> }.into_any(),
        (Role::Viewer, _) => view! { <span class="badge" title="Shared with you — read only"><Icon name="eye"/></span> }.into_any(),
    }
}

#[component]
fn VaultSwitcher() -> impl IntoView {
    let ui = ui();
    let open = RwSignal::new(false);
    super::close_on_outside(open);
    let current = move || {
        let v = ui.route.get().vault().map(str::to_string);
        ui.vaults
            .get()
            .into_iter()
            .find(|x| Some(&x.id) == v.as_ref())
    };
    view! {
        <div class="vault-switch">
            <button class="vault-btn" on:click=move |_| open.update(|o| *o = !*o) aria-haspopup="true">
                <Icon name="vault"/>
                <span class="vname">{move || current().map(|v| v.name).unwrap_or_else(|| "All vaults".into())}</span>
                {move || current().map(|v| role_badge(v.role, v.shared))}
                <Icon name="chevron-down" class="caret"/>
            </button>
            <Show when=move || open.get()>
                <div class="menu vault-menu" on:click=move |_| open.set(false)>
                    <div class="menu-label">"Your vaults"</div>
                    {move || ui.vaults.get().into_iter().map(|v| {
                        let id = v.id.clone();
                        let err = v.error.clone();
                        view! {
                            <button class:archived=v.archived on:click=move |_| state::navigate(Route::Vault(id.clone()))>
                                <Icon name="vault"/>
                                <span class="grow">{v.name.clone()}</span>
                                {role_badge(v.role, v.shared)}
                                {err.map(|e| view! { <span class="small warn" title=e>"locked"</span> })}
                            </button>
                        }
                    }).collect_view()}
                    {move || {
                        let shared = ui.shared.get();
                        (!shared.is_empty()).then(|| view! {
                            <div class="menu-sep"></div>
                            <div class="menu-label">"Notes shared with me"</div>
                            {shared.into_iter().map(|s| {
                                let (v, n) = (s.vault.clone(), s.note.clone());
                                view! {
                                    <button title=format!("Shared by {}", s.owner) on:click=move |_| state::navigate(Route::Shared(v.clone(), n.clone()))>
                                        <Icon name="file"/><span class="grow">{s.title.clone()}</span>
                                    </button>
                                }
                            }).collect_view()}
                        })
                    }}
                    <div class="menu-sep"></div>
                    <button on:click=move |_| ui.modal.set(Some(Modal::NewVault))><Icon name="plus"/>" New vault…"</button>
                    <button on:click=move |_| state::navigate(Route::Home)><Icon name="book"/>" All vaults"</button>
                </div>
            </Show>
        </div>
    }
}

#[component]
pub fn Sidebar() -> impl IntoView {
    let ui = ui();
    let user_menu = RwSignal::new(false);
    super::close_on_outside(user_menu);
    let vault = move || ui.route.get().vault().map(str::to_string);
    let writable = move || {
        ui.vaults.get();
        vault().is_some_and(|v| state::can_write(&v))
    };
    let current_folder = move || {
        ui.route
            .get()
            .note()
            .and_then(|(v, n)| state::entry(v, n))
            .map(|e| coolpsonotes_core::paths::parent(&e.path).to_string())
            .unwrap_or_default()
    };
    view! {
        <aside class="sidebar" aria-label="Navigation">
            <div class="sb-top">
                <VaultSwitcher/>
                <button class="sb-search" on:click=move |_| ui.modal.set(Some(Modal::QuickSwitch))>
                    <Icon name="search"/><span>"Search"</span><kbd>{format!("{}K", util::mod_key())}</kbd>
                </button>
                <Show when=writable>
                    <div class="sb-actions">
                        <button class="btn primary small grow" on:click=move |_| if let Some(v) = vault() { actions::new_note(&v, &current_folder()); }>
                            <Icon name="plus"/>" New note"</button>
                        <button class="icon-btn" title="New folder" aria-label="New folder" on:click=move |_| if let Some(v) = vault() {
                            ui.modal.set(Some(Modal::NewFolder { vault: v, parent: String::new() }));
                        }><Icon name="folder-plus"/></button>
                        <button class="icon-btn" title="Upload files" aria-label="Upload files" on:click=move |_| if let Some(v) = vault() {
                            actions::pick_files(false, "", move |files| {
                                leptos::task::spawn_local(async move {
                                    let n = actions::upload_files(&v, files, "attachments").await.len();
                                    if n > 0 { state::success(format!("Uploaded {n} file(s).")); }
                                });
                            });
                        }><Icon name="upload"/></button>
                    </div>
                </Show>
            </div>
            <nav class="sb-scroll">
                <Favorites/>
                <Recent/>
                <SharedWithMe/>
                {move || match vault() {
                    Some(v) => {
                        let v2 = v.clone();
                        view! {
                            <div class="sb-section">
                                <div class="sb-label">
                                    <span>"Notes"</span>
                                    <button class="icon-btn tiny" title="Vault settings" aria-label="Vault settings" on:click=move |_| ui.modal.set(Some(Modal::VaultSettings(v2.clone())))><Icon name="settings"/></button>
                                </div>
                                <Tree vault=v/>
                            </div>
                        }.into_any()
                    }
                    None => view! {
                        <div class="sb-section">
                            <div class="sb-label"><span>"Vaults"</span></div>
                            {move || ui.vaults.get().into_iter().map(|v| {
                                let id = v.id.clone();
                                view! { <button class="sb-link" on:click=move |_| state::navigate(Route::Vault(id.clone()))>
                                    <Icon name="vault"/><span class="grow">{v.name.clone()}</span>{role_badge(v.role, v.shared)}</button> }
                            }).collect_view()}
                        </div>
                    }.into_any(),
                }}
            </nav>
            <div class="sb-bottom">
                {move || vault().map(|v| {
                    let v1 = v.clone();
                    let v2 = v.clone();
                    view! {
                        <button class="sb-link" on:click=move |_| state::navigate(Route::Trash(v1.clone()))><Icon name="trash"/>" Trash"</button>
                        <Show when=move || state::is_owner(&v2)>
                            {
                                let v3 = v.clone();
                                view! { <button class="sb-link" on:click=move |_| ui.modal.set(Some(Modal::Share(v3.clone())))><Icon name="share"/>" Share vault"</button> }
                            }
                        </Show>
                    }
                })}
                <div class="sb-user">
                    <button class="user-btn" on:click=move |_| user_menu.update(|o| *o = !*o)>
                        {move || ui.user.get().map(|u| avatar(&u.display_name, &u.id))}
                        <span class="grow ellipsis">{move || ui.user.get().map(|u| u.display_name).unwrap_or_default()}</span>
                    </button>
                    <SyncStatus/>
                    <Show when=move || user_menu.get()>
                        <div class="menu up" on:click=move |_| user_menu.set(false)>
                            <button on:click=move |_| state::navigate(Route::Settings("account".into()))><Icon name="settings"/>" Settings"</button>
                            <button on:click=move |_| state::navigate(Route::Settings("devices".into()))><Icon name="device"/>" Devices"</button>
                            <button on:click=move |_| ui.modal.set(Some(Modal::Help))><Icon name="keyboard"/>" Keyboard shortcuts"</button>
                            <button on:click=move |_| state::set_ui_prefs(|p| p.theme = match p.theme.as_str() { "dark" => "light".into(), "light" => "system".into(), _ => "dark".into() })>
                                <Icon name="moon"/>{move || format!(" Theme: {}", ui.ui_prefs.get().theme)}</button>
                            <div class="menu-sep"></div>
                            <button on:click=move |_| leptos::task::spawn_local(state::sign_out())><Icon name="logout"/>" Sign out"</button>
                        </div>
                    </Show>
                </div>
            </div>
        </aside>
    }
}

#[component]
fn Favorites() -> impl IntoView {
    let ui = ui();
    let items = move || {
        ui.prefs_rev.get();
        ui.tree_rev.get();
        let favs = state::with(|c| c.prefs.favorites.clone());
        favs.into_iter()
            .filter_map(|(v, n)| state::note_title(&v, &n).map(|t| (v, n, t)))
            .collect::<Vec<_>>()
    };
    view! {
        <Show when=move || !items().is_empty()>
            <div class="sb-section">
                <div class="sb-label"><span>"Favorites"</span></div>
                {move || items().into_iter().map(|(v, n, t)| {
                    let (v2, n2) = (v.clone(), n.clone());
                    let active = move || ui.route.get() == Route::Note(v2.clone(), n2.clone());
                    view! { <button class="sb-link" class:active=active on:click=move |_| state::navigate(Route::Note(v.clone(), n.clone()))>
                        <Icon name="star"/><span class="grow ellipsis">{t}</span></button> }
                }).collect_view()}
            </div>
        </Show>
    }
}

#[component]
fn Recent() -> impl IntoView {
    let ui = ui();
    let items = move || {
        ui.prefs_rev.get();
        ui.tree_rev.get();
        let cur = ui.route.get().vault().map(str::to_string);
        let recent = state::with(|c| c.prefs.recent.clone());
        recent
            .into_iter()
            .filter(|(v, _)| cur.as_ref().is_none_or(|c| c == v))
            .filter_map(|(v, n)| state::note_title(&v, &n).map(|t| (v, n, t)))
            .take(5)
            .collect::<Vec<_>>()
    };
    view! {
        <Show when=move || !items().is_empty()>
            <div class="sb-section">
                <div class="sb-label"><span>"Recent"</span></div>
                {move || items().into_iter().map(|(v, n, t)| {
                    let (v2, n2) = (v.clone(), n.clone());
                    let active = move || ui.route.get() == Route::Note(v2.clone(), n2.clone());
                    view! { <button class="sb-link" class:active=active on:click=move |_| state::navigate(Route::Note(v.clone(), n.clone()))>
                        <Icon name="clock"/><span class="grow ellipsis">{t}</span></button> }
                }).collect_view()}
            </div>
        </Show>
    }
}

/// Notes shared with us individually (without their vault).
#[component]
fn SharedWithMe() -> impl IntoView {
    let ui = ui();
    view! {
        <Show when=move || !ui.shared.get().is_empty()>
            <div class="sb-section">
                <div class="sb-label"><span>"Shared with me"</span></div>
                {move || ui.shared.get().into_iter().map(|s| {
                    let (v, n) = (s.vault.clone(), s.note.clone());
                    let (v2, n2) = (s.vault.clone(), s.note.clone());
                    let active = move || ui.route.get() == Route::Shared(v2.clone(), n2.clone());
                    let hint = format!("Shared by {}{}", s.owner, if s.role.can_write() { "" } else { " · read only" });
                    view! {
                        <button class="sb-link" class:active=active title=hint on:click=move |_| state::navigate(Route::Shared(v.clone(), n.clone()))>
                            <Icon name=if s.role.can_write() { "file" } else { "eye" }/><span class="grow ellipsis">{s.title.clone()}</span>
                        </button>
                    }
                }).collect_view()}
            </div>
        </Show>
    }
}
