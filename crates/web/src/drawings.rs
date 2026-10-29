//! Pictures of drawings: previews where a note embeds one
//! (`![[Board.drawing]]`, in the editor and the reading view) and the
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

thread_local! {
    /// Drawings' pictures as `data:` URLs, by the vault's file (files
    /// never change): for previews and files written out.
    static PICTURE_URLS: RefCell<HashMap<String, Rc<String>>> = RefCell::new(HashMap::new());
    /// Drawings' pictures as images for the canvas, by the vault's file
    /// (or the picture inside the drawing): `None` while loading or when
    /// it can't be.
    static IMAGES: RefCell<HashMap<String, Option<web_sys::HtmlImageElement>>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// The Drawings section is being made (once, not by two tasks at once).
    static MAKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One's Drawings section (its vault), if there is one.
pub fn section() -> Option<String> {
    step(|c| c.drawings())
}

/// The Drawings section, made the first time (`None`: it can't be made
/// now, e.g. offline).
pub async fn ensure_section() -> Option<String> {
    while MAKING.get() {
        util::sleep(50).await;
    }
    if let Some(v) = section() {
        return Some(v);
    }
    if step(|c| c.account.is_none()) {
        return None;
    }
    MAKING.set(true);
    let made = crate::actions::create_vault_of(
        notekpr_core::drawings::NAME,
        notekpr_core::protocol::VaultKind::Drawings,
    )
    .await;
    let v = match made {
        Ok(Some(v)) => Some(v),
        // Another app made it meanwhile.
        Ok(None) => {
            crate::state::refresh_vaults().await;
            section()
        }
        Err(e) => {
            util::warn(&format!("the Drawings section could not be set up: {e}"));
            None
        }
    };
    MAKING.set(false);
    v
}

/// How many drawings one has — in one's Drawings and the vaults one owns,
/// in the trash too, as the server counts them — and may have (`/me`'s
/// limit; the count seen here, which may be newer than `/me`'s).
pub fn usage() -> (u64, notekpr_core::protocol::Limit) {
    step(|c| {
        let here: u64 = c
            .replicas
            .iter()
            .filter(|(v, _)| c.is_owner(v) || c.is_drawings(v))
            .map(|(_, r)| {
                r.index
                    .entries()
                    .iter()
                    .filter(|e| {
                        e.kind == notekpr_core::doc::EntryKind::Drawing && e.purged.is_none()
                    })
                    .count() as u64
            })
            .sum();
        let (limit, counted) =
            c.me.as_ref()
                .map_or((notekpr_core::protocol::Limit::Unlimited, 0), |m| {
                    (m.drawing_limit, m.drawing_count)
                });
        (here.max(counted), limit)
    })
}

/// Whether a new drawing fits in `vault`: always in someone else's (their
/// owner's limit isn't known here; the server says); otherwise one's own
/// limit, telling why not.
pub fn has_room_in(vault: &str) -> bool {
    let own = step(|c| c.is_owner(vault) || c.is_drawings(vault));
    if !own {
        return true;
    }
    let (count, limit) = usage();
    if notekpr_core::drawings::has_room(count, limit) {
        return true;
    }
    crate::state::error(notekpr_core::drawings::full_message(limit, true));
    false
}

/// A new drawing in the Drawings section, opened.
pub fn new_in_section() {
    leptos::task::spawn_local(async move {
        match ensure_section().await {
            Some(v) => crate::actions::new_drawing(&v, ""),
            None => crate::state::error("Drawings can't be made right now: try again once online."),
        }
    });
}

/// The drawings in the section, newest first: (id, title, modified).
pub fn in_section() -> Vec<(String, String, i64)> {
    let Some(v) = section() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String, i64)> = step(|c| c.live_entries(&v))
        .into_iter()
        .filter(|e| e.kind == notekpr_core::doc::EntryKind::Drawing)
        .map(|e| (e.id.clone(), e.title().to_string(), e.modified))
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.2));
    out
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

/// The drawing with the pictures it shows inside (fetched and decrypted
/// once), for previews and files written out; those that can't be had
/// are left out.
pub async fn with_pictures(vault: &str, mut scene: Scene) -> Scene {
    let wanted: Vec<(String, String, String)> = scene
        .files
        .iter()
        .filter_map(|(id, f)| {
            let att = notekpr_core::pictures::attachment_of(f)?;
            let mime = f.get("mimeType")?.as_str().unwrap_or("image/png");
            Some((id.clone(), att.to_string(), mime.to_string()))
        })
        .collect();
    for (file_id, att, mime) in wanted {
        let cached = PICTURE_URLS.with(|p| p.borrow().get(&att).cloned());
        let url = match cached {
            Some(u) => u,
            None => match crate::actions::attachment_bytes(vault, &att).await {
                Some(b) => {
                    let u = Rc::new(notekpr_core::pictures::data_url(&mime, &b));
                    PICTURE_URLS.with(|p| p.borrow_mut().insert(att.clone(), u.clone()));
                    u
                }
                None => continue,
            },
        };
        if let Some(serde_json::Value::Object(m)) = scene.files.get_mut(&file_id) {
            m.insert("dataURL".into(), serde_json::json!(url.as_str()));
        }
    }
    scene
}

/// The image to draw for a picture a drawing shows (its `files` entry),
/// if it is loaded; otherwise it starts loading and `ready` is called
/// once it is.
pub fn picture(
    vault: &str,
    entry: &serde_json::Value,
    ready: impl FnOnce() + 'static,
) -> Option<web_sys::HtmlImageElement> {
    let att = notekpr_core::pictures::attachment_of(entry).map(str::to_string);
    let inline = entry
        .get("dataURL")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let key = match (&att, &inline) {
        (Some(a), _) => a.clone(),
        (None, Some(u)) => format!("{:x}", {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            u.hash(&mut h);
            h.finish()
        }),
        _ => return None,
    };
    if let Some(slot) = IMAGES.with(|i| i.borrow().get(&key).cloned()) {
        return slot;
    }
    IMAGES.with(|i| i.borrow_mut().insert(key.clone(), None));
    let mime = entry
        .get("mimeType")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("image/png")
        .to_string();
    let vault = vault.to_string();
    leptos::task::spawn_local(async move {
        let src = match (att, inline) {
            (Some(a), _) => crate::actions::attachment_bytes(&vault, &a)
                .await
                .and_then(|b| {
                    // Kept for copies and files written out too.
                    let url = Rc::new(notekpr_core::pictures::data_url(&mime, &b));
                    PICTURE_URLS.with(|p| p.borrow_mut().insert(a.clone(), url));
                    util::blob_url(&b, &mime)
                }),
            (None, u) => u,
        };
        let Some(src) = src else {
            return;
        };
        let Ok(img) = web_sys::HtmlImageElement::new() else {
            return;
        };
        img.set_src(&src);
        if wasm_bindgen_futures::JsFuture::from(img.decode())
            .await
            .is_ok()
        {
            IMAGES.with(|i| i.borrow_mut().insert(key, Some(img)));
            ready();
        }
    });
    None
}

/// A picture just put in a drawing (from file `att`, as `img`): drawn
/// without loading it again.
pub fn remember_picture(att: &str, img: web_sys::HtmlImageElement, data_url: String) {
    IMAGES.with(|i| i.borrow_mut().insert(att.to_string(), Some(img)));
    PICTURE_URLS.with(|p| p.borrow_mut().insert(att.to_string(), Rc::new(data_url)));
}

/// [`with_pictures`] with only those at hand now (for the clipboard,
/// which can't wait).
pub fn with_pictures_at_hand(mut scene: Scene) -> Scene {
    for f in scene.files.values_mut() {
        let Some(att) = notekpr_core::pictures::attachment_of(f).map(str::to_string) else {
            continue;
        };
        if let (Some(url), serde_json::Value::Object(m)) =
            (PICTURE_URLS.with(|p| p.borrow().get(&att).cloned()), f)
        {
            m.insert("dataURL".into(), serde_json::json!(url.as_str()));
        }
    }
    scene
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
pub async fn font_faces(scene: &Scene) -> String {
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

/// The drawing as an SVG file of its own, its fonts inside, and the drawing
/// too (as Excalidraw's "Embed scene"): it opens again as the drawing.
pub async fn standalone_svg(vault: &str, id: &str, dark: bool) -> Option<String> {
    Some(svg_file(vault, scene(vault, id)?, dark, true).await)
}

/// `scene` as an SVG file of its own: its fonts and pictures inside.
pub async fn svg_of(vault: &str, scene: Scene, dark: bool) -> String {
    svg_file(vault, scene, dark, false).await
}

async fn svg_file(vault: &str, scene: Scene, dark: bool, embed_scene: bool) -> String {
    let scene = with_pictures(vault, scene).await;
    let font_faces = font_faces(&scene).await;
    svg::export(
        &scene,
        &SvgOptions {
            dark,
            font_faces: Some(font_faces),
            time: crate::drawing::sticky::local_time(),
            measure: crate::drawing::editing::measure,
            standalone: true,
            embed_scene,
            ..SvgOptions::default()
        },
    )
}

/// A picture of `scene` (a version of a drawing, say), as a blob URL; the
/// caller revokes it.
pub async fn picture_url(vault: &str, scene: Scene) -> Option<String> {
    let dark = is_dark();
    let scene = with_pictures(vault, scene).await;
    let font_faces = font_faces(&scene).await;
    let image = svg::export(
        &scene,
        &SvgOptions {
            dark,
            font_faces: Some(font_faces),
            ..SvgOptions::default()
        },
    );
    util::blob_url(image.as_bytes(), "image/svg+xml")
}

/// Makes the drawing's preview (when it changed since the last one): its
/// URL, or `None` if the drawing isn't loaded.
pub async fn load_preview(vault: &str, id: &str) -> Option<String> {
    let dark = is_dark();
    let v = version(vault, id, dark)?;
    if let Some(url) = cached_preview(vault, id) {
        return Some(url);
    }
    let scene = with_pictures(vault, scene(vault, id)?).await;
    let font_faces = font_faces(&scene).await;
    let image = svg::export(
        &scene,
        &SvgOptions {
            dark,
            font_faces: Some(font_faces),
            time: crate::drawing::sticky::local_time(),
            measure: crate::drawing::editing::measure,
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
