//! The drawing editor's runtime: one open drawing on a `<canvas>`, its
//! viewport (scroll and zoom), and the browser objects tied to it. The
//! page component (`views::drawing`) creates a [`Board`] and hands it the
//! canvas; the board draws on the next animation frame after anything
//! changes.

pub mod canvas;
pub mod editing;
pub mod frames;
pub mod others;
pub mod snapping;
pub mod sticky;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use std::collections::HashMap;

use notekpr_draw::bounds;
use notekpr_draw::edit::Editor;
use notekpr_draw::scene::{Element, Scene};
use notekpr_draw::viewport::Viewport;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::drawings;
use crate::util;

/// One open drawing.
pub struct Board {
    pub vault: String,
    pub id: String,
    pub view: Cell<Viewport>,
    pixel_ratio: Cell<f64>,
    /// The drawing as last read (see [`Board::reload`]).
    scene: RefCell<Rc<Scene>>,
    cache: RefCell<canvas::ShapeCache>,
    canvas: RefCell<Option<HtmlCanvasElement>>,
    resize: RefCell<Option<(web_sys::ResizeObserver, Closure<dyn FnMut(js_sys::Array)>)>>,
    wheel: RefCell<Option<Closure<dyn FnMut(web_sys::WheelEvent)>>>,
    frame: Cell<Option<i32>>,
    /// The view was fitted to the drawing once (on opening).
    fitted: Cell<bool>,
    closed: Cell<bool>,
    /// A drag that scrolls the view: the pointer and where it was.
    pub pan: Cell<Option<(i32, f64, f64)>>,
    /// Told the zoom after each frame (for the zoom control).
    on_zoom: RefCell<Option<Box<dyn Fn(f64)>>>,
    /// Fonts asked for (they load in the background, then it's drawn again).
    fonts_asked: RefCell<std::collections::HashSet<String>>,
    /// Tools, selection, the style of new elements, the gesture going on.
    pub editor: RefCell<Editor>,
    /// Changes shown but not written to the drawing yet, by element.
    unwritten: RefCell<HashMap<String, Element>>,
    last_write: Cell<f64>,
    write_soon: Cell<bool>,
    /// Told when the tool, selection or style changed (the page's panels).
    on_change: RefCell<Option<Box<dyn Fn()>>>,
    /// Told the text to edit (or "" when editing ends).
    on_edit_text: RefCell<Option<Box<dyn Fn(String)>>>,
    text_draft: RefCell<String>,
    /// Where the pointer last was on the canvas (pastes go there).
    last_pointer: Cell<Option<[f64; 2]>>,
    /// Counts pointer moves: a look again after holding still (an arrow's
    /// end over a shape) is for the last one only.
    moves: Cell<u32>,
    /// The others on the drawing (presence), and the lasers' trails.
    others: RefCell<Vec<others::Other>>,
    trails: RefCell<others::Trails>,
    /// What we last told the others we selected.
    shared_selection: RefCell<Vec<String>>,
}

pub type Rt = Rc<Board>;

impl Board {
    pub fn new(vault: &str, id: &str) -> Rt {
        Rc::new(Board {
            vault: vault.to_string(),
            id: id.to_string(),
            view: Cell::new(Viewport::default()),
            pixel_ratio: Cell::new(1.0),
            scene: RefCell::new(Rc::new(Scene::default())),
            cache: RefCell::new(canvas::ShapeCache::default()),
            canvas: RefCell::new(None),
            resize: RefCell::new(None),
            wheel: RefCell::new(None),
            frame: Cell::new(None),
            fitted: Cell::new(false),
            closed: Cell::new(false),
            pan: Cell::new(None),
            on_zoom: RefCell::new(None),
            fonts_asked: RefCell::new(std::collections::HashSet::new()),
            editor: RefCell::new({
                let mut ed = Editor::default();
                ed.measure = editing::measure;
                ed.snap.to_objects = snapping::remembered();
                ed
            }),
            unwritten: RefCell::new(HashMap::new()),
            last_write: Cell::new(0.0),
            write_soon: Cell::new(false),
            on_change: RefCell::new(None),
            on_edit_text: RefCell::new(None),
            text_draft: RefCell::new(String::new()),
            last_pointer: Cell::new(None),
            moves: Cell::new(0),
            others: RefCell::new(Vec::new()),
            trails: RefCell::new(others::Trails::default()),
            shared_selection: RefCell::new(Vec::new()),
        })
    }

    /// Calls `f` when the tool, selection or style changed.
    pub fn on_change(&self, f: impl Fn() + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(f));
    }

    /// Calls `f` with a text to edit (or "" when editing ends).
    pub fn on_edit_text(&self, f: impl Fn(String) + 'static) {
        *self.on_edit_text.borrow_mut() = Some(Box::new(f));
    }

    /// Asks the browser for the fonts the drawing's texts use: a canvas
    /// doesn't make it load them. Draws again once they're there.
    fn load_fonts(self: &Rc<Self>) {
        let scene = self.scene();
        let fonts = util::document().fonts();
        for e in scene.visible() {
            if e.kind() != notekpr_draw::scene::ElementType::Text {
                continue;
            }
            let family = notekpr_draw::fonts::get(e.font_family()).name;
            // Per font and character range (the files are split by them).
            let sample: String = e
                .text()
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(64)
                .collect();
            let key = format!(
                "{family}:{}",
                sample.chars().map(|c| (c as u32) >> 8).max().unwrap_or(0)
            );
            if !self.fonts_asked.borrow_mut().insert(key) {
                continue;
            }
            let promise = fonts.load_with_text(&format!("20px \"{family}\""), &sample);
            let me = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                    me.schedule();
                }
            });
        }
    }

    /// Calls `f` with the zoom after each frame.
    pub fn on_zoom(&self, f: impl Fn(f64) + 'static) {
        *self.on_zoom.borrow_mut() = Some(Box::new(f));
    }

    /// The drawing as last read.
    pub fn scene(&self) -> Rc<Scene> {
        self.scene.borrow().clone()
    }

    /// Reads the drawing again (it changed) and draws it.
    pub fn reload(self: &Rc<Self>) {
        self.reread();
        self.load_fonts();
        self.fit_once();
        self.schedule();
    }

    /// On opening: the whole drawing in view (zoomed in no further than 1).
    fn fit_once(&self) {
        let v = self.view.get();
        if self.fitted.get() || v.width <= 0.0 {
            return;
        }
        // Loaded and empty: nothing to fit, and the view stays as it is
        // once drawing starts.
        if drawings::scene(&self.vault, &self.id).is_none() {
            return;
        }
        self.fitted.set(true);
        let scene = self.scene();
        if scene.visible().next().is_none() {
            return;
        }
        let content = bounds::common_bounds(scene.visible());
        self.view.set(v.fitted(content, 1.0));
    }

    /// Draws on the next frame.
    pub fn schedule(self: &Rc<Self>) {
        if self.closed.get() || self.frame.get().is_some() {
            return;
        }
        let me = self.clone();
        let cb = Closure::once_into_js(move || {
            me.frame.set(None);
            me.draw();
        });
        if let Ok(id) = util::window().request_animation_frame(cb.unchecked_ref()) {
            self.frame.set(Some(id));
        }
    }

    fn draw(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        let Some(canvas) = self.canvas.borrow().clone() else {
            return;
        };
        let Some(ctx) = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|c| c.dyn_into::<CanvasRenderingContext2d>().ok())
        else {
            return;
        };
        let scene = self.scene();
        let view = self.view.get();
        let pictures = |e: &Element| {
            let entry = scene.files.get(e.file_id()?)?;
            let me = self.clone();
            drawings::picture(&self.vault, entry, move || me.schedule())
        };
        canvas::draw_scene(
            &ctx,
            &scene,
            &view,
            &canvas::Look {
                pixel_ratio: self.pixel_ratio.get(),
                dark: drawings::is_dark(),
                grid: notekpr_draw::edit::snap::Grid::of(&scene),
                pictures: &pictures,
            },
            &mut self.cache.borrow_mut(),
            Some(&self.editor.borrow()),
        );
        canvas::draw_overlay(
            &ctx,
            &scene,
            &self.editor.borrow(),
            &view,
            self.pixel_ratio.get(),
        );
        // The others: what they selected, the lasers, their pointers.
        {
            let s = self.pixel_ratio.get() * view.zoom;
            ctx.set_transform(s, 0.0, 0.0, s, view.scroll_x * s, view.scroll_y * s)
                .ok();
            canvas::draw_links(&ctx, &scene, view.zoom);
            let list = self.others.borrow();
            others::draw_selections(&ctx, &scene, &list, view.zoom);
            let mut trails = self.trails.borrow_mut();
            others::draw_trails(&ctx, &mut trails, view.zoom);
            others::draw_pointers(&ctx, &list, &view, self.pixel_ratio.get());
            // Lasers fade: frames go on while one does.
            if trails.active() {
                drop(trails);
                self.schedule();
            }
        }
        if let Some(f) = self.on_zoom.borrow().as_ref() {
            f(self.view.get().zoom);
        }
    }

    /// Takes the canvas: follows its size, zooms and scrolls with the wheel.
    pub fn attach(self: &Rc<Self>, canvas: HtmlCanvasElement) {
        if self.canvas.borrow().is_some() {
            return;
        }
        *self.canvas.borrow_mut() = Some(canvas.clone());
        let me = self.clone();
        let on_resize = Closure::wrap(Box::new(move |_: js_sys::Array| {
            let Some(c) = me.canvas.borrow().clone() else {
                return;
            };
            let (w, h) = (c.client_width() as f64, c.client_height() as f64);
            let dpr = util::window().device_pixel_ratio().max(1.0);
            c.set_width((w * dpr).round() as u32);
            c.set_height((h * dpr).round() as u32);
            me.pixel_ratio.set(dpr);
            let mut v = me.view.get();
            v.width = w;
            v.height = h;
            me.view.set(v);
            me.fit_once();
            me.schedule();
        }) as Box<dyn FnMut(js_sys::Array)>);
        if let Ok(observer) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
            observer.observe(&canvas);
            *self.resize.borrow_mut() = Some((observer, on_resize));
        }
        // A listener of our own: it must keep the page from scrolling.
        let me = self.clone();
        let on_wheel = Closure::wrap(Box::new(move |ev: web_sys::WheelEvent| {
            ev.prevent_default();
            let lines = if ev.delta_mode() == 1 { 16.0 } else { 1.0 };
            let (dx, dy) = (ev.delta_x() * lines, ev.delta_y() * lines);
            let v = me.view.get();
            let next = if ev.ctrl_key() || ev.meta_key() {
                // Pinching on a touchpad comes as a wheel with Ctrl.
                let at = [ev.offset_x() as f64, ev.offset_y() as f64];
                v.zoomed_at(v.zoom * (-dy * 0.01).exp(), at)
            } else if ev.shift_key() && dx == 0.0 {
                v.scrolled(-dy, 0.0)
            } else {
                v.scrolled(-dx, -dy)
            };
            me.view.set(next);
            me.schedule();
        }) as Box<dyn FnMut(web_sys::WheelEvent)>);
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_passive(false);
        let _ = canvas.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            on_wheel.as_ref().unchecked_ref(),
            &opts,
        );
        *self.wheel.borrow_mut() = Some(on_wheel);
        self.enable_undo();
        others::register(self);
        self.reload();
    }

    /// Who else is on the drawing now (from presence).
    pub fn set_others(self: &Rc<Self>, list: Vec<others::Other>) {
        self.trails.borrow_mut().follow(&list);
        *self.others.borrow_mut() = list;
        self.schedule();
    }

    /// Tells the others where our pointer is on the drawing (`None`: off
    /// it), whether with the laser and held down, and what we selected.
    pub fn share_pointer(&self, at: Option<[f64; 2]>, laser: bool, down: bool) {
        let pointer = at.map(|[x, y]| notekpr_core::engine::DrawingPointer { x, y, laser, down });
        let selected = self.editor.borrow().selection.clone();
        crate::state::step(|c| {
            c.drawing_pointer = pointer;
            c.drawing_selection = selected;
            crate::state::send_pointer(c);
        });
    }

    /// Tells the others what we selected, when it changed.
    pub fn share_selection(&self) {
        let selected = self.editor.borrow().selection.clone();
        if *self.shared_selection.borrow() == selected {
            return;
        }
        *self.shared_selection.borrow_mut() = selected.clone();
        crate::state::step(|c| {
            c.drawing_selection = selected;
            crate::state::send_pointer(c);
        });
    }

    /// Zooms by `factor` around the middle of the view.
    pub fn zoom_by(self: &Rc<Self>, factor: f64) {
        let v = self.view.get();
        self.view
            .set(v.zoomed_at(v.zoom * factor, [v.width / 2.0, v.height / 2.0]));
        self.schedule();
    }

    /// The whole drawing in view.
    pub fn fit(self: &Rc<Self>) {
        let scene = self.scene();
        if scene.visible().next().is_some() {
            let v = self.view.get();
            self.view
                .set(v.fitted(bounds::common_bounds(scene.visible()), 1.0));
            self.schedule();
        }
    }

    /// Lets the canvas go (the page closed).
    pub fn close(&self) {
        self.write();
        self.closed.set(true);
        // Not on the drawing any more, for the others (at an edge: closing
        // may happen while the state is taken).
        leptos::task::spawn_local(async {
            crate::state::step(|c| {
                c.drawing_pointer = None;
                c.drawing_selection.clear();
            });
        });
        if let Some((observer, _)) = self.resize.borrow_mut().take() {
            observer.disconnect();
        }
        if let (Some(c), Some(w)) = (
            self.canvas.borrow_mut().take(),
            self.wheel.borrow_mut().take(),
        ) {
            let _ = c.remove_event_listener_with_callback("wheel", w.as_ref().unchecked_ref());
        }
    }
}
