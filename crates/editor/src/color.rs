//! Text and background colors in notes: the palette, how a color is written
//! into a note, and how colors other editors wrote are read.
//!
//! A color is HTML in the note, so that it syncs like any text, stays in the
//! Markdown files of the desktop client and shows in other Markdown editors
//! (Obsidian, Typora, VS Code, Joplin, …). The toolbar writes
//!
//! * `<span style="color: #e44045">…</span>` for text, and
//! * `<span style="background-color: #e5484d4d">…</span>` for a background,
//!
//! with values that read on light and dark pages alike (text in mid tones,
//! backgrounds see-through), since other editors show them as they are.
//! What other editors write is read too — `<font color="…">`,
//! `<mark style="background: …">`, any CSS color (`red`, `#f00`, `rgb(…)`,
//! `hsl(…)`) — and shown as the palette color nearest to it ([`nearest`]).
//!
//! A note's CSS never reaches the page: an element's colors become classes
//! of the app's own ([`Colors::classes`], `fg-red`, `bg-blue`), whose values
//! for the light and the dark theme are in the stylesheet.

/// A color of the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Gray,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
}

/// Which color of the text: its own or its background's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Text,
    Background,
}

impl Color {
    pub const ALL: [Color; 8] = [
        Color::Gray,
        Color::Red,
        Color::Orange,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Purple,
        Color::Pink,
    ];

    /// Its name, as in its classes (`fg-red`).
    pub fn name(self) -> &'static str {
        match self {
            Color::Gray => "gray",
            Color::Red => "red",
            Color::Orange => "orange",
            Color::Yellow => "yellow",
            Color::Green => "green",
            Color::Blue => "blue",
            Color::Purple => "purple",
            Color::Pink => "pink",
        }
    }

    /// The CSS color written into notes for it.
    pub fn css(self, kind: Kind) -> &'static str {
        match kind {
            Kind::Text => match self {
                Color::Gray => "#7c7c7c",
                Color::Red => "#e44045",
                Color::Orange => "#c46114",
                Color::Yellow => "#9c7705",
                Color::Green => "#298e5d",
                Color::Blue => "#3a7bdf",
                Color::Purple => "#9664d6",
                Color::Pink => "#d6419f",
            },
            Kind::Background => match self {
                Color::Gray => "#8c8c8c4d",
                Color::Red => "#e5484d4d",
                Color::Orange => "#f7a2404d",
                Color::Yellow => "#ffd0004d",
                Color::Green => "#30a46c4d",
                Color::Blue => "#3e7ee04d",
                Color::Purple => "#9a6bd84d",
                Color::Pink => "#d6409f4d",
            },
        }
    }

    /// Its number in style flags, 1–8 (0 is no color).
    pub fn code(self) -> u8 {
        self as u8 + 1
    }

    pub fn from_code(n: u8) -> Option<Color> {
        Color::ALL.get(usize::from(n).checked_sub(1)?).copied()
    }

    fn named(name: &str) -> Option<Color> {
        Color::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// The colors an element sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Colors {
    pub text: Option<Color>,
    pub background: Option<Color>,
}

impl Colors {
    pub fn get(self, kind: Kind) -> Option<Color> {
        match kind {
            Kind::Text => self.text,
            Kind::Background => self.background,
        }
    }

    pub fn with(mut self, kind: Kind, color: Option<Color>) -> Colors {
        match kind {
            Kind::Text => self.text = color,
            Kind::Background => self.background = color,
        }
        self
    }

    pub fn is_empty(self) -> bool {
        self.text.is_none() && self.background.is_none()
    }

    /// The app's classes that show them: `fg-red bg-blue` (empty for none).
    pub fn classes(self) -> String {
        let fg = self.text.map(|c| format!("fg-{}", c.name()));
        let bg = self.background.map(|c| format!("bg-{}", c.name()));
        [fg, bg].into_iter().flatten().collect::<Vec<_>>().join(" ")
    }

    /// The opening tag written into notes for them, closed by `</span>`.
    pub fn span(self) -> String {
        let fg = self.text.map(|c| format!("color: {}", c.css(Kind::Text)));
        let bg = self
            .background
            .map(|c| format!("background-color: {}", c.css(Kind::Background)));
        let style = [fg, bg]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        format!("<span style=\"{style}\">")
    }
}

/// Whether the colors of an element `name` are shown: `span`, `font`
/// (older editors) and `mark` (colored highlights).
pub fn colorable(name: &str) -> bool {
    matches!(name, "span" | "font" | "mark")
}

/// The colors element `name` with attributes `attrs` (lowercase names,
/// decoded values) sets: the app's classes (`fg-red`), a `<font>`'s `color`
/// and `style` (`color`, `background-color`, `background`), which wins, as in
/// a browser. Elements that aren't [`colorable`] set none.
pub fn of_element<'a>(name: &str, attrs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Colors {
    let mut c = Colors::default();
    if !colorable(name) {
        return c;
    }
    let (mut class, mut font, mut style) = (None, None, None);
    for (k, v) in attrs {
        match k {
            "class" => class = Some(v),
            "color" if name == "font" => font = Some(v),
            "style" => style = Some(v),
            _ => {}
        }
    }
    for w in class.unwrap_or("").split_ascii_whitespace() {
        if let Some(x) = w.strip_prefix("fg-").and_then(Color::named) {
            c.text = Some(x);
        } else if let Some(x) = w.strip_prefix("bg-").and_then(Color::named) {
            c.background = Some(x);
        }
    }
    if let Some(v) = font {
        // A legacy attribute: browsers take hex digits without `#` too.
        let v = v.trim();
        let hex = (matches!(v.len(), 3 | 6) && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| format!("#{v}"));
        if let Some(x) = value(hex.as_deref().unwrap_or(v), Kind::Text) {
            c.text = x;
        }
    }
    // Later declarations override earlier ones; invalid ones are ignored.
    for decl in style.unwrap_or("").split(';') {
        let Some((prop, v)) = decl.split_once(':') else {
            continue;
        };
        match prop.trim().to_ascii_lowercase().as_str() {
            "color" => {
                if let Some(x) = value(v, Kind::Text) {
                    c.text = x;
                }
            }
            "background-color" => {
                if let Some(x) = value(v, Kind::Background) {
                    c.background = x;
                }
            }
            "background" => c.background = shorthand(v),
            _ => {}
        }
    }
    c
}

/// A CSS color value as the palette color shown for it: `Some(None)` for a
/// valid value without one (`black` text, `transparent`, `inherit`), `None`
/// for something that isn't a color.
fn value(v: &str, kind: Kind) -> Option<Option<Color>> {
    let v = v.trim().to_ascii_lowercase();
    let v = v
        .strip_suffix("important")
        .and_then(|r| r.trim_end().strip_suffix('!'))
        .unwrap_or(&v)
        .trim();
    match v {
        "transparent" | "currentcolor" | "inherit" | "initial" | "unset" | "revert"
        | "revert-layer" => Some(None),
        _ => rgba(v).map(|c| nearest(c, kind)),
    }
}

/// The background color the `background` shorthand sets: the color among
/// its parts, if there is one (none resets it).
fn shorthand(v: &str) -> Option<Color> {
    let v = v.to_ascii_lowercase();
    let mut depth = 0usize;
    let mut parts = vec![];
    let mut start = 0;
    for (i, ch) in v.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if depth == 0 && (c.is_whitespace() || c == ',' || c == '/') => {
                parts.push(&v[start..i]);
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&v[start..]);
    parts
        .into_iter()
        .find_map(|p| value(p, Kind::Background))
        .flatten()
}

/// Red, green and blue (0–255) and alpha (0–1) of a CSS color in lowercase:
/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`/`rgba()`,
/// `hsl()`/`hsla()` (with commas or spaces and `/`) or a named color.
fn rgba(v: &str) -> Option<[f32; 4]> {
    if let Some(h) = v.strip_prefix('#') {
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let digit = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(f32::from);
        let pair = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(f32::from);
        return match h.len() {
            3 | 4 => {
                let a = if h.len() == 4 {
                    digit(3)? * 17.0
                } else {
                    255.0
                };
                Some([
                    digit(0)? * 17.0,
                    digit(1)? * 17.0,
                    digit(2)? * 17.0,
                    a / 255.0,
                ])
            }
            6 | 8 => {
                let a = if h.len() == 8 { pair(6)? } else { 255.0 };
                Some([pair(0)?, pair(2)?, pair(4)?, a / 255.0])
            }
            _ => None,
        };
    }
    if let Some((f, rest)) = v.split_once('(') {
        let args: Vec<&str> = rest
            .strip_suffix(')')?
            .split(|c: char| c.is_whitespace() || c == ',' || c == '/')
            .filter(|s| !s.is_empty())
            .collect();
        if !matches!(args.len(), 3 | 4) {
            return None;
        }
        let alpha = match args.get(3) {
            Some(a) => number(a, 1.0)?.clamp(0.0, 1.0),
            None => 1.0,
        };
        return match f.trim() {
            "rgb" | "rgba" => Some([
                number(args[0], 255.0)?.clamp(0.0, 255.0),
                number(args[1], 255.0)?.clamp(0.0, 255.0),
                number(args[2], 255.0)?.clamp(0.0, 255.0),
                alpha,
            ]),
            "hsl" | "hsla" => {
                let h = angle(args[0])?;
                let s = (number(args[1], 100.0)? / 100.0).clamp(0.0, 1.0);
                let l = (number(args[2], 100.0)? / 100.0).clamp(0.0, 1.0);
                let [r, g, b] = hsl_to_rgb(h, s, l);
                Some([r * 255.0, g * 255.0, b * 255.0, alpha])
            }
            _ => None,
        };
    }
    let i = NAMED.binary_search_by(|(n, _)| (*n).cmp(v)).ok()?;
    let x = NAMED[i].1;
    Some([
        (x >> 16) as u8 as f32,
        (x >> 8) as u8 as f32,
        x as u8 as f32,
        1.0,
    ])
}

/// A number, or a percentage of `full`.
fn number(s: &str, full: f32) -> Option<f32> {
    let n = match s.strip_suffix('%') {
        Some(p) => p.parse::<f32>().ok()? / 100.0 * full,
        None => s.parse::<f32>().ok()?,
    };
    n.is_finite().then_some(n)
}

/// A hue in degrees.
fn angle(s: &str) -> Option<f32> {
    let (n, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => s.split_at(i),
        None => (s, "deg"),
    };
    let n = n.parse::<f32>().ok().filter(|n| n.is_finite())?;
    let deg = match unit {
        "deg" => n,
        "turn" => n * 360.0,
        "rad" => n.to_degrees(),
        "grad" => n * 0.9,
        _ => return None,
    };
    Some(deg.rem_euclid(360.0))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let f = |n: f32| {
        let k = (n + h / 30.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [f(0.0), f(8.0), f(4.0)]
}

/// The palette color shown for `[r, g, b, a]`: by hue, gray for colors
/// close to it, none for transparent ones and for text in black or white
/// (the default color reads better on both themes).
pub fn nearest([r, g, b, a]: [f32; 4], kind: Kind) -> Option<Color> {
    if a <= 0.0 {
        return None;
    }
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    let light = (max + min) / 2.0;
    if chroma < 0.1 {
        let (dark, bright) = match kind {
            Kind::Text => (0.15, 0.85),
            Kind::Background => (0.03, 0.97),
        };
        return (light > dark && light < bright).then_some(Color::Gray);
    }
    let hue = if max == r {
        60.0 * ((g - b) / chroma)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    }
    .rem_euclid(360.0);
    Some(match hue {
        // Light red text is pink (`pink`, `lightpink`).
        h if !(15.0..345.0).contains(&h) => {
            if kind == Kind::Text && light >= 0.8 {
                Color::Pink
            } else {
                Color::Red
            }
        }
        h if h < 42.0 => Color::Orange,
        h if h < 70.0 => Color::Yellow,
        h if h < 165.0 => Color::Green,
        h if h < 255.0 => Color::Blue,
        h if h < 305.0 => Color::Purple,
        _ => Color::Pink,
    })
}

/// CSS named colors, sorted by name.
const NAMED: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn el(name: &str, attrs: &[(&str, &str)]) -> Colors {
        of_element(name, attrs.iter().copied())
    }

    fn text(v: &str) -> Option<Color> {
        el("span", &[("style", &format!("color: {v}"))]).text
    }

    #[test]
    fn what_the_toolbar_writes_reads_back() {
        for c in Color::ALL {
            for kind in [Kind::Text, Kind::Background] {
                let x = Colors::default().with(kind, Some(c));
                let tag = x.span();
                let style = tag
                    .strip_prefix("<span style=\"")
                    .and_then(|r| r.strip_suffix("\">"))
                    .unwrap();
                assert_eq!(el("span", &[("style", style)]), x, "{tag}");
                assert_eq!(el("span", &[("class", &x.classes())]), x);
            }
            assert_eq!(Color::from_code(c.code()), Some(c));
        }
        let both = Colors {
            text: Some(Color::Red),
            background: Some(Color::Yellow),
        };
        assert_eq!(
            both.span(),
            "<span style=\"color: #e44045; background-color: #ffd0004d\">"
        );
        assert_eq!(both.classes(), "fg-red bg-yellow");
        assert_eq!(Color::from_code(0), None);
        assert_eq!(Color::from_code(9), None);
    }

    #[test]
    fn reads_what_other_editors_write() {
        // Obsidian's Editing Toolbar, Highlightr, Typora, Notion's HTML.
        assert_eq!(el("font", &[("color", "#FF0000")]).text, Some(Color::Red));
        assert_eq!(el("font", &[("color", "0000ff")]).text, Some(Color::Blue));
        assert_eq!(
            el("mark", &[("style", "background: #FFB86CA6;")]).background,
            Some(Color::Orange)
        );
        assert_eq!(
            el("span", &[("style", "color:rgb(68, 131, 97)")]).text,
            Some(Color::Green)
        );
        assert_eq!(
            el(
                "span",
                &[(
                    "style",
                    "color: red; background-color: hsl(210 80% 50% / .3)"
                )]
            ),
            Colors {
                text: Some(Color::Red),
                background: Some(Color::Blue)
            }
        );
        // Only these elements; other attributes don't count.
        assert_eq!(el("b", &[("style", "color: red")]), Colors::default());
        assert_eq!(el("span", &[("color", "red")]), Colors::default());
        assert_eq!(el("span", &[("class", "red fg-teal x")]), Colors::default());
    }

    #[test]
    fn style_wins_and_later_declarations_override() {
        let c = el(
            "font",
            &[
                ("class", "fg-green"),
                ("color", "red"),
                ("style", "color: blue"),
            ],
        );
        assert_eq!(c.text, Some(Color::Blue));
        assert_eq!(text("red; color: blue"), Some(Color::Blue));
        assert_eq!(text("red; color: nonsense"), Some(Color::Red));
        assert_eq!(text("red; color: black"), None);
        assert_eq!(text("red !important"), Some(Color::Red));
        assert_eq!(text("RED ! IMPORTANT"), Some(Color::Red));
        let bg = |s: &str| el("span", &[("class", "bg-red"), ("style", s)]).background;
        assert_eq!(bg("background: url(x.png) no-repeat"), None);
        assert_eq!(
            bg("background: url(a,b) rgba(0,0,255,.5) top/cover"),
            Some(Color::Blue)
        );
        assert_eq!(bg("background-color: transparent"), None);
        assert_eq!(bg("background-color: #12"), Some(Color::Red));
    }

    #[test]
    fn colors_snap_to_the_palette() {
        let cases = [
            ("red", Some(Color::Red)),
            ("crimson", Some(Color::Red)),
            ("darkred", Some(Color::Red)),
            ("orange", Some(Color::Orange)),
            ("chocolate", Some(Color::Orange)),
            ("gold", Some(Color::Yellow)),
            ("yellow", Some(Color::Yellow)),
            ("green", Some(Color::Green)),
            ("lime", Some(Color::Green)),
            ("teal", Some(Color::Blue)),
            ("navy", Some(Color::Blue)),
            ("purple", Some(Color::Purple)),
            ("rebeccapurple", Some(Color::Purple)),
            ("magenta", Some(Color::Purple)),
            ("deeppink", Some(Color::Pink)),
            ("hotpink", Some(Color::Pink)),
            ("pink", Some(Color::Pink)),
            ("gray", Some(Color::Gray)),
            ("grey", Some(Color::Gray)),
            ("#888", Some(Color::Gray)),
            ("black", None),
            ("white", None),
            ("#ff000000", None),
            ("transparent", None),
            ("inherit", None),
        ];
        for (v, want) in cases {
            assert_eq!(text(v), want, "{v}");
        }
        assert_eq!(
            el("span", &[("style", "background-color: pink")]).background,
            Some(Color::Red)
        );
        assert_eq!(
            el("span", &[("style", "background-color: #333")]).background,
            Some(Color::Gray)
        );
        assert_eq!(
            el("span", &[("style", "background-color: white")]).background,
            None
        );
    }

    #[test]
    fn css_color_syntax() {
        let c = |v: &str| {
            rgba(v).map(|[r, g, b, a]| [r.round(), g.round(), b.round(), (a * 100.0).round()])
        };
        assert_eq!(c("#f80"), Some([255.0, 136.0, 0.0, 100.0]));
        assert_eq!(c("#f808"), Some([255.0, 136.0, 0.0, 53.0]));
        assert_eq!(c("#ff8000"), Some([255.0, 128.0, 0.0, 100.0]));
        assert_eq!(c("#ff800080"), Some([255.0, 128.0, 0.0, 50.0]));
        assert_eq!(c("rgb(255 128 0 / 50%)"), Some([255.0, 128.0, 0.0, 50.0]));
        assert_eq!(c("rgba(100%,50%,0%,0.5)"), Some([255.0, 128.0, 0.0, 50.0]));
        assert_eq!(c("rgb(300, -5, 0)"), Some([255.0, 0.0, 0.0, 100.0]));
        assert_eq!(c("hsl(120, 100%, 25%)"), Some([0.0, 128.0, 0.0, 100.0]));
        assert_eq!(c("hsl(0.5turn 100% 50%)"), Some([0.0, 255.0, 255.0, 100.0]));
        assert_eq!(
            c("hsla(-120deg, 100%, 50%, .5)"),
            Some([0.0, 0.0, 255.0, 50.0])
        );
        assert_eq!(c("rebeccapurple"), Some([102.0, 51.0, 153.0, 100.0]));
        for bad in [
            "",
            "#",
            "#12",
            "#12345",
            "#ggg",
            "#ééé",
            "rgb(1,2)",
            "rgb(1,2,3",
            "rgb(a,b,c)",
            "hsl(1x, 2%, 3%)",
            "rgb(NaN, 1, 2)",
            "rgb(inf, 1, 2)",
            "nope",
            "lab(50% 40 59)",
        ] {
            assert_eq!(c(bad), None, "{bad}");
        }
        assert!(NAMED.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
