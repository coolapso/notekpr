//! Two-dimensional vector helpers of perfect-freehand 1.2.0 (`vec.ts`),
//! MIT, see `crates/draw/PORTED.md`.
//!
//! Each computes exactly what the JavaScript does, in the same order, so the
//! strokes come out bit for bit the same.

use crate::js;

/// A point or a vector: `[x, y]`.
pub type Vec2 = [f64; 2];

/// `neg`: the vector reversed.
pub fn neg(a: Vec2) -> Vec2 {
    [-a[0], -a[1]]
}

/// `add`: the sum of two vectors.
pub fn add(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] + b[0], a[1] + b[1]]
}

/// `sub`: `a` minus `b`.
pub fn sub(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] - b[0], a[1] - b[1]]
}

/// `mul`: the vector scaled by `n`.
pub fn mul(a: Vec2, n: f64) -> Vec2 {
    [a[0] * n, a[1] * n]
}

/// `div`: the vector divided by `n`.
pub fn div(a: Vec2, n: f64) -> Vec2 {
    [a[0] / n, a[1] / n]
}

/// `per`: the vector turned a quarter turn, `[y, -x]` (anticlockwise on
/// screen, where y points down).
pub fn per(a: Vec2) -> Vec2 {
    [a[1], -a[0]]
}

/// `dpr`: the dot product.
pub fn dpr(a: Vec2, b: Vec2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// `isEqual`: whether two points are the same (`===` on each coordinate).
pub fn is_equal(a: Vec2, b: Vec2) -> bool {
    a[0] == b[0] && a[1] == b[1]
}

/// `len`: the vector's length (with `Math.hypot`).
pub fn len(a: Vec2) -> f64 {
    js::hypot(a[0], a[1])
}

/// `len2`: the vector's length squared.
pub fn len2(a: Vec2) -> f64 {
    a[0] * a[0] + a[1] * a[1]
}

/// `dist2`: the distance between two points, squared.
pub fn dist2(a: Vec2, b: Vec2) -> f64 {
    len2(sub(a, b))
}

/// `uni`: the vector scaled to length 1.
pub fn uni(a: Vec2) -> Vec2 {
    div(a, len(a))
}

/// `dist`: the distance between two points. The JavaScript passes the y
/// difference first; `Math.hypot` is not symmetric to the last bit, so
/// neither is this.
pub fn dist(a: Vec2, b: Vec2) -> f64 {
    js::hypot(a[1] - b[1], a[0] - b[0])
}

/// `rotAround`: `a` rotated around `c` by `r` radians.
pub fn rot_around(a: Vec2, c: Vec2, r: f64) -> Vec2 {
    let s = js::sin(r);
    let co = js::cos(r);
    let px = a[0] - c[0];
    let py = a[1] - c[1];
    let nx = px * co - py * s;
    let ny = px * s + py * co;
    [nx + c[0], ny + c[1]]
}

/// `lrp`: the point `t` of the way from `a` to `b`.
pub fn lrp(a: Vec2, b: Vec2, t: f64) -> Vec2 {
    add(a, mul(sub(b, a), t))
}

/// `prj`: `a` moved along direction `b` by `c`.
pub fn prj(a: Vec2, b: Vec2, c: f64) -> Vec2 {
    add(a, mul(b, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(add([1.0, 2.0], [3.0, 4.0]), [4.0, 6.0]);
        assert_eq!(sub([1.0, 2.0], [3.0, 5.0]), [-2.0, -3.0]);
        assert_eq!(per([1.0, 2.0]), [2.0, -1.0]);
        assert_eq!(neg([1.0, -2.0]), [-1.0, 2.0]);
        assert_eq!(dpr([1.0, 2.0], [3.0, 4.0]), 11.0);
        assert_eq!(len([3.0, 4.0]), 5.0);
        assert_eq!(dist([0.0, 0.0], [3.0, 4.0]), 5.0);
        assert_eq!(dist2([0.0, 0.0], [3.0, 4.0]), 25.0);
        assert_eq!(uni([0.0, -2.0]), [0.0, -1.0]);
        assert_eq!(lrp([0.0, 0.0], [10.0, 20.0], 0.25), [2.5, 5.0]);
        assert_eq!(prj([1.0, 1.0], [0.0, 1.0], -2.0), [1.0, -1.0]);
        assert!(is_equal([1.0, 2.0], [1.0, 2.0]));
        assert!(!is_equal([f64::NAN, 2.0], [f64::NAN, 2.0]));
    }

    #[test]
    fn rotates_around_a_center() {
        let [x, y] = rot_around([2.0, 1.0], [1.0, 1.0], std::f64::consts::PI / 2.0);
        assert!((x - 1.0).abs() < 1e-15 && (y - 2.0).abs() < 1e-15);
    }
}
