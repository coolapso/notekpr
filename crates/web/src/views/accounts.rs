//! The desktop app's accounts (docs/desktop.md "Accounts"): one per server
//! it was added for, and *On this computer*. The same list on the first
//! screen, behind the person's name in the sidebar, in *Settings → Desktop
//! app*, and where a server can't be reached. Picking one starts the app
//! again on it; each keeps its own sign-in, notes and Markdown folder.

use leptos::prelude::*;

use crate::desktop::{self, Account};
use crate::views::icons::Icon;

fn icon(a: &Account) -> &'static str {
    if a.server.is_some() {
        "users"
    } else {
        "device"
    }
}

/// After asking the app to open an account: it starts again on it, unless
/// that one is open already — then the window starts again on it.
fn opened(r: Result<(), String>, err: RwSignal<Option<String>>) {
    match r {
        Ok(()) => {
            let _ = crate::util::window().location().reload();
        }
        Err(e) if e == "cancelled" => {}
        Err(e) => err.set(Some(e)),
    }
}

/// Open another account: the app starts again on it.
fn switch(id: String, err: RwSignal<Option<String>>) {
    leptos::task::spawn_local(async move {
        opened(desktop::switch_account(&id).await, err);
    });
}

/// The accounts, the open one marked; *Remove* beside the others on
/// servers.
#[component]
pub fn AccountsList() -> impl IntoView {
    let err = RwSignal::new(None::<String>);
    let accounts = RwSignal::new(desktop::accounts());
    let remove = move |id: String| {
        leptos::task::spawn_local(async move {
            match desktop::remove_account(&id).await {
                Ok(()) => {
                    if let Ok(i) = desktop::refresh().await {
                        accounts.set(i.accounts);
                    }
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => err.set(Some(e)),
            }
        });
    };
    view! {
        {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
        <ul class="accounts">
            {move || accounts.get().into_iter().map(|a| {
                let (id, id2) = (a.id.clone(), a.id.clone());
                let open = a.open;
                let removable = !open && a.server.is_some();
                view! {
                    <li class="account" class:open=open>
                        <button class="account-open" disabled=open on:click=move |_| switch(id.clone(), err)>
                            <Icon name=icon(&a)/>
                            <span class="grow ellipsis">{a.label()}</span>
                            {open.then(|| view! { <span class="muted small">"open"</span> })}
                        </button>
                        {removable.then(|| view! {
                            <button class="btn small ghost remove-account" on:click=move |_| remove(id2.clone())>"Remove"</button>
                        })}
                    </li>
                }
            }).collect_view()}
        </ul>
    }
}

/// The accounts as menu items (the sidebar's menu behind the person's
/// name): pick one to open it.
#[component]
pub fn AccountsMenu() -> impl IntoView {
    let err = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        if let Some(e) = err.get() {
            crate::state::error(e);
        }
    });
    view! {
        <div class="menu-head muted small">"Accounts"</div>
        {desktop::accounts().into_iter().map(|a| {
            let id = a.id.clone();
            let open = a.open;
            view! {
                <button class="account-item" class:open=open on:click=move |_| if !open { switch(id.clone(), err) }>
                    <Icon name=if open { "check" } else { icon(&a) }/>
                    <span class="ellipsis">{a.label()}</span>
                </button>
            }
        }).collect_view()}
        <button class="add-account" on:click=move |_| crate::state::ui().modal.set(Some(crate::state::Modal::Accounts))>
            <Icon name="plus"/>" Add an account…"
        </button>
    }
}

/// Add an account: on a server (the app asks first, then checks it), or
/// *Only on this computer* while there is no such account. `first`: the
/// app's first screen, where nothing is chosen yet.
#[component]
pub fn AddAccount(#[prop(optional)] first: bool) -> impl IntoView {
    let here = desktop::accounts().iter().any(|a| a.server.is_none());
    let url = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let u = url.get_untracked();
        if u.trim().is_empty() {
            return;
        }
        busy.set(true);
        err.set(None);
        leptos::task::spawn_local(async move {
            // The app starts again on it.
            opened(desktop::add_server(u.trim()).await, err);
            busy.set(false);
        });
    };
    let local = move |_| {
        busy.set(true);
        err.set(None);
        leptos::task::spawn_local(async move {
            opened(desktop::use_this_computer().await, err);
            busy.set(false);
        });
    };
    view! {
        {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
        {if first {
            view! { <h2>"On a notekpr server"</h2> }.into_any()
        } else {
            view! { <h3>"On a notekpr server"</h3> }.into_any()
        }}
        <p class="muted">"The address you open notekpr at in a browser. Your notes reach your other devices, and you can share them."</p>
        <form class="connect" on:submit=submit>
            <input class="input server-address" type="url" inputmode="url" placeholder="https://notes.example.com"
                prop:value=move || url.get() on:input=move |ev| url.set(event_target_value(&ev))/>
            <button class="btn primary" type="submit" disabled=move || busy.get()>{if first { "Connect" } else { "Add" }}</button>
        </form>
        {(!here).then(|| view! {
            <div class="divider"><span>"or"</span></div>
            {if first {
                view! { <h2>"Only on this computer"</h2> }.into_any()
            } else {
                view! { <h3>"Only on this computer"</h3> }.into_any()
            }}
            <p class="muted">"No server and no account: your notes stay on this computer, encrypted. Other devices and sharing need a server."</p>
            <button class="btn this-computer" disabled=move || busy.get() on:click=local>"Keep them on this computer"</button>
        })}
    }
}
