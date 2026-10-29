//! Saved shapes on the board, as Excalidraw's library (`data/library.ts`,
//! `actions/actionAddToLibrary.ts`): kept in one's Personal storage (the
//! quick notes' index, `notekpr_core::library`), the same on every device.
//! What is selected becomes a saved shape; a shape goes into the drawing as
//! copies of its elements, where it is dropped or in the middle of the view;
//! they come and go as Excalidraw's `.excalidrawlib` files. The panel
//! showing them is `views::library`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use leptos::prelude::*;
use notekpr_core::library::{self, LibraryItem};
use notekpr_core::protocol::INDEX_OBJECT_ID;
use notekpr_draw::scene::{Element, Scene};
use notekpr_draw::svg::{self, SvgOptions};
use wasm_bindgen::JsCast;

use super::Board;
use super::editing::WebRandom;
use crate::state::{self, step, ui};
use crate::util;

/// What a library item dragged onto the canvas carries: its id, as
/// Excalidraw's `excalidrawlibIds` (`{"itemIds": […]}`).
pub const DRAG_TYPE: &str = "application/vnd.excalidrawlib.ids+json";

/// Library files' type.
const FILE_TYPE: &str = "application/vnd.excalidrawlib+json";

thread_local! {
    /// Items' pictures (blob URLs) by item: the version they show.
    static PICTURES: RefCell<HashMap<String, (u64, String)>> = RefCell::new(HashMap::new());
}

/// The saved shapes, newest first (none before the Personal storage is
/// made).
pub fn items() -> Vec<LibraryItem> {
    step(|c| {
        let v = c.quick_notes()?;
        Some(library::items(&c.replicas.get(&v)?.index))
    })
    .unwrap_or_default()
}

/// The Personal storage the shapes are kept in, made the first time.
async fn section() -> Option<String> {
    let v = crate::quick::ensure().await;
    if v.is_none() {
        state::error("Saved shapes can't be changed right now: try again once online.");
    }
    v
}

fn changed() {
    ui().library_rev.update(|n| *n += 1);
}

/// Saves `item`; whether it was.
pub async fn add(item: LibraryItem) -> bool {
    let Some(v) = section().await else {
        return false;
    };
    let done = step(|c| {
        let budget = state::limits().edit_budget();
        let r = library::add(&c.replicas.get(&v)?.index, &item, budget);
        if let Ok(Some(u)) = &r {
            state::local_update(c, &v, INDEX_OBJECT_ID, u.clone());
        }
        Some(r)
    });
    changed();
    match done {
        Some(Ok(_)) => true,
        Some(Err(e)) => {
            state::error(format!("{}.", capitalized(&e.to_string())));
            false
        }
        None => false,
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Removes a saved shape.
pub fn remove(id: &str) {
    step(|c| {
        let v = c.quick_notes()?;
        let u = library::remove(&c.replicas.get(&v)?.index, [id])?;
        state::local_update(c, &v, INDEX_OBJECT_ID, u);
        Some(())
    });
    changed();
}

/// Saves the shapes of an `.excalidrawlib` file (`text`), leaving out those
/// saved already (`mergeLibraryItems`).
pub async fn import(text: String) {
    let now = js_sys::Date::now();
    let mut ids = notekpr_draw::restore::Ids::new(now as u64);
    let Ok(items) = notekpr_draw::file::read_library(&text, now, &mut ids) else {
        state::error("That isn't an Excalidraw library (.excalidrawlib).");
        return;
    };
    if items.is_empty() {
        state::info("The library file holds no shapes.");
        return;
    }
    let Some(v) = section().await else {
        return;
    };
    let done = step(|c| {
        let budget = state::limits().edit_budget();
        let done = library::import(&c.replicas.get(&v)?.index, items, budget);
        for u in &done.updates {
            state::local_update(c, &v, INDEX_OBJECT_ID, u.clone());
        }
        Some(done)
    });
    changed();
    let Some(done) = done else {
        return;
    };
    let mut said = format!("Saved {}.", util::count(done.added, "shape", "shapes"));
    if done.already > 0 {
        said.push_str(&format!(
            " {} saved already.",
            util::count(done.already, "was", "were")
        ));
    }
    if done.too_large > 0 {
        said.push_str(&format!(
            " {} too large to keep on this server.",
            util::count(done.too_large, "shape was", "shapes were")
        ));
        state::error(said);
    } else {
        state::success(said);
    }
}

/// Saves the shapes of the library files among `files` (`.excalidrawlib`);
/// the others are returned. `open`: the saved shapes' panel, opened when
/// there were some.
pub fn import_files(files: Vec<web_sys::File>, open: RwSignal<bool>) -> Vec<web_sys::File> {
    let (libraries, others): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|f| f.name().ends_with(".excalidrawlib") || f.type_() == FILE_TYPE);
    if !libraries.is_empty() {
        let _ = open.try_set(true);
    }
    for f in libraries {
        leptos::task::spawn_local(async move {
            let text = wasm_bindgen_futures::JsFuture::from(f.text())
                .await
                .ok()
                .and_then(|t| t.as_string());
            if let Some(text) = text {
                import(text).await;
            }
        });
    }
    others
}

/// Downloads the saved shapes as an `.excalidrawlib` file.
pub fn export() {
    let items = items();
    if items.is_empty() {
        state::info("No saved shapes yet.");
        return;
    }
    let file = notekpr_draw::file::write_library(&items);
    util::download("Saved shapes.excalidrawlib", FILE_TYPE, file.as_bytes());
}

/// A picture of `elements` (an item's, or the selection's) as a blob URL,
/// made again when they or the theme changed. `key`: whose it is.
pub async fn picture(key: &str, elements: &[Element]) -> Option<String> {
    let dark = crate::drawings::is_dark();
    let version = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for e in elements {
            serde_json::to_string(e.props())
                .unwrap_or_default()
                .hash(&mut h);
        }
        dark.hash(&mut h);
        h.finish()
    };
    let cached = PICTURES.with(|p| {
        p.borrow()
            .get(key)
            .filter(|(v, _)| *v == version)
            .map(|(_, url)| url.clone())
    });
    if cached.is_some() {
        return cached;
    }
    let scene = Scene {
        elements: elements.to_vec(),
        ..Scene::default()
    };
    // An image can't load fonts: those its texts need are put inside.
    let font_faces = crate::drawings::font_faces(&scene).await;
    let image = svg::export(
        &scene,
        &SvgOptions {
            dark,
            background: false,
            font_faces: Some(font_faces),
            time: super::sticky::local_time(),
            measure: super::editing::measure,
            ..SvgOptions::default()
        },
    );
    let url = util::blob_url(image.as_bytes(), "image/svg+xml")?;
    let old = PICTURES.with(|p| {
        p.borrow_mut()
            .insert(key.to_string(), (version, url.clone()))
    });
    if let Some((_, old)) = old {
        let _ = web_sys::Url::revoke_object_url(&old);
    }
    Some(url)
}

impl Board {
    /// What is selected as a new saved shape, or why it can't be one.
    pub fn library_item(&self) -> Result<LibraryItem, &'static str> {
        self.editor
            .borrow()
            .library_item(&self.scene(), &mut WebRandom)
    }

    /// Saves what is selected as a shape (`actionAddToLibrary`).
    pub fn add_to_library(&self) {
        match self.library_item() {
            Ok(item) => leptos::task::spawn_local(async move {
                if add(item).await {
                    state::success("Added to saved shapes");
                }
            }),
            Err(why) => state::error(why),
        }
    }

    /// Puts a library item in the drawing: copies of its elements with new
    /// ids, centred at `at` on the canvas (else in the middle of the
    /// view), selected.
    pub fn insert_library_item(self: &Rc<Self>, item: &LibraryItem, at: Option<[f64; 2]>) {
        if !self.writable() {
            return;
        }
        let v = self.view.get();
        let at = v.to_scene(at.unwrap_or([v.width / 2.0, v.height / 2.0]));
        let elements = item.elements.clone();
        self.command(move |ed, s, r| ed.paste(s, &elements, at, r));
    }

    /// A library item dragged from the panel and dropped: put in where it
    /// was dropped on the canvas (dropped elsewhere, nothing happens).
    /// Whether it was one.
    pub fn drop_library_item(self: &Rc<Self>, ev: &web_sys::DragEvent) -> bool {
        let Some(data) = ev
            .data_transfer()
            .and_then(|d| d.get_data(DRAG_TYPE).ok())
            .filter(|d| !d.is_empty())
        else {
            return false;
        };
        ev.prevent_default();
        let canvas = self.canvas.borrow().clone();
        let on_canvas = canvas.as_ref().is_some_and(|c| {
            ev.target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|t| c.is_same_node(Some(&t)))
        });
        let wanted: Vec<String> = serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|v| {
                let ids = v.get("itemIds")?.as_array()?.clone();
                Some(
                    ids.iter()
                        .filter_map(|i| i.as_str().map(str::to_string))
                        .collect(),
                )
            })
            .unwrap_or_default();
        if let (true, Some(c)) = (on_canvas, canvas) {
            let r = c.get_bounding_client_rect();
            let at = [
                ev.client_x() as f64 - r.left(),
                ev.client_y() as f64 - r.top(),
            ];
            if let Some(item) = items().into_iter().find(|i| wanted.contains(&i.id)) {
                self.insert_library_item(&item, Some(at));
            }
        }
        true
    }
}
