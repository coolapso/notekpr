//! Elbow arrows, routed at right angles as Excalidraw routes them
//! (`elbowArrow.ts`, with what it needs of `heading.ts`, `binding.ts` and
//! `distance.ts`; MIT, see `PORTED.md`).
//!
//! An elbow arrow's points are computed, not drawn: from where its ends are
//! (on the outlines of the shapes they are attached to, a gap away) and
//! which way they leave them, a route with one or two corners if one fits,
//! else an A* search over a grid made of the padded boxes around the shapes,
//! finds the path with the fewest turns. Segments the user moved
//! (`fixedSegments`) stay where they were put; the rest of the route
//! changes around them.
//!
//! Routing works as Excalidraw's does with the zoom at 1 (its routing never
//! gets the zoom), so the same drawing routes the same in both.

use serde_json::{Value, json};

use super::{binding, hit};
use crate::bounds;
use crate::geom::{self, Point};
use crate::js;
use crate::scene::{Element, ElementType, Scene, num};
use crate::shape::{self, ELBOW_ARROW_CORNER_RADIUS};

/// A direction an arrow leaves or enters in: right, down, left or up.
type Heading = Point;
const RIGHT: Heading = [1.0, 0.0];
const DOWN: Heading = [0.0, 1.0];
const LEFT: Heading = [-1.0, 0.0];
const UP: Heading = [0.0, -1.0];

/// `[min x, min y, max x, max y]`.
type Bounds = [f64; 4];

/// `BASE_PADDING`: how far a route keeps from the shapes it connects.
const BASE_PADDING: f64 = 40.0;
/// `DEDUP_TRESHOLD`: segments shorter than this go.
const DEDUP_THRESHOLD: f64 = 1.0;
/// `MAX_POS`: coordinates are kept within this.
const MAX_POS: f64 = 1e6;
/// `BASE_BINDING_GAP`.
const BASE_GAP: f64 = 5.0;
/// `PRECISION`.
const PRECISION: f64 = 10e-5;
/// `POINT_HANDLE_SIZE`: the handles on an elbow arrow's segments.
const HANDLE_SIZE: f64 = 10.0;

// Headings (`heading.ts`).

/// `vectorToHeading`.
fn heading_of(v: Point) -> Heading {
    geom::heading(v, [0.0, 0.0])
}

/// `flipHeading`.
fn flip(h: Heading) -> Heading {
    let f = |x: f64| if x == 0.0 { 0.0 } else { -x.signum() };
    [f(h[0]), f(h[1])]
}

/// `headingIsHorizontal`.
fn horizontal(h: Heading) -> bool {
    h == RIGHT || h == LEFT
}

/// `vectorCross`.
fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - b[0] * a[1]
}

/// `pointScaleFromOrigin`: `p` moved away from `mid` `by` times as far.
fn scale_from(p: Point, mid: Point, by: f64) -> Point {
    [mid[0] + (p[0] - mid[0]) * by, mid[1] + (p[1] - mid[1]) * by]
}

/// `triangleIncludesPoint`.
fn in_triangle([a, b, c]: [Point; 3], p: Point) -> bool {
    let sign = |p1: Point, p2: Point, p3: Point| {
        (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
    };
    let (d1, d2, d3) = (sign(p, a, b), sign(p, b, c), sign(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

// Boxes.

/// `getCenterForBounds`.
fn bounds_center(b: Bounds) -> Point {
    [b[0] + (b[2] - b[0]) / 2.0, b[1] + (b[3] - b[1]) / 2.0]
}

/// `elementCenterPoint`, for shapes.
fn element_center(e: &Element) -> Point {
    bounds_center(bounds::element_bounds(e))
}

/// `aabbForElement`: the box around the turned shape, grown by `offset`
/// (top, right, bottom, left).
fn aabb(e: &Element, offset: Option<[f64; 4]>) -> Bounds {
    let (x, y, w, h) = (e.x(), e.y(), e.width(), e.height());
    let c = element_center(e);
    let corners =
        [[x, y], [x + w, y], [x + w, y + h], [x, y + h]].map(|p| geom::rotate(p, c, e.angle()));
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [px, py] in corners {
        b = [
            js::min(b[0], px),
            js::min(b[1], py),
            js::max(b[2], px),
            js::max(b[3], py),
        ];
    }
    match offset {
        Some([top, right, down, left]) => [b[0] - left, b[1] - top, b[2] + right, b[3] + down],
        None => b,
    }
}

/// `pointInsideBounds` (the edges are outside).
fn inside(p: Point, b: Bounds) -> bool {
    p[0] > b[0] && p[0] < b[2] && p[1] > b[1] && p[1] < b[3]
}

/// `doBoundsIntersect`.
fn overlap(a: Bounds, b: Bounds) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

/// `segmentCrossesBounds`: an upright or level segment through a box.
fn crosses(a: Point, b: Point, bounds: Bounds) -> bool {
    overlap(
        [
            js::min(a[0], b[0]),
            js::min(a[1], b[1]),
            js::max(a[0], b[0]),
            js::max(a[1], b[1]),
        ],
        bounds,
    )
}

/// `commonAABB`.
fn common(boxes: &[Bounds]) -> Bounds {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for x in boxes {
        b = [
            js::min(b[0], x[0]),
            js::min(b[1], x[1]),
            js::max(b[2], x[2]),
            js::max(b[3], x[3]),
        ];
    }
    b
}

/// `offsetFromHeading`: `head` ahead, `side` elsewhere (top, right, bottom,
/// left).
fn offset_from_heading(h: Heading, head: f64, side: f64) -> [f64; 4] {
    if h == UP {
        [head, side, side, side]
    } else if h == RIGHT {
        [side, head, side, side]
    } else if h == DOWN {
        [side, side, head, side]
    } else {
        [side, side, side, head]
    }
}

/// `pointsEqual`.
fn same_point(a: Point, b: Point) -> bool {
    (a[0] - b[0]).abs() < PRECISION && (a[1] - b[1]).abs() < PRECISION
}

/// `validateElbowPoints`: every segment upright or level.
fn orthogonal(points: &[Point]) -> bool {
    points.windows(2).all(|w| {
        (w[1][0] - w[0][0]).abs() < DEDUP_THRESHOLD || (w[1][1] - w[0][1]).abs() < DEDUP_THRESHOLD
    })
}

// Shapes (`distance.ts`, `heading.ts`, `binding.ts`).

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let ab = geom::vector(b, a);
    let len2 = geom::dot(ab, ab);
    let t = if len2 == 0.0 {
        0.0
    } else {
        (geom::dot(geom::vector(p, a), ab) / len2).clamp(0.0, 1.0)
    };
    geom::distance(p, [a[0] + t * ab[0], a[1] + t * ab[1]])
}

/// `ellipseDistanceFromPoint`.
fn ellipse_distance(p: Point, center: Point, a: f64, b: f64) -> f64 {
    let t = [p[0] - center[0], p[1] - center[1]];
    if a == b {
        return (js::hypot(t[0], t[1]) - a).abs();
    }
    let (px, py) = (t[0].abs(), t[1].abs());
    let (mut tx, mut ty) = (0.707, 0.707);
    for _ in 0..3 {
        let (x, y) = (a * tx, b * ty);
        let ex = ((a * a - b * b) * tx.powf(3.0)) / a;
        let ey = ((b * b - a * a) * ty.powf(3.0)) / b;
        let (rx, ry) = (x - ex, y - ey);
        let (qx, qy) = (px - ex, py - ey);
        let r = js::hypot(ry, rx);
        let q = js::hypot(qy, qx);
        if q == 0.0 {
            break;
        }
        tx = js::min(1.0, js::max(0.0, ((qx * r) / q + ex) / a));
        ty = js::min(1.0, js::max(0.0, ((qy * r) / q + ey) / b));
        let s = js::hypot(ty, tx);
        tx /= s;
        ty /= s;
    }
    let closest = [
        a * tx * if t[0] < 0.0 { -1.0 } else { 1.0 },
        b * ty * if t[1] < 0.0 { -1.0 } else { 1.0 },
    ];
    geom::distance(t, closest)
}

/// How far `p` is from a shape's outline (`distanceToElement`; rounded
/// corners are taken as sharp).
fn distance_to(e: &Element, p: Point) -> f64 {
    let [lx, ly] = hit::local(e, p);
    let (w, h) = (e.width(), e.height());
    let sides = |c: [Point; 4]| {
        (0..4)
            .map(|i| segment_distance([lx, ly], c[i], c[(i + 1) % 4]))
            .fold(f64::INFINITY, js::min)
    };
    match e.kind() {
        ElementType::Ellipse => ellipse_distance([lx, ly], [w / 2.0, h / 2.0], w / 2.0, h / 2.0),
        ElementType::Diamond => sides(shape::diamond_points(e)),
        _ => sides([[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]]),
    }
}

/// `headingForPointFromDiamondElement`.
fn heading_from_diamond(e: &Element, aabb: Bounds, p: Point) -> Heading {
    let mid = bounds_center(aabb);
    let (x, y, w, h, angle) = (e.x(), e.y(), e.width(), e.height(), e.angle());
    let shrunk = |q: Point| scale_from(geom::rotate(q, mid, angle), mid, 0.95);
    let top = shrunk([x + w / 2.0, y]);
    let right = shrunk([x + w, y + h / 2.0]);
    let bottom = shrunk([x + w / 2.0, y + h]);
    let left = shrunk([x, y + h / 2.0]);
    let v = geom::vector;
    // Corners.
    if cross(v(p, top), v(top, right)) <= 0.0 && cross(v(p, top), v(top, left)) > 0.0 {
        return geom::heading(top, mid);
    } else if cross(v(p, right), v(right, bottom)) <= 0.0 && cross(v(p, right), v(right, top)) > 0.0
    {
        return geom::heading(right, mid);
    } else if cross(v(p, bottom), v(bottom, left)) <= 0.0
        && cross(v(p, bottom), v(bottom, right)) > 0.0
    {
        return geom::heading(bottom, mid);
    } else if cross(v(p, left), v(left, top)) <= 0.0 && cross(v(p, left), v(left, bottom)) > 0.0 {
        return geom::heading(left, mid);
    }
    // Sides.
    let wide = w > h;
    let pm = v(p, mid);
    let q = if cross(pm, v(top, mid)) <= 0.0 && cross(pm, v(right, mid)) > 0.0 {
        if wide { top } else { right }
    } else if cross(pm, v(right, mid)) <= 0.0 && cross(pm, v(bottom, mid)) > 0.0 {
        if wide { bottom } else { right }
    } else if cross(pm, v(bottom, mid)) <= 0.0 && cross(pm, v(left, mid)) > 0.0 {
        if wide { bottom } else { left }
    } else if wide {
        top
    } else {
        left
    };
    geom::heading(q, mid)
}

/// Which side of a shape `p` is on (`headingForPointFromElement`): four
/// cones from the centre of `aabb`, twice its size.
fn heading_from_element(e: &Element, aabb: Bounds, p: Point) -> Heading {
    if e.kind() == ElementType::Diamond {
        return heading_from_diamond(e, aabb, p);
    }
    let mid = bounds_center(aabb);
    let tl = scale_from([aabb[0], aabb[1]], mid, 2.0);
    let tr = scale_from([aabb[2], aabb[1]], mid, 2.0);
    let bl = scale_from([aabb[0], aabb[3]], mid, 2.0);
    let br = scale_from([aabb[2], aabb[3]], mid, 2.0);
    if in_triangle([tl, tr, mid], p) {
        UP
    } else if in_triangle([tr, br, mid], p) {
        RIGHT
    } else if in_triangle([br, bl, mid], p) {
        DOWN
    } else {
        LEFT
    }
}

/// The way an end at `p` leaves `hovered` (`getBindPointHeading`): the
/// side it is on, or, far from the shape, away from its centre; unattached,
/// towards the other end.
fn bind_heading(p: Point, other: Point, hovered: Option<&Element>, orig: Point) -> Heading {
    let Some(e) = hovered else {
        return heading_of(geom::vector(other, p));
    };
    let d = distance_to(e, p);
    let grown = aabb(e, Some([d; 4]));
    let distance = distance_to(e, orig);
    if distance > binding::max_distance(1.0) || !js::truthy(distance) {
        return heading_of(geom::vector(p, element_center(e)));
    }
    heading_from_element(e, grown, p)
}

/// The middles of a shape's sides: right, bottom, left, top
/// (`getAllMidpoints`; a diamond's, on its rounded corners).
fn midpoints(e: &Element) -> [Point; 4] {
    let (x, y, w, h) = (e.x(), e.y(), e.width(), e.height());
    let c = element_center(e);
    let at = |p: Point| geom::rotate(p, c, e.angle());
    if e.kind() == ElementType::Diamond {
        // `getDiamondBaseCorners`, each corner's curve at its middle.
        let [t, r, b, l] = shape::diamond_points(e);
        let (vr, hr) = if e.roundness().is_some() {
            (
                shape::corner_radius((t[0] - l[0]).abs(), e),
                shape::corner_radius((r[1] - t[1]).abs(), e),
            )
        } else {
            ((t[0] - l[0]) * 0.01, (r[1] - t[1]) * 0.01)
        };
        // A curve from `v + a` through the corner `v` twice to `v + d`, at
        // its middle (`bezierEquation` at 0.5).
        let corner = |[px, py]: Point, a: Point, d: Point| {
            let v = [x + px, y + py];
            let (p0, p3) = ([v[0] + a[0], v[1] + a[1]], [v[0] + d[0], v[1] + d[1]]);
            let mid = |i: usize| 0.125 * p0[i] + 0.375 * v[i] + 0.375 * v[i] + 0.125 * p3[i];
            at([mid(0), mid(1)])
        };
        return [
            corner(r, [-vr, -hr], [-vr, hr]),
            corner(b, [vr, -hr], [-vr, -hr]),
            corner(l, [vr, hr], [vr, -hr]),
            corner(t, [-vr, hr], [vr, hr]),
        ];
    }
    [
        at([x + w, y + h / 2.0]),
        at([x + w / 2.0, y + h]),
        at([x, y + h / 2.0]),
        at([x + w / 2.0, y]),
    ]
}

/// A side's middle near `p`, for an end to snap to
/// (`getElbowArrowSnapMidPoint`): the point and whether it is on one of the
/// shape's axes.
fn snap_midpoint(e: &Element, p: Point) -> Option<(Point, bool)> {
    let max = binding::max_distance(1.0) + e.stroke_width() / 2.0;
    let clamp = |v: f64| js::min(js::max(v, 5.0), max);
    let (h_reach, v_reach) = (clamp(0.05 * e.width()), clamp(0.05 * e.height()));
    let (x, y, w, h, angle) = (e.x(), e.y(), e.width(), e.height(), e.angle());
    let center = element_center(e);
    let [px, py] = geom::rotate(p, center, -angle);
    let gap = binding::gap(e);
    if geom::distance(center, [px, py]) < gap {
        return None;
    }
    let [right, bottom, left, top] = midpoints(e);
    let level = py > center[1] - v_reach && py < center[1] + v_reach;
    let upright = px > center[0] - h_reach && px < center[0] + h_reach;
    if px <= x + w / 2.0 && level {
        return Some((left, true));
    } else if py <= y + h / 2.0 && upright {
        return Some((top, true));
    } else if px >= x + w / 2.0 && level {
        return Some((right, true));
    } else if py >= y + h / 2.0 && upright {
        return Some((bottom, true));
    }
    if e.kind() == ElementType::Diamond {
        // The middles of a diamond's edges, a gap outside them.
        let reach = js::max(h_reach, v_reach);
        for (ex, ey, dx, dy) in [
            (x + w / 4.0, y + h / 4.0, -1.0, -1.0),
            (x + (3.0 * w) / 4.0, y + h / 4.0, 1.0, -1.0),
            (x + w / 4.0, y + (3.0 * h) / 4.0, -1.0, 1.0),
            (x + (3.0 * w) / 4.0, y + (3.0 * h) / 4.0, 1.0, 1.0),
        ] {
            let zone = [ex + dx * gap, ey + dy * gap];
            if geom::distance(zone, [px, py]) < reach {
                return Some((geom::rotate([ex, ey], center, angle), false));
            }
        }
    }
    None
}

/// `avoidRectangularCorner`: a point off a box's corner moved onto one of
/// its sides.
fn avoid_corner(e: &Element, p: Point) -> Point {
    let center = element_center(e);
    let (x, y, w, h, angle) = (e.x(), e.y(), e.width(), e.height(), e.angle());
    let [px, py] = geom::rotate(p, center, -angle);
    let gap = binding::gap(e);
    let at = |q: Point| geom::rotate(q, center, angle);
    if px < x && py < y {
        // Top left.
        if py - y > -gap {
            return at([x - gap, y]);
        }
        at([x, y - gap])
    } else if px < x && py > y + h {
        // Bottom left.
        if px - x > -gap {
            return at([x, y + h + gap]);
        }
        at([x - gap, y + h])
    } else if px > x + w && py > y + h {
        // Bottom right.
        if px - x < w + gap {
            return at([x + w, y + h + gap]);
        }
        at([x + w + gap, y + h])
    } else if px > x + w && py < y {
        // Top right.
        if px - x < w + gap {
            return at([x + w, y - gap]);
        }
        at([x + w + gap, y])
    } else {
        p
    }
}

/// Where segment `a`–`b` crosses a shape's outline grown by `grow`
/// (`intersectElementWithLineSegment`), nearest `to` first.
fn crossing_nearest(e: &Element, a: Point, b: Point, grow: f64, to: Point) -> Option<Point> {
    let c = [e.x() + e.width() / 2.0, e.y() + e.height() / 2.0];
    let (la, lb) = (
        geom::rotate(a, c, -e.angle()),
        geom::rotate(b, c, -e.angle()),
    );
    let dist2 = |p: Point| {
        let (dx, dy) = (to[0] - p[0], to[1] - p[1]);
        dx * dx + dy * dy
    };
    let mut best: Option<Point> = None;
    for w in binding::outline(e, grow).windows(2) {
        if let Some(p) = binding::segment_intersection(la, lb, w[0], w[1]) {
            let p = geom::rotate(p, c, e.angle());
            if best.is_none_or(|q| dist2(p) < dist2(q)) {
                best = Some(p);
            }
        }
    }
    best
}

/// An end of an arrow being drawn, snapped onto the outline of the shape
/// under it, a gap away (`bindPointToSnapToElementOutline`, for elbow
/// arrows): a side's middle when near one.
fn snap_to_outline(points: &[Point], start: bool, e: &Element) -> Point {
    let Some(&point) = (if start { points.first() } else { points.last() }) else {
        return [0.0, 0.0];
    };
    if points.len() < 2 {
        return point;
    }
    let rectangular = !matches!(e.kind(), ElementType::Ellipse);
    let edge = if rectangular {
        avoid_corner(e, point)
    } else {
        point
    };
    let gap = binding::gap(e);
    let bounds = aabb(e, None);
    let center = bounds_center(bounds);
    let snap = snap_midpoint(e, edge);
    let resolved = snap.map_or(point, |s| s.0);
    let level = horizontal(match snap {
        Some((m, true)) => heading_of(geom::vector(m, center)),
        _ => heading_from_element(e, bounds, point),
    });
    let reach = js::max(e.width(), e.height()) * 2.0;
    let ray = |from: Point| {
        let v = geom::vector(resolved, from);
        let m = (v[0] * v[0] + v[1] * v[1]).sqrt();
        let n = if m == 0.0 {
            [0.0, 0.0]
        } else {
            [v[0] / m, v[1] / m]
        };
        [from[0] + n[0] * reach, from[1] + n[1] * reach]
    };
    let other = [
        if level { center[0] } else { resolved[0] },
        if !level { center[1] } else { resolved[1] },
    ];
    let mut found = crossing_nearest(e, other, ray(other), gap, resolved);
    if found.is_none() {
        let another = [
            if !level { center[0] } else { resolved[0] },
            if level { center[1] } else { resolved[1] },
        ];
        found = crossing_nearest(e, another, ray(another), BASE_GAP, resolved);
    }
    match found {
        Some(p) => {
            let (dx, dy) = (p[0] - edge[0], p[1] - edge[1]);
            if dx * dx + dy * dy < PRECISION {
                edge
            } else {
                p
            }
        }
        None => edge,
    }
}

/// `normalizeFixedPoint`: within reason, and never exactly a half (the
/// arrow's heading would flip on rounding).
fn normalize_fixed(fp: [f64; 2]) -> [f64; 2] {
    if !fp.iter().all(|x| x.is_finite()) {
        return [0.5001, 0.5001];
    }
    let c = fp.map(|r| js::min(js::max(r, -10.0), 10.0));
    c.map(|r| if (r - 0.5).abs() < 0.0001 { 0.5001 } else { r })
}

/// `getGlobalFixedPointForBindableElement`.
fn fixed_global(e: &Element, fp: [f64; 2]) -> Point {
    let [fx, fy] = normalize_fixed(fp);
    geom::rotate(
        [e.x() + e.width() * fx, e.y() + e.height() * fy],
        element_center(e),
        e.angle(),
    )
}

/// Where an elbow arrow's end attaches on `target`, as a fraction of its
/// box (`calculateFixedPointForElbowArrowBinding`): the end snapped onto its
/// outline.
pub fn fixed_point(arrow: &Element, start: bool, target: &Element) -> [f64; 2] {
    let snapped = snap_to_outline(&global_points(arrow), start, target);
    let (x, y, w, h) = (target.x(), target.y(), target.width(), target.height());
    let mid = [x + ((x + w) - x) / 2.0, y + ((y + h) - y) / 2.0];
    let [px, py] = geom::rotate(snapped, mid, -target.angle());
    if w < 1.0 || h < 1.0 {
        return normalize_fixed([0.5, 0.5]);
    }
    let floor = binding::gap(target);
    normalize_fixed([(px - x) / js::max(w, floor), (py - y) / js::max(h, floor)])
}

// The arrow.

/// A segment the user moved, kept where it was put: the `index`th one (from
/// point `index - 1` to point `index`), relative to the arrow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FixedSegment {
    pub index: i64,
    pub start: Point,
    pub end: Point,
}

fn point_of(v: &Value) -> Option<Point> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

/// An arrow's moved segments; `None` when it has none (`null`).
pub fn fixed_segments(arrow: &Element) -> Option<Vec<FixedSegment>> {
    let list = arrow.get("fixedSegments")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|s| {
                Some(FixedSegment {
                    index: s.get("index")?.as_f64()? as i64,
                    start: point_of(s.get("start")?)?,
                    end: point_of(s.get("end")?)?,
                })
            })
            .collect(),
    )
}

fn segments_value(segments: &[FixedSegment]) -> Value {
    if segments.is_empty() {
        return Value::Null;
    }
    Value::Array(
        segments
            .iter()
            .map(|s| {
                json!({
                    "index": s.index,
                    "start": [num(s.start[0]), num(s.start[1])],
                    "end": [num(s.end[0]), num(s.end[1])],
                })
            })
            .collect(),
    )
}

/// An end's binding, as routing reads it.
#[derive(Clone, Debug)]
struct Bound {
    id: String,
    fixed: [f64; 2],
}

fn bound(v: Option<&Value>) -> Option<Bound> {
    let b = v?.as_object()?;
    Some(Bound {
        id: b.get("elementId")?.as_str()?.to_string(),
        // `fixedPoint || [0, 0]`.
        fixed: b.get("fixedPoint").and_then(point_of).unwrap_or([0.0, 0.0]),
    })
}

/// What routing needs of an arrow (`ElbowArrowState`).
#[derive(Clone, Debug)]
struct State {
    x: f64,
    y: f64,
    start: Option<Bound>,
    end: Option<Bound>,
    start_arrowhead: Option<String>,
    end_arrowhead: Option<String>,
}

impl State {
    fn of(arrow: &Element) -> State {
        State {
            x: arrow.x(),
            y: arrow.y(),
            start: bound(arrow.get("startBinding")),
            end: bound(arrow.get("endBinding")),
            start_arrowhead: arrow.start_arrowhead().map(str::to_string),
            end_arrowhead: arrow.end_arrowhead().map(str::to_string),
        }
    }
}

/// The arrow's points on the drawing (an elbow arrow isn't turned).
fn global_points(arrow: &Element) -> Vec<Point> {
    let (x, y) = (arrow.x(), arrow.y());
    arrow
        .points()
        .iter()
        .map(|p| [x + p[0], y + p[1]])
        .collect()
}

/// `getBindableElementForId`.
fn bindable_by_id<'a>(scene: &'a Scene, id: &str) -> Option<&'a Element> {
    scene
        .element(id)
        .filter(|e| !e.is_deleted() && e.kind().is_bindable() && e.container_id().is_none())
}

/// The shape an end at `p` would attach to (`getHoveredElementForBinding`,
/// as `binding::attach` finds it).
fn hovered(scene: &Scene, p: Point) -> Option<&Element> {
    let reach = binding::max_distance(1.0);
    scene
        .elements
        .iter()
        .rev()
        .filter(|e| !e.is_deleted() && !e.is_locked() && binding::bindable(e))
        .find(|e| {
            hit::hits(e, p, 1.0) || {
                let [lx, ly] = hit::local(e, p);
                lx >= -reach && ly >= -reach && lx <= e.width() + reach && ly <= e.height() + reach
            }
        })
}

/// `getArrowheadSize`.
fn arrowhead_size(arrowhead: &str) -> f64 {
    match arrowhead {
        "arrow" => 25.0,
        "diamond" | "diamond_outline" => 12.0,
        "cardinality_one" | "cardinality_exactly_one" | "cardinality_zero_or_one" => 20.0,
        _ => 15.0,
    }
}

// Routing (`elbowArrow.ts`).

/// Where the route goes between (`ElbowArrowData`).
struct Data {
    aabbs: [Bounds; 2],
    start_dongle: Point,
    start_global: Point,
    start_heading: Heading,
    end_dongle: Point,
    end_global: Point,
    end_heading: Heading,
    common: Bounds,
    hovered_start: Option<Element>,
    hovered_end: Option<Element>,
}

/// `getElbowArrowData`: the ends, the ways they leave, and the boxes the
/// route keeps out of; `next` are the arrow's new points. While `dragging`
/// the ends attach to what is under them, else to what they are bound to.
fn data(state: &State, scene: &Scene, next: &[Point], dragging: bool) -> Data {
    let first = next[0];
    let last = next[next.len() - 1];
    let orig_start = [first[0] + state.x, first[1] + state.y];
    let orig_end = [last[0] + state.x, last[1] + state.y];
    let (hs, he) = if dragging {
        (
            hovered(scene, orig_start).cloned(),
            hovered(scene, orig_end).cloned(),
        )
    } else {
        let by = |b: &Option<Bound>| {
            b.as_ref()
                .and_then(|b| bindable_by_id(scene, &b.id))
                .cloned()
        };
        (by(&state.start), by(&state.end))
    };
    // `getGlobalPoint`.
    let points: Vec<Point> = next
        .iter()
        .map(|p| [state.x + p[0], state.y + p[1]])
        .collect();
    let end_point = |start: bool, el: &Option<Element>, b: &Option<Bound>, orig: Point| match el {
        Some(e) if dragging => snap_to_outline(&points, start, e),
        Some(e) => fixed_global(e, b.as_ref().map_or([0.0, 0.0], |b| b.fixed)),
        None => orig,
    };
    let sg = end_point(true, &hs, &state.start, orig_start);
    let eg = end_point(false, &he, &state.end, orig_end);
    let sh = bind_heading(sg, eg, hs.as_ref(), orig_start);
    let eh = bind_heading(eg, sg, he.as_ref(), orig_end);
    let spb = [sg[0] - 2.0, sg[1] - 2.0, sg[0] + 2.0, sg[1] + 2.0];
    let epb = [eg[0] - 2.0, eg[1] - 2.0, eg[0] + 2.0, eg[1] + 2.0];
    let element_box = |e: &Option<Element>, h: Heading, head: bool, point: Bounds| match e {
        Some(e) => {
            let gap = binding::gap(e);
            aabb(
                e,
                Some(offset_from_heading(
                    h,
                    if head { gap * 6.0 } else { gap * 2.0 },
                    1.0,
                )),
            )
        }
        None => point,
    };
    let seb = element_box(&hs, sh, state.start_arrowhead.is_some(), spb);
    let eeb = element_box(&he, eh, state.end_arrowhead.is_some(), epb);
    let padded = |e: &Option<Element>, h: Heading, point: Bounds| {
        e.as_ref().map_or(point, |e| {
            aabb(e, Some(offset_from_heading(h, BASE_PADDING, BASE_PADDING)))
        })
    };
    let bounds_overlap = inside(sg, padded(&he, eh, epb)) || inside(eg, padded(&hs, sh, spb));
    let common_bounds = if bounds_overlap {
        common(&[spb, epb])
    } else {
        common(&[seb, eeb])
    };
    let free = hs.is_none() && he.is_none();
    let difference = |h: Heading, head: bool| {
        if bounds_overlap {
            offset_from_heading(h, if free { 0.0 } else { BASE_PADDING }, 0.0)
        } else {
            let gaps = if head { BASE_GAP * 6.0 } else { BASE_GAP * 2.0 };
            offset_from_heading(
                h,
                if free { 0.0 } else { BASE_PADDING - gaps },
                BASE_PADDING,
            )
        }
    };
    let aabbs = dynamic_aabbs(
        if bounds_overlap { spb } else { seb },
        if bounds_overlap { epb } else { eeb },
        common_bounds,
        difference(sh, state.start_arrowhead.is_some()),
        difference(eh, state.end_arrowhead.is_some()),
        bounds_overlap,
        hs.as_ref().map(|e| aabb(e, None)),
        he.as_ref().map(|e| aabb(e, None)),
    );
    Data {
        start_dongle: dongle(aabbs[0], sh, sg),
        end_dongle: dongle(aabbs[1], eh, eg),
        aabbs,
        start_global: sg,
        start_heading: sh,
        end_global: eg,
        end_heading: eh,
        common: common_bounds,
        hovered_start: hs,
        hovered_end: he,
    }
}

/// `getSimpleRoute`: between two shapes, one corner (the arrow leaves one
/// and enters the other at a right angle) or two (it leaves and enters
/// heading the same way), if that fits clear of both.
fn simple_route(state: &State, d: &Data) -> Option<Vec<Point>> {
    let (Some(hs), Some(he)) = (&d.hovered_start, &d.hovered_end) else {
        return None;
    };
    let (sg, eg, sh, eh) = (d.start_global, d.end_global, d.start_heading, d.end_heading);
    let one_corner = horizontal(sh) != horizontal(eh);
    if hs.id() == he.id() || (!one_corner && sh != flip(eh)) {
        return None;
    }
    // How far the end is ahead of the start, the way the start leaves.
    let ahead = (eg[0] - sg[0]) * sh[0] + (eg[1] - sg[1]) * sh[1];
    let (first_leg, last_leg);
    if one_corner {
        first_leg = ahead;
        last_leg = (sg[0] - eg[0]) * eh[0] + (sg[1] - eg[1]) * eh[1];
    } else {
        // Halfway, unless that leaves an arrowhead less than the padding
        // and the other end can spare some.
        let mut first = ahead / 2.0;
        if state.start_arrowhead.is_none() != state.end_arrowhead.is_none() {
            let arrowhead_leg = js::max(
                ahead / 2.0,
                js::min(BASE_PADDING, ahead - ELBOW_ARROW_CORNER_RADIUS * 2.0),
            );
            first = if state.start_arrowhead.is_some() {
                arrowhead_leg
            } else {
                ahead - arrowhead_leg
            };
        }
        first_leg = first;
        last_leg = ahead - first;
    }
    // A leg keeps its corner round, and fits its arrowhead.
    let shortest = |head: &Option<String>| {
        js::max(
            ELBOW_ARROW_CORNER_RADIUS / 2.0,
            head.as_deref().map_or(0.0, arrowhead_size),
        )
    };
    if first_leg <= shortest(&state.start_arrowhead) || last_leg <= shortest(&state.end_arrowhead) {
        return None;
    }
    let first_turn = [sg[0] + sh[0] * first_leg, sg[1] + sh[1] * first_leg];
    let last_turn = [eg[0] + eh[0] * last_leg, eg[1] + eh[1] * last_leg];
    let sb = aabb(hs, Some([binding::gap(hs); 4]));
    let eb = aabb(he, Some([binding::gap(he); 4]));
    if crosses(sg, first_turn, eb)
        || crosses(first_turn, last_turn, sb)
        || crosses(first_turn, last_turn, eb)
        || crosses(last_turn, eg, sb)
    {
        return None;
    }
    Some(vec![sg, first_turn, last_turn, eg])
}

/// `getElbowArrowRoute`: one or two corners if that fits, else on the grid;
/// without short segments or points between segments in line.
fn route_points(state: &State, d: &Data) -> Vec<Point> {
    let points = simple_route(state, d)
        .or_else(|| grid_route(state, d))
        .unwrap_or_default();
    corner_points(&remove_short_segments(&points))
}

/// A node of the routing grid.
#[derive(Clone, Debug)]
struct Node {
    f: f64,
    g: f64,
    h: f64,
    closed: bool,
    visited: bool,
    parent: Option<usize>,
    pos: Point,
    addr: [usize; 2],
}

/// `Grid`: nodes where the lines of the boxes cross, row by row.
struct Grid {
    xs: Vec<f64>,
    ys: Vec<f64>,
    nodes: Vec<Node>,
}

impl Grid {
    /// `gridNodeFromAddr`.
    fn at(&self, col: i64, row: i64) -> Option<usize> {
        let (cols, rows) = (self.xs.len() as i64, self.ys.len() as i64);
        (col >= 0 && col < cols && row >= 0 && row < rows).then(|| (row * cols + col) as usize)
    }

    /// `pointToGridNode`.
    fn node_for(&self, p: Point) -> Option<usize> {
        let col = self.xs.iter().position(|x| *x == p[0])?;
        let row = self.ys.iter().position(|y| *y == p[1])?;
        self.at(col as i64, row as i64)
    }
}

/// `getElbowArrowGridRoute`: from dongle to dongle by A*, keeping out of the
/// boxes around the shapes.
fn grid_route(state: &State, d: &Data) -> Option<Vec<Point>> {
    let mut grid = calculate_grid(
        &d.aabbs,
        d.start_dongle,
        d.start_heading,
        d.end_dongle,
        d.end_heading,
        d.common,
    );
    let start_dongle = grid.node_for(d.start_dongle);
    let end_dongle = grid.node_for(d.end_dongle);
    // The true ends can't be stepped on.
    let end_node = grid.node_for(d.end_global);
    if let Some(n) = end_node
        && d.hovered_end.is_some()
    {
        grid.nodes[n].closed = true;
    }
    let start_node = grid.node_for(d.start_global);
    if let Some(n) = start_node
        && state.start.is_some()
    {
        grid.nodes[n].closed = true;
    }
    let dongles_overlap = match (start_dongle, end_dongle) {
        (Some(s), Some(e)) => {
            inside(grid.nodes[s].pos, d.aabbs[1]) || inside(grid.nodes[e].pos, d.aabbs[0])
        }
        _ => false,
    };
    let from = start_dongle.or(start_node)?;
    let to = end_dongle.or(end_node)?;
    let boxes: &[Bounds] = if dongles_overlap { &[] } else { &d.aabbs };
    let path = astar(from, to, &mut grid, d.start_heading, d.end_heading, boxes)?;
    let mut points: Vec<Point> = path.iter().map(|&n| grid.nodes[n].pos).collect();
    if start_dongle.is_some() {
        points.insert(0, d.start_global);
    }
    if end_dongle.is_some() {
        points.push(d.end_global);
    }
    Some(points)
}

/// `BinaryHeap`: node indices, the lowest `f` on top.
#[derive(Default)]
struct Heap {
    content: Vec<usize>,
}

impl Heap {
    fn sink_down(&mut self, nodes: &[Node], mut idx: usize) {
        let node = self.content[idx];
        let score = nodes[node].f;
        while idx > 0 {
            let parent_n = ((idx + 1) >> 1) - 1;
            let parent = self.content[parent_n];
            if score < nodes[parent].f {
                self.content[idx] = parent;
                idx = parent_n;
            } else {
                break;
            }
        }
        self.content[idx] = node;
    }

    fn bubble_up(&mut self, nodes: &[Node], mut idx: usize) {
        let length = self.content.len();
        let node = self.content[idx];
        let score = nodes[node].f;
        loop {
            let child1 = ((idx + 1) << 1) - 1;
            let child2 = child1 + 1;
            let mut smallest = idx;
            let mut smallest_score = score;
            if child1 < length {
                let s = nodes[self.content[child1]].f;
                if s < smallest_score {
                    smallest = child1;
                    smallest_score = s;
                }
            }
            if child2 < length && nodes[self.content[child2]].f < smallest_score {
                smallest = child2;
            }
            if smallest == idx {
                break;
            }
            self.content[idx] = self.content[smallest];
            idx = smallest;
        }
        self.content[idx] = node;
    }

    fn push(&mut self, nodes: &[Node], node: usize) {
        self.content.push(node);
        self.sink_down(nodes, self.content.len() - 1);
    }

    fn pop(&mut self, nodes: &[Node]) -> Option<usize> {
        let result = *self.content.first()?;
        let end = self.content.pop()?;
        if !self.content.is_empty() {
            self.content[0] = end;
            self.bubble_up(nodes, 0);
        }
        Some(result)
    }

    fn rescore(&mut self, nodes: &[Node], node: usize) {
        if let Some(i) = self.content.iter().position(|&n| n == node) {
            self.sink_down(nodes, i);
        }
    }
}

/// `m_dist`: the Manhattan distance.
fn m_dist(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs()
}

/// `astar`: the shortest path on the grid, each turn costing much, never
/// going back on itself nor through the boxes.
fn astar(
    start: usize,
    end: usize,
    grid: &mut Grid,
    start_heading: Heading,
    end_heading: Heading,
    aabbs: &[Bounds],
) -> Option<Vec<usize>> {
    let bend = m_dist(grid.nodes[start].pos, grid.nodes[end].pos);
    let mut open = Heap::default();
    open.push(&grid.nodes, start);
    while let Some(current) = open.pop(&grid.nodes) {
        if grid.nodes[current].closed {
            continue;
        }
        if current == end {
            // `pathTo`.
            let mut path = vec![current];
            let mut at = current;
            while let Some(p) = grid.nodes[at].parent {
                at = p;
                path.push(at);
            }
            path.pop();
            path.push(start);
            path.reverse();
            return Some(path);
        }
        grid.nodes[current].closed = true;
        let [col, row] = grid.nodes[current].addr.map(|a| a as i64);
        let neighbors = [
            grid.at(col, row - 1),
            grid.at(col + 1, row),
            grid.at(col, row + 1),
            grid.at(col - 1, row),
        ];
        for (i, neighbor) in neighbors.into_iter().enumerate() {
            let Some(nb) = neighbor else { continue };
            if grid.nodes[nb].closed {
                continue;
            }
            let cur = &grid.nodes[current];
            let half = scale_from(grid.nodes[nb].pos, cur.pos, 0.5);
            if aabbs.iter().any(|b| inside(half, *b)) {
                continue;
            }
            let heading = [UP, RIGHT, DOWN, LEFT][i];
            let previous = match cur.parent {
                Some(p) => heading_of(geom::vector(cur.pos, grid.nodes[p].pos)),
                None => start_heading,
            };
            // Never back the way it came.
            let reverse = flip(previous) == heading
                || (grid.nodes[start].addr == grid.nodes[nb].addr && heading == start_heading)
                || (grid.nodes[end].addr == grid.nodes[nb].addr && heading == end_heading);
            if reverse {
                continue;
            }
            let turn = previous != heading;
            let g = cur.g
                + m_dist(grid.nodes[nb].pos, cur.pos)
                + if turn { bend.powf(3.0) } else { 0.0 };
            let been = grid.nodes[nb].visited;
            if !been || g < grid.nodes[nb].g {
                let bends = estimate_segment_count(
                    grid.nodes[nb].pos,
                    grid.nodes[end].pos,
                    heading,
                    end_heading,
                );
                let end_pos = grid.nodes[end].pos;
                let n = &mut grid.nodes[nb];
                n.visited = true;
                n.parent = Some(current);
                n.h = m_dist(end_pos, n.pos) + bends * bend.powf(2.0);
                n.g = g;
                n.f = n.g + n.h;
                if been {
                    open.rescore(&grid.nodes, nb);
                } else {
                    open.push(&grid.nodes, nb);
                }
            }
        }
    }
    None
}

/// `generateDynamicAABBs`: the boxes around the two ends, grown to touch
/// halfway between them.
#[allow(clippy::too_many_arguments)]
fn dynamic_aabbs(
    a: Bounds,
    b: Bounds,
    common: Bounds,
    start_difference: [f64; 4],
    end_difference: [f64; 4],
    disable_side_hack: bool,
    start_element: Option<Bounds>,
    end_element: Option<Bounds>,
) -> [Bounds; 2] {
    let se = start_element.unwrap_or(a);
    let ee = end_element.unwrap_or(b);
    let [start_up, start_right, start_down, start_left] = start_difference;
    let [end_up, end_right, end_down, end_left] = end_difference;
    let (min, max) = (js::min, js::max);
    let first = [
        if a[0] > b[2] {
            if a[1] > b[3] || a[3] < b[1] {
                min((se[0] + ee[2]) / 2.0, a[0] - start_left)
            } else {
                (se[0] + ee[2]) / 2.0
            }
        } else if a[0] > b[0] {
            a[0] - start_left
        } else {
            common[0] - start_left
        },
        if a[1] > b[3] {
            if a[0] > b[2] || a[2] < b[0] {
                min((se[1] + ee[3]) / 2.0, a[1] - start_up)
            } else {
                (se[1] + ee[3]) / 2.0
            }
        } else if a[1] > b[1] {
            a[1] - start_up
        } else {
            common[1] - start_up
        },
        if a[2] < b[0] {
            if a[1] > b[3] || a[3] < b[1] {
                max((se[2] + ee[0]) / 2.0, a[2] + start_right)
            } else {
                (se[2] + ee[0]) / 2.0
            }
        } else if a[2] < b[2] {
            a[2] + start_right
        } else {
            common[2] + start_right
        },
        if a[3] < b[1] {
            if a[0] > b[2] || a[2] < b[0] {
                max((se[3] + ee[1]) / 2.0, a[3] + start_down)
            } else {
                (se[3] + ee[1]) / 2.0
            }
        } else if a[3] < b[3] {
            a[3] + start_down
        } else {
            common[3] + start_down
        },
    ];
    let second = [
        if b[0] > a[2] {
            if b[1] > a[3] || b[3] < a[1] {
                min((ee[0] + se[2]) / 2.0, b[0] - end_left)
            } else {
                (ee[0] + se[2]) / 2.0
            }
        } else if b[0] > a[0] {
            b[0] - end_left
        } else {
            common[0] - end_left
        },
        if b[1] > a[3] {
            if b[0] > a[2] || b[2] < a[0] {
                min((ee[1] + se[3]) / 2.0, b[1] - end_up)
            } else {
                (ee[1] + se[3]) / 2.0
            }
        } else if b[1] > a[1] {
            b[1] - end_up
        } else {
            common[1] - end_up
        },
        if b[2] < a[0] {
            if b[1] > a[3] || b[3] < a[1] {
                max((ee[2] + se[0]) / 2.0, b[2] + end_right)
            } else {
                (ee[2] + se[0]) / 2.0
            }
        } else if b[2] < a[2] {
            b[2] + end_right
        } else {
            common[2] + end_right
        },
        if b[3] < a[1] {
            if b[0] > a[2] || b[2] < a[0] {
                max((ee[3] + se[1]) / 2.0, b[3] + end_down)
            } else {
                (ee[3] + se[1]) / 2.0
            }
        } else if b[3] < a[3] {
            b[3] + end_down
        } else {
            common[3] + end_down
        },
    ];
    let c = common_of(&[first, second]);
    if !disable_side_hack
        && first[2] - first[0] + second[2] - second[0] > c[2] - c[0] + 0.00000000001
        && first[3] - first[1] + second[3] - second[1] > c[3] - c[1] + 0.00000000001
    {
        let end_center = [(second[0] + second[2]) / 2.0, (second[1] + second[3]) / 2.0];
        let v = |x: f64, y: f64| [x - end_center[0], y - end_center[1]];
        if b[0] > a[2] && a[1] > b[3] {
            // Bottom left.
            let cx = first[2] + (second[0] - first[2]) / 2.0;
            let cy = second[3] + (first[1] - second[3]) / 2.0;
            if cross(v(a[2], a[1]), v(a[0], a[3])) > 0.0 {
                return [
                    [first[0], first[1], cx, first[3]],
                    [cx, second[1], second[2], second[3]],
                ];
            }
            return [
                [first[0], cy, first[2], first[3]],
                [second[0], second[1], second[2], cy],
            ];
        } else if a[2] < b[0] && a[3] < b[1] {
            // Top left.
            let cx = first[2] + (second[0] - first[2]) / 2.0;
            let cy = first[3] + (second[1] - first[3]) / 2.0;
            if cross(v(a[0], a[1]), v(a[2], a[3])) > 0.0 {
                return [
                    [first[0], first[1], first[2], cy],
                    [second[0], cy, second[2], second[3]],
                ];
            }
            return [
                [first[0], first[1], cx, first[3]],
                [cx, second[1], second[2], second[3]],
            ];
        } else if a[0] > b[2] && a[3] < b[1] {
            // Top right.
            let cx = second[2] + (first[0] - second[2]) / 2.0;
            let cy = first[3] + (second[1] - first[3]) / 2.0;
            if cross(v(a[2], a[1]), v(a[0], a[3])) > 0.0 {
                return [
                    [cx, first[1], first[2], first[3]],
                    [second[0], second[1], cx, second[3]],
                ];
            }
            return [
                [first[0], first[1], first[2], cy],
                [second[0], cy, second[2], second[3]],
            ];
        } else if a[0] > b[2] && a[1] > b[3] {
            // Bottom right.
            let cx = second[2] + (first[0] - second[2]) / 2.0;
            let cy = second[3] + (first[1] - second[3]) / 2.0;
            if cross(v(a[0], a[1]), v(a[2], a[3])) > 0.0 {
                return [
                    [cx, first[1], first[2], first[3]],
                    [second[0], second[1], cx, second[3]],
                ];
            }
            return [
                [first[0], cy, first[2], first[3]],
                [second[0], second[1], second[2], cy],
            ];
        }
    }
    [first, second]
}

fn common_of(boxes: &[Bounds]) -> Bounds {
    common(boxes)
}

/// `calculateGrid`: lines through the boxes' edges and the dongles.
fn calculate_grid(
    aabbs: &[Bounds; 2],
    start: Point,
    start_heading: Heading,
    end: Point,
    end_heading: Heading,
    common: Bounds,
) -> Grid {
    // A JavaScript `Set` of numbers: in the order added, each once.
    fn add(set: &mut Vec<f64>, v: f64) {
        if !set.iter().any(|x| *x == v || (x.is_nan() && v.is_nan())) {
            set.push(v);
        }
    }
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for (p, h) in [(start, start_heading), (end, end_heading)] {
        if horizontal(h) {
            add(&mut ys, p[1]);
        } else {
            add(&mut xs, p[0]);
        }
    }
    for b in aabbs {
        add(&mut xs, b[0]);
        add(&mut xs, b[2]);
        add(&mut ys, b[1]);
        add(&mut ys, b[3]);
    }
    add(&mut xs, common[0]);
    add(&mut xs, common[2]);
    add(&mut ys, common[1]);
    add(&mut ys, common[3]);
    js::sort_by(&mut ys, |a, b| a - b);
    js::sort_by(&mut xs, |a, b| a - b);
    let mut nodes = Vec::with_capacity(xs.len() * ys.len());
    for (row, y) in ys.iter().enumerate() {
        for (col, x) in xs.iter().enumerate() {
            nodes.push(Node {
                f: 0.0,
                g: 0.0,
                h: 0.0,
                closed: false,
                visited: false,
                parent: None,
                pos: [*x, *y],
                addr: [col, row],
            });
        }
    }
    Grid { xs, ys, nodes }
}

/// `getDonglePosition`: where the route leaves the box around an end.
fn dongle(b: Bounds, h: Heading, p: Point) -> Point {
    if h == UP {
        [p[0], b[1]]
    } else if h == RIGHT {
        [b[2], p[1]]
    } else if h == DOWN {
        [p[0], b[3]]
    } else {
        [b[0], p[1]]
    }
}

/// `estimateSegmentCount`: how many turns are still to come, about.
fn estimate_segment_count(start: Point, end: Point, sh: Heading, eh: Heading) -> f64 {
    let [sx, sy] = start;
    let [ex, ey] = end;
    let n = if eh == RIGHT {
        if sh == RIGHT {
            if sx >= ex {
                4
            } else if sy == ey {
                0
            } else {
                2
            }
        } else if sh == UP {
            if sy > ey && sx < ex { 1 } else { 3 }
        } else if sh == DOWN {
            if sy < ey && sx < ex { 1 } else { 3 }
        } else if sy == ey {
            4
        } else {
            2
        }
    } else if eh == LEFT {
        if sh == RIGHT {
            if sy == ey { 4 } else { 2 }
        } else if sh == UP {
            if sy > ey && sx > ex { 1 } else { 3 }
        } else if sh == DOWN {
            if sy < ey && sx > ex { 1 } else { 3 }
        } else if sx <= ex {
            4
        } else if sy == ey {
            0
        } else {
            2
        }
    } else if eh == UP {
        if sh == RIGHT {
            if sy > ey && sx < ex { 1 } else { 3 }
        } else if sh == UP {
            if sy >= ey {
                4
            } else if sx == ex {
                0
            } else {
                2
            }
        } else if sh == DOWN {
            if sx == ex { 4 } else { 2 }
        } else if sy > ey && sx > ex {
            1
        } else {
            3
        }
    } else if eh == DOWN {
        if sh == RIGHT {
            if sy < ey && sx < ex { 1 } else { 3 }
        } else if sh == UP {
            if sx == ex { 4 } else { 2 }
        } else if sh == DOWN {
            if sy <= ey {
                4
            } else if sx == ex {
                0
            } else {
                2
            }
        } else if sy < ey && sx > ex {
            1
        } else {
            3
        }
    } else {
        0
    };
    n as f64
}

/// `getElbowArrowCornerPoints`: only the corners (and the ends).
fn corner_points(points: &[Point]) -> Vec<Point> {
    if points.len() <= 1 {
        return points.to_vec();
    }
    let level = |a: Point, b: Point| (a[1] - b[1]).abs() < (a[0] - b[0]).abs();
    let mut previous = level(points[0], points[1]);
    let last = points.len() - 1;
    points
        .iter()
        .enumerate()
        .filter(|&(i, &p)| {
            if i == 0 || i == last {
                return true;
            }
            let next = level(p, points[i + 1]);
            let keep = previous != next;
            previous = next;
            keep
        })
        .map(|(_, p)| *p)
        .collect()
}

/// `removeElbowArrowShortSegments`.
fn remove_short_segments(points: &[Point]) -> Vec<Point> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let last = points.len() - 1;
    points
        .iter()
        .enumerate()
        .filter(|&(i, &p)| {
            i == 0 || i == last || geom::distance(points[i - 1], p) > DEDUP_THRESHOLD
        })
        .map(|(_, p)| *p)
        .collect()
}

// Updating an arrow (`updateElbowArrowPoints`).

/// What one step of routing changes on an arrow (`ElementUpdate`).
enum Patch {
    None,
    /// A new route, on the drawing, with the fixed segments and whether the
    /// ends turn at once (`startIsSpecial`, `endIsSpecial`; `None`: as is).
    Route {
        global: Vec<Point>,
        fixed: Option<Vec<FixedSegment>>,
        start_special: Option<Value>,
        end_special: Option<Value>,
    },
    /// Points and fixed segments taken as they are (a resize).
    Raw {
        points: Vec<Point>,
        fixed: Vec<FixedSegment>,
    },
}

/// `normalizeArrowElementUpdate`.
fn route_patch(
    global: Vec<Point>,
    fixed: Option<Vec<FixedSegment>>,
    start_special: Option<Value>,
    end_special: Option<Value>,
) -> Patch {
    Patch::Route {
        global,
        fixed,
        start_special,
        end_special,
    }
}

fn special(v: Option<bool>) -> Option<Value> {
    Some(v.map_or(Value::Null, Value::Bool))
}

/// Points relative to the first, within bounds (as
/// `normalizeArrowElementUpdate` makes them).
fn local_points(global: &[Point]) -> Vec<Point> {
    let Some(&[ox, oy]) = global.first() else {
        return Vec::new();
    };
    let clamp = |v: f64| js::min(js::max(v, -MAX_POS), MAX_POS);
    global
        .iter()
        .map(|p| [clamp(p[0] + -ox), clamp(p[1] + -oy)])
        .collect()
}

fn set_points(e: &mut Element, points: &[Point]) {
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for [x, y] in points {
        x0 = js::min(x0, *x);
        y0 = js::min(y0, *y);
        x1 = js::max(x1, *x);
        y1 = js::max(y1, *y);
    }
    e.set(
        "points",
        Value::Array(
            points
                .iter()
                .map(|[x, y]| json!([num(*x), num(*y)]))
                .collect(),
        ),
    );
    e.set("width", num(x1 - x0));
    e.set("height", num(y1 - y0));
}

fn apply(base: &Element, patch: Patch) -> Element {
    let mut e = base.clone();
    match patch {
        Patch::None => {}
        Patch::Route {
            global,
            fixed,
            start_special,
            end_special,
        } => {
            // No route (the search found none): as it was.
            let Some(&[ox, oy]) = global.first() else {
                return e;
            };
            let clamp = |v: f64| js::min(js::max(v, -MAX_POS), MAX_POS);
            e.set("x", num(clamp(ox)));
            e.set("y", num(clamp(oy)));
            set_points(&mut e, &local_points(&global));
            e.set(
                "fixedSegments",
                segments_value(fixed.as_deref().unwrap_or_default()),
            );
            if let Some(v) = start_special {
                e.set("startIsSpecial", v);
            }
            if let Some(v) = end_special {
                e.set("endIsSpecial", v);
            }
        }
        Patch::Raw { points, fixed } => {
            set_points(&mut e, &points);
            e.set("fixedSegments", segments_value(&fixed));
        }
    }
    e
}

/// What changes on an elbow arrow, for [`update`].
#[derive(Clone, Debug, Default)]
pub struct Changes {
    /// New points, relative to the arrow: its two ends, or all of them.
    pub points: Option<Vec<Point>>,
    /// New fixed segments.
    pub fixed_segments: Option<Vec<FixedSegment>>,
    /// New bindings (`Value::Null`: let go).
    pub start_binding: Option<Value>,
    pub end_binding: Option<Value>,
}

/// The arrow after `changes`, routed (`updateElbowArrowPoints`). While
/// `dragging` an end, the ends snap to what is under them; else bound ends
/// are where their bindings put them.
pub fn update(arrow: &Element, scene: &Scene, changes: &Changes, dragging: bool) -> Element {
    // What is given is set, and routing adds to it (`mutateElement`).
    let mut base = arrow.clone();
    base.set("angle", json!(0));
    if let Some(b) = &changes.start_binding {
        base.set("startBinding", b.clone());
    }
    if let Some(b) = &changes.end_binding {
        base.set("endBinding", b.clone());
    }
    if let Some(f) = &changes.fixed_segments {
        base.set("fixedSegments", segments_value(f));
    }
    let points = arrow.points();
    if points.len() < 2 {
        if let Some(p) = &changes.points {
            set_points(&mut base, p);
        }
        return base;
    }
    let fixed = changes
        .fixed_segments
        .clone()
        .or_else(|| fixed_segments(arrow))
        .unwrap_or_default();
    let updated: Vec<Point> = match &changes.points {
        Some(p) if p.len() == 2 => {
            let last = points.len() - 1;
            (0..points.len())
                .map(|i| match i {
                    0 => p[0],
                    i if i == last => p[1],
                    i => points[i],
                })
                .collect()
        }
        Some(p) => p.clone(),
        None => points.clone(),
    };
    let start_binding = match &changes.start_binding {
        Some(v) => bound(Some(v)),
        None => bound(arrow.get("startBinding")),
    };
    let end_binding = match &changes.end_binding {
        Some(v) => bound(Some(v)),
        None => bound(arrow.get("endBinding")),
    };
    let missing = |b: &Option<Bound>| {
        b.as_ref()
            .is_some_and(|b| bindable_by_id(scene, &b.id).is_none())
    };
    let valid = orthogonal(&updated);
    let (ax, ay) = (arrow.x(), arrow.y());
    let as_global =
        |pts: &[Point]| -> Vec<Point> { pts.iter().map(|p| [ax + p[0], ay + p[1]]).collect() };
    let no_points = changes.points.is_none() && changes.fixed_segments.is_none();
    // Attached to what isn't there: the points are kept as they are.
    if ((missing(&start_binding) || missing(&end_binding)) && (valid || no_points))
        || (scene.visible().next().is_none() && valid)
    {
        return apply(
            &base,
            route_patch(
                as_global(&updated),
                fixed_segments(arrow),
                arrow.get("startIsSpecial").cloned(),
                arrow.get("endIsSpecial").cloned(),
            ),
        );
    }
    // 1. Tidied where it is.
    let given = |b: &Option<Value>| b.as_ref().is_some_and(|v| !v.is_null());
    if no_points && !given(&changes.start_binding) && !given(&changes.end_binding) {
        return apply(&base, renormalize_patch(arrow, scene));
    }
    // Nothing changes.
    let unchanged = |given: &Option<Value>, key: &str| {
        matches!(
            (given, arrow.get(key)),
            (Some(Value::Null), Some(Value::Null))
        )
    };
    if unchanged(&changes.start_binding, "startBinding")
        && unchanged(&changes.end_binding, "endBinding")
        && changes
            .points
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .all(|(i, p)| points.get(i).is_some_and(|q| same_point(*p, *q)))
        && valid
    {
        return base;
    }
    let state = State {
        start: start_binding,
        end: end_binding,
        ..State::of(arrow)
    };
    let d = data(&state, scene, &updated, dragging);
    // 2. Routed anew.
    if fixed.is_empty() {
        return apply(
            &base,
            route_patch(
                route_points(&State::of(arrow), &d),
                Some(fixed),
                Some(Value::Null),
                Some(Value::Null),
            ),
        );
    }
    // 3. A fixed segment let go.
    if fixed_segments(arrow).map_or(0, |f| f.len()) > fixed.len() {
        return apply(&base, segment_release(arrow, &fixed, scene));
    }
    // 4. A segment moved.
    if changes.points.is_none() {
        return apply(
            &base,
            segment_move(
                arrow,
                &fixed,
                d.start_heading,
                d.end_heading,
                d.hovered_start.is_some(),
                d.hovered_end.is_some(),
            ),
        );
    }
    // 5. Resized: as given.
    if changes.fixed_segments.is_some() {
        return apply(
            &base,
            Patch::Raw {
                points: updated,
                fixed,
            },
        );
    }
    // 6. The ends moved, around the fixed segments.
    apply(&base, endpoint_drag(arrow, &updated, &fixed, &d))
}

/// `handleSegmentRenormalization`: segments that came to line up merged,
/// too short ones gone; routed anew once no fixed segment is left.
fn renormalize_patch(arrow: &Element, scene: &Scene) -> Patch {
    let Some(mut fixed) = fixed_segments(arrow) else {
        return Patch::None;
    };
    let (ax, ay) = (arrow.x(), arrow.y());
    let points = global_points(arrow);
    let mut merged: Vec<Point> = Vec::new();
    for (i, &p) in points.iter().enumerate() {
        if i >= 2 && geom::heading(p, points[i - 1]) == geom::heading(points[i - 1], points[i - 2])
        {
            // In line with the one before: one segment.
            let i = i as i64;
            let prev = fixed.iter().position(|s| s.index == i - 1);
            if let Some(k) = fixed.iter().position(|s| s.index == i) {
                let o = points[i as usize - 2];
                fixed[k].start = [o[0] - ax, o[1] - ay];
            }
            if let Some(k) = prev {
                fixed.remove(k);
            }
            merged.pop();
            for s in &mut fixed {
                if s.index > i - 1 {
                    s.index -= 1;
                }
            }
        }
        merged.push(p);
    }
    let mut next: Vec<Point> = Vec::new();
    for (i, &p) in merged.iter().enumerate() {
        if i >= 3 && geom::distance(merged[i - 2], merged[i - 1]) < DEDUP_THRESHOLD {
            // Too short: it goes, and the segments around it join.
            let ii = i as i64;
            let prev_prev = fixed.iter().position(|s| s.index == ii - 2);
            if let Some(k) = fixed.iter().position(|s| s.index == ii - 1) {
                fixed.remove(k);
            }
            if let Some(k) = prev_prev.filter(|k| *k < fixed.len()) {
                fixed.remove(k);
            }
            next.truncate(next.len().saturating_sub(2));
            for s in &mut fixed {
                if s.index > ii - 2 {
                    s.index -= 2;
                }
            }
            let level = geom::heading_is_horizontal(p, merged[i - 1]);
            next.push([
                if !level { merged[i - 2][0] } else { p[0] },
                if level { merged[i - 2][1] } else { p[1] },
            ]);
            continue;
        }
        next.push(p);
    }
    let n = next.len() as i64;
    let fixed: Vec<FixedSegment> = fixed
        .into_iter()
        .filter(|s| s.index != 1 && s.index != n - 1)
        .collect();
    if fixed.is_empty() {
        let local: Vec<Point> = next.iter().map(|p| [p[0] - ax, p[1] - ay]).collect();
        if local.len() < 2 {
            return Patch::None;
        }
        let state = State::of(arrow);
        let d = data(&state, scene, &local, false);
        return route_patch(
            route_points(&state, &d),
            Some(fixed),
            Some(Value::Null),
            Some(Value::Null),
        );
    }
    route_patch(
        next,
        Some(fixed),
        arrow.get("startIsSpecial").cloned(),
        arrow.get("endIsSpecial").cloned(),
    )
}

/// `handleSegmentRelease`: the route between the fixed segments around the
/// one let go is made anew.
fn segment_release(arrow: &Element, fixed: &[FixedSegment], scene: &Scene) -> Patch {
    let old = fixed_segments(arrow).unwrap_or_default();
    let Some(deleted) = old
        .iter()
        .position(|s| !fixed.iter().any(|f| f.index == s.index))
    else {
        return Patch::None;
    };
    let deleted_index = old[deleted].index;
    let prev = deleted.checked_sub(1).map(|k| old[k]);
    let next = old.get(deleted + 1).copied();
    let points = arrow.points();
    let (ax, ay) = (arrow.x(), arrow.y());
    let x = ax + prev.map_or(0.0, |s| s.end[0]);
    let y = ay + prev.map_or(0.0, |s| s.end[1]);
    let last = points[points.len() - 1];
    let state = State {
        x,
        y,
        start: if prev.is_some() {
            None
        } else {
            bound(arrow.get("startBinding"))
        },
        end: if next.is_some() {
            None
        } else {
            bound(arrow.get("endBinding"))
        },
        start_arrowhead: None,
        end_arrowhead: None,
    };
    let to = [
        ax + next.map_or(last[0], |s| s.start[0]) - x,
        ay + next.map_or(last[1], |s| s.start[1]) - y,
    ];
    let d = data(&state, scene, &[[0.0, 0.0], to], false);
    let restored = local_points(&route_points(&State::of(arrow), &d));
    if restored.len() < 2 {
        return Patch::None;
    }
    let at = |i: usize| points.get(i).map(|p| [ax + p[0], ay + p[1]]);
    let mut next_points: Vec<Point> = Vec::new();
    if let Some(p) = prev {
        next_points.extend((0..p.index.max(0) as usize).filter_map(at));
    }
    for p in &restored {
        next_points.push([
            ax + prev.map_or(0.0, |s| s.end[0]) + p[0],
            ay + prev.map_or(0.0, |s| s.end[1]) + p[1],
        ]);
    }
    if let Some(n) = next {
        next_points.extend((n.index.max(0) as usize..points.len()).filter_map(at));
    }
    let diff = next.map_or(points.len() as i64, |s| s.index) - prev.map_or(0, |s| s.index) - 1;
    let mut next_fixed: Vec<FixedSegment> = fixed
        .iter()
        .map(|s| {
            if s.index > deleted_index {
                FixedSegment {
                    index: s.index - diff + (restored.len() as i64 - 1),
                    ..*s
                }
            } else {
                *s
            }
        })
        .collect();
    let mut simplified: Vec<Point> = Vec::new();
    for (i, &p) in next_points.iter().enumerate() {
        if i > 0 && i + 1 < next_points.len() {
            let before = geom::heading(p, next_points[i - 1]);
            let after = geom::heading(next_points[i + 1], p);
            if before == after {
                for s in &mut next_fixed {
                    if s.index > i as i64 {
                        s.index -= 1;
                    }
                }
                continue;
            } else if before == flip(after) {
                for s in &mut next_fixed {
                    if s.index > i as i64 {
                        s.index += 1;
                    }
                }
                simplified.push(p);
                simplified.push(p);
                continue;
            }
        }
        simplified.push(p);
    }
    route_patch(
        simplified,
        Some(next_fixed),
        Some(json!(false)),
        Some(json!(false)),
    )
}

/// `handleSegmentMove`: the moved segment where it was put, its neighbours
/// stretched to it; the first or last one moving adds a segment.
fn segment_move(
    arrow: &Element,
    fixed_in: &[FixedSegment],
    start_heading: Heading,
    end_heading: Heading,
    hovered_start: bool,
    hovered_end: bool,
) -> Patch {
    let old = fixed_segments(arrow);
    let mut fixed = fixed_in.to_vec();
    let active =
        fixed
            .iter()
            .enumerate()
            .find_map(|(i, s)| match old.as_ref().and_then(|o| o.get(i)) {
                Some(o) if o.index == s.index => {
                    let x_moved = s.start[0] != o.start[0] && s.end[0] != o.end[0];
                    let y_moved = s.start[1] != o.start[1] && s.end[1] != o.end[1];
                    (x_moved != y_moved).then_some(i)
                }
                _ => Some(i),
            });
    let Some(active) = active else {
        return Patch::None;
    };
    let points = arrow.points();
    let n = points.len() as i64;
    let first_fixed = old
        .as_ref()
        .and_then(|o| o.iter().position(|s| s.index == 1));
    let last_fixed = old
        .as_ref()
        .and_then(|o| o.iter().position(|s| s.index == n - 1));
    // The first or last segment moved: it keeps clear of the shape.
    let length = geom::distance(fixed[active].start, fixed[active].end);
    let too_short = length < BASE_PADDING + 5.0;
    let padding = |h: Heading| {
        let level = horizontal(h);
        let positive = if level { h == RIGHT } else { h == DOWN };
        let pad = if too_short {
            length / 2.0
        } else {
            BASE_PADDING
        };
        let pad = if positive { pad } else { -pad };
        if level { [pad, 0.0] } else { [0.0, pad] }
    };
    if first_fixed.is_none() && fixed[active].index == 1 && hovered_start {
        let [dx, dy] = padding(start_heading);
        fixed[active].start = [fixed[active].start[0] + dx, fixed[active].start[1] + dy];
    }
    if last_fixed.is_none() && fixed[active].index == n - 1 && hovered_end {
        let [dx, dy] = padding(end_heading);
        fixed[active].end = [fixed[active].end[0] + dx, fixed[active].end[1] + dy];
    }
    let (ax, ay) = (arrow.x(), arrow.y());
    let mut next_fixed: Vec<FixedSegment> = fixed
        .iter()
        .map(|s| FixedSegment {
            index: s.index,
            start: [ax + s.start[0], ay + s.start[1]],
            end: [ax + s.end[0], ay + s.end[1]],
        })
        .collect();
    let mut new_points = global_points(arrow);
    let get = |v: &Vec<Point>, i: i64| (i >= 0).then(|| v.get(i as usize).copied()).flatten();
    let start_idx = next_fixed[active].index - 1;
    let end_idx = next_fixed[active].index;
    let (start, end) = (next_fixed[active].start, next_fixed[active].end);
    let prev_level = match (get(&new_points, start_idx - 1), get(&new_points, start_idx)) {
        (Some(a), Some(b)) if !same_point(b, a) => Some(geom::heading_is_horizontal(a, b)),
        _ => None,
    };
    let next_level = match (get(&new_points, end_idx + 1), get(&new_points, end_idx)) {
        (Some(a), Some(b)) if !same_point(b, a) => Some(geom::heading_is_horizontal(a, b)),
        _ => None,
    };
    let (Some(_), Some(_)) = (get(&new_points, start_idx), get(&new_points, end_idx)) else {
        return Patch::None;
    };
    if let Some(level) = prev_level {
        let dir = usize::from(level);
        new_points[start_idx as usize - 1][dir] = start[dir];
    }
    new_points[start_idx as usize] = start;
    new_points[end_idx as usize] = end;
    if let Some(level) = next_level {
        let dir = usize::from(level);
        new_points[end_idx as usize + 1][dir] = end[dir];
    }
    // Fixed segments next to it line up with it.
    if let Some(k) = next_fixed.iter().position(|s| s.index == start_idx) {
        let dir = usize::from(geom::heading_is_horizontal(
            next_fixed[k].end,
            next_fixed[k].start,
        ));
        next_fixed[k].start[dir] = start[dir];
        next_fixed[k].end = start;
    }
    if let Some(k) = next_fixed.iter().position(|s| s.index == end_idx + 1) {
        let dir = usize::from(geom::heading_is_horizontal(
            next_fixed[k].end,
            next_fixed[k].start,
        ));
        next_fixed[k].end[dir] = end[dir];
        next_fixed[k].start = end;
    }
    let first = [ax + points[0][0], ay + points[0][1]];
    let last = [
        ax + points[points.len() - 1][0],
        ay + points[points.len() - 1][1],
    ];
    if first_fixed.is_none() && start_idx == 0 {
        let level = if hovered_start {
            horizontal(start_heading)
        } else {
            geom::heading_is_horizontal(new_points[1], new_points[0])
        };
        new_points.insert(
            0,
            [
                if level { start[0] } else { first[0] },
                if !level { start[1] } else { first[1] },
            ],
        );
        if hovered_start {
            new_points.insert(0, first);
        }
        for s in &mut next_fixed {
            s.index += if hovered_start { 2 } else { 1 };
        }
    }
    if last_fixed.is_none() && end_idx == n - 1 {
        let level = horizontal(end_heading);
        new_points.push([
            if level { end[0] } else { last[0] },
            if !level { end[1] } else { last[1] },
        ]);
        if hovered_end {
            new_points.push(last);
        }
    }
    let local = next_fixed
        .into_iter()
        .map(|s| FixedSegment {
            index: s.index,
            start: [s.start[0] - ax, s.start[1] - ay],
            end: [s.end[0] - ax, s.end[1] - ay],
        })
        .collect();
    // Moved by hand: no end turns at once any more.
    route_patch(
        new_points,
        Some(local),
        Some(json!(false)),
        Some(json!(false)),
    )
}

/// `Array.prototype.at`.
fn at_js(v: &[Point], i: i64) -> Option<Point> {
    let i = if i < 0 { v.len() as i64 + i } else { i };
    (i >= 0).then(|| v.get(i as usize).copied()).flatten()
}

/// `handleEndpointDrag`: the ends moved with fixed segments in between; the
/// segments next to the ends stretch (or turn, `…IsSpecial`) to reach them.
fn endpoint_drag(arrow: &Element, updated: &[Point], fixed: &[FixedSegment], d: &Data) -> Patch {
    let truthy = |k: &str| arrow.bool(k);
    let mut start_special = truthy("startIsSpecial");
    let mut end_special = truthy("endIsSpecial");
    let (ax, ay) = (arrow.x(), arrow.y());
    let points = arrow.points();
    let n = updated.len();
    let global: Vec<Point> = updated
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let q = if i == 0 || i == n - 1 {
                *p
            } else {
                points.get(i).copied().unwrap_or(*p)
            };
            [ax + q[0], ay + q[1]]
        })
        .collect();
    let o = updated[0];
    let mut next_fixed: Vec<FixedSegment> = fixed
        .iter()
        .map(|s| FixedSegment {
            index: s.index,
            start: [ax + (s.start[0] - o[0]), ay + (s.start[1] - o[1])],
            end: [ax + (s.end[0] - o[0]), ay + (s.end[1] - o[1])],
        })
        .collect();
    let (sg, eg, sh, eh) = (d.start_global, d.end_global, d.start_heading, d.end_heading);
    let is = |s: Option<bool>| s == Some(true);
    let mut new_points: Vec<Point> = Vec::new();
    // The points inside.
    let offset = 2 + i64::from(is(start_special));
    let end_offset = 2 + i64::from(is(end_special));
    while (new_points.len() as i64) + offset < global.len() as i64 - end_offset {
        new_points.push(global[new_points.len() + offset as usize]);
    }
    let pad = |h: Heading| {
        let positive = if horizontal(h) { h == RIGHT } else { h == DOWN };
        if positive {
            BASE_PADDING
        } else {
            -BASE_PADDING
        }
    };
    // The start, and the segment to the second point.
    {
        let (Some(second), Some(third)) = (
            at_js(&global, if is(start_special) { 2 } else { 1 }),
            at_js(&global, if is(start_special) { 3 } else { 2 }),
        ) else {
            return Patch::None;
        };
        let start_level = horizontal(sh);
        let second_level = horizontal(heading_of(geom::vector(second, third)));
        if d.hovered_start.is_some() && start_level == second_level {
            let p = pad(sh);
            new_points.insert(
                0,
                [
                    if !second_level { third[0] } else { sg[0] + p },
                    if second_level { third[1] } else { sg[1] + p },
                ],
            );
            new_points.insert(
                0,
                [
                    if start_level { sg[0] + p } else { sg[0] },
                    if !start_level { sg[1] + p } else { sg[1] },
                ],
            );
            if !is(start_special) {
                start_special = Some(true);
                for s in &mut next_fixed {
                    if s.index > 1 {
                        s.index += 1;
                    }
                }
            }
        } else {
            new_points.insert(
                0,
                [
                    if !second_level { second[0] } else { sg[0] },
                    if second_level { second[1] } else { sg[1] },
                ],
            );
            if is(start_special) {
                start_special = Some(false);
                for s in &mut next_fixed {
                    if s.index > 1 {
                        s.index -= 1;
                    }
                }
            }
        }
        new_points.insert(0, sg);
    }
    // The end, and the segment from the second to last point.
    {
        let len = global.len() as i64;
        let (Some(second_last), Some(third_last)) = (
            at_js(&global, len - if is(end_special) { 3 } else { 2 }),
            at_js(&global, len - if is(end_special) { 4 } else { 3 }),
        ) else {
            return Patch::None;
        };
        let end_level = horizontal(eh);
        let second_level = geom::heading_is_horizontal(third_last, second_last);
        if d.hovered_end.is_some() && end_level == second_level {
            let p = pad(eh);
            new_points.push([
                if !second_level {
                    third_last[0]
                } else {
                    eg[0] + p
                },
                if second_level {
                    third_last[1]
                } else {
                    eg[1] + p
                },
            ]);
            new_points.push([
                if end_level { eg[0] + p } else { eg[0] },
                if !end_level { eg[1] + p } else { eg[1] },
            ]);
            end_special = Some(true);
        } else {
            new_points.push([
                if !second_level { second_last[0] } else { eg[0] },
                if second_level { second_last[1] } else { eg[1] },
            ]);
            if is(end_special) {
                end_special = Some(false);
            }
        }
    }
    new_points.push(eg);
    let fixed: Vec<FixedSegment> = next_fixed
        .iter()
        .filter_map(|s| {
            let start = at_js(&new_points, s.index - 1).filter(|_| s.index >= 1)?;
            let end = at_js(&new_points, s.index).filter(|_| s.index >= 0)?;
            Some(FixedSegment {
                index: s.index,
                start: [start[0] - sg[0], start[1] - sg[1]],
                end: [end[0] - sg[0], end[1] - sg[1]],
            })
        })
        .collect();
    route_patch(
        new_points,
        Some(fixed),
        special(start_special),
        special(end_special),
    )
}

// What the editor calls.

/// The arrow routed again where it is (`updateElbowArrowRoute`): bound ends
/// where their bindings put them on the shapes (with `moved` in place of
/// theirs in `scene`), free ends where they are, fixed segments kept.
pub fn route(arrow: &Element, scene: &Scene, moved: &[Element]) -> Element {
    let points = arrow.points();
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return arrow.clone();
    };
    let changes = Changes {
        points: Some(vec![first, last]),
        ..Changes::default()
    };
    if moved.is_empty() {
        return update(arrow, scene, &changes, false);
    }
    update(arrow, &with_moved(scene, moved), &changes, false)
}

/// The scene with `moved` in place of the elements they are.
fn with_moved(scene: &Scene, moved: &[Element]) -> Scene {
    let mut s = scene.clone();
    for m in moved {
        if let Some(slot) = s.elements.iter_mut().find(|e| e.id() == m.id()) {
            *slot = m.clone();
        }
    }
    s
}

/// A new elbow arrow being drawn from `start` to `end`: routed between the
/// shapes there, its ends snapped onto their outlines.
pub fn drag(arrow: &Element, scene: &Scene, start: Point, end: Point) -> Element {
    let mut a = arrow.clone();
    a.set("x", num(start[0]));
    a.set("y", num(start[1]));
    let changes = Changes {
        points: Some(vec![[0.0, 0.0], [end[0] - start[0], end[1] - start[1]]]),
        ..Changes::default()
    };
    update(&a, scene, &changes, true)
}

/// The arrow tidied where it is (`mutateElement(arrow, {})`): moved segments
/// that came to line up merged, too short ones gone.
pub fn renormalize(arrow: &Element, scene: &Scene) -> Element {
    update(arrow, scene, &Changes::default(), false)
}

/// Elbow arrows attached to `ids` or among them, tidied once a drag is over
/// (those that changed).
pub fn tidy(scene: &Scene, ids: &[&str]) -> Vec<Element> {
    scene
        .visible()
        .filter(|e| e.kind() == ElementType::Arrow && e.is_elbowed())
        .filter(|e| {
            ids.contains(&e.id())
                || [e.start_binding(), e.end_binding()]
                    .iter()
                    .flatten()
                    .any(|b| ids.contains(&b.element_id.as_str()))
        })
        .filter_map(|e| {
            let t = renormalize(e, scene);
            let same = ["x", "y", "points", "fixedSegments"]
                .iter()
                .all(|k| t.get(k) == e.get(k));
            (!same).then_some(t)
        })
        .collect()
}

/// Whether an elbow arrow stays put while `moving` (it among them) are
/// dragged (`dragSelectedElements`): one attached to something isn't dragged
/// on its own, and one attached at both ends only along with both.
pub fn stays(arrow: &Element, moving: &[Element]) -> bool {
    if !arrow.is_elbowed() {
        return false;
    }
    let (start, end) = (arrow.start_binding(), arrow.end_binding());
    let alone = moving
        .iter()
        .all(|m| m.id() == arrow.id() || m.container_id() == Some(arrow.id()));
    if alone && (start.is_some() || end.is_some()) {
        return true;
    }
    let has = |id: &str| moving.iter().any(|m| m.id() == id);
    match (start, end) {
        (Some(s), Some(e)) => !(has(&s.element_id) && has(&e.element_id)),
        _ => false,
    }
}

/// Elbow arrows among `changed` after a resize or a turn: routed again, not
/// turned (`turned`: from where they were, as Excalidraw turns them; else
/// from their resized ends).
pub fn transformed(scene: &Scene, changed: &mut [Element], turned: bool) {
    let snapshot = changed.to_vec();
    for slot in changed.iter_mut() {
        if !(slot.kind() == ElementType::Arrow && slot.is_elbowed()) {
            continue;
        }
        let from = if turned {
            scene
                .element(slot.id())
                .cloned()
                .unwrap_or_else(|| slot.clone())
        } else {
            slot.clone()
        };
        *slot = route(&from, scene, &snapshot);
    }
}

/// The segment of an elbow arrow whose middle handle is at `p`
/// (`getSegmentMidpointHitCoords`): the index of its end point.
pub fn segment_at(arrow: &Element, p: Point, zoom: f64) -> Option<usize> {
    let reach = (HANDLE_SIZE + 1.0) / zoom;
    segment_handles(arrow, zoom)
        .into_iter()
        .find(|(_, m)| geom::distance(*m, p) <= reach)
        .map(|(i, _)| i)
}

/// The handles an elbow arrow's segments are moved by: each segment's index
/// and middle, too short ones left out (`isSegmentTooShort`).
pub fn segment_handles(arrow: &Element, zoom: f64) -> Vec<(usize, Point)> {
    let points = global_points(arrow);
    (1..points.len())
        .filter(|&i| geom::distance(points[i - 1], points[i]) * zoom >= HANDLE_SIZE / 2.0)
        .map(|i| {
            let (a, b) = (points[i - 1], points[i]);
            (i, [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0])
        })
        .collect()
}

/// Whether segment `index` of the arrow was moved by hand.
pub fn is_fixed(arrow: &Element, index: usize) -> bool {
    fixed_segments(arrow).is_some_and(|f| f.iter().any(|s| s.index == index as i64))
}

/// Segment `index` of an elbow arrow dragged to `p` (`moveFixedSegment`):
/// it becomes a fixed segment there. Returns the arrow and the segment's
/// index now (moving the first or last segment adds segments before it).
pub fn move_segment(arrow: &Element, scene: &Scene, index: usize, p: Point) -> (Element, usize) {
    let points = arrow.points();
    if index == 0 || index >= points.len() {
        return (arrow.clone(), index);
    }
    let (a, b) = (points[index - 1], points[index]);
    let level = horizontal(heading_of(geom::vector(b, a)));
    let (lx, ly) = (p[0] - arrow.x(), p[1] - arrow.y());
    let moved = FixedSegment {
        index: index as i64,
        start: [
            if !level { lx } else { a[0] },
            if level { ly } else { a[1] },
        ],
        end: [
            if !level { lx } else { b[0] },
            if level { ly } else { b[1] },
        ],
    };
    let mut segments: Vec<FixedSegment> = fixed_segments(arrow)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.index != moved.index)
        .collect();
    segments.push(moved);
    segments.sort_by_key(|s| s.index);
    let offset = segments.iter().filter(|s| s.index < moved.index).count();
    let changes = Changes {
        fixed_segments: Some(segments),
        ..Changes::default()
    };
    let e = update(arrow, scene, &changes, false);
    let now = fixed_segments(&e)
        .and_then(|f| f.get(offset).map(|s| s.index.max(0) as usize))
        .unwrap_or(index);
    (e, now)
}

/// A moved segment let go (`deleteFixedSegment`): the route around it is
/// made anew.
pub fn release_segment(arrow: &Element, scene: &Scene, index: usize) -> Element {
    let changes = Changes {
        fixed_segments: Some(
            fixed_segments(arrow)
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.index != index as i64)
                .collect(),
        ),
        ..Changes::default()
    };
    update(arrow, scene, &changes, false)
}

/// An arrow made sharp, curved (`round`) or elbow (`changeArrowType`): an
/// elbow arrow is routed between where its ends are, attached to the same
/// shapes; one that was an elbow arrow keeps only its ends.
pub fn set_type(arrow: &Element, scene: &Scene, kind: &str) -> Element {
    let points = arrow.points();
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return arrow.clone();
    };
    let [_, _, _, _, cx, cy] = bounds::absolute_coords(arrow);
    let on = |p: Point| {
        geom::rotate(
            [arrow.x() + p[0], arrow.y() + p[1]],
            [cx, cy],
            arrow.angle(),
        )
    };
    let (start, end) = (on(first), on(last));
    let elbow = kind == "elbow";
    let mut e = arrow.clone();
    if elbow {
        e.set("x", num(start[0]));
        e.set("y", num(start[1]));
        e.set("angle", json!(0));
    }
    e.set(
        "roundness",
        if kind == "round" {
            json!({"type": crate::scene::roundness::PROPORTIONAL_RADIUS})
        } else {
            Value::Null
        },
    );
    e.set("elbowed", json!(elbow));
    if elbow || arrow.is_elbowed() {
        let to = [end[0] - start[0], end[1] - start[1]];
        e.set("points", json!([[0, 0], [num(to[0]), num(to[1])]]));
        if !elbow {
            e.set("x", num(start[0]));
            e.set("y", num(start[1]));
            e.set("width", num(to[0].abs()));
            e.set("height", num(to[1].abs()));
        }
    }
    let mut bindings = [Value::Null, Value::Null];
    for (i, key) in ["startBinding", "endBinding"].into_iter().enumerate() {
        let Some(b) = e.get(key).and_then(Value::as_object).cloned() else {
            continue;
        };
        let target = b
            .get("elementId")
            .and_then(Value::as_str)
            .and_then(|id| bindable_by_id(scene, id));
        let Some(t) = target else {
            if elbow {
                e.set(key, Value::Null);
            }
            continue;
        };
        let fixed = if elbow {
            fixed_point(&e, i == 0, t)
        } else {
            binding::global_to_fixed(t, if i == 0 { start } else { end })
        };
        let mut b = b;
        b.insert("fixedPoint".into(), json!(fixed.map(num)));
        if !elbow {
            b.insert("mode".into(), json!("orbit"));
        }
        bindings[i] = Value::Object(b.clone());
        e.set(key, Value::Object(b));
    }
    if !elbow {
        e.remove("fixedSegments");
        return e;
    }
    e.set("fixedSegments", Value::Null);
    let [sb, eb] = bindings;
    let changes = Changes {
        points: Some(vec![[0.0, 0.0], [end[0] - start[0], end[1] - start[1]]]),
        fixed_segments: None,
        start_binding: Some(sb),
        end_binding: Some(eb),
    };
    update(&e, scene, &changes, false)
}

#[cfg(test)]
mod tests;
