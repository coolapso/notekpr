//! Colours as drawings have them (`#1e1e1e`, `transparent`, `rgba(…)`, CSS
//! names), Excalidraw's palette, and its dark mode: each colour inverted and
//! turned around the colour wheel, the way Excalidraw draws and exports a
//! drawing on a dark background.
//!
//! Parsing follows tinycolor 1.6 (MIT), which Excalidraw uses; the dark mode
//! follows Excalidraw's `applyDarkModeFilter` (MIT, see `PORTED.md`).

use crate::js;

/// A colour: red, green and blue 0–255, alpha 0–1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// What a colour that can't be read counts as (tinycolor: black).
const INVALID: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// The CSS colour names, as tinycolor knows them.
const NAMES: &[(&str, &str)] = &[
    ("aliceblue", "f0f8ff"),
    ("antiquewhite", "faebd7"),
    ("aqua", "0ff"),
    ("aquamarine", "7fffd4"),
    ("azure", "f0ffff"),
    ("beige", "f5f5dc"),
    ("bisque", "ffe4c4"),
    ("black", "000"),
    ("blanchedalmond", "ffebcd"),
    ("blue", "00f"),
    ("blueviolet", "8a2be2"),
    ("brown", "a52a2a"),
    ("burlywood", "deb887"),
    ("burntsienna", "ea7e5d"),
    ("cadetblue", "5f9ea0"),
    ("chartreuse", "7fff00"),
    ("chocolate", "d2691e"),
    ("coral", "ff7f50"),
    ("cornflowerblue", "6495ed"),
    ("cornsilk", "fff8dc"),
    ("crimson", "dc143c"),
    ("cyan", "0ff"),
    ("darkblue", "00008b"),
    ("darkcyan", "008b8b"),
    ("darkgoldenrod", "b8860b"),
    ("darkgray", "a9a9a9"),
    ("darkgreen", "006400"),
    ("darkgrey", "a9a9a9"),
    ("darkkhaki", "bdb76b"),
    ("darkmagenta", "8b008b"),
    ("darkolivegreen", "556b2f"),
    ("darkorange", "ff8c00"),
    ("darkorchid", "9932cc"),
    ("darkred", "8b0000"),
    ("darksalmon", "e9967a"),
    ("darkseagreen", "8fbc8f"),
    ("darkslateblue", "483d8b"),
    ("darkslategray", "2f4f4f"),
    ("darkslategrey", "2f4f4f"),
    ("darkturquoise", "00ced1"),
    ("darkviolet", "9400d3"),
    ("deeppink", "ff1493"),
    ("deepskyblue", "00bfff"),
    ("dimgray", "696969"),
    ("dimgrey", "696969"),
    ("dodgerblue", "1e90ff"),
    ("firebrick", "b22222"),
    ("floralwhite", "fffaf0"),
    ("forestgreen", "228b22"),
    ("fuchsia", "f0f"),
    ("gainsboro", "dcdcdc"),
    ("ghostwhite", "f8f8ff"),
    ("gold", "ffd700"),
    ("goldenrod", "daa520"),
    ("gray", "808080"),
    ("green", "008000"),
    ("greenyellow", "adff2f"),
    ("grey", "808080"),
    ("honeydew", "f0fff0"),
    ("hotpink", "ff69b4"),
    ("indianred", "cd5c5c"),
    ("indigo", "4b0082"),
    ("ivory", "fffff0"),
    ("khaki", "f0e68c"),
    ("lavender", "e6e6fa"),
    ("lavenderblush", "fff0f5"),
    ("lawngreen", "7cfc00"),
    ("lemonchiffon", "fffacd"),
    ("lightblue", "add8e6"),
    ("lightcoral", "f08080"),
    ("lightcyan", "e0ffff"),
    ("lightgoldenrodyellow", "fafad2"),
    ("lightgray", "d3d3d3"),
    ("lightgreen", "90ee90"),
    ("lightgrey", "d3d3d3"),
    ("lightpink", "ffb6c1"),
    ("lightsalmon", "ffa07a"),
    ("lightseagreen", "20b2aa"),
    ("lightskyblue", "87cefa"),
    ("lightslategray", "789"),
    ("lightslategrey", "789"),
    ("lightsteelblue", "b0c4de"),
    ("lightyellow", "ffffe0"),
    ("lime", "0f0"),
    ("limegreen", "32cd32"),
    ("linen", "faf0e6"),
    ("magenta", "f0f"),
    ("maroon", "800000"),
    ("mediumaquamarine", "66cdaa"),
    ("mediumblue", "0000cd"),
    ("mediumorchid", "ba55d3"),
    ("mediumpurple", "9370db"),
    ("mediumseagreen", "3cb371"),
    ("mediumslateblue", "7b68ee"),
    ("mediumspringgreen", "00fa9a"),
    ("mediumturquoise", "48d1cc"),
    ("mediumvioletred", "c71585"),
    ("midnightblue", "191970"),
    ("mintcream", "f5fffa"),
    ("mistyrose", "ffe4e1"),
    ("moccasin", "ffe4b5"),
    ("navajowhite", "ffdead"),
    ("navy", "000080"),
    ("oldlace", "fdf5e6"),
    ("olive", "808000"),
    ("olivedrab", "6b8e23"),
    ("orange", "ffa500"),
    ("orangered", "ff4500"),
    ("orchid", "da70d6"),
    ("palegoldenrod", "eee8aa"),
    ("palegreen", "98fb98"),
    ("paleturquoise", "afeeee"),
    ("palevioletred", "db7093"),
    ("papayawhip", "ffefd5"),
    ("peachpuff", "ffdab9"),
    ("peru", "cd853f"),
    ("pink", "ffc0cb"),
    ("plum", "dda0dd"),
    ("powderblue", "b0e0e6"),
    ("purple", "800080"),
    ("rebeccapurple", "663399"),
    ("red", "f00"),
    ("rosybrown", "bc8f8f"),
    ("royalblue", "4169e1"),
    ("saddlebrown", "8b4513"),
    ("salmon", "fa8072"),
    ("sandybrown", "f4a460"),
    ("seagreen", "2e8b57"),
    ("seashell", "fff5ee"),
    ("sienna", "a0522d"),
    ("silver", "c0c0c0"),
    ("skyblue", "87ceeb"),
    ("slateblue", "6a5acd"),
    ("slategray", "708090"),
    ("slategrey", "708090"),
    ("snow", "fffafa"),
    ("springgreen", "00ff7f"),
    ("steelblue", "4682b4"),
    ("tan", "d2b48c"),
    ("teal", "008080"),
    ("thistle", "d8bfd8"),
    ("tomato", "ff6347"),
    ("turquoise", "40e0d0"),
    ("violet", "ee82ee"),
    ("wheat", "f5deb3"),
    ("white", "fff"),
    ("whitesmoke", "f5f5f5"),
    ("yellow", "ff0"),
    ("yellowgreen", "9acd32"),
];

fn hex_digits(h: &str) -> Option<Rgba> {
    let v = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    let d = |i: usize| h.get(i..i + 1);
    let twice = |i: usize| d(i).and_then(|c| v(&c.repeat(2)));
    match h.len() {
        3 => Some(Rgba {
            r: twice(0)?,
            g: twice(1)?,
            b: twice(2)?,
            a: 1.0,
        }),
        4 => Some(Rgba {
            r: twice(0)?,
            g: twice(1)?,
            b: twice(2)?,
            a: twice(3)? / 255.0,
        }),
        6 => Some(Rgba {
            r: v(&h[0..2])?,
            g: v(&h[2..4])?,
            b: v(&h[4..6])?,
            a: 1.0,
        }),
        8 => Some(Rgba {
            r: v(&h[0..2])?,
            g: v(&h[2..4])?,
            b: v(&h[4..6])?,
            a: v(&h[6..8])? / 255.0,
        }),
        _ => None,
    }
}

/// A number of a colour function: `12`, `12.5`, `50%` (of `full`).
fn component(s: &str, full: f64) -> Option<f64> {
    let s = s.trim();
    match s.strip_suffix('%') {
        Some(p) => p.trim().parse::<f64>().ok().map(|p| p / 100.0 * full),
        None => s.parse::<f64>().ok(),
    }
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

/// Reads a colour; `None` if it isn't one.
pub fn parse(color: &str) -> Option<Rgba> {
    let c = color.trim().to_lowercase();
    if c == "transparent" {
        return Some(Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        });
    }
    if let Some((_, hex)) = NAMES.iter().find(|(n, _)| *n == c) {
        return hex_digits(hex);
    }
    if let Some(open) = c.find('(')
        && c.ends_with(')')
    {
        let name = c[..open].trim();
        let args: Vec<&str> = c[open + 1..c.len() - 1]
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .collect();
        let alpha = |i: usize| match args.get(i) {
            Some(a) => component(a, 1.0),
            None => Some(1.0),
        };
        return match name {
            "rgb" | "rgba" if args.len() >= 3 => Some(Rgba {
                r: component(args[0], 255.0)?.clamp(0.0, 255.0),
                g: component(args[1], 255.0)?.clamp(0.0, 255.0),
                b: component(args[2], 255.0)?.clamp(0.0, 255.0),
                a: alpha(3)?.clamp(0.0, 1.0),
            }),
            "hsl" | "hsla" if args.len() >= 3 => {
                let h =
                    (args[0].trim_end_matches("deg").parse::<f64>().ok()? / 360.0).rem_euclid(1.0);
                let s = component(args[1], 1.0)?.clamp(0.0, 1.0);
                let l = component(args[2], 1.0)?.clamp(0.0, 1.0);
                let (r, g, b) = if s == 0.0 {
                    (l, l, l)
                } else {
                    let q = if l < 0.5 {
                        l * (1.0 + s)
                    } else {
                        l + s - l * s
                    };
                    let p = 2.0 * l - q;
                    (
                        hue_to_rgb(p, q, h + 1.0 / 3.0),
                        hue_to_rgb(p, q, h),
                        hue_to_rgb(p, q, h - 1.0 / 3.0),
                    )
                };
                Some(Rgba {
                    r: r * 255.0,
                    g: g * 255.0,
                    b: b * 255.0,
                    a: alpha(3)?.clamp(0.0, 1.0),
                })
            }
            _ => None,
        };
    }
    // Hex, with or without `#` (tinycolor takes both).
    hex_digits(c.strip_prefix('#').unwrap_or(&c))
}

/// Whether a colour shows nothing (`transparent`, alpha 0).
pub fn is_transparent(color: &str) -> bool {
    parse(color).is_some_and(|c| c.a == 0.0)
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque (Excalidraw's `rgbToHex`).
pub fn to_hex(r: f64, g: f64, b: f64, a: f64) -> String {
    let hex6 = format!("#{:02x}{:02x}{:02x}", r as u8, g as u8, b as u8);
    if a < 1.0 {
        format!("{hex6}{:02x}", js::round(a * 255.0) as u8)
    } else {
        hex6
    }
}

/// How much Excalidraw's dark mode inverts colours, and turns them.
const DARK_INVERT: f64 = 0.93;
const DARK_HUE_ROTATE_DEG: f64 = 180.0;

/// A colour as Excalidraw shows it in dark mode (`applyDarkModeFilter`).
pub fn dark(color: &str) -> String {
    let c = parse(color).unwrap_or(INVALID);
    // tinycolor's `toRgb` rounds.
    let (r, g, b) = (js::round(c.r), js::round(c.g), js::round(c.b));
    // CSS `invert(93%)`…
    let invert =
        |v: f64| js::round((v * (1.0 - DARK_INVERT) + (255.0 - v) * DARK_INVERT).clamp(0.0, 255.0));
    let (r, g, b) = (invert(r), invert(g), invert(b));
    // …then `hue-rotate(180deg)`.
    let a = DARK_HUE_ROTATE_DEG * std::f64::consts::PI / 180.0;
    let (cos, sin) = (a.cos(), a.sin());
    let m = [
        0.213 + cos * 0.787 - sin * 0.213,
        0.715 - cos * 0.715 - sin * 0.715,
        0.072 - cos * 0.072 + sin * 0.928,
        0.213 - cos * 0.213 + sin * 0.143,
        0.715 + cos * 0.285 + sin * 0.14,
        0.072 - cos * 0.072 - sin * 0.283,
        0.213 - cos * 0.213 - sin * 0.787,
        0.715 - cos * 0.715 + sin * 0.715,
        0.072 + cos * 0.928 + sin * 0.072,
    ];
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let out = |x: f64| js::round(x.clamp(0.0, 1.0) * 255.0);
    to_hex(
        out(r * m[0] + g * m[1] + b * m[2]),
        out(r * m[3] + g * m[4] + b * m[5]),
        out(r * m[6] + g * m[7] + b * m[8]),
        c.a,
    )
}

/// A colour as drawn: as it is, or in dark mode.
pub fn themed(color: &str, dark_mode: bool) -> String {
    if dark_mode {
        dark(color)
    } else {
        color.to_string()
    }
}

/// Excalidraw's palette: open-color's shades 50, 200, 400, 600 and 800.
pub mod palette {
    pub const TRANSPARENT: &str = "transparent";
    pub const BLACK: &str = "#1e1e1e";
    pub const WHITE: &str = "#ffffff";
    pub type Shades = [&'static str; 5];
    pub const GRAY: Shades = ["#f8f9fa", "#e9ecef", "#ced4da", "#868e96", "#343a40"];
    pub const RED: Shades = ["#fff5f5", "#ffc9c9", "#ff8787", "#fa5252", "#e03131"];
    pub const PINK: Shades = ["#fff0f6", "#fcc2d7", "#f783ac", "#e64980", "#c2255c"];
    pub const GRAPE: Shades = ["#f8f0fc", "#eebefa", "#da77f2", "#be4bdb", "#9c36b5"];
    pub const VIOLET: Shades = ["#f3f0ff", "#d0bfff", "#9775fa", "#7950f2", "#6741d9"];
    pub const BLUE: Shades = ["#e7f5ff", "#a5d8ff", "#4dabf7", "#228be6", "#1971c2"];
    pub const CYAN: Shades = ["#e3fafc", "#99e9f2", "#3bc9db", "#15aabf", "#0c8599"];
    pub const TEAL: Shades = ["#e6fcf5", "#96f2d7", "#38d9a9", "#12b886", "#099268"];
    pub const GREEN: Shades = ["#ebfbee", "#b2f2bb", "#69db7c", "#40c057", "#2f9e44"];
    pub const YELLOW: Shades = ["#fff9db", "#ffec99", "#ffd43b", "#fab005", "#f08c00"];
    pub const ORANGE: Shades = ["#fff4e6", "#ffd8a8", "#ffa94d", "#fd7e14", "#e8590c"];
    pub const BRONZE: Shades = ["#f8f1ee", "#eaddd7", "#d2bab0", "#a18072", "#846358"];

    /// The quick picks for strokes and for backgrounds, in their order.
    pub const STROKE_PICKS: [&str; 5] = [BLACK, RED[4], GREEN[4], BLUE[4], YELLOW[4]];
    pub const BACKGROUND_PICKS: [&str; 5] = [TRANSPARENT, RED[1], GREEN[1], BLUE[1], YELLOW[1]];
    /// A sticky note's colour, and the picks for them.
    pub const STICKY_NOTE: &str = "#ffdf6b";
    pub const STICKY_NOTE_PICKS: [&str; 5] = [STICKY_NOTE, PINK[1], GREEN[1], BLUE[1], ORANGE[1]];
    pub const CANVAS_PICKS: [&str; 5] = [WHITE, "#f8f9fa", "#f5faff", "#fffce8", "#fdf8f6"];
    /// The shaded colours of the full palette, in its rows' order.
    pub const SHADED: [(&str, Shades); 10] = [
        ("cyan", CYAN),
        ("blue", BLUE),
        ("violet", VIOLET),
        ("grape", GRAPE),
        ("pink", PINK),
        ("green", GREEN),
        ("teal", TEAL),
        ("yellow", YELLOW),
        ("orange", ORANGE),
        ("red", RED),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_drawings_have() {
        assert_eq!(
            parse("#1e1e1e"),
            Some(Rgba {
                r: 30.0,
                g: 30.0,
                b: 30.0,
                a: 1.0
            })
        );
        assert_eq!(parse("1e1e1e").map(|c| c.r), Some(30.0));
        assert_eq!(parse("#fff").map(|c| c.g), Some(255.0));
        assert_eq!(parse("#ff000080").map(|c| c.a), Some(128.0 / 255.0));
        assert_eq!(parse("Transparent").map(|c| c.a), Some(0.0));
        assert_eq!(
            parse("red"),
            Some(Rgba {
                r: 255.0,
                g: 0.0,
                b: 0.0,
                a: 1.0
            })
        );
        assert_eq!(
            parse("rgba(10, 20, 30, 0.5)"),
            Some(Rgba {
                r: 10.0,
                g: 20.0,
                b: 30.0,
                a: 0.5
            })
        );
        assert_eq!(parse("rgb(100%, 0%, 50%)").map(|c| c.b), Some(127.5));
        assert_eq!(
            parse("hsl(0, 100%, 50%)").map(|c| (c.r, c.g)),
            Some((255.0, 0.0))
        );
        assert_eq!(parse("not a colour"), None);
        assert!(is_transparent("transparent"));
        assert!(is_transparent("#00000000"));
        assert!(!is_transparent("#1e1e1e"));
    }

    #[test]
    fn dark_mode_like_excalidraws_export() {
        // example.svg (exported in dark mode): the default stroke and the
        // white canvas.
        assert_eq!(dark("#1e1e1e"), "#d3d3d3");
        assert_eq!(dark("#ffffff"), "#121212");
        assert_eq!(dark("transparent"), "#ededed00");
        // A colour keeps its alpha.
        assert_eq!(dark("#1971c280").len(), 9);
    }

    #[test]
    fn hex_like_excalidraw() {
        assert_eq!(to_hex(0.0, 0.0, 0.0, 1.0), "#000000");
        assert_eq!(to_hex(255.0, 128.0, 1.0, 0.5), "#ff800180");
    }
}
