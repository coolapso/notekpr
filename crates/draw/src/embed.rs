//! A drawing inside a PNG or SVG picture of it, as Excalidraw's exports
//! with "Embed scene" hold it: the picture opens again as the drawing, in
//! Excalidraw and in notekpr.
//!
//! A PNG holds it in a `tEXt` chunk (`data/image.ts`), an SVG in comments
//! in its `<metadata>` (`scene/export.ts`); both as Excalidraw's `encode`
//! wraps text (`data/encode.ts`): deflated, as a byte string, in JSON.
//! Ported from Excalidraw (MIT, see `PORTED.md`); deflate is miniz_oxide's,
//! so the compressed bytes differ from pako's but read the same.

use serde::Serialize;
use serde_json::Value;

use crate::file::{self, FormatError};
use crate::js;
use crate::scene::Scene;

/// What Excalidraw calls its drawings (`MIME_TYPES.excalidraw`): the
/// keyword of the PNG chunk, the payload type of the SVG.
pub const MIME: &str = "application/vnd.excalidraw+json";

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// pako's default compression level.
const LEVEL: u8 = 6;

/// The most a compressed drawing may inflate to: far more than drawings
/// hold, so that a crafted picture can't take all the memory there is.
const MAX_INFLATED: usize = 256 << 20;

/// A byte string (each character a byte, as JavaScript keeps binary data in
/// strings) from bytes.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// A byte string's bytes (`byteStringToArrayBuffer`: each UTF-16 code
/// unit, cut to 8 bits).
fn bytes_of(s: &str) -> Vec<u8> {
    s.encode_utf16().map(|u| u as u8).collect()
}

#[derive(Serialize)]
struct Encoded<'a> {
    version: &'a str,
    encoding: &'a str,
    compressed: bool,
    encoded: String,
}

/// `JSON.stringify(encode({ text }))`: `text` deflated, as a byte string,
/// in Excalidraw's wrapper; the wrapper's bytes (all its characters are
/// bytes).
pub fn encode(text: &str) -> Vec<u8> {
    let deflated = miniz_oxide::deflate::compress_to_vec_zlib(text.as_bytes(), LEVEL);
    let wrapper = Encoded {
        version: "1",
        encoding: "bstring",
        compressed: true,
        encoded: latin1(&deflated),
    };
    bytes_of(&serde_json::to_string(&wrapper).expect("strings serialize"))
}

/// `decode(JSON.parse(json))`: the text in Excalidraw's wrapper. An older
/// picture holds the drawing's JSON itself, which comes back as it is.
pub fn decode(json: &str) -> Result<String, FormatError> {
    let bad = FormatError::BadCompressedData;
    let v: Value = serde_json::from_str(json).map_err(|_| bad)?;
    let wrapper = v.as_object().ok_or(bad)?;
    if !wrapper.contains_key("encoded") {
        return match wrapper.get("type").and_then(Value::as_str) {
            Some("excalidraw") => Ok(json.to_string()),
            _ => Err(bad),
        };
    }
    if wrapper.get("encoding").and_then(Value::as_str) != Some("bstring") {
        return Err(bad);
    }
    let bytes = bytes_of(wrapper.get("encoded").and_then(Value::as_str).ok_or(bad)?);
    let text = if crate::restore::truthy(wrapper.get("compressed")) {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&bytes, MAX_INFLATED)
            .map_err(|_| bad)?
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// The drawing to put in a picture of it: an `.excalidraw` file of the
/// elements there (`serializeAsJSON` of what is exported), so Excalidraw
/// opens it too.
pub fn scene_text(scene: &Scene) -> String {
    file::write_excalidraw(&Scene {
        elements: scene.visible().cloned().collect(),
        app_state: scene.app_state.clone(),
        files: scene.files.clone(),
    })
}

// ------------------------------------------------------------------- PNG

/// Whether `bytes` are a PNG file.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_SIGNATURE)
}

/// One chunk of a PNG: where it is in the file, its type and its data.
struct Chunk<'a> {
    at: std::ops::Range<usize>,
    kind: &'a [u8],
    data: &'a [u8],
    crc: u32,
}

/// A PNG's chunks up to `IEND` (`png-chunks-extract`); `None` if it isn't
/// one, or is cut short.
fn chunks(png: &[u8]) -> Option<Vec<Chunk<'_>>> {
    if !is_png(png) {
        return None;
    }
    let mut out = Vec::new();
    let mut i = PNG_SIGNATURE.len();
    loop {
        let len = u32::from_be_bytes(png.get(i..i + 4)?.try_into().ok()?) as usize;
        let end = i.checked_add(12)?.checked_add(len)?;
        let chunk = png.get(i..end)?;
        out.push(Chunk {
            at: i..end,
            kind: &chunk[4..8],
            data: &chunk[8..8 + len],
            crc: u32::from_be_bytes(chunk[8 + len..].try_into().ok()?),
        });
        i = end;
        if &chunk[4..8] == b"IEND" {
            return Some(out);
        }
    }
}

/// The CRC-32 a PNG chunk ends with, of its type and data.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for part in parts {
        for &b in *part {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
    }
    !c
}

/// `decodePngMetadata`: the drawing's JSON in a PNG. Not a drawing if it
/// holds none (`tEXt` keyed by [`MIME`]; Excalidraw reads only the first
/// `tEXt` chunk, this any).
pub fn png_scene(png: &[u8]) -> Result<String, FormatError> {
    let chunks = chunks(png).ok_or(FormatError::NotADrawing)?;
    let chunk = chunks
        .iter()
        .filter(|c| c.kind == b"tEXt")
        .find(|c| c.data.starts_with(MIME.as_bytes()) && c.data.get(MIME.len()) == Some(&0))
        .ok_or(FormatError::NotADrawing)?;
    let text = &chunk.data[MIME.len() + 1..];
    if chunk.crc != crc32(&[chunk.kind, chunk.data]) || text.contains(&0) {
        return Err(FormatError::BadCompressedData);
    }
    decode(&latin1(text))
}

/// `encodePngMetadata`: the PNG with `text` (a drawing's JSON, see
/// [`scene_text`]) inside, in a `tEXt` chunk before its last (`IEND`);
/// `None` if `png` isn't a PNG.
pub fn png_with(png: &[u8], text: &str) -> Option<Vec<u8>> {
    let chunks = chunks(png)?;
    let last = chunks.last()?.at.clone();
    let mut data = MIME.as_bytes().to_vec();
    data.push(0);
    data.extend(encode(text));
    let mut out = Vec::with_capacity(png.len() + data.len() + 12);
    out.extend_from_slice(&png[..last.start]);
    out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
    out.extend_from_slice(b"tEXt");
    out.extend_from_slice(&data);
    out.extend_from_slice(&crc32(&[b"tEXt", &data]).to_be_bytes());
    out.extend_from_slice(&png[last]);
    Some(out)
}

// ------------------------------------------------------------------- SVG

const PAYLOAD_START: &str = "<!-- payload-start -->";
const PAYLOAD_END: &str = "<!-- payload-end -->";
const PAYLOAD_VERSION: &str = "<!-- payload-version:";

/// Whether an SVG says it holds a drawing.
fn svg_has_scene(svg: &str) -> bool {
    svg.contains(&format!("payload-type:{MIME}"))
}

/// `encodeSvgBase64Payload`: the comments holding `text` (a drawing's
/// JSON, see [`scene_text`]) that go in an SVG's `<metadata>`.
pub fn svg_payload(text: &str) -> String {
    format!(
        "<!-- payload-type:{MIME} --><!-- payload-version:2 -->{PAYLOAD_START}{}{PAYLOAD_END}",
        js::btoa(&encode(text))
    )
}

/// The payload's version (`/<!-- payload-version:(\d+) -->/`), "1" if
/// none says.
fn payload_version(svg: &str) -> &str {
    svg.match_indices(PAYLOAD_VERSION)
        .find_map(|(i, _)| {
            let rest = &svg[i + PAYLOAD_VERSION.len()..];
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            (digits > 0 && rest[digits..].starts_with(" -->")).then(|| &rest[..digits])
        })
        .unwrap_or("1")
}

/// `decodeSvgBase64Payload`: the drawing's JSON in an SVG; not a drawing
/// if it holds none.
pub fn svg_scene(svg: &str) -> Result<String, FormatError> {
    if !svg_has_scene(svg) {
        return Err(FormatError::NotADrawing);
    }
    // `/<!-- payload-start -->\s*(.+?)\s*<!-- payload-end -->/`
    let start = svg.find(PAYLOAD_START).ok_or(FormatError::NotADrawing)? + PAYLOAD_START.len();
    let len = svg[start..]
        .find(PAYLOAD_END)
        .ok_or(FormatError::NotADrawing)?;
    let payload = svg[start..start + len].trim_matches(js::is_space);
    if payload.is_empty() || payload.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return Err(FormatError::NotADrawing);
    }
    let bytes = js::atob(payload).ok_or(FormatError::BadCompressedData)?;
    // Version 1 held UTF-8, later ones a byte string.
    let json = if payload_version(svg) == "1" {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        latin1(&bytes)
    };
    decode(&json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::read_drawing;
    use crate::restore::Ids;
    use crate::scene::ElementType;

    const EXAMPLE: &str = include_str!("../tests/fixtures/example.excalidraw");
    /// Exported by Excalidraw with the scene embedded (from its own test
    /// fixtures: MIT, see `PORTED.md`): version 2, and an older one.
    const SMILEY_PNG: &[u8] = include_bytes!("../tests/fixtures/smiley_embedded_v2.png");
    const SMILEY_SVG: &str = include_str!("../tests/fixtures/smiley_embedded_v2.svg");
    const OLD_PNG: &[u8] = include_bytes!("../tests/fixtures/test_embedded_v1.png");
    const OLD_SVG: &str = include_str!("../tests/fixtures/test_embedded_v1.svg");

    /// A 1×1 PNG, as a canvas makes one.
    fn tiny_png() -> Vec<u8> {
        let mut png = PNG_SIGNATURE.to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        };
        chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        let pixel = miniz_oxide::deflate::compress_to_vec_zlib(&[0, 255, 0, 0, 255], 6);
        chunk(b"IDAT", &pixel);
        chunk(b"IEND", &[]);
        png
    }

    #[test]
    fn reads_excalidraws_pictures() {
        // As Excalidraw's own tests expect: one text in each.
        for (json, text) in [
            (png_scene(SMILEY_PNG), "😀"),
            (svg_scene(SMILEY_SVG), "😀"),
            (png_scene(OLD_PNG), "test"),
            (svg_scene(OLD_SVG), "test"),
        ] {
            let d = read_drawing(&json.unwrap(), &mut Ids::new(1)).unwrap();
            assert_eq!(d.scene.elements.len(), 1);
            assert_eq!(d.scene.elements[0].kind(), ElementType::Text);
            assert_eq!(d.scene.elements[0].text(), text);
        }
    }

    #[test]
    fn the_crc_is_pngs() {
        // The CRC of IEND, in every PNG.
        assert_eq!(crc32(&[b"IEND", b""]), 0xAE42_6082);
        assert!(SMILEY_PNG.ends_with(&[0xAE, 0x42, 0x60, 0x82]));
    }

    #[test]
    fn writes_what_excalidraw_reads() {
        let wrapper = latin1(&encode("{\"type\":\"excalidraw\"}"));
        assert!(
            wrapper.starts_with(
                r#"{"version":"1","encoding":"bstring","compressed":true,"encoded":"x"#
            )
        );
        let scene = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap().scene;
        let text = scene_text(&scene);
        // Into a PNG and back.
        let png = png_with(&tiny_png(), &text).unwrap();
        let back = chunks(&png).unwrap();
        let kinds: Vec<&[u8]> = back.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [&b"IHDR"[..], b"IDAT", b"tEXt", b"IEND"]);
        assert_eq!(png_scene(&png).unwrap(), text);
        // Into an SVG and back.
        let svg = format!(
            "<svg><!-- svg-source:excalidraw --><metadata>{}</metadata></svg>",
            svg_payload(&text)
        );
        assert_eq!(svg_scene(&svg).unwrap(), text);
        // The drawing there, without what is deleted.
        let d = read_drawing(&text, &mut Ids::new(1)).unwrap();
        assert_eq!(d.scene.elements.len(), scene.visible().count());
        assert_eq!(d.scene.app_state, scene.app_state);
    }

    #[test]
    fn pictures_without_a_drawing() {
        assert_eq!(png_scene(&tiny_png()), Err(FormatError::NotADrawing));
        assert_eq!(png_scene(b"GIF89a"), Err(FormatError::NotADrawing));
        // Cut short.
        let png = tiny_png();
        assert_eq!(png_scene(&png[..20]), Err(FormatError::NotADrawing));
        assert_eq!(
            svg_scene("<svg><metadata></metadata></svg>"),
            Err(FormatError::NotADrawing)
        );
        // Broken: a wrong CRC, a payload that isn't base64 or deflate.
        let mut broken = png_with(&png, "{}").unwrap();
        let at = broken.len() - 13;
        broken[at] ^= 1;
        assert_eq!(png_scene(&broken), Err(FormatError::BadCompressedData));
        let bad = |payload: &str| {
            svg_scene(&format!(
                "<svg><!-- payload-type:{MIME} --><!-- payload-version:2 -->{PAYLOAD_START}{payload}{PAYLOAD_END}</svg>"
            ))
        };
        assert_eq!(bad("!!!!"), Err(FormatError::BadCompressedData));
        let not_deflated = js::btoa(br#"{"encoding":"bstring","compressed":true,"encoded":"abc"}"#);
        assert_eq!(bad(&not_deflated), Err(FormatError::BadCompressedData));
        // Uncompressed: UTF-8 in the byte string.
        let plain = js::btoa(&bytes_of(
            r#"{"encoding":"bstring","compressed":false,"encoded":"Ã©"}"#,
        ));
        assert_eq!(bad(&plain).as_deref(), Ok("é"));
    }

    #[test]
    fn base64_as_browsers_read_it() {
        assert_eq!(js::btoa(b"hi!"), "aGkh");
        assert_eq!(js::btoa(b"h"), "aA==");
        assert_eq!(js::atob("aGkh").as_deref(), Some(&b"hi!"[..]));
        assert_eq!(js::atob(" aA= =\n").as_deref(), Some(&b"h"[..]));
        assert_eq!(js::atob("aA").as_deref(), Some(&b"h"[..]));
        assert_eq!(js::atob("a"), None);
        assert_eq!(js::atob("a$b="), None);
    }
}
