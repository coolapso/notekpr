//! Comments on notes (docs/comments.md): the button by selected text,
//! writing a comment, a thread next to its text, and the list of threads.
//!
//! The popups sit in the note's `.editor-area`, at positions taken from the
//! page (the selection, a highlight, a click) when they open.

use coolpsonotes_core::comments::Action;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use super::icons::Icon;
use super::{avatar, focus_on_mount};
use crate::state::{self, CommentUi, ThreadView, ViewMode, step, ui};
use crate::util;

/// The area comment popups are placed in, and its rectangle.
fn area() -> Option<web_sys::DomRect> {
    util::document()
        .query_selector(".editor-area")
        .ok()
        .flatten()
        .map(|e| e.get_bounding_client_rect())
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
    let Some(ed) = open_editor() else { return };
    let (a, h) = ed.selection();
    if a == h {
        state::info("Select the text you want to comment on first.");
        return;
    }
    let allowed = step(|c| {
        c.editor
            .as_ref()
            .is_some_and(|(v, n, _)| state::can_comment(c, v, n))
    });
    if !allowed {
        state::info("You can't comment on this note.");
        return;
    }
    compose(a.min(h), a.max(h));
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

/// Open a thread next to its highlighted text (from the list).
pub fn open_thread(id: &str) {
    let ui = ui();
    if ui.mode.get_untracked() == ViewMode::Reading {
        // Highlights live in the editor.
        ui.mode.set(ViewMode::Live);
    }
    let id = id.to_string();
    // After the editor is shown and its highlights are drawn.
    util::set_timeout(60, move || {
        let mark = util::document()
            .query_selector(&format!(".ed-marks [data-mark=\"{id}\"]"))
            .ok()
            .flatten();
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

#[component]
fn Compose(vault: String, note: String, start: usize, end: usize, x: f64, y: f64) -> impl IntoView {
    let ui = ui();
    let text = RwSignal::new(String::new());
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
    let hint = format!("{}+Enter to comment", util::mod_key());
    view! {
        <div class="cm-pop cm-compose" style=place(x, y)>
            <textarea class="input" rows="3" placeholder="Add a comment…" aria-label="Comment"
                use:focus_on_mount prop:value=move || text.get()
                on:input=move |ev| text.set(event_target_value(&ev))
                on:keydown=move |ev: web_sys::KeyboardEvent| submit_keys(&ev, || submit.with_value(|f| f()), cancel)></textarea>
            <div class="cm-actions">
                <span class="muted small grow">{hint}</span>
                <button class="btn small ghost" on:click=move |_| cancel()>"Cancel"</button>
                <button class="btn small primary" disabled=move || text.get().trim().is_empty()
                    on:click=move |_| submit.with_value(|f| f())>"Comment"</button>
            </div>
        </div>
    }
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
                    {t.comments.iter().cloned().map(|cm| comment_row(cm, editing, confirm, act)).collect_view()}
                </div>
                {can.get().then(|| view! {
                    <div class="cm-reply">
                        <textarea class="input" rows="1" placeholder="Reply…" aria-label="Reply"
                            prop:value=move || reply.get() on:input=move |ev| reply.set(event_target_value(&ev))
                            on:keydown=move |ev: web_sys::KeyboardEvent| submit_keys(&ev, send_reply, close)></textarea>
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
) -> impl IntoView {
    let id = cm.id.clone();
    let (id_e, id_d, id_c) = (id.clone(), id.clone(), id.clone());
    let body = cm.body.clone();
    let is_editing = {
        let id = id.clone();
        move || editing.with(|e| e.as_ref().is_some_and(|(i, _)| *i == id))
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
                    view! {
                        <textarea class="input" rows="2" aria-label="Edit comment" use:focus_on_mount
                            prop:value=move || editing.get().map(|(_, t)| t).unwrap_or_default()
                            on:input=move |ev| editing.update(|e| if let Some((_, t)) = e { *t = event_target_value(&ev) })
                            on:keydown=move |ev: web_sys::KeyboardEvent| submit_keys(&ev, save.clone(), move || editing.set(None))></textarea>
                        <div class="cm-actions">
                            <button class="btn small ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                            <button class="btn small primary" on:click=move |_| save2()>"Save"</button>
                        </div>
                    }.into_any()
                } else {
                    view! { <div class="cm-text">{body.clone()}</div> }.into_any()
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
