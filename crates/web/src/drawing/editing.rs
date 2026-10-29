//! Editing on the board: pointer gestures and commands go through the
//! editor's model (`notekpr_draw::edit`); what changes is shown at once and
//! written to the drawing's CRDT — during a drag at most every
//! [`WRITE_EVERY_MS`] (so others see it move), at its end at once, as one
//! undo step.

use std::collections::HashMap;
use std::rc::Rc;

use notekpr_draw::edit::{Editor, Mods, Output, Random, Tool};
use notekpr_draw::scene::{Element, Scene};
use serde_json::Value;
use wasm_bindgen::JsCast;

use super::Board;
use crate::state::{self, step};
use crate::util;

/// How often a drag's changes are written while it goes on.
const WRITE_EVERY_MS: f64 = 100.0;

/// Ids, seeds and the time, from the browser.
pub struct WebRandom;

impl Random for WebRandom {
    fn id(&mut self) -> String {
        // As Excalidraw's (nanoid's alphabet, 21 characters).
        const ALPHABET: &[u8] = b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
        let bytes = notekpr_core::crypto::random_bytes::<21>();
        bytes
            .iter()
            .map(|b| ALPHABET[(*b & 63) as usize] as char)
            .collect()
    }

    fn seed(&mut self) -> i32 {
        (js_sys::Math::random() * 2f64.powi(31)) as i32
    }

    fn now(&self) -> f64 {
        js_sys::Date::now()
    }
}

/// Puts changed elements into a scene (new ones in their stacking place).
pub fn patch(scene: &mut Scene, changed: &[Element]) {
    let mut added = false;
    for c in changed {
        match scene.elements.iter_mut().find(|e| e.id() == c.id()) {
            Some(e) => *e = c.clone(),
            None => {
                scene.elements.push(c.clone());
                added = true;
            }
        }
    }
    if added {
        scene.elements.sort_by(|a, b| {
            match (a.index(), b.index()) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.id().cmp(b.id()))
        });
    }
}

/// Measures text with the browser (a canvas of its own).
pub fn measure(font: &str, line: &str) -> f64 {
    thread_local! {
        static CTX: Option<web_sys::CanvasRenderingContext2d> = util::document()
            .create_element("canvas")
            .ok()
            .and_then(|c| c.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .and_then(|c| c.get_context("2d").ok().flatten())
            .and_then(|c| c.dyn_into().ok());
    }
    CTX.with(|ctx| {
        let Some(ctx) = ctx else {
            return line.chars().count() as f64 * 10.0;
        };
        ctx.set_font(font);
        ctx.measure_text(line).map_or(0.0, |m| m.width())
    })
}

/// Modifier keys of a pointer or keyboard event.
pub fn mods(shift: bool, alt: bool, ctrl: bool) -> Mods {
    Mods { shift, alt, ctrl }
}

impl Board {
    /// Whether the drawing can be changed here.
    pub fn writable(&self) -> bool {
        step(|c| c.can_write(&self.vault))
    }

    /// Starts undo tracking of the drawing (once).
    pub(super) fn enable_undo(&self) {
        step(|c| {
            if let Some(d) = c
                .replicas
                .get_mut(&self.vault)
                .and_then(|r| r.notes.get_mut(&self.id))
                && !d.undo_enabled()
            {
                d.enable_drawing_undo_with_clock(|| js_sys::Date::now() as u64);
            }
        });
    }

    fn boundary(&self) {
        step(|c| {
            if let Some(d) = c
                .replicas
                .get_mut(&self.vault)
                .and_then(|r| r.notes.get_mut(&self.id))
            {
                d.undo_boundary();
            }
        });
    }

    /// Shows what an editor event changed, and writes it.
    pub fn apply(self: &Rc<Self>, out: Output) {
        let edit_text = out.edit_text.clone();
        if out.changed.is_empty() || !self.writable() {
            if let Some(id) = &edit_text {
                self.open_text(id);
            }
            self.schedule();
            self.changed();
            return;
        }
        {
            let mut s = self.scene.borrow_mut();
            super::editing::patch(Rc::make_mut(&mut s), &out.changed);
        }
        {
            let mut u = self.unwritten.borrow_mut();
            for e in out.changed {
                u.insert(e.id().to_string(), e);
            }
        }
        let now = js_sys::Date::now();
        if out.done || now - self.last_write.get() >= WRITE_EVERY_MS {
            self.write();
        } else if !self.write_soon.get() {
            self.write_soon.set(true);
            let me = self.clone();
            util::set_timeout(WRITE_EVERY_MS as i32, move || {
                me.write_soon.set(false);
                me.write();
            });
        }
        if out.done {
            self.boundary();
        }
        // After the scene has it: a new text's box opens on it.
        if let Some(id) = &edit_text {
            self.open_text(id);
        }
        self.schedule();
        self.changed();
    }

    /// Writes what is shown but not yet written.
    pub fn write(&self) {
        let els: Vec<Element> = self
            .unwritten
            .borrow_mut()
            .drain()
            .map(|(_, e)| e)
            .collect();
        if els.is_empty() {
            return;
        }
        self.last_write.set(js_sys::Date::now());
        let (v, id) = (self.vault.clone(), self.id.clone());
        step(|c| {
            let budget = crate::state::limits().edit_budget();
            let Some(d) = c.replicas.get_mut(&v).and_then(|r| r.notes.get_mut(&id)) else {
                return;
            };
            // A lot at once (a paste) goes in several updates.
            let mut updates = Vec::new();
            let mut batch: Vec<Element> = Vec::new();
            let mut size = 0;
            for e in els {
                let len =
                    serde_json::to_string(e.props()).map_or(0, |s| s.len()) + 12 * e.props().len();
                if !batch.is_empty() && size + len > budget {
                    updates.extend(d.drawing().put(&batch));
                    batch.clear();
                    size = 0;
                }
                size += len;
                batch.push(e);
            }
            updates.extend(d.drawing().put(&batch));
            let total = d.drawing().size() as usize;
            for u in updates {
                state::local_update(c, &v, &id, u);
            }
            state::touch(c, &v, &id, total);
        });
    }

    /// Undoes (or redoes) the last own change.
    pub fn undo(self: &Rc<Self>, redo: bool) {
        self.write();
        let (v, id) = (self.vault.clone(), self.id.clone());
        let done = step(|c| {
            let Some(d) = c.replicas.get_mut(&v).and_then(|r| r.notes.get_mut(&id)) else {
                return false;
            };
            let u = if redo { d.redo() } else { d.undo() };
            match u {
                Some(u) => {
                    state::local_update(c, &v, &id, u);
                    true
                }
                None => false,
            }
        });
        if done {
            // What was selected may be gone.
            let scene = self.reread();
            self.editor
                .borrow_mut()
                .selection
                .retain(|s| scene.element(s).is_some_and(|e| !e.is_deleted()));
            self.schedule();
            self.changed();
        }
    }

    /// The drawing read again, with what isn't written yet on top.
    pub(super) fn reread(&self) -> Rc<Scene> {
        if let Some(mut s) = crate::drawings::scene(&self.vault, &self.id) {
            let pending: Vec<Element> = self.unwritten.borrow().values().cloned().collect();
            patch(&mut s, &pending);
            *self.scene.borrow_mut() = Rc::new(s);
        }
        self.scene()
    }

    /// Calls the page's listener: the tool, selection or style changed.
    pub fn changed(&self) {
        if let Some(f) = self.on_change.borrow().as_ref() {
            f();
        }
        self.share_selection();
    }

    /// The laser: a trail while the pointer is down (`down`: it went down;
    /// `up`: it went up), shown to the others too. Whether it is the tool.
    fn laser(self: &Rc<Self>, q: [f64; 2], down: bool, up: bool) -> bool {
        if self.editor.borrow().tool != Tool::Laser {
            return false;
        }
        let mut trails = self.trails.borrow_mut();
        if down {
            trails.own.start(q);
        } else if up {
            trails.own.end();
        } else if trails.own.drawing() {
            trails.own.add(q);
        }
        let drawing = trails.own.drawing();
        drop(trails);
        self.share_pointer(Some(q), true, drawing);
        self.schedule();
        true
    }

    /// Runs an editor command (Delete, Ctrl+D, a style, …).
    pub fn command(self: &Rc<Self>, f: impl FnOnce(&mut Editor, &Scene, &mut WebRandom) -> Output) {
        self.boundary();
        let scene = self.scene();
        let out = f(&mut self.editor.borrow_mut(), &scene, &mut WebRandom);
        self.apply(out);
    }

    pub fn set_tool(self: &Rc<Self>, tool: Tool) {
        self.finish_text_if_open();
        // The picture tool asks which pictures, and puts them in.
        if tool == Tool::Image {
            if self.writable() {
                let me = self.clone();
                crate::actions::pick_files(false, "image/*", move |files| {
                    me.add_pictures(files, None)
                });
            }
            return;
        }
        self.editor.borrow_mut().set_tool(tool);
        self.schedule();
        self.changed();
    }

    /// A double click on the canvas: edits a text or a shape's label, or
    /// the points of a line (an arrow's with Ctrl).
    pub fn double_click(self: &Rc<Self>, p: [f64; 2], ctrl: bool) {
        if !self.writable() {
            return;
        }
        self.boundary();
        let scene = self.scene();
        let view = self.view.get();
        let out = self.editor.borrow_mut().double_click(
            &scene,
            view.to_scene(p),
            Mods {
                ctrl,
                ..Mods::default()
            },
            view.zoom,
            &mut WebRandom,
        );
        self.apply(out);
        self.schedule();
        self.changed();
    }

    /// Enter: edits the selected text or shape's label, or the points of
    /// the selected line (an arrow's with Ctrl).
    pub fn enter(self: &Rc<Self>, ctrl: bool) {
        self.command(|ed, s, r| ed.enter(s, ctrl, r));
        self.schedule();
        self.changed();
    }

    /// The pointer went down on the canvas (`p` relative to it).
    pub fn pointer_down(self: &Rc<Self>, p: [f64; 2], pressure: Option<f64>, m: Mods) {
        self.finish_text_if_open();
        if self.laser(self.view.get().to_scene(p), true, false) {
            return;
        }
        self.boundary();
        let scene = self.scene();
        let view = self.view.get();
        self.editor.borrow_mut().snap.view = Some(view.visible());
        let out = self.editor.borrow_mut().pointer_down(
            &scene,
            view.to_scene(p),
            pressure,
            m,
            view.zoom,
            &mut WebRandom,
        );
        self.apply(out);
    }

    pub fn pointer_move(self: &Rc<Self>, p: [f64; 2], pressure: Option<f64>, m: Mods) {
        let view = self.view.get();
        if self.laser(view.to_scene(p), false, false) {
            return;
        }
        self.share_pointer(Some(view.to_scene(p)), false, false);
        let scene = self.scene();
        let snapped = !self.editor.borrow().snap.lines().is_empty();
        self.editor.borrow_mut().snap.view = Some(view.visible());
        let shown = self
            .editor
            .borrow()
            .frame_to_highlight()
            .map(str::to_string);
        let out = self.editor.borrow_mut().pointer_move(
            &scene,
            view.to_scene(p),
            pressure,
            m,
            view.zoom,
            &mut WebRandom,
        );
        let busy = self.editor.borrow().busy();
        if busy {
            self.apply(out);
            self.schedule();
        } else if snapped
            || !self.editor.borrow().snap.lines().is_empty()
            || self.editor.borrow().frame_to_highlight() != shown.as_deref()
        {
            // A shape tool over the canvas: the lines it snaps to, the
            // frame it would draw in.
            self.schedule();
        }
        // An arrow's end held still over a shape attaches inside it: look
        // again then, unless the pointer moved meanwhile.
        let n = self.moves.get().wrapping_add(1);
        self.moves.set(n);
        let held = self
            .editor
            .borrow()
            .binding_target()
            .is_some_and(|h| !h.inside);
        if held {
            let me = self.clone();
            leptos::task::spawn_local(async move {
                util::sleep(notekpr_draw::edit::BIND_INSIDE_AFTER as i32 + 20).await;
                if me.moves.get() == n && me.editor.borrow().busy() {
                    me.pointer_move(p, None, m);
                }
            });
        }
    }

    pub fn pointer_up(self: &Rc<Self>, p: [f64; 2]) {
        if self.laser(self.view.get().to_scene(p), false, true) {
            return;
        }
        let scene = self.scene();
        let view = self.view.get();
        let out = self
            .editor
            .borrow_mut()
            .pointer_up(&scene, view.to_scene(p), &mut WebRandom);
        self.apply(out);
    }

    /// The CSS cursor for the pointer at `p` (on the canvas).
    pub fn cursor_at(&self, p: [f64; 2]) -> &'static str {
        let ed = self.editor.borrow();
        match ed.tool {
            Tool::Hand => return "grab",
            Tool::Selection => {}
            Tool::Text => return "text",
            Tool::Eraser => return "cell",
            _ => return "crosshair",
        }
        let scene = self.scene();
        let view = self.view.get();
        let q = view.to_scene(p);
        if let Some(line) = ed.single_linear(&scene)
            && let Some(h) = ed.point_handles(&scene, view.zoom)
        {
            let reach = (notekpr_draw::edit::points::HANDLE + 1.0) / view.zoom;
            let near = |p: &[f64; 2]| notekpr_draw::geom::distance(*p, q) <= reach;
            if notekpr_draw::edit::points::point_at(line, q, view.zoom).is_some()
                || h.midpoints.iter().any(near)
            {
                return "pointer";
            }
        }
        if let Some(f) = ed.frame(&scene)
            && let Some(h) = notekpr_draw::edit::transform::handle_at(&f, q, view.zoom)
                .filter(|h| ed.can_use(&scene, *h))
        {
            return h.cursor(f.angle);
        }
        if notekpr_draw::edit::hit::element_at(&scene, q, view.zoom, false).is_some() {
            "move"
        } else {
            "default"
        }
    }

    /// Asks the page to edit a text (it puts a text box over it).
    fn open_text(&self, id: &str) {
        if let Some(f) = self.on_edit_text.borrow().as_ref() {
            f(id.to_string());
        }
    }

    /// Where a text being edited is on screen, and how it looks:
    /// `(left, top, width, height, font, colour, align, text, zoom)`.
    pub fn text_box(&self, id: &str) -> Option<TextBox> {
        let scene = self.scene();
        let e = scene.element(id)?;
        let view = self.view.get();
        if notekpr_draw::edit::frame::is_frame(e) {
            return Some(super::frames::name_box(
                e,
                &view,
                crate::drawings::is_dark(),
            ));
        }
        // A label is typed across the room in its shape, centred.
        let container = e.container_id().and_then(|c| scene.element(c));
        let (x, width) = match container {
            Some(c) if c.kind() == notekpr_draw::scene::ElementType::Arrow => {
                let w = notekpr_draw::edit::label::max_width(c, e.font_size());
                (notekpr_draw::edit::label::arrow_middle(c)[0] - w / 2.0, w)
            }
            Some(c) => (
                notekpr_draw::edit::label::inner_origin(c)[0],
                notekpr_draw::edit::label::max_width(c, e.font_size()),
            ),
            None => (e.x(), e.width()),
        };
        let [left, top] = view.to_screen([x, e.y()]);
        Some(TextBox {
            left,
            top,
            width: width * view.zoom,
            height: e.height() * view.zoom,
            font_size: e.font_size() * view.zoom,
            line_height: e.line_height(),
            font_family: notekpr_draw::fonts::css_family(e.font_family()),
            color: notekpr_draw::color::themed(e.stroke_color(), crate::drawings::is_dark()),
            align: e.text_align().to_string(),
            text: e.original_text().to_string(),
            angle: e.angle(),
            frame_name: false,
        })
    }

    /// Ends text editing with `text`.
    pub fn finish_text(self: &Rc<Self>, text: &str) {
        let scene = self.scene();
        let out = self
            .editor
            .borrow_mut()
            .finish_text(&scene, text, &measure, &mut WebRandom);
        self.apply(out);
        if let Some(f) = self.on_edit_text.borrow().as_ref() {
            f(String::new());
        }
    }

    /// Text editing ends when anything else starts.
    fn finish_text_if_open(self: &Rc<Self>) {
        if self.editor.borrow().editing_text.is_none() {
            return;
        }
        let text = self.text_draft.borrow().clone();
        self.finish_text(&text);
    }

    /// The text being typed (the page keeps it here as it changes).
    pub fn set_text_draft(&self, text: String) {
        *self.text_draft.borrow_mut() = text;
    }

    /// Sets a style property on the selection and on what comes next.
    pub fn set_property(self: &Rc<Self>, key: &str, value: Value) {
        let key = key.to_string();
        self.command(move |ed, scene, rng| ed.set_property(scene, &key, value, rng));
    }

    /// A text box for editing a text on the canvas.
    pub fn editing(&self) -> Option<String> {
        self.editor.borrow().editing_text.clone()
    }

    /// Ids of what is selected.
    pub fn selection(&self) -> Vec<String> {
        self.editor.borrow().selection.clone()
    }

    pub fn selection_kinds(&self) -> HashMap<String, notekpr_draw::scene::ElementType> {
        let scene = self.scene();
        self.editor
            .borrow()
            .selected(&scene)
            .into_iter()
            .map(|e| (e.id().to_string(), e.kind()))
            .collect()
    }
}

/// A text being edited: where and how to show its text box.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub line_height: f64,
    pub font_family: String,
    pub color: String,
    pub align: String,
    pub text: String,
    pub angle: f64,
    /// A frame's name: one line, Enter ends it.
    pub frame_name: bool,
}

/// What a drawing can be exported as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    /// notekpr's own file (`.drawing`).
    Drawing,
    /// Excalidraw's own file.
    Excalidraw,
    /// Obsidian's Excalidraw plugin's.
    Obsidian,
    Svg,
    Png,
}

impl Board {
    /// Where the pointer last was, in the drawing (else the view's middle).
    pub fn paste_point(&self) -> [f64; 2] {
        let v = self.view.get();
        v.to_scene(
            self.last_pointer
                .get()
                .unwrap_or([v.width / 2.0, v.height / 2.0]),
        )
    }

    pub fn note_pointer(&self, p: [f64; 2]) {
        self.last_pointer.set(Some(p));
    }

    /// What is selected, for the clipboard (Excalidraw's format).
    pub fn copy(&self) -> Option<String> {
        // With the pictures they show inside, for other apps.
        let scene = crate::drawings::with_pictures_at_hand((*self.scene()).clone());
        self.editor.borrow().copy(&scene)
    }

    /// Pastes clipboard text: elements (Excalidraw's or ours), else a text.
    pub fn paste(self: &Rc<Self>, text: &str) {
        if !self.writable() || text.is_empty() {
            return;
        }
        let at = self.paste_point();
        let mut ids = notekpr_draw::restore::Ids::new(js_sys::Date::now() as u64);
        match notekpr_draw::file::read_drawing(text, &mut ids) {
            Ok(d) => {
                // The pictures they show come along, as files of the vault.
                let files = d.scene.files.clone();
                if !files.is_empty() {
                    self.adopt_files(files);
                }
                self.command(move |ed, s, r| ed.paste(s, &d.scene.elements, at, r));
            }
            Err(_) => {
                let text = text.replace("\r\n", "\n");
                self.command(move |ed, s, r| {
                    let mut t =
                        notekpr_draw::edit::new::text(at[0], at[1], &text, &ed.style, &measure, r);
                    let top = s.elements.iter().filter_map(Element::index).max();
                    t.set(
                        "index",
                        notekpr_draw::order::key_between(top, None)
                            .map_or(Value::Null, Value::String),
                    );
                    t.set("x", notekpr_draw::scene::num(at[0] - t.width() / 2.0));
                    t.set("y", notekpr_draw::scene::num(at[1] - t.height() / 2.0));
                    ed.selection = vec![t.id().to_string()];
                    Output {
                        changed: vec![t],
                        done: true,
                        ..Output::default()
                    }
                });
            }
        }
    }

    /// Pictures that came in inside elements (pasted, dropped) become files
    /// of the vault, which the drawing then shows.
    fn adopt_files(self: &Rc<Self>, files: notekpr_draw::scene::Props) {
        let me = self.clone();
        leptos::task::spawn_local(async move {
            let name = me.export_name();
            let mut known = HashMap::new();
            let files = crate::actions::adopt_pictures(&me.vault, &name, files, &mut known).await;
            me.put_files(&files);
            me.schedule();
        });
    }

    /// Puts pictures (image files) in the drawing, centred at `at` (in the
    /// drawing; else the middle of the view), side by side; each becomes a
    /// file of the vault (`attachments/`).
    pub fn add_pictures(self: &Rc<Self>, files: Vec<web_sys::File>, at: Option<[f64; 2]>) {
        if !self.writable() {
            return;
        }
        let me = self.clone();
        leptos::task::spawn_local(async move {
            let v = me.view.get();
            let mut at = at.unwrap_or_else(|| v.to_scene([v.width / 2.0, v.height / 2.0]));
            for f in files {
                let mime = f.type_();
                if !mime.starts_with("image/") {
                    crate::state::error(format!("{} isn't a picture.", f.name()));
                    continue;
                }
                let Some(bytes) = util::read_file(&f).await else {
                    continue;
                };
                let Some(url) = util::blob_url(&bytes, &mime) else {
                    continue;
                };
                let Ok(img) = web_sys::HtmlImageElement::new() else {
                    continue;
                };
                img.set_src(&url);
                if wasm_bindgen_futures::JsFuture::from(img.decode())
                    .await
                    .is_err()
                {
                    crate::state::error(format!("{} can't be shown.", f.name()));
                    continue;
                }
                let path = format!("attachments/{}", f.name());
                let Some((att, _)) =
                    crate::actions::upload_attachment(&me.vault, &path, &bytes).await
                else {
                    continue;
                };
                crate::drawings::remember_picture(
                    &att,
                    img.clone(),
                    notekpr_core::pictures::data_url(&mime, &bytes),
                );
                let file_id = crate::state::new_id();
                let mut entry = notekpr_draw::scene::Props::new();
                entry.insert(
                    file_id.clone(),
                    notekpr_core::pictures::entry(&file_id, &mime, &att, js_sys::Date::now()),
                );
                me.put_files(&entry);
                // As big as it is, but no more than half the view's height
                // (`initializeImageDimensions`).
                let (nw, nh) = (img.natural_width() as f64, img.natural_height() as f64);
                let v = me.view.get();
                let max_h = (v.height - 120.0)
                    .max(160.0)
                    .min((v.height * 0.5).floor() / v.zoom);
                let h = nh.min(max_h).max(1.0);
                let w = h * nw / nh.max(1.0);
                let place = at;
                me.command(|ed, s, r| {
                    let pic = notekpr_draw::edit::new::image(
                        place[0] - w / 2.0,
                        place[1] - h / 2.0,
                        w,
                        h,
                        &file_id,
                        &ed.style,
                        r,
                    );
                    ed.add_on_top(s, pic, r)
                });
                at[0] += w + 20.0;
            }
            me.changed();
            me.schedule();
        });
    }

    /// Adds pictures to the drawing's files.
    fn put_files(&self, files: &notekpr_draw::scene::Props) {
        let (v, id) = (self.vault.clone(), self.id.clone());
        step(|c| {
            let Some(d) = c.replicas.get_mut(&v).and_then(|r| r.notes.get_mut(&id)) else {
                return;
            };
            if let Some(u) = d.drawing().put_files(files) {
                state::local_update(c, &v, &id, u);
            }
        });
        self.reread();
    }

    /// The drawing's file name for exports: its title.
    fn export_name(&self) -> String {
        step(|c| c.entry(&self.vault, &self.id))
            .map(|e| e.title().to_string())
            .unwrap_or_else(|| "Drawing".into())
    }

    /// What is selected (with labels), else everything: what "Copy as"
    /// copies.
    fn selected_part(&self, scene: &Scene) -> Scene {
        let ed = self.editor.borrow();
        if ed.selection.is_empty() {
            return scene.clone();
        }
        let mut ids: std::collections::HashSet<String> = ed.selection.iter().cloned().collect();
        for e in ed.selected(scene) {
            for b in e.bound_elements() {
                if b.kind == "text" {
                    ids.insert(b.id);
                }
            }
        }
        Scene {
            elements: scene
                .elements
                .iter()
                .filter(|e| ids.contains(e.id()))
                .cloned()
                .collect(),
            ..scene.clone()
        }
    }

    /// Copies what is selected (else the whole drawing) as a PNG picture,
    /// or as an SVG image (its text).
    pub fn copy_as(self: &Rc<Self>, png: bool) {
        self.write();
        let scene = self.reread();
        let part = self.selected_part(&scene);
        let dark = crate::drawings::is_dark();
        if png {
            match png_of(&self.vault, &part, dark) {
                Some(bytes) if util::copy_png_to_clipboard(&bytes) => {
                    crate::state::success("Copied as a PNG picture.")
                }
                _ => crate::state::error("The picture could not be copied."),
            }
            return;
        }
        let v = self.vault.clone();
        leptos::task::spawn_local(async move {
            let svg = crate::drawings::svg_of(&v, part, dark).await;
            util::copy_to_clipboard(&svg);
            crate::state::success("Copied as an SVG image.");
        });
    }

    /// Copies what shows the drawing in a note: `![[Name.drawing]]`.
    pub fn copy_embed(&self) {
        let Some(e) = step(|c| c.entry(&self.vault, &self.id)) else {
            return;
        };
        util::copy_to_clipboard(&format!("![[{}]]", notekpr_core::paths::file_name(&e.path)));
        crate::state::success("Link copied — paste it into a note to show the drawing there.");
    }

    /// Downloads the drawing as `kind`.
    pub fn export(self: &Rc<Self>, kind: Export) {
        self.write();
        let scene = self.reread();
        let name = self.export_name();
        let dark = crate::drawings::is_dark();
        // Files hold the pictures inside.
        let file = |write: fn(&Scene) -> String, name: String, mime: &'static str| {
            let (v, scene) = (self.vault.clone(), scene.clone());
            leptos::task::spawn_local(async move {
                let full = crate::drawings::with_pictures(&v, (*scene).clone()).await;
                util::download(&name, mime, write(&full).as_bytes());
            });
        };
        match kind {
            Export::Drawing => file(
                notekpr_draw::file::write_drawing,
                format!("{name}.drawing"),
                "application/json",
            ),
            Export::Excalidraw => file(
                notekpr_draw::file::write_excalidraw,
                format!("{name}.excalidraw"),
                "application/vnd.excalidraw+json",
            ),
            Export::Obsidian => file(
                |s| {
                    let mut ids = notekpr_draw::restore::Ids::new(js_sys::Date::now() as u64);
                    notekpr_draw::obsidian::write(s, true, &mut ids)
                },
                format!("{name}.excalidraw.md"),
                "text/markdown",
            ),
            Export::Svg => {
                let (v, id) = (self.vault.clone(), self.id.clone());
                leptos::task::spawn_local(async move {
                    // The preview's fonts are inside: the file stands alone.
                    if let Some(svg) = crate::drawings::standalone_svg(&v, &id, dark).await {
                        util::download(&format!("{name}.svg"), "image/svg+xml", svg.as_bytes());
                    }
                });
            }
            Export::Png => {
                let Some(png) = png_of(&self.vault, &scene, dark) else {
                    crate::state::error("The picture could not be made.");
                    return;
                };
                util::download(&format!("{name}.png"), "image/png", &png);
            }
        }
    }
}

/// The drawing as a PNG picture, twice its size (sharp on dense screens).
fn png_of(vault: &str, scene: &Scene, dark: bool) -> Option<Vec<u8>> {
    const SCALE: f64 = 2.0;
    const PAD: f64 = 10.0;
    // Frames' names above them are in the picture.
    use notekpr_draw::edit::frame;
    let frames: Vec<&Element> = scene.visible().filter(|e| frame::is_frame(e)).collect();
    let names = frame::name_labels(&frames, dark, &measure);
    let [x1, y1, x2, y2] = frame::export_bounds(scene, names.values());
    let (w, h) = ((x2 - x1).abs() + PAD * 2.0, (y2 - y1).abs() + PAD * 2.0);
    let canvas: web_sys::HtmlCanvasElement = util::document()
        .create_element("canvas")
        .ok()?
        .dyn_into()
        .ok()?;
    canvas.set_width((w * SCALE).ceil() as u32);
    canvas.set_height((h * SCALE).ceil() as u32);
    let ctx: web_sys::CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    let view = notekpr_draw::viewport::Viewport {
        scroll_x: PAD - x1,
        scroll_y: PAD - y1,
        zoom: 1.0,
        width: w,
        height: h,
    };
    // The pictures are the ones loaded for the canvas.
    let pictures = |e: &Element| {
        let entry = scene.files.get(e.file_id()?)?;
        crate::drawings::picture(vault, entry, || {})
    };
    super::canvas::draw_scene(
        &ctx,
        scene,
        &view,
        &super::canvas::Look {
            pixel_ratio: SCALE,
            dark,
            grid: None,
            pictures: &pictures,
        },
        &mut super::canvas::ShapeCache::default(),
        None,
    );
    let url = canvas.to_data_url_with_type("image/png").ok()?;
    let b64 = url.split_once(',')?.1;
    notekpr_core::b64::decode(b64).ok()
}
