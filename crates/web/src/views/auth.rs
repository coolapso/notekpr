//! Signed-out and onboarding screens.

use leptos::prelude::*;

use crate::state::{self, Phase, ui};
use crate::util;
use crate::views::icons::Icon;

#[component]
fn Card(children: Children) -> impl IntoView {
    view! {
        <div class="auth">
            <div class="auth-card">
                <div class="brand"><span class="logo">"◆"</span>" coolpsonotes"</div>
                {children()}
            </div>
            <p class="auth-foot">"End-to-end encrypted · self-hosted · open source"</p>
        </div>
    }
}

#[component]
pub fn Loading() -> impl IntoView {
    view! { <div class="center-fill"><div class="spinner"></div></div> }
}

#[component]
pub fn Fatal(msg: String) -> impl IntoView {
    view! {
        <Card>
            <h1>"Something went wrong"</h1>
            <p class="muted">{msg}</p>
            <button class="btn primary" on:click=move |_| { let _ = util::window().location().reload(); }>"Try again"</button>
        </Card>
    }
}

#[component]
pub fn Login(error: Option<String>) -> impl IntoView {
    let ui = ui();
    let server = ui.server.get_untracked();
    let oidc = server.as_ref().map(|s| s.oidc_enabled).unwrap_or(false);
    let dev = server.as_ref().map(|s| s.dev_mode).unwrap_or(false);
    let sso_name = server
        .as_ref()
        .and_then(|s| s.oidc_display_name.clone())
        .unwrap_or_else(|| "Single sign-on".into());
    let username = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let start_sso = move |_| {
        let route = state::current_route();
        let rt = if matches!(route, state::Route::Home) {
            "/".to_string()
        } else {
            route.path()
        };
        let _ = util::window().location().set_href(&format!(
            "/api/v1/auth/oidc/start?return_to={}",
            crate::api::q(&rt)
        ));
    };
    let dev_login = move |name: String| {
        if name.trim().is_empty() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            state::dev_login(name.trim().to_lowercase()).await;
            busy.set(false);
        });
    };
    view! {
        <Card>
            <h1>"Sign in"</h1>
            {error.map(|e| view! { <div class="alert error">{e}</div> })}
            {oidc.then(|| view! {
                <button class="btn primary big" on:click=start_sso>{format!("Continue with {sso_name}")}</button>
            })}
            {(!oidc && !dev).then(|| view! { <p class="muted">"No sign-in method is configured on this server."</p> })}
            {dev.then(|| view! {
                <div class="dev-box">
                    <div class="dev-title"><Icon name="alert"/>" Development mode"</div>
                    <p class="small muted">"Sign in as any user without a password. Each browser tab keeps its own session, so open another tab to sign in as a second person."</p>
                    <form on:submit=move |ev| { ev.prevent_default(); dev_login(username.get_untracked()); }>
                        <input class="input" placeholder="user name (e.g. alice)" autofocus
                            prop:value=move || username.get() on:input=move |ev| username.set(event_target_value(&ev))/>
                        <button class="btn" type="submit" disabled=move || busy.get()>"Sign in"</button>
                    </form>
                    <div class="dev-quick">
                        {["alice", "bob", "carol"].into_iter().map(|n| view! {
                            <button class="chip" on:click=move |_| dev_login(n.to_string())>{n}</button>
                        }).collect_view()}
                    </div>
                </div>
            })}
        </Card>
    }
}

#[component]
pub fn Setup() -> impl IntoView {
    let busy = RwSignal::new(false);
    let name = ui()
        .user
        .get_untracked()
        .map(|u| u.display_name)
        .unwrap_or_default();
    view! {
        <Card>
            <h1>{format!("Welcome, {name}!")}</h1>
            <p>"Before you start, coolpsonotes creates your personal encryption keys on this device. Your notes are encrypted with them before anything is uploaded — not even the server administrator can read them."</p>
            <ul class="bullets">
                <li><Icon name="lock"/>" Keys are created and kept on your devices."</li>
                <li><Icon name="key"/>" You'll get a recovery key to keep somewhere safe."</li>
                <li><Icon name="device"/>" Add more devices later by approving them from this one."</li>
            </ul>
            <button class="btn primary big" disabled=move || busy.get() on:click=move |_| {
                busy.set(true);
                leptos::task::spawn_local(async move { state::setup_keys().await; busy.set(false); });
            }>{move || if busy.get() { "Creating keys…" } else { "Create my keys" }}</button>
            <button class="btn ghost" on:click=move |_| leptos::task::spawn_local(state::sign_out())>"Sign out"</button>
        </Card>
    }
}

#[component]
pub fn ShowRecovery(key: String) -> impl IntoView {
    let saved = RwSignal::new(false);
    let k1 = key.clone();
    let k2 = key.clone();
    view! {
        <Card>
            <h1>"Save your recovery key"</h1>
            <p>"If you lose all your devices, this key is the only way to get your notes back. Nobody — including the server administrator — can reset it for you."</p>
            <div class="recovery-key" aria-label="Recovery key">{key.clone()}</div>
            <div class="row">
                <button class="btn" on:click=move |_| { util::copy_to_clipboard(&k1); state::success("Copied to clipboard"); }><Icon name="copy"/>" Copy"</button>
                <button class="btn" on:click=move |_| {
                    let text = format!("coolpsonotes recovery key\n\nAccount: {}\nServer: {}\n\n{}\n\nKeep this somewhere safe and private (a password manager or a printed copy). Anyone with this key and access to your account can read your notes.\n",
                        ui().user.get_untracked().map(|u| u.display_name).unwrap_or_default(),
                        util::window().location().host().unwrap_or_default(), k2);
                    util::download("coolpsonotes-recovery-key.txt", "text/plain", text.as_bytes());
                }><Icon name="download"/>" Download"</button>
            </div>
            <label class="check">
                <input type="checkbox" prop:checked=move || saved.get() on:change=move |ev| saved.set(event_target_checked(&ev))/>
                " I have stored my recovery key somewhere safe"
            </label>
            <button class="btn primary big" disabled=move || !saved.get() on:click=move |_| {
                leptos::task::spawn_local(state::finish_setup());
            }>"Continue"</button>
        </Card>
    }
}

#[component]
pub fn Enroll(pending: Option<(String, String)>, error: Option<String>) -> impl IntoView {
    let key = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let show_recovery = RwSignal::new(false);
    let has_pending = pending.is_some();
    view! {
        <Card>
            <h1>"Add this browser"</h1>
            {error.map(|e| view! { <div class="alert error">{e}</div> })}
            {match pending {
                Some((_, code)) => view! {
                    <p>"Open coolpsonotes on a device that is already set up. It will ask you to approve this browser. Check that it shows this code:"</p>
                    <div class="verify-code">{code}</div>
                    <p class="muted small"><span class="spinner inline"></span>" Waiting for approval…"</p>
                }.into_any(),
                None => view! {
                    <p>"Your notes are encrypted with keys that live on your other devices. Choose how to unlock them here:"</p>
                    <button class="btn primary big" disabled=move || busy.get() on:click=move |_| {
                        busy.set(true);
                        leptos::task::spawn_local(async move { state::request_approval().await; busy.set(false); });
                    }><Icon name="device"/>" Approve from another device"</button>
                }.into_any(),
            }}
            <div class="divider"><span>"or"</span></div>
            <Show when=move || show_recovery.get() fallback=move || view! {
                <button class="btn ghost" on:click=move |_| show_recovery.set(true)><Icon name="key"/>" Use my recovery key"</button>
            }>
                <form class="stack" on:submit=move |ev| {
                    ev.prevent_default();
                    busy.set(true);
                    let k = key.get_untracked();
                    leptos::task::spawn_local(async move { state::recover(k).await; busy.set(false); });
                }>
                    <textarea class="input mono" rows="3" placeholder="XXXXX-XXXXX-…" autofocus
                        prop:value=move || key.get() on:input=move |ev| key.set(event_target_value(&ev))></textarea>
                    <button class="btn primary" type="submit" disabled=move || busy.get()>"Unlock with recovery key"</button>
                </form>
            </Show>
            {has_pending.then(|| view! { <p class="small muted">"Lost all your devices? Use your recovery key instead."</p> })}
            <button class="btn ghost" on:click=move |_| leptos::task::spawn_local(state::sign_out())>"Sign out"</button>
        </Card>
    }
}

pub fn phase_view(p: Phase) -> AnyView {
    match p {
        Phase::Loading => view! { <Loading/> }.into_any(),
        Phase::Fatal(m) => view! { <Fatal msg=m/> }.into_any(),
        Phase::SignedOut { error } => view! { <Login error=error/> }.into_any(),
        Phase::Setup => view! { <Setup/> }.into_any(),
        Phase::ShowRecovery { key } => view! { <ShowRecovery key=key/> }.into_any(),
        Phase::Enroll { pending, error } => {
            view! { <Enroll pending=pending error=error/> }.into_any()
        }
        Phase::Ready => view! { <crate::views::Shell/> }.into_any(),
    }
}
