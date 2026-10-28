//! Making a drawing from a file safe to use, as Excalidraw's `restore` does:
//! missing properties get their defaults, old drawings' properties are
//! brought up to date, malformed points are repaired, and every element
//! gets a unique id and a stacking index. Properties this code doesn't know
//! are kept as they are.
//!
//! Follows Excalidraw's `data/restore.ts` (MIT, see `PORTED.md`). Unlike
//! Excalidraw, elements of a kind it doesn't know are kept (not drawn), so a
//! newer Excalidraw's drawing loses nothing passing through.

use serde_json::{Map, Value, json};

use crate::fonts;
use crate::order;
use crate::scene::{Element, ElementType, Props, SAVED_APP_STATE, Scene, num, roundness};

/// Lines and arrows longer than this are deleted: drawing them (a dashed
/// line especially) would freeze the app.
const MAX_LINEAR_PX: f64 = 75_000.0;

/// What [`restore_element`] needs that isn't in the element: a source of
/// new ids and seeds.
pub trait Fresh {
    fn id(&mut self) -> String;
}

fn finite(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

fn truthy_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn valid_point(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    let x = finite(a.first())?;
    let y = finite(a.get(1))?;
    Some([x, y])
}

fn points_value(points: &[[f64; 2]]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|[x, y]| Value::Array(vec![num(*x), num(*y)]))
            .collect(),
    )
}

/// Width and height of the box around `points`.
fn size_of(points: &[[f64; 2]]) -> (f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for [x, y] in points {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    if points.is_empty() {
        (0.0, 0.0)
    } else {
        (x1 - x0, y1 - y0)
    }
}

/// An arrowhead's name, or none (`null`), old names renamed
/// (`normalizeArrowhead`).
fn arrowhead(v: Option<&Value>) -> Value {
    let name = match v.and_then(Value::as_str) {
        None => return Value::Null,
        Some("dot") => "circle",
        Some("crowfoot_one") => "cardinality_one",
        Some("crowfoot_many") => "cardinality_many",
        Some("crowfoot_one_or_many") => "cardinality_one_or_many",
        Some(other) => other,
    };
    json!(name)
}

/// The element's own properties first, then the defaults of everything
/// common (`restoreElementWithProperties`'s `base`).
fn base(e: &mut Props, fresh: &mut dyn Fresh) {
    let get = |e: &Props, k: &str| e.get(k).cloned();
    let kind = e
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // `version || 1`, `width || 0`, …: JavaScript's falsy values.
    let or_default = |e: &mut Props, k: &str, default: Value| {
        let falsy = match e.get(k) {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => !b,
            Some(Value::Number(n)) => n.as_f64().is_none_or(|x| x == 0.0 || x.is_nan()),
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        };
        if falsy {
            e.insert(k.to_string(), default);
        }
    };
    // `x ?? default`: only missing or null.
    let or_null = |e: &mut Props, k: &str, default: Value| {
        if matches!(e.get(k), None | Some(Value::Null)) {
            e.insert(k.to_string(), default);
        }
    };
    or_default(e, "version", json!(1));
    or_null(e, "versionNonce", json!(0));
    or_null(e, "index", Value::Null);
    or_null(e, "isDeleted", json!(false));
    if truthy_str(e.get("id")).is_none() {
        e.insert("id".into(), json!(fresh.id()));
    }
    or_default(e, "fillStyle", json!("solid"));
    or_default(e, "strokeWidth", json!(2));
    or_null(e, "strokeStyle", json!("solid"));
    or_null(e, "roughness", json!(1));
    or_null(e, "opacity", json!(100));
    or_default(e, "angle", json!(0));
    or_null(e, "x", json!(0));
    or_null(e, "y", json!(0));
    or_default(e, "strokeColor", json!("#1e1e1e"));
    or_default(e, "backgroundColor", json!("transparent"));
    or_default(e, "width", json!(0));
    or_default(e, "height", json!(0));
    or_null(e, "seed", json!(1));
    or_null(e, "groupIds", json!([]));
    or_null(e, "frameId", Value::Null);
    // Old drawings: `strokeSharpness: "round"` instead of `roundness`.
    let has_roundness = !matches!(
        e.get("roundness"),
        None | Some(Value::Null) | Some(Value::Bool(false))
    );
    if !has_roundness {
        let round = get(e, "strokeSharpness").is_some_and(|s| s == "round");
        let adaptive = matches!(
            kind.as_str(),
            "rectangle" | "embeddable" | "iframe" | "image"
        );
        e.insert(
            "roundness".into(),
            if round {
                json!({"type": if adaptive { roundness::LEGACY } else { roundness::PROPORTIONAL_RADIUS }})
            } else {
                Value::Null
            },
        );
    }
    // Old drawings: `boundElementIds` (all arrows).
    if let Some(Value::Array(ids)) = e.remove("boundElementIds") {
        let bound: Vec<Value> = ids
            .iter()
            .filter_map(Value::as_str)
            .map(|id| json!({"type": "arrow", "id": id}))
            .collect();
        e.insert("boundElements".into(), Value::Array(bound));
    } else {
        or_null(e, "boundElements", json!([]));
    }
    or_null(e, "updated", json!(1));
    or_null(e, "created", Value::Null);
    let link = truthy_str(e.get("link")).map(|l| l.trim().to_string());
    e.insert(
        "link".into(),
        link.filter(|l| !l.is_empty())
            .map_or(Value::Null, Value::String),
    );
    or_null(e, "locked", json!(false));
    e.remove("strokeSharpness");
    e.remove("moveMidPointsWithElement");
    // `getNormalizedDimensions`: no negative sizes.
    let (x, y) = (
        finite(e.get("x")).unwrap_or(0.0),
        finite(e.get("y")).unwrap_or(0.0),
    );
    let (w, h) = (
        finite(e.get("width")).unwrap_or(0.0),
        finite(e.get("height")).unwrap_or(0.0),
    );
    if w < 0.0 {
        e.insert("x".into(), num(x + w));
        e.insert("width".into(), num(-w));
    }
    if h < 0.0 {
        e.insert("y".into(), num(y + h));
        e.insert("height".into(), num(-h));
    }
}

/// `restoreLinearElementPoints`: valid points, at least two.
fn linear_points(e: &Props) -> Vec<[f64; 2]> {
    let points: Vec<[f64; 2]> = e
        .get("points")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(valid_point).collect())
        .unwrap_or_default();
    if points.len() < 2 {
        vec![
            [0.0, 0.0],
            [
                finite(e.get("width")).unwrap_or(0.0),
                finite(e.get("height")).unwrap_or(0.0),
            ],
        ]
    } else {
        points
    }
}

/// Moves a line's points so the first is at 0,0, moving the element the
/// other way (`getNormalizeElementPointsAndCoords`).
fn normalize_points(e: &mut Props, mut points: Vec<[f64; 2]>) {
    let [ox, oy] = points[0];
    if ox != 0.0 || oy != 0.0 {
        for p in &mut points {
            p[0] -= ox;
            p[1] -= oy;
        }
        let x = finite(e.get("x")).unwrap_or(0.0) + ox;
        let y = finite(e.get("y")).unwrap_or(0.0) + oy;
        e.insert("x".into(), num(x));
        e.insert("y".into(), num(y));
    }
    let (w, h) = size_of(&points);
    e.insert("width".into(), num(w));
    e.insert("height".into(), num(h));
    e.insert("points".into(), points_value(&points));
    if w > MAX_LINEAR_PX || h > MAX_LINEAR_PX {
        e.insert("x".into(), json!(0));
        e.insert("y".into(), json!(0));
        e.insert("width".into(), json!(100));
        e.insert("height".into(), json!(100));
        e.insert("points".into(), json!([[0, 0], [100, 100]]));
        e.insert("isDeleted".into(), json!(true));
    }
}

/// One element made safe to use; `None` for the legacy `selection`
/// element and anything without a type.
pub fn restore_element(props: Props, fresh: &mut dyn Fresh) -> Option<Element> {
    let mut e = props;
    let kind = e.get("type")?.as_str()?.to_string();
    match ElementType::parse(&kind) {
        ElementType::Selection => return None,
        ElementType::Text => {
            // Obsidian's own copy of the text; Excalidraw drops it so the
            // two apps don't disagree.
            e.remove("rawText");
            let (mut size, mut family) = (finite(e.get("fontSize")), finite(e.get("fontFamily")));
            // Old drawings: `font: "20px Virgil"`.
            if let Some(font) = e.remove("font").as_ref().and_then(Value::as_str) {
                let mut parts = font.split(' ');
                size = parts
                    .next()
                    .and_then(|px| px.trim_end_matches("px").parse::<f64>().ok());
                family = Some(fonts::by_name(parts.next().unwrap_or_default()) as f64);
            }
            let size = size.unwrap_or(20.0);
            let family = family.unwrap_or(fonts::DEFAULT as f64);
            let text = e
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            e.insert("fontSize".into(), num(size));
            e.insert("fontFamily".into(), num(family));
            if finite(e.get("lineHeight")).is_none_or(|l| l == 0.0) {
                // An old drawing's line height is whatever its box says.
                let lines = text.split('\n').count().max(1) as f64;
                let detected = finite(e.get("height"))
                    .filter(|h| *h > 0.0)
                    .map(|h| h / lines / size);
                let lh = detected
                    .filter(|l| l.is_finite() && *l > 0.0)
                    .unwrap_or(fonts::get(family as u32).line_height);
                e.insert("lineHeight".into(), num(lh));
            }
            if truthy_str(e.get("originalText")).is_none() {
                e.insert("originalText".into(), json!(text));
            }
            e.insert("text".into(), json!(text));
            for (k, default) in [("textAlign", "left"), ("verticalAlign", "top")] {
                if truthy_str(e.get(k)).is_none() {
                    e.insert(k.into(), json!(default));
                }
            }
            if !e.get("containerId").is_some_and(Value::is_string) {
                e.insert("containerId".into(), Value::Null);
            }
            if !e.get("autoResize").is_some_and(Value::is_boolean) {
                e.insert("autoResize".into(), json!(true));
            }
            let label = finite(e.get("labelPosition")).map(|p| num(p.clamp(0.0, 1.0)));
            e.insert("labelPosition".into(), label.unwrap_or(Value::Null));
            if finite(e.get("baseFontSize")).is_none() {
                e.insert("baseFontSize".into(), Value::Null);
            }
        }
        ElementType::Freedraw => {
            let pressures: Vec<Value> = e
                .get("pressures")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut points = Vec::new();
            let mut kept = Vec::new();
            if let Some(Value::Array(raw)) = e.get("points") {
                for (i, p) in raw.iter().enumerate() {
                    if let Some(p) = valid_point(p) {
                        points.push(p);
                        if let Some(pr) = pressures.get(i) {
                            kept.push(num(pr.as_f64().filter(|x| x.is_finite()).unwrap_or(0.5)));
                        }
                    }
                }
            }
            e.insert("points".into(), points_value(&points));
            e.insert("pressures".into(), Value::Array(kept));
            let options = e.get("strokeOptions").and_then(Value::as_object);
            let variability = options
                .and_then(|o| o.get("variability"))
                .and_then(Value::as_str)
                .filter(|v| matches!(*v, "constant" | "variable"))
                .unwrap_or("variable");
            let streamline = finite(options.and_then(|o| o.get("streamline"))).unwrap_or(0.5);
            e.insert(
                "strokeOptions".into(),
                json!({"variability": variability, "streamline": num(streamline)}),
            );
        }
        ElementType::Image => {
            if truthy_str(e.get("status")).is_none() {
                e.insert("status".into(), json!("pending"));
            }
            if !e.get("scale").is_some_and(|s| valid_point(s).is_some()) {
                e.insert("scale".into(), json!([1, 1]));
            }
            if !e.contains_key("crop") {
                e.insert("crop".into(), Value::Null);
            }
        }
        ElementType::Line => {
            let start = arrowhead(e.get("startArrowhead"));
            let end = arrowhead(e.get("endArrowhead"));
            e.insert("startArrowhead".into(), start);
            e.insert("endArrowhead".into(), end);
            e.insert("startBinding".into(), Value::Null);
            e.insert("endBinding".into(), Value::Null);
            let points = linear_points(&e);
            normalize_points(&mut e, points);
        }
        ElementType::Other(ref k) if k == "draw" => {
            // Excalidraw's very first name for lines.
            e.insert("type".into(), json!("line"));
            return restore_element(e, fresh);
        }
        ElementType::Arrow => {
            let start = arrowhead(e.get("startArrowhead"));
            // An old arrow without the property has an arrowhead.
            let end = if e.contains_key("endArrowhead") {
                arrowhead(e.get("endArrowhead"))
            } else {
                json!("arrow")
            };
            e.insert("startArrowhead".into(), start);
            e.insert("endArrowhead".into(), end);
            for k in ["startBinding", "endBinding"] {
                let ok = e
                    .get(k)
                    .and_then(Value::as_object)
                    .is_some_and(|b| b.get("elementId").is_some_and(Value::is_string));
                if !ok {
                    e.insert(k.into(), Value::Null);
                }
            }
            let points = linear_points(&e);
            normalize_points(&mut e, points);
            if e.get("elbowed").and_then(Value::as_bool) == Some(true) {
                let n = e
                    .get("points")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let fixed = e
                    .get("fixedSegments")
                    .and_then(Value::as_array)
                    .is_some_and(|f| !f.is_empty());
                if !(fixed && n >= 4) {
                    e.insert("fixedSegments".into(), Value::Null);
                }
            }
        }
        ElementType::StickyNote => {
            if finite(e.get("baseHeight")).is_none() {
                let h = finite(e.remove("maxHeight").as_ref()).or(finite(e.get("height")));
                e.insert("baseHeight".into(), h.map_or(Value::Null, num));
            }
        }
        ElementType::Frame | ElementType::MagicFrame
            if !e.get("name").is_some_and(Value::is_string) =>
        {
            e.insert("name".into(), Value::Null);
        }
        _ => {}
    }
    base(&mut e, fresh);
    Element::from_props(e)
}

/// Restores every element: unique ids, valid stacking indices, and arrows
/// and labels pointing only at elements that are there.
pub fn restore_elements(raw: &[Value], fresh: &mut dyn Fresh) -> Vec<Element> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Element> = Vec::new();
    for v in raw {
        let Some(props) = v.as_object().cloned() else {
            continue;
        };
        let Some(mut e) = restore_element(props, fresh) else {
            continue;
        };
        if !seen.insert(e.id().to_string()) {
            e.set("id", Value::String(fresh.id()));
            seen.insert(e.id().to_string());
        }
        out.push(e);
    }
    order::sync_invalid_indices(&mut out);
    // Bindings and labels to elements that aren't there are dropped.
    let ids: std::collections::HashSet<String> = out.iter().map(|e| e.id().to_string()).collect();
    for e in &mut out {
        if e.kind().is_linear() {
            for k in ["startBinding", "endBinding"] {
                let gone = e
                    .get(k)
                    .and_then(|b| b.get("elementId"))
                    .and_then(Value::as_str)
                    .is_some_and(|id| !ids.contains(id));
                if gone {
                    e.set(k, Value::Null);
                }
            }
        }
        if e.kind() == ElementType::Text && e.container_id().is_some_and(|c| !ids.contains(c)) {
            e.set("containerId", Value::Null);
        }
    }
    out
}

/// The drawing's settings from a file's `appState`: the saved keys only.
pub fn restore_app_state(raw: Option<&Value>) -> Props {
    let mut out = Map::new();
    if let Some(state) = raw.and_then(Value::as_object) {
        for k in SAVED_APP_STATE {
            if let Some(v) = state.get(k) {
                out.insert(k.to_string(), v.clone());
            }
        }
    }
    out
}

/// A whole drawing from a file's parts.
pub fn restore_scene(
    elements: &[Value],
    app_state: Option<&Value>,
    files: Option<&Value>,
    fresh: &mut dyn Fresh,
) -> Scene {
    Scene {
        elements: restore_elements(elements, fresh),
        app_state: restore_app_state(app_state),
        files: files
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    }
}

/// New ids like Excalidraw's (21 characters from `A–Z a–z 0–9 _ -`), from a
/// small deterministic generator: [`restore_element`] only needs them for
/// broken drawings, and tests need them repeatable.
pub struct Ids(u64);

impl Ids {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
}

impl Fresh for Ids {
    fn id(&mut self) -> String {
        const ALPHABET: &[u8] = b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
        (0..21)
            .map(|_| {
                // xorshift64*
                self.0 ^= self.0 >> 12;
                self.0 ^= self.0 << 25;
                self.0 ^= self.0 >> 27;
                let r = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
                ALPHABET[(r >> 58) as usize] as char
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(v: Value) -> Element {
        restore_element(v.as_object().unwrap().clone(), &mut Ids::new(7)).unwrap()
    }

    #[test]
    fn defaults_for_missing_properties() {
        let e = restore(json!({"type": "rectangle"}));
        assert_eq!(e.id().len(), 21);
        assert_eq!(e.version(), 1);
        assert_eq!(e.stroke_color(), "#1e1e1e");
        assert_eq!(e.get("roundness"), Some(&Value::Null));
        assert_eq!(e.get("boundElements"), Some(&json!([])));
        assert_eq!(e.get("locked"), Some(&json!(false)));
        assert_eq!(e.get("opacity"), Some(&json!(100)));
    }

    #[test]
    fn keeps_what_it_doesnt_know() {
        let e = restore(
            json!({"id": "a", "type": "rectangle", "customData": {"x": 1}, "nextYear": true}),
        );
        assert_eq!(e.get("customData"), Some(&json!({"x": 1})));
        assert_eq!(e.get("nextYear"), Some(&json!(true)));
        let e = restore(json!({"id": "a", "type": "hexagon", "x": 3}));
        assert_eq!(e.kind(), ElementType::Other("hexagon".into()));
        assert_eq!(e.x(), 3.0);
    }

    #[test]
    fn old_drawings_brought_up_to_date() {
        let e = restore(json!({
            "id": "t", "type": "text", "font": "28px Cascadia", "text": "hi\nthere",
            "height": 70, "rawText": "hi\nthere", "strokeSharpness": "round"
        }));
        assert_eq!(e.font_size(), 28.0);
        assert_eq!(e.font_family(), fonts::CASCADIA);
        assert_eq!(e.line_height(), 1.25);
        assert_eq!(e.get("rawText"), None);
        assert_eq!(e.get("font"), None);
        assert_eq!(
            e.roundness().map(|r| r.kind),
            Some(roundness::PROPORTIONAL_RADIUS)
        );
        let r = restore(
            json!({"id": "r", "type": "rectangle", "strokeSharpness": "round",
            "boundElementIds": ["x"]}),
        );
        assert_eq!(r.roundness().map(|r| r.kind), Some(roundness::LEGACY));
        assert_eq!(r.bound_elements()[0].kind, "arrow");
        let a = restore(json!({"id": "a", "type": "arrow", "points": [[0, 0], [10, 10]]}));
        assert_eq!(a.end_arrowhead(), Some("arrow"));
        assert_eq!(a.start_arrowhead(), None);
        let l = restore(json!({"id": "l", "type": "draw", "points": [[0, 0], [10, 10]]}));
        assert_eq!(l.kind(), ElementType::Line);
    }

    #[test]
    fn points_repaired_and_normalized() {
        let a = restore(json!({"id": "a", "type": "arrow", "x": 10, "y": 10,
            "points": [[5, 5], ["x", 1], [15, 25]]}));
        assert_eq!(a.points(), vec![[0.0, 0.0], [10.0, 20.0]]);
        assert_eq!(
            (a.x(), a.y(), a.width(), a.height()),
            (15.0, 15.0, 10.0, 20.0)
        );
        let a = restore(json!({"id": "a", "type": "line", "width": 30, "height": 40}));
        assert_eq!(a.points(), vec![[0.0, 0.0], [30.0, 40.0]]);
        let huge = restore(json!({"id": "h", "type": "line", "points": [[0, 0], [100000, 0]]}));
        assert!(huge.is_deleted());
        let f = restore(
            json!({"id": "f", "type": "freedraw", "points": [[0, 0], [1, null], [2, 2]],
            "pressures": [0.1, 0.2, 0.3]}),
        );
        assert_eq!(f.points().len(), 2);
        assert_eq!(f.pressures(), vec![0.1, 0.3]);
        assert_eq!(
            f.get("strokeOptions"),
            Some(&json!({"variability": "variable", "streamline": 0.5}))
        );
    }

    #[test]
    fn whole_drawing() {
        let raw = json!([
            {"id": "a", "type": "rectangle", "index": "a1"},
            {"id": "a", "type": "ellipse"},
            {"type": "selection"},
            {"id": "t", "type": "text", "containerId": "gone", "text": "x"},
            {"id": "ar", "type": "arrow", "points": [[0, 0], [1, 1]],
             "startBinding": {"elementId": "a"}, "endBinding": {"elementId": "gone"}},
            "not an element"
        ]);
        let els = restore_elements(raw.as_array().unwrap(), &mut Ids::new(1));
        assert_eq!(els.len(), 4);
        assert_ne!(els[0].id(), els[1].id());
        assert!(els.windows(2).all(|w| w[0].index() < w[1].index()));
        assert_eq!(els[0].index(), Some("a1"));
        assert_eq!(els[2].container_id(), None);
        assert!(els[3].start_binding().is_some());
        assert!(els[3].end_binding().is_none());
    }
}
