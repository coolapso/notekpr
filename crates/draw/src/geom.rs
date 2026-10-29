//! Points and vectors as Excalidraw's `math` package has them (MIT, see
//! `PORTED.md`), in the same order of operations: drawing, hit testing and
//! export depend on the exact numbers.

use crate::js;

/// A point or vector: `[x, y]`.
pub type Point = [f64; 2];

/// `pointDistance`.
pub fn distance(a: Point, b: Point) -> f64 {
    js::hypot(b[0] - a[0], b[1] - a[1])
}

/// `pointRotateRads`: `point` turned by `angle` (radians, clockwise on
/// screen) around `center`.
pub fn rotate(point: Point, center: Point, angle: f64) -> Point {
    if angle == 0.0 || angle.is_nan() {
        return point;
    }
    let [x, y] = point;
    let [cx, cy] = center;
    [
        (x - cx) * js::cos(angle) - (y - cy) * js::sin(angle) + cx,
        (x - cx) * js::sin(angle) + (y - cy) * js::cos(angle) + cy,
    ]
}

/// `vectorFromPoint`: from `origin` to `p`.
pub fn vector(p: Point, origin: Point) -> Point {
    [p[0] - origin[0], p[1] - origin[1]]
}

/// `vectorDot`.
pub fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// `degreesToRadians`.
pub fn radians(degrees: f64) -> f64 {
    (degrees * std::f64::consts::PI) / 180.0
}

/// Which way `p` lies from `o`, in the four directions of an elbow arrow
/// (`headingForPoint`): right, down, left or up.
pub fn heading(p: Point, o: Point) -> Point {
    let [x, y] = vector(p, o);
    let (ax, ay) = (x.abs(), y.abs());
    if x > ay {
        [1.0, 0.0]
    } else if x <= -ay {
        [-1.0, 0.0]
    } else if y > ax {
        [0.0, 1.0]
    } else {
        [0.0, -1.0]
    }
}

/// `headingForPointIsHorizontal`.
pub fn heading_is_horizontal(p: Point, o: Point) -> bool {
    heading(p, o)[0] != 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_and_headings() {
        let p = rotate([10.0, 0.0], [0.0, 0.0], std::f64::consts::FRAC_PI_2);
        assert!(p[0].abs() < 1e-12 && (p[1] - 10.0).abs() < 1e-12);
        assert_eq!(rotate([1.0, 2.0], [0.0, 0.0], 0.0), [1.0, 2.0]);
        assert!(heading_is_horizontal([5.0, 1.0], [0.0, 0.0]));
        assert!(!heading_is_horizontal([1.0, 5.0], [0.0, 0.0]));
        assert_eq!(heading([0.0, -3.0], [0.0, 0.0]), [0.0, -1.0]);
    }
}
