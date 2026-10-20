pub mod auth;
pub mod dialogs;
pub mod home;
pub mod icons;
pub mod note;
pub mod panels;
pub mod quick;
pub mod settings;
pub mod sidebar;
pub mod tree;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::state::{self, Modal, Route, ToastKind, ViewMode, ui};
use crate::util;
use crate::ws::ConnState;
use icons::Icon;

#[component]
pub fn App() -> impl IntoView {
    let ui = ui();
    install_shortcuts();
    install_menu_closer();
    Effect::new(move |_| {
        let theme = ui.ui_prefs.get().theme;
        if let Some(root) = util::document().document_element() {
            let _ = root.set_attribute("data-theme", &theme);
        }
    });
    view! { {move || auth::phase_view(ui.phase.get())} }
}

#[component]
pub fn Shell() -> impl IntoView {
    let ui = ui();
    view! {
        <div class="app" class:drawer=move || ui.sidebar.get()>
            <sidebar::Sidebar/>
            <div class="scrim" on:click=move |_| ui.sidebar.set(false)></div>
            <main class="main">
                <Banners/>
                {move || match ui.route.get() {
                    Route::Home => view! { <home::Home/> }.into_any(),
                    Route::Vault(v) => view! { <home::VaultHome vault=v/> }.into_any(),
                    Route::Note(v, n) => view! { <note::NoteView vault=v note=n/> }.into_any(),
                    Route::Trash(v) => view! { <home::Trash vault=v/> }.into_any(),
                    Route::Settings(tab) => view! { <settings::Settings tab=tab/> }.into_any(),
                    Route::Device(code) => view! { <settings::DeviceCodePage code=code/> }.into_any(),
                    Route::Shared(v, n) => view! { <note::SharedNoteView vault=v note=n/> }.into_any(),
                    Route::Quick => view! { <quick::QuickPage/> }.into_any(),
                }}
            </main>
            <dialogs::ModalHost/>
        </div>
        <Toasts/>
    }
}

#[component]
fn Banners() -> impl IntoView {
    let ui = ui();
    view! {
        {move || {
            let devs = ui.pending_devices.get();
            (!devs.is_empty()).then(|| {
                let d = devs[0].clone();
                let n = devs.len();
                view! {
                    <div class="banner">
                        <Icon name="device"/>
                        <span>{if n == 1 { format!("“{}” is asking to access your notes.", d.name) } else { format!("{n} devices are waiting for approval.") }}</span>
                        <button class="btn small primary" on:click=move |_| ui.modal.set(Some(Modal::ApproveDevice(d.clone())))>"Review"</button>
                    </div>
                }
            })
        }}
        {move || ui.warning.get().map(|w| view! {
            <div class="banner warn"><Icon name="alert"/><span>{w}</span>
                <button class="btn small ghost" on:click=move |_| ui.warning.set(None)>"Dismiss"</button></div>
        })}
        {move || (ui.server.get().is_some_and(|s| s.dev_mode)).then(|| view! {
            <div class="devstrip" title="The server runs in development mode: anyone can sign in as anyone.">"DEV MODE"</div>
        })}
    }
}

#[component]
fn Toasts() -> impl IntoView {
    let ui = ui();
    view! {
        <div class="toasts" role="status" aria-live="polite">
            <For each=move || ui.toasts.get() key=|t| t.id let:t>
                <div class=move || match t.kind { ToastKind::Error => "toast error", ToastKind::Success => "toast success", ToastKind::Info => "toast" }>
                    {t.text.clone()}
                </div>
            </For>
        </div>
    }
}

/// Connection / sync status indicator.
#[component]
pub fn SyncStatus() -> impl IntoView {
    let ui = ui();
    view! {
        <span class="sync" title=move || match (ui.conn.get(), ui.pending.get()) {
            (ConnState::Open, 0) => "All changes synced (end-to-end encrypted)".to_string(),
            (ConnState::Open, n) => format!("Syncing changes to {n} document(s)…"),
            (ConnState::Connecting, _) => "Connecting…".to_string(),
            (ConnState::Offline, 0) => "Offline — changes will sync when you reconnect".to_string(),
            (ConnState::Offline, n) => format!("Offline — changes to {n} document(s) are kept and will sync when you reconnect"),
        }>
            {move || match (ui.conn.get(), ui.pending.get()) {
                (ConnState::Open, 0) => view! { <Icon name="cloud" class="ok"/><span class="lbl">"Synced"</span> }.into_any(),
                (ConnState::Open, _) => view! { <Icon name="refresh" class="spin"/><span class="lbl">"Saving"</span> }.into_any(),
                (ConnState::Connecting, _) => view! { <Icon name="refresh" class="spin"/><span class="lbl">"Connecting"</span> }.into_any(),
                (ConnState::Offline, n) => view! { <Icon name="cloud-off" class="warn"/><span class="lbl">{if n > 0 { format!("Offline · {n}") } else { "Offline".into() }}</span> }.into_any(),
            }}
        </span>
    }
}

fn in_text_input(e: &web_sys::KeyboardEvent) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|el| {
            let tag = el.tag_name().to_lowercase();
            tag == "input"
                || tag == "textarea"
                || el
                    .closest("[contenteditable=true]")
                    .ok()
                    .flatten()
                    .is_some()
        })
}

/// Elements a press may land on without closing the open menus: the menus
/// themselves and the buttons that open them.
const MENU_PARTS: &str = ".menu, .tb-drop, .vault-switch, .user-btn, .tree-more";

fn install_menu_closer() {
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |e: web_sys::Event| {
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| el.closest(MENU_PARTS).ok().flatten().is_some());
        if !inside {
            ui().close_menus.update(|n| *n += 1);
        }
    }) as Box<dyn FnMut(_)>);
    let _ = util::document()
        .add_event_listener_with_callback("pointerdown", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// `use:focus_on_mount` — focus an input inserted after the page loaded (the
/// `autofocus` attribute only works for the initial page).
pub fn focus_on_mount(el: web_sys::Element) {
    util::set_timeout(0, move || {
        if let Some(h) = el.dyn_ref::<web_sys::HtmlElement>() {
            let _ = h.focus();
        }
    });
}

/// Close the menu `open` controls on a click outside any menu (or Escape).
pub fn close_on_outside(open: RwSignal<bool>) {
    let ui = ui();
    Effect::new(move |_| {
        ui.close_menus.track();
        open.set(false);
    });
}

/// Ctrl+Alt+Q (⌘+Alt+Q on macs): a new quick note. On macs Alt changes the
/// key ("œ"), so the physical key counts there; elsewhere the key must be a
/// "q", so AltGr+Q (which types "@" on some layouts) never triggers it.
fn is_quick_note_key(e: &web_sys::KeyboardEvent) -> bool {
    if !e.alt_key() || e.shift_key() {
        return false;
    }
    let q = e.key().eq_ignore_ascii_case("q");
    if e.meta_key() && !e.ctrl_key() {
        return q || e.code() == "KeyQ";
    }
    e.ctrl_key() && !e.meta_key() && q && !e.get_modifier_state("AltGraph")
}

fn install_shortcuts() {
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |e: web_sys::KeyboardEvent| {
        let ui = ui();
        // Shortcuts the editor already handled (e.g. Ctrl+K = link) stay local.
        if ui.phase.get_untracked() != state::Phase::Ready || e.default_prevented() {
            return;
        }
        let m = e.ctrl_key() || e.meta_key();
        let key = e.key();
        let route = ui.route.get_untracked();
        if is_quick_note_key(&e) {
            e.prevent_default();
            crate::quick::new_and_open();
            return;
        }
        match (m, e.shift_key(), e.alt_key(), key.as_str()) {
            (true, false, false, "k") | (true, false, false, "p") | (true, false, false, "o") => {
                e.prevent_default();
                ui.modal.set(Some(Modal::QuickSwitch));
            }
            (true, true, _, "F") | (true, true, _, "f") => {
                e.prevent_default();
                ui.modal.set(Some(Modal::Search(String::new())));
            }
            (true, false, true, "n") | (true, false, true, "N") => {
                e.prevent_default();
                if let Some(v) = route.vault() {
                    let folder = route
                        .note()
                        .and_then(|(v, n)| state::step(|c| c.entry(v, n)))
                        .map(|e| coolpsonotes_core::paths::parent(&e.path).to_string())
                        .unwrap_or_default();
                    crate::actions::new_note(v, &folder);
                }
            }
            (true, false, false, "e") => {
                if route.note().is_some() {
                    e.prevent_default();
                    ui.mode.update(|m| {
                        *m = if *m == ViewMode::Reading {
                            ViewMode::Live
                        } else {
                            ViewMode::Reading
                        }
                    });
                }
            }
            (true, false, false, "\\") => {
                e.prevent_default();
                state::set_ui_prefs(|p| p.right_panel = !p.right_panel);
            }
            (true, false, false, "s") => {
                // Everything saves automatically; don't open the browser's save dialog.
                e.prevent_default();
                state::step(state::flush_now);
                state::info("Changes are saved automatically.");
            }
            (true, false, false, "/") => {
                e.prevent_default();
                ui.modal.set(Some(Modal::Help));
            }
            (false, _, false, "?") if !in_text_input(&e) => {
                e.prevent_default();
                ui.modal.set(Some(Modal::Help));
            }
            (false, false, false, "Escape") => {
                ui.close_menus.update(|n| *n += 1);
                if ui.modal.get_untracked().is_some() {
                    ui.modal.set(None);
                } else if ui.sidebar.get_untracked() {
                    ui.sidebar.set(false);
                }
            }
            _ => {}
        }
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    let _ =
        util::document().add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    cb.forget();
}

pub fn avatar(name: &str, user_id: &str) -> impl IntoView + use<> {
    let color = util::user_color(user_id);
    let initials = util::initials(name);
    view! { <span class="avatar" style=format!("--c:{color}") title=name.to_string()>{initials}</span> }
}
