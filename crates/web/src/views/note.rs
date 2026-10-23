//! The note page: title, toolbar, editor / reading view and the side panel.

use std::cell::Cell;

use leptos::html;
use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::paths;
use wasm_bindgen::JsCast;

use crate::actions::{self, NoteHost};
use crate::editor::{Command, Editor, Mode};
use crate::state::{self, Modal, Route, ViewMode, step, ui};
use crate::views::comments::{self, CommentLayer, CommentsList};
use crate::views::icons::Icon;
use crate::views::panels::RightPanel;
use crate::views::{SyncStatus, avatar};
use crate::{knowledge, render, util};

thread_local! {
    static FOCUS_TITLE: Cell<bool> = const { Cell::new(false) };
}

/// Ask the next note view to focus its title (used for "New note").
pub fn focus_title_soon() {
    FOCUS_TITLE.with(|f| f.set(true));
}

fn editor() -> Option<std::rc::Rc<Editor>> {
    step(|c| c.editor.as_ref().map(|(_, _, e)| e.clone()))
}

fn cmd(c: Command) {
    if let Some(ed) = editor() {
        ed.command(c);
    }
}

#[component]
fn ToolButton(icon: &'static str, title: String, c: Command) -> impl IntoView {
    view! {
        <button class="tb" title=title.clone() aria-label=title on:mousedown=move |ev| { ev.prevent_default(); cmd(c); }>
            <Icon name=icon/>
        </button>
    }
}

/// Formatting toolbar. Without a vault (a note shared with us) there is no
/// file attachment button.
#[component]
fn Toolbar(vault: Option<String>) -> impl IntoView {
    let m = util::mod_key();
    let heading_open = RwSignal::new(false);
    super::close_on_outside(heading_open);
    view! {
        // The frame is a wrapper: on phones the buttons scroll sideways, and
        // their menus must not be cut off with them.
        <div class="toolbar-wrap"><div class="toolbar" role="toolbar" aria-label="Formatting">
            <ToolButton icon="undo" title=format!("Undo ({m}+Z)") c=Command::Undo/>
            <ToolButton icon="redo" title=format!("Redo ({m}+Shift+Z)") c=Command::Redo/>
            <span class="tb-sep"></span>
            <div class="tb-drop">
                <button class="tb" title="Heading" aria-label="Heading" on:mousedown=move |ev| { ev.prevent_default(); heading_open.update(|o| *o = !*o); }><Icon name="heading"/></button>
                <Show when=move || heading_open.get()>
                    <div class="menu" on:mousedown=move |ev| { ev.prevent_default(); heading_open.set(false); }>
                        <button on:mousedown=move |_| cmd(Command::Heading(1))><span class="h1s">"Heading 1"</span></button>
                        <button on:mousedown=move |_| cmd(Command::Heading(2))><span class="h2s">"Heading 2"</span></button>
                        <button on:mousedown=move |_| cmd(Command::Heading(3))><span class="h3s">"Heading 3"</span></button>
                        <button on:mousedown=move |_| cmd(Command::Heading(0))>"Normal text"</button>
                    </div>
                </Show>
            </div>
            <ToolButton icon="bold" title=format!("Bold ({m}+B)") c=Command::Bold/>
            <ToolButton icon="italic" title=format!("Italic ({m}+I)") c=Command::Italic/>
            <ToolButton icon="strike" title=format!("Strikethrough ({m}+Shift+X)") c=Command::Strike/>
            <span class="tb-sep"></span>
            <ToolButton icon="list" title="Bulleted list".to_string() c=Command::Bullet/>
            <ToolButton icon="list-ordered" title="Numbered list".to_string() c=Command::Numbered/>
            <ToolButton icon="checkbox" title=format!("Checklist ({m}+Enter toggles)") c=Command::Task/>
            <span class="tb-sep"></span>
            <ToolButton icon="quote" title="Quote".to_string() c=Command::Quote/>
            <ToolButton icon="code" title=format!("Code ({m}+`)") c=Command::Code/>
            <ToolButton icon="code-block" title=format!("Code block ({m}+Shift+`)") c=Command::CodeBlock/>
            <ToolButton icon="link" title=format!("Link ({m}+K)") c=Command::Link/>
            <ToolButton icon="table" title="Table".to_string() c=Command::Table/>
            <ToolButton icon="minus" title="Divider".to_string() c=Command::Rule/>
            {vault.map(|vault| view! {
                <button class="tb" title="Attach a file or image" aria-label="Attach a file or image" on:mousedown=move |ev| {
                    ev.prevent_default();
                    let v = vault.clone();
                    // The note the file is for: the one open now, not when the upload ends.
                    let Some(n) = step(|c| c.editor.as_ref().filter(|(ev, _, _)| *ev == v).map(|(_, n, _)| n.clone())) else { return };
                    actions::pick_files(false, "", move |files| {
                        leptos::task::spawn_local(async move {
                            let embeds = actions::upload_files(&v, files, "attachments").await;
                            actions::insert_embeds(&v, &n, &embeds);
                        });
                    });
                }><Icon name="paperclip"/></button>
            })}
        </div></div>
    }
}

/// Keep the open editor in line with the view mode and vim preference.
fn mode_effects() {
    let ui = ui();
    Effect::new(move |_| {
        let m = ui.mode.get();
        if let Some(ed) = editor() {
            match m {
                ViewMode::Live => ed.set_mode(Mode::Live),
                ViewMode::Source => ed.set_mode(Mode::Source),
                ViewMode::Reading => {}
            }
        }
        state::set_ui_prefs(|p| {
            p.editor_mode = match m {
                ViewMode::Live => "live",
                ViewMode::Source => "source",
                ViewMode::Reading => "reading",
            }
            .into()
        });
    });
    Effect::new(move |_| {
        let vim = ui.ui_prefs.get().vim;
        if let Some(ed) = editor() {
            ed.set_vim(vim);
        }
    });
    // Long lines in code blocks wrap or scroll: the page's class for it
    // changes in the same update, so this waits a moment.
    Effect::new(move |prev: Option<bool>| {
        let wrap = ui.ui_prefs.get().code_wrap;
        if prev.is_some_and(|p| p != wrap) {
            util::set_timeout(0, || {
                if let Some(ed) = editor() {
                    ed.code_styles_changed();
                }
            });
        }
        wrap
    });
}

/// Keep the open note's comment highlights current, and learn who wrote
/// its comments.
fn comment_effects(vault: String, note: String) {
    let ui = ui();
    Effect::new(move |_| {
        ui.comments_rev.track();
        ui.comment_ui.track();
        state::refresh_comment_marks();
    });
    Effect::new(move |_| {
        ui.comments_rev.track();
        leptos::task::spawn_local(state::learn_commenters(vault.clone(), note.clone()));
    });
}

/// The button with the number of the note's open comment threads.
fn comments_button(
    vault: String,
    note: String,
    on_click: impl Fn() + Send + Sync + 'static,
) -> impl IntoView {
    let ui = ui();
    let count = Memo::new(move |_| {
        ui.comments_rev.track();
        ui.content_rev.track();
        comments::open_count(&vault, &note)
    });
    view! {
        <button class="icon-btn cm-count" class:has={move || count.get() > 0}
            title=move || { match count.get() { 0 => "Comments".to_string(), n => util::count(n, "open comment", "open comments") } }
            aria-label="Comments" on:click=move |_| on_click()>
            <Icon name="comment"/>{move || (count.get() > 0).then(|| view! { <span class="badge">{count.get()}</span> })}
        </button>
    }
}

/// Mount an editor for a note into `container` (unless it is already there).
fn mount_editor(
    container: &web_sys::HtmlElement,
    host: std::rc::Rc<NoteHost>,
    ro: bool,
) -> Option<std::rc::Rc<Editor>> {
    let ui = ui();
    let (v, n) = (host.vault.clone(), host.note.clone());
    let mounted = step(|c| {
        c.editor
            .as_ref()
            .filter(|(ev, en, _)| *ev == v && *en == n)
            .map(|(_, _, e)| e.clone())
    });
    if let Some(ed) = mounted
        && container.contains(Some(ed.element()))
    {
        return None;
    }
    let text = step(|c| c.replicas.get(&v).map(|r| r.note_text(&n))).unwrap_or_default();
    let mode = if ui.mode.get_untracked() == ViewMode::Source {
        Mode::Source
    } else {
        Mode::Live
    };
    let vim = ui.ui_prefs.get_untracked().vim;
    container.set_inner_html("");
    let ed = Editor::new(container.unchecked_ref(), text, host, mode, ro, vim);
    let old = step(|c| c.editor.replace((v, n, ed.clone())));
    if let Some((_, _, o)) = old {
        o.destroy();
    }
    state::publish_presence();
    Some(ed)
}

/// Toggle the checkbox of a task line (clicked in the reading view).
fn toggle_task(vault: &str, note: &str, line: usize) {
    let text = step(|c| c.replicas.get(vault).map(|r| r.note_text(note))).unwrap_or_default();
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let Some(l) = lines.get_mut(line) else { return };
    let Some(toggled) = crate::editor::syntax::toggle_task(l) else {
        return;
    };
    *l = toggled;
    let new = lines.join("\n");
    if step(|c| state::set_note_text(c, vault, note, &new)) > 0 {
        if let Some(ed) = editor() {
            ed.set_text(new, false);
        }
        ui().content_rev.update(|x| *x += 1);
    }
}

#[component]
pub fn NoteView(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let host_ref = NodeRef::<html::Div>::new();
    let reading_ref = NodeRef::<html::Article>::new();
    let title_ref = NodeRef::<html::Input>::new();
    let title_text = RwSignal::new(String::new());

    let v = vault.clone();
    let loaded = Memo::new(move |_| ui.vaults.get().iter().any(|x| x.id == v && x.loaded));
    let (v, n2) = (vault.clone(), note.clone());
    let exists = Memo::new(move |_| {
        ui.tree_rev.get();
        state::step(|c| c.entry(&v, &n2)).is_some_and(|e| e.is_live() && e.kind == EntryKind::Note)
    });
    // The note's icon, if it has one (changed from the tree or the menu).
    let (v, n6) = (vault.clone(), note.clone());
    let entry_icon = Memo::new(move |_| {
        ui.tree_rev.get();
        state::step(|c| c.entry(&v, &n6)).and_then(|e| e.icon)
    });
    let (vi, ni) = (vault.clone(), note.clone());
    let open_icon = StoredValue::new(move || {
        let path = state::step(|c| c.entry(&vi, &ni))
            .map(|e| e.path)
            .unwrap_or_default();
        ui.modal.set(Some(Modal::EntryIcon {
            vault: vi.clone(),
            id: Some(ni.clone()),
            path,
            folder: false,
        }));
    });
    let (v, n3) = (vault.clone(), note.clone());
    let trashed = Memo::new(move |_| {
        ui.tree_rev.get();
        state::step(|c| c.entry(&v, &n3)).is_some_and(|e| !e.is_live())
    });
    let v = vault.clone();
    let writable = Memo::new(move |_| {
        ui.vaults.get();
        state::step(|c| c.can_write(&v))
    });

    // Keep the title field in sync with the index (renames from elsewhere).
    let (v, n4) = (vault.clone(), note.clone());
    Effect::new(move |_| {
        ui.tree_rev.get();
        if let Some(e) = state::step(|c| c.entry(&v, &n4)) {
            let focused = title_ref.get().is_some_and(|t| {
                util::document()
                    .active_element()
                    .is_some_and(|a| a.is_same_node(Some(t.unchecked_ref())))
            });
            if !focused {
                title_text.set(e.title().to_string());
            }
        }
    });

    // Mount the editor once the vault is loaded and the container exists.
    let (v, nn) = (vault.clone(), note.clone());
    Effect::new(move |_| {
        let Some(container) = host_ref.get() else {
            return;
        };
        if !loaded.get() || !exists.get() {
            return;
        }
        let ro = !state::step(|c| c.can_write(&v));
        let Some(ed) = mount_editor(&container, NoteHost::new(&v, &nn), ro) else {
            return;
        };
        state::refresh_comment_marks();
        let focus_title = FOCUS_TITLE.with(|f| f.replace(false));
        if focus_title {
            if let Some(t) = title_ref.get() {
                // After the title value has been rendered, select it so typing replaces "Untitled".
                util::set_timeout(30, move || {
                    let _ = t.focus();
                    t.select();
                });
            }
        } else if !ro && ui.mode.get_untracked() != ViewMode::Reading && !util::is_touch() {
            ed.focus();
        }
    });

    mode_effects();
    comment_effects(vault.clone(), note.clone());
    let v = vault.clone();
    Effect::new(move |_| {
        ui.vaults.get();
        if let Some(ed) = editor() {
            ed.set_readonly(!state::step(|c| c.can_write(&v)));
        }
    });

    let (v, n) = (vault.clone(), note.clone());
    on_cleanup(move || {
        let ed = step(|c| {
            if c.editor
                .as_ref()
                .is_some_and(|(ev, en, _)| *ev == v && *en == n)
            {
                c.editor.take()
            } else {
                None
            }
        });
        if let Some((_, _, ed)) = ed {
            ed.destroy();
        }
        step(|c| state::note_closed(c, &v, &n));
    });

    // Reading view HTML.
    let (v, n5) = (vault.clone(), note.clone());
    let reading_html = Memo::new(move |_| {
        if ui.mode.get() != ViewMode::Reading {
            return String::new();
        }
        ui.content_rev.get();
        ui.tree_rev.get();
        let (text, from) = step(|c| {
            (
                c.replicas
                    .get(&v)
                    .map(|r| r.note_text(&n5))
                    .unwrap_or_default(),
                c.entry(&v, &n5).map(|e| e.path).unwrap_or_default(),
            )
        });
        let vv = v.clone();
        let note = move |t: &str| knowledge::resolve_target(&vv, t, &from);
        let va = v.clone();
        let att = move |t: &str| knowledge::resolve_attachment(&va, t);
        render::to_html(
            &text,
            &render::Ctx {
                note: &note,
                attachment: &att,
            },
        )
    });
    // Load images after each render of the reading view.
    let va = vault.clone();
    Effect::new(move |_| {
        let _ = reading_html.get();
        let Some(art) = reading_ref.get() else { return };
        let Ok(imgs) = art.query_selector_all("img[data-att]") else {
            return;
        };
        for i in 0..imgs.length() {
            let Some(img) = imgs
                .item(i)
                .and_then(|n| n.dyn_into::<web_sys::HtmlImageElement>().ok())
            else {
                continue;
            };
            let Some(id) = img.get_attribute("data-att") else {
                continue;
            };
            let v = va.clone();
            leptos::task::spawn_local(async move {
                if let Some(url) = actions::load_attachment(&v, &id).await {
                    img.set_src(&url);
                }
            });
        }
    });

    let (vr, nr) = (vault.clone(), note.clone());
    let on_reading_click = move |ev: web_sys::MouseEvent| {
        let Some(t) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if let Some(a) = t.closest("[data-note]").ok().flatten() {
            ev.prevent_default();
            if let Some(id) = a.get_attribute("data-note") {
                if state::current_route().note() != Some((vr.as_str(), id.as_str())) {
                    state::navigate(Route::Note(vr.clone(), id.clone()));
                }
                if let Some(h) = a.get_attribute("data-heading") {
                    actions::jump_to_heading(vr.clone(), id, h);
                }
            }
        } else if let Some(a) = t.closest("[data-create]").ok().flatten() {
            ev.prevent_default();
            if let Some(name) = a.get_attribute("data-create") {
                let from = state::step(|c| c.entry(&vr, &nr))
                    .map(|e| e.path)
                    .unwrap_or_default();
                actions::open_wiki(&vr, &name, &from);
            }
        } else if let Some(a) = t.closest("[data-tag]").ok().flatten() {
            ev.prevent_default();
            if let Some(tag) = a.get_attribute("data-tag") {
                ui.modal.set(Some(Modal::Search(format!("tag:{tag}"))));
            }
        } else if t.class_list().contains("task") {
            ev.prevent_default();
            if !state::step(|c| c.can_write(&vr)) {
                return;
            }
            let Some(line) = t
                .get_attribute("data-task")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            toggle_task(&vr, &nr, line);
        }
    };

    let (vt, nt) = (vault.clone(), note.clone());
    let commit_title = move || {
        let new = title_text.get_untracked();
        if let Some(e) = state::step(|c| c.entry(&vt, &nt)) {
            if new.trim() == e.title() {
                return;
            }
            if let Err(err) = actions::rename_entry(&vt, &nt, &new) {
                state::error(err);
                title_text.set(e.title().to_string());
            }
        }
    };
    let commit_title2 = commit_title.clone();

    let (vb, nb) = (vault.clone(), note.clone());
    let breadcrumb = move || {
        ui.tree_rev.get();
        let (path, name) = state::step(|c| {
            let path = c.entry(&vb, &nb).map(|e| e.path).unwrap_or_default();
            (path, c.vault_name(&vb))
        });
        let folder = paths::parent(&path).to_string();
        let mut crumbs = vec![name];
        if !folder.is_empty() {
            crumbs.extend(folder.split('/').map(str::to_string));
        }
        crumbs.join(" / ")
    };
    let np = note.clone();
    let others_here = move || {
        ui.presence
            .get()
            .into_iter()
            .filter(|p| p.note.as_deref() == Some(np.as_str()))
            .collect::<Vec<_>>()
    };
    let (vf, nf) = (vault.clone(), note.clone());
    let fav = move || {
        ui.prefs_rev.get();
        actions::is_favorite(&vf, &nf)
    };
    let fav2 = fav.clone();
    let more = RwSignal::new(false);
    super::close_on_outside(more);
    let (vm, nm) = (vault.clone(), note.clone());
    let (vs, vh, nh) = (vault.clone(), vault.clone(), note.clone());
    let (vfa, nfa) = (vault.clone(), note.clone());
    let vtool = vault.clone();
    let vback = vault.clone();
    let (vrp, nrp) = (vault.clone(), note.clone());
    let (vrestore, nrestore) = (vault.clone(), note.clone());
    let (vcm, ncm) = (vault.clone(), note.clone());
    let (vcb, ncb) = (vault.clone(), note.clone());

    view! {
        <div class="note-page">
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{breadcrumb}</div>
                <div class="presence">
                    {move || others_here().into_iter().map(|p| avatar(&p.name, &p.user_id)).collect_view()}
                </div>
                <span class="mobile-only"><SyncStatus/></span>
                <div class="seg" role="group" aria-label="View mode">
                    <button class:on=move || ui.mode.get() == ViewMode::Live title="Live preview" aria-label="Live preview" aria-pressed=move || (ui.mode.get() == ViewMode::Live).to_string() on:click=move |_| ui.mode.set(ViewMode::Live)><Icon name="edit"/></button>
                    <button class:on=move || ui.mode.get() == ViewMode::Source title="Markdown source" aria-label="Markdown source" aria-pressed=move || (ui.mode.get() == ViewMode::Source).to_string() on:click=move |_| ui.mode.set(ViewMode::Source)><Icon name="code"/></button>
                    <button class:on=move || ui.mode.get() == ViewMode::Reading title=format!("Reading view ({}+E)", util::mod_key()) aria-label="Reading view" aria-pressed=move || (ui.mode.get() == ViewMode::Reading).to_string() on:click=move |_| ui.mode.set(ViewMode::Reading)><Icon name="book"/></button>
                </div>
                <button class="icon-btn fav" class:on=fav.clone() title=move || if fav2() { "Remove from favorites" } else { "Add to favorites" } aria-label="Favorite" aria-pressed=move || fav().to_string() on:click=move |_| actions::toggle_favorite(&vfa, &nfa)><Icon name="star"/></button>
                <Show when=move || state::step(|c| c.is_owner(&vs))>
                    {
                        let (v, n) = (vault.clone(), note.clone());
                        let (v2, n2) = (vault.clone(), note.clone());
                        let shared = move || { ui.tree_rev.get(); step(|c| state::is_shared_note(c, &v2, &n2)) };
                        let shared2 = shared.clone();
                        view! { <button class="icon-btn" class:on=shared
                            title=move || if shared2() { "Shared individually — manage who can open this note" } else { "Share this note" }
                            aria-label="Share this note"
                            on:click=move |_| ui.modal.set(Some(Modal::ShareNote(v.clone(), n.clone())))><Icon name="share"/></button> }
                    }
                </Show>
                {comments_button(vcb.clone(), ncb.clone(), move || {
                    let narrow = util::window().inner_width().ok().and_then(|w| w.as_f64()).unwrap_or(1200.0) <= 1100.0;
                    if narrow { ui.panel_overlay.set(true) } else { state::set_ui_prefs(|p| p.right_panel = true) }
                })}
                <button class="icon-btn" title="Version history" aria-label="Version history" on:click=move |_| ui.modal.set(Some(Modal::History(vh.clone(), nh.clone())))><Icon name="history"/></button>
                <button class="icon-btn" class:on=move || ui.ui_prefs.get().right_panel || ui.panel_overlay.get() title=format!("Side panel ({}+\\)", util::mod_key()) aria-label="Side panel"
                    on:click=move |_| {
                        let narrow = util::window().inner_width().ok().and_then(|w| w.as_f64()).unwrap_or(1200.0) <= 1100.0;
                        if narrow { ui.panel_overlay.update(|o| *o = !*o) } else { state::set_ui_prefs(|p| p.right_panel = !p.right_panel) }
                    }><Icon name="panel"/></button>
                <div class="tb-drop">
                    <button class="icon-btn" title="More" aria-label="More" on:click=move |_| more.update(|o| *o = !*o)><Icon name="more"/></button>
                    <Show when=move || more.get()>
                        {
                            let (v, n) = (vm.clone(), nm.clone());
                            let (v2, n2) = (vm.clone(), nm.clone());
                            let (v3, n3) = (vm.clone(), nm.clone());
                            let (v4, n4) = (vm.clone(), nm.clone());
                            let owner = state::step(|c| c.is_owner(&vm));
                            let share = (vm.clone(), nm.clone());
                            view! {
                                <div class="menu right" on:click=move |_| more.set(false)>
                                    <button on:click=move |_| {
                                        let (text, name) = step(|c| (
                                            c.replicas.get(&v).map(|r| r.note_text(&n)).unwrap_or_default(),
                                            c.entry(&v, &n).map(|e| paths::file_name(&e.path).to_string()),
                                        ));
                                        let name = name.unwrap_or_else(|| "note.md".into());
                                        util::download(&name, "text/markdown", text.as_bytes());
                                    }><Icon name="download"/>" Download as Markdown"</button>
                                    <button on:click=move |_| {
                                        let t = state::step(|c| c.entry(&v2, &n2)).map(|e| e.title().to_string()).unwrap_or_default();
                                        util::copy_to_clipboard(&format!("[[{t}]]"));
                                        state::success("Link copied — paste it into another note.");
                                    }><Icon name="link"/>" Copy link to note"</button>
                                    {owner.then(|| view! {
                                        <button on:click=move |_| ui.modal.set(Some(Modal::ShareNote(share.0.clone(), share.1.clone())))><Icon name="share"/>" Share…"</button>
                                    })}
                                    <Show when=move || writable.get()>
                                        {
                                            let (v3, n3) = (v3.clone(), n3.clone());
                                            let (v4, n4) = (v4.clone(), n4.clone());
                                            view! {
                                                <button on:click=move |_| open_icon.with_value(|f| f())><Icon name="smile"/>" Set icon…"</button>
                                                <button on:click=move |_| ui.modal.set(Some(Modal::Move { vault: v3.clone(), id: n3.clone() }))><Icon name="move"/>" Move to…"</button>
                                                <button class="danger" on:click=move |_| { actions::trash(&v4, &n4); state::navigate(Route::Vault(v4.clone())); }><Icon name="trash"/>" Move to trash"</button>
                                            }
                                        }
                                    </Show>
                                </div>
                            }
                        }
                    </Show>
                </div>
            </header>
            <div class="note-body" class:with-panel=move || ui.ui_prefs.get().right_panel class:overlay=move || ui.panel_overlay.get()>
                <div class="note-main" class:wide=move || ui.ui_prefs.get().wide>
                    {move || {
                        if !loaded.get() {
                            return view! { <div class="center-fill"><div class="spinner"></div></div> }.into_any();
                        }
                        if !exists.get() {
                            let vback = vback.clone();
                            let (vr, nr) = (vrestore.clone(), nrestore.clone());
                            return view! {
                                <div class="empty">
                                    {if trashed.get_untracked() {
                                        view! { <p>"This note is in the trash."</p>
                                            <button class="btn" on:click=move |_| actions::restore(&vr, &nr)>"Restore it"</button> }.into_any()
                                    } else {
                                        view! { <p>"This note doesn't exist (anymore)."</p> }.into_any()
                                    }}
                                    <button class="btn ghost" on:click=move |_| state::navigate(Route::Vault(vback.clone()))>"Back to vault"</button>
                                </div>
                            }.into_any();
                        }
                        let c1 = commit_title.clone();
                        let c2 = commit_title2.clone();
                        let on_reading_click = on_reading_click.clone();
                        let vtool = vtool.clone();
                        view! {
                            <div class="note-title-row">
                                // The icon, or for writers a button to add one in its place,
                                // so the title stays put when an icon comes or goes.
                                {move || match entry_icon.get() {
                                    Some(i) => view! {
                                        <button class="note-icon" title="Change icon" aria-label="Change icon" disabled=move || !writable.get()
                                            on:click=move |_| open_icon.with_value(|f| f())>{i}</button>
                                    }.into_any(),
                                    None if writable.get() => view! {
                                        <button class="note-icon add" title="Add an icon" aria-label="Add an icon"
                                            on:click=move |_| open_icon.with_value(|f| f())><Icon name="smile"/></button>
                                    }.into_any(),
                                    None => ().into_any(),
                                }}
                                <input class="note-title" node_ref=title_ref aria-label="Note title" placeholder="Untitled"
                                    readonly=move || !writable.get()
                                    prop:value=move || title_text.get()
                                    on:input=move |ev| title_text.set(event_target_value(&ev))
                                    on:blur=move |_| c1()
                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                        if ev.key() == "Enter" || ev.key() == "ArrowDown" {
                                            ev.prevent_default();
                                            c2();
                                            if let Some(ed) = editor() { ed.focus(); }
                                        }
                                    }/>
                            </div>
                            {move || (!writable.get()).then(|| view! {
                                <div class="readonly-note"><Icon name="eye"/>" You can view this note. Ask the vault owner for edit access."</div>
                            })}
                            {move || (writable.get() && ui.mode.get() != ViewMode::Reading).then(|| view! { <Toolbar vault=Some(vtool.clone())/> })}
                            <div class="editor-area" class:hidden=move || ui.mode.get() == ViewMode::Reading on:click=comments::on_area_click>
                                <div class="editor-host" node_ref=host_ref></div>
                                <CommentLayer vault=vcm.clone() note=ncm.clone()/>
                            </div>
                            <article class="reading" node_ref=reading_ref class:hidden=move || ui.mode.get() != ViewMode::Reading
                                inner_html=move || reading_html.get() on:click=on_reading_click></article>
                        }.into_any()
                    }}
                </div>
                <Show when=move || ui.ui_prefs.get().right_panel || ui.panel_overlay.get()>
                    <RightPanel vault=vrp.clone() note=nrp.clone()/>
                </Show>
            </div>
        </div>
    }
}

/// A note someone shared with us individually. Only the note itself is
/// available: no vault tree, linked notes or attachments.
#[component]
pub fn SharedNoteView(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let host_ref = NodeRef::<html::Div>::new();
    let key = StoredValue::new((vault.clone(), note.clone()));
    let info = Memo::new(move |_| {
        let (v, n) = key.get_value();
        ui.shared
            .get()
            .into_iter()
            .find(|s| s.vault == v && s.note == n)
    });
    let loaded = Memo::new(move |_| info.get().is_some_and(|s| s.loaded));
    let writable = Memo::new(move |_| info.get().is_some_and(|s| s.role.can_write()));

    let (v, n) = (vault.clone(), note.clone());
    Effect::new(move |_| {
        let Some(container) = host_ref.get() else {
            return;
        };
        if !loaded.get() {
            return;
        }
        let ro = !writable.get_untracked();
        let Some(ed) = mount_editor(&container, NoteHost::for_shared(&v, &n), ro) else {
            return;
        };
        state::refresh_comment_marks();
        step(|c| state::send_presence(c));
        if !ro && ui.mode.get_untracked() != ViewMode::Reading && !util::is_touch() {
            ed.focus();
        }
    });
    mode_effects();
    comment_effects(vault.clone(), note.clone());
    Effect::new(move |_| {
        let w = writable.get();
        if let Some(ed) = editor() {
            ed.set_readonly(!w);
        }
    });
    let (v, n) = (vault.clone(), note.clone());
    on_cleanup(move || {
        let ed = step(|c| {
            if c.editor
                .as_ref()
                .is_some_and(|(ev, en, _)| *ev == v && *en == n)
            {
                c.editor.take()
            } else {
                None
            }
        });
        if let Some((_, _, ed)) = ed {
            ed.destroy();
        }
        step(|c| state::note_closed(c, &v, &n));
    });

    let (v, n) = (vault.clone(), note.clone());
    let reading_html = Memo::new(move |_| {
        if ui.mode.get() != ViewMode::Reading {
            return String::new();
        }
        ui.content_rev.get();
        let text = step(|c| c.replicas.get(&v).map(|r| r.note_text(&n))).unwrap_or_default();
        let none = |_: &str| None;
        render::to_html(
            &text,
            &render::Ctx {
                note: &none,
                attachment: &none,
            },
        )
    });
    let (vr, nr) = (vault.clone(), note.clone());
    let on_reading_click = move |ev: web_sys::MouseEvent| {
        let Some(t) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if t.closest("[data-note], [data-create], [data-tag]")
            .ok()
            .flatten()
            .is_some()
        {
            ev.prevent_default();
            state::info("Only this note is shared with you, not the notes it links to.");
        } else if t.class_list().contains("task") {
            ev.prevent_default();
            if writable.get_untracked()
                && let Some(line) = t
                    .get_attribute("data-task")
                    .and_then(|s| s.parse::<usize>().ok())
            {
                toggle_task(&vr, &nr, line);
            }
        }
    };

    let np = note.clone();
    let others_here = move || {
        ui.presence
            .get()
            .into_iter()
            .filter(|p| p.note.as_deref() == Some(np.as_str()))
            .collect::<Vec<_>>()
    };
    let more = RwSignal::new(false);
    super::close_on_outside(more);
    let (vh, nh) = (vault.clone(), note.clone());
    let (vd, nd) = (vault.clone(), note.clone());
    let (vl, nl) = (vault.clone(), note.clone());
    let title = move || info.get().map(|s| s.title).unwrap_or_default();
    let icon = move || info.get().and_then(|s| s.icon);
    let (vcm, ncm) = (vault.clone(), note.clone());
    let (vcl, ncl) = (vault.clone(), note.clone());
    let (vcb, ncb) = (vault.clone(), note.clone());
    let comments_open = RwSignal::new(false);
    super::close_on_outside(comments_open);

    view! {
        <div class="note-page shared-note">
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{move || info.get().map(|s| format!("Shared with you by {}", s.owner)).unwrap_or_default()}</div>
                <div class="presence">
                    {move || others_here().into_iter().map(|p| avatar(&p.name, &p.user_id)).collect_view()}
                </div>
                <span class="mobile-only"><SyncStatus/></span>
                <div class="seg" role="group" aria-label="View mode">
                    <button class:on=move || ui.mode.get() == ViewMode::Live title="Live preview" aria-label="Live preview" aria-pressed=move || (ui.mode.get() == ViewMode::Live).to_string() on:click=move |_| ui.mode.set(ViewMode::Live)><Icon name="edit"/></button>
                    <button class:on=move || ui.mode.get() == ViewMode::Source title="Markdown source" aria-label="Markdown source" aria-pressed=move || (ui.mode.get() == ViewMode::Source).to_string() on:click=move |_| ui.mode.set(ViewMode::Source)><Icon name="code"/></button>
                    <button class:on=move || ui.mode.get() == ViewMode::Reading title=format!("Reading view ({}+E)", util::mod_key()) aria-label="Reading view" aria-pressed=move || (ui.mode.get() == ViewMode::Reading).to_string() on:click=move |_| ui.mode.set(ViewMode::Reading)><Icon name="book"/></button>
                </div>
                <div class="tb-drop">
                    {comments_button(vcb.clone(), ncb.clone(), move || comments_open.update(|o| *o = !*o))}
                    <Show when=move || comments_open.get()>
                        <div class="menu right cm-menu">
                            <CommentsList vault=vcl.clone() note=ncl.clone()/>
                        </div>
                    </Show>
                </div>
                <button class="icon-btn" title="Version history" aria-label="Version history" on:click=move |_| ui.modal.set(Some(Modal::History(vh.clone(), nh.clone())))><Icon name="history"/></button>
                <div class="tb-drop">
                    <button class="icon-btn" title="More" aria-label="More" on:click=move |_| more.update(|o| *o = !*o)><Icon name="more"/></button>
                    <Show when=move || more.get()>
                        {
                            let (v, n) = (vd.clone(), nd.clone());
                            let (v2, n2) = (vl.clone(), nl.clone());
                            view! {
                                <div class="menu right" on:click=move |_| more.set(false)>
                                    <button on:click=move |_| {
                                        let text = step(|c| c.replicas.get(&v).map(|r| r.note_text(&n))).unwrap_or_default();
                                        let name = format!("{}.md", info.get_untracked().map(|s| s.title).unwrap_or_else(|| "note".into()));
                                        util::download(&name, "text/markdown", text.as_bytes());
                                    }><Icon name="download"/>" Download as Markdown"</button>
                                    <button class="danger" on:click=move |_| {
                                        if !util::confirm("Remove this note from your list? You'll lose access until it is shared with you again.") { return; }
                                        let (v, n) = (v2.clone(), n2.clone());
                                        leptos::task::spawn_local(async move { actions::leave_shared_note(&v, &n).await });
                                    }><Icon name="logout"/>" Remove from my list"</button>
                                </div>
                            }
                        }
                    </Show>
                </div>
            </header>
            <div class="note-body">
                <div class="note-main" class:wide=move || ui.ui_prefs.get().wide>
                    {move || {
                        let Some(s) = info.get() else {
                            return view! {
                                <div class="empty">
                                    <p>"This note isn't shared with you (anymore)."</p>
                                    <button class="btn ghost" on:click=move |_| state::navigate(Route::Home)>"Back home"</button>
                                </div>
                            }.into_any();
                        };
                        if let Some(e) = s.error {
                            return view! { <div class="empty"><p>"This note can't be opened right now."</p><p class="muted small">{e}</p></div> }.into_any();
                        }
                        if !loaded.get() {
                            return view! { <div class="center-fill"><div class="spinner"></div></div> }.into_any();
                        }
                        let on_reading_click = on_reading_click.clone();
                        view! {
                            <div class="note-title-row">
                                {move || icon().map(|i| view! { <span class="note-icon" aria-hidden="true">{i}</span> })}
                                <h1 class="note-title static" aria-label="Note title">{title}</h1>
                            </div>
                            {move || (!writable.get()).then(|| view! {
                                <div class="readonly-note"><Icon name="eye"/>" You can view this note. Ask the person who shared it for edit access."</div>
                            })}
                            {move || (writable.get() && ui.mode.get() != ViewMode::Reading).then(|| view! { <Toolbar vault=None/> })}
                            <div class="editor-area" class:hidden=move || ui.mode.get() == ViewMode::Reading on:click=comments::on_area_click>
                                <div class="editor-host" node_ref=host_ref></div>
                                <CommentLayer vault=vcm.clone() note=ncm.clone()/>
                            </div>
                            <article class="reading" class:hidden=move || ui.mode.get() != ViewMode::Reading
                                inner_html=move || reading_html.get() on:click=on_reading_click></article>
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
