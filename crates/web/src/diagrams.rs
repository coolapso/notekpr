//! Mermaid diagrams, drawn by the diagram module (`crates/diagram`): a
//! WebAssembly module of its own, too big to load with the app, loaded the
//! first time a note shows a diagram (`notekprDiagrams` in `boot.js`). Until
//! then a diagram says it is being drawn; once loaded, the open note is shown
//! again (`content_rev`). docs/development.md ("Diagrams") has the why.
//!
//! The module is another program's memory: if a call into it throws
//! anything but its own error message (it crashed: a panic or a stack
//! overflow in WebAssembly), it is dropped and a fresh one loaded, a few
//! times at most.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use leptos::prelude::*;
use wasm_bindgen::{JsCast, JsValue};

use crate::editor::Drawing;
use crate::state::ui;
use crate::util;

/// How many drawings are kept (a note's diagrams, both themes, recent edits).
const KEEP: usize = 64;

/// How many times a module that crashed is loaded again.
const RELOADS: u32 = 3;

/// How long after the module didn't load (offline, say) it is tried again.
const RETRY_MS: f64 = 30_000.0;

enum Module {
    Idle,
    Loading,
    /// Its `render(code, dark, font, id)`.
    Ready(js_sys::Function),
    /// Why there are no drawings, and when to load it again, if ever.
    Failed(String, Option<f64>),
}

struct Diagrams {
    module: Module,
    /// Modules loaded so far (the next one is loaded fresh, by its URL).
    loads: u32,
    drawn: HashMap<Key, Result<String, String>>,
    order: VecDeque<Key>,
    /// The font text is drawn in (the app's `--sans`).
    font: Option<String>,
}

/// A drawing: of which code, in the dark theme or not, for which view.
type Key = (u64, bool, View);

/// Where a diagram is shown. A page can hold the same diagram in two views
/// at once (the editor stays, hidden, under the reading view): their SVGs
/// need ids of their own, or one's arrows would point at the hidden one's
/// markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Editor,
    Reading,
    /// An earlier version (the history).
    History,
}

thread_local! {
    static DIAGRAMS: RefCell<Diagrams> = RefCell::new(Diagrams {
        module: Module::Idle,
        loads: 0,
        drawn: HashMap::new(),
        order: VecDeque::new(),
        font: None,
    });
}

fn hash(code: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// The app's sans-serif font stack, as CSS.
fn app_font() -> String {
    util::document()
        .document_element()
        .and_then(|root| util::window().get_computed_style(&root).ok().flatten())
        .and_then(|s| s.get_property_value("--sans").ok())
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "sans-serif".into())
}

/// `code` (a ```` ```mermaid ```` block's) as drawn now, in the app's theme,
/// for `view`.
pub fn drawing(code: &str, view: View) -> Drawing {
    let dark = crate::drawings::is_dark();
    let key = (hash(code), dark, view);
    let call = DIAGRAMS.with(|d| {
        let mut d = d.borrow_mut();
        if let Some(r) = d.drawn.get(&key) {
            return Err(Some(r.clone()));
        }
        if let Module::Failed(_, Some(at)) = d.module
            && js_sys::Date::now() >= at
        {
            d.module = Module::Idle;
        }
        match &d.module {
            Module::Ready(f) => {
                let f = f.clone();
                let font = d.font.get_or_insert_with(app_font).clone();
                Ok((f, font))
            }
            Module::Failed(why, _) => Err(Some(Err(why.clone()))),
            Module::Loading => Err(None),
            Module::Idle => {
                d.module = Module::Loading;
                let fresh = d.loads;
                d.loads += 1;
                leptos::task::spawn_local(load(fresh));
                Err(None)
            }
        }
    });
    let (render, font) = match call {
        Err(Some(r)) => return as_drawing(r),
        Err(None) => return Drawing::Loading,
        Ok(f) => f,
    };
    let place = match view {
        View::Editor => 'e',
        View::Reading => 'r',
        View::History => 'h',
    };
    let id = format!("dg{place}{:x}{}", key.0, if dark { "d" } else { "" });
    let args = js_sys::Array::of4(
        &JsValue::from_str(code),
        &JsValue::from_bool(dark),
        &JsValue::from_str(&font),
        &JsValue::from_str(&id),
    );
    let result = match render.apply(&JsValue::NULL, &args) {
        Ok(svg) => Ok(svg.as_string().unwrap_or_default()),
        // Its own message: the diagram can't be drawn.
        Err(e) if e.is_string() => Err(e.as_string().unwrap_or_default()),
        // It crashed: start over with a fresh module.
        Err(e) => {
            util::warn(&format!("The diagram module stopped: {e:?}"));
            DIAGRAMS.with(|d| {
                let mut d = d.borrow_mut();
                d.module = if d.loads < RELOADS {
                    Module::Idle
                } else {
                    Module::Failed(
                        "drawing diagrams stopped working; reload the page".into(),
                        None,
                    )
                };
            });
            Err("this diagram stopped the program that draws diagrams".into())
        }
    };
    DIAGRAMS.with(|d| {
        let mut d = d.borrow_mut();
        if d.order.len() >= KEEP
            && let Some(old) = d.order.pop_front()
        {
            d.drawn.remove(&old);
        }
        d.order.push_back(key);
        d.drawn.insert(key, result.clone());
    });
    as_drawing(result)
}

fn as_drawing(r: Result<String, String>) -> Drawing {
    match r {
        Ok(svg) => Drawing::Svg(svg),
        Err(why) => Drawing::Failed(why),
    }
}

/// Load the module (`fresh`: how many were loaded before; a later one is
/// a new instance), then show the open note again.
async fn load(fresh: u32) {
    let loaded = async {
        let loader: js_sys::Function =
            js_sys::Reflect::get(&util::window(), &"notekprDiagrams".into())
                .ok()?
                .dyn_into()
                .ok()?;
        let promise: js_sys::Promise = loader
            .call1(&JsValue::NULL, &fresh.into())
            .ok()?
            .dyn_into()
            .ok()?;
        wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()
    }
    .await;
    DIAGRAMS.with(|d| {
        d.borrow_mut().module = match loaded {
            Some(render) => Module::Ready(render),
            None => Module::Failed(
                "what draws diagrams didn’t load (offline?)".into(),
                Some(js_sys::Date::now() + RETRY_MS),
            ),
        };
    });
    ui().content_rev.update(|n| *n += 1);
}
