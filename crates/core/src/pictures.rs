//! Pictures in drawings. In a vault they are the vault's files
//! (attachments): a drawing's `files` entry names the file it shows
//! (`attachment`, the file's entry id) instead of holding the picture as
//! Excalidraw does (`dataURL`) — so a picture is stored and synced once,
//! encrypted like any file, and doesn't weigh on the drawing's history.
//! Drawing files written out (exports, the Markdown folder, `cat`) hold
//! the pictures inside, like Excalidraw's, and pictures inside a drawing
//! file that comes in become files of the vault.

use serde_json::{Value, json};

use notekpr_draw::scene::{Props, Scene};

/// The key of a `files` entry naming the vault's file it shows.
pub const ATTACHMENT: &str = "attachment";

/// A picture as a `data:` URL.
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", crate::b64::encode(bytes))
}

/// The type and bytes of a `data:` URL (base64 ones, as Excalidraw writes).
pub fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let mime = head.strip_suffix(";base64")?;
    let bytes = crate::b64::decode(data.trim()).ok()?;
    Some((mime.to_string(), bytes))
}

/// A file name ending for a picture's type.
pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
        _ => "png",
    }
}

/// The vault's file a `files` entry shows, if it names one.
pub fn attachment_of(entry: &Value) -> Option<&str> {
    entry.get(ATTACHMENT)?.as_str()
}

/// The vault's files the drawing shows: (file id, attachment id).
pub fn attachments(scene: &Scene) -> Vec<(String, String)> {
    scene
        .files
        .iter()
        .filter_map(|(id, f)| Some((id.clone(), attachment_of(f)?.to_string())))
        .collect()
}

/// The pictures held inside the drawing (a `dataURL`, no file of the
/// vault): file id, type and bytes.
pub fn inline(scene: &Scene) -> Vec<(String, String, Vec<u8>)> {
    scene
        .files
        .iter()
        .filter(|(_, f)| attachment_of(f).is_none())
        .filter_map(|(id, f)| {
            let (mime, bytes) = decode_data_url(f.get("dataURL")?.as_str()?)?;
            Some((id.clone(), mime, bytes))
        })
        .collect()
}

/// A `files` entry showing the vault's file `attachment`.
pub fn entry(file_id: &str, mime: &str, attachment: &str, created: f64) -> Value {
    json!({
        "mimeType": mime,
        "id": file_id,
        "created": notekpr_draw::scene::num(created),
        ATTACHMENT: attachment,
    })
}

/// The `files` entry `file_id` shows `attachment` instead of holding the
/// picture.
pub fn link(scene: &mut Scene, file_id: &str, attachment: &str) {
    if let Some(Value::Object(f)) = scene.files.get_mut(file_id) {
        f.remove("dataURL");
        f.insert(ATTACHMENT.into(), json!(attachment));
    }
}

/// The drawing with its pictures inside, for a file written out: each
/// entry naming a file of the vault gets its `dataURL` from `bytes`
/// (`None`: not at hand, left as it is).
pub fn embed(scene: &Scene, bytes: impl Fn(&str) -> Option<Vec<u8>>) -> Scene {
    let mut out = scene.clone();
    for (_, f) in out.files.iter_mut() {
        let Some(Value::Object(m)) = Some(f) else {
            continue;
        };
        let Some(att) = m.get(ATTACHMENT).and_then(Value::as_str) else {
            continue;
        };
        let mime = m
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string();
        if let Some(b) = bytes(att) {
            m.insert("dataURL".into(), json!(data_url(&mime, &b)));
        }
    }
    out
}

/// The drawing's references to the vault's files changed with `map` (a
/// copy in another vault shows the copies of its pictures); entries `map`
/// has nothing for are left alone.
pub fn relink(scene: &mut Scene, map: impl Fn(&str) -> Option<String>) {
    for (_, f) in scene.files.iter_mut() {
        if let Value::Object(m) = f
            && let Some(new) = m.get(ATTACHMENT).and_then(Value::as_str).and_then(&map)
        {
            m.insert(ATTACHMENT.into(), json!(new));
        }
    }
}

/// The `files` of `scene` without what only a file needs (`dataURL` when
/// the entry names a file of the vault): what a drawing keeps.
pub fn kept(files: &Props) -> Props {
    files
        .iter()
        .map(|(id, f)| {
            let mut f = f.clone();
            if let Value::Object(m) = &mut f
                && m.contains_key(ATTACHMENT)
            {
                m.remove("dataURL");
            }
            (id.clone(), f)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(files: Value) -> Scene {
        Scene {
            files: files.as_object().unwrap().clone(),
            ..Scene::default()
        }
    }

    #[test]
    fn data_urls_both_ways() {
        let u = data_url("image/png", b"\x89PNG");
        assert_eq!(u, "data:image/png;base64,iVBORw==");
        assert_eq!(
            decode_data_url(&u),
            Some(("image/png".to_string(), b"\x89PNG".to_vec()))
        );
        assert_eq!(decode_data_url("data:image/svg+xml,<svg/>"), None);
        assert_eq!(decode_data_url("https://x"), None);
    }

    #[test]
    fn pictures_inside_become_links_and_back() {
        let mut s = scene(json!({
            "f1": {"id": "f1", "mimeType": "image/png", "dataURL": data_url("image/png", b"one"), "created": 1},
            "f2": {"id": "f2", "mimeType": "image/jpeg", "attachment": "att2", "created": 2},
        }));
        assert_eq!(
            inline(&s),
            [("f1".to_string(), "image/png".to_string(), b"one".to_vec())]
        );
        link(&mut s, "f1", "att1");
        assert!(inline(&s).is_empty());
        assert!(s.files["f1"].get("dataURL").is_none());
        let mut a = attachments(&s);
        a.sort();
        assert_eq!(
            a,
            [
                ("f1".to_string(), "att1".to_string()),
                ("f2".to_string(), "att2".to_string())
            ]
        );
        // Written out, the pictures are inside again (those at hand).
        let out = embed(&s, |att| (att == "att1").then(|| b"one".to_vec()));
        assert_eq!(out.files["f1"]["dataURL"], data_url("image/png", b"one"));
        assert!(out.files["f2"].get("dataURL").is_none());
        assert_eq!(kept(&out.files), s.files);
        // A copy elsewhere shows the copies.
        relink(&mut s, |old| (old == "att2").then(|| "new2".to_string()));
        assert_eq!(s.files["f2"]["attachment"], "new2");
        assert_eq!(s.files["f1"]["attachment"], "att1");
    }
}
