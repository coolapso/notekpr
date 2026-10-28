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
    fn baseline_matches_excalidraws_export() {
        // example.svg: 20px Excalifont, line height 1.25 → y="17.619999999999997".
        assert_eq!(baseline_offset(EXCALIFONT, 20.0, 25.0), 17.619999999999997);
    }
}
