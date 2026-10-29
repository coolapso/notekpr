//! Arrows attached to shapes, as Excalidraw binds them (`binding.ts`; MIT,
//! see `PORTED.md`): an end drawn onto a shape (or near its outline)
//! attaches to it at a point of the shape (`fixedPoint`, a fraction of its
//! box). In `orbit` mode the arrow aims at that point and stops at the
//! shape's outline, a small gap away; in `inside` mode it ends at the point.
//! When the shape moves, is resized or turned, its arrows follow.

use serde_json::{Value, json};

use super::hit;
use crate::geom::{self, Point};
use crate::scene::{Element, ElementType, Scene, num};

/// `BASE_BINDING_GAP`.
const GAP: f64 = 5.0;
/// `BASE_ARROW_MIN_LENGTH`.
const MIN_LENGTH: f64 = 10.0;

/// The gap between an arrow and a shape's outline (`getBindingGap`).
pub fn gap(e: &Element) -> f64 {
    GAP + e.stroke_width() / 2.0
}

/// How far from a shape an arrow's end still attaches
/// (`maxBindingDistance_simple`).
pub fn max_distance(zoom: f64) -> f64 {
    let base = GAP.max(15.0);
    let z = if zoom < 1.0 { zoom } else { 1.0 };
    (base / (z * 1.5)).clamp(base, base * 2.0)
}

/// Whether arrows attach to an element.
pub fn bindable(e: &Element) -> bool {
    matches!(
        e.kind(),
        ElementType::Rectangle
            | ElementType::Diamond
            | ElementType::Ellipse
            | ElementType::Text
            | ElementType::Image
            | ElementType::Frame
            | ElementType::Embeddable
            | ElementType::Iframe
            | ElementType::StickyNote
    ) && e.container_id().is_none()
}

fn center(e: &Element) -> Point {
    [e.x() + e.width() / 2.0, e.y() + e.height() / 2.0]
}

/// A fixed point (fractions of the box) on the drawing
/// (`getGlobalFixedPointForBindableElement`).
pub fn fixed_to_global(e: &Element, [fx, fy]: [f64; 2]) -> Point {
    geom::rotate(
        [e.x() + e.width() * fx, e.y() + e.height() * fy],
        center(e),
        e.angle(),
    )
}

/// A point of the drawing as a fixed point of `e`
/// (`calculateFixedPointForNonElbowArrowBinding`).
pub fn global_to_fixed(e: &Element, p: Point) -> [f64; 2] {
    if e.width() < 1.0 || e.height() < 1.0 {
        return [0.5, 0.5];
    }
    let floor = gap(e);
    let [x, y] = geom::rotate(p, center(e), -e.angle());
    let fx = (x - e.x()) / e.width().max(floor);
    let fy = (y - e.y()) / e.height().max(floor);
    // `normalizeFixedPoint`: within reason.
    [fx.clamp(-10.0, 10.0), fy.clamp(-10.0, 10.0)]
}

/// The shape's outline grown by `grow`, in its own (unturned) frame.
fn outline(e: &Element, grow: f64) -> Vec<Point> {
    let (x, y, w, h) = (e.x(), e.y(), e.width(), e.height());
    match e.kind() {
        ElementType::Ellipse => {
            let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0 + grow, h / 2.0 + grow);
            (0..=48)
                .map(|i| {
                    let a = i as f64 / 48.0 * std::f64::consts::TAU;
                    [cx + rx * a.cos(), cy + ry * a.sin()]
                })
                .collect()
        }
        ElementType::Diamond => {
            let [t, r, b, l] = crate::shape::diamond_points(e);
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            // Each corner pushed out from the centre by the gap, about.
            let push = |[px, py]: Point| {
                let (dx, dy) = (x + px - cx, y + py - cy);
                let len = dx.hypot(dy).max(1e-9);
                [
                    x + px + dx / len * grow * std::f64::consts::SQRT_2,
                    y + py + dy / len * grow * std::f64::consts::SQRT_2,
                ]
            };
            let pts = [push(t), push(r), push(b), push(l)];
            vec![pts[0], pts[1], pts[2], pts[3], pts[0]]
        }
        _ => vec![
            [x - grow, y - grow],
            [x + w + grow, y - grow],
            [x + w + grow, y + h + grow],
            [x - grow, y + h + grow],
            [x - grow, y - grow],
        ],
    }
}

fn segment_intersection(a: Point, b: Point, c: Point, d: Point) -> Option<Point> {
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let denom = r[0] * s[1] - r[1] * s[0];
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = ((c[0] - a[0]) * s[1] - (c[1] - a[1]) * s[0]) / denom;
    let u = ((c[0] - a[0]) * r[1] - (c[1] - a[1]) * r[0]) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u))
        .then(|| [a[0] + r[0] * t, a[1] + r[1] * t])
}

/// Where a segment crosses the shape's outline grown by the gap
/// (`intersectElementWithLineSegment`), in drawing coordinates.
fn crossings(e: &Element, a: Point, b: Point) -> Vec<Point> {
    let c = center(e);
    let (la, lb) = (
        geom::rotate(a, c, -e.angle()),
        geom::rotate(b, c, -e.angle()),
    );
    outline(e, gap(e))
        .windows(2)
        .filter_map(|w| segment_intersection(la, lb, w[0], w[1]))
        .map(|p| geom::rotate(p, c, e.angle()))
        .collect()
}

/// The binding of an arrow's end, as stored.
struct Bound {
    id: String,
    fixed: [f64; 2],
    inside: bool,
}

fn bound(arrow: &Element, key: &str) -> Option<Bound> {
    let b = arrow.get(key)?.as_object()?;
    let id = b.get("elementId")?.as_str()?.to_string();
    let fixed = b
        .get("fixedPoint")
        .and_then(Value::as_array)
        .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]))
        .unwrap_or([0.5, 0.5]);
    let inside = b.get("mode").and_then(Value::as_str) == Some("inside");
    Some(Bound { id, fixed, inside })
}

/// Where an arrow's end goes for its binding to `target` (`updateBoundPoint`,
/// in short): `inside` at the point; `orbit` aimed at the point, on the
/// outline, a gap away.
fn bound_end(
    arrow: &Element,
    start: bool,
    target: &Element,
    scene: &Scene,
    global: &[Point],
) -> Option<Point> {
    let b = bound(arrow, if start { "startBinding" } else { "endBinding" })?;
    let focus = fixed_to_global(target, b.fixed);
    if b.inside {
        return Some(focus);
    }
    // Aimed from the other end: its focus if it is bound too (two points),
    // else the arrow's next point.
    let n = global.len();
    let other_key = if start { "endBinding" } else { "startBinding" };
    let other_focus = (n == 2)
        .then(|| bound(arrow, other_key))
        .flatten()
        .and_then(|o| {
            scene
                .element(&o.id)
                .filter(|e| !e.is_deleted())
                .map(|e| fixed_to_global(e, o.fixed))
        });
    let other = other_focus.unwrap_or(if start { global[1] } else { global[n - 2] });
    let mut hits = crossings(target, focus, other);
    hits.sort_by(|a, b| geom::distance(*a, other).total_cmp(&geom::distance(*b, other)));
    let out = hits.first().copied();
    // Too short an arrow would turn inside out: to the focus point.
    match out {
        Some(p) if geom::distance(p, other) > MIN_LENGTH => Some(p),
        _ => Some(focus),
    }
}

/// The arrow with its ends where their bindings put them.
pub fn follow(arrow: &Element, scene: &Scene, moved: &[Element]) -> Element {
    let find = |id: &str| -> Option<Element> {
        moved
            .iter()
            .find(|e| e.id() == id)
            .cloned()
            .or_else(|| scene.element(id).cloned())
            .filter(|e| !e.is_deleted())
    };
    let mut global: Vec<Point> = arrow
        .points()
        .iter()
        .map(|[x, y]| [arrow.x() + x, arrow.y() + y])
        .collect();
    if global.len() < 2 {
        return arrow.clone();
    }
    // A scene with the moved elements, for the other end's lookup.
    let mut s = scene.clone();
    for m in moved {
        if let Some(slot) = s.elements.iter_mut().find(|e| e.id() == m.id()) {
            *slot = m.clone();
        }
    }
    let mut changed = false;
    for start in [true, false] {
        let key = if start { "startBinding" } else { "endBinding" };
        let Some(target) = bound(arrow, key).and_then(|b| find(&b.id)) else {
            continue;
        };
        if let Some(p) = bound_end(arrow, start, &target, &s, &global) {
            let i = if start { 0 } else { global.len() - 1 };
            global[i] = p;
            changed = true;
        }
    }
    let mut a = arrow.clone();
    if changed {
        super::new::normalize_points(&mut a, &global);
    }
    a
}

/// Attaches an arrow's end at `p` to what is there, if something bindable
/// is (`bindOrUnbindBindingElement`): returns the arrow and the shape (with
/// the arrow among its bound elements), or just the arrow unbound.
pub fn attach(
    arrow: &Element,
    start: bool,
    p: Point,
    scene: &Scene,
    zoom: f64,
) -> (Element, Option<Element>) {
    let key = if start { "startBinding" } else { "endBinding" };
    let mut a = arrow.clone();
    let reach = max_distance(zoom);
    let target = scene
        .elements
        .iter()
        .rev()
        .filter(|e| !e.is_deleted() && !e.is_locked() && e.id() != arrow.id() && bindable(e))
        .find(|e| {
            hit::hits(e, p, zoom) || {
                // Inside it, filled or not, or near its outline.
                let [lx, ly] = hit::local(e, p);
                let (w, h) = (e.width(), e.height());
                lx >= -reach && ly >= -reach && lx <= w + reach && ly <= h + reach
            }
        });
    let Some(t) = target else {
        a.set(key, Value::Null);
        return (a, None);
    };
    a.set(
        key,
        json!({"elementId": t.id(), "fixedPoint": global_to_fixed(t, p).map(num), "mode": "orbit"}),
    );
    let mut shape = t.clone();
    let mut list: Vec<Value> = t
        .bound_elements()
        .iter()
        .filter(|b| b.id != arrow.id())
        .map(|b| json!({"id": b.id, "type": b.kind}))
        .collect();
    list.push(json!({"id": arrow.id(), "type": "arrow"}));
    shape.set("boundElements", Value::Array(list));
    (a, Some(shape))
}

/// The arrows attached to any of `ids`, not among `ids` themselves.
pub fn arrows_of<'a>(scene: &'a Scene, ids: &[&str]) -> Vec<&'a Element> {
    scene
        .visible()
        .filter(|e| e.kind() == ElementType::Arrow && !ids.contains(&e.id()))
        .filter(|e| {
            ["startBinding", "endBinding"]
                .iter()
                .any(|k| bound(e, k).is_some_and(|b| ids.contains(&b.id.as_str())))
        })
        .collect()
}

/// Arrows follow elements that changed (among `changed`); arrows attached
/// to deleted ones let go.
pub fn update_arrows(scene: &Scene, changed: &mut Vec<Element>) {
    let ids: Vec<String> = changed.iter().map(|e| e.id().to_string()).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let deleted: Vec<&str> = changed
        .iter()
        .filter(|e| e.is_deleted())
        .map(Element::id)
        .collect::<Vec<_>>();
    let deleted: Vec<String> = deleted.iter().map(|s| s.to_string()).collect();
    for arrow in arrows_of(scene, &refs) {
        let mut a = arrow.clone();
        for k in ["startBinding", "endBinding"] {
            if bound(&a, k).is_some_and(|b| deleted.contains(&b.id)) {
                a.set(k, Value::Null);
            }
        }
        let a = follow(&a, scene, changed);
        changed.push(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(v: serde_json::Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    fn scene() -> Scene {
        Scene {
            elements: vec![
                el(
                    json!({"id": "a", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 100, "strokeWidth": 2}),
                ),
                el(
                    json!({"id": "b", "type": "rectangle", "x": 300, "y": 0, "width": 100, "height": 100, "strokeWidth": 2}),
                ),
                el(
                    json!({"id": "arr", "type": "arrow", "x": 50, "y": 50, "width": 300, "height": 0,
                    "points": [[0, 0], [300, 0]], "seed": 1}),
                ),
            ],
            ..Scene::default()
        }
    }

    #[test]
    fn ends_attach_and_stop_at_the_outline() {
        let s = scene();
        let arrow = s.element("arr").unwrap();
        let (a, shape) = attach(arrow, true, [50.0, 50.0], &s, 1.0);
        assert_eq!(shape.unwrap().bound_elements()[0].id, "arr");
        let (a, _) = attach(&a, false, [350.0, 50.0], &s, 1.0);
        let b = a.start_binding().unwrap();
        assert_eq!(
            (b.element_id.as_str(), b.fixed_point),
            ("a", Some([0.5, 0.5]))
        );
        let a = follow(&a, &s, &[]);
        let pts = a.points();
        // Out of the left box by its gap (5 + 1), into the right one likewise.
        assert!((a.x() - 106.0).abs() < 1e-6, "{}", a.x());
        assert!((a.x() + pts[1][0] - 294.0).abs() < 1e-6);
        // Nothing there: unbound.
        let (free, none) = attach(&a, false, [600.0, 600.0], &s, 1.0);
        assert!(none.is_none() && free.end_binding().is_none());
    }

    #[test]
    fn arrows_follow_moved_shapes() {
        let mut s = scene();
        let arrow = s.element("arr").unwrap().clone();
        let (a, _) = attach(&arrow, true, [50.0, 50.0], &s, 1.0);
        let (a, _) = attach(&a, false, [350.0, 50.0], &s, 1.0);
        let a = follow(&a, &s, &[]);
        s.elements[2] = a;
        // The right box goes down 200.
        let mut moved = s.element("b").unwrap().clone();
        moved.set("y", json!(200));
        let mut changed = vec![moved];
        update_arrows(&s, &mut changed);
        let a = changed.iter().find(|e| e.id() == "arr").unwrap();
        let end = [a.x() + a.points()[1][0], a.y() + a.points()[1][1]];
        assert!(end[1] > 150.0 && end[1] < 260.0, "{end:?}");
        // Deleting it lets the arrow go.
        let mut gone = s.element("b").unwrap().clone();
        gone.set("isDeleted", json!(true));
        let mut changed = vec![gone];
        update_arrows(&s, &mut changed);
        assert!(changed[1].end_binding().is_none());
        assert!(changed[1].start_binding().is_some());
    }

    #[test]
    fn inside_mode_ends_at_the_point() {
        let s = scene();
        let mut a = s.element("arr").unwrap().clone();
        a.set(
            "endBinding",
            json!({"elementId": "b", "fixedPoint": [0.25, 0.75], "mode": "inside"}),
        );
        let a = follow(&a, &s, &[]);
        let end = [a.x() + a.points()[1][0], a.y() + a.points()[1][1]];
        assert_eq!(end, [325.0, 75.0]);
    }
}
