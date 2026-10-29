//! What is under the pointer, as Excalidraw decides it (`collision.ts`):
//! a filled shape anywhere inside, an empty one only near its outline;
//! lines, arrows and pen strokes near their path; texts and pictures
//! anywhere in their box. The topmost wins.

use crate::bounds;
use crate::color;
use crate::geom::{self, Point};
use crate::rough::{OpType, points_on_curve};
use crate::scene::{Element, ElementType, Scene};
use crate::shape::{self, ElementShape, RenderConfig};

/// How close counts, in scene units, at `zoom` (Excalidraw: 10 screen
/// pixels, or half the stroke).
pub fn threshold(e: &Element, zoom: f64) -> f64 {
    (e.stroke_width() / 2.0 + 0.1).max(10.0 / zoom)
}

/// The point in the element's own frame: unrotated, its top left at 0,0.
pub fn local(e: &Element, p: Point) -> Point {
    let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(e);
    let center = [(x1 + x2) / 2.0, (y1 + y2) / 2.0];
    let [x, y] = geom::rotate(p, center, -e.angle());
    [x - e.x(), y - e.y()]
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let ab = geom::vector(b, a);
    let len2 = geom::dot(ab, ab);
    if len2 == 0.0 {
        return geom::distance(p, a);
    }
    let t = (geom::dot(geom::vector(p, a), ab) / len2).clamp(0.0, 1.0);
    geom::distance(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

fn polyline_distance(p: Point, points: &[Point]) -> f64 {
    match points {
        [] => f64::INFINITY,
        [only] => geom::distance(p, *only),
        _ => points
            .windows(2)
            .map(|w| segment_distance(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min),
    }
}

/// Whether `p` is inside the polygon `points` (even-odd).
pub fn inside_polygon(p: Point, points: &[Point]) -> bool {
    let mut inside = false;
    let n = points.len();
    for i in 0..n {
        let (a, b) = (points[i], points[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
    }
    inside
}

/// The path a line or arrow follows, as points (its shaft without the
/// wobble).
pub fn linear_path(e: &Element) -> Vec<Point> {
    let mut plain = e.clone();
    plain.set("roughness", serde_json::json!(0));
    let ElementShape::Rough(shapes) = shape::element_shape(&plain, &RenderConfig::default()) else {
        return e.points();
    };
    let Some(shaft) = shapes.first() else {
        return e.points();
    };
    let Some(set) = shaft.sets.first() else {
        return e.points();
    };
    // Each op continues from the last: Bézier curves flattened. A solid
    // line is drawn twice over: the first stroke is its path.
    let mut out: Vec<Point> = Vec::new();
    let mut at = [0.0, 0.0];
    for op in &set.ops {
        let d = &op.data;
        match op.op {
            OpType::Move if !out.is_empty() => break,
            OpType::Move => {
                at = [d[0], d[1]];
                out.push(at);
            }
            OpType::LineTo => {
                at = [d[0], d[1]];
                out.push(at);
            }
            OpType::BCurveTo => {
                let curve = [at, [d[0], d[1]], [d[2], d[3]], [d[4], d[5]]];
                out.extend(
                    points_on_curve::points_on_bezier_curves(&curve, 0.5, None)
                        .into_iter()
                        .skip(1),
                );
                at = [d[4], d[5]];
            }
        }
    }
    out
}

/// Whether the element is hit at `p` (scene coordinates).
pub fn hits(e: &Element, p: Point, zoom: f64) -> bool {
    let t = threshold(e, zoom);
    let [lx, ly] = local(e, p);
    let (w, h) = (e.width(), e.height());
    let filled = !color::is_transparent(e.background_color());
    match e.kind() {
        ElementType::Rectangle
        | ElementType::Iframe
        | ElementType::Embeddable
        | ElementType::Image
        | ElementType::StickyNote => {
            let inside = lx >= 0.0 && ly >= 0.0 && lx <= w && ly <= h;
            if inside
                && (filled || matches!(e.kind(), ElementType::Image | ElementType::StickyNote))
            {
                return true;
            }
            let outline = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h], [0.0, 0.0]];
            polyline_distance([lx, ly], &outline) <= t
        }
        ElementType::Diamond => {
            let [top, right, bottom, left] = shape::diamond_points(e);
            let outline = [top, right, bottom, left, top];
            (filled && inside_polygon([lx, ly], &outline[..4]))
                || polyline_distance([lx, ly], &outline) <= t
        }
        ElementType::Ellipse => {
            let (rx, ry) = (w / 2.0, h / 2.0);
            if rx <= 0.0 || ry <= 0.0 {
                return geom::distance([lx, ly], [rx, ry]) <= t;
            }
            let (dx, dy) = (lx - rx, ly - ry);
            let r = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
            (filled && r <= 1.0) || (r - 1.0).abs() * rx.min(ry) <= t
        }
        ElementType::Line | ElementType::Arrow => {
            let path = linear_path(e);
            let closed = shape::is_path_a_loop(&e.points()) && filled;
            (closed && inside_polygon([lx, ly], &path)) || polyline_distance([lx, ly], &path) <= t
        }
        ElementType::Freedraw => {
            let points = e.points();
            let closed = shape::is_path_a_loop(&points) && filled;
            (closed && inside_polygon([lx, ly], &points))
                || polyline_distance([lx, ly], &points) <= t + e.stroke_width()
        }
        ElementType::Text => lx >= -t && ly >= -t && lx <= w + t && ly <= h + t,
        ElementType::Frame | ElementType::MagicFrame => {
            // Its border, or its name above it (its width guessed).
            let outline = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h], [0.0, 0.0]];
            polyline_distance([lx, ly], &outline) <= t
                || super::frame::on_name(e, p, zoom, &super::guess_width, t)
        }
        _ => false,
    }
}

/// The topmost element at `p`; locked ones only if `locked_too`. A label
/// counts as its container.
pub fn element_at(scene: &Scene, p: Point, zoom: f64, locked_too: bool) -> Option<&Element> {
    let hit = scene
        .elements
        .iter()
        .rev()
        .filter(|e| !e.is_deleted() && (locked_too || !e.is_locked()))
        .find(|e| hits(e, p, zoom) && !super::frame::clipped_away(scene, e, p))?;
    match hit.container_id().and_then(|c| scene.element(c)) {
        Some(container) if !container.is_deleted() => Some(container),
        _ => Some(hit),
    }
}

/// The topmost shape or arrow whose area holds `p`, filled or not: where a
/// double click puts a label (`getTextBindableContainerAtPosition`).
pub fn container_at(scene: &Scene, p: Point, zoom: f64) -> Option<&Element> {
    scene
        .elements
        .iter()
        .rev()
        .filter(|e| !e.is_deleted() && !e.is_locked() && super::label::can_hold(e))
        .find(|e| {
            if e.kind() == ElementType::Arrow {
                return hits(e, p, zoom);
            }
            let [lx, ly] = local(e, p);
            let (w, h) = (e.width(), e.height());
            match e.kind() {
                ElementType::Ellipse => {
                    let (rx, ry) = (w / 2.0, h / 2.0);
                    rx > 0.0
                        && ry > 0.0
                        && ((lx - rx) / rx).powi(2) + ((ly - ry) / ry).powi(2) <= 1.0
                }
                ElementType::Diamond => {
                    let [t, r, b, l] = crate::shape::diamond_points(e);
                    inside_polygon([lx, ly], &[t, r, b, l])
                }
                _ => lx >= 0.0 && ly >= 0.0 && lx <= w && ly <= h,
            }
        })
}

/// The elements wholly inside a box (scene coordinates), labels left out;
/// what is in a frame by what of it shows.
pub fn elements_in(scene: &Scene, [x1, y1, x2, y2]: [f64; 4]) -> Vec<&Element> {
    let (ax, bx) = (x1.min(x2), x1.max(x2));
    let (ay, by) = (y1.min(y2), y1.max(y2));
    scene
        .visible()
        .filter(|e| !e.is_locked() && e.container_id().is_none())
        .filter(|e| {
            let [ex1, ey1, ex2, ey2] = super::frame::shown_bounds(scene, e);
            ex1 >= ax && ey1 >= ay && ex2 <= bx && ey2 <= by
        })
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
    fn filled_inside_empty_on_the_outline() {
        let empty = el(
            json!({"id": "a", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50,
            "backgroundColor": "transparent", "strokeWidth": 2}),
        );
        assert!(!hits(&empty, [50.0, 25.0], 1.0));
        assert!(hits(&empty, [0.0, 25.0], 1.0));
        assert!(hits(&empty, [98.0, 48.0], 1.0));
        let full = el(
            json!({"id": "b", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50,
            "backgroundColor": "#ffc9c9"}),
        );
        assert!(hits(&full, [50.0, 25.0], 1.0));
        assert!(!hits(&full, [150.0, 25.0], 1.0));
        // Zoomed out, the pointer reaches further.
        assert!(hits(&empty, [-30.0, 25.0], 0.2));
    }

    #[test]
    fn ellipses_diamonds_rotation() {
        let e = el(
            json!({"id": "e", "type": "ellipse", "x": 0, "y": 0, "width": 100, "height": 50,
            "backgroundColor": "transparent"}),
        );
        assert!(hits(&e, [50.0, 0.0], 1.0));
        assert!(!hits(&e, [50.0, 25.0], 1.0));
        let d = el(
            json!({"id": "d", "type": "diamond", "x": 0, "y": 0, "width": 100, "height": 100,
            "backgroundColor": "#fff"}),
        );
        assert!(hits(&d, [50.0, 50.0], 1.0));
        assert!(!hits(&d, [5.0, 5.0], 1.0));
        // A tall rectangle turned a quarter is wide.
        let r = el(
            json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 20, "height": 100,
            "backgroundColor": "#fff", "angle": std::f64::consts::FRAC_PI_2}),
        );
        assert!(hits(&r, [-30.0, 50.0], 1.0));
        assert!(!hits(&r, [10.0, 5.0], 1.0));
    }

    #[test]
    fn lines_and_topmost() {
        let line = el(
            json!({"id": "l", "type": "line", "x": 0, "y": 0, "width": 100, "height": 0,
            "points": [[0, 0], [100, 0]], "seed": 1}),
        );
        assert!(hits(&line, [50.0, 3.0], 1.0));
        assert!(!hits(&line, [50.0, 30.0], 1.0));
        let under = el(
            json!({"id": "u", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 100,
            "backgroundColor": "#fff"}),
        );
        let over = el(
            json!({"id": "o", "type": "rectangle", "x": 20, "y": 20, "width": 10, "height": 10,
            "backgroundColor": "#fff"}),
        );
        let label = el(
            json!({"id": "t", "type": "text", "x": 40, "y": 40, "width": 20, "height": 10,
            "containerId": "u", "text": "x"}),
        );
        let scene = Scene {
            elements: vec![under, over, label],
            ..Scene::default()
        };
        assert_eq!(
            element_at(&scene, [25.0, 25.0], 1.0, false).map(Element::id),
            Some("o")
        );
        assert_eq!(
            element_at(&scene, [80.0, 80.0], 1.0, false).map(Element::id),
            Some("u")
        );
        // A click on a label selects its container.
        assert_eq!(
            element_at(&scene, [45.0, 45.0], 1.0, false).map(Element::id),
            Some("u")
        );
        assert_eq!(elements_in(&scene, [10.0, 10.0, 40.0, 40.0]).len(), 1);
    }
}
