//! Pictures of drawings: previews where a note embeds one
//! (`![[Board.drawing]]`, in the editor and the reading view) and the
//! drawing's own page.
//!
//! A preview is an SVG image (`notekpr_draw::svg`) behind a blob URL, made
//! again when the drawing changes. An image can't load fonts, so the font
//! files its texts need are put inside it, fetched once from the app's own
//! `fonts/` (never from elsewhere).
//!
//! Other apps' drawing files in a vault (Obsidian's `X.excalidraw.md`, a
//! note; Excalidraw's `X.excalidraw`, a file — synced as they are from a
//! Markdown folder) are shown as drawings too, view-only: read here, never
//! written, so those apps keep working with them. Making a drawing of one
//! is the person's choice (`actions::make_drawing_of`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::paths;
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
    /// Other apps' drawing files read, by (vault, file): the version read
    /// ([`shown_version`]) and its drawing (`None`: the file isn't one).
    static OTHER_SCENES: RefCell<HashMap<(String, String), (u64, Option<Rc<Scene>>)>> = RefCell::new(HashMap::new());
}

/// How many drawings `vault` holds — those in the trash too, as the server
/// counts them — and may hold (the vault list's limit).
pub fn usage(vault: &str) -> (u64, notekpr_core::protocol::Limit) {
    step(|c| {
        let count = c.replicas.get(vault).map_or(0, |r| {
            r.index
                .entries()
                .iter()
                .filter(|e| e.kind == notekpr_core::doc::EntryKind::Drawing && e.purged.is_none())
                .count() as u64
        });
        (count, c.drawing_limit(vault))
    })
}

/// Whether a new drawing fits in `vault`, telling why not.
pub fn has_room_in(vault: &str) -> bool {
    let (count, limit) = usage(vault);
    if notekpr_core::drawings::has_room(count, limit) {
        return true;
    }
    crate::state::error(notekpr_core::drawings::full_message(limit));
    false
}

/// A drawing in [`in_vault`]'s list.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub id: String,
    pub title: String,
    pub folder: String,
    pub modified: i64,
    /// Another app's drawing file: that app.
    pub app: Option<&'static str>,
}

/// The drawings in `vault`, wherever in it, newest first, other apps'
/// drawing files among them.
pub fn in_vault(vault: &str) -> Vec<Listed> {
    let mut out: Vec<Listed> = step(|c| c.live_entries(vault))
        .into_iter()
        .filter(|e| e.kind == EntryKind::Drawing || is_other_apps_entry(e))
        .map(|e| {
            let folder = e
                .path
                .rsplit_once('/')
                .map(|(f, _)| f.to_string())
                .unwrap_or_default();
            let app = other_app(&e.path);
            let title = match paths::drawing_path_for(&e.path) {
                Some(p) if app.is_some() => paths::drawing_title(&p).to_string(),
                _ => e.title().to_string(),
            };
            Listed {
                id: e.id.clone(),
                title,
                folder,
                modified: e.modified,
                app,
            }
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.modified));
    out
}

/// The app a drawing file at `path` is another app's of: Obsidian (its
/// Excalidraw plugin's `.excalidraw.md`) or Excalidraw (`.excalidraw`).
pub fn other_app(path: &str) -> Option<&'static str> {
    if !paths::is_other_apps_drawing(path) {
        return None;
    }
    Some(if path.to_ascii_lowercase().ends_with(".md") {
        "Obsidian"
    } else {
        "Excalidraw"
    })
}

/// Another app's drawing file: a note or file named as one.
pub fn is_other_apps_entry(e: &Entry) -> bool {
    matches!(e.kind, EntryKind::Note | EntryKind::Attachment)
        && paths::is_other_apps_drawing(&e.path)
}

/// Another app's drawing file of the vault, shown as a drawing as it is.
pub fn is_other_apps(vault: &str, id: &str) -> bool {
    step(|c| c.entry(vault, id)).is_some_and(|e| is_other_apps_entry(&e))
}

/// Shown as a drawing: a drawing of the vault, or another app's drawing file.
pub fn shows_as_drawing(vault: &str, id: &str) -> bool {
    step(|c| c.entry(vault, id))
        .is_some_and(|e| e.kind == EntryKind::Drawing || is_other_apps_entry(&e))
}

/// The drawing another app's file holds, read once per version of it, with
/// the pictures it shows from the vault; `None` if it isn't one, or can't
/// be had (yet: a file is fetched).
pub async fn other_apps_scene(vault: &str, id: &str) -> Option<Scene> {
    let e = step(|c| c.entry(vault, id)).filter(is_other_apps_entry)?;
    if e.kind == EntryKind::Note {
        step(|c| c.ensure_note_loaded(vault, id));
    }
    let at = shown_version(vault, id, false)?;
    let key = (vault.to_string(), id.to_string());
    if let Some((v, scene)) = OTHER_SCENES.with(|m| m.borrow().get(&key).cloned())
        && v == at
    {
        return scene.map(|s| (*s).clone());
    }
    let mut fresh = notekpr_draw::restore::Ids::new(js_sys::Date::now() as u64);
    let read = if e.kind == EntryKind::Note {
        let text = step(|c| c.replicas.get(vault).map(|r| r.note_text(id)))?;
        notekpr_draw::file::read_drawing(&text, &mut fresh)
    } else {
        let bytes = crate::actions::attachment_bytes(vault, id).await?;
        notekpr_draw::file::read_drawing_bytes(&bytes, &mut fresh)
    };
    let scene = read.ok().map(|d| {
        let mut scene = d.scene;
        link_vault_pictures(vault, &mut scene, &d.embedded);
        Rc::new(scene)
    });
    OTHER_SCENES.with(|m| m.borrow_mut().insert(key, (at, scene.clone())));
    scene.map(|s| (*s).clone())
}

/// The pictures an Obsidian drawing shows from the vault (`[[cat.png]]`
/// in its "Embedded Files"): their `files` entries show those files of the
/// vault. Links to the web and formulas are left out: nothing is fetched
/// from elsewhere.
pub fn link_vault_pictures(vault: &str, scene: &mut Scene, embedded: &[(String, String)]) {
    for (file_id, link) in embedded {
        let Some(target) = link
            .trim()
            .strip_prefix("[[")
            .and_then(|l| l.strip_suffix("]]"))
        else {
            continue;
        };
        let target = target.split('|').next().unwrap_or(target);
        let Some((att, path)) = crate::knowledge::resolve_embed(vault, target).and_then(|a| {
            step(|c| c.entry(vault, &a))
                .filter(|e| e.kind == EntryKind::Attachment)
                .map(|e| (a, e.path))
        }) else {
            continue;
        };
        let created = scene
            .files
            .get(file_id)
            .and_then(|f| f.get("created"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        scene.files.insert(
            file_id.clone(),
            notekpr_core::pictures::entry(file_id, util::mime_for(&path), &att, created),
        );
    }
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

/// Which version of an entry a picture of it shows: a drawing's or a
/// note's (and the theme); a file never changes.
fn shown_version(vault: &str, id: &str, dark: bool) -> Option<u64> {
    if step(|c| c.entry(vault, id)).is_some_and(|e| e.kind == EntryKind::Attachment) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        dark.hash(&mut h);
        return Some(h.finish());
    }
    version(vault, id, dark)
}

/// The preview's URL if it shows the drawing as it is now.
pub fn cached_preview(vault: &str, id: &str) -> Option<String> {
    let v = shown_version(vault, id, is_dark())?;
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

/// Makes the preview of a drawing, or of another app's drawing file (when
/// it changed since the last one): its URL, or `None` if it isn't loaded
/// (or the file isn't a drawing).
pub async fn load_preview(vault: &str, id: &str) -> Option<String> {
    let dark = is_dark();
    let e = step(|c| c.entry(vault, id))?;
    // (A file isn't a note: nothing to load.)
    if e.kind != EntryKind::Attachment {
        step(|c| c.ensure_note_loaded(vault, id));
    }
    let v = shown_version(vault, id, dark)?;
    if let Some(url) = cached_preview(vault, id) {
        return Some(url);
    }
    let scene = if is_other_apps_entry(&e) {
        other_apps_scene(vault, id).await?
    } else {
        scene(vault, id)?
    };
    let scene = with_pictures(vault, scene).await;
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
