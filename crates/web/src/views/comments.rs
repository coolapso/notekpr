//! Comments on notes (docs/comments.md): the button by selected text,
//! writing a comment, a thread next to its text, and the list of threads —
//! in the editor and in the reading view alike. `@` in a comment's text
//! offers the people who may be mentioned.
//!
//! The popups sit in the note's `.note-content` (the editor and the reading
//! view), at positions taken from the page (the selection, a highlight, a
//! click) when they open.

use leptos::html;
use leptos::prelude::*;
use notekpr_core::comments::{Action, Person};
use wasm_bindgen::JsCast;

use super::icons::Icon;
use super::{avatar, focus_on_mount};
use crate::state::{self, CommentUi, ThreadView, ViewMode, step, ui};
use crate::util;

/// The area comment popups are placed in, and its rectangle.
fn area() -> Option<web_sys::DomRect> {
    util::document()
        .query_selector(".note-content")
        .ok()
        .flatten()
        .map(|e| e.get_bounding_client_rect())
}

/// The id of the thread open on the note, if one is.
pub fn open_thread_id() -> Memo<Option<String>> {
    let ui = ui();
    Memo::new(move |_| match ui.comment_ui.get() {
        CommentUi::Thread { id, .. } => Some(id),
        _ => None,
    })
}

/// The reading view's element for thread `id`'s text (its first part).
fn reading_mark(id: &str) -> Option<web_sys::Element> {
    util::document()
        .query_selector(&format!(".reading [data-thread=\"{id}\"]"))
        .ok()
        .flatten()
}

/// The note on screen (vault, note), whether in its vault or shared with us.
fn shown_note() -> Option<(String, String)> {
    state::current_route()
        .any_note()
        .map(|(v, n)| (v.to_string(), n.to_string()))
}

/// The byte length of the first `units` UTF-16 code units of `s` (the DOM
/// counts text in those).
fn utf16_to_bytes(s: &str, units: usize) -> usize {
    let mut u = 0;
    for (i, c) in s.char_indices() {
        if u >= units {
            return i;
        }
        u += c.len_utf16();
    }
    s.len()
}

/// Where a point of the page's selection is in the note's source, in the
/// reading view: the text node's element says where its text starts
/// (`data-o`, see `render`). A point outside such text moves to the next
/// one (a `start`) or back to the previous one.
fn source_point(
    article: &web_sys::Element,
    node: &web_sys::Node,
    offset: u32,
    start: bool,
) -> Option<usize> {
    let base = |t: &web_sys::Node| -> Option<usize> {
        t.parent_element()?.get_attribute("data-o")?.parse().ok()
    };
    if node.node_type() == web_sys::Node::TEXT_NODE
        && let Some(b) = base(node)
    {
        let text = node.text_content().unwrap_or_default();
        return Some(b + utf16_to_bytes(&text, offset as usize));
    }
    let here = util::document().create_range().ok()?;
    here.set_start(node, offset).ok()?;
    let walker = util::document()
        .create_tree_walker_with_what_to_show(article, 4 /* text */)
        .ok()?;
    let mut found = None;
    while let Ok(Some(t)) = walker.next_node() {
        let Some(b) = base(&t) else { continue };
        let text = t.text_content().unwrap_or_default();
        let len = text.encode_utf16().count() as u32;
        if start {
            if here.compare_point(&t, 0).ok()? >= 0 {
                return Some(b);
            }
        } else if here.compare_point(&t, len).ok()? <= 0 {
            found = Some(b + text.len());
        } else {
            break;
        }
    }
    found
}

/// The reading view's selection as bytes of the note's source.
fn reading_selection() -> Option<(usize, usize)> {
    let article = util::document()
        .query_selector(".note-content .reading")
        .ok()??;
    let sel = util::window().get_selection().ok()??;
    if sel.is_collapsed() || sel.range_count() == 0 {
        return None;
    }
    let r = sel.get_range_at(0).ok()?;
    if !article.contains(Some(&r.common_ancestor_container().ok()?)) {
        return None;
    }
    let start = source_point(
        &article,
        &r.start_container().ok()?,
        r.start_offset().ok()?,
        true,
    )?;
    let end = source_point(
        &article,
        &r.end_container().ok()?,
        r.end_offset().ok()?,
        false,
    )?;
    (start < end).then_some((start, end))
}

/// The page's selection changed (a document listener): in the reading
/// view, offer to comment on selected text, or stop offering.
fn reading_selection_changed() {
    let ui = ui();
    if ui.mode.get_untracked() != ViewMode::Reading {
        return;
    }
    let cur = ui.comment_ui.get_untracked();
    if matches!(cur, CommentUi::Compose { .. }) {
        return;
    }
    let Some((vault, note)) = shown_note() else {
        return;
    };
    let Some((start, end)) = reading_selection() else {
        if matches!(cur, CommentUi::Offer { .. }) {
            ui.comment_ui.set(CommentUi::Closed);
        }
        return;
    };
    if !step(|c| state::can_comment(c, &vault, &note)) {
        return;
    }
    let (Some(r), Some(a)) = (selection_rect(), area()) else {
        return;
    };
    ui.comment_ui.set(CommentUi::Offer {
        start,
        end,
        x: (r.left() + r.right()) / 2.0 - a.left(),
        y: r.top() - a.top(),
    });
}

/// Offer comments on text selected in the reading view (installed once).
pub fn install_reading_selection() {
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |_: web_sys::Event| {
        // After the click that may come with it.
        util::set_timeout(0, reading_selection_changed);
    }) as Box<dyn FnMut(_)>);
    let _ = util::document()
        .add_event_listener_with_callback("selectionchange", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// A click in the reading view that no link took: on highlighted text,
/// open its thread below it.
pub fn reading_clicked(ev: &web_sys::MouseEvent) {
    let selected = util::window()
        .get_selection()
        .ok()
        .flatten()
        .is_some_and(|s| !s.is_collapsed());
    let Some(mark) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|el| el.closest("[data-thread]").ok().flatten())
    else {
        return;
    };
    let (Some(id), Some(a)) = (mark.get_attribute("data-thread"), area()) else {
        return;
    };
    if selected {
        return;
    }
    let r = mark.get_bounding_client_rect();
    ui().comment_ui.set(CommentUi::Thread {
        id,
        x: r.left() - a.left(),
        y: r.bottom() - a.top() + 6.0,
    });
}

/// The rectangle of the page's selection.
fn selection_rect() -> Option<web_sys::DomRect> {
    let sel = util::window().get_selection().ok()??;
    if sel.range_count() == 0 {
        return None;
    }
    Some(sel.get_range_at(0).ok()?.get_bounding_client_rect())
}

fn open_editor() -> Option<std::rc::Rc<crate::editor::Editor>> {
    step(|c| c.editor.as_ref().map(|(_, _, e)| e.clone()))
}

/// The editor's selection changed (called by the note's host): offer to
/// comment on selected text, or stop offering.
pub fn selection_changed(vault: &str, note: &str, anchor: usize, head: usize) {
    let ui = ui();
    let cur = ui.comment_ui.get_untracked();
    if anchor == head {
        if matches!(cur, CommentUi::Offer { .. }) {
            ui.comment_ui.set(CommentUi::Closed);
        }
        return;
    }
    if matches!(cur, CommentUi::Compose { .. }) || !step(|c| state::can_comment(c, vault, note)) {
        return;
    }
    let (Some(r), Some(a)) = (selection_rect(), area()) else {
        return;
    };
    ui.comment_ui.set(CommentUi::Offer {
        start: anchor.min(head),
        end: anchor.max(head),
        x: (r.left() + r.right()) / 2.0 - a.left(),
        y: r.top() - a.top(),
    });
}

/// Start a comment on the selected text (the shortcut).
pub fn compose_from_selection() {
    let ui = ui();
    if let CommentUi::Offer { start, end, .. } = ui.comment_ui.get_untracked() {
        compose(start, end);
        return;
    }
    let selected = if ui.mode.get_untracked() == ViewMode::Reading {
        reading_selection()
    } else {
        open_editor()
            .map(|ed| ed.selection())
            .filter(|(a, h)| a != h)
            .map(|(a, h)| (a.min(h), a.max(h)))
    };
    let Some((start, end)) = selected else {
        state::info("Select the text you want to comment on first.");
        return;
    };
    let allowed = shown_note().is_some_and(|(v, n)| step(|c| state::can_comment(c, &v, &n)));
    if !allowed {
        state::info("You can't comment on this note.");
        return;
    }
    compose(start, end);
}

fn compose(start: usize, end: usize) {
    let (Some(r), Some(a)) = (selection_rect(), area()) else {
        return;
    };
    // Typing before the form has the focus must not replace the selected
    // text in the editor.
    if let Some(ed) = open_editor()
        && let Some(h) = ed.element().dyn_ref::<web_sys::HtmlElement>()
    {
        let _ = h.blur();
    }
    ui().comment_ui.set(CommentUi::Compose {
        start,
        end,
        x: r.left() - a.left(),
        y: r.bottom() - a.top() + 6.0,
    });
}

/// A click in the editor at `(x, y)` (client coordinates): inside
/// highlighted text, open its thread. Run once the click's caret is known
/// (`selectionchange` may come after `click`).
fn editor_clicked(x: f64, y: f64) {
    let Some(ed) = open_editor() else { return };
    let (a, h) = ed.selection();
    if a != h {
        return;
    }
    let Some(id) = ed.marks_at(h).into_iter().next() else {
        return;
    };
    let Some(r) = area() else { return };
    ui().comment_ui.set(CommentUi::Thread {
        id,
        x: x - r.left() - 24.0,
        y: y - r.top() + 16.0,
    });
}

/// Open a thread next to its highlighted text (from the list), in the
/// editor or the reading view, whichever is shown.
pub fn open_thread(id: &str) {
    open_thread_after(id.to_string(), 60, 0);
}

/// [`open_thread`] once its highlight is drawn: tried `tries` more times,
/// every `ms` (a note that is still opening), then by the top of the note.
fn open_thread_after(id: String, ms: i32, tries: u32) {
    let ui = ui();
    util::set_timeout(ms, move || {
        let mark = if ui.mode.get_untracked() == ViewMode::Reading {
            reading_mark(&id)
        } else {
            util::document()
                .query_selector(&format!(".ed-marks [data-mark=\"{id}\"]"))
                .ok()
                .flatten()
        };
        if mark.is_none() && tries > 0 {
            open_thread_after(id, 150, tries - 1);
            return;
        }
        if let Some(m) = &mark {
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Center);
            m.scroll_into_view_with_scroll_into_view_options(&opts);
        }
        // Both measured after scrolling.
        let (x, y) = match (&mark, area()) {
            (Some(m), Some(a)) => {
                let r = m.get_bounding_client_rect();
                (r.left() - a.left(), r.bottom() - a.top() + 6.0)
            }
            _ => (0.0, 0.0),
        };
        ui.comment_ui.set(CommentUi::Thread { id, x, y });
    });
}

/// `left`/`top` for a popup at `(x, y)` in the area, kept within its width.
fn place(x: f64, y: f64) -> String {
    let width = area().map_or(f64::MAX, |a| a.width());
    let x = x.min(width - 360f64.min(width)).max(0.0);
    format!("left:{x:.0}px;top:{y:.0}px")
}

/// The note's floating comment UI: the button by selected text, the form
/// for a new comment, an open thread.
#[component]
pub fn CommentLayer(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    // A click elsewhere (or Escape) closes it.
    Effect::new(move |prev: Option<()>| {
        ui.close_menus.track();
        if prev.is_some() {
            ui.comment_ui.set(CommentUi::Closed);
        }
    });
    on_cleanup(move || ui.comment_ui.set(CommentUi::Closed));
    // A thread to open here (from a mention).
    let here = (vault.clone(), note.clone());
    Effect::new(move |_| {
        let Some((v, n, thread)) = ui.thread_to_open.get() else {
            return;
        };
        if (&v, &n) == (&here.0, &here.1) {
            ui.thread_to_open.set(None);
            open_thread_after(thread, 60, 20);
        }
    });
    move || match ui.comment_ui.get() {
        CommentUi::Closed => ().into_any(),
        CommentUi::Offer { start, end, x, y } => view! {
            <button class="cm-offer" style=place(x, y) on:mousedown=|ev| ev.prevent_default()
                on:click=move |_| compose(start, end)>
                <Icon name="comment"/>" Comment"
            </button>
        }
        .into_any(),
        CommentUi::Compose { start, end, x, y } => {
            view! { <Compose vault=vault.clone() note=note.clone() start=start end=end x=x y=y/> }
                .into_any()
        }
        CommentUi::Thread { id, x, y } => {
            view! { <ThreadCard vault=vault.clone() note=note.clone() id=id x=x y=y/> }.into_any()
        }
    }
}

/// Submit with Ctrl+Enter (⌘+Enter on macs); Escape cancels.
fn submit_keys(ev: &web_sys::KeyboardEvent, submit: impl Fn(), cancel: impl Fn()) {
    if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) {
        ev.prevent_default();
        submit();
    } else if ev.key() == "Escape" {
        ev.prevent_default();
        ev.stop_propagation();
        cancel();
    }
}

/// Who may be mentioned in the note's comments, kept current (asked for
/// when a comment box opens, unless known from the last minute).
fn people_of(vault: String, note: String) -> Signal<Vec<Person>> {
    let ui = ui();
    leptos::task::spawn_local(state::load_mention_people(vault.clone()));
    Memo::new(move |_| {
        ui.comments_rev.track();
        step(|c| state::mention_people(c, &vault, &note))
    })
    .into()
}

#[component]
fn Compose(vault: String, note: String, start: usize, end: usize, x: f64, y: f64) -> impl IntoView {
    let ui = ui();
    let text = RwSignal::new(String::new());
    let people = people_of(vault.clone(), note.clone());
    let submit = StoredValue::new(move || {
        let body = text.get_untracked();
        if body.trim().is_empty() {
            return;
        }
        match state::comment_on(&vault, &note, start, end, &body) {
            Ok(id) => {
                ui.comment_ui.set(CommentUi::Closed);
                open_thread(&id);
            }
            Err(e) => state::error(e),
        }
    });
    let cancel = move || ui.comment_ui.set(CommentUi::Closed);
    let hint = format!("{}+Enter to comment · @ to mention", util::mod_key());
    view! {
        <div class="cm-pop cm-compose" style=place(x, y)>
            <MentionBox text=text people=people placeholder="Add a comment…" label="Comment" rows=3 focus=true
                on_submit=Callback::new(move |_| submit.with_value(|f| f())) on_cancel=Callback::new(move |_| cancel())/>
            <div class="cm-actions">
                <span class="muted small grow">{hint}</span>
                <button class="btn small ghost" on:click=move |_| cancel()>"Cancel"</button>
                <button class="btn small primary" disabled=move || text.get().trim().is_empty()
                    on:click=move |_| submit.with_value(|f| f())>"Comment"</button>
            </div>
        </div>
    }
}

/// Whether `name` is a match for what was typed after `@`.
fn name_matches(name: &str, typed: &str) -> bool {
    let (name, typed) = (name.to_lowercase(), typed.to_lowercase());
    name.starts_with(&typed) || name.split_whitespace().any(|w| w.starts_with(&typed))
}

/// A comment's text box. Typing `@` offers the people who may be
/// mentioned (arrows and Enter or Tab, or a click, pick one); Ctrl+Enter
/// sends, Escape cancels.
#[component]
fn MentionBox(
    text: RwSignal<String>,
    people: Signal<Vec<Person>>,
    #[prop(into)] placeholder: String,
    #[prop(into)] label: String,
    rows: u32,
    #[prop(optional)] focus: bool,
    on_submit: Callback<()>,
    on_cancel: Callback<()>,
) -> impl IntoView {
    let area = NodeRef::<html::Textarea>::new();
    // The `@…` being typed: where its `@` is (bytes) and what follows.
    let typing = RwSignal::new(None::<(usize, String)>);
    let chosen = RwSignal::new(0usize);
    let offered = Memo::new(move |_| {
        let Some((_, typed)) = typing.get() else {
            return Vec::new();
        };
        people
            .get()
            .into_iter()
            .filter(|(_, n)| name_matches(n, &typed))
            .take(6)
            .collect::<Vec<_>>()
    });
    let follow = move || {
        let Some(el) = area.get_untracked() else {
            return;
        };
        let value = el.value();
        let caret = el.selection_start().ok().flatten().unwrap_or(0) as usize;
        let before = &value[..utf16_to_bytes(&value, caret)];
        let now = before.rfind('@').and_then(|at| {
            let typed = &before[at + 1..];
            let starts = before[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '.' | '-' | '+' | '@'));
            (starts && !typed.contains('\n') && typed.chars().count() <= 40)
                .then(|| (at, typed.to_string()))
        });
        if now != typing.get_untracked() {
            chosen.set(0);
        }
        typing.set(now);
    };
    let pick = move |name: String| {
        let (Some((at, typed)), Some(el)) = (typing.get_untracked(), area.get_untracked()) else {
            return;
        };
        let value = el.value();
        let end = (at + 1 + typed.len()).min(value.len());
        let new = format!("{}@{name} {}", &value[..at], &value[end..]);
        let caret = (value[..at].encode_utf16().count() + name.encode_utf16().count() + 2) as u32;
        el.set_value(&new);
        text.set(new);
        let _ = el.set_selection_range(caret, caret);
        let _ = el.focus();
        typing.set(None);
    };
    let keydown = move |ev: web_sys::KeyboardEvent| {
        let n = offered.with_untracked(Vec::len);
        let plain = !ev.ctrl_key() && !ev.meta_key() && !ev.alt_key();
        if n > 0 && plain {
            match ev.key().as_str() {
                "ArrowDown" => {
                    ev.prevent_default();
                    chosen.update(|c| *c = (*c + 1) % n);
                    return;
                }
                "ArrowUp" => {
                    ev.prevent_default();
                    chosen.update(|c| *c = (*c + n - 1) % n);
                    return;
                }
                "Enter" | "Tab" => {
                    ev.prevent_default();
                    let i = chosen.get_untracked().min(n - 1);
                    if let Some((_, name)) = offered.with_untracked(|o| o.get(i).cloned()) {
                        pick(name);
                    }
                    return;
                }
                "Escape" => {
                    ev.prevent_default();
                    ev.stop_propagation();
                    typing.set(None);
                    return;
                }
                _ => {}
            }
        }
        submit_keys(&ev, || on_submit.run(()), || on_cancel.run(()));
    };
    let mounted = move |el: web_sys::Element| {
        if focus {
            focus_on_mount(el);
        }
    };
    view! {
        <div class="mention-box">
            <textarea class="input" node_ref=area rows=rows placeholder=placeholder aria-label=label
                use:mounted prop:value=move || text.get()
                on:input=move |ev| { text.set(event_target_value(&ev)); follow(); }
                on:keyup=move |ev: web_sys::KeyboardEvent| if matches!(ev.key().as_str(), "ArrowLeft" | "ArrowRight" | "Home" | "End") { follow() }
                on:click=move |_| follow()
                on:blur=move |_| typing.set(None)
                on:keydown=keydown></textarea>
            {move || {
                let list = offered.get();
                (!list.is_empty()).then(|| view! {
                    <div class="mention-menu" role="listbox" aria-label="People to mention">
                        {list.into_iter().enumerate().map(|(i, (id, name))| {
                            let n2 = name.clone();
                            view! {
                                <button role="option" class:on=move || chosen.get() == i aria-selected=move || (chosen.get() == i).to_string()
                                    on:mousedown=move |ev| { ev.prevent_default(); pick(n2.clone()); }>
                                    {avatar(&name, &id)}<span>{name.clone()}</span>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                })
            }}
        </div>
    }
}

/// A comment's text, its mentions marked (ours more so).
fn body_view(body: &str, mentions: &[Person], me: &str) -> AnyView {
    let pieces: Vec<(String, Option<String>)> =
        notekpr_core::comments::split_mentions(body, mentions)
            .into_iter()
            .map(|(t, who)| (t.to_string(), who.map(str::to_string)))
            .collect();
    pieces
        .into_iter()
        .map(|(t, who)| match who {
            Some(u) => {
                let mine = u == me;
                view! { <span class="mention" class:me=mine>{t}</span> }.into_any()
            }
            None => t.into_any(),
        })
        .collect_view()
        .into_any()
}

/// One thread, by its text.
#[component]
fn ThreadCard(vault: String, note: String, id: String, x: f64, y: f64) -> impl IntoView {
    let ui = ui();
    let (v, n, tid) = (vault.clone(), note.clone(), id.clone());
    let thread = Memo::new(move |_| {
        ui.comments_rev.track();
        ui.content_rev.track();
        step(|c| {
            state::comment_threads(c, &v, &n)
                .into_iter()
                .find(|t| t.id == tid)
        })
    });
    let (v, n) = (vault.clone(), note.clone());
    let can = Memo::new(move |_| {
        ui.vaults.track();
        ui.shared.track();
        step(|c| state::can_comment(c, &v, &n))
    });
    // Deleted (here or elsewhere): close.
    Effect::new(move |_| {
        if thread.get().is_none() {
            ui.comment_ui.set(CommentUi::Closed);
        }
    });
    // Seen: its mentions of us are read (also those arriving while open).
    let (v, n, t) = (vault.clone(), note.clone(), id.clone());
    Effect::new(move |_| {
        ui.comments_rev.track();
        step(|c| state::mark_thread_read(c, &v, &n, &t));
    });
    let people = people_of(vault.clone(), note.clone());
    let me = step(|c| c.user_id.clone());
    let ctx = StoredValue::new((vault, note, id));
    let act = move |a: Action| {
        ctx.with_value(|(v, n, _)| {
            if let Err(e) = state::send_comment(v, n, a) {
                state::error(e);
            }
        })
    };
    let reply = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<(String, String)>);
    let confirm = RwSignal::new(None::<String>);
    let send_reply = move || {
        let body = reply.get_untracked();
        if body.trim().is_empty() {
            return;
        }
        let thread = ctx.with_value(|(_, _, id)| id.clone());
        act(Action::Reply {
            thread,
            body: body.trim().to_string(),
        });
        reply.set(String::new());
    };
    let close = move || ui.comment_ui.set(CommentUi::Closed);
    move || {
        let Some(t) = thread.get() else {
            return ().into_any();
        };
        let resolved = t.resolved_by.clone();
        let tid = t.id.clone();
        let toggle = {
            let tid = tid.clone();
            move |_| {
                let thread = tid.clone();
                if resolved.is_some() {
                    act(Action::Reopen { thread });
                } else {
                    act(Action::Resolve { thread });
                    close();
                }
            }
        };
        let gone = t.range.is_none();
        view! {
            <div class="cm-pop cm-thread" style=place(x, y) role="dialog" aria-label="Comment thread">
                <div class="cm-head">
                    <span class="cm-quote grow" title=t.quote.clone()>{format!("“{}”", t.quote)}</span>
                    {can.get().then(|| view! {
                        <button class="icon-btn tiny" title=if t.resolved_by.is_some() { "Reopen" } else { "Resolve" }
                            aria-label=if t.resolved_by.is_some() { "Reopen" } else { "Resolve" } on:click=toggle>
                            <Icon name=if t.resolved_by.is_some() { "undo" } else { "check" }/></button>
                    })}
                    <button class="icon-btn tiny" title="Close" aria-label="Close" on:click=move |_| close()><Icon name="x"/></button>
                </div>
                {gone.then(|| view! { <div class="muted small cm-gone">"The text this was about has been deleted."</div> })}
                {t.resolved_by.clone().map(|who| view! { <div class="muted small">{format!("Resolved by {who}")}</div> })}
                <div class="cm-list">
                    {t.comments.iter().cloned().map(|cm| comment_row(cm, editing, confirm, act, people, me.clone())).collect_view()}
                </div>
                {can.get().then(|| view! {
                    <div class="cm-reply">
                        <MentionBox text=reply people=people placeholder="Reply…" label="Reply" rows=1
                            on_submit=Callback::new(move |_| send_reply()) on_cancel=Callback::new(move |_| close())/>
                        <button class="btn small primary" disabled=move || reply.get().trim().is_empty()
                            on:click=move |_| send_reply()>"Reply"</button>
                    </div>
                })}
            </div>
        }
        .into_any()
    }
}

fn comment_row(
    cm: state::CommentView,
    editing: RwSignal<Option<(String, String)>>,
    confirm: RwSignal<Option<String>>,
    act: impl Fn(Action) + Copy + Send + Sync + 'static,
    people: Signal<Vec<Person>>,
    me: String,
) -> impl IntoView {
    let id = cm.id.clone();
    let (id_e, id_d, id_c) = (id.clone(), id.clone(), id.clone());
    let body = cm.body.clone();
    let mentions = cm.mentions.clone();
    // A memo: typing in the box changes `editing`, not whether it is shown.
    let is_editing = {
        let id = id.clone();
        let m = Memo::new(move |_| editing.with(|e| e.as_ref().is_some_and(|(i, _)| *i == id)));
        move || m.get()
    };
    let is_confirming = {
        let id = id.clone();
        move || confirm.with(|c| c.as_deref() == Some(id.as_str()))
    };
    let save = {
        let id = id.clone();
        move || {
            if let Some((_, text)) = editing.get_untracked()
                && !text.trim().is_empty()
            {
                act(Action::Edit {
                    target: id.clone(),
                    body: text.trim().to_string(),
                });
            }
            editing.set(None);
        }
    };
    let save2 = save.clone();
    let meta = {
        let mut m = util::relative_time(cm.created);
        if cm.edited {
            m.push_str(" · edited");
        }
        if cm.pending {
            m.push_str(" · sending…");
        }
        m
    };
    view! {
        <div class="cm-item" data-comment=id.clone()>
            {avatar(&cm.name, &cm.author)}
            <div class="cm-body grow">
                <div class="cm-meta">
                    <b>{cm.name.clone()}</b>" "<span class="muted small">{meta}</span>
                    {(!cm.verified).then(|| view! {
                        <span class="cm-unverified small" title="We couldn't confirm who wrote this: their key isn't known here yet.">" unverified"</span>
                    })}
                    {(cm.mine || cm.deletable).then(|| view! {
                        <span class="cm-tools">
                            {cm.mine.then(|| {
                                let (id, body) = (id_e.clone(), body.clone());
                                view! { <button class="linkish small" on:click=move |_| editing.set(Some((id.clone(), body.clone())))>"Edit"</button> }
                            })}
                            {cm.deletable.then(|| {
                                let id = id_d.clone();
                                view! { <button class="linkish small danger" on:click=move |_| confirm.set(Some(id.clone()))>"Delete"</button> }
                            })}
                        </span>
                    })}
                </div>
                {move || if is_editing() {
                    let save = save.clone();
                    let save2 = save2.clone();
                    // The box's text, kept in `editing`.
                    let draft = RwSignal::new(editing.get_untracked().map(|(_, t)| t).unwrap_or_default());
                    Effect::new(move |_| {
                        let t = draft.get();
                        editing.update(|e| if let Some((_, d)) = e { *d = t });
                    });
                    view! {
                        <MentionBox text=draft people=people label="Edit comment" placeholder="" rows=2 focus=true
                            on_submit=Callback::new(move |_| save()) on_cancel=Callback::new(move |_| editing.set(None))/>
                        <div class="cm-actions">
                            <button class="btn small ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                            <button class="btn small primary" on:click=move |_| save2()>"Save"</button>
                        </div>
                    }.into_any()
                } else {
                    view! { <div class="cm-text">{body_view(&body, &mentions, &me)}</div> }.into_any()
                }}
                {move || is_confirming().then(|| {
                    let id = id_c.clone();
                    view! {
                        <div class="cm-confirm small">
                            "Delete this comment?"
                            <button class="btn small danger" on:click=move |_| { act(Action::Delete { target: id.clone() }); confirm.set(None); }>"Delete"</button>
                            <button class="btn small ghost" on:click=move |_| confirm.set(None)>"Cancel"</button>
                        </div>
                    }
                })}
            </div>
        </div>
    }
}

/// The note's threads (open ones first, by where they are in the text).
fn sorted_threads(vault: &str, note: &str) -> Vec<ThreadView> {
    let mut t = step(|c| state::comment_threads(c, vault, note));
    t.sort_by_key(|t| (t.resolved_by.is_some(), t.range.map_or(usize::MAX, |r| r.0)));
    t
}

/// The list of a note's threads (the side panel).
#[component]
pub fn CommentsList(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let threads = Memo::new(move |_| {
        ui.comments_rev.track();
        ui.content_rev.track();
        sorted_threads(&vault, &note)
    });
    let show_resolved = RwSignal::new(false);
    move || {
        let all = threads.get();
        if all.is_empty() {
            return view! { <p class="muted small cm-none">"No comments. Select text in the note to comment on it."</p> }.into_any();
        }
        let resolved = all.iter().filter(|t| t.resolved_by.is_some()).count();
        let item = |t: ThreadView| {
            let id = t.id.clone();
            let first = t.comments.first().cloned();
            let replies = t.comments.len().saturating_sub(1);
            view! {
                <button class="panel-item cm-entry" class:resolved=t.resolved_by.is_some() on:click=move |_| open_thread(&id)>
                    <span class="cm-quote">{format!("“{}”", t.quote)}</span>
                    {first.map(|f| view! { <span class="pi-snip"><b>{f.name}</b>{format!(": {}", f.body)}</span> })}
                    {(replies > 0).then(|| view! { <span class="muted small">{util::count(replies, "reply", "replies")}</span> })}
                </button>
            }
        };
        let (open, done): (Vec<ThreadView>, Vec<ThreadView>) =
            all.into_iter().partition(|t| t.resolved_by.is_none());
        view! {
            <div class="cm-entries">{open.into_iter().map(item).collect_view()}</div>
            {(resolved > 0).then(|| view! {
                <button class="linkish small" on:click=move |_| show_resolved.update(|s| *s = !*s)>
                    {move || format!("{} resolved ({resolved})", if show_resolved.get() { "Hide" } else { "Show" })}
                </button>
            })}
            {move || show_resolved.get().then(|| view! { <div class="cm-entries">{done.clone().into_iter().map(item).collect_view()}</div> })}
        }
        .into_any()
    }
}

/// How many open threads the note has (the header's button).
pub fn open_count(vault: &str, note: &str) -> usize {
    step(|c| {
        state::comment_threads(c, vault, note)
            .iter()
            .filter(|t| t.resolved_by.is_none())
            .count()
    })
}

/// `on:click` of the editor's container.
pub fn on_area_click(ev: web_sys::MouseEvent) {
    if ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|el| el.closest(".cm-pop, .cm-offer").ok().flatten().is_some())
    {
        return;
    }
    let (x, y) = (f64::from(ev.client_x()), f64::from(ev.client_y()));
    util::set_timeout(20, move || editor_clicked(x, y));
}
