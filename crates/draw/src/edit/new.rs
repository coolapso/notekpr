//! New elements, with the properties Excalidraw gives them (`newElement.ts`;
//! MIT, see `PORTED.md`), in the style being drawn with.

use serde_json::{Value, json};

use super::{Random, Style};
use crate::fonts;
use crate::geom::Point;
use crate::scene::{Element, ElementType, Props, num, roundness};

/// What every element has (`_newElementBase`).
fn base(kind: &ElementType, x: f64, y: f64, style: &Style, rng: &mut dyn Random) -> Props {
    let now = rng.now();
    let round = style.round_edges
        && matches!(
            kind,
            ElementType::Rectangle | ElementType::Diamond | ElementType::Line | ElementType::Image
        );
    let mut p = Props::new();
    let mut set = |k: &str, v: Value| {
        p.insert(k.to_string(), v);
    };
    set("id", json!(rng.id()));
    set("type", json!(kind.as_str()));
    set("x", num(x));
    set("y", num(y));
    set("width", json!(0));
    set("height", json!(0));
    set("angle", json!(0));
    set("strokeColor", json!(style.stroke_color));
    set("backgroundColor", json!(style.background_color));
    set("fillStyle", json!(style.fill_style));
    set("strokeWidth", num(style.stroke_width));
    set("strokeStyle", json!(style.stroke_style));
    set("roughness", num(style.roughness));
    set("opacity", num(style.opacity));
    set("groupIds", json!([]));
    set("frameId", Value::Null);
    set("index", Value::Null);
    set(
        "roundness",
        if round {
            // Rectangles round adaptively, the others proportionally.
            let kind_of = if *kind == ElementType::Rectangle || *kind == ElementType::Image {
                roundness::ADAPTIVE_RADIUS
            } else {
                roundness::PROPORTIONAL_RADIUS
            };
            json!({"type": kind_of})
        } else {
            Value::Null
        },
    );
    set("seed", json!(rng.seed()));
    set("version", json!(1));
    set("versionNonce", json!(rng.seed()));
    set("isDeleted", json!(false));
    set("boundElements", Value::Null);
    set("updated", num(now));
    set("created", num(now));
    set("link", Value::Null);
    set("locked", json!(false));
    p
}

fn element(p: Props) -> Element {
    Element::from_props(p).expect("has an id and a type")
}

/// A rectangle, diamond, ellipse, frame or embed at `x`, `y`, no size yet.
pub fn shape(kind: ElementType, x: f64, y: f64, style: &Style, rng: &mut dyn Random) -> Element {
    let mut p = base(&kind, x, y, style, rng);
    if matches!(kind, ElementType::Frame | ElementType::MagicFrame) {
        // Frames have their own look (`FRAME_STYLE`).
        p.insert("strokeColor".into(), json!("#bbb"));
        p.insert("backgroundColor".into(), json!("transparent"));
        p.insert("fillStyle".into(), json!("solid"));
        p.insert("strokeWidth".into(), json!(2));
        p.insert("strokeStyle".into(), json!("solid"));
        p.insert("roughness".into(), json!(0));
        p.insert("roundness".into(), Value::Null);
        p.insert("name".into(), Value::Null);
    }
    element(p)
}

/// A line or arrow starting at `x`, `y` (its first point).
pub fn linear(kind: ElementType, x: f64, y: f64, style: &Style, rng: &mut dyn Random) -> Element {
    let mut p = base(&kind, x, y, style, rng);
    p.insert("points".into(), json!([[0, 0]]));
    p.insert("startBinding".into(), Value::Null);
    p.insert("endBinding".into(), Value::Null);
    if kind == ElementType::Arrow {
        p.insert("startArrowhead".into(), opt(&style.start_arrowhead));
        p.insert("endArrowhead".into(), opt(&style.end_arrowhead));
        match style.arrow_type.as_str() {
            "sharp" => {
                p.insert("roundness".into(), Value::Null);
                p.insert("elbowed".into(), json!(false));
            }
            "elbow" => {
                p.insert("roundness".into(), Value::Null);
                p.insert("elbowed".into(), json!(true));
                p.insert("fixedSegments".into(), json!([]));
                p.insert("startIsSpecial".into(), json!(false));
                p.insert("endIsSpecial".into(), json!(false));
            }
            _ => {
                p.insert(
                    "roundness".into(),
                    json!({"type": roundness::PROPORTIONAL_RADIUS}),
                );
                p.insert("elbowed".into(), json!(false));
            }
        }
    } else {
        p.insert("startArrowhead".into(), Value::Null);
        p.insert("endArrowhead".into(), Value::Null);
        p.insert("polygon".into(), json!(false));
    }
    element(p)
}

fn opt(s: &Option<String>) -> Value {
    s.as_ref().map_or(Value::Null, |s| json!(s))
}

/// A pen stroke starting at `x`, `y`.
pub fn freedraw(
    x: f64,
    y: f64,
    pressure: Option<f64>,
    style: &Style,
    rng: &mut dyn Random,
) -> Element {
    let mut p = base(&ElementType::Freedraw, x, y, style, rng);
    p.insert("points".into(), json!([[0, 0]]));
    p.insert(
        "pressures".into(),
        match pressure {
            Some(pr) => json!([num(pr)]),
            None => json!([]),
        },
    );
    p.insert("simulatePressure".into(), json!(pressure.is_none()));
    p.insert(
        "strokeOptions".into(),
        json!({"variability": style.stroke_variability, "streamline": 0.5}),
    );
    element(p)
}

/// A picture (`newImageElement`): the drawing's file `file_id`, its box at
/// `x`, `y`, `width` by `height`.
pub fn image(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    file_id: &str,
    style: &Style,
    rng: &mut dyn Random,
) -> Element {
    let mut p = base(&ElementType::Image, x, y, style, rng);
    p.insert("width".into(), num(width));
    p.insert("height".into(), num(height));
    p.insert("fileId".into(), json!(file_id));
    p.insert("status".into(), json!("saved"));
    p.insert("scale".into(), json!([1, 1]));
    p.insert("crop".into(), Value::Null);
    element(p)
}

/// Measures a line of text: its width in pixels, in a CSS font (`20px
/// Excalifont, …`). The app measures with the browser.
pub type Measure<'a> = &'a dyn Fn(&str, &str) -> f64;

/// A text's box: the widest line, the lines' height (`measureText`).
pub fn text_size(
    text: &str,
    font_size: f64,
    font_family: u32,
    line_height: f64,
    measure: Measure,
) -> (f64, f64) {
    let font = format!(
        "{}px {}",
        crate::js::number_to_string(font_size),
        fonts::css_family(font_family)
    );
    let lines: Vec<&str> = text.split('\n').collect();
    let width = lines.iter().map(|l| measure(&font, l)).fold(0.0, f64::max);
    let height = lines.len() as f64 * font_size * line_height;
    (width, height)
}

/// A text at `x`, `y` (its top left).
pub fn text(
    x: f64,
    y: f64,
    content: &str,
    style: &Style,
    measure: Measure,
    rng: &mut dyn Random,
) -> Element {
    let mut p = base(&ElementType::Text, x, y, style, rng);
    let line_height = fonts::get(style.font_family).line_height;
    let (w, h) = text_size(
        content,
        style.font_size,
        style.font_family,
        line_height,
        measure,
    );
    p.insert("width".into(), num(w));
    p.insert("height".into(), num(h));
    p.insert("text".into(), json!(content));
    p.insert("originalText".into(), json!(content));
    p.insert("fontSize".into(), num(style.font_size));
    p.insert("fontFamily".into(), json!(style.font_family));
    p.insert("textAlign".into(), json!(style.text_align));
    p.insert("verticalAlign".into(), json!("top"));
    p.insert("containerId".into(), Value::Null);
    p.insert("autoResize".into(), json!(true));
    p.insert("lineHeight".into(), num(line_height));
    p.insert("labelPosition".into(), Value::Null);
    p.insert("baseFontSize".into(), Value::Null);
    // Text has no roundness or fill.
    p.insert("roundness".into(), Value::Null);
    element(p)
}

/// Points relative to the first, the first at 0,0, as linear elements keep
/// them; with the element's position and size following.
pub fn normalize_points(e: &mut Element, absolute: &[Point]) {
    let Some(&[ox, oy]) = absolute.first() else {
        return;
    };
    let rel: Vec<Value> = absolute
        .iter()
        .map(|[x, y]| json!([num(x - ox), num(y - oy)]))
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for [x, y] in absolute {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    e.set("x", num(ox));
    e.set("y", num(oy));
    e.set("points", Value::Array(rel));
    e.set("width", num(x1 - x0));
    e.set("height", num(y1 - y0));
}
