//! A text field with suggestions in a menu right under it, for where one may
//! pick a value or type a new one (say a folder). Instead of a `<datalist>`:
//! browsers place its popup as they like (Chromium centres it on wide
//! fields), while this opens under the field like the other menus.

use leptos::prelude::*;

use super::icons::Icon;
use crate::util;

/// `id` is the input's (for a `<label for>`); the list and its options are
/// named after it. The list opens from the ▾, a click into the field, ↓ or
/// typing (then only what matches the text); pick with the mouse or ↑ ↓ and
/// Enter. Escape closes it, not the dialog around it.
#[component]
pub fn SuggestField(
    #[prop(into)] id: String,
    value: RwSignal<String>,
    #[prop(into)] options: Signal<Vec<String>>,
    #[prop(into, optional)] class: String,
    #[prop(into, optional)] placeholder: String,
) -> impl IntoView {
    let open = RwSignal::new(false);
    // Typed since opening: only the options matching the text.
    let narrow = RwSignal::new(false);
    let active = RwSignal::new(None::<usize>);
    let input = NodeRef::<leptos::html::Input>::new();
    let shown = Memo::new(move |_| {
        let all = options.get();
        if !narrow.get() {
            return all;
        }
        let typed = value.get().to_lowercase();
        all.into_iter()
            .filter(|o| o.to_lowercase().contains(&typed))
            .collect()
    });
    let list_id = format!("{id}-list");
    let option_id = StoredValue::new(format!("{id}-"));
    let show_all = move || {
        narrow.set(false);
        let current = value.get_untracked();
        active.set(shown.with_untracked(|s| s.iter().position(|o| *o == current)));
        open.set(true);
    };
    let pick = move |i: usize| {
        if let Some(o) = shown.with_untracked(|s| s.get(i).cloned()) {
            value.set(o);
        }
        open.set(false);
    };
    // The option chosen with the keys stays in sight in a long list.
    Effect::new(move |_| {
        let Some(i) = active.get() else { return };
        let el = util::document().get_element_by_id(&format!("{}{i}", option_id.get_value()));
        if let Some(el) = el {
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
            el.scroll_into_view_with_scroll_into_view_options(&opts);
        }
    });
    let keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
            return;
        }
        let is_open = open.get_untracked() && shown.with_untracked(|s| !s.is_empty());
        match ev.key().as_str() {
            "ArrowDown" | "ArrowUp" if !is_open => {
                ev.prevent_default();
                show_all();
            }
            k @ ("ArrowDown" | "ArrowUp") => {
                ev.prevent_default();
                let n = shown.with_untracked(Vec::len);
                active.update(|a| {
                    *a = Some(match (*a, k == "ArrowDown") {
                        (None, true) => 0,
                        (None, false) => n - 1,
                        (Some(i), true) => (i + 1) % n,
                        (Some(i), false) => (i + n - 1) % n,
                    })
                });
            }
            "Enter" if is_open => {
                if let Some(i) = active.get_untracked() {
                    ev.prevent_default();
                    pick(i);
                }
            }
            "Escape" if is_open => {
                ev.prevent_default();
                open.set(false);
            }
            _ => {}
        }
    };
    let expanded = move || (open.get() && shown.with(|s| !s.is_empty())).to_string();
    let descendant = move || {
        active
            .get()
            .filter(|_| open.get())
            .map(|i| format!("{}{i}", option_id.get_value()))
    };
    view! {
        <div class="suggest">
            <input class=format!("input {class}") id=id node_ref=input placeholder=placeholder
                role="combobox" aria-autocomplete="list" aria-controls=list_id.clone()
                aria-expanded=expanded aria-activedescendant=descendant autocomplete="off"
                prop:value=move || value.get()
                on:input=move |ev| {
                    value.set(event_target_value(&ev));
                    narrow.set(true);
                    active.set(None);
                    open.set(true);
                }
                on:click=move |_| if !open.get_untracked() { show_all() }
                on:blur=move |_| open.set(false)
                on:keydown=keydown/>
            <button type="button" class="suggest-toggle" tabindex="-1" aria-label="Show suggestions"
                on:mousedown=move |ev| {
                    // The field keeps the focus (or gets it).
                    ev.prevent_default();
                    if let Some(el) = input.get_untracked() {
                        let _ = el.focus();
                    }
                    if open.get_untracked() { open.set(false) } else { show_all() }
                }>
                <Icon name="chevron-down"/>
            </button>
            <Show when=move || open.get() && shown.with(|s| !s.is_empty())>
                <div class="menu" id=list_id.clone() role="listbox">
                    {move || shown.get().into_iter().enumerate().map(|(i, o)| {
                        let current = value.with_untracked(|v| *v == o);
                        view! {
                            <button type="button" role="option" tabindex="-1"
                                id=format!("{}{i}", option_id.get_value())
                                class:active=move || active.get() == Some(i)
                                class:current=current
                                aria-selected=move || (active.get() == Some(i)).to_string()
                                on:mouseenter=move |_| active.set(Some(i))
                                on:mousedown=move |ev| { ev.prevent_default(); pick(i); }>
                                <span class="grow ellipsis">{o}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
            </Show>
        </div>
    }
}
