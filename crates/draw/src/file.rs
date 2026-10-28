//! Excalidraw's files: drawings (`.excalidraw`, and what Excalidraw puts on
//! the clipboard) and libraries (`.excalidrawlib`). Obsidian's
//! `.excalidraw.md` is in [`crate::obsidian`]; [`read_drawing`] takes any.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::obsidian;
use crate::restore::{self, Fresh};
use crate::scene::{Element, Props, Scene};

/// What notekpr writes as a file's `source` (Excalidraw writes its site's
/// address, Obsidian's plugin its release page).
pub const SOURCE: &str = "https://github.com/coolapso/notekpr";

#[derive(Debug, PartialEq)]
pub enum FormatError {
    /// Not JSON, or not a drawing (or library) in it.
    NotADrawing,
    /// An Obsidian drawing whose compressed data can't be read.
    BadCompressedData,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NotADrawing => f.write_str("not an Excalidraw drawing"),
            FormatError::BadCompressedData => {
                f.write_str("the drawing's compressed data can't be read")
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// A drawing read from a file.
#[derive(Debug, PartialEq)]
pub struct Drawing {
    pub scene: Scene,
    /// Obsidian only: pictures the drawing shows from the vault, by file
    /// id (`[[link]]`, a URL or a `$$formula$$`, as written).
    pub embedded: Vec<(String, String)>,
}

/// Reads a drawing from an `.excalidraw` file, Excalidraw's clipboard, or
/// Obsidian's `.excalidraw.md`.
pub fn read_drawing(text: &str, fresh: &mut dyn Fresh) -> Result<Drawing, FormatError> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') {
        let v: Value = serde_json::from_str(trimmed).map_err(|_| FormatError::NotADrawing)?;
        return Ok(Drawing {
            scene: scene_from_json(&v, fresh)?,
            embedded: Vec::new(),
        });
    }
    obsidian::read(text, fresh)
}

/// A drawing from the JSON of an `.excalidraw` file or the clipboard.
pub fn scene_from_json(v: &Value, fresh: &mut dyn Fresh) -> Result<Scene, FormatError> {
    let kind = v.get("type").and_then(Value::as_str);
    let elements = v.get("elements").and_then(Value::as_array);
    match (kind, elements) {
        (
            Some("excalidraw" | "excalidraw/clipboard" | "excalidraw-api/clipboard") | None,
            Some(els),
        ) => Ok(restore::restore_scene(
            els,
            v.get("appState"),
            v.get("files"),
            fresh,
        )),
        _ => Err(FormatError::NotADrawing),
    }
}

#[derive(Serialize)]
struct FileOut<'a> {
    r#type: &'a str,
    version: u32,
    source: &'a str,
    elements: Vec<&'a Props>,
    #[serde(rename = "appState")]
    app_state: &'a Props,
    files: Props,
}

/// The pictures image elements that are there show: Excalidraw saves only
/// those (`filterOutDeletedFiles`).
pub fn used_files(scene: &Scene) -> Props {
    let mut out = Map::new();
    for e in scene.visible() {
        if let Some(id) = e.file_id()
            && let Some(f) = scene.files.get(id)
        {
            out.insert(id.to_string(), f.clone());
        }
    }
    out
}

/// An `.excalidraw` file, as Excalidraw writes it (`serializeAsJSON`).
pub fn write_excalidraw(scene: &Scene) -> String {
    let out = FileOut {
        r#type: "excalidraw",
        version: 2,
        source: SOURCE,
        elements: scene.elements.iter().map(Element::props).collect(),
        app_state: &scene.app_state,
        files: used_files(scene),
    };
    serde_json::to_string_pretty(&out).expect("JSON values serialize")
}

/// What Excalidraw puts on the clipboard for copied elements: another app
/// with Excalidraw (or notekpr) pastes them as elements.
pub fn write_clipboard(elements: &[&Element], files: &Props) -> String {
    let mut used = Map::new();
    for e in elements {
        if let Some(id) = e.file_id()
            && let Some(f) = files.get(id)
        {
            used.insert(id.to_string(), f.clone());
        }
    }
    let els: Vec<&Props> = elements.iter().map(|e| e.props()).collect();
    serde_json::to_string(&json!({
        "type": "excalidraw/clipboard",
        "elements": els,
        "files": used,
    }))
    .expect("JSON values serialize")
}

/// One item of a library: elements to drop into drawings.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryItem {
    pub id: String,
    /// `published` or `unpublished`.
    pub status: String,
    /// Milliseconds since 1970.
    pub created: f64,
    pub name: Option<String>,
    pub elements: Vec<Element>,
}

/// Reads an `.excalidrawlib` file (version 1 or 2). `now`: the time new
/// items (from version 1, which has none) are made at.
pub fn read_library(
    text: &str,
    now: f64,
    fresh: &mut dyn Fresh,
) -> Result<Vec<LibraryItem>, FormatError> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| FormatError::NotADrawing)?;
    if v.get("type").and_then(Value::as_str) != Some("excalidrawlib") {
        return Err(FormatError::NotADrawing);
    }
    let mut out = Vec::new();
    if let Some(items) = v.get("libraryItems").and_then(Value::as_array) {
        for item in items {
            let Some(els) = item.get("elements").and_then(Value::as_array) else {
                continue;
            };
            let elements = restore::restore_elements(els, fresh);
            if elements.is_empty() {
                continue;
            }
            out.push(LibraryItem {
                id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| fresh.id(), str::to_string),
                status: item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unpublished")
                    .to_string(),
                created: item.get("created").and_then(Value::as_f64).unwrap_or(now),
                name: item.get("name").and_then(Value::as_str).map(str::to_string),
                elements,
            });
        }
    } else if let Some(items) = v.get("library").and_then(Value::as_array) {
        // Version 1: just lists of elements.
        for els in items.iter().filter_map(Value::as_array) {
            let elements = restore::restore_elements(els, fresh);
            if !elements.is_empty() {
                out.push(LibraryItem {
                    id: fresh.id(),
                    status: "unpublished".into(),
                    created: now,
                    name: None,
                    elements,
                });
            }
        }
    }
    Ok(out)
}

/// An `.excalidrawlib` file (version 2).
pub fn write_library(items: &[LibraryItem]) -> String {
    let items: Vec<Value> = items
        .iter()
        .map(|i| {
            let mut item = json!({
                "status": i.status,
                "elements": i.elements.iter().map(Element::props).collect::<Vec<_>>(),
                "id": i.id,
                "created": crate::scene::num(i.created),
            });
            if let Some(name) = &i.name {
                item["name"] = json!(name);
            }
            item
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "type": "excalidrawlib",
        "version": 2,
        "source": SOURCE,
        "libraryItems": items,
    }))
    .expect("JSON values serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restore::Ids;
    use crate::scene::ElementType;

    const EXAMPLE: &str = include_str!("../tests/fixtures/example.excalidraw");

    #[test]
    fn reads_excalidraws_file() {
        let d = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap();
        assert_eq!(d.scene.elements.len(), 73);
        assert_eq!(d.scene.background(), "#ffffff");
        assert_eq!(d.scene.app_state.get("gridSize"), Some(&json!(20)));
        let sticky = d
            .scene
            .elements
            .iter()
            .find(|e| e.kind() == ElementType::StickyNote);
        assert!(sticky.is_some_and(Element::is_deleted));
        assert!(d.scene.texts().any(|t| t == "SearchBase Core"));
    }

    #[test]
    fn round_trip_loses_nothing() {
        let original: Value = serde_json::from_str(EXAMPLE).unwrap();
        let d = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap();
        let again: Value = serde_json::from_str(&write_excalidraw(&d.scene)).unwrap();
        // A current Excalidraw file is restored without changes, but for
        // what Excalidraw's restore changes too (`boundElements: null`).
        let (a, b) = (
            original["elements"].as_array().unwrap(),
            again["elements"].as_array().unwrap(),
        );
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            for (k, v) in x.as_object().unwrap() {
                let same = y.get(k) == Some(v)
                    || (k == "boundElements" && v.is_null() && y[k] == json!([]));
                assert!(same, "{} {k}: {v} became {:?}", x["id"], y.get(k));
            }
            assert_eq!(x.as_object().unwrap().len(), y.as_object().unwrap().len());
        }
        assert_eq!(again["appState"], original["appState"]);
        assert_eq!(again["type"], "excalidraw");
        // And reads back the same.
        let twice = read_drawing(&write_excalidraw(&d.scene), &mut Ids::new(1)).unwrap();
        assert_eq!(twice.scene, d.scene);
    }

    #[test]
    fn clipboard_and_not_drawings() {
        let clip = r#"{"type":"excalidraw/clipboard","elements":[{"id":"a","type":"ellipse"}],"files":{}}"#;
        let d = read_drawing(clip, &mut Ids::new(1)).unwrap();
        assert_eq!(d.scene.elements[0].kind(), ElementType::Ellipse);
        assert_eq!(
            read_drawing(r#"{"type":"excalidrawlib"}"#, &mut Ids::new(1)),
            Err(FormatError::NotADrawing)
        );
        assert_eq!(
            read_drawing("{nope", &mut Ids::new(1)),
            Err(FormatError::NotADrawing)
        );
        let one = &d.scene.elements[0];
        let text = write_clipboard(&[one], &Map::new());
        assert_eq!(
            read_drawing(&text, &mut Ids::new(1))
                .unwrap()
                .scene
                .elements[0]
                .id(),
            "a"
        );
    }

    #[test]
    fn libraries_both_versions() {
        let v1 = r#"{"type":"excalidrawlib","version":1,"library":[[{"id":"a","type":"rectangle"}],[]]}"#;
        let items = read_library(v1, 5.0, &mut Ids::new(1)).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].created, 5.0);
        assert_eq!(items[0].status, "unpublished");
        let v2 = write_library(&items);
        let back = read_library(&v2, 9.0, &mut Ids::new(2)).unwrap();
        assert_eq!(back, items);
        assert!(read_library(EXAMPLE, 0.0, &mut Ids::new(1)).is_err());
    }
}
