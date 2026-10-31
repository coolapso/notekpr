//! Settings: account, devices, sessions, recovery, agents, editor, admin.
//! Also the page used to approve a desktop/CLI sign-in code.

use leptos::prelude::Update;
use leptos::prelude::*;
use notekpr_core::protocol::*;

use crate::state::{self, Modal, Route, step, ui};
use crate::util;
use crate::views::icons::Icon;
use crate::views::width::WidthSlider;
use crate::{actions, api, views::avatar};

#[component]
pub fn Settings(tab: String) -> impl IntoView {
    let ui = ui();
    let is_admin = ui.user.get_untracked().is_some_and(|u| u.is_admin);
    let tabs: Vec<(&str, &str, &str)> = {
        let mut t = vec![
            ("account", "Account", "users"),
            ("devices", "Devices", "device"),
            ("sessions", "Sessions", "key"),
            ("security", "Recovery", "shield"),
            ("agents", "AI agents", "bot"),
            ("editor", "Editor", "edit"),
        ];
        if is_admin {
            t.push(("admin", "Administration", "settings"));
        }
        t
    };
    let current = tab.clone();
    view! {
        <div class="page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow">"Settings"</h1>
            </header>
            <div class="settings">
                <nav class="settings-nav">
                    {tabs.into_iter().map(|(id, label, icon)| {
                        let active = current == id;
                        view! { <button class:active=active on:click=move |_| state::navigate(Route::Settings(id.to_string()))><Icon name=icon/>" "{label}</button> }
                    }).collect_view()}
                </nav>
                <section class="settings-body">
                    {match tab.as_str() {
                        "devices" => view! { <Devices/> }.into_any(),
                        "sessions" => view! { <Sessions/> }.into_any(),
                        "security" => view! { <Security/> }.into_any(),
                        "agents" => view! { <Agents/> }.into_any(),
                        "editor" => view! { <EditorPrefs/> }.into_any(),
                        "admin" => view! { <Admin/> }.into_any(),
                        _ => view! { <Account/> }.into_any(),
                    }}
                </section>
            </div>
        </div>
    }
}

#[component]
fn Account() -> impl IntoView {
    let ui = ui();
    let u = ui.user.get_untracked();
    // What an export leaves out (everything goes, unless unticked).
    let skipped = RwSignal::new(std::collections::BTreeSet::<String>::new());
    let quick_notes = RwSignal::new(true);
    let has_quick = move || {
        ui.vaults.track();
        step(|c| c.quick_notes().is_some())
    };
    let busy = RwSignal::new(false);
    let chosen = move || {
        let s = skipped.get();
        ui.vaults
            .get()
            .into_iter()
            .filter(|v| v.loaded && !s.contains(&v.id))
            .map(|v| v.id)
            .collect::<Vec<_>>()
    };
    let export = move |_| {
        let (vaults, quick) = (chosen(), quick_notes.get_untracked() && has_quick());
        busy.set(true);
        leptos::task::spawn_local(async move {
            actions::export_account(vaults, quick).await;
            busy.set(false);
        });
    };
    let import = move |_| {
        actions::pick_files(false, ".zip", move |files| {
            busy.set(true);
            leptos::task::spawn_local(async move {
                let items = actions::files_to_import(files).await;
                actions::import_account(items).await;
                busy.set(false);
            });
        });
    };
    let fp = step(|c| c.account.as_ref().map(|a| a.public().fingerprint())).unwrap_or_default();
    // Asked afresh: the server may have been updated since this page loaded.
    let server_version = RwSignal::new(None::<String>);
    leptos::task::spawn_local(async move {
        if let Ok(i) = api::get::<ServerInfo>("/info").await {
            server_version.set(Some(i.version));
        }
    });
    let versions = move || {
        let app = notekpr_core::VERSION;
        match server_version.get() {
            None => format!("This app: {app}"),
            Some(s) if s == app => format!("This app: {app} · the server: {s}"),
            Some(s) => format!(
                "This app: {app} · the server: {s} — reload the page to use the server's version"
            ),
        }
    };
    view! {
        {u.map(|u| view! {
            <div class="person big">
                {avatar(&u.display_name, &u.id)}
                <div>
                    <div class="h">{u.display_name.clone()}</div>
                    <div class="muted">{u.email.clone().unwrap_or_default()}</div>
                    <div class="muted small">{format!("Member since {}", util::absolute_time(u.created_at))}</div>
                </div>
            </div>
            <p class="muted small">"Your name and e-mail come from your sign-in provider."</p>
        })}
        <h3>"Your key fingerprint"</h3>
        <p class="fp big">{fp}</p>
        <p class="muted small">"When someone shares a vault with you, they can compare this fingerprint with what their screen shows to make sure nobody is in the middle."</p>
        <h3>"Data"</h3>
        <p class="muted small">"Export your notes as plain Markdown files in one .zip: the vaults you choose (their drawings too) and your Personal storage — your quick notes and saved shapes —, with their folders and attachments. Your data is never locked into a proprietary format."</p>
        <div class="export-choice">
            {move || has_quick().then(|| view! {
                <label class="check">
                    <input type="checkbox" class="export-quick" prop:checked=move || quick_notes.get()
                        on:change=move |ev| quick_notes.set(event_target_checked(&ev))/>
                    <Icon name="zap"/>
                    <span class="grow ellipsis">"Personal storage (quick notes, saved shapes)"</span>
                </label>
            })}
            {move || ui.vaults.get().into_iter().map(|v| {
                let id = v.id.clone();
                let id2 = v.id.clone();
                let loaded = v.loaded;
                view! {
                    <label class="check" title=v.error.clone()>
                        <input type="checkbox" class="export-vault" disabled=!loaded
                            prop:checked=move || loaded && !skipped.with(|s| s.contains(&id))
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                skipped.update(|s| { if on { s.remove(&id2); } else { s.insert(id2.clone()); } });
                            }/>
                        {super::vault_icon(Some(&v))}
                        <span class="grow ellipsis">{v.name.clone()}</span>
                        {(v.role != Role::Owner).then(|| view! { <span class="pill" title="Shared with you: you get a copy of your own">"shared"</span> })}
                    </label>
                }
            }).collect_view()}
        </div>
        <div class="row">
            <button class="btn primary export-account"
                disabled=move || busy.get() || (chosen().is_empty() && !(quick_notes.get() && has_quick()))
                on:click=export><Icon name="download"/>" Export"</button>
            <button class="btn import-account" disabled=move || busy.get() on:click=import><Icon name="upload"/>" Import an export…"</button>
            <Show when=move || busy.get()><span class="spinner inline"></span></Show>
        </div>
        <p class="muted small">"Moving to another notekpr server? Export here, sign up there and import the .zip under Settings → Account. The vaults come back named as they were, with their folders, files and icons, and your quick notes and saved shapes join that account's — without history, comments or the people they were shared with: the new server starts fresh. Then delete your account here, if you like."</p>
        <div class="row">
            <button class="btn danger delete-account" on:click=move |_| ui.modal.set(Some(Modal::DeleteAccount))>"Delete my account…"</button>
        </div>
        <h3>"This browser"</h3>
        <p class="muted small">"Signing out keeps this browser as one of your devices: it keeps its key and your unsent changes, so next time you only sign in and your notes open right away."</p>
        <p class="muted small">"On a shared or borrowed computer, forget this browser instead: it signs out, removes this device from your account and deletes its key, unsent changes and local copy of your notes. Signing in here again then needs approval from another device or your recovery key."</p>
        <div class="row">
            <button class="btn" on:click=move |_| leptos::task::spawn_local(state::sign_out())><Icon name="logout"/>" Sign out"</button>
            <button class="btn danger" on:click=move |_| leptos::task::spawn_local(state::forget_browser())>"Forget this browser"</button>
        </div>
        <h3>"Version"</h3>
        <p class="muted small app-version">{versions}</p>
    }
}

#[component]
fn Devices() -> impl IntoView {
    let ui = ui();
    let list = RwSignal::new(Vec::<DeviceInfo>::new());
    let reload = move || {
        leptos::task::spawn_local(async move {
            if let Ok(d) = api::get::<Vec<DeviceInfo>>("/devices").await {
                list.set(d);
            }
        });
    };
    reload();
    Effect::new(move |_| {
        ui.pending_devices.get();
        reload();
    });
    view! {
        <p class="muted">"Each device keeps its own key. Removing a device signs it out and deletes its copy of your account key on the server."</p>
        <div class="list">
            {move || list.get().into_iter().map(|d| {
                let d2 = d.clone();
                let id = d.id.clone();
                let id2 = d.id.clone();
                let name = d.name.clone();
                view! {
                    <div class="list-row">
                        <Icon name="device"/>
                        <div class="grow">
                            <div>{d.name.clone()}{d.current.then(|| view! { <span class="pill">"this device"</span> })}
                                {(d.status == DeviceStatus::Pending).then(|| view! { <span class="pill warn">"waiting for approval"</span> })}</div>
                            <div class="muted small">{format!("added {} · last seen {}", util::relative_time(d.created_at), d.last_seen_at.map(util::relative_time).unwrap_or_else(|| "never".into()))}</div>
                        </div>
                        {(d.status == DeviceStatus::Pending).then(|| view! {
                            <button class="btn small primary" on:click=move |_| ui.modal.set(Some(Modal::ApproveDevice(d2.clone())))>"Review"</button>
                        })}
                        <button class="btn small" on:click=move |_| {
                            let n = util::window().prompt_with_message_and_default("Device name", &name).ok().flatten();
                            if let Some(n) = n {
                                let id = id.clone();
                                leptos::task::spawn_local(async move {
                                    let _ = api::patch::<_, ()>(&format!("/devices/{id}"), &RenameRequest { name: n }).await;
                                    reload();
                                });
                            }
                        }>"Rename"</button>
                        {d.current.then(|| view! {
                            <button class="btn small danger" on:click=move |_| leptos::task::spawn_local(state::forget_browser())>"Forget this browser"</button>
                        })}
                        <button class="btn small danger" class:hidden=d.current on:click=move |_| {
                            if util::confirm("Remove this device? It will be signed out and can't decrypt new data. If it was lost or stolen, also rotate the keys of your shared vaults.") {
                                let id = id2.clone();
                                leptos::task::spawn_local(async move {
                                    match api::delete(&format!("/devices/{id}")).await {
                                        Ok(()) => state::success("Device removed."),
                                        Err(e) => state::error(e.message),
                                    }
                                    reload();
                                });
                            }
                        }>"Remove"</button>
                    </div>
                }
            }).collect_view()}
        </div>
        <OfflineSetting/>
        <h3>"Add the desktop app"</h3>
        <p class="muted small">"Install the notekpr command-line client and run " <code>{format!("notekpr login --server {}", util::window().location().origin().unwrap_or_default())}</code>
            ". It will show a code to confirm here, then ask this device to approve it."</p>
    }
}

/// "Keep this browser usable offline" (see `state::offline`).
#[component]
fn OfflineSetting() -> impl IntoView {
    let on = RwSignal::new(step(|c| state::offline_enabled(c)));
    let busy = RwSignal::new(false);
    let supported = state::offline_supported();
    let toggle = move |want: bool| {
        if want
            && !util::confirm(
                "Keep what's needed to decrypt your notes in this browser? Anyone with a copy of this browser's data could then read your notes without the server, even after you remove this device. Only do this on your own computer with disk encryption.",
            )
        {
            on.set(false);
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            if want {
                match state::enable_offline().await {
                    Ok(()) => state::success("This browser now works without the server."),
                    Err(e) => {
                        on.set(false);
                        state::error(format!("Could not turn on offline use: {e}"));
                    }
                }
            } else {
                state::disable_offline().await;
                state::info("The offline data was removed from this browser.");
            }
            busy.set(false);
        });
    };
    view! {
        <h3>"Use without the server"</h3>
        <label class="check">
            <input type="checkbox" class="offline-toggle" prop:checked=move || on.get() disabled=move || busy.get() || !supported
                on:change=move |ev| { let want = event_target_checked(&ev); on.set(want); toggle(want); }/>
            " Keep this browser usable offline"
        </label>
        <p class="muted small">
            "Opens and edits your notes from this browser's copy when the server can't be reached; changes sync when it is back. "
            "This keeps the keys to your notes in this browser: anyone with a copy of its data could read them, even after you remove this device. Use it only on your own computer, with disk encryption."
        </p>
        {(!supported).then(|| view! { <p class="muted small">"Needs the app to be served over HTTPS."</p> })}
    }
}

#[component]
fn Sessions() -> impl IntoView {
    let list = RwSignal::new(Vec::<SessionEntry>::new());
    let reload = move || {
        leptos::task::spawn_local(async move {
            if let Ok(s) = api::get::<Vec<SessionEntry>>("/me/sessions").await {
                list.set(s);
            }
        });
    };
    reload();
    view! {
        <p class="muted">"Places where you are signed in."</p>
        <div class="list">
            {move || list.get().into_iter().map(|s| {
                let id = s.id.clone();
                view! {
                    <div class="list-row">
                        <Icon name="key"/>
                        <div class="grow">
                            <div>{s.client_name.clone().unwrap_or_else(|| s.kind.clone())}{s.current.then(|| view! { <span class="pill">"this session"</span> })}</div>
                            <div class="muted small">{format!("{} · started {} · active {}", s.kind, util::relative_time(s.created_at), s.last_used_at.map(util::relative_time).unwrap_or_else(|| "—".into()))}</div>
                        </div>
                        {(!s.current).then(|| view! {
                            <button class="btn small danger" on:click=move |_| {
                                let id = id.clone();
                                leptos::task::spawn_local(async move { let _ = api::delete(&format!("/me/sessions/{id}")).await; reload(); });
                            }>"Sign out"</button>
                        })}
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

#[component]
fn Security() -> impl IntoView {
    let new_key = RwSignal::new(None::<String>);
    view! {
        <h3>"Recovery key"</h3>
        <p>"Your recovery key unlocks your notes on a new device if all your devices are lost. Create a new one if you think the old one was exposed — the old key stops working immediately."</p>
        {move || match new_key.get() {
            Some(k) => {
                let k2 = k.clone();
                view! {
                    <div class="recovery-key">{k.clone()}</div>
                    <div class="row">
                        <button class="btn" on:click=move |_| { util::copy_to_clipboard(&k2); state::success("Copied."); }><Icon name="copy"/>" Copy"</button>
                        <button class="btn" on:click=move |_| util::download("notekpr-recovery-key.txt", "text/plain", format!("notekpr recovery key\n\n{k}\n").as_bytes())><Icon name="download"/>" Download"</button>
                    </div>
                }.into_any()
            }
            None => view! {
                <button class="btn" on:click=move |_| {
                    if !util::confirm("Create a new recovery key? Your old recovery key will stop working.") { return; }
                    leptos::task::spawn_local(async move {
                        let req = step(|c| c.account.as_ref().map(|a| notekpr_core::engine::rotate_recovery(a, &c.user_id, util::now_ms().max(0) as u64)));
                        if let Some((rk, req)) = req {
                            match api::put::<_, ()>("/me/recovery", &req).await {
                                Ok(()) => new_key.set(Some(rk.to_display())),
                                Err(e) => state::error(e.message),
                            }
                        }
                    });
                }><Icon name="key"/>" Create a new recovery key"</button>
            }.into_any(),
        }}
        <h3>"How your notes are protected"</h3>
        <ul class="bullets small">
            <li>"Notes, file names, folders, attachments and history are encrypted on your devices (XChaCha20-Poly1305) before upload."</li>
            <li>"Every note has its own key, and each vault a key that wraps them, shared with members via public-key encryption (HPKE, X25519). The server only stores encrypted copies."</li>
            <li>"Signing in (OIDC) proves who you are, but never gives anyone your keys."</li>
            <li>"Removing a member gives the vault new keys, so they can't read anything written afterwards. Removing a device signs it out; if it was lost or stolen, also rotate the keys of your vaults (“Rotate encryption key” in each vault’s settings)."</li>
        </ul>
    }
}

#[component]
fn Agents() -> impl IntoView {
    let list = RwSignal::new(Vec::<AgentInfo>::new());
    let created = RwSignal::new(None::<CreatedCredential>);
    let name = RwSignal::new(String::new());
    let reload = move || {
        leptos::task::spawn_local(async move {
            if let Ok(a) = api::get::<Vec<AgentInfo>>("/agents").await {
                list.set(a);
            }
        });
    };
    reload();
    let origin = util::window().location().origin().unwrap_or_default();
    view! {
        <p>"An agent is a separate identity for an AI assistant or automation. It has its own keys and only sees the vaults you explicitly share with it (as viewer or editor), through the notekpr CLI, the local agent gateway or a Markdown folder it edits."</p>
        <form class="row" on:submit=move |ev| {
            ev.prevent_default();
            let n = name.get_untracked();
            if n.trim().is_empty() { return; }
            leptos::task::spawn_local(async move {
                match api::post::<_, CreateAgentResponse>("/agents", &CreateAgentRequest { name: n }).await {
                    Ok(r) => { created.set(Some(r.credential)); name.set(String::new()); reload(); }
                    Err(e) => state::error(e.message),
                }
            });
        }>
            <input class="input grow" placeholder="Agent name (e.g. Home assistant)" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            <button class="btn primary" type="submit"><Icon name="plus"/>" Create agent"</button>
        </form>
        {move || created.get().map(|c| {
            let origin = origin.clone();
            let setup = format!("echo '{}' > agent.secret && chmod 600 agent.secret\nnotekpr agent setup --server {} --client-id {} --client-secret-file agent.secret", c.client_secret, origin, c.client_id);
            let s2 = setup.clone();
            view! {
                <div class="alert">
                    <b>"Copy these credentials now — the secret is shown only once."</b>
                    <pre class="mono small">{setup}</pre>
                    <button class="btn small" on:click=move |_| { util::copy_to_clipboard(&s2); state::success("Copied."); }><Icon name="copy"/>" Copy"</button>
                    <p class="small">"After setup, the agent prints its key fingerprint. Then share a vault with it from the vault's Share dialog (search for its name) and check that the fingerprint matches."</p>
                </div>
            }
        })}
        <div class="list">
            {move || list.get().into_iter().map(|a| {
                let id = a.id.clone();
                let id2 = a.id.clone();
                let fp = a.account_key.as_ref().and_then(|k| k.verify().ok()).map(|p| p.fingerprint());
                view! {
                    <div class="list-row col">
                        <div class="row">
                            <Icon name="bot"/>
                            <div class="grow">
                                <div>{a.display_name.clone()}</div>
                                <div class="muted small">{match &fp { Some(f) => format!("key {f}"), None => "not set up yet — run `notekpr agent setup`".into() }}</div>
                            </div>
                            <button class="btn small" on:click=move |_| {
                                let id = id.clone();
                                leptos::task::spawn_local(async move {
                                    match api::post_empty::<CreatedCredential>(&format!("/agents/{id}/credentials")).await {
                                        Ok(c) => { created.set(Some(c)); reload(); }
                                        Err(e) => state::error(e.message),
                                    }
                                });
                            }>"New secret"</button>
                            <button class="btn small danger" on:click=move |_| {
                                if util::confirm("Delete this agent? It loses access to every vault immediately.") {
                                    let id = id2.clone();
                                    leptos::task::spawn_local(async move { let _ = api::delete(&format!("/agents/{id}")).await; reload(); });
                                }
                            }>"Delete"</button>
                        </div>
                        {a.credentials.iter().filter(|c| c.revoked_at.is_none()).map(|c| {
                            let (aid, cid) = (a.id.clone(), c.client_id.clone());
                            view! {
                                <div class="cred small">
                                    <code>{c.client_id.clone()}</code>
                                    <span class="muted">{format!("created {} · used {}", util::relative_time(c.created_at), c.last_used_at.map(util::relative_time).unwrap_or_else(|| "never".into()))}</span>
                                    <button class="btn tiny" on:click=move |_| {
                                        let (aid, cid) = (aid.clone(), cid.clone());
                                        leptos::task::spawn_local(async move { let _ = api::delete(&format!("/agents/{aid}/credentials/{cid}")).await; reload(); });
                                    }>"Revoke"</button>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

#[component]
fn EditorPrefs() -> impl IntoView {
    let ui = ui();
    view! {
        <label class="check">
            <input type="checkbox" prop:checked=move || ui.ui_prefs.get().vim on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.vim = on); }/>
            " Vim keybindings (Esc for normal mode, i to insert)"
        </label>
        <label class="check">
            <input type="checkbox" class="code-numbers-toggle" prop:checked=move || ui.ui_prefs.get().code_numbers on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.code_numbers = on); }/>
            " Line numbers in code blocks"<span class="muted small">" — ln=false after a block's language hides them for that block"</span>
        </label>
        <label class="check">
            <input type="checkbox" class="code-guides-toggle" prop:checked=move || ui.ui_prefs.get().code_guides on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.code_guides = on); }/>
            " Indentation guides in code blocks"<span class="muted small">" — vl=false after a block's language hides them for that block"</span>
        </label>
        <label class="check">
            <input type="checkbox" class="code-wrap-toggle" prop:checked=move || ui.ui_prefs.get().code_wrap on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.code_wrap = on); }/>
            " Wrap long lines in code blocks"<span class="muted small">" — wrap=false or wrap=true after a block's language decides for that block"</span>
        </label>
        <label class="check">
            <input type="checkbox" prop:checked=move || ui.ui_prefs.get().right_panel on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.right_panel = on); }/>
            " Show backlinks & outline panel"
        </label>
        <h3>"Theme"</h3>
        <div class="seg">
            {["system", "light", "dark"].into_iter().map(|t| view! {
                <button class:on=move || ui.ui_prefs.get().theme == t on:click=move |_| state::set_ui_prefs(|p| p.theme = t.into())>{t}</button>
            }).collect_view()}
        </div>
        <NoteWidthPrefs/>
        <QuickNotesPrefs/>
        <MentionPrefs/>
    }
}

/// System notifications when someone mentions us (this browser).
#[component]
fn MentionPrefs() -> impl IntoView {
    let ui = ui();
    let supported = state::notifications_supported();
    view! {
        <h3>"Mentions"</h3>
        <label class="check">
            <input type="checkbox" class="mention-notify-toggle" disabled=!supported
                prop:checked=move || ui.ui_prefs.get().mention_notifications
                on:change=move |ev| {
                    let on = event_target_checked(&ev);
                    leptos::task::spawn_local(state::set_mention_notifications(on));
                }/>
            " Notify me when someone mentions me"
            <span class="muted small">{if supported {
                " — a system notification from this browser while notekpr is open but not in front (the sidebar's @ lists mentions either way)"
            } else {
                " — this browser can't show notifications"
            }}</span>
        </label>
    }
}

/// How wide notes are by default: in this browser, or on all devices (synced).
#[component]
fn NoteWidthPrefs() -> impl IntoView {
    let ui = ui();
    let shared = Memo::new(move |_| {
        ui.prefs_rev.get();
        step(|c| c.prefs.note_width.is_some())
    });
    let value = Memo::new(move |_| {
        ui.prefs_rev.get();
        let local = ui.ui_prefs.with(|p| p.note_width);
        step(|c| state::default_width(c, local))
    });
    view! {
        <h3>"Note width"</h3>
        <div class="width-prefs">
            <WidthSlider value=value.into() on_change=Callback::new(|w| step(|c| state::set_default_width(c, w)))/>
        </div>
        <label class="check">
            <input type="checkbox" class="width-shared" prop:checked=move || shared.get()
                on:change=move |ev| { let on = event_target_checked(&ev); step(|c| state::share_default_width(c, on)); }/>
            " The same on all my devices"<span class="muted small">" — otherwise just this browser. A note's ⋯ menu gives it its own width, only for you."</span>
        </label>
    }
}

/// Whether the sidebar shows the quick notes (this browser).
#[component]
fn QuickNotesPrefs() -> impl IntoView {
    let ui = ui();
    view! {
        <h3>"Quick notes"</h3>
        <label class="check">
            <input type="checkbox" class="quick-sidebar-toggle" prop:checked=move || ui.ui_prefs.get().quick_notes
                on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.quick_notes = on); }/>
            " Show quick notes in the sidebar"
        </label>
        <p class="muted small">"Quick notes are yours alone, in no vault: move one to a vault to keep it. New one: Alt+Q."</p>
    }
}

#[component]
fn Admin() -> impl IntoView {
    let users = RwSignal::new(Vec::<AdminUser>::new());
    let stats = RwSignal::new(None::<AdminStats>);
    // The instance's defaults, once the settings arrived.
    let defaults = RwSignal::new(None::<InstanceSettings>);
    let editing = RwSignal::new(None::<String>);
    // Whose vaults the vault list shows (all: `None`).
    let vaults_of = RwSignal::new(None::<(String, String)>);
    let reload = move || {
        leptos::task::spawn_local(async move {
            if let Ok(u) = api::get("/admin/users").await {
                users.set(u);
            }
            if let Ok(s) = api::get("/admin/stats").await {
                stats.set(Some(s));
            }
        });
    };
    reload();
    let me = step(|c| c.user_id.clone());
    let patch_user = move |id: String, change: AdminUpdateUser| {
        leptos::task::spawn_local(async move {
            if let Err(e) =
                api::patch::<_, serde_json::Value>(&format!("/admin/users/{id}"), &change).await
            {
                state::error(e.message);
            }
            reload();
        });
    };
    view! {
        <p class="muted">"Administrators manage accounts. They cannot read anyone's notes or vault names."</p>
        {move || stats.get().map(|s| view! {
            <div class="stats">
                <div><b>{s.users}</b><span>"people"</span></div>
                <div><b>{s.agents}</b><span>"agents"</span></div>
                <div><b>{s.vaults}</b><span>"vaults"</span></div>
                <div><b>{s.objects}</b><span>"documents"</span></div>
                <div><b>{s.attachments}</b><span>"files"</span></div>
                <div><b>{util::human_bytes(s.storage_bytes as u64)}</b><span>"database"</span></div>
            </div>
            <p class="muted small server-version">{format!("Server version {}", s.version)}</p>
        })}
        <InstanceSettingsPanel defaults=defaults on_saved=Callback::new(move |()| reload())/>
        <h3>"Accounts"</h3>
        <div class="list scroll admin-users">
            {move || users.get().into_iter().map(|u| {
                let id = u.id.clone();
                let id2 = u.id.clone();
                let id3 = u.id.clone();
                let is_me = u.id == me;
                let disabled = u.disabled;
                let admin = u.is_admin;
                let person = u.kind == PrincipalKind::Human;
                let open = {
                    let id = u.id.clone();
                    move || editing.get().as_deref() == Some(id.as_str())
                };
                let (owned, agent_count, va, aa) = (u.owned_vault_count, u.agent_count, u.vault_allowance, u.agent_allowance);
                let (quick_count, qa) = (u.quick_note_count, u.quick_note_allowance);
                let (quick_stored, qsa) = (u.quick_notes_storage_bytes, u.quick_notes_storage_allowance);
                let stored = util::human_bytes(u.storage_bytes);
                let usage = move || person.then(|| {
                    let d = defaults.get();
                    format!(
                        "owns {} · {} · {}, their Personal storage taking {} · their vaults take {stored}",
                        allowance_usage(owned, va, d.as_ref().map(|d| d.vaults_per_user), "vault"),
                        allowance_usage(agent_count, aa, d.as_ref().map(|d| d.agents_per_user), "agent"),
                        allowance_usage(quick_count, qa, d.as_ref().map(|d| d.quick_notes_per_user), "quick note"),
                        storage_usage(quick_stored, qsa, d.as_ref().map(|d| d.quick_notes_storage_mb)),
                    )
                });
                let whose = (u.id.clone(), u.display_name.clone());
                let (vaults, agents) = (RwSignal::new(u.vault_allowance), RwSignal::new(u.agent_allowance));
                let quick_notes = RwSignal::new(u.quick_note_allowance);
                let quick_notes_storage = RwSignal::new(qsa);
                let quick_notes_full = move || person && {
                    let d = defaults.get();
                    qsa.resolve(d.map(|d| d.quick_notes_storage_mb).unwrap_or_default())
                        .reached_mb(quick_stored)
                };
                view! {
                    <div class="list-row col" class:dim=disabled data-user=u.display_name.clone()>
                        <div class="row">
                            {avatar(&u.display_name, &u.id)}
                            <div class="grow">
                                <div>{u.display_name.clone()}{admin.then(|| view! { <span class="pill">"admin"</span> })}
                                    {(u.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}
                                    {disabled.then(|| view! { <span class="pill warn">"disabled"</span> })}
                                    {move || quick_notes_full().then(|| view! { <span class="pill warn">"Personal storage full"</span> })}</div>
                                <div class="muted small">{format!("{} · {} vaults · {} devices · last login {}", u.email.clone().unwrap_or_default(), u.vault_count, u.device_count, u.last_login_at.map(util::relative_time).unwrap_or_else(|| "never".into()))}</div>
                                {person.then(|| view! { <div class="muted small allowance-usage">{usage}</div> })}
                            </div>
                            <div class="row-actions">
                                {person.then(|| {
                                    let id = id3.clone();
                                    let whose = whose.clone();
                                    view! {
                                        <button class="btn small their-vaults-btn" title="Their vaults, in the list below"
                                            on:click=move |_| {
                                                vaults_of.set(Some(whose.clone()));
                                                if let Some(list) = util::window().document().and_then(|d| d.query_selector(".admin-vaults-title").ok().flatten()) {
                                                    list.scroll_into_view();
                                                }
                                            }>"Vaults"</button>
                                        <button class="btn small limits-btn" on:click=move |ev| {
                                            editing.update(|e| {
                                                *e = if e.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                                            });
                                            reveal_row(&ev);
                                        }>"Limits"</button>
                                    }
                                })}
                                {(!is_me && person).then(|| view! {
                                    <button class="btn small" on:click=move |_| patch_user(id.clone(), AdminUpdateUser { is_admin: Some(!admin), ..Default::default() })>
                                        {if admin { "Remove admin" } else { "Make admin" }}</button>
                                })}
                                {(!is_me).then(|| view! {
                                    <button class="btn small danger" on:click=move |_| patch_user(id2.clone(), AdminUpdateUser { disabled: Some(!disabled), ..Default::default() })>
                                        {if disabled { "Enable" } else { "Disable" }}</button>
                                })}
                            </div>
                        </div>
                        {move || open().then(|| {
                            let id = u.id.clone();
                            let d = defaults.get_untracked();
                            view! {
                                <div class="allowances">
                                    <AllowanceField label="Vaults they may own" value=vaults default=d.as_ref().map(|d| d.vaults_per_user) class="vault-allowance"/>
                                    <AllowanceField label="Agents they may have" value=agents default=d.as_ref().map(|d| d.agents_per_user) class="agent-allowance"/>
                                    <AllowanceField label="Quick notes they may keep" value=quick_notes default=d.as_ref().map(|d| d.quick_notes_per_user) class="quick-note-allowance"/>
                                    <AllowanceField label="Personal storage (MiB)" value=quick_notes_storage default=d.as_ref().map(|d| d.quick_notes_storage_mb) class="quick-notes-storage-allowance"/>
                                    <div class="row">
                                        <button class="btn small primary save-allowances" on:click=move |_| {
                                            editing.set(None);
                                            patch_user(id.clone(), AdminUpdateUser {
                                                vault_allowance: Some(vaults.get_untracked()),
                                                agent_allowance: Some(agents.get_untracked()),
                                                quick_note_allowance: Some(quick_notes.get_untracked()),
                                                quick_notes_storage_allowance: Some(quick_notes_storage.get_untracked()),
                                                ..Default::default()
                                            });
                                        }>"Save"</button>
                                        <span class="muted small">"Shared vaults don't count. Nothing they have is removed when you lower a limit."</span>
                                    </div>
                                </div>
                            }
                        })}
                    </div>
                }
            }).collect_view()}
        </div>
        <AdminVaults defaults=defaults owner=vaults_of on_saved=Callback::new(move |()| reload())/>
        <AdminClients/>
    }
}

/// Vaults by size, the largest first: their id, owner and size — never a
/// name or anything in them. People read a vault's id in its settings and
/// give it to ask for more room; *Limit* gives a vault a limit of its own.
#[component]
fn AdminVaults(
    defaults: RwSignal<Option<InstanceSettings>>,
    owner: RwSignal<Option<(String, String)>>,
    on_saved: Callback<()>,
) -> impl IntoView {
    let list = RwSignal::new(Vec::<AdminVault>::new());
    let search = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<String>);
    // Only the latest search's answer is shown: an earlier one may arrive last.
    let latest = StoredValue::new(0u64);
    let load = move || {
        let id = search.get_untracked().trim().to_string();
        let whose = owner.get_untracked().map(|(id, _)| id);
        let n = latest.get_value() + 1;
        latest.set_value(n);
        leptos::task::spawn_local(async move {
            let mut query = Vec::new();
            if !id.is_empty() {
                query.push(format!("id={}", api::q(&id)));
            }
            if let Some(o) = whose {
                query.push(format!("owner={}", api::q(&o)));
            }
            let path = match query.is_empty() {
                true => "/admin/vaults".to_string(),
                false => format!("/admin/vaults?{}", query.join("&")),
            };
            match api::get::<Vec<AdminVault>>(&path).await {
                Ok(l) if latest.get_value() == n => list.set(l),
                Ok(_) => {}
                Err(e) => state::error(e.message),
            }
        });
    };
    // Again whenever the owner filter changes.
    Effect::new(move |_| {
        owner.track();
        load();
    });
    let save = move |id: String, storage: Allowance, drawings: Allowance| {
        leptos::task::spawn_local(async move {
            let change = AdminUpdateVault {
                storage_allowance: Some(storage),
                drawing_allowance: Some(drawings),
            };
            match api::patch::<_, serde_json::Value>(&format!("/admin/vaults/{id}"), &change).await
            {
                Ok(_) => {
                    state::success("Vault limit saved.");
                    on_saved.run(());
                }
                Err(e) => state::error(e.message),
            }
            load();
        });
    };
    view! {
        <h3 class="admin-vaults-title">"Vaults"</h3>
        <p class="muted small">"The largest first. You see a vault's id, owner, size and how many drawings it holds — never its name or what's in it. Its members find the id in its settings, to ask you for more room. Someone's Personal storage (quick notes, saved shapes) isn't listed: its limit is the person's (Accounts → Limits)."</p>
        <form class="row vault-search" on:submit=move |ev| { ev.prevent_default(); load(); }>
            <input class="input grow vault-search-id" placeholder="A vault's id"
                prop:value=move || search.get() on:input=move |ev| search.set(event_target_value(&ev))/>
            <button class="btn" type="submit">"Find"</button>
            {move || (!search.get().is_empty()).then(|| view! {
                <button class="btn ghost" type="button" on:click=move |_| { search.set(String::new()); load(); }>"All"</button>
            })}
        </form>
        {move || owner.get().map(|(_, name)| view! {
            <p class="vaults-of small">{format!("{name}'s vaults ")}
                <button class="btn small ghost" on:click=move |_| owner.set(None)>"Show all"</button></p>
        })}
        <div class="list scroll admin-vaults">
            {move || {
                let d = defaults.get();
                let rows = list.get();
                if rows.is_empty() {
                    return view! { <p class="muted small">"No vault found."</p> }.into_any();
                }
                rows.into_iter().map(|a| {
                    let limit = a.storage_allowance.resolve(d.as_ref().map(|d| d.storage_per_vault_mb).unwrap_or_default());
                    let used = util::human_bytes(a.storage_bytes);
                    let usage = match limit.max() {
                        Some(mb) => format!("{used} of {}", util::human_bytes(mb.saturating_mul(MIB))),
                        None => format!("{used} (no limit)"),
                    };
                    let full = limit.reached_mb(a.storage_bytes);
                    let own = if a.storage_allowance == Allowance::Default { "" } else { " · its own limit" };
                    let drawings = notekpr_core::drawings::count_text(
                        a.drawing_count,
                        a.drawing_allowance.resolve(d.as_ref().map(|d| d.drawings_per_vault).unwrap_or_default()),
                    );
                    let value = RwSignal::new(a.storage_allowance);
                    let drawing_value = RwSignal::new(a.drawing_allowance);
                    let open = {
                        let id = a.id.clone();
                        move || editing.get().as_deref() == Some(id.as_str())
                    };
                    let (id, id2) = (a.id.clone(), a.id.clone());
                    view! {
                        <div class="list-row col" data-vault=a.id.clone()>
                            <div class="row">
                                {avatar(&a.owner_name, &a.owner_id)}
                                <div class="grow">
                                    <div><code class="vault-id">{a.id.clone()}</code>
                                        {full.then(|| view! { <span class="pill warn">"full"</span> })}</div>
                                    <div class="muted small vault-usage">{format!("{} · {usage}{own} · {drawings}", a.owner_name)}</div>
                                </div>
                                <button class="btn small vault-limit-btn" on:click=move |ev| {
                                    editing.update(|e| {
                                        *e = if e.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                                    });
                                    reveal_row(&ev);
                                }>"Limit"</button>
                            </div>
                            {move || open().then(|| {
                                let id = id2.clone();
                                let d = defaults.get_untracked();
                                view! {
                                    <div class="allowances">
                                        <AllowanceField label="Storage (MiB)" value=value default=d.as_ref().map(|d| d.storage_per_vault_mb) class="storage-allowance"/>
                                        <AllowanceField label="Drawings it may hold" value=drawing_value default=d.as_ref().map(|d| d.drawings_per_vault) class="vault-drawing-allowance"/>
                                        <div class="row">
                                            <button class="btn small primary save-vault-limit" on:click=move |_| {
                                                editing.set(None);
                                                save(id.clone(), value.get_untracked(), drawing_value.get_untracked());
                                            }>"Save"</button>
                                            <span class="muted small">"At its storage limit a vault takes no new content until something in it is deleted for good; at its drawings limit, no new drawing. Nothing is removed when you lower them."</span>
                                        </div>
                                    </div>
                                }
                            })}
                        </div>
                    }
                }).collect_view().into_any()
            }}
        </div>
    }
}

/// Keep the row whose button was clicked in sight once it has opened, in a
/// list that scrolls (`.list.scroll`).
fn reveal_row(ev: &web_sys::MouseEvent) {
    use wasm_bindgen::JsCast;
    let row = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|e| e.closest(".list-row").ok().flatten());
    if let Some(row) = row {
        request_animation_frame(move || {
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
            row.scroll_into_view_with_scroll_into_view_options(&opts);
        });
    }
}

/// Admin clients: scripts and services (a billing system, say) that use the
/// admin API with tokens of their own, limited to their scopes.
#[component]
fn AdminClients() -> impl IntoView {
    let list = RwSignal::new(Vec::<AdminClientInfo>::new());
    let created = RwSignal::new(None::<CreatedAdminToken>);
    let name = RwSignal::new(String::new());
    let chosen = RwSignal::new(vec![AdminScope::Accounts]);
    let reload = move || {
        leptos::task::spawn_local(async move {
            if let Ok(c) = api::get::<Vec<AdminClientInfo>>("/admin/clients").await {
                list.set(c);
            }
        });
    };
    reload();
    let shown = move |r: api::Result<CreatedAdminToken>| match r {
        Ok(t) => {
            created.set(Some(t));
            reload();
        }
        Err(e) => state::error(e.message),
    };
    let scope_text = |s: AdminScope| match s {
        AdminScope::Read => "read: accounts, statistics, settings, admin clients",
        AdminScope::Settings => "settings: limits and read-only mode",
        AdminScope::Accounts => "accounts: disable, enable, limits per person and per vault",
        AdminScope::Admins => "admins: give and take the admin role",
        AdminScope::Clients => {
            "clients: manage admin clients (never with more than its own scopes)"
        }
    };
    view! {
        <h3>"Admin clients"</h3>
        <p class="muted small">"Scripts and services that administer this server through the admin API — a billing system setting limits, say. Each has its own name, scopes and tokens (rotate them by adding a new one, then revoking the old); none can read anything in a vault."</p>
        <form class="admin-client-new" on:submit=move |ev| {
            ev.prevent_default();
            let req = CreateAdminClient { name: name.get_untracked().trim().to_string(), scopes: chosen.get_untracked() };
            if req.name.is_empty() { return; }
            leptos::task::spawn_local(async move {
                let r = api::post::<_, CreatedAdminToken>("/admin/clients", &req).await;
                if r.is_ok() { name.set(String::new()); }
                shown(r);
            });
        }>
            <div class="row">
                <input class="input grow client-name" placeholder="Name (e.g. billing)" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                <button class="btn primary create-client" type="submit"><Icon name="plus"/>" Create admin client"</button>
            </div>
            <div class="scopes">
                {AdminScope::ALL.into_iter().map(|s| {
                    let fixed = s == AdminScope::Read;
                    view! {
                        <label class="check">
                            <input type="checkbox" class=format!("scope-{}", s.as_str())
                                disabled=fixed
                                prop:checked=move || fixed || chosen.with(|c| c.contains(&s))
                                on:change=move |ev| {
                                    let on = event_target_checked(&ev);
                                    chosen.update(|c| { c.retain(|x| *x != s); if on { c.push(s); } });
                                }/>
                            <span class="small">{scope_text(s)}</span>
                        </label>
                    }
                }).collect_view()}
            </div>
        </form>
        {move || created.get().map(|t| {
            let token = t.token.clone();
            view! {
                <div class="alert new-admin-token">
                    <b>{format!("The token of “{}” — copy it now, it is shown only once.", t.client.name)}</b>
                    <pre class="mono small">{t.token.clone()}</pre>
                    <div class="row">
                        <button class="btn small" on:click=move |_| { util::copy_to_clipboard(&token); state::success("Copied."); }><Icon name="copy"/>" Copy"</button>
                        <button class="btn small ghost" on:click=move |_| created.set(None)>"Done"</button>
                    </div>
                    <p class="small muted">"Send it as Authorization: Bearer <token> to /api/v1/admin/…"</p>
                </div>
            }
        })}
        <div class="list admin-clients">
            {move || list.get().into_iter().map(|c| {
                let (id, id2) = (c.id.clone(), c.id.clone());
                let name = c.name.clone();
                let scopes = c.scopes.iter().filter(|s| **s != AdminScope::Read).map(|s| s.as_str()).collect::<Vec<_>>();
                view! {
                    <div class="list-row col" data-client=c.name.clone()>
                        <div class="row">
                            <Icon name="key"/>
                            <div class="grow">
                                <div>{c.name.clone()}{c.managed.then(|| view! { <span class="pill" title="Its scopes and tokens come from the server's configuration (notekpr-server admin-client ensure), which puts them back whenever it runs">"managed"</span> })}</div>
                                <div class="muted small">{if scopes.is_empty() { "read only".to_string() } else { format!("read, {}", scopes.join(", ")) }}{format!(" · created {}", util::relative_time(c.created_at))}</div>
                            </div>
                            <button class="btn small new-token" on:click=move |_| {
                                let id = id.clone();
                                leptos::task::spawn_local(async move { shown(api::post_empty::<CreatedAdminToken>(&format!("/admin/clients/{id}/tokens")).await) });
                            }>"New token"</button>
                            <button class="btn small danger delete-client" on:click=move |_| {
                                if util::confirm(&format!("Delete the admin client “{name}”? Its tokens stop working immediately.")) {
                                    let id = id2.clone();
                                    leptos::task::spawn_local(async move {
                                        if let Err(e) = api::delete(&format!("/admin/clients/{id}")).await { state::error(e.message); }
                                        reload();
                                    });
                                }
                            }>"Delete"</button>
                        </div>
                        {c.tokens.iter().map(|t| {
                            let (cid, tid) = (c.id.clone(), t.id.clone());
                            view! {
                                <div class="cred small">
                                    <code>{t.hint.clone()}</code>
                                    <span class="muted">{format!("created {} · used {}", util::relative_time(t.created_at), t.last_used_at.map(util::relative_time).unwrap_or_else(|| "never".into()))}</span>
                                    <button class="btn tiny revoke-token" on:click=move |_| {
                                        let (cid, tid) = (cid.clone(), tid.clone());
                                        leptos::task::spawn_local(async move {
                                            if let Err(e) = api::delete(&format!("/admin/clients/{cid}/tokens/{tid}")).await { state::error(e.message); }
                                            reload();
                                        });
                                    }>"Revoke"</button>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

/// "2 of 3 vaults", "1 agent (no limit)".
fn allowance_usage(n: i64, a: Allowance, default: Option<Limit>, what: &str) -> String {
    let things = if n == 1 {
        what.to_string()
    } else {
        format!("{what}s")
    };
    let limit = match a {
        Allowance::Default => default,
        a => Some(a.resolve(Limit::Unlimited)),
    };
    match limit {
        Some(Limit::Max(m)) => format!("{n} of {m} {what}s"),
        Some(Limit::Unlimited) => format!("{n} {things} (no limit)"),
        None => format!("{n} {things}"),
    }
}

/// What something takes of its storage limit (MiB): its own allowance, else
/// `default` (unknown until the settings arrived).
fn storage_usage(bytes: u64, a: Allowance, default: Option<Limit>) -> String {
    let used = util::human_bytes(bytes);
    let limit = match a {
        Allowance::Default => default,
        a => Some(a.resolve(Limit::Unlimited)),
    };
    match limit.and_then(|l| l.max()) {
        Some(mb) => format!("{used} of {}", util::human_bytes(mb.saturating_mul(MIB))),
        None => used,
    }
}

/// The instance settings (limits, read-only mode).
#[component]
fn InstanceSettingsPanel(
    defaults: RwSignal<Option<InstanceSettings>>,
    on_saved: Callback<()>,
) -> impl IntoView {
    let status = RwSignal::new(None::<AdminSettings>);
    let s = InstanceSettings::default();
    let fields = (
        RwSignal::new(s.max_users),
        RwSignal::new(s.max_agents),
        RwSignal::new(s.max_vaults),
        RwSignal::new(s.vaults_per_user),
        RwSignal::new(s.agents_per_user),
        RwSignal::new(s.quick_notes_per_user),
        RwSignal::new(s.max_storage_mb),
        RwSignal::new(s.storage_per_vault_mb),
        RwSignal::new(s.quick_notes_storage_mb),
        RwSignal::new(s.drawings_per_vault),
    );
    let (
        users,
        agents,
        vaults,
        vaults_per_user,
        agents_per_user,
        quick_per_user,
        storage,
        per_vault,
        quick_notes_storage,
        drawings_per_vault,
    ) = fields;
    let oldest_app = RwSignal::new(String::new());
    let show = move |a: AdminSettings| {
        let s = &a.settings;
        oldest_app.set(s.oldest_app_version.clone());
        users.set(s.max_users);
        agents.set(s.max_agents);
        vaults.set(s.max_vaults);
        vaults_per_user.set(s.vaults_per_user);
        agents_per_user.set(s.agents_per_user);
        quick_per_user.set(s.quick_notes_per_user);
        storage.set(s.max_storage_mb);
        per_vault.set(s.storage_per_vault_mb);
        quick_notes_storage.set(s.quick_notes_storage_mb);
        drawings_per_vault.set(s.drawings_per_vault);
        defaults.set(Some(a.settings.clone()));
        status.set(Some(a));
    };
    leptos::task::spawn_local(async move {
        if let Ok(a) = api::get::<AdminSettings>("/admin/settings").await {
            show(a);
        }
    });
    let save = move |change: InstanceSettingsUpdate| {
        leptos::task::spawn_local(async move {
            match api::patch::<_, AdminSettings>("/admin/settings", &change).await {
                Ok(a) => {
                    show(a);
                    state::success("Instance settings saved.");
                    on_saved.run(());
                }
                Err(e) => state::error(e.message),
            }
        });
    };
    // The fields are made once the settings arrived (they start from them).
    let loaded = Memo::new(move |_| status.with(|s| s.is_some()));
    view! {
        {move || loaded.get().then(|| view! {
            <h3>"Instance"</h3>
            <div class="instance-settings">
                <label class="check">
                    <input type="checkbox" class="read-only-toggle"
                        prop:checked=move || status.get().is_some_and(|s| s.settings.read_only)
                        on:change=move |ev| save(InstanceSettingsUpdate { read_only: Some(event_target_checked(&ev)), ..Default::default() })/>
                    <span class="nowrap">"Read-only"</span>
                    <span class="muted small">" — the server keeps everything but accepts no changes; the apps keep people's edits and send them when you switch it off"</span>
                </label>
                <p class="instance-status small">{move || status.get().map(|s| {
                    let used = util::human_bytes(s.storage_bytes);
                    let of = s.settings.max_storage_mb.max().map(|mb| format!(" of {}", util::human_bytes(mb.saturating_mul(1024 * 1024)))).unwrap_or_default();
                    match s.read_only {
                        None if s.storage_full => format!("Full: the instance reached its storage limit ({used}{of}). Every vault takes no new content until something is deleted for good or you raise the limit."),
                        None => format!("Accepting changes. Storage: {used}{of}."),
                        Some(ReadOnly::Manual) => format!("Read-only, switched on by an administrator. Storage: {used}{of}."),
                        Some(ReadOnly::StorageFull) => format!("Read-only: the database reached its size limit ({used}{of}). Raise the limit to accept changes again."),
                        Some(ReadOnly::Other) => format!("Read-only. Storage: {used}{of}."),
                    }
                })}</p>
                <div class="limits">
                    <LimitField label="Accounts" value=users class="max-users"/>
                    <LimitField label="Agents" value=agents class="max-agents"/>
                    <LimitField label="Vaults" value=vaults class="max-vaults"/>
                    <LimitField label="Vaults per person" value=vaults_per_user class="vaults-per-user"/>
                    <LimitField label="Agents per person" value=agents_per_user class="agents-per-user"/>
                    <LimitField label="Quick notes per person" value=quick_per_user class="quick-notes-per-user"/>
                    <LimitField label="Instance storage (MiB)" value=storage class="max-storage"/>
                    <LimitField label="Storage per vault (MiB)" value=per_vault class="storage-per-vault"/>
                    <LimitField label="Personal storage (MiB)" value=quick_notes_storage class="quick-notes-storage"/>
                    <LimitField label="Drawings per vault" value=drawings_per_vault class="drawings-per-vault"/>
                </div>
                <div class="row">
                    <button class="btn primary save-limits" on:click=move |_| save(InstanceSettingsUpdate {
                        max_users: Some(users.get_untracked()),
                        max_agents: Some(agents.get_untracked()),
                        max_vaults: Some(vaults.get_untracked()),
                        vaults_per_user: Some(vaults_per_user.get_untracked()),
                        agents_per_user: Some(agents_per_user.get_untracked()),
                        quick_notes_per_user: Some(quick_per_user.get_untracked()),
                        max_storage_mb: Some(storage.get_untracked()),
                        storage_per_vault_mb: Some(per_vault.get_untracked()),
                        quick_notes_storage_mb: Some(quick_notes_storage.get_untracked()),
                        drawings_per_vault: Some(drawings_per_vault.get_untracked()),
                        ..Default::default()
                    })>"Save limits"</button>
                </div>
                <p class="muted small">"Limits apply to what is created from now on: nothing is removed when you lower one. Accounts count people who aren't disabled; a person's own limits are under Accounts. At the instance storage limit every vault is full until there is room again (deleting for good keeps working): on an instance of its own, the room a family or a team shares however they like. A vault at its storage limit takes no new content until something in it is deleted for good, or you give it more room (Vaults, below), and so many drawings, whoever draws them: give one vault more there too. Someone's Personal storage (quick notes, saved shapes) has its own storage limit, like how many quick notes they may keep: give one person more under Accounts → Limits."</p>
                <div class="row oldest-app">
                    <label class="limit-field">
                        <span class="limit-label">"Oldest app version"</span>
                        <input class="input small oldest-app-version" placeholder="0.6.0"
                            prop:value=move || oldest_app.get()
                            on:input=move |ev| oldest_app.set(event_target_value(&ev))/>
                    </label>
                    <button class="btn save-oldest-app" on:click=move |_| save(InstanceSettingsUpdate {
                        oldest_app_version: Some(oldest_app.get_untracked()),
                        ..Default::default()
                    })>"Save"</button>
                </div>
                <p class="muted small">{move || {
                    let build = status.get().map(|s| s.build_oldest_app_version).unwrap_or_default();
                    format!("Apps older than this (the command line now, desktop and phone apps later) are told to update instead of syncing, and keep their unsent edits; browsers get the current version with a reload. Empty: what this server's version needs ({build}).")
                }}</p>
            </div>
        })}
    }
}

/// A limit: no limit, or at most a number.
#[component]
fn LimitField(label: &'static str, value: RwSignal<Limit>, class: &'static str) -> impl IntoView {
    let limited = Memo::new(move |_| value.get() != Limit::Unlimited);
    view! {
        <label class=format!("limit-field {class}")>
            <span class="limit-label">{label}</span>
            <select class="input small limit-kind" on:change=move |ev| {
                value.set(match event_target_value(&ev).as_str() {
                    "max" => Limit::Max(0),
                    _ => Limit::Unlimited,
                });
            }>
                <option value="unlimited" selected=move || !limited.get()>"No limit"</option>
                <option value="max" selected=move || limited.get()>"At most"</option>
            </select>
            {move || limited.get().then(|| view! { <NumberInput value=Signal::derive(move || value.get_untracked().max().unwrap_or(0)) set=Callback::new(move |n| value.set(Limit::Max(n)))/> })}
        </label>
    }
}

/// A person's own limit: the instance's default, no limit, or a number.
#[component]
fn AllowanceField(
    label: &'static str,
    value: RwSignal<Allowance>,
    default: Option<Limit>,
    class: &'static str,
) -> impl IntoView {
    let kind = Memo::new(move |_| match value.get() {
        Allowance::Default => "default",
        Allowance::Unlimited => "unlimited",
        Allowance::Max(_) => "max",
    });
    let default_label = match default {
        Some(Limit::Max(n)) => format!("Default ({n})"),
        Some(Limit::Unlimited) => "Default (no limit)".to_string(),
        None => "Default".to_string(),
    };
    view! {
        <label class=format!("limit-field {class}")>
            <span class="limit-label">{label}</span>
            <select class="input small limit-kind" on:change=move |ev| {
                value.set(match event_target_value(&ev).as_str() {
                    "max" => Allowance::Max(0),
                    "unlimited" => Allowance::Unlimited,
                    _ => Allowance::Default,
                });
            }>
                <option value="default" selected=move || kind.get() == "default">{default_label}</option>
                <option value="unlimited" selected=move || kind.get() == "unlimited">"No limit"</option>
                <option value="max" selected=move || kind.get() == "max">"At most"</option>
            </select>
            {move || (kind.get() == "max").then(|| view! { <NumberInput value=Signal::derive(move || match value.get_untracked() { Allowance::Max(n) => n, _ => 0 }) set=Callback::new(move |n| value.set(Allowance::Max(n)))/> })}
        </label>
    }
}

/// A whole number; made once, so typing isn't interrupted by re-renders.
#[component]
fn NumberInput(value: Signal<u64>, set: Callback<u64>) -> impl IntoView {
    view! {
        <input class="input small num limit-num" type="number" min="0" step="1"
            prop:value=value.get_untracked().to_string()
            on:input=move |ev| {
                if let Ok(n) = event_target_value(&ev).trim().parse::<u64>() {
                    set.run(n);
                }
            }/>
    }
}

/// Approve (or deny) a sign-in code shown by the desktop / CLI client.
#[component]
pub fn DeviceCodePage(code: Option<String>) -> impl IntoView {
    let code_in = RwSignal::new(code.unwrap_or_default());
    let found = RwSignal::new(None::<DeviceCodeLookup>);
    let msg = RwSignal::new(None::<String>);
    let lookup = move || {
        let c = code_in.get_untracked();
        leptos::task::spawn_local(async move {
            match api::get::<DeviceCodeLookup>(&format!("/oauth/device?user_code={}", api::q(&c)))
                .await
            {
                Ok(l) => {
                    found.set(Some(l));
                    msg.set(None);
                }
                Err(_) => {
                    found.set(None);
                    msg.set(Some("That code is unknown or has expired.".into()));
                }
            }
        });
    };
    if !code_in.get_untracked().is_empty() {
        lookup();
    }
    let decide = move |approve: bool| {
        let Some(l) = found.get_untracked() else {
            return;
        };
        leptos::task::spawn_local(async move {
            match api::post::<_, ()>(
                "/oauth/device/decision",
                &DeviceCodeDecision {
                    user_code: l.user_code,
                    approve,
                },
            )
            .await
            {
                Ok(()) => {
                    found.set(None);
                    msg.set(Some(if approve {
                        "Signed in. Return to your terminal — it will now ask this browser to approve the new device's keys.".into()
                    } else {
                        "Request denied.".into()
                    }));
                }
                Err(e) => msg.set(Some(e.message)),
            }
        });
    };
    view! {
        <div class="page">
            <header class="page-head"><h1>"Sign in a device"</h1></header>
            <div class="page-body narrow">
                <p>"Enter the code shown by the notekpr desktop app or command-line client."</p>
                <form class="row" on:submit=move |ev| { ev.prevent_default(); lookup(); }>
                    <input class="input mono grow" placeholder="ABCD-EFGH" prop:value=move || code_in.get() on:input=move |ev| code_in.set(event_target_value(&ev))/>
                    <button class="btn" type="submit">"Continue"</button>
                </form>
                {move || msg.get().map(|m| view! { <div class="alert">{m}</div> })}
                {move || found.get().map(|l| view! {
                    <div class="alert">
                        <p>{format!("“{}” wants to sign in as you. Only continue if you started this yourself, just now.", l.client_name)}</p>
                        // Where it came from: a code someone sent you was asked for from their address.
                        <p class="muted small device-origin">
                            {format!("Requested {}", util::relative_time(l.created_at))}
                            {l.requested_from.map(|ip| view! { " from " <span class="mono">{ip}</span> })}
                            "."
                        </p>
                        <div class="row">
                            <button class="btn primary" on:click=move |_| decide(true)>"Allow"</button>
                            <button class="btn danger" on:click=move |_| decide(false)>"Deny"</button>
                        </div>
                    </div>
                })}
            </div>
        </div>
    }
}
