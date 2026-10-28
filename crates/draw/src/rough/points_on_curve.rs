//! Points along Bézier curves, and simplifying a polyline. Ported from
//! points-on-curve 0.2.0 / 1.0.1 (MIT, see `crates/draw/PORTED.md`); the
//! two versions differ only in 1.0.1 exporting `simplifyPoints`.

use super::Point;
use crate::js;

/// Deeper than any curve with finite coordinates needs: each split divides
/// the flatness measure by about 16.
const MAX_SPLITS: u32 = 64;

fn distance(p1: Point, p2: Point) -> f64 {
    distance_sq(p1, p2).sqrt()
}

fn distance_sq(p1: Point, p2: Point) -> f64 {
    // `Math.pow(d, 2)` is exactly `d * d` (fdlibm special-cases it).
    let dx = p1[0] - p2[0];
    let dy = p1[1] - p2[1];
    dx * dx + dy * dy
}

/// The squared distance from `p` to the segment `v`–`w`.
fn distance_to_segment_sq(p: Point, v: Point, w: Point) -> f64 {
    let l2 = distance_sq(v, w);
    if l2 == 0.0 {
        return distance_sq(p, v);
    }
    let mut t = ((p[0] - v[0]) * (w[0] - v[0]) + (p[1] - v[1]) * (w[1] - v[1])) / l2;
    t = js::max(0.0, js::min(1.0, t));
    distance_sq(p, lerp(v, w, t))
}

fn lerp(a: Point, b: Point, t: f64) -> Point {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// How far the curve at `points[offset..offset + 4]` is from a straight
/// line (squared, scaled).
fn flatness(points: &[Point], offset: usize) -> f64 {
    let p1 = points[offset];
    let p2 = points[offset + 1];
    let p3 = points[offset + 2];
    let p4 = points[offset + 3];
    let mut ux = 3.0 * p2[0] - 2.0 * p1[0] - p4[0];
    ux *= ux;
    let mut uy = 3.0 * p2[1] - 2.0 * p1[1] - p4[1];
    uy *= uy;
    let mut vx = 3.0 * p3[0] - 2.0 * p4[0] - p1[0];
    vx *= vx;
    let mut vy = 3.0 * p3[1] - 2.0 * p4[1] - p1[1];
    vy *= vy;
    if ux < vx {
        ux = vx;
    }
    if uy < vy {
        uy = vy;
    }
    ux + uy
}

/// Splits the curve at `points[offset..offset + 4]` in halves until each
/// part is flat enough, adding the ends of the parts to `out`. A curve with
/// non-finite coordinates is never flat (points-on-curve recurses until the
/// stack runs out); it is taken as flat instead.
fn points_on_bezier_curve_with_splitting(
    points: &[Point],
    offset: usize,
    tolerance: f64,
    out: &mut Vec<Point>,
    depth: u32,
) {
    let flat = flatness(points, offset);
    if flat < tolerance || !flat.is_finite() || depth >= MAX_SPLITS {
        let p0 = points[offset];
        if let Some(&last) = out.last() {
            if distance(last, p0) > 1.0 {
                out.push(p0);
            }
        } else {
            out.push(p0);
        }
        out.push(points[offset + 3]);
    } else {
        let t = 0.5;
        let p1 = points[offset];
        let p2 = points[offset + 1];
        let p3 = points[offset + 2];
        let p4 = points[offset + 3];
        let q1 = lerp(p1, p2, t);
        let q2 = lerp(p2, p3, t);
        let q3 = lerp(p3, p4, t);
        let r1 = lerp(q1, q2, t);
        let r2 = lerp(q2, q3, t);
        let red = lerp(r1, r2, t);
        points_on_bezier_curve_with_splitting(&[p1, q1, r1, red], 0, tolerance, out, depth + 1);
        points_on_bezier_curve_with_splitting(&[red, r2, q3, p4], 0, tolerance, out, depth + 1);
    }
}

/// `points` with the ones that hardly change its shape left out
/// (Ramer–Douglas–Peucker): no point of the result's segments is further
/// than `distance` from the original line.
pub fn simplify(points: &[Point], distance: f64) -> Vec<Point> {
    let mut out = Vec::new();
    simplify_points(points, 0, points.len(), distance, &mut out);
    out
}

/// [`simplify`] of `points[start..end]`, added to `out` (`simplifyPoints`;
/// its first point only when `out` is empty).
///
/// The recursion of points-on-curve runs here on a stack of its own, in the
/// same order, so long polylines can't overflow the thread's stack. A
/// negative `epsilon` makes points-on-curve recurse forever; here it acts
/// as 0.
pub fn simplify_points(
    points: &[Point],
    start: usize,
    end: usize,
    epsilon: f64,
    out: &mut Vec<Point>,
) {
    let end = end.min(points.len());
    if start >= end {
        return;
    }
    let mut ranges = vec![(start, end)];
    while let Some((start, end)) = ranges.pop() {
        let s = points[start];
        let e = points[end - 1];
        let mut max_dist_sq = 0.0;
        let mut max_ndx = 1;
        for (i, &p) in points.iter().enumerate().take(end - 1).skip(start + 1) {
            let dist_sq = distance_to_segment_sq(p, s, e);
            if dist_sq > max_dist_sq {
                max_dist_sq = dist_sq;
                max_ndx = i;
            }
        }
        if max_dist_sq > 0.0 && max_dist_sq.sqrt() > epsilon {
            // Left part first: pushed last.
            ranges.push((max_ndx, end));
            ranges.push((start, max_ndx + 1));
        } else {
            if out.is_empty() {
                out.push(s);
            }
            out.push(e);
        }
    }
}

/// Points along a chain of cubic Bézier curves (`[start, c1, c2, end, c1,
/// c2, end, …]`), close enough to the curves for `tolerance`, then
/// simplified with `distance` if given and positive. Trailing points that
/// don't make a whole curve are ignored (points-on-curve would throw).
pub fn points_on_bezier_curves(
    points: &[Point],
    tolerance: f64,
    distance: Option<f64>,
) -> Vec<Point> {
    let mut new_points = Vec::new();
    let segments = points.len().saturating_sub(1) / 3;
    for i in 0..segments {
        points_on_bezier_curve_with_splitting(points, i * 3, tolerance, &mut new_points, 0);
    }
    match distance {
        Some(d) if d > 0.0 => {
            let mut out = Vec::new();
            simplify_points(&new_points, 0, new_points.len(), d, &mut out);
            out
        }
        _ => new_points,
    }
}

/// The cubic Bézier chain of a Catmull-Rom-like curve through `points`
/// (`curveToBezier`), the curve rough draws. Needs at least three points:
/// fewer give an empty chain (points-on-curve throws).
pub fn curve_to_bezier(points_in: &[Point], curve_tightness: f64) -> Vec<Point> {
    let len = points_in.len();
    if len < 3 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if len == 3 {
        out.extend([points_in[0], points_in[1], points_in[2], points_in[2]]);
    } else {
        let mut points = vec![points_in[0], points_in[0]];
        for (i, &p) in points_in.iter().enumerate().skip(1) {
            points.push(p);
            if i == len - 1 {
                points.push(p);
            }
        }
        let s = 1.0 - curve_tightness;
        out.push(points[0]);
        let mut i = 1;
        while i + 2 < points.len() {
            let v = points[i];
            let b1 = [
                v[0] + (s * points[i + 1][0] - s * points[i - 1][0]) / 6.0,
                v[1] + (s * points[i + 1][1] - s * points[i - 1][1]) / 6.0,
            ];
            let b2 = [
                points[i + 1][0] + (s * points[i][0] - s * points[i + 2][0]) / 6.0,
                points[i + 1][1] + (s * points[i][1] - s * points[i + 2][1]) / 6.0,
            ];
            let b3 = [points[i + 1][0], points[i + 1][1]];
            out.extend([b1, b2, b3]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straight_curve_is_its_ends() {
        let pts = points_on_bezier_curves(
            &[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]],
            0.15,
            None,
        );
        assert_eq!(pts, [[0.0, 0.0], [30.0, 0.0]]);
    }

    #[test]
    fn a_bent_curve_is_split_into_points_on_it() {
        let curve = [[0.0, 0.0], [0.0, 100.0], [100.0, 100.0], [100.0, 0.0]];
        let pts = points_on_bezier_curves(&curve, 0.15, None);
        assert!(pts.len() > 10, "{}", pts.len());
        assert_eq!(pts[0], [0.0, 0.0]);
        assert_eq!(*pts.last().unwrap(), [100.0, 0.0]);
        // The middle of this symmetric curve is (50, 75), and a split point.
        assert!(pts.contains(&[50.0, 75.0]));
        // Simplifying keeps the ends and drops points.
        let simple = points_on_bezier_curves(&curve, 0.15, Some(1.0));
        assert!(simple.len() < pts.len());
        assert_eq!(simple[0], [0.0, 0.0]);
        assert_eq!(*simple.last().unwrap(), [100.0, 0.0]);
    }

    #[test]
    fn chained_curves_share_their_joints() {
        let chain = [
            [0.0, 0.0],
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 0.0],
            [10.0, 0.0],
            [20.0, 0.0],
            [20.0, 0.0],
        ];
        // Flat enough at this tolerance: each curve is its ends, and the
        // joint, the same point, is there once.
        assert_eq!(
            points_on_bezier_curves(&chain, 1000.0, None),
            [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]]
        );
    }

    #[test]
    fn non_finite_curves_end() {
        let pts = points_on_bezier_curves(
            &[
                [0.0, 0.0],
                [f64::NAN, 1.0],
                [2.0, f64::INFINITY],
                [3.0, 0.0],
            ],
            0.15,
            None,
        );
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn simplify_drops_points_near_the_line() {
        let pts = [[0.0, 0.0], [1.0, 0.1], [2.0, -0.1], [3.0, 5.0], [4.0, 0.0]];
        assert_eq!(
            simplify(&pts, 0.5),
            [[0.0, 0.0], [2.0, -0.1], [3.0, 5.0], [4.0, 0.0]]
        );
        assert_eq!(simplify(&pts, 10.0), [[0.0, 0.0], [4.0, 0.0]]);
        // One point: it twice, as points-on-curve gives it.
        assert_eq!(simplify(&[[1.0, 2.0]], 1.0), [[1.0, 2.0], [1.0, 2.0]]);
        assert_eq!(simplify(&[], 1.0), Vec::<Point>::new());
        // A negative distance acts as 0 (no endless recursion).
        assert_eq!(simplify(&pts, -1.0), pts);
    }

    #[test]
    fn simplify_points_appends() {
        let mut out = vec![[9.0, 9.0]];
        simplify_points(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]], 0, 3, 1.0, &mut out);
        // Not empty: the first point isn't added.
        assert_eq!(out, [[9.0, 9.0], [2.0, 0.0]]);
    }

    #[test]
    fn curve_to_bezier_through_the_points() {
        let pts = [[0.0, 0.0], [10.0, 10.0], [20.0, 0.0], [30.0, 10.0]];
        let b = curve_to_bezier(&pts, 0.0);
        // Start, then three points per segment ending at each input point.
        assert_eq!(b.len(), 1 + 3 * 3);
        assert_eq!(b[0], [0.0, 0.0]);
        assert_eq!(b[3], [10.0, 10.0]);
        assert_eq!(b[6], [20.0, 0.0]);
        assert_eq!(b[9], [30.0, 10.0]);
        // The first control point: (p1 - p0) / 6 from the start.
        assert_eq!(b[1], [10.0 / 6.0, 10.0 / 6.0]);
        assert_eq!(
            curve_to_bezier(&pts[..3], 0.0),
            [pts[0], pts[1], pts[2], pts[2]]
        );
        assert!(curve_to_bezier(&pts[..2], 0.0).is_empty());
    }
}
