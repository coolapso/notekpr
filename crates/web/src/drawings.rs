//! Pictures of drawings: previews where a note embeds one
//! (`![[Board.excalidraw]]`, in the editor and the reading view) and the
//! drawing's own page.
//!
//! A preview is an SVG image (`notekpr_draw::svg`) behind a blob URL, made
//! again when the drawing changes. An image can't load fonts, so the font
//! files its texts need are put inside it, fetched once from the app's own
//! `fonts/` (never from elsewhere).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use notekpr_core::yrs::updates::encoder::Encode;
use notekpr_draw::fonts::{self, FontFace};
use notekpr_draw::scene::Scene;
use notekpr_draw::svg::{self, SvgOptions};

use leptos::prelude::*;

use crate::api;
use crate::state::{step, ui};
use crate::util;

const FONTS_CSS: &str = "/fonts/drawing-fonts.css";

thread_local! {
    /// Preview blob URLs by (vault, drawing): the version they show.
    static PREVIEWS: RefCell<HashMap<(String, String), (u64, String)>> = RefCell::new(HashMap::new());
    /// The drawing fonts' faces, once read.
    static FACES: RefCell<Option<Rc<Vec<FontFace>>>> = const { RefCell::new(None) };
    /// Font files fetched, by URL.
    static FONT_FILES: RefCell<HashMap<String, Rc<Vec<u8>>>> = RefCell::new(HashMap::new());
}

/// Whether the app shows dark: chosen, or the system's when automatic.
pub fn is_dark() -> bool {
    match ui().ui_prefs.get_untracked().theme.as_str() {
        "dark" => true,
        "light" => false,
        _ => util::window()
            .match_media("(prefers-color-scheme: dark)")
            .ok()
            .flatten()
            .is_some_and(|m| m.matches()),
    }
}

/// A drawing's content, if it is loaded.
pub fn scene(vault: &str, id: &str) -> Option<Scene> {
    step(|c| {
        let d = c.replicas.get(vault)?.notes.get(id)?;
        Some(d.drawing().scene())
    })
}

/// What a preview shows: the drawing's state, and the theme.
fn version(vault: &str, id: &str, dark: bool) -> Option<u64> {
    step(|c| {
        let d = c.replicas.get(vault)?.notes.get(id)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.state_vector().encode_v1().hash(&mut h);
        dark.hash(&mut h);
        Some(h.finish())
    })
}

/// The preview's URL if it shows the drawing as it is now.
pub fn cached_preview(vault: &str, id: &str) -> Option<String> {
    let v = version(vault, id, is_dark())?;
    PREVIEWS.with(|p| {
        p.borrow()
            .get(&(vault.to_string(), id.to_string()))
            .filter(|(at, _)| *at == v)
            .map(|(_, url)| url.clone())
    })
}

/// Fetches one of the app's own files.
async fn app_file(path: &str) -> Option<Vec<u8>> {
    api::app_file(path).await.ok()
}

async fn faces() -> Rc<Vec<FontFace>> {
    if let Some(f) = FACES.with(|f| f.borrow().clone()) {
        return f;
    }
    let css = app_file(FONTS_CSS)
        .await
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default();
    let faces = Rc::new(fonts::parse_font_faces(&css));
    // A failed fetch isn't kept: the next preview tries again.
    if !faces.is_empty() {
        FACES.with(|f| *f.borrow_mut() = Some(faces.clone()));
    }
    faces
}

/// The `@font-face` rules a drawing needs, its fonts inside.
async fn font_faces(scene: &Scene) -> String {
    let faces = faces().await;
    let mut rules = Vec::new();
    for face in fonts::faces_needed(&faces, scene) {
        let url = format!("/fonts/{}", face.url);
        let cached = FONT_FILES.with(|f| f.borrow().get(&url).cloned());
        let data = match cached {
            Some(d) => d,
            None => match app_file(&url).await {
                Some(d) => {
                    let d = Rc::new(d);
                    FONT_FILES.with(|f| f.borrow_mut().insert(url.clone(), d.clone()));
                    d
                }
                None => continue,
            },
        };
        rules.push(fonts::embedded_face(&face, &data));
    }
    rules.join("\n      ")
}

/// Makes the drawing's preview (when it changed since the last one): its
/// URL, or `None` if the drawing isn't loaded.
pub async fn load_preview(vault: &str, id: &str) -> Option<String> {
    let dark = is_dark();
    let v = version(vault, id, dark)?;
    if let Some(url) = cached_preview(vault, id) {
        return Some(url);
    }
    let scene = scene(vault, id)?;
    let font_faces = font_faces(&scene).await;
    let image = svg::export(
        &scene,
        &SvgOptions {
            dark,
            font_faces: Some(font_faces),
            ..SvgOptions::default()
        },
    );
    let url = util::blob_url(image.as_bytes(), "image/svg+xml")?;
    let old = PREVIEWS.with(|p| {
        p.borrow_mut()
            .insert((vault.to_string(), id.to_string()), (v, url.clone()))
    });
    if let Some((_, old)) = old {
        let _ = web_sys::Url::revoke_object_url(&old);
    }
    Some(url)
}
