//! Modal dialogs.

use super::focus_on_mount;
use leptos::html;
use leptos::prelude::Update;
use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::engine::PinStatus;
use notekpr_core::protocol::*;

use crate::actions;
use crate::state::{self, Modal, Route, step, ui};
use crate::util;
use crate::views::avatar;
use crate::views::icons::Icon;
use crate::{api, knowledge, render};

#[component]
pub(crate) fn Dialog(
    #[prop(into)] title: String,
    #[prop(optional)] wide: bool,
    children: Children,
) -> impl IntoView {
    let ui = ui();
    let label = title.clone();
    let root = NodeRef::<html::Div>::new();
    // Focus moves into the dialog at once, before the next key: a key typed
    // right after opening it (Ctrl+/, then Tab) would otherwise go to the
    // note behind it. An input inside takes the focus next.
    Effect::new(move |_| {
        if let Some(el) = root.get() {
            let inside = util::document()
                .active_element()
                .is_some_and(|a| el.contains(Some(&a)));
            if !inside {
                remember_focus();
                let _ = el.focus();
            }
        }
    });
    view! {
        <div class="modal-back" on:mousedown=move |_| ui.modal.set(None)>
            <div class="modal" class:wide=wide role="dialog" aria-modal="true" aria-label=label tabindex="-1" node_ref=root
                on:mousedown=|ev| ev.stop_propagation()
                on:keydown=move |ev: web_sys::KeyboardEvent| {
                    if ev.key() == "Tab" && let Some(el) = root.get() {
                        trap_focus(&el, &ev);
                    }
                }>
                <header>
                    <h2>{title}</h2>
                    <button class="icon-btn" title="Close" aria-label="Close" on:click=move |_| ui.modal.set(None)><Icon name="x"/></button>
                </header>
                <div class="modal-body">{children()}</div>
            </div>
        </div>
    }
}

/// Keep Tab and Shift+Tab inside the dialog.
fn trap_focus(root: &web_sys::HtmlElement, ev: &web_sys::KeyboardEvent) {
    use wasm_bindgen::JsCast;
    let Ok(list) = root.query_selector_all(
        "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
    ) else {
        return;
    };
    let items: Vec<web_sys::HtmlElement> = (0..list.length())
        .filter_map(|i| list.item(i)?.dyn_into().ok())
        .collect();
    let (Some(first), Some(last)) = (items.first(), items.last()) else {
        return;
    };
    let active = util::document().active_element();
    let at = |e: &web_sys::HtmlElement| {
        active
            .as_ref()
            .is_some_and(|a| a == e.unchecked_ref::<web_sys::Element>())
    };
    let outside = !active.as_ref().is_some_and(|a| root.contains(Some(a)))
        || active
            .as_ref()
            .is_some_and(|a| a == root.unchecked_ref::<web_sys::Element>());
    if ev.shift_key() && (at(first) || outside) {
        ev.prevent_default();
        let _ = last.focus();
    } else if !ev.shift_key() && (at(last) || outside) {
        ev.prevent_default();
        let _ = first.focus();
    }
}

thread_local! {
    /// What had the focus before a dialog opened; it gets it back afterwards.
    static RETURN_FOCUS: std::cell::RefCell<Option<web_sys::HtmlElement>> = const { std::cell::RefCell::new(None) };
}

/// Remember what has the focus, for when the dialogs close: what had it
/// before the first one opened (the dialog and [`ModalHost`] may come to it
/// in either order, so never something in a dialog).
fn remember_focus() {
    use wasm_bindgen::JsCast;
    RETURN_FOCUS.with(|r| {
        let mut r = r.borrow_mut();
        if r.is_none() {
            *r = util::document()
                .active_element()
                .filter(|a| a.closest(".modal").ok().flatten().is_none())
                .and_then(|a| a.dyn_into().ok());
        }
    });
}

#[component]
pub fn ModalHost() -> impl IntoView {
    let ui = ui();
    Effect::new(move |was_open: Option<bool>| {
        let open = ui.modal.with(|m| m.is_some());
        match (was_open.unwrap_or(false), open) {
            (false, true) => remember_focus(),
            (true, false) => {
                if let Some(el) = RETURN_FOCUS.with(|r| r.borrow_mut().take()) {
                    let _ = el.focus();
                }
            }
            _ => {}
        }
        open
    });
    view! {
        {move || ui.modal.get().map(|m| match m {
            Modal::QuickSwitch => view! { <QuickSwitch/> }.into_any(),
            Modal::Search(q) => view! { <SearchDialog initial=q/> }.into_any(),
            Modal::NewVault => view! { <NewVault/> }.into_any(),
            Modal::Share(v) => view! { <Share vault=v/> }.into_any(),
            Modal::ShareNote(v, n) => view! { <ShareNote vault=v note=n/> }.into_any(),
            Modal::VaultSettings(v) => view! { <VaultSettings vault=v/> }.into_any(),
            Modal::History(v, n) => view! { <History vault=v note=n/> }.into_any(),
            Modal::ApproveDevice(d) => view! { <ApproveDevice device=d/> }.into_any(),
            Modal::Help => view! { <Help/> }.into_any(),
            Modal::Rename { vault, id } => view! { <RenameDialog vault=vault id=id/> }.into_any(),
            Modal::NewFolder { vault, parent } => view! { <NewFolder vault=vault parent=parent/> }.into_any(),
            Modal::Move { vault, id } => view! { <MoveDialog vault=vault id=id/> }.into_any(),
            Modal::Import(v) => view! { <ImportDialog vault=v/> }.into_any(),
            Modal::DeleteAccount => view! { <DeleteAccountDialog/> }.into_any(),
            Modal::MoveToVault { vault, what } => view! { <super::quick::MoveToVaultDialog vault=vault what=what/> }.into_any(),
            Modal::EntryIcon { vault, id, path, folder } => view! { <EntryIconDialog vault=vault id=id path=path folder=folder/> }.into_any(),
        })}
    }
}

// ------------------------------------------------------------------ quick switcher

#[component]
fn QuickSwitch() -> impl IntoView {
    let ui = ui();
    let q = RwSignal::new(String::new());
    let sel = RwSignal::new(0usize);
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(i) = input.get() {
            let _ = i.focus();
        }
    });
    let results = Memo::new(move |_| knowledge::quick(&q.get()));
    let current_vault = ui.route.get_untracked().vault().map(str::to_string);
    let open = move |i: usize| {
        let r = results.get_untracked();
        if let Some((v, id, _, _)) = r.get(i) {
            ui.modal.set(None);
            state::navigate(if step(|c| state::is_member(c, v)) {
                Route::Note(v.clone(), id.clone())
            } else {
                Route::Shared(v.clone(), id.clone())
            });
        } else if let Some(v) = current_vault.clone() {
            // No match: create a note with that title.
            let title = q.get_untracked();
            if !title.trim().is_empty() && state::step(|c| c.can_write(&v)) {
                ui.modal.set(None);
                leptos::task::spawn_local(async move {
                    if let Some(id) = actions::create_note_with(&v, "", &title, "").await {
                        state::navigate(Route::Note(v, id));
                    }
                });
            }
        }
    };
    let open2 = open.clone();
    view! {
        <div class="modal-back" on:mousedown=move |_| ui.modal.set(None)>
            <div class="modal palette" role="dialog" aria-label="Go to note" on:mousedown=|ev| ev.stop_propagation()>
                <div class="palette-input">
                    <Icon name="search"/>
                    <input node_ref=input placeholder="Jump to a note, or type a new title…" prop:value=move || q.get()
                        on:input=move |ev| { q.set(event_target_value(&ev)); sel.set(0); }
                        on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                            "ArrowDown" => { ev.prevent_default(); sel.update(|s| *s = (*s + 1).min(results.get_untracked().len().saturating_sub(1))); }
                            "ArrowUp" => { ev.prevent_default(); sel.update(|s| *s = s.saturating_sub(1)); }
                            "Enter" => { ev.prevent_default(); open(sel.get_untracked()); }
                            _ => {}
                        }/>
                    <button class="btn ghost small" on:click=move |_| ui.modal.set(Some(Modal::Search(q.get_untracked())))>"Full search"</button>
                </div>
                <div class="palette-list">
                    {move || {
                        let r = results.get();
                        if r.is_empty() {
                            let t = q.get();
                            return view! { <div class="palette-empty muted">{if t.trim().is_empty() { "No notes yet.".to_string() } else { format!("Press Enter to create “{t}”") }}</div> }.into_any();
                        }
                        r.into_iter().enumerate().map(|(i, (vid, id, path, vname))| {
                            let open = open2.clone();
                            let icon = step(|c| c.entry(&vid, &id)).and_then(|e| e.icon);
                            view! {
                                <button class="palette-item" class:sel=move || sel.get() == i on:mouseenter=move |_| sel.set(i) on:click=move |_| open(i)>
                                    {super::entry_icon(icon, "file")}
                                    <span class="grow ellipsis">{notekpr_core::paths::title(&path).to_string()}</span>
                                    <span class="muted small ellipsis">{match notekpr_core::paths::parent(&path) {
                                        "" => vname,
                                        folder => format!("{vname} · {folder}"),
                                    }}</span>
                                </button>
                            }
                        }).collect_view().into_any()
                    }}
                </div>
            </div>
        </div>
    }
}

// ------------------------------------------------------------------ search

fn highlighted(snippet: &str, hl: &[(usize, usize)]) -> String {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    for (a, b) in hl {
        if *a < i || *b > chars.len() {
            continue;
        }
        let pre: String = chars[i..*a].iter().collect();
        crate::editor::syntax::escape(&pre, &mut out);
        out.push_str("<mark>");
        let m: String = chars[*a..*b].iter().collect();
        crate::editor::syntax::escape(&m, &mut out);
        out.push_str("</mark>");
        i = *b;
    }
    let rest: String = chars[i.min(chars.len())..].iter().collect();
    crate::editor::syntax::escape(&rest, &mut out);
    out
}

#[component]
fn SearchDialog(initial: String) -> impl IntoView {
    let ui = ui();
    let q = RwSignal::new(initial);
    let scope = RwSignal::new(ui.route.get_untracked().vault().map(str::to_string));
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(i) = input.get() {
            let _ = i.focus();
        }
    });
    let results = Memo::new(move |_| {
        ui.content_rev.get();
        knowledge::search_all(&q.get(), scope.get().as_deref())
            .into_iter()
            .map(|h| (h.vault, h.vault_name, h.id, h.path, h.snippet, h.highlights))
            .collect::<Vec<_>>()
    });
    view! {
        <Dialog title="Search" wide=true>
            <div class="search-bar">
                <input class="input" node_ref=input placeholder="Search notes…  (tag:x  folder:y  \"exact phrase\")"
                    prop:value=move || q.get() on:input=move |ev| q.set(event_target_value(&ev))/>
                <select class="input" on:change=move |ev| { let v = event_target_value(&ev); scope.set(if v.is_empty() { None } else { Some(v) }); }>
                    <option value="" selected=move || scope.get().is_none()>"All vaults"</option>
                    {move || ui.vaults.get().into_iter().map(|v| {
                        let id = v.id.clone();
                        let id2 = v.id.clone();
                        view! { <option value=id selected=move || scope.get().as_deref() == Some(id2.as_str())>{v.name.clone()}</option> }
                    }).collect_view()}
                </select>
            </div>
            <p class="muted small">"Search runs on this device over your decrypted notes; the server never sees your queries."</p>
            <div class="results">
                {move || {
                    let r = results.get();
                    if q.get().trim().is_empty() {
                        return view! { <p class="muted">"Type to search."</p> }.into_any();
                    }
                    if r.is_empty() {
                        return view! { <p class="muted">"No matches."</p> }.into_any();
                    }
                    r.into_iter().map(|(v, vname, id, path, snippet, hl)| {
                        let html = highlighted(&snippet, &hl);
                        view! {
                            <button class="result" on:click=move |_| { ui.modal.set(None); state::navigate(Route::Note(v.clone(), id.clone())); }>
                                <div class="r-title">{notekpr_core::paths::title(&path).to_string()}<span class="muted small">{format!("  {vname} / {path}")}</span></div>
                                <div class="r-snip" inner_html=html></div>
                            </button>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </Dialog>
    }
}

// ------------------------------------------------------------------ vaults

#[component]
fn NewVault() -> impl IntoView {
    let ui = ui();
    let name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    view! {
        <Dialog title="New vault">
            <p class="muted">"A vault is a separate collection of notes with its own encryption key and its own people. Use one for private notes, one for the family, one per project…"</p>
            <form class="stack" on:submit=move |ev| {
                ev.prevent_default();
                busy.set(true);
                let n = name.get_untracked();
                leptos::task::spawn_local(async move {
                    if let Some(id) = actions::create_vault(n).await {
                        ui.modal.set(None);
                        state::navigate(Route::Vault(id));
                    }
                    busy.set(false);
                });
            }>
                <input class="input" placeholder="Vault name (e.g. Family)" use:focus_on_mount prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                <button class="btn primary" type="submit" disabled=move || busy.get() || name.get().trim().is_empty()>"Create vault"</button>
            </form>
        </Dialog>
    }
}

#[component]
fn Share(vault: String) -> impl IntoView {
    let ui = ui();
    let members = RwSignal::new(Vec::<MemberInfo>::new());
    let q = RwSignal::new(String::new());
    let found = RwSignal::new(Vec::<PublicUser>::new());
    let role = RwSignal::new(Role::Editor);
    let err = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let v = vault.clone();
    let reload = move || {
        let v = v.clone();
        leptos::task::spawn_local(async move {
            match api::get::<Vec<MemberInfo>>(&format!("/vaults/{v}/members")).await {
                Ok(m) => {
                    step(|c| {
                        for x in &m {
                            c.users.insert(x.user.id.clone(), x.user.clone());
                        }
                    });
                    members.set(m);
                }
                Err(e) => err.set(Some(e.message)),
            }
        });
    };
    reload();
    let timer = StoredValue::new(None::<i32>);
    let do_search = move |text: String| search_people(text, q, found, timer);
    let (vname, me) = step(|c| (c.vault_name(&vault), c.user_id.clone()));
    let (vs, vr, vm) = (vault.clone(), vault.clone(), vault.clone());
    let reload2 = reload.clone();
    let reload3 = reload.clone();
    view! {
        <Dialog title=format!("Share “{vname}”") wide=true>
            {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
            <div class="share-add">
                <input class="input grow" placeholder="Add people by name or e-mail…" prop:value=move || q.get() on:input=move |ev| do_search(event_target_value(&ev))/>
                <select class="input share-role" on:change=move |ev| role.set(member_role(&event_target_value(&ev)))>
                    <option value="admin" title=ADMIN_HINT>"Admin"</option>
                    <option value="editor" selected=true>"Can edit"</option>
                    <option value="viewer">"Can view"</option>
                </select>
            </div>
            <div class="found">
                {move || found.get().into_iter().filter(|u| !members.get().iter().any(|m| m.user.id == u.id)).map(|u| {
                    let v = vs.clone();
                    let u2 = u.clone();
                    let agent = u.kind == PrincipalKind::Agent;
                    let fp = actions::fingerprint(&u);
                    let reload = reload2.clone();
                    let pin = actions::pin_status(&u);
                    view! {
                        <div class="person">
                            {avatar(&u.display_name, &u.id)}
                            <div class="grow">
                                <div>{u.display_name.clone()}{(u.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}</div>
                                <div class="muted small">{u.email.clone().unwrap_or_default()}</div>
                                {fp.map(|f| view! { <div class="fp" title="Key fingerprint — compare it with the person to be sure the key is really theirs">{f}</div> })}
                                {(pin == Some(PinStatus::Changed)).then(|| view! { <div class="alert error small">"This person's key changed since you last shared with them. Verify the fingerprint before sharing."</div> })}
                                {u.account_key.is_none().then(|| view! { <div class="muted small">"Hasn't set up encryption yet — they'll get access after their first sign-in."</div> })}
                            </div>
                            {(pin == Some(PinStatus::Changed)).then(|| { let u3 = u.clone(); view! {
                                <button class="btn small" on:click=move |_| { actions::trust_key(&u3); state::success("New key trusted."); }>"I verified it"</button>
                            }})}
                            {agent.then(|| view! {
                                <Show when=move || role.get() == Role::Admin>
                                    <span class="muted small">"Agents can't be admins"</span>
                                </Show>
                            })}
                            <button class="btn small primary" disabled=move || busy.get() || (agent && role.get() == Role::Admin) on:click=move |_| {
                                busy.set(true);
                                let (v, u, r) = (v.clone(), u2.clone(), role.get_untracked());
                                let reload = reload.clone();
                                leptos::task::spawn_local(async move {
                                    match actions::share(&v, &u, r).await {
                                        Ok(()) => { q.set(String::new()); found.set(vec![]); reload(); }
                                        Err(e) => err.set(Some(e)),
                                    }
                                    busy.set(false);
                                });
                            }>"Share"</button>
                        </div>
                    }
                }).collect_view()}
            </div>
            <h3>"People with access"</h3>
            <div class="people">
                {move || members.get().into_iter().map(|m| {
                    let is_me = m.user.id == me;
                    let (v, uid) = (vr.clone(), m.user.id.clone());
                    let (v2, uid2) = (vm.clone(), m.user.id.clone());
                    let reload = reload3.clone();
                    let reload_b = reload3.clone();
                    let fp = actions::fingerprint(&m.user);
                    let (vouched, changed) = step(|c| {
                        (state::vouched(c, &vr, &m), state::member_log(c, &vr).key_changed(&m))
                    });
                    let verify = (changed && !m.has_current_key).then(|| {
                        let (u, v) = (m.user.clone(), vr.clone());
                        let reload = reload3.clone();
                        move |_| {
                            actions::trust_key(&u);
                            let (v, reload) = (v.clone(), reload.clone());
                            leptos::task::spawn_local(async move {
                                actions::grant_missing_keys(&v).await;
                                reload();
                            });
                        }
                    });
                    view! {
                        <div class="person">
                            {avatar(&m.user.display_name, &m.user.id)}
                            <div class="grow">
                                <div>{m.user.display_name.clone()}{is_me.then_some(" (you)")}{(m.user.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}</div>
                                <div class="muted small">{m.user.email.clone().unwrap_or_default()}{(!m.has_current_key).then_some(" · waiting for keys")}</div>
                                {fp.map(|f| view! { <div class="fp">{f}</div> })}
                                {(!vouched && changed).then(|| view! { <div class="alert error small key-changed">"Their encryption key changed since they were added — for example, they reset their account, or someone is intercepting. Until you've compared fingerprints with them, they don't get the vault's new key."</div> })}
                                {(!vouched && !changed).then(|| view! { <div class="alert error small unvouched">"Not added by the vault's owner: the server lists this person, but the owner's signed member list doesn't. They don't get the vault key; tell your administrator."</div> })}
                            </div>
                            {verify.map(|f| view! { <button class="btn small verify-key" on:click=f>"I verified it"</button> })}
                            {if m.role == Role::Owner {
                                view! { <span class="muted small">"Owner"</span> }.into_any()
                            } else {
                                view! {
                                    <select class="input small member-role" on:change=move |ev| {
                                        let r = member_role(&event_target_value(&ev));
                                        let (v, u) = (v.clone(), uid.clone());
                                        let reload = reload.clone();
                                        leptos::task::spawn_local(async move { actions::set_role(&v, &u, r).await; reload(); });
                                    }>
                                        {(m.user.kind != PrincipalKind::Agent).then(|| view! {
                                            <option value="admin" title=ADMIN_HINT selected=m.role == Role::Admin>"Admin"</option>
                                        })}
                                        <option value="editor" selected=m.role == Role::Editor>"Can edit"</option>
                                        <option value="viewer" selected=m.role == Role::Viewer>"Can view"</option>
                                        {other_role(&m.role)}
                                    </select>
                                    <button class="icon-btn" title="Remove access (rotates the vault key)" aria-label="Remove access (rotates the vault key)" on:click=move |_| {
                                        if !util::confirm("Remove this person? They keep anything they already downloaded, but can't read anything new.") { return; }
                                        let (v, u) = (v2.clone(), uid2.clone());
                                        let reload = reload_b.clone();
                                        leptos::task::spawn_local(async move { actions::remove_member(&v, &u).await; reload(); });
                                    }><Icon name="x"/></button>
                                }.into_any()
                            }}
                        </div>
                    }
                }).collect_view()}
            </div>
            <p class="muted small">"Admins can also edit, empty the trash and see the activity log. Only the owner manages people and the vault's settings."</p>
            <p class="muted small"><Icon name="shield"/>" Sharing sends the vault key encrypted to each person's own key. The server can't read it, and every change to who has access is signed by the vault's owner. Removing someone rotates the key."</p>
            {let _ = ui;}
        </Dialog>
    }
}

const ADMIN_HINT: &str = "Can edit, empty the trash and see the activity log";

/// A role from a newer version, shown as its name until the page is
/// reloaded (it can't be chosen).
fn other_role(role: &Role) -> Option<impl IntoView + use<>> {
    let Role::Other(name) = role else { return None };
    let name = name.clone();
    Some(
        view! { <option value="" selected disabled title="A role from a newer version of notekpr: reload the page to see it">{name}</option> },
    )
}

/// A vault member's role as chosen in a role `<select>`.
fn member_role(value: &str) -> Role {
    match value {
        "admin" => Role::Admin,
        "viewer" => Role::Viewer,
        _ => Role::Editor,
    }
}

/// Debounced people search shared by the sharing dialogs.
fn search_people(
    text: String,
    q: RwSignal<String>,
    found: RwSignal<Vec<PublicUser>>,
    timer: StoredValue<Option<i32>>,
) {
    q.set(text.clone());
    if let Some(t) = timer.get_value() {
        util::clear_timeout(t);
    }
    timer.set_value(Some(util::set_timeout(250, move || {
        leptos::task::spawn_local(async move {
            if text.trim().len() < 2 {
                found.set(vec![]);
                return;
            }
            if let Ok(u) =
                api::get::<Vec<PublicUser>>(&format!("/users/search?q={}", api::q(text.trim())))
                    .await
            {
                found.set(u);
            }
        });
    })));
}

/// Share a single note with people outside its vault.
#[component]
fn ShareNote(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let shares = RwSignal::new(Vec::<ObjectShareInfo>::new());
    let members = RwSignal::new(Vec::<MemberInfo>::new());
    let q = RwSignal::new(String::new());
    let found = RwSignal::new(Vec::<PublicUser>::new());
    let role = RwSignal::new(Role::Editor);
    let err = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let with_comments = RwSignal::new(true);
    let has_comments = step(|c| !state::comment_threads(c, &vault, &note).is_empty());
    let timer = StoredValue::new(None::<i32>);
    let (v, n) = (vault.clone(), note.clone());
    Effect::new(move |_| {
        ui.shares_rev.get();
        let (v, n) = (v.clone(), n.clone());
        leptos::task::spawn_local(async move {
            match api::get::<Vec<ObjectShareInfo>>(&format!("/vaults/{v}/objects/{n}/shares")).await
            {
                Ok(l) => {
                    step(|c| {
                        for x in &l {
                            c.users.insert(x.user.id.clone(), x.user.clone());
                        }
                    });
                    shares.set(l);
                }
                Err(e) => err.set(Some(e.message)),
            }
            if let Ok(m) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{v}/members")).await {
                members.set(m);
            }
        });
    });
    let (title, vname) = step(|c| (c.note_title(&vault, &note), c.vault_name(&vault)));
    let title = title.unwrap_or_else(|| "this note".into());
    let (vs, ns) = (vault.clone(), note.clone());
    let (vr, nr) = (vault.clone(), note.clone());
    let vw = vault.clone();
    let others = move || members.get().len().saturating_sub(1);
    view! {
        <Dialog title=format!("Share “{title}”") wide=true>
            {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
            <p class="muted small">{format!("Only this note is shared — not the rest of “{vname}”, its other notes or attached files.")}</p>
            <div class="share-add">
                <input class="input grow" placeholder="Add people by name or e-mail…" prop:value=move || q.get()
                    on:input=move |ev| search_people(event_target_value(&ev), q, found, timer)/>
                <select class="input" on:change=move |ev| role.set(if event_target_value(&ev) == "viewer" { Role::Viewer } else { Role::Editor })>
                    <option value="editor">"Can edit"</option>
                    <option value="viewer">"Can view"</option>
                </select>
            </div>
            {has_comments.then(|| view! {
                <label class="check share-comments">
                    <input type="checkbox" prop:checked=move || with_comments.get()
                        on:change=move |ev| with_comments.set(event_target_checked(&ev))/>
                    <span>"Include the comments written so far"
                        <span class="muted small">" — for everyone this note is shared with. Otherwise they see only comments written from now on."</span></span>
                </label>
            })}
            <div class="found">
                {move || {
                    let taken = |u: &PublicUser| shares.get().iter().any(|s| s.user.id == u.id) || members.get().iter().any(|m| m.user.id == u.id);
                    found.get().into_iter().filter(|u| !taken(u)).map(|u| {
                        let (v, n) = (vs.clone(), ns.clone());
                        let u2 = u.clone();
                        let fp = actions::fingerprint(&u);
                        let pin = actions::pin_status(&u);
                        let no_key = u.account_key.is_none();
                        view! {
                            <div class="person">
                                {avatar(&u.display_name, &u.id)}
                                <div class="grow">
                                    <div>{u.display_name.clone()}{(u.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}</div>
                                    <div class="muted small">{u.email.clone().unwrap_or_default()}</div>
                                    {fp.map(|f| view! { <div class="fp" title="Key fingerprint — compare it with the person to be sure the key is really theirs">{f}</div> })}
                                    {(pin == Some(PinStatus::Changed)).then(|| view! { <div class="alert error small">"This person's key changed since you last shared with them. Verify the fingerprint before sharing."</div> })}
                                    {no_key.then(|| view! { <div class="muted small">"Hasn't set up encryption yet — ask them to sign in once first."</div> })}
                                </div>
                                {(pin == Some(PinStatus::Changed)).then(|| { let u3 = u.clone(); view! {
                                    <button class="btn small" on:click=move |_| { actions::trust_key(&u3); state::success("New key trusted."); }>"I verified it"</button>
                                }})}
                                <button class="btn small primary" disabled=move || busy.get() || no_key on:click=move |_| {
                                    busy.set(true);
                                    err.set(None);
                                    let (v, n, u, r) = (v.clone(), n.clone(), u2.clone(), role.get_untracked());
                                    let cm = with_comments.get_untracked();
                                    leptos::task::spawn_local(async move {
                                        match actions::share_note(&v, &n, &u, r, cm).await {
                                            Ok(()) => { q.set(String::new()); found.set(vec![]); }
                                            Err(e) => err.set(Some(e)),
                                        }
                                        busy.set(false);
                                    });
                                }>"Share"</button>
                            </div>
                        }
                    }).collect_view()
                }}
            </div>
            <h3>"People with access to this note"</h3>
            <div class="people">
                {move || shares.get().is_empty().then(|| view! { <p class="muted small">"Not shared with anyone outside the vault yet."</p> })}
                {move || shares.get().into_iter().map(|sh| {
                    let (v, n, uid) = (vr.clone(), nr.clone(), sh.user.id.clone());
                    let (v2, n2, uid2) = (vr.clone(), nr.clone(), sh.user.id.clone());
                    let fp = actions::fingerprint(&sh.user);
                    // Left without the note's current key because their key
                    // changed: once verified, sharing again gives it.
                    let changed = !sh.has_current_key && actions::pin_status(&sh.user) == Some(PinStatus::Changed);
                    let verify = changed.then(|| {
                        let (v, n, u, r) = (vr.clone(), nr.clone(), sh.user.clone(), sh.role.clone());
                        move |_| {
                            actions::trust_key(&u);
                            let (v, n, u, r) = (v.clone(), n.clone(), u.clone(), r.clone());
                            leptos::task::spawn_local(async move {
                                if let Err(e) = actions::share_note(&v, &n, &u, r, false).await {
                                    err.set(Some(e));
                                }
                                ui.shares_rev.update(|x| *x += 1);
                            });
                        }
                    });
                    view! {
                        <div class="person">
                            {avatar(&sh.user.display_name, &sh.user.id)}
                            <div class="grow">
                                <div>{sh.user.display_name.clone()}{(sh.user.kind == PrincipalKind::Agent).then(|| view! { <span class="tag-agent">"agent"</span> })}</div>
                                <div class="muted small">{sh.user.email.clone().unwrap_or_default()}{(!sh.has_current_key).then_some(" · waiting for the new key")}</div>
                                {fp.map(|f| view! { <div class="fp">{f}</div> })}
                                {changed.then(|| view! { <div class="alert error small key-changed">"Their encryption key changed — for example, they reset their account, or someone is intercepting. Until you've compared fingerprints with them, they don't get the note's new key."</div> })}
                            </div>
                            {verify.map(|f| view! { <button class="btn small verify-key" on:click=f>"I verified it"</button> })}
                            <select class="input small" on:change=move |ev| {
                                let r = if event_target_value(&ev) == "viewer" { Role::Viewer } else { Role::Editor };
                                let (v, n, u) = (v.clone(), n.clone(), uid.clone());
                                leptos::task::spawn_local(async move {
                                    match actions::set_note_share_role(&v, &n, &u, r).await {
                                        Ok(()) => ui.shares_rev.update(|x| *x += 1),
                                        Err(e) => err.set(Some(e)),
                                    }
                                });
                            }>
                                <option value="editor" selected=sh.role == Role::Editor>"Can edit"</option>
                                <option value="viewer" selected=sh.role == Role::Viewer>"Can view"</option>
                                {other_role(&sh.role)}
                            </select>
                            <button class="icon-btn" title="Stop sharing with this person (renews the note's key)" aria-label="Stop sharing with this person (renews the note's key)" on:click=move |_| {
                                if !util::confirm("Stop sharing this note with this person? They keep anything they already downloaded, but can't read anything new.") { return; }
                                let (v, n, u) = (v2.clone(), n2.clone(), uid2.clone());
                                leptos::task::spawn_local(async move {
                                    match actions::unshare_note(&v, &n, &u).await {
                                        Ok(()) => state::success("Stopped sharing and renewed the note's key."),
                                        Err(e) => err.set(Some(e)),
                                    }
                                });
                            }><Icon name="x"/></button>
                        </div>
                    }
                }).collect_view()}
            </div>
            <div class="share-vault-hint">
                <span class="muted small">{move || match others() {
                    0 => "Nobody else is in this vault.".to_string(),
                    1 => "1 other person in this vault can open it too.".to_string(),
                    k => format!("{k} other people in this vault can open it too."),
                }}</span>
                <button class="btn ghost small" on:click=move |_| ui.modal.set(Some(Modal::Share(vw.clone())))><Icon name="users"/>" Share the whole vault…"</button>
            </div>
            <p class="muted small"><Icon name="shield"/>" The note gets its own key, sent encrypted to each person's own key — the server can't read it. Removing someone renews the note's key."</p>
        </Dialog>
    }
}

/// Emoji offered for icons (of vaults, notes and folders); any other can be
/// typed.
const EMOJI: [&str; 24] = [
    "📓", "📚", "🏠", "💼", "🧪", "🎓", "💡", "🧠", "❤️", "⭐", "🔒", "🎨", "🎵", "🍳", "🌱", "✈️",
    "🏃", "💰", "🛠️", "🎮", "📷", "🗂️", "🐾", "🌍",
];

/// Pick an emoji, type any, or remove the icon: `on_pick` gets the emoji,
/// or "" to remove it. `children` are more actions, next to Remove.
#[component]
fn EmojiPicker(
    #[prop(into)] chosen: Signal<Option<String>>,
    #[prop(into)] removable: Signal<bool>,
    on_pick: Callback<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let typed = RwSignal::new(String::new());
    view! {
        <div class="grow">
            <div class="emoji-grid" role="group" aria-label="Icon">
                {EMOJI.iter().map(|&e| {
                    let on = move || chosen.get().as_deref() == Some(e);
                    view! { <button type="button" class="emoji-btn" class:on=on aria-label=e on:click=move |_| on_pick.run(e.to_string())>{e}</button> }
                }).collect_view()}
            </div>
            <div class="icon-actions">
                <form class="row" on:submit=move |ev| {
                    ev.prevent_default();
                    let t = typed.get_untracked();
                    if !t.trim().is_empty() {
                        typed.set(String::new());
                        on_pick.run(t);
                    }
                }>
                    <input class="input icon-input" placeholder="Type an emoji" prop:value=move || typed.get() on:input=move |ev| typed.set(event_target_value(&ev))/>
                    <button class="btn" type="submit">"Use"</button>
                </form>
                {children.map(|c| c())}
                <Show when=move || removable.get()>
                    <button class="btn icon-remove" on:click=move |_| on_pick.run(String::new())>"Remove"</button>
                </Show>
            </div>
        </div>
    }
}

/// The vault's icon: an emoji, or a picture made from an uploaded image.
#[component]
fn VaultIconPicker(vault: String) -> impl IntoView {
    let ui = ui();
    let vid = StoredValue::new(vault);
    let current = Memo::new(move |_| {
        ui.vaults
            .get()
            .into_iter()
            .find(|v| v.id == vid.get_value())
    });
    let busy = RwSignal::new(false);
    let set = move |icon: String| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            actions::set_vault_icon(&vid.get_value(), &icon).await;
            busy.set(false);
        });
    };
    let upload = move |ev: leptos::ev::Event| {
        let input: web_sys::HtmlInputElement = event_target(&ev);
        let file = input.files().and_then(|l| l.get(0));
        // The same file can be chosen again.
        input.set_value("");
        let Some(file) = file else { return };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            actions::set_vault_picture(&vid.get_value(), file).await;
            busy.set(false);
        });
    };
    let chosen = Signal::derive(move || {
        current
            .get()
            .filter(|v| v.picture.is_none())
            .and_then(|v| v.icon)
    });
    let removable = Signal::derive(move || {
        current
            .get()
            .is_some_and(|v| v.icon.is_some() || v.picture.is_some())
    });
    view! {
        <div class="icon-pick">
            <div class="icon-now">{move || super::vault_icon(current.get().as_ref())}</div>
            <EmojiPicker chosen=chosen removable=removable on_pick=Callback::new(set)>
                <label class="btn" class:disabled=move || busy.get()>
                    <Icon name="image"/>" Upload picture…"
                    <input type="file" class="vault-picture-input" accept="image/*" on:change=upload/>
                </label>
            </EmojiPicker>
        </div>
    }
}

/// A note's or folder's icon.
#[component]
fn EntryIconDialog(vault: String, id: Option<String>, path: String, folder: bool) -> impl IntoView {
    let ui = ui();
    let name = if folder {
        notekpr_core::paths::file_name(&path).to_string()
    } else {
        notekpr_core::paths::title(&path).to_string()
    };
    let target = StoredValue::new((vault, id, path));
    let current = Memo::new(move |_| {
        ui.tree_rev.get();
        let (v, id, _) = target.get_value();
        id.and_then(|id| step(|c| c.entry(&v, &id)))
            .and_then(|e| e.icon)
    });
    let pick = move |icon: String| {
        let (v, id, path) = target.get_value();
        if actions::set_entry_icon(&v, id.as_deref(), &path, &icon) {
            ui.modal.set(None);
        }
    };
    let fallback = if folder { "folder" } else { "file" };
    view! {
        <Dialog title=format!("Icon for “{name}”")>
            <div class="icon-pick">
                <div class="icon-now">{move || match current.get() {
                    Some(i) => view! { <span class="entry-icon">{i}</span> }.into_any(),
                    None => view! { <Icon name=fallback/> }.into_any(),
                }}</div>
                <EmojiPicker chosen=current removable=Signal::derive(move || current.get().is_some()) on_pick=Callback::new(pick)/>
            </div>
        </Dialog>
    }
}

#[component]
fn VaultSettings(vault: String) -> impl IntoView {
    let ui = ui();
    let (name, owner, admin, archived, epoch) = step(|c| {
        (
            c.vault_name(&vault),
            c.is_owner(&vault),
            c.can_administer(&vault),
            c.vault_infos.get(&vault).is_some_and(|i| i.archived),
            c.replicas
                .get(&vault)
                .and_then(|r| r.keyring.current_epoch())
                .unwrap_or(0),
        )
    });
    let name = RwSignal::new(name);
    let audit = RwSignal::new(Vec::<AuditEntry>::new());
    let show_audit = RwSignal::new(false);
    let (v1, v2, v3, v4, v5, v6, v7, v8) = (
        vault.clone(),
        vault.clone(),
        vault.clone(),
        vault.clone(),
        vault.clone(),
        vault.clone(),
        vault.clone(),
        vault.clone(),
    );
    let (v9, va) = (vault.clone(), vault.clone());
    view! {
        <Dialog title="Vault settings" wide=true>
            <Show when=move || owner fallback=|| view! { <p class="muted">"Only the owner can change this vault's settings."</p> }>
                {
                    let v1 = v1.clone();
                    let vi = v1.clone();
                    view! {
                        <form class="row" on:submit=move |ev| {
                            ev.prevent_default();
                            let (v, n) = (v1.clone(), name.get_untracked());
                            leptos::task::spawn_local(async move { if actions::rename_vault(&v, &n).await { state::success("Renamed."); } });
                        }>
                            <input class="input grow" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                            <button class="btn" type="submit">"Rename"</button>
                        </form>
                        <VaultIconPicker vault=vi/>
                    }
                }
                {
                    let v = v9.clone();
                    let on = move || {
                        ui.vaults.track();
                        step(|c| c.vault_infos.get(&v).is_some_and(|i| i.viewers_comment))
                    };
                    let v = v9.clone();
                    view! {
                        <label class="check viewers-comment">
                            <input type="checkbox" prop:checked=on
                                on:change=move |ev| {
                                    let (v, on) = (v.clone(), event_target_checked(&ev));
                                    leptos::task::spawn_local(async move { actions::set_viewers_comment(&v, on).await });
                                }/>
                            <span>"Viewers can comment"<span class="muted small">" — on notes they can read (also people a note is shared with to view)"</span></span>
                        </label>
                    }
                }
            </Show>
            <div class="settings-grid">
                <button class="btn" on:click=move |_| { let v = v2.clone(); leptos::task::spawn_local(async move { actions::export_vault(&v).await }); }>
                    <Icon name="download"/>" Export as Markdown (.zip)"</button>
                <Show when=move || state::step(|c| c.can_write(&v3))>
                    {
                        let v = v4.clone();
                        view! { <button class="btn" on:click=move |_| ui.modal.set(Some(Modal::Import(v.clone())))><Icon name="upload"/>" Import notes"</button> }
                    }
                </Show>
                <Show when=move || owner>
                    {
                        let (v5, v6, v7) = (v5.clone(), v6.clone(), v7.clone());
                        view! {
                            <button class="btn" on:click=move |_| ui.modal.set(Some(Modal::Share(v5.clone())))><Icon name="share"/>" People & sharing"</button>
                            <button class="btn" on:click=move |_| {
                                let v = v6.clone();
                                leptos::task::spawn_local(async move { if actions::rotate_keys(&v).await { state::success("Vault key rotated."); } });
                            }><Icon name="key"/>" Rotate encryption key"</button>
                            <button class="btn" on:click={ let v7 = v7.clone(); move |_| { let v = v7.clone(); leptos::task::spawn_local(async move { actions::set_archived(&v, !archived).await; }); ui.modal.set(None); } }>
                                <Icon name="archive"/>{if archived { " Unarchive" } else { " Archive" }}</button>
                        }
                    }
                </Show>
                <Show when=move || admin>
                    {
                        let va = va.clone();
                        view! {
                            <button class="btn activity-log" on:click={
                                let v = va.clone();
                                move |_| {
                                    show_audit.set(true);
                                    let v = v.clone();
                                    leptos::task::spawn_local(async move { if let Ok(a) = api::get(&format!("/vaults/{v}/audit")).await { audit.set(a); } });
                                }
                            }><Icon name="history"/>" Activity log"</button>
                        }
                    }
                </Show>
            </div>
            <p class="muted small">{format!("Key epoch {epoch}. Content is encrypted with XChaCha20-Poly1305; the key is shared with members via HPKE (X25519).")}</p>
            <Show when=move || show_audit.get()>
                <div class="audit">
                    {move || audit.get().into_iter().map(|a| view! {
                        <div class="audit-row"><span class="muted small">{util::relative_time(a.at)}</span>
                            <span>{a.actor_name.clone().unwrap_or_default()}</span><code>{a.action.clone()}</code></div>
                    }).collect_view()}
                </div>
            </Show>
            <div class="danger-zone">
                {if owner {
                    let v = v8.clone();
                    let vname = state::step(|c| c.vault_name(&v8));
                    view! {
                        <button class="btn danger" on:click=move |_| {
                            let typed = util::window().prompt_with_message(&format!("This permanently deletes “{vname}” for everyone. Type the vault name to confirm:")).ok().flatten();
                            if typed.as_deref() == Some(vname.as_str()) {
                                ui.modal.set(None);
                                let v = v.clone();
                                leptos::task::spawn_local(async move { actions::delete_vault(&v).await });
                            }
                        }><Icon name="trash"/>" Delete vault"</button>
                    }.into_any()
                } else {
                    let v = v8.clone();
                    view! {
                        <button class="btn danger" on:click=move |_| {
                            if util::confirm("Leave this vault? You'll lose access until someone shares it again.") {
                                ui.modal.set(None);
                                let v = v.clone();
                                leptos::task::spawn_local(async move { actions::leave_vault(&v).await });
                            }
                        }><Icon name="logout"/>" Leave vault"</button>
                    }.into_any()
                }}
            </div>
        </Dialog>
    }
}

// ------------------------------------------------------------------ history

#[component]
fn History(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let list = RwSignal::new(Vec::<SnapshotInfo>::new());
    let loading = RwSignal::new(true);
    let preview = RwSignal::new(None::<(String, String)>);
    let (v, n) = (vault.clone(), note.clone());
    leptos::task::spawn_local(async move {
        step(state::flush_now);
        if let Ok(l) =
            api::get::<Vec<SnapshotInfo>>(&format!("/vaults/{v}/objects/{n}/snapshots")).await
        {
            list.set(l);
        }
        loading.set(false);
    });
    let (vp, np) = (vault.clone(), note.clone());
    let pick = move |id: String| {
        let (v, n) = (vp.clone(), np.clone());
        leptos::task::spawn_local(async move {
            match api::get::<Snapshot>(&format!("/vaults/{v}/objects/{n}/snapshots/{id}")).await {
                Ok(s) => {
                    let text = step(|c| c.replicas.get(&v).map(|r| r.decrypt_revision(&s)));
                    match text {
                        Some(Ok(t)) => preview.set(Some((s.id, t))),
                        _ => state::error("This version could not be decrypted."),
                    }
                }
                Err(e) => state::error(e.message),
            }
        });
    };
    let (vr, nr) = (vault.clone(), note.clone());
    let writable = step(|c| state::can_write_note(c, &vault, &note));
    let vv = vault.clone();
    let names = move |uid: &str| {
        step(|c| match c.users.get(uid) {
            Some(u) => u.display_name.clone(),
            None if uid == c.user_id => "you".into(),
            None => "someone".into(),
        })
    };
    view! {
        <Dialog title="Version history" wide=true>
            <div class="history">
                <div class="h-list">
                    <Show when=move || loading.get()><div class="spinner"></div></Show>
                    <Show when=move || !loading.get() && list.get().is_empty()>
                        <p class="muted small">"No saved versions yet. Versions are stored automatically after editing sessions."</p>
                    </Show>
                    {move || list.get().into_iter().map(|s| {
                        let id = s.id.clone();
                        let id2 = s.id.clone();
                        let pick = pick.clone();
                        let who = names(&s.author_id);
                        view! {
                            <button class="h-item" class:sel=move || preview.get().is_some_and(|p| p.0 == id2) on:click=move |_| pick(id.clone())>
                                <div>{util::relative_time(s.created_at)}</div>
                                <div class="muted small">{format!("{} · {}", who, util::absolute_time(s.created_at))}</div>
                            </button>
                        }
                    }).collect_view()}
                </div>
                <div class="h-preview">
                    {move || match preview.get() {
                        None => view! { <p class="muted">"Select a version to preview it."</p> }.into_any(),
                        Some((_, text)) => {
                            let vv = vv.clone();
                            let note = move |t: &str| knowledge::resolve_target(&vv, t, "");
                            let none = |_: &str| None;
                            let html = render::to_html(&text, &render::Ctx { date_label: &crate::editor::date_label, ..render::Ctx::new(&note, &none) });
                            let (v, n, t) = (vr.clone(), nr.clone(), text.clone());
                            view! {
                                <div class="h-actions">
                                    {writable.then(|| view! {
                                        <button class="btn primary small" on:click=move |_| { actions::restore_revision(&v, &n, &t); ui.modal.set(None); }>"Restore this version"</button>
                                    })}
                                    <button class="btn small" on:click={ let t = text.clone(); move |_| { util::copy_to_clipboard(&t); state::success("Copied."); } }>"Copy text"</button>
                                </div>
                                <article class="reading" inner_html=html></article>
                            }.into_any()
                        }
                    }}
                </div>
            </div>
        </Dialog>
    }
}

// ------------------------------------------------------------------ devices

#[component]
fn ApproveDevice(device: DeviceInfo) -> impl IntoView {
    let ui = ui();
    let (uid, account) = step(|c| (c.user_id.clone(), c.account.as_ref().map(|a| a.public())));
    let code = account
        .and_then(|a| {
            notekpr_core::engine::verification_code(&uid, &device.id, &device.enc_public, &a)
        })
        .unwrap_or_default();
    let busy = RwSignal::new(false);
    let d1 = device.clone();
    let d2 = device.clone();
    view! {
        <Dialog title="Approve a new device">
            <p>{format!("“{}” wants to access your encrypted notes.", device.name)}</p>
            <p>"Only approve it if this is your device and it shows exactly this code:"</p>
            <div class="verify-code">{code}</div>
            <p class="muted small">{format!("Requested {}", util::relative_time(device.created_at))}</p>
            <div class="row">
                <button class="btn primary" disabled=move || busy.get() on:click=move |_| {
                    busy.set(true);
                    let d = d1.clone();
                    leptos::task::spawn_local(async move {
                        let req = step(|c| c.account.as_ref().map(|a| notekpr_core::engine::approve_device(a, &c.user_id, &d.id, &d.enc_public)));
                        match req {
                            Some(Ok(r)) => match api::post::<_, ()>(&format!("/devices/{}/activate", d.id), &r).await {
                                Ok(()) => { state::success("Device approved."); ui.modal.set(None); }
                                Err(e) => state::error(e.message),
                            },
                            _ => state::error("Could not prepare the approval."),
                        }
                        state::refresh_pending_devices().await;
                        busy.set(false);
                    });
                }>"Codes match — approve"</button>
                <button class="btn danger" on:click=move |_| {
                    let d = d2.clone();
                    leptos::task::spawn_local(async move {
                        let _ = api::delete(&format!("/devices/{}", d.id)).await;
                        state::refresh_pending_devices().await;
                        ui.modal.set(None);
                    });
                }>"Deny"</button>
            </div>
        </Dialog>
    }
}

// ------------------------------------------------------------------ misc

#[component]
fn Help() -> impl IntoView {
    let m = util::mod_key();
    let rows: Vec<(String, &str)> = vec![
        (
            format!("{m}+P (or {m}+O, or {m}+K outside the editor)"),
            "Jump to a note / create one",
        ),
        (format!("{m}+Shift+F"), "Search all notes"),
        (format!("{m}+Alt+N"), "New note"),
        ("Alt+Q".into(), "New quick note"),
        (
            "Alt+M".into(),
            "Comment on the selected text (also in the reading view)",
        ),
        ("@ (in a comment)".into(), "Mention someone in the vault"),
        (
            "@today, @tomorrow, @yesterday (in a note)".into(),
            "That day's date (or pick one from the @ menu)",
        ),
        (format!("{m}+E"), "Toggle reading view"),
        (format!("{m}+\\"), "Toggle side panel"),
        (format!("{m}+G"), "Graph of the vault"),
        (format!("{m}+B / {m}+I"), "Bold / italic"),
        (format!("{m}+K (in editor)"), "Insert link"),
        (format!("{m}+`"), "Inline code"),
        (format!("{m}+Shift+`"), "Code block"),
        (format!("{m}+Shift+X / H"), "Strikethrough / highlight"),
        (
            format!("{m}+Shift+8 / 7 / 9 / ."),
            "Bullet / numbered / task list / quote",
        ),
        (format!("{m}+Alt+1…6, 0"), "Heading level / normal text"),
        (format!("{m}+Enter"), "Toggle checkbox"),
        (
            "Tab / Shift+Tab".into(),
            "Indent / outdent list item; next / previous table cell",
        ),
        (
            "Enter (in a table)".into(),
            "New row; on an empty row, end the table",
        ),
        (format!("{m}+Z / {m}+Shift+Z or {m}+Y"), "Undo / redo"),
        (format!("{m}+click a link"), "Follow it while editing"),
        ("[[".into(), "Link to another note"),
        ("#word".into(), "Add a tag"),
        (format!("{m}+/ (or ? outside the editor)"), "This help"),
    ];
    view! {
        <Dialog title="Keyboard shortcuts">
            <table class="keys">
                {rows.into_iter().map(|(k, d)| view! { <tr><td><kbd>{k}</kbd></td><td>{d}</td></tr> }).collect_view()}
            </table>
            <p class="muted small">"Vim keybindings can be enabled in Settings → Editor."</p>
        </Dialog>
    }
}

#[component]
fn RenameDialog(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let name = RwSignal::new(
        state::step(|c| c.entry(&vault, &id))
            .map(|e| e.title().to_string())
            .unwrap_or_default(),
    );
    view! {
        <Dialog title="Rename">
            <form class="stack" on:submit=move |ev| {
                ev.prevent_default();
                match actions::rename_entry(&vault, &id, &name.get_untracked()) { Ok(()) => ui.modal.set(None), Err(e) => state::error(e) }
            }>
                <input class="input" use:focus_on_mount prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                <button class="btn primary" type="submit">"Rename"</button>
            </form>
        </Dialog>
    }
}

#[component]
fn NewFolder(vault: String, parent: String) -> impl IntoView {
    let ui = ui();
    let name = RwSignal::new(String::new());
    let label = if parent.is_empty() {
        "New folder".to_string()
    } else {
        format!("New folder in {parent}")
    };
    view! {
        <Dialog title=label>
            <form class="stack" on:submit=move |ev| {
                ev.prevent_default();
                let n = name.get_untracked();
                if n.trim().is_empty() { return; }
                actions::create_folder(&vault, &notekpr_core::paths::join(&parent, n.trim()));
                ui.modal.set(None);
            }>
                <input class="input" use:focus_on_mount placeholder="Folder name" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                <button class="btn primary" type="submit">"Create folder"</button>
            </form>
        </Dialog>
    }
}

#[component]
fn MoveDialog(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let (folders, current) = step(|c| {
        (
            c.replicas
                .get(&vault)
                .map(|r| r.index.folders())
                .unwrap_or_default(),
            c.entry(&vault, &id)
                .map(|e| notekpr_core::paths::parent(&e.path).to_string())
                .unwrap_or_default(),
        )
    });
    let mut all = vec![String::new()];
    all.extend(folders);
    view! {
        <Dialog title="Move to folder">
            <div class="list">
                {all.into_iter().map(|f| {
                    let (v, i, f2) = (vault.clone(), id.clone(), f.clone());
                    let is_current = f == current;
                    view! {
                        <button class="list-row" disabled=is_current on:click=move |_| {
                            match actions::move_entry(&v, &i, &f2) { Ok(()) => { ui.modal.set(None); state::success("Moved."); } Err(e) => state::error(e) }
                        }>
                            <Icon name="folder"/><span class="grow">{if f.is_empty() { "(vault root)".to_string() } else { f.clone() }}</span>
                            {is_current.then(|| view! { <span class="muted small">"current"</span> })}
                        </button>
                    }
                }).collect_view()}
            </div>
        </Dialog>
    }
}

/// Delete one's account: what goes and what stays, an export first if
/// wanted, and "delete" typed to be sure.
#[component]
fn DeleteAccountDialog() -> impl IntoView {
    let ui = ui();
    let typed = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);
    let vaults = ui.vaults.get_untracked();
    let (owned, joined): (Vec<_>, Vec<_>) = vaults.iter().partition(|v| v.role == Role::Owner);
    // ("" when there are none; the count, for "vault" or "vaults").
    let owned_names = |shared: bool| {
        let names: Vec<String> = owned
            .iter()
            .filter(|v| v.shared == shared)
            .map(|v| format!("“{}”", v.name))
            .collect();
        (names.join(", "), names.len())
    };
    let vaults_word = |n: usize| if n == 1 { "vault" } else { "vaults" };
    let ((shared_owned, n_shared), (private_owned, n_private)) =
        (owned_names(true), owned_names(false));
    let joined = joined
        .iter()
        .map(|v| format!("“{}”", v.name))
        .collect::<Vec<_>>()
        .join(", ");
    let everything: Vec<String> = vaults
        .iter()
        .filter(|v| v.loaded)
        .map(|v| v.id.clone())
        .collect();
    let export = move |_| {
        let all = everything.clone();
        busy.set(true);
        leptos::task::spawn_local(async move {
            actions::export_account(all, true).await;
            busy.set(false);
        });
    };
    let delete = move |_| {
        busy.set(true);
        leptos::task::spawn_local(async move {
            match state::delete_account().await {
                Ok(()) => ui.modal.set(None),
                Err(e) => {
                    err.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };
    view! {
        <Dialog title="Delete your account">
            {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
            <div class="stack">
                <p>"Your account on this server is deleted for good, with everything that is yours here:"</p>
                <ul class="small delete-what">
                    {(n_private > 0).then(|| view! { <li>{format!("your {} {private_owned};", vaults_word(n_private))}</li> })}
                    {(n_shared > 0).then(|| view! { <li><b>{format!("the {} you share with others, {shared_owned} — gone for them too;", vaults_word(n_shared))}</b></li> })}
                    {(!joined.is_empty()).then(|| view! { <li>{format!("you leave what is shared with you ({joined}); its owners keep it;")}</li> })}
                    <li>"your quick notes, AI agents, devices and sessions."</li>
                </ul>
                <p class="muted small">"Moving to another server? Export first and import the .zip there. Signing in here again starts a new, empty account."</p>
                <div class="row">
                    <button class="btn" disabled=move || busy.get() on:click=export><Icon name="download"/>" Export everything first"</button>
                </div>
                <label class="stack small">
                    "Type delete to confirm"
                    <input class="input delete-confirm" autocomplete="off" prop:value=move || typed.get()
                        on:input=move |ev| typed.set(event_target_value(&ev))/>
                </label>
                <div class="row">
                    <button class="btn danger delete-account-now"
                        disabled=move || busy.get() || !typed.get().trim().eq_ignore_ascii_case("delete")
                        on:click=delete>"Delete my account"</button>
                    <button class="btn ghost" on:click=move |_| ui.modal.set(None)>"Cancel"</button>
                </div>
            </div>
        </Dialog>
    }
}

#[component]
fn ImportDialog(vault: String) -> impl IntoView {
    let ui = ui();
    let busy = RwSignal::new(false);
    let run = move |dir: bool, v: String| {
        actions::pick_files(
            dir,
            if dir { "" } else { ".md,.markdown,.txt,.zip" },
            move |files| {
                busy.set(true);
                leptos::task::spawn_local(async move {
                    let items = actions::files_to_import(files).await;
                    actions::import(&v, items).await;
                    busy.set(false);
                    ui.modal.set(None);
                });
            },
        );
    };
    let run2 = run;
    let (v1, v2) = (vault.clone(), vault.clone());
    let _ = EntryKind::Note;
    view! {
        <Dialog title="Import notes">
            <p>"Bring in Markdown notes from Obsidian, other apps or a previous export. Folder structure and attachments are kept. Files are encrypted on this device before upload."</p>
            <div class="stack">
                <button class="btn" disabled=move || busy.get() on:click=move |_| run(true, v1.clone())><Icon name="folder"/>" Import a folder…"</button>
                <button class="btn" disabled=move || busy.get() on:click=move |_| run2(false, v2.clone())><Icon name="file"/>" Import files or a .zip…"</button>
            </div>
            <Show when=move || busy.get()><p class="muted"><span class="spinner inline"></span>" Importing…"</p></Show>
        </Dialog>
    }
}
