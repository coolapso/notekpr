//! Settings: account, devices, sessions, recovery, agents, editor, admin.
//! Also the page used to approve a desktop/CLI sign-in code.

use coolpsonotes_core::protocol::*;
use leptos::prelude::*;

use crate::state::{self, Modal, Route, ui, with};
use crate::util;
use crate::views::icons::Icon;
use crate::{api, views::avatar};

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
                <button class="icon-btn mobile-only" title="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
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
    let fp = with(|c| c.account.as_ref().map(|a| a.public().fingerprint())).unwrap_or_default();
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
        <p class="muted small">"Export any vault as plain Markdown from its settings. Your data is never locked into a proprietary format."</p>
        <button class="btn" on:click=move |_| leptos::task::spawn_local(state::sign_out())><Icon name="logout"/>" Sign out"</button>
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
                        <button class="btn small danger" on:click=move |_| {
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
        <h3>"Add the desktop app"</h3>
        <p class="muted small">"Install the coolpsonotes command-line client and run " <code>{format!("coolpsonotes login --server {}", util::window().location().origin().unwrap_or_default())}</code>
            ". It will show a code to confirm here, then ask this device to approve it."</p>
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
                        <button class="btn" on:click=move |_| util::download("coolpsonotes-recovery-key.txt", "text/plain", format!("coolpsonotes recovery key\n\n{k}\n").as_bytes())><Icon name="download"/>" Download"</button>
                    </div>
                }.into_any()
            }
            None => view! {
                <button class="btn" on:click=move |_| {
                    if !util::confirm("Create a new recovery key? Your old recovery key will stop working.") { return; }
                    leptos::task::spawn_local(async move {
                        let req = with(|c| c.account.as_ref().map(|a| coolpsonotes_core::engine::rotate_recovery(a, &c.user_id)));
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
            <li>"Each vault has its own key, shared with members via public-key encryption (HPKE, X25519). The server only stores encrypted copies."</li>
            <li>"Signing in (OIDC) proves who you are, but never gives anyone your keys."</li>
            <li>"Removing a member or device rotates keys so they can't read anything new."</li>
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
        <p>"An agent is a separate identity for an AI assistant or automation. It has its own keys and only sees the vaults you explicitly share with it (as viewer or editor), through the coolpsonotes CLI, the local agent gateway or a Markdown folder it edits."</p>
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
            let setup = format!("echo '{}' > agent.secret && chmod 600 agent.secret\ncoolpsonotes agent setup --server {} --client-id {} --client-secret-file agent.secret", c.client_secret, origin, c.client_id);
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
                                <div class="muted small">{match &fp { Some(f) => format!("key {f}"), None => "not set up yet — run `coolpsonotes agent setup`".into() }}</div>
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
            <input type="checkbox" prop:checked=move || ui.ui_prefs.get().wide on:change=move |ev| { let on = event_target_checked(&ev); state::set_ui_prefs(|p| p.wide = on); }/>
            " Full-width notes"
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
    }
}

#[component]
fn Admin() -> impl IntoView {
    let users = RwSignal::new(Vec::<AdminUser>::new());
    let stats = RwSignal::new(None::<AdminStats>);
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
    let me = with(|c| c.user_id.clone());
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
        })}
        <div class="list">
            {move || users.get().into_iter().map(|u| {
                let id = u.id.clone();
                let id2 = u.id.clone();
                let is_me = u.id == me;
                let disabled = u.disabled;
                let admin = u.is_admin;
                view! {
                    <div class="list-row" class:dim=disabled>
                        {avatar(&u.display_name, &u.id)}
                        <div class="grow">
                            <div>{u.display_name.clone()}{admin.then(|| view! { <span class="pill">"admin"</span> })}
                                {(u.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}
                                {disabled.then(|| view! { <span class="pill warn">"disabled"</span> })}</div>
                            <div class="muted small">{format!("{} · {} vaults · {} devices · last login {}", u.email.clone().unwrap_or_default(), u.vault_count, u.device_count, u.last_login_at.map(util::relative_time).unwrap_or_else(|| "never".into()))}</div>
                        </div>
                        {(!is_me && u.kind == PrincipalKind::Human).then(|| view! {
                            <button class="btn small" on:click=move |_| {
                                let id = id.clone();
                                leptos::task::spawn_local(async move {
                                    let _ = api::patch::<_, serde_json::Value>(&format!("/admin/users/{id}"), &AdminUpdateUser { is_admin: Some(!admin), disabled: None }).await;
                                    reload();
                                });
                            }>{if admin { "Remove admin" } else { "Make admin" }}</button>
                        })}
                        {(!is_me).then(|| view! {
                            <button class="btn small danger" on:click=move |_| {
                                let id = id2.clone();
                                leptos::task::spawn_local(async move {
                                    let _ = api::patch::<_, serde_json::Value>(&format!("/admin/users/{id}"), &AdminUpdateUser { disabled: Some(!disabled), is_admin: None }).await;
                                    reload();
                                });
                            }>{if disabled { "Enable" } else { "Disable" }}</button>
                        })}
                    </div>
                }
            }).collect_view()}
        </div>
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
                <p>"Enter the code shown by the coolpsonotes desktop app or command-line client."</p>
                <form class="row" on:submit=move |ev| { ev.prevent_default(); lookup(); }>
                    <input class="input mono grow" placeholder="ABCD-EFGH" prop:value=move || code_in.get() on:input=move |ev| code_in.set(event_target_value(&ev))/>
                    <button class="btn" type="submit">"Continue"</button>
                </form>
                {move || msg.get().map(|m| view! { <div class="alert">{m}</div> })}
                {move || found.get().map(|l| view! {
                    <div class="alert">
                        <p>{format!("“{}” wants to sign in as you. Only continue if you started this yourself, just now.", l.client_name)}</p>
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
