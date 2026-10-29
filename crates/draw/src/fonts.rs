//! The fonts of drawings, by Excalidraw's numbers (`fontFamily`), with the
//! metrics Excalidraw lays text out with.

/// A drawing font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    /// Excalidraw's number for it.
    pub id: u32,
    /// Its CSS family name.
    pub name: &'static str,
    pub units_per_em: f64,
    pub ascender: f64,
    /// Below the baseline: negative.
    pub descender: f64,
    /// The default line height, as a multiple of the font size.
    pub line_height: f64,
    /// Typewriter-like: falls back to `monospace`, not `sans-serif`.
    pub monospace: bool,
}

pub const VIRGIL: u32 = 1;
pub const HELVETICA: u32 = 2;
pub const CASCADIA: u32 = 3;
pub const EXCALIFONT: u32 = 5;
pub const NUNITO: u32 = 6;
pub const LILITA_ONE: u32 = 7;
pub const COMIC_SHANNS: u32 = 8;
pub const LIBERATION_SANS: u32 = 9;
pub const ASSISTANT: u32 = 10;

/// What new text is written in.
pub const DEFAULT: u32 = EXCALIFONT;

const fn font(
    id: u32,
    name: &'static str,
    units_per_em: f64,
    ascender: f64,
    descender: f64,
    line_height: f64,
    monospace: bool,
) -> Font {
    Font {
        id,
        name,
        units_per_em,
        ascender,
        descender,
        line_height,
        monospace,
    }
}

/// Every font, in Excalidraw's order (`FONT_FAMILY`, `FONT_METADATA`).
pub const ALL: [Font; 9] = [
    font(VIRGIL, "Virgil", 1000.0, 886.0, -374.0, 1.25, false),
    font(HELVETICA, "Helvetica", 2048.0, 1577.0, -471.0, 1.15, false),
    font(CASCADIA, "Cascadia", 2048.0, 1900.0, -480.0, 1.2, true),
    font(EXCALIFONT, "Excalifont", 1000.0, 886.0, -374.0, 1.25, false),
    font(NUNITO, "Nunito", 1000.0, 1011.0, -353.0, 1.25, false),
    font(LILITA_ONE, "Lilita One", 1000.0, 923.0, -220.0, 1.15, false),
    font(
        COMIC_SHANNS,
        "Comic Shanns",
        1000.0,
        750.0,
        -250.0,
        1.25,
        true,
    ),
    font(
        LIBERATION_SANS,
        "Liberation Sans",
        2048.0,
        1854.0,
        -434.0,
        1.15,
        false,
    ),
    font(ASSISTANT, "Assistant", 2048.0, 1021.0, -287.0, 1.25, false),
];

/// A font by its number; unknown numbers (a custom font of Obsidian's,
/// say) get the default font.
pub fn get(id: u32) -> &'static Font {
    ALL.iter()
        .find(|f| f.id == id)
        .unwrap_or_else(|| ALL.iter().find(|f| f.id == DEFAULT).expect("default font"))
}

/// A font's number by its name, as old drawings' `font` property names it
/// (`"20px Virgil"`); the default font for other names.
pub fn by_name(name: &str) -> u32 {
    ALL.iter()
        .find(|f| f.name == name)
        .map_or(DEFAULT, |f| f.id)
}

/// The CSS `font-family` list for a font, as Excalidraw writes it
/// (`getFontFamilyString`): the font, then fallbacks for handwriting in
/// Chinese and Japanese, for anything else, and for emoji on Windows.
pub fn css_family(id: u32) -> String {
    let Some(f) = ALL.iter().find(|f| f.id == id) else {
        return "Segoe UI Emoji".into();
    };
    let generic = if f.monospace {
        "monospace"
    } else {
        "sans-serif"
    };
    if id == EXCALIFONT {
        format!("{}, Xiaolai, {generic}, Segoe UI Emoji", f.name)
    } else {
        format!("{}, {generic}, Segoe UI Emoji", f.name)
    }
}

/// The distance from a line's top to its baseline, for text of `size` in
/// lines `line_height` high (Excalidraw's `getVerticalOffset`).
pub fn baseline_offset(id: u32, size: f64, line_height_px: f64) -> f64 {
    let f = get(id);
    // In Excalidraw's order of operations: the result is printed in SVGs.
    let em = size / f.units_per_em;
    let line_gap = (line_height_px - em * f.ascender + em * f.descender) / 2.0;
    em * f.ascender + line_gap
}

/// One `@font-face` of the drawing fonts' stylesheet (the web app's
/// `fonts/drawing-fonts.css`): a file holding some characters of a font.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFace {
    pub family: String,
    /// As written in the stylesheet (relative to it).
    pub url: String,
    /// The characters it has (`unicode-range`); empty: all.
    pub ranges: Vec<(u32, u32)>,
    pub weight: Option<String>,
}

/// `U+20-7e, U+a0, U+4??` as ranges of characters.
fn unicode_ranges(s: &str) -> Vec<(u32, u32)> {
    s.split(',')
        .filter_map(|part| {
            let part = part
                .trim()
                .trim_start_matches(['U', 'u'])
                .trim_start_matches('+');
            let hex = |h: &str, fill: char| {
                u32::from_str_radix(&h.replace('?', &fill.to_string()), 16).ok()
            };
            match part.split_once('-') {
                Some((a, b)) => Some((hex(a, '0')?, hex(b, 'f')?)),
                None => Some((hex(part, '0')?, hex(part, 'f')?)),
            }
        })
        .collect()
}

/// The `@font-face` rules of a stylesheet.
pub fn parse_font_faces(css: &str) -> Vec<FontFace> {
    let mut out = Vec::new();
    for block in css.split("@font-face").skip(1) {
        let Some(body) = block
            .split_once('{')
            .and_then(|(_, b)| b.split_once('}'))
            .map(|(b, _)| b)
        else {
            continue;
        };
        let mut family = None;
        let mut url = None;
        let mut ranges = Vec::new();
        let mut weight = None;
        for decl in body.split(';') {
            let Some((k, v)) = decl.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "font-family" => family = Some(v.trim_matches(['"', '\'']).to_string()),
                "src" => {
                    url = v
                        .split_once("url(")
                        .and_then(|(_, r)| r.split_once(')'))
                        .map(|(u, _)| u.trim_matches(['"', '\'']).to_string())
                }
                "unicode-range" => ranges = unicode_ranges(v),
                "font-weight" => weight = Some(v.to_string()),
                _ => {}
            }
        }
        if let (Some(family), Some(url)) = (family, url) {
            out.push(FontFace {
                family,
                url,
                ranges,
                weight,
            });
        }
    }
    out
}

/// The font files a drawing's texts need: the faces of the fonts it uses
/// that hold characters of its texts.
pub fn faces_needed(faces: &[FontFace], scene: &crate::scene::Scene) -> Vec<FontFace> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut chars: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    for e in scene.visible() {
        if e.kind() != crate::scene::ElementType::Text {
            continue;
        }
        let Some(f) = ALL.iter().find(|f| f.id == e.font_family()) else {
            continue;
        };
        chars
            .entry(f.name)
            .or_default()
            .extend(e.text().chars().map(|c| c as u32));
    }
    faces
        .iter()
        .filter(|face| {
            chars.get(face.family.as_str()).is_some_and(|cs| {
                face.ranges.is_empty()
                    || cs
                        .iter()
                        .any(|c| face.ranges.iter().any(|(a, b)| a <= c && c <= b))
            })
        })
        .cloned()
        .collect()
}

/// A face as an `@font-face` rule with its file inside (`data:` URL), for
/// an SVG image, which can't load fonts.
pub fn embedded_face(face: &FontFace, woff2: &[u8]) -> String {
    let mut rule = format!(
        "@font-face {{ font-family: {}; src: url(data:font/woff2;base64,{}); ",
        face.family,
        crate::js::btoa(woff2)
    );
    if !face.ranges.is_empty() {
        let ranges: Vec<String> = face
            .ranges
            .iter()
            .map(|(a, b)| {
                if a == b {
                    format!("U+{a:x}")
                } else {
                    format!("U+{a:x}-{b:x}")
                }
            })
            .collect();
        rule.push_str(&format!("unicode-range: {}; ", ranges.join(", ")));
    }
    rule.push('}');
    rule
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_like_excalidraw() {
        assert_eq!(
            css_family(EXCALIFONT),
            "Excalifont, Xiaolai, sans-serif, Segoe UI Emoji"
        );
        assert_eq!(
            css_family(COMIC_SHANNS),
            "Comic Shanns, monospace, Segoe UI Emoji"
        );
        assert_eq!(css_family(VIRGIL), "Virgil, sans-serif, Segoe UI Emoji");
        assert_eq!(css_family(4), "Segoe UI Emoji");
        assert_eq!(get(4).id, EXCALIFONT);
        assert_eq!(by_name("Cascadia"), CASCADIA);
        assert_eq!(by_name("Comic Sans"), EXCALIFONT);
    }

    #[test]
    fn faces_for_a_drawing() {
        let css = include_str!("../../web/static/fonts/drawing-fonts.css");
        let faces = parse_font_faces(css);
        assert_eq!(faces.len(), 22);
        assert!(faces.iter().all(|f| f.url.ends_with(".woff2")));
        let excalifont: Vec<_> = faces.iter().filter(|f| f.family == "Excalifont").collect();
        assert_eq!(excalifont.len(), 7);
        assert!(excalifont[0].ranges.contains(&(0x20, 0x7e)));
        let nunito = faces.iter().find(|f| f.family == "Nunito").unwrap();
        assert_eq!(nunito.weight.as_deref(), Some("500"));
        // The example's texts are Latin, in Excalifont: one file.
        let scene = crate::file::read_drawing(
            include_str!("../tests/fixtures/example.excalidraw"),
            &mut crate::restore::Ids::new(1),
        )
        .unwrap()
        .scene;
        let needed = faces_needed(&faces, &scene);
        assert_eq!(needed.len(), 1, "{needed:?}");
        assert_eq!(needed[0].family, "Excalifont");
        let rule = embedded_face(&needed[0], b"abcd");
        assert!(
            rule.contains("url(data:font/woff2;base64,YWJjZA==)"),
            "{rule}"
        );
        assert!(rule.contains("unicode-range: U+20-7e"), "{rule}");
    }

    #[test]
    fn baseline_matches_excalidraws_export() {
        // example.svg: 20px Excalifont, line height 1.25 → y="17.619999999999997".
        assert_eq!(baseline_offset(EXCALIFONT, 20.0, 25.0), 17.619999999999997);
    }
}
