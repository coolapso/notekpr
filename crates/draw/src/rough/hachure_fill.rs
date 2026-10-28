//! Parallel lines filling polygons, with a scan line. Ported from
//! hachure-fill 0.5.2 (MIT, see `crates/draw/PORTED.md`).

use std::f64::consts::PI;

use super::Point;
use crate::js;

/// Rotates `points` in place about `center` by `degrees`.
fn rotate_points(points: &mut [Point], center: Point, degrees: f64) {
    if points.is_empty() {
        return;
    }
    let [cx, cy] = center;
    let angle = (PI / 180.0) * degrees;
    let cos = angle.cos();
    let sin = angle.sin();
    for p in points {
        let [x, y] = *p;
        p[0] = ((x - cx) * cos) - ((y - cy) * sin) + cx;
        p[1] = ((x - cx) * sin) + ((y - cy) * cos) + cy;
    }
}

fn rotate_lines(lines: &mut [[Point; 2]], center: Point, degrees: f64) {
    rotate_points(lines.as_flattened_mut(), center, degrees);
}

/// Lines `hachure_gap` apart at `hachure_angle` degrees filling `polygons`
/// (even-odd), each `[start, end]` (`hachureLines`).
///
/// Like hachure-fill, this rotates the polygons' points in place and back:
/// they come back with the rounding of two rotations, which rough's later
/// uses of the same points (the second pass of a cross-hatch) see too. A
/// `hachure_step_offset` other than 1 draws a line on every step of that
/// size instead of every `hachure_gap`-th step of 1.
pub fn hachure_lines(
    polygons: &mut [Vec<Point>],
    hachure_gap: f64,
    hachure_angle: f64,
    hachure_step_offset: f64,
) -> Vec<[Point; 2]> {
    let angle = hachure_angle;
    let gap = js::max(hachure_gap, 0.1);
    let rotation_center = [0.0, 0.0];
    if js::truthy(angle) {
        for polygon in polygons.iter_mut() {
            rotate_points(polygon, rotation_center, angle);
        }
    }
    let mut lines = straight_hachure_lines(polygons, gap, hachure_step_offset);
    if js::truthy(angle) {
        for polygon in polygons.iter_mut() {
            rotate_points(polygon, rotation_center, -angle);
        }
        rotate_lines(&mut lines, rotation_center, -angle);
    }
    lines
}

/// An edge of a polygon, for the scan.
#[derive(Clone, Copy, Debug)]
struct Edge {
    ymin: f64,
    ymax: f64,
    /// Where the scan line crosses the edge (starts at the lower end).
    x: f64,
    /// dx/dy.
    islope: f64,
}

/// Horizontal lines filling the polygons.
fn straight_hachure_lines(
    polygons: &[Vec<Point>],
    gap: f64,
    hachure_step_offset: f64,
) -> Vec<[Point; 2]> {
    let mut vertex_array: Vec<Vec<Point>> = Vec::new();
    for polygon in polygons {
        // An empty polygon makes hachure-fill throw; it has no lines.
        let (Some(&first), Some(&last)) = (polygon.first(), polygon.last()) else {
            continue;
        };
        let mut vertices = polygon.clone();
        if !(first[0] == last[0] && first[1] == last[1]) {
            vertices.push([first[0], first[1]]);
        }
        if vertices.len() > 2 {
            vertex_array.push(vertices);
        }
    }

    let mut lines = Vec::new();
    let gap = js::max(gap, 0.1);

    // The edges, sorted by where they start.
    let mut edges: Vec<Edge> = Vec::new();
    for vertices in &vertex_array {
        for pair in vertices.windows(2) {
            let (p1, p2) = (pair[0], pair[1]);
            if p1[1] != p2[1] {
                let ymin = js::min(p1[1], p2[1]);
                edges.push(Edge {
                    ymin,
                    ymax: js::max(p1[1], p2[1]),
                    x: if ymin == p1[1] { p1[0] } else { p2[0] },
                    islope: (p2[0] - p1[0]) / (p2[1] - p1[1]),
                });
            }
        }
    }
    // An edge that doesn't start or end at a finite height makes the scan
    // below run forever in hachure-fill.
    if edges
        .iter()
        .any(|e| !e.ymin.is_finite() || !e.ymax.is_finite())
    {
        return lines;
    }
    js::sort_by(&mut edges, |e1, e2| {
        if e1.ymin < e2.ymin {
            return -1.0;
        }
        if e1.ymin > e2.ymin {
            return 1.0;
        }
        if e1.x < e2.x {
            return -1.0;
        }
        if e1.x > e2.x {
            return 1.0;
        }
        if e1.ymax == e2.ymax {
            return 0.0;
        }
        (e1.ymax - e2.ymax) / (e1.ymax - e2.ymax).abs()
    });
    if edges.is_empty() {
        return lines;
    }

    // Scan upwards, keeping the edges the scan line crosses.
    let mut active: Vec<Edge> = Vec::new();
    let mut next = 0;
    let mut y = edges[0].ymin;
    let mut iteration = 0.0;
    while !active.is_empty() || next < edges.len() {
        if next < edges.len() {
            // (No NaN here: the edges and so `y` are finite.)
            while next < edges.len() && edges[next].ymin <= y {
                active.push(edges[next]);
                next += 1;
            }
        }
        active.retain(|e| e.ymax > y);
        js::sort_by(&mut active, |a, b| {
            if a.x == b.x {
                return 0.0;
            }
            (a.x - b.x) / (a.x - b.x).abs()
        });

        // Fill between pairs of edges.
        if (hachure_step_offset != 1.0 || iteration % gap == 0.0) && active.len() > 1 {
            for [ce, ne] in active.as_chunks::<2>().0 {
                lines.push([[js::round(ce.x), y], [js::round(ne.x), y]]);
            }
        }

        let next_y = y + hachure_step_offset;
        if next_y == y {
            // Too far out to move on (or a step of 0): hachure-fill would
            // never finish.
            break;
        }
        y = next_y;
        for e in &mut active {
            e.x += hachure_step_offset * e.islope;
        }
        iteration += 1.0;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<Point> {
        vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]
    }

    #[test]
    fn horizontal_lines_every_gap() {
        let mut polygons = vec![square()];
        let lines = hachure_lines(&mut polygons, 4.0, 0.0, 1.0);
        // Rows 0, 4 and 8 (10 is the top edge, where the sides end).
        assert_eq!(
            lines,
            [
                [[0.0, 0.0], [10.0, 0.0]],
                [[0.0, 4.0], [10.0, 4.0]],
                [[0.0, 8.0], [10.0, 8.0]],
            ]
        );
        assert_eq!(polygons, [square()]);
    }

    #[test]
    fn a_step_offset_draws_every_step() {
        let mut polygons = vec![square()];
        let lines = hachure_lines(&mut polygons, 4.0, 0.0, 4.0);
        let ys: Vec<f64> = lines.iter().map(|l| l[0][1]).collect();
        assert_eq!(ys, [0.0, 4.0, 8.0]);
        let lines = hachure_lines(&mut polygons, 3.0, 0.0, 3.0);
        let ys: Vec<f64> = lines.iter().map(|l| l[0][1]).collect();
        assert_eq!(ys, [0.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn rotated_lines_and_polygons_rotated_back() {
        let mut polygons = vec![square()];
        let lines = hachure_lines(&mut polygons, 2.0, 90.0, 1.0);
        // At 90° the lines run vertically through the square.
        assert!(!lines.is_empty());
        for [a, b] in &lines {
            assert!((a[0] - b[0]).abs() < 1e-9, "{a:?} {b:?}");
            assert!((0.0..=10.0 + 1e-9).contains(&a[0]));
        }
        // The polygon is back where it was, up to rounding.
        for (p, q) in polygons[0].iter().zip(square()) {
            assert!((p[0] - q[0]).abs() < 1e-12 && (p[1] - q[1]).abs() < 1e-12);
        }
    }

    #[test]
    fn holes_are_left_empty() {
        let outer = vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]];
        let hole = vec![[5.0, 5.0], [15.0, 5.0], [15.0, 15.0], [5.0, 15.0]];
        let lines = hachure_lines(&mut [outer, hole], 4.0, 0.0, 1.0);
        let at_8: Vec<_> = lines.iter().filter(|l| l[0][1] == 8.0).collect();
        assert_eq!(
            at_8,
            [&[[0.0, 8.0], [5.0, 8.0]], &[[15.0, 8.0], [20.0, 8.0]]]
        );
    }

    #[test]
    fn degenerate_polygons_have_no_lines() {
        assert!(hachure_lines(&mut [vec![]], 4.0, 45.0, 1.0).is_empty());
        assert!(hachure_lines(&mut [vec![[1.0, 1.0], [1.0, 1.0]]], 4.0, 45.0, 1.0).is_empty());
        let mut nan = vec![vec![[0.0, 0.0], [f64::NAN, 10.0], [10.0, f64::NAN]]];
        assert!(hachure_lines(&mut nan, 4.0, 0.0, 1.0).is_empty());
    }
}
