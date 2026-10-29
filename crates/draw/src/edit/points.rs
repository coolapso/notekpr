//! The points of lines and arrows, edited one by one, as Excalidraw's line
//! editor does it (`linearElementEditor.ts`; MIT, see `PORTED.md`): handles
//! on the points, a handle halfway along short segments that adds a point
//! when dragged, points moved, added and deleted with the element staying
//! where it was (also turned), and a line closed into a loop when its ends
//! meet. Where a label sits along an arrow (`labelPosition`) too.

use serde_json::{Value, json};

use crate::bounds;
use crate::geom::{self, Point};
use crate::scene::{Element, ElementType, num};

/// `POINT_HANDLE_SIZE`: a point's handle, in screen pixels.
pub const HANDLE: f64 = 10.0;

/// Whether an element is a line or an arrow.
pub fn is_linear(e: &Element) -> bool {
    matches!(e.kind(), ElementType::Line | ElementType::Arrow)
}

/// Whether an arrow is an elbow arrow (its points are routed, only its
/// ends can be moved).
pub fn is_elbow(e: &Element) -> bool {
    e.kind() == ElementType::Arrow && e.get("elbowed").and_then(Value::as_bool) == Some(true)
}

fn center(e: &Element) -> Point {
    let [_, _, _, _, cx, cy] = bounds::absolute_coords(e);
    [cx, cy]
}

/// The points on the drawing (`getPointsGlobalCoordinates`).
pub fn global(e: &Element) -> Vec<Point> {
    let c = center(e);
    e.points()
        .iter()
        .map(|[x, y]| geom::rotate([e.x() + x, e.y() + y], c, e.angle()))
        .collect()
}

/// A point of the drawing in the element's own frame (`createPointAt`).
pub fn local(e: &Element, p: Point) -> Point {
    let [x, y] = geom::rotate(p, center(e), -e.angle());
    [x - e.x(), y - e.y()]
}

/// Which point's handle is at `p`, the last drawn on top first
/// (`getPointIndexUnderCursor`); an elbow arrow's ends only.
pub fn point_at(e: &Element, p: Point, zoom: f64) -> Option<usize> {
    let pts = global(e);
    let n = pts.len();
    (0..n).rev().find(|&i| {
        (!is_elbow(e) || i == 0 || i == n - 1) && geom::distance(p, pts[i]) * zoom < HANDLE + 1.0
    })
}

/// The handles halfway along each segment that add a point when dragged
/// (`getEditorMidPoints`): while editing, on lines of two points and on
/// labelled arrows; `None` for segments too short to have one.
pub fn midpoints(e: &Element, editing: bool, labelled: bool, zoom: f64) -> Vec<Option<Point>> {
    let pts = global(e);
    if is_elbow(e) || (!editing && pts.len() > 2 && !labelled) {
        return Vec::new();
    }
    let path = path_of(e);
    pts.windows(2)
        .map(|w| {
            let curved = pts.len() > 2 && e.roundness().is_some();
            let (len, mid) = if curved {
                // Along the curve between the two points.
                let part = segment_of(&path, w[0], w[1]);
                (length(&part), along(&part, 0.5))
            } else {
                (
                    geom::distance(w[0], w[1]),
                    Some([(w[0][0] + w[1][0]) / 2.0, (w[0][1] + w[1][1]) / 2.0]),
                )
            };
            (len * zoom >= HANDLE * 4.0).then_some(mid).flatten()
        })
        .collect()
}

/// The path as drawn (curves flattened), on the drawing.
fn path_of(e: &Element) -> Vec<Point> {
    let c = center(e);
    super::hit::linear_path(e)
        .iter()
        .map(|[x, y]| geom::rotate([e.x() + x, e.y() + y], c, e.angle()))
        .collect()
}

/// The part of `path` from the point nearest `a` to the one nearest `b`.
fn segment_of(path: &[Point], a: Point, b: Point) -> Vec<Point> {
    let nearest = |q: Point| {
        (0..path.len())
            .min_by(|&i, &j| geom::distance(path[i], q).total_cmp(&geom::distance(path[j], q)))
            .unwrap_or(0)
    };
    let (i, j) = (nearest(a), nearest(b));
    if i <= j {
        path[i..=j].to_vec()
    } else {
        let mut v = path[j..=i].to_vec();
        v.reverse();
        v
    }
}

fn length(path: &[Point]) -> f64 {
    path.windows(2).map(|w| geom::distance(w[0], w[1])).sum()
}

/// The point `t` (0 to 1) of the way along `path`.
fn along(path: &[Point], t: f64) -> Option<Point> {
    let total = length(path);
    let first = *path.first()?;
    if total == 0.0 {
        return Some(first);
    }
    let mut left = t.clamp(0.0, 1.0) * total;
    for w in path.windows(2) {
        let len = geom::distance(w[0], w[1]);
        if left <= len && len > 0.0 {
            let f = left / len;
            return Some([
                w[0][0] + (w[1][0] - w[0][0]) * f,
                w[0][1] + (w[1][1] - w[0][1]) * f,
            ]);
        }
        left -= len;
    }
    path.last().copied()
}

/// The point `t` (0 to 1) of the way along an arrow's path
/// (`getPointAtPathParameter`), on the drawing.
pub fn point_along(e: &Element, t: f64) -> Option<Point> {
    along(&path_of(e), t)
}

/// How far along an arrow's path (0 to 1) the point nearest `p` is
/// (`getPathParameterAtPoint`).
pub fn parameter_at(e: &Element, p: Point) -> Option<f64> {
    let path = path_of(e);
    let total = length(&path);
    if path.len() < 2 || total == 0.0 {
        return None;
    }
    let (mut best, mut best_at, mut walked) = (f64::INFINITY, 0.0, 0.0);
    for w in path.windows(2) {
        let ab = geom::vector(w[1], w[0]);
        let len2 = geom::dot(ab, ab);
        let len = len2.sqrt();
        let f = if len2 == 0.0 {
            0.0
        } else {
            (geom::dot(geom::vector(p, w[0]), ab) / len2).clamp(0.0, 1.0)
        };
        let d = geom::distance(p, [w[0][0] + ab[0] * f, w[0][1] + ab[1] * f]);
        if d < best {
            best = d;
            best_at = walked + len * f;
        }
        walked += len;
    }
    Some((best_at / total).clamp(0.0, 1.0))
}

/// New local points for the element (the first need not be at 0,0 any
/// more): they are made to start at 0,0 again and the element moves so
/// that nothing else shifts, also when it is turned (`_updatePoints`).
pub fn set_points(e: &Element, next: &[Point]) -> Element {
    let mut out = e.clone();
    let Some(&[ox, oy]) = next.first() else {
        return out;
    };
    let normalized: Vec<Point> = next.iter().map(|[x, y]| [x - ox, y - oy]).collect();
    let mut probe = e.clone();
    write_points(&mut probe, &normalized);
    let [px1, py1, px2, py2, _, _] = bounds::absolute_coords(e);
    let [nx1, ny1, nx2, ny2, _, _] = bounds::absolute_coords(&probe);
    // The box's centre moves by the change of points: turned with the
    // element, the offset keeps the rest in place.
    let dx = (px1 - e.x() + px2 - e.x()) / 2.0 - (nx1 - e.x() + nx2 - e.x()) / 2.0;
    let dy = (py1 - e.y() + py2 - e.y()) / 2.0 - (ny1 - e.y() + ny2 - e.y()) / 2.0;
    let [rx, ry] = geom::rotate([ox, oy], [dx, dy], e.angle());
    write_points(&mut out, &normalized);
    out.set("x", num(e.x() + rx));
    out.set("y", num(e.y() + ry));
    out
}

/// Sets the points and the size they span (`getSizeFromPoints`).
fn write_points(e: &mut Element, points: &[Point]) {
    let [x1, y1, x2, y2] = bounds::points_bounds(points);
    e.set(
        "points",
        Value::Array(
            points
                .iter()
                .map(|[x, y]| json!([num(*x), num(*y)]))
                .collect(),
        ),
    );
    e.set("width", num(x2 - x1));
    e.set("height", num(y2 - y1));
}

/// Whether a line is closed into a shape (`polygon`).
pub fn is_polygon(e: &Element) -> bool {
    e.kind() == ElementType::Line && e.get("polygon").and_then(Value::as_bool) == Some(true)
}

/// Moves points to new local positions (`movePoints`); a closed line's
/// ends move together.
pub fn move_points(e: &Element, moves: &[(usize, Point)]) -> Element {
    let mut pts = e.points();
    let n = pts.len();
    let mut moves = moves.to_vec();
    if is_polygon(e) && n > 1 {
        let first = moves.iter().find(|(i, _)| *i == 0).map(|(_, p)| *p);
        let last = moves.iter().find(|(i, _)| *i == n - 1).map(|(_, p)| *p);
        match (first, last) {
            (Some(p), _) => moves.push((n - 1, p)),
            (None, Some(p)) => moves.push((0, p)),
            _ => {}
        }
    }
    for (i, p) in moves {
        if let Some(slot) = pts.get_mut(i) {
            *slot = p;
        }
    }
    set_points(e, &pts)
}

/// Deletes points (`deletePoints`); a closed line stays closed.
pub fn delete_points(e: &Element, indices: &[usize]) -> Element {
    let mut pts: Vec<Point> = e
        .points()
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !indices.contains(i))
        .map(|(_, p)| p)
        .collect();
    let n = e.points().len();
    if is_polygon(e)
        && pts.len() > 1
        && (indices.contains(&0) || indices.contains(&(n.saturating_sub(1))))
    {
        pts[0] = pts[pts.len() - 1];
    }
    set_points(e, &pts)
}

/// Adds a point at `p` (local) before point `index` (`addMidpoint`).
pub fn insert_point(e: &Element, index: usize, p: Point) -> Element {
    let mut pts = e.points();
    pts.insert(index.min(pts.len()), p);
    set_points(e, &pts)
}

/// A line whose ends meet becomes a closed shape (`isPathALoop` at the
/// end of a drag): the moved end lands exactly on the other.
pub fn close_if_loop(e: &Element, moved: &[usize], zoom: f64) -> Option<Element> {
    let pts = e.points();
    let n = pts.len();
    if n < 3 || !(moved.contains(&0) || moved.contains(&(n - 1))) {
        return None;
    }
    // `LINE_CONFIRM_THRESHOLD`, in screen pixels.
    let threshold = 8.0 / zoom;
    if geom::distance(pts[0], pts[n - 1]) > threshold {
        return None;
    }
    let mut out = if moved.contains(&0) {
        move_points(e, &[(0, pts[n - 1])])
    } else {
        move_points(e, &[(n - 1, pts[0])])
    };
    if e.kind() == ElementType::Line {
        out.set("polygon", json!(true));
    }
    Some(out)
}

/// The angle a dragged point keeps with Shift: steps of 15° from the point
/// next to it (`_getShiftLockedDelta`), in local coordinates.
pub fn locked_angle(pivot: Point, p: Point) -> Point {
    let (dx, dy) = (p[0] - pivot[0], p[1] - pivot[1]);
    let len = dx.hypot(dy);
    let step = std::f64::consts::PI / 12.0;
    let a = (dy.atan2(dx) / step).round() * step;
    [pivot[0] + len * a.cos(), pivot[1] + len * a.sin()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(v: serde_json::Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    fn line(points: serde_json::Value) -> Element {
        el(
            json!({"id": "l", "type": "line", "x": 100, "y": 100, "width": 0, "height": 0,
            "points": points, "seed": 1, "roughness": 0, "roundness": null}),
        )
    }

    #[test]
    fn moving_the_first_point_keeps_the_rest() {
        let l = line(json!([[0, 0], [100, 0], [100, 50]]));
        let m = move_points(&l, &[(0, [-20.0, 10.0])]);
        assert_eq!(m.points()[0], [0.0, 0.0]);
        assert_eq!((m.x(), m.y()), (80.0, 110.0));
        // The other points stay where they were on the drawing.
        assert_eq!(global(&m)[1], [200.0, 100.0]);
        assert_eq!(global(&m)[2], [200.0, 150.0]);
        assert_eq!((m.width(), m.height()), (120.0, 50.0));
    }

    #[test]
    fn a_turned_line_keeps_its_other_points() {
        let mut l = line(json!([[0, 0], [100, 0], [100, 100]]));
        l.set("angle", json!(0.7));
        let before = global(&l);
        let target = local(&l, [before[0][0] - 30.0, before[0][1] + 5.0]);
        let m = move_points(&l, &[(0, target)]);
        let after = global(&m);
        for i in 1..3 {
            assert!(geom::distance(before[i], after[i]) < 1e-6, "{i}: {after:?}");
        }
        assert!(geom::distance(after[0], [before[0][0] - 30.0, before[0][1] + 5.0]) < 1e-6);
    }

    #[test]
    fn handles_points_and_midpoints() {
        let l = line(json!([[0, 0], [200, 0]]));
        assert_eq!(point_at(&l, [301.0, 102.0], 1.0), Some(1));
        assert_eq!(point_at(&l, [200.0, 100.0], 1.0), None);
        // Two points: one handle halfway; three points only while editing.
        assert_eq!(midpoints(&l, false, false, 1.0), [Some([200.0, 100.0])]);
        let l3 = line(json!([[0, 0], [200, 0], [200, 200]]));
        assert!(midpoints(&l3, false, false, 1.0).is_empty());
        assert_eq!(midpoints(&l3, true, false, 1.0).len(), 2);
        // Too short to have one when zoomed out.
        assert_eq!(midpoints(&l, false, false, 0.1), [None]);
    }

    #[test]
    fn points_are_added_and_deleted() {
        let l = line(json!([[0, 0], [200, 0]]));
        let a = insert_point(&l, 1, [100.0, 50.0]);
        assert_eq!(a.points(), [[0.0, 0.0], [100.0, 50.0], [200.0, 0.0]]);
        let d = delete_points(&a, &[0]);
        assert_eq!(d.points(), [[0.0, 0.0], [100.0, -50.0]]);
        assert_eq!((d.x(), d.y()), (200.0, 150.0));
    }

    #[test]
    fn ends_that_meet_close_the_line() {
        let l = line(json!([[0, 0], [100, 0], [100, 100], [3, 4]]));
        let c = close_if_loop(&l, &[3], 1.0).unwrap();
        assert!(is_polygon(&c));
        assert_eq!(c.points()[3], [0.0, 0.0]);
        assert!(close_if_loop(&l, &[1], 1.0).is_none());
        // A closed line's ends move together.
        let m = move_points(&c, &[(0, [10.0, 10.0])]);
        assert_eq!(m.points()[0], m.points()[3]);
    }

    #[test]
    fn labels_go_along_the_path() {
        let a = el(
            json!({"id": "a", "type": "arrow", "x": 0, "y": 0, "width": 200, "height": 0,
            "points": [[0, 0], [200, 0]], "seed": 1, "roughness": 0}),
        );
        assert_eq!(point_along(&a, 0.25), Some([50.0, 0.0]));
        let t = parameter_at(&a, [150.0, 30.0]).unwrap();
        assert!((t - 0.75).abs() < 1e-9, "{t}");
    }
}
