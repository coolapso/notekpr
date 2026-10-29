//! Text inside shapes and on arrows (labels), as Excalidraw lays them out
//! (`textElement.ts`, `textWrapping.ts`; MIT, see `PORTED.md`): wrapped to
//! the room inside the shape, centred, the shape growing to fit; on an
//! arrow, centred halfway along it.

use serde_json::{Value, json};

use super::new::Measure;
use crate::fonts;
use crate::geom::{self, Point};
use crate::scene::{Element, ElementType, num};

/// `BOUND_TEXT_PADDING`.
pub const PADDING: f64 = 5.0;
const ARROW_LABEL_WIDTH_FRACTION: f64 = 0.7;
const ARROW_LABEL_FONT_SIZE_TO_MIN_WIDTH_RATIO: f64 = 11.0;

/// Whether an element can hold a label.
pub fn can_hold(e: &Element) -> bool {
    matches!(
        e.kind(),
        ElementType::Rectangle | ElementType::Diamond | ElementType::Ellipse | ElementType::Arrow
    )
}

/// `getBoundTextMaxWidth`: how wide its label may be.
pub fn max_width(container: &Element, font_size: f64) -> f64 {
    let w = container.width();
    match container.kind() {
        ElementType::Arrow => (ARROW_LABEL_WIDTH_FRACTION * w)
            .max(font_size * ARROW_LABEL_FONT_SIZE_TO_MIN_WIDTH_RATIO),
        ElementType::Ellipse => {
            crate::js::round((w / 2.0) * std::f64::consts::SQRT_2) - PADDING * 2.0
        }
        ElementType::Diamond => crate::js::round(w / 2.0) - PADDING * 2.0,
        _ => w - PADDING * 2.0,
    }
}

/// `getBoundTextMaxHeight`: how high its label may be before it grows.
pub fn max_height(container: &Element, label_height: f64) -> f64 {
    let h = container.height();
    match container.kind() {
        ElementType::Arrow => {
            if h - PADDING * 8.0 * 2.0 <= 0.0 {
                label_height
            } else {
                h
            }
        }
        ElementType::Ellipse => {
            crate::js::round((h / 2.0) * std::f64::consts::SQRT_2) - PADDING * 2.0
        }
        ElementType::Diamond => crate::js::round(h / 2.0) - PADDING * 2.0,
        _ => h - PADDING * 2.0,
    }
}

/// `getContainerCoords`: the top left of the room inside.
pub fn inner_origin(c: &Element) -> Point {
    let (mut ox, mut oy) = (PADDING, PADDING);
    match c.kind() {
        ElementType::Ellipse => {
            ox += (c.width() / 2.0) * (1.0 - std::f64::consts::SQRT_2 / 2.0);
            oy += (c.height() / 2.0) * (1.0 - std::f64::consts::SQRT_2 / 2.0);
        }
        ElementType::Diamond => {
            ox += c.width() / 4.0;
            oy += c.height() / 4.0;
        }
        _ => {}
    }
    [c.x() + ox, c.y() + oy]
}

/// `computeContainerDimensionForBoundText`: the size a shape needs for a
/// label of `size`.
fn needed(size: f64, kind: &ElementType) -> f64 {
    let size = size.ceil();
    let pad = PADDING * 2.0;
    match kind {
        ElementType::Ellipse => crate::js::round(((size + pad) / std::f64::consts::SQRT_2) * 2.0),
        ElementType::Arrow => size + pad * 8.0,
        ElementType::Diamond => 2.0 * (size + pad),
        _ => size + pad,
    }
}

/// Wraps text to `max` pixels wide: at spaces, a word longer than a line
/// cut where it must; the lines the writer made stay.
pub fn wrap(text: &str, font: &str, max: f64, measure: Measure) -> String {
    if max <= 0.0 {
        return text.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if measure(font, line) <= max {
            out.push(line.to_string());
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if measure(font, &candidate) <= max {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            // A word too long for a line: cut it by characters.
            let mut piece = String::new();
            for ch in word.chars() {
                let next = format!("{piece}{ch}");
                if measure(font, &next) > max && !piece.is_empty() {
                    out.push(std::mem::take(&mut piece));
                    piece.push(ch);
                } else {
                    piece = next;
                }
            }
            current = piece;
        }
        out.push(current);
    }
    out.join("\n")
}

/// Halfway along an arrow's path (its label's centre).
pub fn arrow_middle(arrow: &Element) -> Point {
    let path = super::hit::linear_path(arrow);
    let lengths: Vec<f64> = path
        .windows(2)
        .map(|w| geom::distance(w[0], w[1]))
        .collect();
    let total: f64 = lengths.iter().sum();
    let mut left = total / 2.0;
    for (i, len) in lengths.iter().enumerate() {
        if left <= *len && *len > 0.0 {
            let t = left / len;
            let (a, b) = (path[i], path[i + 1]);
            return [
                arrow.x() + a[0] + (b[0] - a[0]) * t,
                arrow.y() + a[1] + (b[1] - a[1]) * t,
            ];
        }
        left -= len;
    }
    let p = path.first().copied().unwrap_or([0.0, 0.0]);
    [arrow.x() + p[0], arrow.y() + p[1]]
}

/// Lays a label out in its container: its text wrapped to fit, its size
/// measured, its place inside; the container grows if the label needs more
/// height. Returns both (the container only if it changed).
pub fn layout(
    container: &Element,
    label: &Element,
    measure: Measure,
) -> (Element, Option<Element>) {
    let mut t = label.clone();
    let mut c = container.clone();
    let size = t.font_size();
    let font = format!(
        "{}px {}",
        crate::js::number_to_string(size),
        fonts::css_family(t.font_family())
    );
    let wrapped = wrap(t.original_text(), &font, max_width(&c, size), measure);
    let lines: Vec<&str> = wrapped.split('\n').collect();
    let width = lines.iter().map(|l| measure(&font, l)).fold(0.0, f64::max);
    let height = lines.len() as f64 * size * t.line_height();
    t.set("text", json!(wrapped));
    t.set("width", num(width));
    t.set("height", num(height));
    t.set(
        "angle",
        if c.kind() == ElementType::Arrow {
            json!(0)
        } else {
            num(c.angle())
        },
    );
    let mut grew = false;
    if c.kind() != ElementType::Arrow {
        if height > max_height(&c, height) {
            c.set("height", num(needed(height, &c.kind())));
            grew = true;
        }
        if width > max_width(&c, size) + 0.5 && c.width() < needed(width, &c.kind()) {
            c.set("width", num(needed(width, &c.kind())));
            grew = true;
        }
    }
    let (x, y) = if c.kind() == ElementType::Arrow {
        let [mx, my] = arrow_middle(&c);
        (mx - width / 2.0, my - height / 2.0)
    } else {
        let [ox, oy] = inner_origin(&c);
        let (mw, mh) = (max_width(&c, size), max_height(&c, height));
        let y = match t.vertical_align() {
            "top" => oy,
            "bottom" => oy + (mh - height),
            _ => oy + (mh / 2.0 - height / 2.0),
        };
        let x = match t.text_align() {
            "left" => ox,
            "right" => ox + (mw - width),
            _ => ox + (mw / 2.0 - width / 2.0),
        };
        (x, y)
    };
    t.set("x", num(x));
    t.set("y", num(y));
    (t, grew.then_some(c))
}

/// A new, empty label for `container`, and the container listing it.
pub fn new_label(
    container: &Element,
    style: &super::Style,
    rng: &mut dyn super::Random,
) -> (Element, Element) {
    let mut s = style.clone();
    s.text_align = "center".into();
    let mut t = super::new::text(container.x(), container.y(), "", &s, &|_, _| 0.0, rng);
    t.set("containerId", json!(container.id()));
    t.set("verticalAlign", json!("middle"));
    t.set("textAlign", json!("center"));
    t.set("strokeColor", json!(container.stroke_color()));
    let mut c = container.clone();
    let mut bound: Vec<Value> = container
        .bound_elements()
        .iter()
        .map(|b| json!({"id": b.id, "type": b.kind}))
        .collect();
    bound.push(json!({"id": t.id(), "type": "text"}));
    c.set("boundElements", Value::Array(bound));
    (t, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(v: serde_json::Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    /// Ten pixels a character.
    fn m(_: &str, s: &str) -> f64 {
        s.chars().count() as f64 * 10.0
    }

    #[test]
    fn wraps_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", "f", 75.0, &m), "one two\nthree");
        assert_eq!(wrap("abcdefghij", "f", 40.0, &m), "abcd\nefgh\nij");
        assert_eq!(wrap("kept\nlines", "f", 1000.0, &m), "kept\nlines");
    }

    #[test]
    fn centred_in_shapes_which_grow() {
        let rect =
            el(json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 200, "height": 50}));
        let label = el(
            json!({"id": "t", "type": "text", "text": "hello", "originalText": "hello",
            "fontSize": 20, "lineHeight": 1.25, "textAlign": "center", "verticalAlign": "middle", "containerId": "r"}),
        );
        let (t, grown) = layout(&rect, &label, &m);
        assert!(grown.is_none());
        assert_eq!((t.width(), t.height()), (50.0, 25.0));
        assert_eq!((t.x(), t.y()), (75.0, 12.5));
        // Four lines don't fit in 50: it grows.
        let long = el(
            json!({"id": "t", "type": "text", "originalText": "a\nb\nc\nd", "text": "",
            "fontSize": 20, "lineHeight": 1.25, "textAlign": "center", "verticalAlign": "middle", "containerId": "r"}),
        );
        let (t, grown) = layout(&rect, &long, &m);
        assert_eq!(grown.unwrap().height(), 110.0);
        assert_eq!(t.height(), 100.0);
    }

    #[test]
    fn on_arrows_halfway() {
        let arrow = el(
            json!({"id": "a", "type": "arrow", "x": 0, "y": 0, "width": 200, "height": 0,
            "points": [[0, 0], [200, 0]], "seed": 3}),
        );
        let label = el(
            json!({"id": "t", "type": "text", "originalText": "go", "text": "go",
            "fontSize": 20, "lineHeight": 1.25, "textAlign": "center", "verticalAlign": "middle", "containerId": "a"}),
        );
        let (t, grown) = layout(&arrow, &label, &m);
        assert!(grown.is_none());
        assert!((t.x() + t.width() / 2.0 - 100.0).abs() < 1.0, "{}", t.x());
    }
}
