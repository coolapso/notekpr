pub mod ask;
pub mod attachment;
pub mod auth;
pub mod comments;
pub mod dialogs;
pub mod drawing;
pub mod graph;
pub mod home;
pub mod icons;
pub mod library;
pub mod mentions;
pub mod note;
pub mod panels;
pub mod quick;
pub mod settings;
pub mod sidebar;
pub mod suggest;
pub mod tree;
pub mod width;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::state::{self, Modal, Route, ToastKind, ViewMode, ui};
use crate::util;
use crate::ws::ConnState;
use icons::Icon;
use notekpr_core::protocol::ReadOnly;

#[component]
pub fn App() -> impl IntoView {
    let ui = ui();
    install_shortcuts();
    install_menu_closer();
    install_code_copy();
    comments::install_reading_selection();
    install_day_change();
    crate::viewport::install();
    Effect::new(move |_| {
        let prefs = ui.ui_prefs.get();
        if let Some(root) = util::document().document_element() {
            let _ = root.set_attribute("data-theme", &prefs.theme);
            let classes = root.class_list();
            let _ = classes.toggle_with_force("line-numbers", prefs.line_numbers);
            let _ = classes.toggle_with_force(
                "relative-numbers",
                prefs.line_numbers && prefs.relative_numbers,
            );
            let _ = classes.toggle_with_force("no-code-numbers", !prefs.code_numbers);
            let _ = classes.toggle_with_force("no-code-guides", !prefs.code_guides);
            let _ = classes.toggle_with_force("no-code-wrap", !prefs.code_wrap);
        }
    });
    view! {
        {move || auth::phase_view(ui.phase.get())}
        <ask::AskHost/>
    }
}

#[component]
pub fn Shell() -> impl IntoView {
    let ui = ui();
    let banners = NodeRef::<leptos::html::Div>::new();
    watch_banners(banners);
    // On a phone the side panel covers the note: another page closes it.
    Effect::new(move |seen: Option<()>| {
        ui.route.track();
        if seen.is_some() {
            panels::close_over_note();
        }
    });
    view! {
        <div class="app" class:drawer=move || ui.sidebar.get()>
            <sidebar::Sidebar/>
            <div class="scrim" on:click=move |_| ui.sidebar.set(false)></div>
            <div class="main-col">
                // Above the page, not in it: they stay in view however far it
                // scrolls.
                <div class="banners" node_ref=banners><Banners/></div>
                <main class="main">
                    {move || match ui.route.get() {
                        Route::Home => view! { <home::Home/> }.into_any(),
                        // The quick notes' vault is shown as the quick notes.
                        Route::Vault(v) | Route::Trash(v) if state::step(|c| c.is_quick_notes(&v)) => view! { <quick::QuickPage/> }.into_any(),
                        Route::Vault(v) => view! { <home::VaultHome vault=v/> }.into_any(),
                        Route::Note(v, n) => view! { <drawing::Document vault=v note=n/> }.into_any(),
                        Route::Trash(v) => view! { <home::Trash vault=v/> }.into_any(),
                        Route::Settings(tab) => view! { <settings::Settings tab=tab/> }.into_any(),
                        Route::Device(code) => view! { <settings::DeviceCodePage code=code/> }.into_any(),
                        Route::Shared(v, n) => view! { <note::SharedNoteView vault=v note=n/> }.into_any(),
                        Route::Quick => view! { <quick::QuickPage/> }.into_any(),
                        Route::Drawings(v) => view! { <drawing::DrawingsPage vault=v/> }.into_any(),
                        Route::Graph(v) => view! { <graph::GraphPage vault=v/> }.into_any(),
                    }}
                </main>
            </div>
            <dialogs::ModalHost/>
        </div>
        <Toasts/>
    }
}

/// How tall the banners are, as `--banners-h` on the page: what is sized to
/// the window below them (the side panel) leaves them room.
fn watch_banners(banners: NodeRef<leptos::html::Div>) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    type Observer = (web_sys::ResizeObserver, Closure<dyn FnMut(js_sys::Array)>);
    let observer: Rc<RefCell<Option<Observer>>> = Rc::default();
    let mount = observer.clone();
    Effect::new(move |_| {
        let Some(el) = banners.get() else {
            return;
        };
        if mount.borrow().is_some() {
            return;
        }
        let target = el.clone();
        let on_resize = Closure::wrap(Box::new(move |_: js_sys::Array| {
            let h = target.get_bounding_client_rect().height();
            util::set_root_var("--banners-h", &format!("{h}px"));
        }) as Box<dyn FnMut(js_sys::Array)>);
        if let Ok(o) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
            o.observe(&el);
            *mount.borrow_mut() = Some((o, on_resize));
        }
    });
    let kept = StoredValue::new_local(observer);
    on_cleanup(move || {
        kept.try_with_value(|o| {
            if let Some((o, _)) = o.borrow_mut().take() {
                o.disconnect();
            }
        });
        util::set_root_var("--banners-h", "0px");
    });
}

#[component]
fn Banners() -> impl IntoView {
    let ui = ui();
    // A device waits for an answer, maybe in a tab out of sight: its tab's
    // title counts them too.
    Effect::new(move |_| {
        let n = ui.pending_devices.with(Vec::len);
        util::document().set_title(&if n > 0 {
            format!("({n}) notekpr")
        } else {
            "notekpr".into()
        });
    });
    on_cleanup(|| util::document().set_title("notekpr"));
    view! {
        {move || {
            let devs = ui.pending_devices.get();
            (!devs.is_empty()).then(|| {
                let d = devs[0].clone();
                let n = devs.len();
                view! {
                    <div class="banner device-banner" role="alert">
                        <Icon name="device"/>
                        <span>
                            <b>{if n == 1 { "A device is waiting for your approval. ".to_string() } else { format!("{n} devices are waiting for your approval. ") }}</b>
                            {if n == 1 { format!("“{}” is asking to access your notes.", d.name) } else { "Review them one at a time.".into() }}
                        </span>
                        <button class="btn small" on:click=move |_| ui.modal.set(Some(Modal::ApproveDevice(d.clone())))>"Review"</button>
                    </div>
                }
            })
        }}
        {move || ui.outdated.get().then(|| view! {
            <div class="banner warn outdated-banner"><Icon name="alert"/>
                <span>"This page is out of date: the server needs a newer version of notekpr. Your unsent edits are kept in this browser; reload to get the new version."</span>
                <button class="btn small primary" on:click=|_| { let _ = util::window().location().reload(); }>"Reload"</button></div>
        })}
        {move || ui.read_only.get().map(|why| view! {
            <div class="banner warn read-only-banner"><Icon name="cloud-off"/>
                <span>{match why {
                    ReadOnly::Manual | ReadOnly::Other => "The server is read-only for now.",
                    ReadOnly::StorageFull => "The server's storage is full.",
                }}" Your edits are kept in this browser and sent as soon as it accepts changes again."</span></div>
        })}
        <VaultFullBanner/>
        {move || ui.warning.get().map(|w| view! {
            <div class="banner warn"><Icon name="alert"/><span>{w}</span>
                <button class="btn small ghost" on:click=move |_| ui.warning.set(None)>"Dismiss"</button></div>
        })}
        {move || ui.server.get().filter(|s| s.dev_mode).map(|s| match s.demo_resets_at {
            Some(at) => {
                let at = util::clock_time(at);
                view! {
                    <div class="devstrip" title=format!("A demo: anyone can sign in as anyone, and everything is deleted at {at}, when it starts over.")>
                        {format!("DEMO · resets at {at}")}</div>
                }.into_any()
            }
            None => view! {
                <div class="devstrip" title="The server runs in development mode: anyone can sign in as anyone.">"DEV MODE"</div>
            }.into_any(),
        })}
    }
}

/// Vaults at their storage limit: their edits wait in this browser.
#[component]
fn VaultFullBanner() -> impl IntoView {
    let ui = ui();
    move || {
        let full = ui.full.get();
        ui.vaults.track();
        // Ours by name (quick notes as such: they have no settings to show);
        // the others are vaults of notes shared with us.
        let ours: Vec<(String, Option<String>)> = state::step(|c| {
            full.iter()
                .filter(|v| c.vault_infos.contains_key(*v))
                .map(|v| (v.clone(), (!c.is_quick_notes(v)).then(|| c.vault_name(v))))
                .collect()
        });
        let text = match ours.as_slice() {
            [] if full.is_empty() => return None,
            _ if ui.server_full.get() => "The server's storage is full: your edits are kept in this browser and sent once there's room. Deleting things for good makes room, or ask an administrator for more.".to_string(),
            [] => "The vault of a note shared with you is full: your edits of it are kept in this browser and sent once it has room.".to_string(),
            [(_, None)] => "Your personal storage is full: your quick notes and saved shapes are kept in this browser and sent once there's room. Deleting quick notes or saved shapes makes room, or ask an administrator for more.".to_string(),
            [(_, Some(name))] => format!("“{name}” is full: your edits there are kept in this browser and sent once it has room."),
            many => format!(
                "{} vaults are full ({}): your edits there are kept in this browser and sent once they have room.",
                many.len(),
                many.iter()
                    .map(|(_, n)| n.as_ref().map_or("your personal storage".into(), |n| format!("“{n}”")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        // Where a vault's limit shows (none to show for the server's).
        let first = ours
            .iter()
            .find_map(|(v, n)| n.is_some().then(|| v.clone()))
            .filter(|_| !ui.server_full.get());
        Some(view! {
            <div class="banner warn vault-full-banner"><Icon name="cloud-off"/>
                <span>{text}</span>
                {first.map(|v| view! {
                    <button class="btn small" on:click=move |_| ui.modal.set(Some(Modal::VaultSettings(v.clone())))>"Details"</button>
                })}
            </div>
        })
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
            (_, n) if ui.outdated.get() => format!("This page is out of date: changes to {n} document(s) are kept in this browser and sync after a reload"),
            (ConnState::Open, n) if ui.read_only.get().is_some() => format!("The server is read-only: changes to {n} document(s) are kept and will sync when it accepts changes again"),
            (ConnState::Open, n) if n > 0 && !ui.full.with(|f| f.is_empty()) => format!("A vault is full: changes to {n} document(s) are kept and will sync once it has room"),
            (ConnState::Open, 0) => "All changes synced (end-to-end encrypted)".to_string(),
            (ConnState::Open, n) => format!("Syncing changes to {n} document(s)…"),
            (ConnState::Connecting, _) => "Connecting…".to_string(),
            (ConnState::Offline, 0) => "Offline — changes will sync when you reconnect".to_string(),
            (ConnState::Offline, n) => format!("Offline — changes to {n} document(s) are kept and will sync when you reconnect"),
        }>
            {move || match (ui.conn.get(), ui.pending.get()) {
                (_, n) if ui.outdated.get() => view! { <Icon name="cloud-off" class="warn"/><span class="lbl">{if n > 0 { format!("Out of date · {n}") } else { "Out of date".into() }}</span> }.into_any(),
                (ConnState::Open, n) if ui.read_only.get().is_some() => view! { <Icon name="cloud-off" class="warn"/><span class="lbl">{if n > 0 { format!("Read-only · {n}") } else { "Read-only".into() }}</span> }.into_any(),
                (ConnState::Open, n) if n > 0 && !ui.full.with(|f| f.is_empty()) => view! { <Icon name="cloud-off" class="warn"/><span class="lbl">{format!("Vault full · {n}")}</span> }.into_any(),
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
const MENU_PARTS: &str =
    ".menu, .tb-drop, .vault-switch, .user-btn, .tree-more, .cm-pop, .cm-offer";

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

/// The copy buttons of code blocks in reading views (the editor handles its
/// own): copy the block's code.
fn install_code_copy() {
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |e: web_sys::Event| {
        let Some(button) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest(".reading .code-copy").ok().flatten())
        else {
            return;
        };
        let code = button
            .closest(".codeblock")
            .ok()
            .flatten()
            .and_then(|b| b.query_selector("code").ok().flatten());
        if let Some(code) = code {
            util::copy_to_clipboard(&code.text_content().unwrap_or_default());
            crate::editor::view::flash_copied(&button);
        }
    }) as Box<dyn FnMut(_)>);
    let _ = util::document().add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// At midnight a date that said Today says Yesterday: show the open note's
/// dates again (looked at every minute).
fn install_day_change() {
    let day = std::cell::Cell::new(crate::editor::today());
    util::set_interval(60_000, move || {
        let now = crate::editor::today();
        if now == day.replace(now) {
            return;
        }
        ui().content_rev.update(|n| *n += 1);
        if let Some(ed) = state::step(|c| c.editor.as_ref().map(|(_, _, e)| e.clone())) {
            ed.refresh();
        }
    });
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

/// Ctrl+Alt+`letter` (⌘+Alt on macs), e.g. N for a new note. On macs Alt
/// changes the key (a dead key for N), so the physical key counts there;
/// elsewhere the key must be the letter itself, so AltGr+N (which types "ń"
/// on some layouts) never triggers it.
fn is_ctrl_alt_letter(e: &web_sys::KeyboardEvent, letter: char) -> bool {
    if !e.alt_key() || e.shift_key() {
        return false;
    }
    let typed = e.key().eq_ignore_ascii_case(&letter.to_string());
    if e.meta_key() && !e.ctrl_key() {
        return typed || e.code() == format!("Key{}", letter.to_ascii_uppercase());
    }
    e.ctrl_key() && !e.meta_key() && typed && !e.get_modifier_state("AltGraph")
}

/// Alt+`letter` alone (Option on macs), e.g. Q for a quick note — not
/// Ctrl+Alt+Q: ⌘+Option+Q quits the browser on macs. As above, the physical
/// key counts on macs (Option+Q types "œ"), and AltGr (Ctrl+Alt on Windows)
/// never triggers it.
fn is_alt_letter(e: &web_sys::KeyboardEvent, letter: char) -> bool {
    if !e.alt_key()
        || e.ctrl_key()
        || e.meta_key()
        || e.shift_key()
        || e.get_modifier_state("AltGraph")
    {
        return false;
    }
    e.key().eq_ignore_ascii_case(&letter.to_string())
        || (util::is_mac() && e.code() == format!("Key{}", letter.to_ascii_uppercase()))
}

fn install_shortcuts() {
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |e: web_sys::KeyboardEvent| {
        let ui = ui();
        // Shortcuts the editor already handled (e.g. Ctrl+K = link) stay local;
        // a question asked (`crate::ask`) has the keys to itself.
        if ui.phase.get_untracked() != state::Phase::Ready
            || e.default_prevented()
            || ui.asking.with_untracked(Option::is_some)
        {
            return;
        }
        let m = e.ctrl_key() || e.meta_key();
        let key = e.key();
        let route = ui.route.get_untracked();
        if is_alt_letter(&e, 'q') {
            e.prevent_default();
            crate::quick::new_and_open();
            return;
        }
        if is_alt_letter(&e, 'm') && route.any_note().is_some() {
            e.prevent_default();
            crate::views::comments::compose_from_selection();
            return;
        }
        if is_ctrl_alt_letter(&e, 'n') {
            e.prevent_default();
            if let Some(v) = route.vault() {
                let folder = route
                    .note()
                    .and_then(|(v, n)| state::step(|c| c.entry(v, n)))
                    .map(|e| notekpr_core::paths::parent(&e.path).to_string())
                    .unwrap_or_default();
                crate::actions::new_note(v, &folder);
            }
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
            (true, false, false, "e") => {
                // Your own notes and notes shared with you.
                if route.any_note().is_some() {
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
            (true, false, false, "g") => {
                if let Some(v) = route.vault() {
                    e.prevent_default();
                    state::navigate(Route::Graph(v.to_string()));
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
                } else {
                    panels::close_over_note();
                }
            }
            _ => {}
        }
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    let _ =
        util::document().add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// A note's or folder's icon (its picture or emoji), else the `fallback`
/// icon.
pub fn entry_icon(icon: Option<state::EntryIcon>, fallback: &'static str) -> AnyView {
    match icon {
        Some(state::EntryIcon::Text(i)) => {
            view! { <span class="entry-icon" aria-hidden="true">{i}</span> }.into_any()
        }
        Some(state::EntryIcon::Picture(src)) => {
            view! { <img class="entry-icon entry-pic" src=src alt=""/> }.into_any()
        }
        None => view! { <Icon name=fallback/> }.into_any(),
    }
}

/// An icon by a note's title: its emoji, or its picture.
pub fn note_icon(icon: state::EntryIcon) -> AnyView {
    match icon {
        state::EntryIcon::Text(i) => i.into_any(),
        state::EntryIcon::Picture(src) => {
            view! { <img class="entry-pic" src=src alt=""/> }.into_any()
        }
    }
}

/// What identifies a vault: its picture, else its icon, else the generic one.
pub fn vault_icon(v: Option<&state::VaultView>) -> AnyView {
    match v {
        Some(state::VaultView {
            picture: Some(src), ..
        }) => view! { <img class="vault-pic" src=src.clone() alt=""/> }.into_any(),
        Some(state::VaultView { icon: Some(i), .. }) => {
            view! { <span class="vault-emoji" aria-hidden="true">{i.clone()}</span> }.into_any()
        }
        _ => view! { <Icon name="vault"/> }.into_any(),
    }
}

pub fn avatar(name: &str, user_id: &str) -> impl IntoView + use<> {
    let color = util::user_color(user_id);
    let initials = util::initials(name);
    view! { <span class="avatar" style=format!("--c:{color}") title=name.to_string()>{initials}</span> }
}
