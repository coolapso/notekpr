//! Signed-out and onboarding screens.

use super::focus_on_mount;
use leptos::prelude::*;

use crate::state::{self, Phase, ui};
use crate::views::icons::Icon;
use crate::{homescreen, storage, util};

#[component]
fn Card(children: Children) -> impl IntoView {
    view! {
        <div class="auth">
            <div class="auth-card">
                <div class="brand"><span class="logo">"◆"</span>" notekpr"</div>
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
    // The desktop app, too old for its server: its files are its own, so
    // trying again brings back the same app. A newer one must be installed.
    if crate::desktop::is_desktop() && ui().outdated.get_untracked() {
        return view! {
            <Card>
                <h1>"This app needs updating"</h1>
                <p class="muted update-required">"The server needs a newer version of notekpr. Install it the way you installed this app; what it keeps on this computer stays."</p>
                <button class="btn primary" on:click=|_| crate::desktop::update_help()>"How to update"</button>
                <AccountsButton/>
            </Card>
        }
        .into_any();
    }
    view! {
        <Card>
            <h1>"Something went wrong"</h1>
            <p class="muted">{msg}</p>
            <button class="btn primary" on:click=move |_| { let _ = util::window().location().reload(); }>"Try again"</button>
            <AccountsButton/>
        </Card>
    }
    .into_any()
}

/// The desktop app, stuck before the notes open (a server that can't be
/// reached or answers wrongly, notes on this computer that can't be
/// opened, keys gone without the recovery key): the way to its other
/// accounts, or a new one ([`Connect`]). Nothing moves by itself.
#[component]
fn AccountsButton() -> impl IntoView {
    crate::desktop::is_desktop().then(|| {
        view! {
            <button class="btn ghost accounts-button" on:click=move |_| ui().phase.set(Phase::Connect { error: None })>"Accounts"</button>
        }
    })
}

#[component]
pub fn Login(error: Option<String>) -> impl IntoView {
    if crate::desktop::this_computer() {
        return view! { <OnThisComputer error/> }.into_any();
    }
    let ui = ui();
    let server = ui.server.get_untracked();
    let oidc = server.as_ref().map(|s| s.oidc_enabled).unwrap_or(false);
    let dev = server.as_ref().map(|s| s.dev_mode).unwrap_or(false);
    // Kept current: a demo that started over has a new time.
    let resets_at = move || {
        ui.server
            .get()
            .and_then(|s| s.demo_resets_at)
            .map(util::clock_time)
    };
    let sso_name = server
        .as_ref()
        .and_then(|s| s.oidc_display_name.clone())
        .unwrap_or_else(|| "Single sign-on".into());
    let username = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let desktop = crate::desktop::is_desktop();
    // The desktop app: signing in happens in the browser meanwhile.
    let waiting = RwSignal::new(false);
    let failed = RwSignal::new(None::<String>);
    let start_sso = move |_| {
        if desktop {
            waiting.set(true);
            failed.set(None);
            leptos::task::spawn_local(async move {
                let r = state::desktop_sign_in().await;
                waiting.set(false);
                match r {
                    Ok(()) => {}
                    Err(e) if e == "cancelled" => {}
                    Err(e) => failed.set(Some(e)),
                }
            });
            return;
        }
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
            {move || failed.get().map(|e| view! { <div class="alert error">{e}</div> })}
            {homescreen::offer().map(|phone| view! { <HomeScreenOffer phone/> })}
            {oidc.then(|| view! {
                <Show when=move || waiting.get() fallback=move || view! {
                    <button class="btn primary big" on:click=start_sso>{format!("Continue with {sso_name}")}</button>
                }>
                    <p class="muted">"Sign in in your browser: notekpr opened it for you. Before it sends you back here, it shows a code: continue there only if it is this one."</p>
                    <p class="sign-in-code">{move || ui.desktop.get().sign_in_code.unwrap_or_default()}</p>
                    <button class="btn" on:click=move |_| crate::desktop::cancel_sign_in()>"Cancel"</button>
                </Show>
            })}
            {(!oidc && !dev).then(|| view! { <p class="muted">"No sign-in method is configured on this server."</p> })}
            {dev.then(|| view! {
                <div class="dev-box">
                    {move || match resets_at() {
                        Some(at) => view! {
                            <div class="dev-title"><Icon name="alert"/>" Demo"</div>
                            <p class="small muted">{if desktop {
                                "Sign in as alice (most to see), bob, carol, dave or eve — no password; other visitors may be signed in as them too."
                            } else {
                                "Sign in as alice (most to see), bob, carol, dave or eve — no password; other visitors may be signed in as them too. Each browser tab keeps its own session."
                            }}</p>
                            <p class="small muted">{format!("Everything is deleted at {at}, when the demo starts over.")}</p>
                        }.into_any(),
                        None if desktop => view! {
                            <div class="dev-title"><Icon name="alert"/>" Development mode"</div>
                            <p class="small muted">"Sign in as any user without a password. For a second person, start the app again with another profile: notekpr-desktop --profile bob."</p>
                        }.into_any(),
                        None => view! {
                            <div class="dev-title"><Icon name="alert"/>" Development mode"</div>
                            <p class="small muted">"Sign in as any user without a password. Each browser tab keeps its own session, so open another tab to sign in as a second person."</p>
                        }.into_any(),
                    }}
                    <form on:submit=move |ev| { ev.prevent_default(); dev_login(username.get_untracked()); }>
                        <input class="input" placeholder="user name (e.g. alice)" use:focus_on_mount
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
            {desktop.then(|| view! {
                <p class="small muted desktop-server">
                    {crate::desktop::server().map(|s| format!("On {s} · "))}
                    <button class="btn small ghost accounts-button" on:click=move |_| ui.phase.set(Phase::Connect { error: None })>"Accounts"</button>
                </p>
            })}
        </Card>
    }
    .into_any()
}

thread_local! {
    /// When the window last asked the app for a session ([`OnThisComputer`]).
    static LAST_LOCAL_SESSION: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// Notes only on this computer: nothing to sign in to — the window gets
/// its session from the app ([`state::desktop_local_sign_in`]), again by
/// itself after one ended. Not over and over, though: one that ended a
/// moment after the last is shown, with a way to try again.
#[component]
fn OnThisComputer(error: Option<String>) -> impl IntoView {
    let failed = RwSignal::new(None::<String>);
    let open = move || {
        LAST_LOCAL_SESSION.with(|t| t.set(js_sys::Date::now()));
        failed.set(None);
        leptos::task::spawn_local(async move {
            if let Err(e) = state::desktop_local_sign_in().await {
                failed.set(Some(e));
            }
        });
    };
    let just_now = LAST_LOCAL_SESSION.with(|t| js_sys::Date::now() - t.get() < 10_000.0);
    if just_now {
        failed.set(Some(error.unwrap_or_else(|| {
            "Your notes on this computer couldn't be opened.".into()
        })));
    } else {
        open();
    }
    view! {
        <Show when=move || failed.get().is_some() fallback=|| view! { <Loading/> }>
            <Card>
                <h1>"Your notes on this computer"</h1>
                <div class="alert error">{move || failed.get()}</div>
                <button class="btn primary" on:click=move |_| open()>"Try again"</button>
                <AccountsButton/>
            </Card>
        </Show>
    }
}

/// The desktop app's accounts screen ([`super::accounts`]): the first
/// screen while there is none yet (where the notes are: on a server, or
/// only on this computer), else the list and adding one.
#[component]
pub fn Connect(error: Option<String>) -> impl IntoView {
    let first = crate::desktop::accounts().is_empty();
    // The open account's server can't be reached: try it again, or another.
    let stuck = error.is_some();
    view! {
        <Card>
            <h1>{if first { "Where are your notes?" } else { "Accounts" }}</h1>
            {error.map(|e| view! {
                <div class="alert error cant-reach">{e}</div>
                <button class="btn primary try-again" on:click=move |_| { let _ = util::window().location().reload(); }>"Try again"</button>
            })}
            {(!first).then(|| view! {
                <super::accounts::AccountsList/>
                <h2>"Add an account"</h2>
            })}
            <super::accounts::AddAccount first/>
            // Back: the open account again, from the start (its server
            // asked again, its session found again).
            {(!first && !stuck).then(|| view! {
                <button class="btn ghost" on:click=move |_| { let _ = util::window().location().reload(); }>"Back"</button>
            })}
        </Card>
    }
}

/// What the offer to add notekpr to the home screen shows.
#[derive(Clone, Copy, PartialEq)]
enum Offer {
    Asking,
    /// No dialog from the browser: how to do it by hand.
    Steps,
    Added,
    Hidden,
}

/// On a phone's sign-in screen: add notekpr to the home screen first, then
/// sign in from its icon ([`homescreen`]). One tap on Android, the steps on
/// an iPhone.
#[component]
fn HomeScreenOffer(phone: homescreen::Phone) -> impl IntoView {
    let shown = RwSignal::new(Offer::Asking);
    let hide = move |_| {
        storage::hide_home_screen_offer();
        shown.set(Offer::Hidden);
    };
    let add = move |_| match homescreen::open_dialog() {
        Some(choice) => leptos::task::spawn_local(async move {
            match choice.await {
                homescreen::Choice::Added => {
                    storage::hide_home_screen_offer();
                    shown.set(Offer::Added);
                }
                homescreen::Choice::NotAdded => {}
                homescreen::Choice::Failed => shown.set(Offer::Steps),
            }
        }),
        None => shown.set(Offer::Steps),
    };
    let steps = move || {
        match phone {
        homescreen::Phone::Apple => view! {
            <ol class="steps">
                <li>"Tap "<b>"Share"</b>" "<Icon name="share-up"/>". In Safari it may be under "<b>"⋯"</b>" first."</li>
                <li>"Tap "<b>"Add to Home Screen"</b>". Not there? "<b>"Edit Actions"</b>" at the end of the list adds it."</li>
                <li>"Keep "<b>"Open as Web App"</b>" on, and tap "<b>"Add"</b>"."</li>
                <li>"Open notekpr from its new icon, and sign in there."</li>
            </ol>
        }
        .into_any(),
        homescreen::Phone::Android => view! {
            <ol class="steps">
                <li>"Open the browser's menu: "<b>"⋮"</b>"."</li>
                <li>"Tap "<b>"Add to Home screen"</b>", then "<b>"Install"</b>". Some browsers say "<b>"Install app"</b>" in the menu."</li>
                <li>"Open notekpr from its new icon, and sign in there."</li>
            </ol>
        }
        .into_any(),
    }
    };
    view! {
        <Show when=move || shown.get() != Offer::Hidden>
            <div class="home-offer">
                <div class="home-offer-title"><Icon name="phone"/>" Add notekpr to your home screen"</div>
                {move || match shown.get() {
                    Offer::Asking => view! {
                        <p>"It opens from its own icon, like any other app. Add it first, then sign in there."</p>
                        <div class="row">
                            <button class="btn primary small" on:click=add>"Add to home screen"</button>
                            <button class="btn ghost small" on:click=hide>"Not now"</button>
                        </div>
                    }
                    .into_any(),
                    Offer::Steps => view! {
                        {steps()}
                        <div class="row"><button class="btn ghost small" on:click=hide>"Done"</button></div>
                    }
                    .into_any(),
                    Offer::Added => view! {
                        <p>"Added. Open notekpr from its icon on your home screen, and sign in there."</p>
                    }
                    .into_any(),
                    Offer::Hidden => ().into_any(),
                }}
            </div>
        </Show>
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
    let here = crate::desktop::this_computer();
    view! {
        <Card>
            <h1>{format!("Welcome, {name}!")}</h1>
            {if here {
                view! { <p>"Before you start, notekpr creates your personal encryption keys on this computer. Your notes are kept encrypted with them, only here."</p> }.into_any()
            } else {
                view! { <p>"Before you start, notekpr creates your personal encryption keys on this device. Your notes are encrypted with them before anything is uploaded — not even the server administrator can read them."</p> }.into_any()
            }}
            <ul class="bullets">
                <li><Icon name="lock"/>{if here { " Keys are created and kept on this computer." } else { " Keys are created and kept on your devices." }}</li>
                <li><Icon name="key"/>" You'll get a recovery key to keep somewhere safe."</li>
                {(!here).then(|| view! { <li><Icon name="device"/>" Add more devices later by approving them from this one."</li> })}
            </ul>
            <button class="btn primary big" disabled=move || busy.get() on:click=move |_| {
                busy.set(true);
                leptos::task::spawn_local(async move { state::setup_keys().await; busy.set(false); });
            }>{move || if busy.get() { "Creating keys…" } else { "Create my keys" }}</button>
            {(!here).then(|| view! {
                <button class="btn ghost" on:click=move |_| leptos::task::spawn_local(state::sign_out())>"Sign out"</button>
            })}
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
            {if crate::desktop::this_computer() {
                "If the app loses the keys it keeps for its window, this key is the only way to open your notes again. Nobody can reset it for you."
            } else {
                "If you lose all your devices, this key is the only way to get your notes back. Nobody — including the server administrator — can reset it for you."
            }}
            <div class="recovery-key" aria-label="Recovery key">{key.clone()}</div>
            <div class="row">
                <button class="btn" on:click=move |_| { util::copy_to_clipboard(&k1); state::success("Copied to clipboard"); }><Icon name="copy"/>" Copy"</button>
                <button class="btn" on:click=move |_| {
                    let text = format!("notekpr recovery key\n\nAccount: {}\nServer: {}\n\n{}\n\nKeep this somewhere safe and private (a password manager or a printed copy). Anyone with this key and access to your account can read your notes.\n",
                        ui().user.get_untracked().map(|u| u.display_name).unwrap_or_default(),
                        server_shown(), k2);
                    util::download("notekpr-recovery-key.txt", "text/plain", text.as_bytes());
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
    // Notes only on this computer: no other device can approve this one.
    let here = crate::desktop::this_computer();
    let show_recovery = RwSignal::new(here);
    let has_pending = pending.is_some();
    view! {
        <Card>
            <h1>{format!("Add {}", crate::desktop::here())}</h1>
            {error.map(|e| view! { <div class="alert error">{e}</div> })}
            {match pending {
                Some((_, code)) => view! {
                    <p>{format!("Open notekpr on a device that is already set up: a bar at the top of the page asks you to approve {} (it is also in Settings → Devices). Check that it shows this code:", crate::desktop::here())}</p>
                    <div class="verify-code">{code}</div>
                    <p class="muted small"><span class="spinner inline"></span>" Waiting for approval…"</p>
                }.into_any(),
                None if here => view! {
                    <p>"The keys the app kept for its window are gone, and your notes are encrypted with them. Your recovery key opens them again:"</p>
                }.into_any(),
                None => view! {
                    <p>"Your notes are encrypted with keys that live on your other devices. Choose how to unlock them here:"</p>
                    <button class="btn primary big" disabled=move || busy.get() on:click=move |_| {
                        busy.set(true);
                        leptos::task::spawn_local(async move { state::request_approval().await; busy.set(false); });
                    }><Icon name="device"/>" Approve from another device"</button>
                }.into_any(),
            }}
            {(!here).then(|| view! { <div class="divider"><span>"or"</span></div> })}
            <Show when=move || show_recovery.get() fallback=move || view! {
                <button class="btn ghost" on:click=move |_| show_recovery.set(true)><Icon name="key"/>" Use my recovery key"</button>
            }>
                <form class="stack" on:submit=move |ev| {
                    ev.prevent_default();
                    busy.set(true);
                    let k = key.get_untracked();
                    leptos::task::spawn_local(async move { state::recover(k).await; busy.set(false); });
                }>
                    <textarea class="input mono" rows="3" placeholder="XXXXX-XXXXX-…" use:focus_on_mount
                        prop:value=move || key.get() on:input=move |ev| key.set(event_target_value(&ev))></textarea>
                    <button class="btn primary" type="submit" disabled=move || busy.get()>"Unlock with recovery key"</button>
                </form>
            </Show>
            {has_pending.then(|| view! { <p class="small muted">"Lost all your devices? Use your recovery key instead."</p> })}
            {(!here).then(|| view! {
                <button class="btn ghost" on:click=move |_| leptos::task::spawn_local(state::sign_out())>"Sign out"</button>
            })}
            <AccountsButton/>
        </Card>
    }
}

/// The server, as the recovery key's file names it.
fn server_shown() -> String {
    if crate::desktop::this_computer() {
        "none, only on this computer (the desktop app)".into()
    } else if let Some(s) = crate::desktop::server() {
        s
    } else {
        util::window().location().host().unwrap_or_default()
    }
}

pub fn phase_view(p: Phase) -> AnyView {
    match p {
        Phase::Loading => view! { <Loading/> }.into_any(),
        Phase::Connect { error } => view! { <Connect error=error/> }.into_any(),
        Phase::Fatal(m) => view! { <Fatal msg=m/> }.into_any(),
        Phase::SignedOut { error } => view! { <Login error=error/> }.into_any(),
        Phase::Setup => view! { <Setup/> }.into_any(),
        Phase::ShowRecovery { key } => view! { <ShowRecovery key=key/> }.into_any(),
        Phase::Enroll { pending, error } => {
            view! { <Enroll pending=pending error=error/> }.into_any()
        }
        Phase::DesktopSetup { again } => {
            view! { <super::desktop_setup::DesktopSetup again/> }.into_any()
        }
        Phase::Ready => view! { <crate::views::Shell/> }.into_any(),
    }
}
