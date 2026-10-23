//! Choosing how wide notes are (`crate::width`): the default in Settings →
//! Editor, a note's own width in its ⋯ menu.

use leptos::prelude::*;

use crate::state::{self, step, ui};
use crate::width::{NoteWidth, STEP};

/// How wide `note` is shown, for its page.
pub fn note_width(note: String) -> Memo<NoteWidth> {
    let ui = ui();
    Memo::new(move |_| {
        ui.prefs_rev.get();
        let local = ui.ui_prefs.with(|p| p.note_width);
        step(|c| state::note_width(c, local, &note))
    })
}

/// A slider from narrow to wide; the words at its ends jump there (and
/// light up there), and in between it says how far along it is.
#[component]
pub fn WidthSlider(value: Signal<NoteWidth>, on_change: Callback<NoteWidth>) -> impl IntoView {
    let at = move |w: NoteWidth| value.get() == w;
    view! {
        <div class="width-slider">
            <button class="width-end" class:on=move || at(NoteWidth::NARROW) title="Narrow: a comfortable line length" on:click=move |_| on_change.run(NoteWidth::NARROW)>"Narrow"</button>
            <input type="range" min="0" max="100" step=STEP.to_string() aria-label="Note width"
                aria-valuetext=move || value.get().label()
                prop:value=move || value.get().percent().to_string()
                on:input=move |ev| {
                    if let Ok(p) = event_target_value(&ev).parse::<f64>() {
                        on_change.run(NoteWidth::new(p));
                    }
                }/>
            <button class="width-end" class:on=move || at(NoteWidth::WIDE) title="Wide: all the room there is" on:click=move |_| on_change.run(NoteWidth::WIDE)>"Wide"</button>
            <output class="width-value">{move || (!at(NoteWidth::NARROW) && !at(NoteWidth::WIDE)).then(|| value.get().label())}</output>
        </div>
    }
}

/// A note's own width, in its ⋯ menu. Only for us (synced to our devices):
/// readers can set it too.
#[component]
pub fn NoteWidthMenu(note: String) -> impl IntoView {
    let ui = ui();
    let note = StoredValue::new(note);
    let shown = note_width(note.get_value());
    let own = Memo::new(move |_| {
        ui.prefs_rev.get();
        note.with_value(|n| step(|c| state::own_width(c, n)))
    });
    let default = Memo::new(move |_| {
        ui.prefs_rev.get();
        let local = ui.ui_prefs.with(|p| p.note_width);
        step(|c| state::default_width(c, local))
    });
    let set =
        move |w: Option<NoteWidth>| note.with_value(|n| step(|c| state::set_note_width(c, n, w)));
    view! {
        <div class="menu-sep"></div>
        <div class="menu-label">"Width (only for you)"</div>
        // Changing it leaves the menu open, to see the note change.
        <div class="menu-width" on:click=|ev| ev.stop_propagation()>
            <WidthSlider value=shown.into() on_change=Callback::new(move |w| set(Some(w)))/>
            {move || own.get().is_some().then(|| view! {
                <button class="width-default" on:click=move |_| set(None)>
                    {move || format!("Use the default ({})", default.get().label().to_lowercase())}
                </button>
            })}
        </div>
    }
}
