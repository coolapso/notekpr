//! The drawing editor's runtime: one open drawing on a `<canvas>`, its
//! viewport (scroll and zoom), and the browser objects tied to it. The
//! page component (`views::drawing`) creates a [`Board`] and hands it the
//! canvas; the board draws on the next animation frame after anything
//! changes.

pub mod canvas;
pub mod editing;

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
            editor: RefCell::new(Editor::default()),
            unwritten: RefCell::new(HashMap::new()),
            last_write: Cell::new(0.0),
            write_soon: Cell::new(false),
            on_change: RefCell::new(None),
            on_edit_text: RefCell::new(None),
            text_draft: RefCell::new(String::new()),
            last_pointer: Cell::new(None),
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
        let scene = self.scene();
        if scene.visible().next().is_none() {
            return;
        }
        let content = bounds::common_bounds(scene.visible());
        self.view.set(v.fitted(content, 1.0));
        self.fitted.set(true);
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

    fn draw(&self) {
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
        canvas::draw_scene(
            &ctx,
            &scene,
            &view,
            self.pixel_ratio.get(),
            drawings::is_dark(),
            &mut self.cache.borrow_mut(),
        );
        canvas::draw_overlay(
            &ctx,
            &scene,
            &self.editor.borrow(),
            &view,
            self.pixel_ratio.get(),
        );
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
        self.reload();
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
