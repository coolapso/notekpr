//! A drawing's page. A document route (`/v/<vault>/<id>`) shows a note or
//! a drawing: which one is only known once the vault's index is there, so
//! [`Document`] picks the view then (the note view never opens an editor on
//! anything but a note).

use std::cell::Cell;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use notekpr_core::doc::EntryKind;
use notekpr_core::paths;
use notekpr_draw::color::palette;
use notekpr_draw::edit::{Tool, snap};
use notekpr_draw::scene::ElementType;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;

use super::icons::Icon;
use crate::drawing::Board;
use crate::drawing::editing::{Export, TextBox, mods};
use crate::state::{self, ui};

/// A note or a drawing, by id.
#[component]
pub fn Document(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let (v, n) = (vault.clone(), note.clone());
    let drawing = Memo::new(move |_| {
        ui.tree_rev.get();
        ui.vaults.track();
        state::step(|c| c.entry(&v, &n)).is_some_and(|e| e.kind == EntryKind::Drawing)
    });
    move || {
        let (vault, note) = (vault.clone(), note.clone());
        if drawing.get() {
            view! { <DrawingView vault=vault id=note/> }.into_any()
        } else {
            view! { <super::note::NoteView vault=vault note=note/> }.into_any()
        }
    }
}

/// The fonts the picker's list offers, as Excalidraw names them.
const MORE_FONTS: [(u32, &str); 8] = [
    (notekpr_draw::fonts::EXCALIFONT, "Excalifont"),
    (notekpr_draw::fonts::NUNITO, "Nunito"),
    (notekpr_draw::fonts::COMIC_SHANNS, "Comic Shanns"),
    (notekpr_draw::fonts::LILITA_ONE, "Lilita One"),
    (notekpr_draw::fonts::VIRGIL, "Virgil"),
    (notekpr_draw::fonts::HELVETICA, "Helvetica"),
    (notekpr_draw::fonts::CASCADIA, "Cascadia"),
    (notekpr_draw::fonts::LIBERATION_SANS, "Liberation Sans"),
];

/// The tools of the toolbar: icon, name, shortcut.
const TOOLS: [(Tool, &str, &str, &str); 14] = [
    (Tool::Hand, "hand", "Hand (panning tool)", "H"),
    (Tool::Selection, "pointer", "Selection", "V or 1"),
    (Tool::Rectangle, "square", "Rectangle", "R or 2"),
    (Tool::Diamond, "diamond", "Diamond", "D or 3"),
    (Tool::Ellipse, "circle", "Ellipse", "O or 4"),
    (Tool::Arrow, "arrow", "Arrow", "A or 5"),
    (Tool::Line, "line", "Line", "L or 6"),
    (Tool::Freedraw, "pen", "Draw", "P or 7"),
    (Tool::Text, "text-tool", "Text", "T or 8"),
    (Tool::Image, "image", "Insert image", "9"),
    (Tool::StickyNote, "sticky-note", "Sticky note", "N"),
    (Tool::Eraser, "eraser", "Eraser", "E or 0"),
    (Tool::Frame, "frame", "Frame tool", "F"),
    (Tool::Laser, "laser", "Laser pointer", "K"),
];

/// Fingers on the canvas, by pointer, and the pinch two of them make:
/// how far apart they began, their middle then, and the view then.
#[derive(Default)]
struct Touches {
    at: std::collections::HashMap<i32, [f64; 2]>,
    pinch: Option<(f64, [f64; 2], notekpr_draw::viewport::Viewport)>,
}

impl Touches {
    fn middle_and_spread(&self) -> Option<([f64; 2], f64)> {
        let mut it = self.at.values();
        let (a, b) = (it.next()?, it.next()?);
        Some((
            [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0],
            (a[0] - b[0]).hypot(a[1] - b[1]),
        ))
    }

    /// A finger went down; with a second, a pinch starts (true).
    fn down(&mut self, id: i32, p: [f64; 2], view: notekpr_draw::viewport::Viewport) -> bool {
        self.at.insert(id, p);
        if self.at.len() == 2
            && let Some((m, d)) = self.middle_and_spread()
        {
            self.pinch = Some((d.max(1.0), m, view));
            return true;
        }
        false
    }

    /// A finger moved: the view the pinch makes, if one goes on
    /// (zoomed by how far apart the fingers are, moved with their middle).
    fn moved(&mut self, id: i32, p: [f64; 2]) -> Option<notekpr_draw::viewport::Viewport> {
        if let Some(q) = self.at.get_mut(&id) {
            *q = p;
        }
        let (d0, m0, v0) = self.pinch?;
        let (m, d) = self.middle_and_spread()?;
        let v = v0.zoomed_at(v0.zoom * d / d0, m0);
        Some(v.scrolled(m[0] - m0[0], m[1] - m0[1]))
    }

    /// A finger went up; whether it was part of a pinch.
    fn up(&mut self, id: i32) -> bool {
        self.at.remove(&id);
        let pinched = self.pinch.is_some();
        if self.at.is_empty() {
            self.pinch = None;
        }
        pinched
    }
}

/// The pointer's position on the canvas.
fn at(ev: &web_sys::PointerEvent) -> [f64; 2] {
    [ev.offset_x() as f64, ev.offset_y() as f64]
}

#[component]
pub fn DrawingView(vault: String, id: String) -> impl IntoView {
    let ui = ui();
    let id_here = StoredValue::new(id.clone());
    let (v, i) = (vault.clone(), id.clone());
    state::step(|c| c.ensure_note_loaded(&v, &i));
    let (v, i) = (vault.clone(), id.clone());
    let crumbs = Memo::new(move |_| {
        ui.tree_rev.get();
        let path = state::step(|c| c.entry(&v, &i))
            .map(|e| e.path)
            .unwrap_or_default();
        let mut crumbs = vec![paths::drawing_title(&path).to_string()];
        let folder = paths::parent(&path);
        if !folder.is_empty() {
            crumbs.extend(folder.split('/').map(str::to_string));
        }
        crumbs.join(" / ")
    });
    let board = Board::new(&vault, &id);
    let b = StoredValue::new_local(board.clone());
    let canvas_ref = NodeRef::<html::Canvas>::new();
    let root_ref = NodeRef::<html::Div>::new();
    Effect::new(move |_| {
        if let Some(c) = canvas_ref.get() {
            b.with_value(|b| b.attach(c.unchecked_into()));
        }
    });
    // Opened: the keyboard's shortcuts work at once.
    Effect::new(move |_| {
        if let Some(r) = root_ref.get() {
            let busy = crate::util::document()
                .active_element()
                .is_some_and(|a| matches!(a.tag_name().as_str(), "INPUT" | "TEXTAREA"));
            if !busy {
                let _ = r.focus();
            }
        }
    });
    // Drawn again when it changes elsewhere, or the theme does.
    Effect::new(move |_| {
        ui.content_rev.get();
        ui.ui_prefs.track();
        ui.vaults.track();
        b.with_value(|b| b.reload());
    });
    on_cleanup(move || {
        b.try_with_value(|b| b.close());
    });
    // The panels follow the editor.
    let rev = RwSignal::new(0u64);
    board.on_change(move || rev.update(|r| *r += 1));
    let zoom = RwSignal::new(1.0);
    let grid = RwSignal::new(false);
    board.on_zoom(move |z| {
        if zoom.get_untracked() != z {
            zoom.set(z);
        }
        // The grid is the drawing's: anyone drawing it turns it on or off.
        let on = b.with_value(|b| b.grid_on());
        if grid.get_untracked() != on {
            grid.set(on);
        }
    });
    let shown_zoom = move || format!("{}%", (zoom.get() * 100.0).round());
    let writable = Memo::new(move |_| {
        ui.vaults.track();
        b.with_value(|b| b.writable())
    });
    let tool = move || {
        rev.get();
        b.with_value(|b| b.editor.borrow().tool)
    };
    // Text being edited: its box over the canvas.
    let editing = RwSignal::new(None::<TextBox>);
    let text_ref = NodeRef::<html::Textarea>::new();
    board.on_edit_text(move |id| {
        let tb = if id.is_empty() {
            None
        } else {
            b.with_value(|b| b.text_box(&id))
        };
        editing.set(tb.clone());
        if let Some(tb) = tb {
            b.with_value(|b| b.set_text_draft(tb.text.clone()));
            crate::util::set_timeout(0, move || {
                if let Some(t) = text_ref.get_untracked() {
                    t.set_value(&tb.text);
                    let _ = t.focus();
                    t.select();
                }
            });
        }
    });
    let finish_text = move || {
        if editing.get_untracked().is_none() {
            return;
        }
        let text = text_ref
            .get_untracked()
            .map(|t| t.value())
            .unwrap_or_default();
        b.with_value(|b| b.finish_text(&text));
        if let Some(r) = root_ref.get_untracked() {
            let _ = r.focus();
        }
    };
    // Space held: the hand, for as long.
    let space = Rc::new(Cell::new(false));
    let panning = Rc::new(Cell::new(None::<(i32, f64, f64)>));
    let (sp, pn) = (space.clone(), panning.clone());
    // Two fingers pinch to zoom and move the view.
    let touches = StoredValue::new_local(Touches::default());
    let on_down = move |ev: web_sys::PointerEvent| {
        if let Some(r) = root_ref.get_untracked() {
            let _ = r.focus();
        }
        if ev.pointer_type() == "touch" {
            let view = b.with_value(|b| b.view.get());
            let started = touches
                .try_update_value(|t| t.down(ev.pointer_id(), at(&ev), view))
                .unwrap_or(false);
            if started {
                // What the first finger began ends where it is.
                b.with_value(|b| b.pointer_up(at(&ev)));
                return;
            }
            if touches.with_value(|t| t.pinch.is_some()) {
                return;
            }
        }
        if let Some(t) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = t.set_pointer_capture(ev.pointer_id());
        }
        let hand = b.with_value(|b| b.editor.borrow().tool == Tool::Hand);
        if ev.button() == 1 || hand || sp.get() || !writable.get_untracked() {
            pn.set(Some((
                ev.pointer_id(),
                ev.client_x() as f64,
                ev.client_y() as f64,
            )));
            return;
        }
        if ev.button() != 0 {
            return;
        }
        let pressure = (ev.pointer_type() == "pen").then(|| ev.pressure() as f64);
        let m = mods(ev.shift_key(), ev.alt_key(), ev.ctrl_key() || ev.meta_key());
        b.with_value(|b| b.pointer_down(at(&ev), pressure, m));
    };
    let pm = panning.clone();
    let on_move = move |ev: web_sys::PointerEvent| {
        if ev.pointer_type() == "touch"
            && let Some(v) = touches
                .try_update_value(|t| t.moved(ev.pointer_id(), at(&ev)))
                .flatten()
        {
            b.with_value(|b| {
                b.view.set(v);
                b.schedule();
            });
            return;
        }
        if let Some((pid, x, y)) = pm.get()
            && pid == ev.pointer_id()
        {
            let (nx, ny) = (ev.client_x() as f64, ev.client_y() as f64);
            b.with_value(|b| {
                b.view.set(b.view.get().scrolled(nx - x, ny - y));
                b.schedule();
            });
            pm.set(Some((pid, nx, ny)));
            return;
        }
        let pressure = (ev.pointer_type() == "pen").then(|| ev.pressure() as f64);
        let m = mods(ev.shift_key(), ev.alt_key(), ev.ctrl_key() || ev.meta_key());
        b.with_value(|b| {
            b.note_pointer(at(&ev));
            b.pointer_move(at(&ev), pressure, m);
            if let Some(c) = canvas_ref.get_untracked() {
                let el: &web_sys::HtmlElement = c.unchecked_ref();
                let _ = el.style().set_property("cursor", b.cursor_at(at(&ev)));
            }
        });
    };
    let pu = panning.clone();
    let on_up = move |ev: web_sys::PointerEvent| {
        if ev.pointer_type() == "touch"
            && touches
                .try_update_value(|t| t.up(ev.pointer_id()))
                .unwrap_or(false)
        {
            return;
        }
        if pu.take().is_some() {
            return;
        }
        b.with_value(|b| b.pointer_up(at(&ev)));
    };
    let (sd, su) = (space.clone(), space.clone());
    let on_key = move |ev: web_sys::KeyboardEvent| {
        if editing.get_untracked().is_some() {
            return;
        }
        let key = ev.key();
        let ctrl = ev.ctrl_key() || ev.meta_key();
        let shift = ev.shift_key();
        let handled = b.with_value(|b| -> bool {
            let w = writable.get_untracked();
            match (ctrl, key.as_str()) {
                (false, " ") => {
                    sd.set(true);
                    true
                }
                (_, "Enter") if w => {
                    b.enter(ctrl);
                    true
                }
                (false, "Escape") => {
                    b.command(|ed, s, r| ed.escape(s, r));
                    true
                }
                (false, "Delete" | "Backspace") if w => {
                    b.command(|ed, s, r| ed.delete_selected(s, r));
                    true
                }
                (false, "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown") if w => {
                    let grid = snap::grid_size(&b.scene(), false);
                    let step = snap::nudge_step(grid, shift);
                    let (dx, dy) = match key.as_str() {
                        "ArrowLeft" => (-step, 0.0),
                        "ArrowRight" => (step, 0.0),
                        "ArrowUp" => (0.0, -step),
                        _ => (0.0, step),
                    };
                    b.command(move |ed, s, r| ed.nudge(s, dx, dy, r));
                    true
                }
                (false, "q" | "Q") => {
                    let locked = !b.editor.borrow().tool_locked;
                    b.editor.borrow_mut().tool_locked = locked;
                    b.changed();
                    true
                }
                (false, "!") => {
                    b.fit();
                    true
                }
                // Shift+Alt+C: copied as a PNG picture (whatever the
                // layout makes of the key).
                (false, _) if ev.alt_key() && shift && ev.code() == "KeyC" => {
                    b.copy_as(true);
                    true
                }
                (false, k) if !ev.alt_key() && w || k == "h" || k == "v" || k == "1" => {
                    match Tool::from_key(k) {
                        Some(t) => {
                            b.set_tool(t);
                            true
                        }
                        _ => false,
                    }
                }
                (true, "k") if w => {
                    b.edit_link();
                    true
                }
                (true, "a") => {
                    let s = b.scene();
                    b.editor.borrow_mut().select_all(&s);
                    b.schedule();
                    b.changed();
                    true
                }
                (true, "z" | "Z") if w => {
                    b.undo(shift);
                    true
                }
                (true, "y") if w => {
                    b.undo(true);
                    true
                }
                (true, "d") if w => {
                    b.command(|ed, s, r| ed.duplicate(s, r));
                    true
                }
                (true, "g" | "G") if w => {
                    if shift {
                        b.command(|ed, s, r| ed.ungroup(s, r));
                    } else {
                        b.command(|ed, s, r| ed.group(s, r));
                    }
                    true
                }
                (true, "l" | "L") if shift && w => {
                    b.command(|ed, s, r| ed.toggle_lock(s, r));
                    true
                }
                (true, "]" | "}") if w => {
                    b.command(move |ed, s, r| ed.reorder(s, true, shift, r));
                    true
                }
                (true, "[" | "{") if w => {
                    b.command(move |ed, s, r| ed.reorder(s, false, shift, r));
                    true
                }
                (true, "=" | "+") => {
                    b.zoom_by(1.1);
                    true
                }
                (true, "-") => {
                    b.zoom_by(1.0 / 1.1);
                    true
                }
                (true, "0") => {
                    let z = b.view.get().zoom;
                    b.zoom_by(1.0 / z);
                    true
                }
                (true, k) if w && (k == "'" || ev.code() == "Quote") => {
                    b.toggle_grid();
                    true
                }
                (false, k)
                    if w && ev.alt_key()
                        && (k.eq_ignore_ascii_case("s") || ev.code() == "KeyS") =>
                {
                    b.set_snapping(!b.snapping());
                    true
                }
                _ => false,
            }
        });
        if handled {
            ev.prevent_default();
            ev.stop_propagation();
        }
    };
    // The clipboard: Excalidraw's format both ways, plain text as a text.
    let on_copy = move |ev: web_sys::ClipboardEvent| {
        if editing.get_untracked().is_some() {
            return;
        }
        if let (Some(data), Some(text)) = (ev.clipboard_data(), b.with_value(|b| b.copy())) {
            let _ = data.set_data("text/plain", &text);
            ev.prevent_default();
        }
    };
    let on_cut = move |ev: web_sys::ClipboardEvent| {
        if editing.get_untracked().is_some() || !writable.get_untracked() {
            return;
        }
        if let (Some(data), Some(text)) = (ev.clipboard_data(), b.with_value(|b| b.copy())) {
            let _ = data.set_data("text/plain", &text);
            ev.prevent_default();
            b.with_value(|b| b.command(|ed, s, r| ed.cut(s, r)));
        }
    };
    let on_paste = move |ev: web_sys::ClipboardEvent| {
        if editing.get_untracked().is_some() {
            return;
        }
        // Pictures pasted: put in where the pointer is.
        let pictures: Vec<web_sys::File> = ev
            .clipboard_data()
            .and_then(|d| d.files())
            .map(|l| (0..l.length()).filter_map(|i| l.get(i)).collect())
            .unwrap_or_default();
        if pictures.iter().any(|f| f.type_().starts_with("image/")) {
            ev.prevent_default();
            let images = pictures
                .into_iter()
                .filter(|f| f.type_().starts_with("image/"))
                .collect();
            b.with_value(|b| b.add_pictures(images, Some(b.paste_point())));
            return;
        }
        let text = ev
            .clipboard_data()
            .and_then(|d| d.get_data("text/plain").ok())
            .unwrap_or_default();
        if !text.is_empty() {
            ev.prevent_default();
            b.with_value(|b| b.paste(&text));
        }
    };
    // A drawing file dropped on the canvas: its elements come in; pictures
    // go where they were dropped.
    let on_drop = move |ev: web_sys::DragEvent| {
        let Some(files) = ev.data_transfer().and_then(|d| d.files()) else {
            return;
        };
        if files.length() == 0 {
            return;
        }
        ev.prevent_default();
        let (images, others): (Vec<web_sys::File>, Vec<web_sys::File>) = (0..files.length())
            .filter_map(|i| files.get(i))
            .partition(|f| f.type_().starts_with("image/"));
        if !images.is_empty() {
            let at = [ev.offset_x() as f64, ev.offset_y() as f64];
            b.with_value(|b| b.add_pictures(images, Some(b.view.get().to_scene(at))));
        }
        for f in others {
            leptos::task::spawn_local(async move {
                let text = wasm_bindgen_futures::JsFuture::from(f.text())
                    .await
                    .ok()
                    .and_then(|t| t.as_string());
                if let Some(text) = text {
                    b.with_value(|b| b.paste(&text));
                }
            });
        }
    };
    let export_open = RwSignal::new(false);
    super::close_on_outside(export_open);
    let export = move |kind: Export| {
        export_open.set(false);
        b.with_value(|b| b.export(kind));
    };
    let on_key_up = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == " " {
            su.set(false);
        }
    };
    let set_tool = move |t: Tool| b.with_value(|b| b.set_tool(t));
    let tool_lock = move || {
        rev.get();
        b.with_value(|b| b.editor.borrow().tool_locked)
    };
    // Shows the properties panel: something selected, or a drawing tool.
    let panel = move || {
        rev.get();
        writable.get()
            && b.with_value(|b| {
                let ed = b.editor.borrow();
                !ed.selection.is_empty()
                    || !matches!(
                        ed.tool,
                        Tool::Selection | Tool::Hand | Tool::Eraser | Tool::Laser
                    )
            })
    };
    view! {
        <div class="note-page drawing-page" tabindex="0" node_ref=root_ref on:keydown=on_key on:keyup=on_key_up
            on:copy=on_copy on:cut=on_cut on:paste=on_paste on:drop=on_drop on:dragover=|ev: web_sys::DragEvent| ev.prevent_default()>
            <header class="note-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <div class="crumbs ellipsis">{move || crumbs.get()}</div>
                <div class="presence">
                    {move || {
                        let here = id_here.get_value();
                        ui.presence
                            .get()
                            .into_iter()
                            .filter(|p| p.note.as_deref() == Some(here.as_str()))
                            .map(|p| super::avatar(&p.name, &p.user_id))
                            .collect_view()
                    }}
                </div>
                <div class="tb-drop">
                    <button class="icon-btn" title="Export" aria-label="Export" aria-expanded=move || export_open.get().to_string()
                        on:click=move |ev| { ev.stop_propagation(); export_open.update(|o| *o = !*o); }><Icon name="download"/></button>
                    <Show when=move || export_open.get()>
                        <div class="menu right" on:click=|ev| ev.stop_propagation()>
                            <div class="menu-label">"Download as"</div>
                            <button on:click=move |_| export(Export::Drawing)><Icon name="drawing"/>" Drawing (.drawing)"</button>
                            <button on:click=move |_| export(Export::Svg)><Icon name="image"/>" SVG image"</button>
                            <button on:click=move |_| export(Export::Png)><Icon name="image"/>" PNG image"</button>
                            <button on:click=move |_| export(Export::Excalidraw)><Icon name="file"/>" Excalidraw (.excalidraw)"</button>
                            <button on:click=move |_| export(Export::Obsidian)><Icon name="file"/>" Obsidian Excalidraw (.excalidraw.md)"</button>
                            <div class="menu-sep"></div>
                            <button on:click=move |_| {
                                export_open.set(false);
                                b.with_value(|b| b.copy_as(true));
                            } title="Shift+Alt+C"><Icon name="copy"/>" Copy as PNG"</button>
                            <button on:click=move |_| {
                                export_open.set(false);
                                b.with_value(|b| b.copy_as(false));
                            }><Icon name="copy"/>" Copy as SVG"</button>
                            <button on:click=move |_| {
                                export_open.set(false);
                                b.with_value(|b| b.copy_embed());
                            }><Icon name="link"/>" Copy link for notes"</button>
                        </div>
                    </Show>
                </div>
            </header>
            <div class="drawing-stage">
                <canvas class="drawing-canvas" node_ref=canvas_ref
                    on:pointerdown=on_down on:pointermove=on_move on:pointerup=on_up.clone() on:pointercancel=on_up
                    on:pointerleave=move |_| b.with_value(|b| b.share_pointer(None, false, false))
                    on:dblclick=move |ev: web_sys::MouseEvent| b.with_value(|b| b.double_click([ev.offset_x() as f64, ev.offset_y() as f64], ev.ctrl_key() || ev.meta_key()))></canvas>
                <Show when=move || writable.get()>
                    <div class="drawing-tools" role="toolbar" aria-label="Drawing tools">
                        <button class="dt" class:on=tool_lock title="Keep selected tool active after drawing — Q" aria-pressed=move || tool_lock().to_string()
                            on:click=move |_| b.with_value(|b| { let l = !b.editor.borrow().tool_locked; b.editor.borrow_mut().tool_locked = l; b.changed(); })>
                            <Icon name=if tool_lock() { "lock" } else { "unlock" }/>
                        </button>
                        <span class="dt-sep"></span>
                        {TOOLS.iter().map(|(t, icon, name, keys)| {
                            let t = *t;
                            let title = format!("{name} — {keys}");
                            view! {
                                <button class="dt" class:on=move || tool() == t title=title.clone() aria-label=*name aria-pressed=move || (tool() == t).to_string()
                                    on:click=move |_| set_tool(t)><Icon name=*icon/></button>
                            }
                        }).collect_view()}
                    </div>
                </Show>
                <Show when=panel>
                    <Properties board=b rev=rev/>
                </Show>
                {move || editing.get().map(|tb| {
                    let style = format!(
                        "left:{}px;top:{}px;min-width:{}px;min-height:{}px;font-size:{}px;line-height:{};font-family:{};color:{};text-align:{};transform:rotate({}rad);transform-origin:0 0",
                        tb.left, tb.top, tb.width.max(8.0), tb.height.max(tb.font_size * tb.line_height), tb.font_size, tb.line_height, tb.font_family, tb.color, tb.align, tb.angle
                    );
                    let frame_name = tb.frame_name;
                    view! {
                        <textarea class="drawing-text" class:drawing-frame-name=frame_name node_ref=text_ref style=style spellcheck="false" wrap="off"
                            on:input=move |ev| b.with_value(|b| b.set_text_draft(event_target_value(&ev)))
                            on:blur=move |_| finish_text()
                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                ev.stop_propagation();
                                // A frame's name is one line.
                                if ev.key() == "Escape" || (ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key() || frame_name)) {
                                    ev.prevent_default();
                                    finish_text();
                                }
                            }></textarea>
                    }
                })}
                <div class="drawing-zoom" role="group" aria-label="Zoom">
                    <button title="Zoom out" aria-label="Zoom out" on:click=move |_| b.with_value(|b| b.zoom_by(1.0 / 1.1))>"−"</button>
                    <button class="zoom-value" title="Show all of the drawing — Shift+1" on:click=move |_| b.with_value(|b| b.fit())>{shown_zoom}</button>
                    <button title="Zoom in" aria-label="Zoom in" on:click=move |_| b.with_value(|b| b.zoom_by(1.1))>"+"</button>
                </div>
                <div class="drawing-history" role="group" aria-label="History">
                    <button title="Undo — Ctrl+Z" aria-label="Undo" on:click=move |_| b.with_value(|b| b.undo(false))><Icon name="undo"/></button>
                    <button title="Redo — Ctrl+Shift+Z" aria-label="Redo" on:click=move |_| b.with_value(|b| b.undo(true))><Icon name="redo"/></button>
                </div>
                <Show when=move || writable.get()>
                    <SnapToggles board=b rev=rev grid=grid/>
                </Show>
            </div>
        </div>
    }
}

/// The grid (the drawing's) and snapping to elements (one's own), on or off.
#[component]
fn SnapToggles(
    board: StoredValue<Rc<Board>, LocalStorage>,
    rev: RwSignal<u64>,
    grid: RwSignal<bool>,
) -> impl IntoView {
    let snapping = move || {
        rev.get();
        board.with_value(|b| b.snapping())
    };
    view! {
        <div class="drawing-snap" role="group" aria-label="Snapping">
            <button class:on=move || grid.get() title="Grid — Ctrl+'" aria-label="Grid" aria-pressed=move || grid.get().to_string()
                on:click=move |_| board.with_value(|b| b.toggle_grid())><Icon name="grid"/></button>
            <button class:on=snapping title="Snap to objects — Alt+S" aria-label="Snap to objects" aria-pressed=move || snapping().to_string()
                on:click=move |_| board.with_value(|b| b.set_snapping(!b.snapping()))><Icon name="magnet"/></button>
        </div>
    }
}

/// One row of choices in the properties panel.
fn choices(
    board: StoredValue<Rc<Board>, LocalStorage>,
    rev: RwSignal<u64>,
    key: &'static str,
    current: impl Fn(&Board) -> Value + Copy + Send + Sync + 'static,
    options: Vec<(Value, AnyView, String)>,
) -> impl IntoView {
    options
        .into_iter()
        .map(|(value, label, title)| {
            let v2 = value.clone();
            let on = move || {
                rev.get();
                board.with_value(|b| current(b)) == v2
            };
            view! {
                <button class="pick" class:on=on title=title.clone() aria-label=title
                    on:click=move |_| board.with_value(|b| b.set_property(key, value.clone()))>{label}</button>
            }
        })
        .collect_view()
}

/// A property's value shown: the first selected element's, else the
/// style's.
fn prop(b: &Board, key: &str, fallback: Value) -> Value {
    let s = b.scene();
    let ed = b.editor.borrow();
    ed.selected(&s)
        .first()
        .and_then(|e| e.get(key).cloned())
        .unwrap_or(fallback)
}

fn swatch(color: &str) -> AnyView {
    let style = if color == "transparent" {
        "background: repeating-conic-gradient(#ccc 0 25%, #fff 0 50%) 0 0 / 8px 8px".to_string()
    } else {
        format!("background:{color}")
    };
    view! { <span class="swatch" style=style></span> }.into_any()
}

fn label(text: &str) -> AnyView {
    view! { <span class="pick-text">{text.to_string()}</span> }.into_any()
}

/// The properties of what is selected (or of what the tool makes).
#[component]
fn Properties(board: StoredValue<Rc<Board>, LocalStorage>, rev: RwSignal<u64>) -> impl IntoView {
    // What is selected decides what shows: kinds of elements.
    let kinds = move || {
        rev.get();
        board.with_value(|b| {
            let ed = b.editor.borrow();
            let mut k: Vec<ElementType> = b.selection_kinds().into_values().collect();
            if k.is_empty() {
                k.push(match ed.tool {
                    Tool::Rectangle => ElementType::Rectangle,
                    Tool::Diamond => ElementType::Diamond,
                    Tool::Ellipse => ElementType::Ellipse,
                    Tool::Arrow => ElementType::Arrow,
                    Tool::Line => ElementType::Line,
                    Tool::Freedraw => ElementType::Freedraw,
                    Tool::Text => ElementType::Text,
                    Tool::Frame => ElementType::Frame,
                    Tool::StickyNote => ElementType::StickyNote,
                    _ => ElementType::Rectangle,
                });
            }
            k
        })
    };
    let has = move |f: fn(&ElementType) -> bool| kinds().iter().any(f);
    let stroke = move |b: &Board| {
        let ed = b.editor.borrow();
        let style = if ed.tool == Tool::StickyNote {
            &ed.style.sticky_stroke_color
        } else {
            &ed.style.stroke_color
        };
        prop(b, "strokeColor", json!(style))
    };
    let paper = move |b: &Board| {
        prop(
            b,
            "backgroundColor",
            json!(b.editor.borrow().style.sticky_background_color),
        )
    };
    let background = move |b: &Board| {
        prop(
            b,
            "backgroundColor",
            json!(b.editor.borrow().style.background_color),
        )
    };
    let fill = move |b: &Board| prop(b, "fillStyle", json!(b.editor.borrow().style.fill_style));
    let width = move |b: &Board| {
        prop(
            b,
            "strokeWidth",
            json!(b.editor.borrow().style.stroke_width),
        )
    };
    let style = move |b: &Board| {
        prop(
            b,
            "strokeStyle",
            json!(b.editor.borrow().style.stroke_style),
        )
    };
    let rough = move |b: &Board| prop(b, "roughness", json!(b.editor.borrow().style.roughness));
    let font = move |b: &Board| prop(b, "fontFamily", json!(b.editor.borrow().style.font_family));
    let size = move |b: &Board| prop(b, "fontSize", json!(b.editor.borrow().style.font_size));
    let align = move |b: &Board| prop(b, "textAlign", json!(b.editor.borrow().style.text_align));
    let start = move |b: &Board| {
        prop(
            b,
            "startArrowhead",
            json!(b.editor.borrow().style.start_arrowhead),
        )
    };
    let end = move |b: &Board| {
        prop(
            b,
            "endArrowhead",
            json!(b.editor.borrow().style.end_arrowhead),
        )
    };
    let arrow_type = move |b: &Board| {
        let s = b.scene();
        let ed = b.editor.borrow();
        let first = ed
            .selected(&s)
            .into_iter()
            .find(|e| e.kind() == ElementType::Arrow);
        json!(first.map_or(ed.style.arrow_type.as_str(), |e| {
            if e.is_elbowed() {
                "elbow"
            } else if e.roundness().is_some() {
                "round"
            } else {
                "sharp"
            }
        }))
    };
    let icon = |name: &'static str| view! { <Icon name=name/> }.into_any();
    let opacity = move || {
        rev.get();
        board.with_value(|b| {
            prop(b, "opacity", json!(b.editor.borrow().style.opacity))
                .as_f64()
                .unwrap_or(100.0)
        })
    };
    let num = |x: f64| json!(x as i64);
    let colors = |picks: [&str; 5]| -> Vec<(Value, AnyView, String)> {
        picks
            .iter()
            .map(|c| (json!(c), swatch(c), (*c).to_string()))
            .collect()
    };
    let custom_color = move |key: &'static str, current: fn(&Board) -> Value| {
        let value = move || {
            rev.get();
            board.with_value(|b| current(b).as_str().unwrap_or_default().to_string())
        };
        // Excalidraw's whole palette, in a popover.
        let open = RwSignal::new(false);
        let all = palette::full(key == "strokeColor");
        view! {
            <span class="color-more">
                <button class="pick" class:on=move || open.get() title="More colours" aria-label="More colours"
                    on:click=move |_| open.update(|o| *o = !*o)><span class="pick-text">"⋯"</span></button>
                <Show when=move || open.get()>
                    <div class="color-grid" role="listbox" aria-label="Colours">
                        {all.iter().map(|c| {
                            let c = *c;
                            let on = move || value() == c;
                            view! {
                                <button class="pick" class:on=on title=c aria-label=c
                                    on:click=move |_| {
                                        board.with_value(|b| b.set_property(key, json!(c)));
                                        open.set(false);
                                    }>{swatch(c)}</button>
                            }
                        }).collect_view()}
                    </div>
                </Show>
            </span>
            <input class="pick-hex" type="text" spellcheck="false" aria-label="Colour (hex)" prop:value=value
                on:change=move |ev| {
                    let v = event_target_value(&ev);
                    if notekpr_draw::color::parse(&v).is_some() {
                        board.with_value(|b| b.set_property(key, json!(v.trim())));
                    }
                }/>
        }
    };
    let not_text = |k: &ElementType| *k != ElementType::Text;
    let fillable = |k: &ElementType| {
        matches!(
            k,
            ElementType::Rectangle
                | ElementType::Diamond
                | ElementType::Ellipse
                | ElementType::Line
                | ElementType::Freedraw
                | ElementType::Embeddable
        )
    };
    let strokes = |k: &ElementType| {
        !matches!(
            k,
            ElementType::Text | ElementType::Image | ElementType::Frame
        )
    };
    // A note's stroke is its ink: no width or style.
    let lines = |k: &ElementType| {
        !matches!(
            k,
            ElementType::Text | ElementType::Image | ElementType::Frame | ElementType::StickyNote
        )
    };
    let notes = |k: &ElementType| *k == ElementType::StickyNote;
    let rough_kinds = |k: &ElementType| {
        matches!(
            k,
            ElementType::Rectangle
                | ElementType::Diamond
                | ElementType::Ellipse
                | ElementType::Line
                | ElementType::Arrow
                | ElementType::StickyNote
        )
    };
    let roundable =
        |k: &ElementType| notekpr_draw::shape::can_change_roundness(k) && *k != ElementType::Line;
    // A note's text is styled with it.
    let texts = |k: &ElementType| matches!(k, ElementType::Text | ElementType::StickyNote);
    let arrows = |k: &ElementType| *k == ElementType::Arrow;
    let round_now = move || {
        rev.get();
        board.with_value(|b| {
            let s = b.scene();
            let ed = b.editor.borrow();
            ed.selected(&s)
                .first()
                .map_or(ed.style.round_edges, |e| e.roundness().is_some())
        })
    };
    let heads = |end: bool| -> Vec<(Value, AnyView, String)> {
        let mut v = vec![(Value::Null, label("—"), "None".to_string())];
        for (h, l) in [
            ("arrow", "→"),
            ("triangle", "▶"),
            ("triangle_outline", "▷"),
            ("circle", "●"),
            ("circle_outline", "○"),
            ("diamond", "◆"),
            ("diamond_outline", "◇"),
            ("bar", "|"),
        ] {
            let shown = if end {
                l.to_string()
            } else {
                l.chars()
                    .rev()
                    .collect::<String>()
                    .replace('→', "←")
                    .replace('▶', "◀")
                    .replace('▷', "◁")
            };
            v.push((json!(h), label(&shown), h.replace('_', " ")));
        }
        v
    };
    let action = move |name: &'static str, icon: &'static str, f: fn(&Rc<Board>)| {
        view! { <button class="pick" title=name aria-label=name on:click=move |_| board.with_value(f)><Icon name=icon/></button> }
    };
    view! {
        <div class="drawing-props" aria-label="Properties">
            <Show when=move || has(strokes) || has(texts)>
                <div class="prop"><span class="prop-name">"Stroke"</span>
                    <div class="picks">{choices(board, rev, "strokeColor", stroke, colors(palette::STROKE_PICKS))}{custom_color("strokeColor", stroke)}</div></div>
            </Show>
            <Show when=move || has(fillable)>
                <div class="prop"><span class="prop-name">"Background"</span>
                    <div class="picks">{choices(board, rev, "backgroundColor", background, colors(palette::BACKGROUND_PICKS))}{custom_color("backgroundColor", |b| prop(b, "backgroundColor", json!(b.editor.borrow().style.background_color)))}</div></div>
                <div class="prop"><span class="prop-name">"Fill"</span>
                    <div class="picks">{choices(board, rev, "fillStyle", fill, vec![
                        (json!("hachure"), label("▤"), "Hachure".into()),
                        (json!("cross-hatch"), label("▦"), "Cross-hatch".into()),
                        (json!("solid"), label("■"), "Solid".into()),
                        (json!("zigzag"), label("≋"), "Zigzag".into()),
                    ])}</div></div>
            </Show>
            <Show when=move || has(notes)>
                <div class="prop"><span class="prop-name">"Background"</span>
                    <div class="picks">{choices(board, rev, "backgroundColor", paper, colors(palette::STICKY_NOTE_PICKS))}{custom_color("backgroundColor", |b| prop(b, "backgroundColor", json!(b.editor.borrow().style.sticky_background_color)))}</div></div>
            </Show>
            <Show when=move || has(lines)>
                <div class="prop"><span class="prop-name">"Stroke width"</span>
                    <div class="picks">{choices(board, rev, "strokeWidth", width, vec![
                        (num(1.0), label("—"), "Thin".into()),
                        (num(2.0), label("━"), "Bold".into()),
                        (num(4.0), label("▬"), "Extra bold".into()),
                    ])}</div></div>
                <div class="prop"><span class="prop-name">"Stroke style"</span>
                    <div class="picks">{choices(board, rev, "strokeStyle", style, vec![
                        (json!("solid"), label("──"), "Solid".into()),
                        (json!("dashed"), label("- -"), "Dashed".into()),
                        (json!("dotted"), label("···"), "Dotted".into()),
                    ])}</div></div>
            </Show>
            <Show when=move || has(rough_kinds)>
                <div class="prop"><span class="prop-name">"Sloppiness"</span>
                    <div class="picks">{choices(board, rev, "roughness", rough, vec![
                        (num(0.0), label("Architect"), "Architect".into()),
                        (num(1.0), label("Artist"), "Artist".into()),
                        (num(2.0), label("Cartoonist"), "Cartoonist".into()),
                    ])}</div></div>
            </Show>
            <Show when=move || has(roundable)>
                <div class="prop"><span class="prop-name">"Edges"</span>
                    <div class="picks">
                        <button class="pick" class:on=move || !round_now() title="Sharp" on:click=move |_| board.with_value(|b| b.command(|ed, s, r| ed.set_round_edges(s, false, r)))><span class="pick-text">"┌"</span></button>
                        <button class="pick" class:on=round_now title="Round" on:click=move |_| board.with_value(|b| b.command(|ed, s, r| ed.set_round_edges(s, true, r)))><span class="pick-text">"╭"</span></button>
                    </div></div>
            </Show>
            <Show when=move || has(arrows)>
                <div class="prop"><span class="prop-name">"Arrow type"</span>
                    <div class="picks">{choices(board, rev, "arrowType", arrow_type, vec![
                        (json!("sharp"), icon("arrow-sharp"), "Sharp arrow".into()),
                        (json!("round"), icon("arrow-curved"), "Curved arrow".into()),
                        (json!("elbow"), icon("arrow-elbow"), "Elbow arrow".into()),
                    ])}</div></div>
                <div class="prop"><span class="prop-name">"Arrowheads"</span>
                    <div class="picks">{choices(board, rev, "startArrowhead", start, heads(false))}</div>
                    <div class="picks">{choices(board, rev, "endArrowhead", end, heads(true))}</div></div>
            </Show>
            <Show when=move || has(texts)>
                <div class="prop"><span class="prop-name">"Font family"</span>
                    <div class="picks">{choices(board, rev, "fontFamily", font, vec![
                        (json!(notekpr_draw::fonts::EXCALIFONT), label("Hand-drawn"), "Excalifont".into()),
                        (json!(notekpr_draw::fonts::NUNITO), label("Normal"), "Nunito".into()),
                        (json!(notekpr_draw::fonts::COMIC_SHANNS), label("Code"), "Comic Shanns".into()),
                        (json!(notekpr_draw::fonts::LILITA_ONE), label("Headline"), "Lilita One".into()),
                    ])}
                    <select class="pick-font" aria-label="More fonts"
                        prop:value=move || { rev.get(); board.with_value(|b| font(b).as_u64().unwrap_or(5).to_string()) }
                        on:change=move |ev| {
                            if let Ok(f) = event_target_value(&ev).parse::<u32>() {
                                board.with_value(|b| b.set_property("fontFamily", json!(f)));
                            }
                        }>
                        {MORE_FONTS.iter().map(|(id, name)| view! { <option value=id.to_string()>{*name}</option> }).collect_view()}
                    </select></div></div>
                <div class="prop"><span class="prop-name">"Font size"</span>
                    <div class="picks">{choices(board, rev, "fontSize", size, vec![
                        (num(16.0), label("S"), "Small".into()),
                        (num(20.0), label("M"), "Medium".into()),
                        (num(28.0), label("L"), "Large".into()),
                        (num(36.0), label("XL"), "Very large".into()),
                    ])}</div></div>
                <div class="prop"><span class="prop-name">"Text align"</span>
                    <div class="picks">{choices(board, rev, "textAlign", align, vec![
                        (json!("left"), label("⇤"), "Left".into()),
                        (json!("center"), label("↔"), "Center".into()),
                        (json!("right"), label("⇥"), "Right".into()),
                    ])}</div></div>
            </Show>
            <Show when=move || has(not_text) || has(texts)>
                <div class="prop"><span class="prop-name">"Opacity"</span>
                    <input type="range" min="0" max="100" step="10" prop:value=move || opacity().to_string() aria-label="Opacity"
                        on:change=move |ev| {
                            let v: f64 = event_target_value(&ev).parse().unwrap_or(100.0);
                            board.with_value(|b| b.set_property("opacity", json!(v as i64)));
                        }/></div>
            </Show>
            <Show when=move || { rev.get(); board.with_value(|b| !b.selection().is_empty()) }>
                <div class="prop"><span class="prop-name">"Layers"</span>
                    <div class="picks">
                        {action("Send to back — Ctrl+Shift+[", "back", |b| b.command(|ed, s, r| ed.reorder(s, false, true, r)))}
                        {action("Send backward — Ctrl+[", "chevron-down", |b| b.command(|ed, s, r| ed.reorder(s, false, false, r)))}
                        {action("Bring forward — Ctrl+]", "chevron-right", |b| b.command(|ed, s, r| ed.reorder(s, true, false, r)))}
                        {action("Bring to front — Ctrl+Shift+]", "front", |b| b.command(|ed, s, r| ed.reorder(s, true, true, r)))}
                    </div></div>
                <Show when=move || { rev.get(); board.with_value(|b| b.selection().len() > 1) }>
                    <div class="prop"><span class="prop-name">"Align"</span>
                        <div class="picks">
                            {action("Align left", "chevron-right", |b| b.command(|ed, s, r| ed.align(s, "left", r)))}
                            {action("Center horizontally", "minus", |b| b.command(|ed, s, r| ed.align(s, "center", r)))}
                            {action("Align right", "chevron-right", |b| b.command(|ed, s, r| ed.align(s, "right", r)))}
                            {action("Align top", "chevron-down", |b| b.command(|ed, s, r| ed.align(s, "top", r)))}
                            {action("Center vertically", "minus", |b| b.command(|ed, s, r| ed.align(s, "middle", r)))}
                            {action("Align bottom", "chevron-down", |b| b.command(|ed, s, r| ed.align(s, "bottom", r)))}
                            {action("Distribute horizontally", "more", |b| b.command(|ed, s, r| ed.distribute(s, true, r)))}
                            {action("Distribute vertically", "more", |b| b.command(|ed, s, r| ed.distribute(s, false, r)))}
                        </div></div>
                </Show>
                <div class="prop"><span class="prop-name">"Actions"</span>
                    <div class="picks">
                        {action("Duplicate — Ctrl+D", "copy", |b| b.command(|ed, s, r| ed.duplicate(s, r)))}
                        {action("Delete — Delete", "trash", |b| b.command(|ed, s, r| ed.delete_selected(s, r)))}
                        {action("Group — Ctrl+G", "group", |b| b.command(|ed, s, r| ed.group(s, r)))}
                        {action("Ungroup — Ctrl+Shift+G", "group", |b| b.command(|ed, s, r| ed.ungroup(s, r)))}
                        {action("Lock or unlock — Ctrl+Shift+L", "lock", |b| b.command(|ed, s, r| ed.toggle_lock(s, r)))}
                        {action("Link — Ctrl+K", "link", |b| b.edit_link())}
                    </div></div>
            </Show>
        </div>
    }
}

/// The sidebar's entry for one's Drawings section.
#[component]
pub fn DrawingsSection() -> impl IntoView {
    let ui = ui();
    view! {
        <div class="sb-section sb-quick">
            <button class="sb-link" class:active=move || {
                    let r = ui.route.get();
                    r == state::Route::Drawings || r.vault().is_some_and(|v| state::step(|c| c.is_drawings(v)))
                }
                title="Your drawings" on:click=move |_| state::navigate(state::Route::Drawings)>
                <Icon name="drawing"/><span class="grow ellipsis">"Drawings"</span></button>
            <button class="icon-btn tiny qn-add" title="New drawing" aria-label="New drawing"
                on:click=move |_| crate::drawings::new_in_section()><Icon name="plus"/></button>
        </div>
    }
}

/// `/drawings`: one's own drawings, as pictures.
#[component]
pub fn DrawingsPage() -> impl IntoView {
    let ui = ui();
    let list = Memo::new(move |_| {
        ui.tree_rev.get();
        ui.vaults.track();
        crate::drawings::in_section()
    });
    view! {
        <div class="page drawings-page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1>"Drawings"</h1>
                <button class="btn primary small" on:click=move |_| crate::drawings::new_in_section()><Icon name="plus"/>" New drawing"</button>
            </header>
            {move || {
                let items = list.get();
                if items.is_empty() {
                    return view! {
                        <p class="muted empty">"Your own drawings live here. Drawings in vaults stay in their vaults, where you draw them with the vault's members."</p>
                    }.into_any();
                }
                let vault = crate::drawings::section().unwrap_or_default();
                view! {
                    <div class="drawing-cards">
                        {items.into_iter().map(|(id, title, _)| {
                            let picture = RwSignal::new(None::<String>);
                            let (v, i) = (vault.clone(), id.clone());
                            Effect::new(move |_| {
                                ui.content_rev.get();
                                ui.ui_prefs.track();
                                let (v, i) = (v.clone(), i.clone());
                                leptos::task::spawn_local(async move {
                                    state::step(|c| c.ensure_note_loaded(&v, &i));
                                    if let Some(u) = crate::drawings::load_preview(&v, &i).await {
                                        picture.set(Some(u));
                                    }
                                });
                            });
                            let (v, i) = (vault.clone(), id.clone());
                            view! {
                                <button class="drawing-card" on:click=move |_| state::navigate(state::Route::Note(v.clone(), i.clone()))>
                                    <span class="drawing-thumb">{move || picture.get().map(|src| view! { <img src=src alt=""/> })}</span>
                                    <span class="drawing-name ellipsis">{title}</span>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}
