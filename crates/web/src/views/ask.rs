//! The app's own dialog for questions (`crate::ask`): a layer of its own
//! over the modals and menus, so it can ask about something in an open
//! dialog. Enter confirms; Escape, Cancel and a click outside don't.

use std::cell::RefCell;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::ask::{self, Question};
use crate::state::{self, ui};
use crate::util;

thread_local! {
    /// What had the focus before the question; it gets it back afterwards.
    static RETURN_FOCUS: RefCell<Option<web_sys::HtmlElement>> = const { RefCell::new(None) };
}

#[component]
pub fn AskHost() -> impl IntoView {
    let ui = ui();
    // Signed out (or the like) while asking: what it asked about can't go
    // on.
    Effect::new(move |was: Option<std::mem::Discriminant<state::Phase>>| {
        let now = ui.phase.with(std::mem::discriminant);
        if was.is_some_and(|w| w != now) {
            ask::cancel();
        }
        now
    });
    // Answered: the focus goes back where it was (if that is still there).
    Effect::new(move |_| {
        if ui.asking.with(Option::is_none)
            && let Some(el) = RETURN_FOCUS.with(|r| r.borrow_mut().take())
            && el.is_connected()
        {
            let _ = el.focus();
        }
    });
    move || ui.asking.get().map(|q| view! { <Asking q=q/> })
}

/// Remember what has the focus, unless it is in the question (one question
/// replacing another keeps what the first one remembered).
fn remember_focus() {
    RETURN_FOCUS.with(|r| {
        let mut r = r.borrow_mut();
        if r.is_none() {
            *r = util::document()
                .active_element()
                .filter(|a| a.closest(".ask").ok().flatten().is_none())
                .and_then(|a| a.dyn_into().ok());
        }
    });
}

#[component]
fn Asking(q: Question) -> impl IntoView {
    // Before the dialog takes the focus.
    remember_focus();
    let Question {
        id,
        title,
        text,
        button,
        danger,
        field,
    } = q;
    let typed = RwSignal::new(field.as_ref().map(|f| f.value.clone()).unwrap_or_default());
    let rule = StoredValue::new(field.clone());
    let can = move || rule.with_value(|f| f.as_ref().is_none_or(|f| typed.with(|t| f.accepts(t))));
    let yes = move || {
        if rule.with_value(|f| {
            f.as_ref()
                .is_none_or(|f| typed.with_untracked(|t| f.accepts(t)))
        }) {
            ask::answer(id, Some(typed.get_untracked()));
        }
    };
    let no = move || ask::answer(id, None);
    let root = NodeRef::<html::Div>::new();
    let input = NodeRef::<html::Input>::new();
    let confirm = NodeRef::<html::Button>::new();
    // The field takes the focus (its text selected, to type anew), else the
    // confirm button: Enter confirms, Tab reaches Cancel.
    Effect::new(move |_| {
        if let Some(i) = input.get() {
            let _ = i.focus();
            i.select();
        } else if let Some(b) = confirm.get() {
            let _ = b.focus();
        }
    });
    let title_id = format!("ask-title-{id}");
    let text_id = (!text.is_empty()).then(|| format!("ask-text-{id}"));
    let (labelled_by, described_by) = (title_id.clone(), text_id.clone());
    let paragraphs = text
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| view! { <p>{p.trim().to_string()}</p> })
        .collect_view();
    view! {
        <div class="modal-back ask-back" on:mousedown=move |_| no()>
            <div class="modal ask" class:danger=danger role="alertdialog" aria-modal="true"
                aria-labelledby=labelled_by aria-describedby=described_by tabindex="-1" node_ref=root
                on:mousedown=|ev| ev.stop_propagation()
                on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                    "Escape" => {
                        ev.prevent_default();
                        ev.stop_propagation();
                        no();
                    }
                    "Tab" => {
                        if let Some(el) = root.get() {
                            super::dialogs::trap_focus(&el, &ev);
                        }
                    }
                    // Buttons and the field do their own (the field submits).
                    "Enter" if !on_control(&ev) => {
                        ev.prevent_default();
                        yes();
                    }
                    _ => {}
                }>
                <form class="ask-form" on:submit=move |ev| { ev.prevent_default(); yes(); }>
                    <h2 id=title_id>{title}</h2>
                    {text_id.map(|tid| view! { <div class="ask-text" id=tid>{paragraphs}</div> })}
                    {field.map(|f| view! {
                        <label class="field ask-field">
                            {(!f.label.is_empty()).then(|| view! { <span class="small">{f.label}</span> })}
                            <input class="input ask-input" node_ref=input autocomplete="off" spellcheck="false"
                                placeholder=f.placeholder prop:value=move || typed.get()
                                on:input=move |ev| typed.set(event_target_value(&ev))/>
                        </label>
                    })}
                    <div class="ask-buttons">
                        <button type="button" class="btn ask-no" on:click=move |_| no()>"Cancel"</button>
                        <button type="submit" class=if danger { "btn danger strong ask-yes" } else { "btn primary ask-yes" }
                            node_ref=confirm disabled=move || !can()>{button}</button>
                    </div>
                </form>
            </div>
        </div>
    }
}

/// The key went to a button or the field.
fn on_control(ev: &web_sys::KeyboardEvent) -> bool {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|el| matches!(el.tag_name().as_str(), "BUTTON" | "INPUT"))
}
