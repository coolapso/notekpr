//! Obsidian's Excalidraw drawings (`.excalidraw.md`): Markdown with the
//! drawing's texts as block references, the pictures it shows from the
//! vault, and the drawing itself as JSON, usually lz-string compressed.
//! Read and written as the Obsidian Excalidraw plugin does
//! (`ExcalidrawData.ts`, `excalidrawMarkdownParsing.ts`; AGPL-3.0, like
//! notekpr).

use serde::Serialize;
use serde_json::Value;

use crate::file::{Drawing, FormatError, SOURCE, scene_from_json, used_files};
use crate::lzstring;
use crate::restore::Fresh;
use crate::scene::{Element, ElementType, Props, Scene};

/// The plugin's front matter and notice above a drawing (`FRONTMATTER`).
const FRONT_MATTER: &str = "---\n\nexcalidraw-plugin: parsed\ntags: [excalidraw]\n\n---\n==⚠  Switch to EXCALIDRAW VIEW in the MORE OPTIONS menu of this document. ⚠== You can decompress Drawing data with the command palette: 'Decompress current Excalidraw file'. For more info check in plugin settings under 'Saving'\n\n\n";

/// Whether a file name is one of Obsidian's drawings.
pub fn is_obsidian_name(name: &str) -> bool {
    name.to_lowercase().ends_with(".excalidraw.md")
}

/// The `## Drawing` (or older `# Drawing`) heading's end.
fn drawing_heading(md: &str) -> Option<usize> {
    ["\n## Drawing\n", "\n# Drawing\n"]
        .iter()
        .filter_map(|h| md.find(h).map(|i| i + h.len()))
        .min()
}

/// The drawing's JSON: from its code block (compressed or not), or, in the
/// oldest files, the line after the heading.
fn drawing_json(md: &str) -> Result<String, FormatError> {
    let md = md.replace("\r\n", "\n");
    let at = drawing_heading(&md).ok_or(FormatError::NotADrawing)?;
    let rest = &md[at..];
    // Anything but backticks may come between the heading and the block.
    let fence = rest.find('`').map(|i| &rest[i..]);
    let block = |fence: &str, opening: &str| -> Option<String> {
        let body = fence.strip_prefix(opening)?;
        let end = body.find("```").unwrap_or(body.len());
        Some(body[..end].to_string())
    };
    let json = if let Some(packed) = fence.and_then(|f| block(f, "```compressed-json\n")) {
        lzstring::decompress_from_base64(&packed).ok_or(FormatError::BadCompressedData)?
    } else if let Some(plain) = fence.and_then(|f| block(f, "```json\n")) {
        plain
    } else {
        rest.lines().next().unwrap_or_default().to_string()
    };
    // As the plugin does: up to the last `}` (a sync tool may have glued two
    // versions of a file together).
    let end = json.rfind('}').ok_or(FormatError::NotADrawing)?;
    Ok(json[..=end].to_string())
}

/// The `## Embedded Files` section: `id: [[link]]` (or a URL, or a
/// `$$formula$$`) per line.
fn embedded_files(md: &str) -> Vec<(String, String)> {
    let Some(start) = md.find("## Embedded Files\n") else {
        return Vec::new();
    };
    md[start..]
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with('#') && !l.starts_with("%%"))
        .filter_map(|l| {
            let (id, link) = l.split_once(": ")?;
            let ok = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            ok.then(|| (id.to_string(), link.trim().to_string()))
        })
        .collect()
}

/// Reads an `.excalidraw.md` drawing.
pub fn read(md: &str, fresh: &mut dyn Fresh) -> Result<Drawing, FormatError> {
    let json = drawing_json(md)?;
    let v: Value = serde_json::from_str(&json).map_err(|_| FormatError::NotADrawing)?;
    Ok(Drawing {
        scene: scene_from_json(&v, fresh)?,
        embedded: embedded_files(md),
    })
}

/// Whether the plugin can use `id` as a text's block reference: it reads
/// exactly eight characters after `^`, and Obsidian allows only letters,
/// digits and dashes.
fn is_block_id(id: &str) -> bool {
    id.len() == 8 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Replaces every string equal to `from` in `v` by `to` (an element's id
/// wherever it is referred to).
fn rename(v: &mut Value, from: &str, to: &str) {
    match v {
        Value::String(s) if s == from => *s = to.to_string(),
        Value::Array(a) => a.iter_mut().for_each(|x| rename(x, from, to)),
        Value::Object(o) => o.values_mut().for_each(|x| rename(x, from, to)),
        _ => {}
    }
}

#[derive(Serialize)]
struct SceneOut<'a> {
    r#type: &'a str,
    version: u32,
    source: &'a str,
    elements: Vec<Value>,
    #[serde(rename = "appState")]
    app_state: &'a Props,
    files: Props,
}

/// An `.excalidraw.md` drawing as the plugin writes one. Texts whose ids
/// the plugin can't refer to get new eight-character ones (as the plugin
/// does when it converts an `.excalidraw` file); `compress` as the plugin's
/// setting of that name.
pub fn write(scene: &Scene, compress: bool, fresh: &mut dyn Fresh) -> String {
    let mut elements: Vec<Value> = scene
        .elements
        .iter()
        .map(|e| Value::Object(e.props().clone()))
        .collect();
    let texts: Vec<String> = scene
        .elements
        .iter()
        .filter(|e| e.kind() == ElementType::Text)
        .map(|e| e.id().to_string())
        .collect();
    let mut ids = Vec::new();
    for old in texts {
        let new = if is_block_id(&old) {
            old
        } else {
            let new: String = fresh
                .id()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .chain("abcdefgh".chars())
                .take(8)
                .collect();
            for e in &mut elements {
                rename(e, &old, &new);
            }
            new
        };
        ids.push(new);
    }
    let mut out = String::from(FRONT_MATTER);
    out.push_str("# Excalidraw Data\n\n## Text Elements\n");
    for e in &elements {
        let Some(el) = e.as_object().cloned().and_then(Element::from_props) else {
            continue;
        };
        if el.kind() == ElementType::Text && !el.is_deleted() && ids.iter().any(|i| i == el.id()) {
            out.push_str(&format!("{} ^{}\n\n", el.original_text(), el.id()));
        }
    }
    let data = SceneOut {
        r#type: "excalidraw",
        version: 2,
        source: SOURCE,
        elements,
        app_state: &scene.app_state,
        files: used_files(scene),
    };
    // The plugin indents its JSON with tabs.
    let mut json = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut json,
        serde_json::ser::PrettyFormatter::with_indent(b"\t"),
    );
    data.serialize(&mut ser).expect("JSON values serialize");
    let json = String::from_utf8(json).expect("JSON is UTF-8");
    out.push_str("%%\n");
    if compress {
        // In lines of 256 with a blank line between (`compress`).
        let packed = lzstring::compress_to_base64(&json);
        let chunks: Vec<&str> = packed
            .as_bytes()
            .chunks(256)
            .map(|c| std::str::from_utf8(c).expect("Base64 is ASCII"))
            .collect();
        out.push_str(&format!(
            "## Drawing\n```compressed-json\n{}\n```\n%%",
            chunks.join("\n\n")
        ));
    } else {
        out.push_str(&format!("## Drawing\n```json\n{json}\n```\n%%"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::read_drawing;
    use crate::restore::Ids;

    const STAND_IN: &str = include_str!("../tests/fixtures/obsidian.excalidraw.md");
    const EXAMPLE: &str = include_str!("../tests/fixtures/example.excalidraw");

    #[test]
    fn pictures_shown_from_the_vault_are_named() {
        let json = r#"{"type":"excalidraw","version":2,"files":{},"elements":[
            {"id":"pic","type":"image","x":0,"y":0,"width":40,"height":40,"fileId":"cat1","status":"saved"}]}"#;
        let md = format!(
            "---\nexcalidraw-plugin: parsed\n---\n# Excalidraw Data\n## Text Elements\n\
             ## Embedded Files\ncat1: [[cat.png]]\n\n%%\n## Drawing\n```json\n{json}\n```\n%%\n"
        );
        let d = read_drawing(&md, &mut Ids::new(1)).unwrap();
        assert_eq!(d.embedded, vec![("cat1".into(), "[[cat.png]]".into())]);
        assert_eq!(d.scene.elements.len(), 1);
        assert_eq!(d.scene.elements[0].file_id(), Some("cat1"));
    }

    #[test]
    fn reads_the_plugins_files() {
        let d = read_drawing(STAND_IN, &mut Ids::new(1)).unwrap();
        assert_eq!(d.scene.elements.len(), 24);
        assert!(d.scene.texts().any(|t| t == "Video Stream"));
        // Obsidian's own copy of texts is dropped, as Excalidraw does.
        assert!(d.scene.elements.iter().all(|e| e.get("rawText").is_none()));
        assert_eq!(d.scene.background(), "#ffffff");
    }

    #[test]
    fn writes_what_it_reads() {
        let scene = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap().scene;
        for compress in [true, false] {
            let md = write(&scene, compress, &mut Ids::new(3));
            assert!(md.starts_with("---\n\nexcalidraw-plugin: parsed\n"));
            assert!(md.ends_with("```\n%%"));
            let back = read(&md, &mut Ids::new(1)).unwrap().scene;
            assert_eq!(back.elements.len(), scene.elements.len());
            // Text ids were made into block ids, and what referred to them follows.
            for t in back
                .elements
                .iter()
                .filter(|e| e.kind() == ElementType::Text)
            {
                assert!(is_block_id(t.id()), "{}", t.id());
                assert!(md.contains(&format!(" ^{}\n", t.id())) || t.is_deleted());
                // (Deleted labels may point anywhere: Excalidraw keeps them as they were.)
                if let Some(c) = t.container_id().filter(|_| !t.is_deleted()) {
                    let container = back.element(c).unwrap();
                    assert!(container.bound_elements().iter().any(|b| b.id == t.id()));
                }
            }
            let texts: Vec<&str> = back.texts().collect();
            assert_eq!(texts, scene.texts().collect::<Vec<_>>());
        }
    }

    #[test]
    fn older_layouts_and_embedded_files() {
        let json = r#"{"type":"excalidraw","version":2,"elements":[{"id":"a","type":"ellipse"}],"appState":{}}"#;
        let old = format!("# Text Elements\n%%\n# Drawing\n```json\n{json}\n```\n%%");
        assert_eq!(
            read(&old, &mut Ids::new(1)).unwrap().scene.elements.len(),
            1
        );
        let oldest = format!("%%\n# Drawing\n{json}%%");
        assert_eq!(
            read(&oldest, &mut Ids::new(1))
                .unwrap()
                .scene
                .elements
                .len(),
            1
        );
        let with_files = format!(
            "## Embedded Files\nabc123: [[Pictures/cat.png]]\n\nf-2: https://x.y/z.png\n\n%%\n## Drawing\n```json\n{json}\n```\n%%"
        );
        let d = read(&with_files, &mut Ids::new(1)).unwrap();
        assert_eq!(
            d.embedded,
            vec![
                ("abc123".into(), "[[Pictures/cat.png]]".into()),
                ("f-2".into(), "https://x.y/z.png".into())
            ]
        );
        assert_eq!(
            read("just a note", &mut Ids::new(1)),
            Err(FormatError::NotADrawing)
        );
        let broken = "%%\n## Drawing\n```compressed-json\n!!!!\n```\n%%";
        assert!(read(broken, &mut Ids::new(1)).is_err());
        assert!(is_obsidian_name("Board.Excalidraw.md"));
        assert!(!is_obsidian_name("Board.excalidraw"));
    }
}
