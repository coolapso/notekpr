//! Small browser helpers: timers, time formatting, downloads, clipboard.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

pub fn window() -> web_sys::Window {
    web_sys::window().expect("no window")
}

pub fn document() -> web_sys::Document {
    window().document().expect("no document")
}

pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Run `f` once after `ms` milliseconds.
pub fn set_timeout(ms: i32, f: impl FnOnce() + 'static) -> i32 {
    let cb = Closure::once_into_js(f);
    window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms)
        .unwrap_or(0)
}

pub fn clear_timeout(id: i32) {
    window().clear_timeout_with_handle(id);
}

/// Run `f` every `ms` milliseconds for the lifetime of the page.
pub fn set_interval(ms: i32, f: impl FnMut() + 'static) {
    let cb = Closure::wrap(Box::new(f) as Box<dyn FnMut()>);
    let _ = window()
        .set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), ms);
    cb.forget();
}

pub async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

/// A trailing-edge debouncer for UI work.
#[derive(Clone, Default)]
pub struct Debounce(Rc<RefCell<Option<i32>>>);

impl Debounce {
    pub fn run(&self, ms: i32, f: impl FnOnce() + 'static) {
        if let Some(id) = self.0.borrow_mut().take() {
            clear_timeout(id);
        }
        let slot = self.0.clone();
        let id = set_timeout(ms, move || {
            slot.borrow_mut().take();
            f();
        });
        *self.0.borrow_mut() = Some(id);
    }
}

pub fn relative_time(ms: i64) -> String {
    let diff = (now_ms() - ms).max(0) / 1000;
    match diff {
        0..=44 => "just now".into(),
        45..=89 => "a minute ago".into(),
        90..=3599 => format!("{} minutes ago", diff / 60),
        3600..=7199 => "an hour ago".into(),
        7200..=86_399 => format!("{} hours ago", diff / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => absolute_time(ms),
    }
}

pub fn absolute_time(ms: i64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    let s: String = d.to_locale_string("default", &JsValue::UNDEFINED).into();
    s
}

/// "1 note", "3 notes".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Offer bytes to the user as a file download.
pub fn download(name: &str, mime: &str, data: &[u8]) {
    let arr = js_sys::Uint8Array::from(data);
    let parts = js_sys::Array::new();
    parts.push(&arr.buffer());
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let Ok(blob) = web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let a: web_sys::HtmlAnchorElement = document().create_element("a").unwrap().unchecked_into();
    a.set_href(&url);
    a.set_download(name);
    a.click();
    set_timeout(1000, move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
}

pub fn blob_url(data: &[u8], mime: &str) -> Option<String> {
    let arr = js_sys::Uint8Array::from(data);
    let parts = js_sys::Array::new();
    parts.push(&arr.buffer());
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts).ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok()
}

pub fn copy_to_clipboard(text: &str) {
    let nav = window().navigator();
    let _ = nav.clipboard().write_text(text);
}

pub async fn read_file(file: &web_sys::File) -> Option<Vec<u8>> {
    let buf = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Guess a MIME type from a file name (only used for display and blob URLs).
pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "json" => "application/json",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

pub fn is_image(name: &str) -> bool {
    mime_for(name).starts_with("image/") && !name.to_ascii_lowercase().ends_with(".svg")
}

/// Whether an attachment may be opened in a browser tab: raster images and
/// PDFs only. A `blob:` URL belongs to this app's origin, so anything that
/// can carry scripts (SVG, HTML, …) would run as the app; those are
/// downloaded instead.
pub fn opens_inline(name: &str) -> bool {
    is_image(name) || mime_for(name) == "application/pdf"
}

/// Stable, pleasant colour for a user id (presence cursors, avatars).
pub fn user_color(id: &str) -> String {
    const COLORS: [&str; 10] = [
        "#e5484d", "#f76b15", "#ffc53d", "#46a758", "#12a594", "#0090ff", "#3e63dd", "#8e4ec6",
        "#d6409f", "#978365",
    ];
    let h = id
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[(h as usize) % COLORS.len()].to_string()
}

pub fn initials(name: &str) -> String {
    let mut parts = name.split_whitespace();
    let a = parts.next().and_then(|p| p.chars().next());
    let b = parts.next().and_then(|p| p.chars().next());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().collect(),
        _ => "?".into(),
    }
}

pub fn is_mac() -> bool {
    window()
        .navigator()
        .platform()
        .map(|p| p.to_lowercase().contains("mac"))
        .unwrap_or(false)
}

pub fn mod_key() -> &'static str {
    if is_mac() { "⌘" } else { "Ctrl" }
}

/// Touch-first devices (don't pop up the keyboard unasked).
pub fn is_touch() -> bool {
    window()
        .match_media("(pointer: coarse)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

pub fn confirm(msg: &str) -> bool {
    window().confirm_with_message(msg).unwrap_or(false)
}

pub fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}

pub fn warn(msg: &str) {
    web_sys::console::warn_1(&JsValue::from_str(msg));
}

/// An image file, decoded (for [`encode_square`]); `None` if the browser
/// can't read it as an image.
pub async fn load_image(file: &web_sys::Blob) -> Option<web_sys::HtmlImageElement> {
    let url = web_sys::Url::create_object_url_with_blob(file).ok()?;
    let img = web_sys::HtmlImageElement::new().ok()?;
    img.set_src(&url);
    let decoded = wasm_bindgen_futures::JsFuture::from(img.decode())
        .await
        .is_ok();
    let _ = web_sys::Url::revoke_object_url(&url);
    (decoded && img.natural_width() > 0 && img.natural_height() > 0).then_some(img)
}

/// The middle square of `img`, `side` pixels wide, encoded as `mime`
/// (`quality` 0–1 for lossy types; `opaque` puts it on white, for types
/// without transparency): `(type, bytes)`. Browsers that can't encode
/// `mime` use PNG, so check the type.
pub fn encode_square(
    img: &web_sys::HtmlImageElement,
    side: u32,
    mime: &str,
    quality: f64,
    opaque: bool,
) -> Option<(String, Vec<u8>)> {
    let (w, h) = (img.natural_width() as f64, img.natural_height() as f64);
    let s = w.min(h);
    let canvas: web_sys::HtmlCanvasElement =
        document().create_element("canvas").ok()?.dyn_into().ok()?;
    canvas.set_width(side);
    canvas.set_height(side);
    let ctx: web_sys::CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    let side = side as f64;
    if opaque {
        ctx.set_fill_style_str("#ffffff");
        ctx.fill_rect(0.0, 0.0, side, side);
    }
    ctx.draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
        img,
        (w - s) / 2.0,
        (h - s) / 2.0,
        s,
        s,
        0.0,
        0.0,
        side,
        side,
    )
    .ok()?;
    let url = canvas
        .to_data_url_with_type_and_encoder_options(mime, &JsValue::from_f64(quality))
        .ok()?;
    let (ty, data) = url.strip_prefix("data:")?.split_once(";base64,")?;
    Some((ty.to_string(), coolpsonotes_core::b64::decode(data).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_raster_images_and_pdfs_open_inline() {
        for ok in [
            "a.png",
            "b.JPG",
            "c.jpeg",
            "d.gif",
            "e.webp",
            "f.avif",
            "g.pdf",
            "dir/h.PDF",
        ] {
            assert!(opens_inline(ok), "{ok}");
        }
        for bad in [
            "a.svg", "b.SVG", "c.html", "d.htm", "e.xhtml", "f.xml", "g.md", "h", "i.js", "j.mp4",
        ] {
            assert!(!opens_inline(bad), "{bad}");
        }
    }
}
