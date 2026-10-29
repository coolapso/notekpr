//! Where elements are: their boxes, rotated, and the box around a whole
//! drawing — as Excalidraw computes them (`bounds.ts`; MIT, see
//! `PORTED.md`). A line's or arrow's box is that of the curve rough draws
//! for it, so exports frame a drawing exactly as Excalidraw does.

use crate::geom::{self, Point};
use crate::js;
use crate::rough::{Op, OpSetType, OpType};
use crate::scene::{Element, ElementType, Scene};
use crate::shape::{self, ElementShape, RenderConfig};

/// `[min x, min y, max x, max y]`.
pub type Bounds = [f64; 4];

/// `getBezierValueForT`.
fn bezier_at(t: f64, p0: f64, p1: f64, p2: f64, p3: f64) -> f64 {
    let u = 1.0 - t;
    u.powf(3.0) * p0 + 3.0 * (u * u) * t * p1 + 3.0 * u * (t * t) * p2 + t.powf(3.0) * p3
}

/// `solveQuadratic`: where a cubic Bézier turns along one axis.
fn turning_values(p0: f64, p1: f64, p2: f64, p3: f64) -> Option<[Option<f64>; 2]> {
    let i = p1 - p0;
    let j = p2 - p1;
    let k = p3 - p2;
    let a = 3.0 * i - 6.0 * j + 3.0 * k;
    let b = 6.0 * j - 6.0 * i;
    let c = 3.0 * i;
    let sqrt_part = b * b - 4.0 * a * c;
    if sqrt_part < 0.0 || sqrt_part.is_nan() {
        return None;
    }
    let (t1, t2) = if a == 0.0 {
        (-c / b, -c / b)
    } else {
        (
            (-b + sqrt_part.sqrt()) / (2.0 * a),
            (-b - sqrt_part.sqrt()) / (2.0 * a),
        )
    };
    let at = |t: f64| {
        (0.0..=1.0)
            .contains(&t)
            .then(|| bezier_at(t, p0, p1, p2, p3))
    };
    Some([at(t1), at(t2)])
}

/// `getCubicBezierCurveBound`.
fn cubic_bounds(p0: Point, p1: Point, p2: Point, p3: Point) -> Bounds {
    let mut min_x = js::min(p0[0], p3[0]);
    let mut max_x = js::max(p0[0], p3[0]);
    if let Some(xs) = turning_values(p0[0], p1[0], p2[0], p3[0]) {
        for x in xs.into_iter().flatten() {
            min_x = js::min(min_x, x);
            max_x = js::max(max_x, x);
        }
    }
    let mut min_y = js::min(p0[1], p3[1]);
    let mut max_y = js::max(p0[1], p3[1]);
    if let Some(ys) = turning_values(p0[1], p1[1], p2[1], p3[1]) {
        for y in ys.into_iter().flatten() {
            min_y = js::min(min_y, y);
            max_y = js::max(max_y, y);
        }
    }
    [min_x, min_y, max_x, max_y]
}

/// `getMinMaxXYFromCurvePathOps`: the box around the curves of a path
/// (straight segments don't count, as in Excalidraw).
pub fn curve_ops_bounds(ops: &[Op], transform: Option<&dyn Fn(Point) -> Point>) -> Bounds {
    let mut current = [0.0, 0.0];
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let t = |p: Point| transform.map_or(p, |f| f(p));
    for op in ops {
        match op.op {
            OpType::Move => current = [op.data[0], op.data[1]],
            OpType::BCurveTo => {
                let d = &op.data;
                let (p1, p2, raw3) = ([d[0], d[1]], [d[2], d[3]], [d[4], d[5]]);
                let b = cubic_bounds(t(current), t(p1), t(p2), t(raw3));
                current = raw3;
                out[0] = js::min(out[0], b[0]);
                out[1] = js::min(out[1], b[1]);
                out[2] = js::max(out[2], b[2]);
                out[3] = js::max(out[3], b[3]);
            }
            OpType::LineTo => {}
        }
    }
    out
}

/// `getBoundsFromPoints`.
pub fn points_bounds(points: &[Point]) -> Bounds {
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [x, y] in points {
        out[0] = js::min(out[0], *x);
        out[1] = js::min(out[1], *y);
        out[2] = js::max(out[2], *x);
        out[3] = js::max(out[3], *y);
    }
    out
}

/// The operations of the curve rough draws for a line or arrow.
fn shaft_ops(e: &Element) -> Vec<Op> {
    match shape::element_shape(e, &RenderConfig::default()) {
        ElementShape::Rough(shapes) => shapes
            .first()
            .and_then(|d| {
                d.sets
                    .iter()
                    .find(|s| s.kind == OpSetType::Path)
                    .or(d.sets.first())
            })
            .map(|s| s.ops.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `getElementAbsoluteCoords`: the element's box before rotation, and its
/// centre: `[x1, y1, x2, y2, cx, cy]`.
pub fn absolute_coords(e: &Element) -> [f64; 6] {
    match e.kind() {
        ElementType::Freedraw => {
            let [min_x, min_y, max_x, max_y] = points_bounds(&e.points());
            let (x1, y1, x2, y2) = (min_x + e.x(), min_y + e.y(), max_x + e.x(), max_y + e.y());
            [x1, y1, x2, y2, (x1 + x2) / 2.0, (y1 + y2) / 2.0]
        }
        ElementType::Arrow | ElementType::Line => {
            let [min_x, min_y, max_x, max_y] = curve_ops_bounds(&shaft_ops(e), None);
            let (x1, y1, x2, y2) = (min_x + e.x(), min_y + e.y(), max_x + e.x(), max_y + e.y());
            [x1, y1, x2, y2, (x1 + x2) / 2.0, (y1 + y2) / 2.0]
        }
        _ => [
            e.x(),
            e.y(),
            e.x() + e.width(),
            e.y() + e.height(),
            e.x() + e.width() / 2.0,
            e.y() + e.height() / 2.0,
        ],
    }
}

fn min4(a: f64, b: f64, c: f64, d: f64) -> f64 {
    js::min(js::min(js::min(a, b), c), d)
}

fn max4(a: f64, b: f64, c: f64, d: f64) -> f64 {
    js::max(js::max(js::max(a, b), c), d)
}

/// `ElementBounds.calculateBounds`: the box around the element as drawn,
/// rotated.
pub fn element_bounds(e: &Element) -> Bounds {
    let [x1, y1, x2, y2, cx, cy] = absolute_coords(e);
    let angle = e.angle();
    match e.kind() {
        ElementType::Freedraw => {
            let center = [cx - e.x(), cy - e.y()];
            let rotated: Vec<Point> = e
                .points()
                .into_iter()
                .map(|p| geom::rotate(p, center, angle))
                .collect();
            let [min_x, min_y, max_x, max_y] = points_bounds(&rotated);
            [min_x + e.x(), min_y + e.y(), max_x + e.x(), max_y + e.y()]
        }
        ElementType::Arrow | ElementType::Line => {
            let points = e.points();
            if points.len() < 2 {
                let [px, py] = points.first().copied().unwrap_or([0.0, 0.0]);
                let [x, y] = geom::rotate([e.x() + px, e.y() + py], [cx, cy], angle);
                return [x, y, x, y];
            }
            let (ex, ey) = (e.x(), e.y());
            let transform = move |[x, y]: Point| geom::rotate([ex + x, ey + y], [cx, cy], angle);
            curve_ops_bounds(&shaft_ops(e), Some(&transform))
        }
        ElementType::Diamond => {
            let [x11, y11] = geom::rotate([cx, y1], [cx, cy], angle);
            let [x12, y12] = geom::rotate([cx, y2], [cx, cy], angle);
            let [x22, y22] = geom::rotate([x1, cy], [cx, cy], angle);
            let [x21, y21] = geom::rotate([x2, cy], [cx, cy], angle);
            [
                min4(x11, x12, x22, x21),
                min4(y11, y12, y22, y21),
                max4(x11, x12, x22, x21),
                max4(y11, y12, y22, y21),
            ]
        }
        ElementType::Ellipse => {
            let w = (x2 - x1) / 2.0;
            let h = (y2 - y1) / 2.0;
            let (cos, sin) = (js::cos(angle), js::sin(angle));
            let ww = js::hypot(w * cos, h * sin);
            let hh = js::hypot(h * cos, w * sin);
            [cx - ww, cy - hh, cx + ww, cy + hh]
        }
        _ => {
            let [x11, y11] = geom::rotate([x1, y1], [cx, cy], angle);
            let [x12, y12] = geom::rotate([x1, y2], [cx, cy], angle);
            let [x22, y22] = geom::rotate([x2, y2], [cx, cy], angle);
            let [x21, y21] = geom::rotate([x2, y1], [cx, cy], angle);
            [
                min4(x11, x12, x22, x21),
                min4(y11, y12, y22, y21),
                max4(x11, x12, x22, x21),
                max4(y11, y12, y22, y21),
            ]
        }
    }
}

/// `getCommonBounds`: the box around all of `elements` (zeros for none).
pub fn common_bounds<'a>(elements: impl IntoIterator<Item = &'a Element>) -> Bounds {
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut any = false;
    for e in elements {
        any = true;
        let b = element_bounds(e);
        out[0] = js::min(out[0], b[0]);
        out[1] = js::min(out[1], b[1]);
        out[2] = js::max(out[2], b[2]);
        out[3] = js::max(out[3], b[3]);
    }
    if any { out } else { [0.0; 4] }
}

/// The elements an export frames (`getRootElements`): all of them but those
/// inside a frame that is there (the frame frames them).
pub fn root_elements(scene: &Scene) -> Vec<&Element> {
    let frames: std::collections::HashSet<&str> = scene
        .visible()
        .filter(|e| matches!(e.kind(), ElementType::Frame | ElementType::MagicFrame))
        .map(Element::id)
        .collect();
    scene
        .visible()
        .filter(|e| e.frame_id().is_none_or(|f| !frames.contains(f)) || frames.contains(e.id()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn el(v: serde_json::Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    #[test]
    fn boxes_rotated() {
        let r =
            el(json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 10, "height": 20}));
        assert_eq!(element_bounds(&r), [0.0, 0.0, 10.0, 20.0]);
        let turned = el(
            json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 10, "height": 20,
            "angle": std::f64::consts::FRAC_PI_2}),
        );
        let b = element_bounds(&turned);
        assert!(
            (b[0] - -5.0).abs() < 1e-9 && (b[2] - 15.0).abs() < 1e-9,
            "{b:?}"
        );
        let e =
            el(json!({"id": "e", "type": "ellipse", "x": 0, "y": 0, "width": 10, "height": 20}));
        assert_eq!(element_bounds(&e), [0.0, 0.0, 10.0, 20.0]);
        assert_eq!(common_bounds([&r, &e]), [0.0, 0.0, 10.0, 20.0]);
        assert_eq!(common_bounds(std::iter::empty()), [0.0; 4]);
    }

    #[test]
    fn curve_bounds_include_bulges() {
        // A curve from (0,0) to (10,0) bulging down.
        let ops = vec![
            Op {
                op: OpType::Move,
                data: vec![0.0, 0.0],
            },
            Op {
                op: OpType::BCurveTo,
                data: vec![0.0, 10.0, 10.0, 10.0, 10.0, 0.0],
            },
        ];
        let b = curve_ops_bounds(&ops, None);
        assert_eq!((b[0], b[2]), (0.0, 10.0));
        assert!((b[3] - 7.5).abs() < 1e-9, "{b:?}");
    }
}
