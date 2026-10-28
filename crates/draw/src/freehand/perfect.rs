//! Pressure-sensitive pen strokes: a port of perfect-freehand 1.2.0 (the
//! version Excalidraw pins), MIT, see `crates/draw/PORTED.md`.
//!
//! A stroke is drawn as a filled polygon around the pen's path: the input
//! points are smoothed ([`get_stroke_points`]), then each gets a radius from
//! its (real or simulated) pressure and the outline runs along both sides,
//! with rounded caps and corners ([`get_stroke_outline_points`]).
//!
//! The arithmetic follows 1.2.0's published build step by step, so that
//! the outline is bit for bit what Excalidraw computes. (Version 1.2.3,
//! whose readable source is the usual reference, computes the same numbers
//! with fewer allocations; the only difference is that 1.2.0 takes a `null`
//! pressure for 0, which can't be expressed here.)

use super::vec::{
    Vec2, add, dist, dist2, dpr, is_equal, lrp, mul, neg, per, prj, rot_around, sub, uni,
};
use crate::js;

/// The rate at which simulated pressure follows the pen's speed.
const RATE_OF_PRESSURE_CHANGE: f64 = 0.275;

/// π nudged up a little: some browsers drew caps of exactly half a turn badly.
const FIXED_PI: f64 = std::f64::consts::PI + 1e-4;

/// Segments of a rounded start cap, a dot and a sharp corner.
const START_CAP_SEGMENTS: f64 = 13.0;

/// Segments of a rounded end cap (one and a half turns).
const END_CAP_SEGMENTS: f64 = 29.0;

/// Points this close to the end of the stroke are skipped (pen noise).
const END_NOISE_THRESHOLD: f64 = 3.0;

/// The smallest radius a point gets, so that no part of a stroke vanishes.
const MIN_RADIUS: f64 = 0.01;

/// The pressure of a first point given without one: lower than the rest,
/// as lines start slowly.
const DEFAULT_FIRST_PRESSURE: f64 = 0.25;

/// The pressure of other points given without one.
const DEFAULT_PRESSURE: f64 = 0.5;

/// An input point: where the pen was, and how hard it pressed (`None` when
/// the device doesn't say).
///
/// perfect-freehand also takes `{x, y, pressure}` objects, where a missing
/// pressure counts as 0.5 even for the first point: pass `Some(0.5)` for those.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputPoint {
    pub point: Vec2,
    pub pressure: Option<f64>,
}

impl From<[f64; 2]> for InputPoint {
    fn from([x, y]: [f64; 2]) -> Self {
        InputPoint {
            point: [x, y],
            pressure: None,
        }
    }
}

impl From<[f64; 3]> for InputPoint {
    fn from([x, y, pressure]: [f64; 3]) -> Self {
        InputPoint {
            point: [x, y],
            pressure: Some(pressure),
        }
    }
}

/// How far a stroke's end narrows to a point (the `taper` option).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Taper {
    /// No taper (`false` or left out).
    None,
    /// Along the whole stroke (`true`): the longer of `size` and the stroke.
    Full,
    /// Along this many pixels from the end; 0 (or NaN) is no taper.
    Distance(f64),
}

/// One end of a stroke: whether it gets a rounded cap (else a flat one) and
/// how it tapers.
#[derive(Clone, Copy, Debug)]
pub struct End {
    pub cap: bool,
    pub taper: Taper,
    /// How the radius grows over the taper, from 0 at the tip to 1.
    pub easing: fn(f64) -> f64,
}

impl End {
    /// The start's defaults: a cap, no taper, ease-out quad.
    pub const START: End = End {
        cap: true,
        taper: Taper::None,
        easing: |t| t * (2.0 - t),
    };

    /// The end's defaults: a cap, no taper, ease-out cubic.
    pub const END: End = End {
        cap: true,
        taper: Taper::None,
        easing: |t| {
            // `--t * t * t + 1`
            let t = t - 1.0;
            t * t * t + 1.0
        },
    };
}

/// perfect-freehand's options, with its defaults ([`Default`]).
#[derive(Clone, Copy, Debug)]
pub struct StrokeOptions {
    /// The stroke's base diameter (16).
    pub size: f64,
    /// How much pressure narrows or widens the stroke, -1 to 1 (0.5).
    pub thinning: f64,
    /// How much the outline's edges are softened, 0 to 1 (0.5).
    pub smoothing: f64,
    /// How much the pen's path is smoothed, 0 to 1 (0.5).
    pub streamline: f64,
    /// Applied to each point's pressure (none).
    pub easing: fn(f64) -> f64,
    /// Whether pressure comes from the pen's speed instead of the input (true).
    pub simulate_pressure: bool,
    pub start: End,
    pub end: End,
    /// Whether the stroke is finished: its last point is used as is (false).
    pub last: bool,
}

impl Default for StrokeOptions {
    fn default() -> Self {
        StrokeOptions {
            size: 16.0,
            thinning: 0.5,
            smoothing: 0.5,
            streamline: 0.5,
            easing: |t| t,
            simulate_pressure: true,
            start: End::START,
            end: End::END,
            last: false,
        }
    }
}

/// A smoothed point of a stroke ([`get_stroke_points`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokePoint {
    /// Where the point is.
    pub point: Vec2,
    /// The input pressure, or the default.
    pub pressure: f64,
    /// The distance from the previous point.
    pub distance: f64,
    /// The unit vector from this point to the previous one.
    pub vector: Vec2,
    /// The stroke's length up to this point.
    pub running_length: f64,
}

/// `getStrokeRadius`: the radius of a point under `pressure`.
pub fn get_stroke_radius(size: f64, thinning: f64, pressure: f64, easing: fn(f64) -> f64) -> f64 {
    size * easing(0.5 - thinning * (0.5 - pressure))
}

/// The pressure the pen probably had, from the previous pressure and how far
/// it moved (fast strokes are thin). 1.2.0 writes this out where it is used;
/// 1.2.3 names it `simulatePressure`.
pub fn simulate_pressure(prev_pressure: f64, distance: f64, size: f64) -> f64 {
    // How fast the pressure changes, and by how much.
    let sp = js::min(1.0, distance / size);
    let rp = js::min(1.0, 1.0 - sp);
    js::min(
        1.0,
        prev_pressure + (rp - prev_pressure) * (sp * RATE_OF_PRESSURE_CHANGE),
    )
}

/// `getStroke`: the outline polygon of a pen stroke through `points`.
pub fn get_stroke<P: Copy + Into<InputPoint>>(points: &[P], options: &StrokeOptions) -> Vec<Vec2> {
    get_stroke_outline_points(&get_stroke_points(points, options), options)
}

/// `getStrokePoints`: the input points smoothed (`streamline`), with their
/// pressure, direction and distance along the stroke. Points too close to
/// the start (within `size`) are dropped, as pens are noisy there.
pub fn get_stroke_points<P: Copy + Into<InputPoint>>(
    points: &[P],
    options: &StrokeOptions,
) -> Vec<StrokePoint> {
    let StrokeOptions {
        streamline,
        size,
        last: is_complete,
        ..
    } = *options;
    if points.is_empty() {
        return Vec::new();
    }
    // How far each point moves towards the next input point.
    let t = 0.15 + (1.0 - streamline) * 0.85;
    let mut pts: Vec<InputPoint> = points.iter().map(|&p| p.into()).collect();
    // Two points: three more in between, so tapered ends don't look like dashes.
    if pts.len() == 2 {
        let last = pts[1].point;
        pts.truncate(1);
        for i in 1..5 {
            pts.push(InputPoint {
                point: lrp(pts[0].point, last, f64::from(i) / 4.0),
                pressure: None,
            });
        }
    }
    // One point: a second one 1px away.
    if pts.len() == 1 {
        pts.push(InputPoint {
            point: add(pts[0].point, [1.0, 1.0]),
            pressure: pts[0].pressure,
        });
    }
    let pressure_or = |p: Option<f64>, default: f64| match p {
        Some(p) if p >= 0.0 => p,
        _ => default,
    };
    let mut stroke_points = vec![StrokePoint {
        point: pts[0].point,
        pressure: pressure_or(pts[0].pressure, DEFAULT_FIRST_PRESSURE),
        vector: [1.0, 1.0],
        distance: 0.0,
        running_length: 0.0,
    }];
    let mut has_reached_minimum_length = false;
    let mut running_length = 0.0;
    let mut prev = stroke_points[0];
    let max = pts.len() - 1;
    for (i, input) in pts.iter().enumerate().skip(1) {
        let point = if is_complete && i == max {
            input.point
        } else {
            lrp(prev.point, input.point, t)
        };
        if is_equal(prev.point, point) {
            continue;
        }
        let distance = dist(point, prev.point);
        running_length += distance;
        if i < max && !has_reached_minimum_length {
            if running_length < size {
                continue;
            }
            has_reached_minimum_length = true;
        }
        prev = StrokePoint {
            point,
            pressure: pressure_or(input.pressure, DEFAULT_PRESSURE),
            vector: uni(sub(prev.point, point)),
            distance,
            running_length,
        };
        stroke_points.push(prev);
    }
    stroke_points[0].vector = stroke_points.get(1).map_or([0.0, 0.0], |p| p.vector);
    stroke_points
}

/// `getStrokeOutlinePoints`: the outline polygon around stroke points —
/// down the left side, around the end cap, back up the right side and
/// around the start cap.
pub fn get_stroke_outline_points(points: &[StrokePoint], options: &StrokeOptions) -> Vec<Vec2> {
    let StrokeOptions {
        size,
        smoothing,
        thinning,
        simulate_pressure: should_simulate_pressure,
        easing,
        start,
        end,
        last: is_complete,
        ..
    } = *options;
    if points.is_empty() || size <= 0.0 {
        return Vec::new();
    }
    let len = points.len();
    let total_length = points[len - 1].running_length;
    let taper_distance = |taper: Taper| match taper {
        Taper::None => 0.0,
        Taper::Full => js::max(size, total_length),
        Taper::Distance(d) => d,
    };
    let taper_start = taper_distance(start.taper);
    let taper_end = taper_distance(end.taper);
    // `Math.pow(size * smoothing, 2)`: squaring is exact multiplication.
    let min_distance = (size * smoothing) * (size * smoothing);
    let mut left_pts: Vec<Vec2> = Vec::new();
    let mut right_pts: Vec<Vec2> = Vec::new();

    // The starting pressure, averaged over the first points: drawn lines
    // almost always start slowly, which would make them start fat.
    let mut prev_pressure = points
        .iter()
        .take(10)
        .fold(points[0].pressure, |acc, curr| {
            let pressure = if should_simulate_pressure {
                simulate_pressure(acc, curr.distance, size)
            } else {
                curr.pressure
            };
            (acc + pressure) / 2.0
        });

    let mut radius = get_stroke_radius(size, thinning, points[len - 1].pressure, easing);
    let mut first_radius: Option<f64> = None;
    let mut prev_vector = points[0].vector;
    let mut prev_left_point = points[0].point;
    let mut prev_right_point = prev_left_point;
    let mut temp_left_point = prev_left_point;
    let mut temp_right_point = prev_right_point;
    // Whether the previous point was a sharp corner, not to draw one twice.
    let mut is_prev_point_sharp_corner = false;

    for i in 0..len {
        let StrokePoint {
            point,
            mut pressure,
            vector,
            distance,
            running_length,
        } = points[i];
        let is_last_point = i == len - 1;

        // Skip the noise at the end of the line.
        if !is_last_point && total_length - running_length < END_NOISE_THRESHOLD {
            continue;
        }

        if js::truthy(thinning) {
            if should_simulate_pressure {
                pressure = simulate_pressure(prev_pressure, distance, size);
            }
            radius = get_stroke_radius(size, thinning, pressure, easing);
        } else {
            radius = size / 2.0;
        }
        if first_radius.is_none() {
            first_radius = Some(radius);
        }

        // Tapering: the smaller of the two strengths.
        let taper_start_strength = if running_length < taper_start {
            (start.easing)(running_length / taper_start)
        } else {
            1.0
        };
        let taper_end_strength = if total_length - running_length < taper_end {
            (end.easing)((total_length - running_length) / taper_end)
        } else {
            1.0
        };
        radius = js::max(
            MIN_RADIUS,
            radius * js::min(taper_start_strength, taper_end_strength),
        );

        // A turn sharper than a right angle gets a rounded cap.
        let next_vector = if is_last_point {
            vector
        } else {
            points[i + 1].vector
        };
        let next_dpr = if is_last_point {
            1.0
        } else {
            dpr(vector, next_vector)
        };
        let prev_dpr = dpr(vector, prev_vector);
        let is_point_sharp_corner = prev_dpr < 0.0 && !is_prev_point_sharp_corner;
        let is_next_point_sharp_corner = next_dpr < 0.0;
        if is_point_sharp_corner || is_next_point_sharp_corner {
            let offset = mul(per(prev_vector), radius);
            let step = 1.0 / START_CAP_SEGMENTS;
            let mut t = 0.0;
            while t <= 1.0 {
                temp_left_point = rot_around(sub(point, offset), point, FIXED_PI * t);
                left_pts.push(temp_left_point);
                temp_right_point = rot_around(add(point, offset), point, FIXED_PI * -t);
                right_pts.push(temp_right_point);
                t += step;
            }
            prev_left_point = temp_left_point;
            prev_right_point = temp_right_point;
            if is_next_point_sharp_corner {
                is_prev_point_sharp_corner = true;
            }
            continue;
        }
        is_prev_point_sharp_corner = false;

        if is_last_point {
            let offset = mul(per(vector), radius);
            left_pts.push(sub(point, offset));
            right_pts.push(add(point, offset));
            continue;
        }

        // A point on each side, kept when far enough from the side's last one.
        let offset = mul(per(lrp(next_vector, vector, next_dpr)), radius);
        temp_left_point = sub(point, offset);
        if i <= 1 || dist2(prev_left_point, temp_left_point) > min_distance {
            left_pts.push(temp_left_point);
            prev_left_point = temp_left_point;
        }
        temp_right_point = add(point, offset);
        if i <= 1 || dist2(prev_right_point, temp_right_point) > min_distance {
            right_pts.push(temp_right_point);
            prev_right_point = temp_right_point;
        }
        prev_pressure = pressure;
        prev_vector = vector;
    }

    // The caps. Tapered ends have none; a single point is a dot.
    let first_point = points[0].point;
    let last_point = if len > 1 {
        points[len - 1].point
    } else {
        add(points[0].point, [1.0, 1.0])
    };
    let mut start_cap: Vec<Vec2> = Vec::new();
    let mut end_cap: Vec<Vec2> = Vec::new();
    if len == 1 {
        if !(js::truthy(taper_start) || js::truthy(taper_end)) || is_complete {
            // `firstRadius || radius`
            let dot_radius = first_radius.filter(|&r| js::truthy(r)).unwrap_or(radius);
            let start = prj(
                first_point,
                uni(per(sub(first_point, last_point))),
                -dot_radius,
            );
            let mut dot_pts = Vec::new();
            let step = 1.0 / START_CAP_SEGMENTS;
            let mut t = step;
            while t <= 1.0 {
                dot_pts.push(rot_around(start, first_point, FIXED_PI * 2.0 * t));
                t += step;
            }
            return dot_pts;
        }
    } else {
        if js::truthy(taper_start) {
            // A tapered start: no cap.
        } else if start.cap {
            let step = 1.0 / START_CAP_SEGMENTS;
            let mut t = step;
            while t <= 1.0 {
                start_cap.push(rot_around(right_pts[0], first_point, FIXED_PI * t));
                t += step;
            }
        } else {
            let corners_vector = sub(left_pts[0], right_pts[0]);
            let offset_a = mul(corners_vector, 0.5);
            let offset_b = mul(corners_vector, 0.51);
            start_cap.extend([
                sub(first_point, offset_a),
                sub(first_point, offset_b),
                add(first_point, offset_b),
                add(first_point, offset_a),
            ]);
        }

        let direction = per(neg(points[len - 1].vector));
        if js::truthy(taper_end) {
            // A tapered end: the line ends at its last point.
            end_cap.push(last_point);
        } else if end.cap {
            // One and a half turns, which covers sharp turns at the very end.
            let start = prj(last_point, direction, radius);
            let step = 1.0 / END_CAP_SEGMENTS;
            let mut t = step;
            while t < 1.0 {
                end_cap.push(rot_around(start, last_point, FIXED_PI * 3.0 * t));
                t += step;
            }
        } else {
            end_cap.extend([
                add(last_point, mul(direction, radius)),
                add(last_point, mul(direction, radius * 0.99)),
                sub(last_point, mul(direction, radius * 0.99)),
                sub(last_point, mul(direction, radius)),
            ]);
        }
    }

    right_pts.reverse();
    let mut outline = left_pts;
    outline.extend(end_cap);
    outline.extend(right_pts);
    outline.extend(start_cap);
    outline
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freehand::vec;
    use serde_json::Value;

    /// perfect-freehand's own test inputs and snapshots (its repository's
    /// `src/test`, computed by V8), gathered by name: `inputs`, and what
    /// `getStroke` and `getStrokePoints` make of them with the defaults.
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/perfect-freehand.json"))
            .expect("fixture")
    }

    fn num(v: &Value) -> f64 {
        v.as_f64().expect("number")
    }

    /// A test input: `[x, y]`, `[x, y, pressure]` or `{x, y, pressure?}`.
    fn input(points: &Value) -> Vec<InputPoint> {
        points
            .as_array()
            .expect("points")
            .iter()
            .map(|p| match p {
                Value::Array(a) => InputPoint {
                    point: [num(&a[0]), num(&a[1])],
                    pressure: a.get(2).map(num),
                },
                _ => InputPoint {
                    point: [num(&p["x"]), num(&p["y"])],
                    pressure: Some(p.get("pressure").map_or(DEFAULT_PRESSURE, num)),
                },
            })
            .collect()
    }

    fn vec2(v: &Value) -> Vec2 {
        [num(&v[0]), num(&v[1])]
    }

    /// The same doubles (-0 and 0 alike: JSON can't tell them apart).
    fn assert_same(ours: &[f64], theirs: &[f64], what: &str) {
        assert_eq!(ours.len(), theirs.len(), "{what}: length");
        for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
            assert!(a == b, "{what}, number {i}: {a:e}, not {b:e}");
        }
    }

    #[test]
    fn get_stroke_matches_snapshots() {
        let fixture = fixture();
        let inputs = fixture["inputs"].as_object().expect("inputs");
        assert_eq!(inputs.len(), 12);
        for (name, points) in inputs {
            let ours: Vec<f64> = get_stroke(&input(points), &StrokeOptions::default())
                .into_iter()
                .flatten()
                .collect();
            let theirs: Vec<f64> = fixture["getStroke"][name]
                .as_array()
                .expect("snapshot")
                .iter()
                .flat_map(vec2)
                .collect();
            assert_same(&ours, &theirs, name);
        }

        let tricky = get_stroke(
            &input(&fixture["inputs"]["onePoint"]),
            &StrokeOptions {
                size: 1.0,
                thinning: 0.6,
                smoothing: 0.5,
                streamline: 0.5,
                simulate_pressure: true,
                last: false,
                ..StrokeOptions::default()
            },
        );
        let theirs: Vec<f64> = fixture["getStrokeTrickyOnePoint"]
            .as_array()
            .expect("snapshot")
            .iter()
            .flat_map(vec2)
            .collect();
        assert_same(
            &tricky.into_iter().flatten().collect::<Vec<_>>(),
            &theirs,
            "tricky",
        );
    }

    #[test]
    fn get_stroke_points_match_snapshots() {
        let fixture = fixture();
        let snapshots = fixture["getStrokePoints"].as_object().expect("snapshots");
        assert_eq!(snapshots.len(), 7);
        for (name, snapshot) in snapshots {
            let ours =
                get_stroke_points(&input(&fixture["inputs"][name]), &StrokeOptions::default());
            let theirs = snapshot.as_array().expect("snapshot");
            assert_eq!(ours.len(), theirs.len(), "{name}");
            for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
                let what = format!("{name}, point {i}");
                assert_same(&a.point, &vec2(&b["point"]), &what);
                assert_same(&a.vector, &vec2(&b["vector"]), &what);
                assert_same(
                    &[a.pressure, a.distance, a.running_length],
                    &[
                        num(&b["pressure"]),
                        num(&b["distance"]),
                        num(&b["runningLength"]),
                    ],
                    &what,
                );
            }
        }
    }

    #[test]
    fn nothing_from_no_points() {
        let options = StrokeOptions::default();
        assert!(get_stroke::<[f64; 2]>(&[], &options).is_empty());
        assert!(get_stroke_points::<[f64; 2]>(&[], &options).is_empty());
        assert!(get_stroke_outline_points(&[], &options).is_empty());
        // Nor from a stroke of no size.
        let options = StrokeOptions {
            size: 0.0,
            ..options
        };
        assert!(get_stroke(&[[0.0, 0.0], [10.0, 0.0]], &options).is_empty());
    }

    #[test]
    fn one_point_and_dots() {
        let options = StrokeOptions {
            last: true,
            ..StrokeOptions::default()
        };
        // One point: a second one is added 1px away, and the outline goes
        // around both.
        let points = get_stroke_points(&[[5.0, 5.0]], &options);
        assert_eq!(
            points.iter().map(|p| p.point).collect::<Vec<_>>(),
            [[5.0, 5.0], [6.0, 6.0]]
        );
        let outline = get_stroke(&[[5.0, 5.0]], &options);
        assert!(outline.len() > 13);
        assert!(outline.iter().all(|&p| vec::dist(p, [5.5, 5.5]) < 16.0));
        // Points that never move: a dot, 13 points on a circle.
        let dot = get_stroke(&[[5.0, 5.0], [5.0, 5.0]], &options);
        assert_eq!(dot.len(), 13);
        let r = vec::dist(dot[0], [5.0, 5.0]);
        for &p in &dot {
            assert!((vec::dist(p, [5.0, 5.0]) - r).abs() < 1e-9);
        }
    }

    #[test]
    fn straight_line() {
        let line: Vec<[f64; 2]> = (0..=20).map(|i| [f64::from(i) * 5.0, 0.0]).collect();
        let options = StrokeOptions {
            thinning: 0.0,
            last: true,
            ..StrokeOptions::default()
        };
        let points = get_stroke_points(&line, &options);
        // Smoothed, so the line ends where it ends, and points away from its start.
        assert_eq!(points.last().map(|p| p.point), Some([100.0, 0.0]));
        assert!(
            points
                .iter()
                .all(|p| p.point[1] == 0.0 && p.vector == [-1.0, 0.0])
        );
        let outline = get_stroke(&line, &options);
        // Without thinning: sides size/2 from the line, round caps around
        // its ends.
        let from_line = |[x, y]: Vec2| {
            if x < 0.0 {
                vec::len([x, y])
            } else if x > 100.0 {
                vec::len([x - 100.0, y])
            } else {
                y.abs()
            }
        };
        assert!(outline.iter().all(|&p| from_line(p) < 8.0 + 1e-9));
        let sides: Vec<&Vec2> = outline
            .iter()
            .filter(|p| p[0] > 10.0 && p[0] < 90.0)
            .collect();
        assert!(sides.len() > 4 && sides.iter().all(|p| p[1].abs() == 8.0));
        let (min_x, max_x) = outline.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p[0]), hi.max(p[0]))
        });
        assert!(min_x < -7.5 && max_x > 107.5, "{min_x} {max_x}");
    }

    #[test]
    fn pressure_makes_strokes_wider() {
        let line: Vec<[f64; 3]> = (0..=20).map(|i| [f64::from(i) * 5.0, 0.0, 0.0]).collect();
        let options = StrokeOptions {
            simulate_pressure: false,
            last: true,
            ..StrokeOptions::default()
        };
        let width = |outline: &[Vec2]| outline.iter().fold(0.0f64, |w, p| w.max(p[1].abs()));
        let light = width(&get_stroke(&line, &options));
        let heavy: Vec<[f64; 3]> = line.iter().map(|&[x, y, _]| [x, y, 1.0]).collect();
        let heavy = width(&get_stroke(&heavy, &options));
        // thinning 0.5: radius from size/4 at no pressure to 3/4 size at full.
        assert!(
            (light - 4.0).abs() < 1e-9 && (heavy - 12.0).abs() < 1e-9,
            "{light} {heavy}"
        );
    }

    #[test]
    fn simulated_pressure() {
        // Standing still: pressure rises towards 1, by 0 at once.
        assert_eq!(simulate_pressure(0.5, 0.0, 16.0), 0.5);
        // Fast (a whole size per step): pressure falls towards 0.
        assert_eq!(
            simulate_pressure(0.5, 16.0, 16.0),
            0.5 - 0.5 * RATE_OF_PRESSURE_CHANGE
        );
        // Any movement lowers full pressure.
        assert!(simulate_pressure(1.0, 1.0, 16.0) < 1.0);
        assert_eq!(get_stroke_radius(100.0, 0.5, 0.25, |t| t), 37.5);
        assert_eq!(get_stroke_radius(100.0, -1.0, 0.25, |t| t * t), 56.25);
    }

    #[test]
    fn tapers_and_flat_caps() {
        let line: Vec<[f64; 2]> = (0..=20).map(|i| [f64::from(i) * 5.0, 0.0]).collect();
        let tapered = StrokeOptions {
            start: End {
                taper: Taper::Full,
                ..End::START
            },
            end: End {
                taper: Taper::Distance(30.0),
                ..End::END
            },
            last: true,
            ..StrokeOptions::default()
        };
        let outline = get_stroke(&line, &tapered);
        // A tapered end ends at the line's last point.
        assert!(outline.contains(&[100.0, 0.0]));
        let flat = StrokeOptions {
            start: End {
                cap: false,
                ..End::START
            },
            end: End {
                cap: false,
                ..End::END
            },
            thinning: 0.0,
            last: true,
            ..StrokeOptions::default()
        };
        let outline = get_stroke(&line, &flat);
        // Flat caps don't reach past the ends.
        assert!(outline.iter().all(|p| p[0] > -1e-9 && p[0] < 100.0 + 1e-9));
    }
}
