//! Constant-width pen strokes: a port of Excalidraw's laser-pointer package
//! 1.3.1 (`packages/laser-pointer`: `state.ts`, `math.ts`, `simplify.ts`),
//! MIT, see `crates/draw/PORTED.md`.
//!
//! Made for the laser pointer's fading trail, Excalidraw also draws its
//! constant-width ("uniform") pen strokes with it: points are added one by
//! one, smoothed as they come, and [`LaserPointer::get_stroke_outline`]
//! traces a polygon around them with round caps and rounded corners.
//!
//! The arithmetic follows the TypeScript step by step (including its
//! quirks, like counting the last segment twice in a tail's length), so the
//! outline is bit for bit Excalidraw's.

use std::f64::consts::PI;

use crate::js;

/// A point of a laser-pointer stroke: `[x, y, pressure]`. Outline points
/// carry a third number too, computed alongside but meaningless.
pub type Point = [f64; 3];

/// What [`LaserPointerOptions::size_mapping`] is told about a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeMappingDetails {
    pub pressure: f64,
    pub running_length: f64,
    pub current_index: usize,
    /// The number of points (not a length, despite the name).
    pub total_length: usize,
}

/// When the stroke's points are simplified (`simplifyPhase`). The original's
/// third phase, `"tail"`, throws "Not implemented yet" as soon as the tail is
/// long enough, so it is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimplifyPhase {
    /// The finished outline (the default).
    Output,
    /// The input points, before the outline is traced.
    Input,
}

/// The laser pointer's options, with its defaults ([`Default`]).
#[derive(Clone, Copy, Debug)]
pub struct LaserPointerOptions {
    /// The stroke's radius (2).
    pub size: f64,
    /// How much each new point is pulled back towards the previous one,
    /// 0 to 1 (0.45).
    pub streamline: f64,
    /// How far (px) points may be from the simplified outline; 0 keeps every
    /// point (0.1).
    pub simplify: f64,
    pub simplify_phase: SimplifyPhase,
    /// Whether a stroke whose every point has size 0 still shows a dot at
    /// its end (false).
    pub keep_head: bool,
    /// Each point's size relative to `size` (always 1).
    pub size_mapping: fn(SizeMappingDetails) -> f64,
}

impl Default for LaserPointerOptions {
    fn default() -> Self {
        LaserPointerOptions {
            size: 2.0,
            streamline: 0.45,
            simplify: 0.1,
            simplify_phase: SimplifyPhase::Output,
            keep_head: false,
            size_mapping: |_| 1.0,
        }
    }
}

/// Turns sharper than this (degrees) get a rounded corner.
const CORNER_DETECTION_MAX_ANGLE: f64 = 75.0;

/// Fast strokes (speed above 35) detect only half as sharp corners.
fn corner_detection_variance(speed: f64) -> f64 {
    if speed > 35.0 { 0.5 } else { 1.0 }
}

/// A tail longer than this (px) becomes stable.
const MAX_TAIL_LENGTH: f64 = 50.0;

/// A stroke being drawn (`LaserPointer`).
#[derive(Clone, Debug)]
pub struct LaserPointer {
    pub options: LaserPointerOptions,
    /// Every point added, as given (except repeats).
    pub original_points: Vec<Point>,
    stable_points: Vec<Point>,
    tail_points: Vec<Point>,
    is_fresh: bool,
}

impl LaserPointer {
    pub fn new(options: LaserPointerOptions) -> Self {
        LaserPointer {
            options,
            original_points: Vec::new(),
            stable_points: Vec::new(),
            tail_points: Vec::new(),
            is_fresh: true,
        }
    }

    /// The latest smoothed point.
    fn last_point(&self) -> Option<Point> {
        self.tail_points
            .last()
            .or(self.stable_points.last())
            .copied()
    }

    /// `addPoint`: adds the pen's next position (a repeat of the last one is
    /// ignored), smoothed towards the previous point by `streamline`.
    pub fn add_point(&mut self, point: Point) {
        if let Some(last) = self.original_points.last()
            && last[0] == point[0]
            && last[1] == point[1]
        {
            return;
        }
        self.original_points.push(point);
        if self.is_fresh {
            self.is_fresh = false;
            self.stable_points.push(point);
            return;
        }
        let mut point = point;
        if self.options.streamline > 0.0
            && let Some(last) = self.last_point()
        {
            point = plerp(last, point, 1.0 - self.options.streamline);
        }
        self.tail_points.push(point);
        if run_length(&self.tail_points) > MAX_TAIL_LENGTH {
            self.stabilize_tail();
        }
    }

    /// `close`: the stroke is finished.
    pub fn close(&mut self) {
        self.stabilize_tail();
    }

    /// `stabilizeTail`: the tail's points become stable.
    pub fn stabilize_tail(&mut self) {
        self.stable_points.append(&mut self.tail_points);
    }

    fn get_size(
        &self,
        size_override: Option<f64>,
        pressure: f64,
        index: usize,
        total_length: usize,
        running_length: f64,
    ) -> f64 {
        size_override.unwrap_or(self.options.size)
            * (self.options.size_mapping)(SizeMappingDetails {
                pressure,
                running_length,
                current_index: index,
                total_length,
            })
    }

    /// `getStrokeOutline`: the polygon around the stroke, `size_override`
    /// standing in for `options.size` if given.
    pub fn get_stroke_outline(&self, size_override: Option<f64>) -> Vec<Point> {
        if self.is_fresh {
            return Vec::new();
        }
        let mut points: Vec<Point> = self
            .stable_points
            .iter()
            .chain(&self.tail_points)
            .copied()
            .collect();
        if self.options.simplify > 0.0 && self.options.simplify_phase == SimplifyPhase::Input {
            points = douglas_peucker(&points, self.options.simplify);
        }
        let len = points.len();
        if len == 0 {
            return Vec::new();
        }

        // One point: a dot.
        if len == 1 {
            let c = points[0];
            let size = self.get_size(size_override, c[2], 0, len, 0.0);
            if size < 0.5 {
                return Vec::new();
            }
            let mut ps = Vec::new();
            let mut theta = 0.0;
            while theta <= PI * 2.0 {
                ps.push(add(c, smul(rot([1.0, 0.0, 0.0], theta), size)));
                theta += PI / 16.0;
            }
            ps.push(add(
                c,
                smul(
                    [1.0, 0.0, 0.0],
                    self.get_size(size_override, c[2], 0, len, 0.0),
                ),
            ));
            return ps;
        }

        // Two points: half a circle around each.
        if len == 2 {
            let c = points[0];
            let n = points[1];
            let c_size = self.get_size(size_override, c[2], 0, len, 0.0);
            let n_size = self.get_size(size_override, n[2], 0, len, 0.0);
            if c_size < 0.5 || n_size < 0.5 {
                return Vec::new();
            }
            let mut ps = Vec::new();
            let p_angle = angle(c, [c[0], c[1] - 100.0, c[2]], n);
            let mut theta = p_angle;
            while theta <= PI + p_angle {
                ps.push(add(c, smul(rot([1.0, 0.0, 0.0], theta), c_size)));
                theta += PI / 16.0;
            }
            let mut theta = PI + p_angle;
            while theta <= PI * 2.0 + p_angle {
                ps.push(add(n, smul(rot([1.0, 0.0, 0.0], theta), n_size)));
                theta += PI / 16.0;
            }
            // (Only empty for NaN coordinates, where the JavaScript throws.)
            if let Some(&first) = ps.first() {
                ps.push(first);
            }
            return ps;
        }

        // Points on both sides of each inner point, and rounded corners.
        let mut forward_points: Vec<Point> = Vec::new();
        let mut backward_points: Vec<Point> = Vec::new();
        let mut prev_speed = 0.0;
        let mut visible_start_index = 0;
        let mut running_length = 0.0;
        for i in 1..len - 1 {
            let p = points[i - 1];
            let c = points[i];
            let n = points[i + 1];
            let pressure = c[2];
            let d = dist(p, c);
            running_length += d;
            let speed = prev_speed + (d - prev_speed) * 0.2;
            let c_size = self.get_size(size_override, pressure, i, len, running_length);
            if c_size == 0.0 {
                visible_start_index = i + 1;
                continue;
            }

            let dir_pc = norm(sub(p, c));
            let dir_nc = norm(sub(n, c));
            let p1dir_pc = rot(dir_pc, PI / 2.0);
            let p2dir_pc = rot(dir_pc, -PI / 2.0);
            let p1dir_nc = rot(dir_nc, PI / 2.0);
            let p2dir_nc = rot(dir_nc, -PI / 2.0);
            let p1_pc = add(c, smul(p1dir_pc, c_size));
            let p2_pc = add(c, smul(p2dir_pc, c_size));
            let p1_nc = add(c, smul(p1dir_nc, c_size));
            let p2_nc = add(c, smul(p2dir_nc, c_size));
            let ftdir = add(p1dir_pc, p2dir_nc);
            let btdir = add(p2dir_pc, p1dir_nc);
            let pa_pc = add(
                c,
                smul(
                    if mag(ftdir) == 0.0 {
                        dir_pc
                    } else {
                        norm(ftdir)
                    },
                    c_size,
                ),
            );
            let pa_nc = add(
                c,
                smul(
                    if mag(btdir) == 0.0 {
                        dir_nc
                    } else {
                        norm(btdir)
                    },
                    c_size,
                ),
            );

            let c_angle = norm_angle(angle(c, p, n));
            let d_angle =
                (CORNER_DETECTION_MAX_ANGLE / 180.0) * PI * corner_detection_variance(speed);
            if c_angle.abs() < d_angle {
                // The turn angle.
                let t_angle = norm_angle(PI - c_angle).abs();
                if t_angle == 0.0 {
                    continue;
                }
                if c_angle < 0.0 {
                    backward_points.extend([p2_pc, pa_nc]);
                    let mut theta = 0.0;
                    while theta <= t_angle {
                        forward_points.push(add(c, rot(smul(p1dir_pc, c_size), theta)));
                        theta += t_angle / 4.0;
                    }
                    let mut theta = t_angle;
                    while theta >= 0.0 {
                        backward_points.push(add(c, rot(smul(p1dir_pc, c_size), theta)));
                        theta -= t_angle / 4.0;
                    }
                    backward_points.extend([pa_nc, p1_nc]);
                } else {
                    forward_points.extend([p1_pc, pa_pc]);
                    let mut theta = 0.0;
                    while theta <= t_angle {
                        backward_points.push(add(c, rot(smul(p1dir_pc, -c_size), -theta)));
                        theta += t_angle / 4.0;
                    }
                    let mut theta = t_angle;
                    while theta >= 0.0 {
                        forward_points.push(add(c, rot(smul(p1dir_pc, -c_size), -theta)));
                        theta -= t_angle / 4.0;
                    }
                    forward_points.extend([pa_pc, p2_nc]);
                }
            } else {
                forward_points.push(pa_pc);
                backward_points.push(pa_nc);
            }
            prev_speed = speed;
        }

        // Nothing visible: nothing, or a dot at the end.
        if visible_start_index >= len - 2 {
            if self.options.keep_head {
                let c = points[len - 1];
                let mut ps = Vec::new();
                let mut theta = 0.0;
                while theta <= PI * 2.0 {
                    ps.push(add(c, smul(rot([1.0, 0.0, 0.0], theta), self.options.size)));
                    theta += PI / 16.0;
                }
                ps.push(add(c, smul([1.0, 0.0, 0.0], self.options.size)));
                return ps;
            }
            return Vec::new();
        }

        // The caps: half a turn at the start, one and a half at the end.
        let first = points[visible_start_index];
        let second = points[visible_start_index + 1];
        let penultimate = points[len - 2];
        let ultimate = points[len - 1];
        let dir_fs = norm(sub(second, first));
        let dir_pu = norm(sub(penultimate, ultimate));
        let ppdir_fs = rot(dir_fs, -PI / 2.0);
        let ppdir_pu = rot(dir_pu, PI / 2.0);
        let start_cap_size = self.get_size(size_override, first[2], 0, len, 0.0);
        let end_cap_size = if self.options.keep_head {
            self.options.size
        } else {
            self.get_size(size_override, penultimate[2], len - 2, len, running_length)
        };

        // The JavaScript `unshift`s each point: built reversed, then flipped.
        let mut start_cap: Vec<Point> = Vec::new();
        if start_cap_size > 0.1 {
            let mut theta = 0.0;
            while theta <= PI {
                start_cap.push(add(first, rot(smul(ppdir_fs, start_cap_size), -theta)));
                theta += PI / 16.0;
            }
            start_cap.push(add(first, smul(ppdir_fs, -start_cap_size)));
            start_cap.reverse();
        } else {
            start_cap.push(first);
        }
        let mut end_cap: Vec<Point> = Vec::new();
        let mut theta = 0.0;
        while theta <= PI * 3.0 {
            end_cap.push(add(ultimate, rot(smul(ppdir_pu, -end_cap_size), -theta)));
            theta += PI / 16.0;
        }

        let mut outline = start_cap.clone();
        outline.extend(forward_points);
        outline.extend(end_cap.into_iter().rev());
        outline.extend(backward_points.into_iter().rev());
        if let Some(&first) = start_cap.first() {
            outline.push(first);
        }
        if self.options.simplify > 0.0 && self.options.simplify_phase == SimplifyPhase::Output {
            return douglas_peucker(&outline, self.options.simplify);
        }
        outline
    }
}

// `math.ts`

fn add([ax, ay, ar]: Point, [bx, by, br]: Point) -> Point {
    [ax + bx, ay + by, ar + br]
}

fn sub([ax, ay, ar]: Point, [bx, by, br]: Point) -> Point {
    [ax - bx, ay - by, ar - br]
}

fn smul([x, y, r]: Point, s: f64) -> Point {
    [x * s, y * s, r * s]
}

/// The vector scaled to length 1 (`x ** 2` is `x * x`).
fn norm([x, y, r]: Point) -> Point {
    [x / (x * x + y * y).sqrt(), y / (x * x + y * y).sqrt(), r]
}

/// The vector rotated by `rad`.
fn rot([x, y, r]: Point, rad: f64) -> Point {
    [
        js::cos(rad) * x - js::sin(rad) * y,
        js::sin(rad) * x + js::cos(rad) * y,
        r,
    ]
}

/// The point `t` of the way from `a` to `b`.
fn plerp(a: Point, b: Point, t: f64) -> Point {
    add(a, smul(sub(b, a), t))
}

/// The angle at `p` from `p1` to `p2`.
fn angle(p: Point, p1: Point, p2: Point) -> f64 {
    js::atan2(p2[1] - p[1], p2[0] - p[0]) - js::atan2(p1[1] - p[1], p1[0] - p[0])
}

/// The angle brought into -π…π.
fn norm_angle(a: f64) -> f64 {
    js::atan2(js::sin(a), js::cos(a))
}

fn mag([x, y, _]: Point) -> f64 {
    (x * x + y * y).sqrt()
}

fn dist([ax, ay, _]: Point, [bx, by, _]: Point) -> f64 {
    ((bx - ax) * (bx - ax) + (by - ay) * (by - ay)).sqrt()
}

/// The length of a run of points, plus its last segment once more (as the
/// original does).
fn run_length(ps: &[Point]) -> f64 {
    if ps.len() < 2 {
        return 0.0;
    }
    let mut len = 0.0;
    for pair in ps.windows(2) {
        len += dist(pair[0], pair[1]);
    }
    len += dist(ps[ps.len() - 2], ps[ps.len() - 1]);
    len
}

fn clamp(v: f64, min: f64, max: f64) -> f64 {
    js::max(min, js::min(max, v))
}

/// The distance from `p3` to the segment from `p1` to `p2`.
fn distance_point_to_segment(p3: Point, p1: Point, p2: Point) -> f64 {
    let s_mag = dist(p1, p2);
    if s_mag == 0.0 {
        return dist(p3, p1);
    }
    let u = clamp(
        ((p3[0] - p1[0]) * (p2[0] - p1[0]) + (p3[1] - p1[1]) * (p2[1] - p1[1])) / (s_mag * s_mag),
        0.0,
        1.0,
    );
    let pi = [
        p1[0] + u * (p2[0] - p1[0]),
        p1[1] + u * (p2[1] - p1[1]),
        p3[2],
    ];
    dist(pi, p3)
}

// `simplify.ts`

/// Ramer–Douglas–Peucker: drops points closer than `epsilon` to the line
/// through their neighbours. (The original passes the farthest point twice
/// to its second half; harmless, and kept.)
fn douglas_peucker(points: &[Point], epsilon: f64) -> Vec<Point> {
    if epsilon == 0.0 || points.len() <= 2 {
        return points.to_vec();
    }
    let first = points[0];
    let last = points[points.len() - 1];
    let mut max_distance = 0.0;
    let mut max_index = None;
    for (index, &point) in points.iter().enumerate() {
        let distance = distance_point_to_segment(point, first, last);
        if distance > max_distance {
            max_distance = distance;
            max_index = Some(index);
        }
    }
    match max_index {
        // `None` only for epsilon <= 0 (when the JavaScript fails): keep all.
        Some(max_index) if max_distance >= epsilon => {
            let max_index_point = points[max_index];
            let mut head = vec![first];
            head.extend_from_slice(&points[1..max_index.max(1)]);
            head.push(max_index_point);
            let mut tail = vec![max_index_point];
            tail.extend_from_slice(&points[max_index..points.len() - 1]);
            tail.push(last);
            let mut out = douglas_peucker(&head, epsilon);
            out.pop();
            out.push(max_index_point);
            out.extend(douglas_peucker(&tail, epsilon).into_iter().skip(1));
            out
        }
        None if max_distance >= epsilon => points.to_vec(),
        _ => vec![first, last],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A laser pointer as Excalidraw sets one up for a constant-width
    /// stroke 1.4px wide (no simplification, every point at pressure 1).
    fn excalidraw_laser(points: &[[f64; 2]]) -> LaserPointer {
        let mut laser = LaserPointer::new(LaserPointerOptions {
            size: 1.4,
            streamline: 0.5,
            simplify: 0.0,
            size_mapping: |details| js::max(0.1, details.pressure),
            ..LaserPointerOptions::default()
        });
        for &[x, y] in points {
            laser.add_point([x, y, 1.0]);
        }
        laser
    }

    /// The distance from `p` to the polyline through `line`.
    fn distance_to_line(p: Point, line: &[Point]) -> f64 {
        line.windows(2)
            .map(|w| distance_point_to_segment(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn nothing_before_the_first_point() {
        assert!(excalidraw_laser(&[]).get_stroke_outline(None).is_empty());
    }

    #[test]
    fn repeated_points_are_ignored_and_new_ones_smoothed() {
        let laser = excalidraw_laser(&[[0.0, 0.0], [0.0, 0.0], [10.0, 0.0]]);
        assert_eq!(laser.original_points, [[0.0, 0.0, 1.0], [10.0, 0.0, 1.0]]);
        // Halfway, with streamline 0.5.
        assert_eq!(laser.tail_points, [[5.0, 0.0, 1.0]]);
    }

    #[test]
    fn long_tails_become_stable() {
        let points: Vec<[f64; 2]> = (0..40).map(|i| [f64::from(i) * 5.0, 0.0]).collect();
        let mut laser = excalidraw_laser(&points);
        assert!(
            !laser.stable_points.is_empty() && run_length(&laser.tail_points) <= MAX_TAIL_LENGTH
        );
        let outline = laser.get_stroke_outline(None);
        laser.close();
        assert!(laser.tail_points.is_empty());
        // Where the points are kept makes no difference to the outline.
        assert_eq!(laser.get_stroke_outline(None), outline);
    }

    #[test]
    fn one_point_is_a_dot() {
        let outline = excalidraw_laser(&[[3.0, 4.0]]).get_stroke_outline(None);
        assert!(outline.len() > 30);
        assert_eq!(
            outline.first().map(|p| [p[0], p[1]]),
            Some([3.0 + 1.4, 4.0])
        );
        for p in &outline {
            assert!((dist(*p, [3.0, 4.0, 0.0]) - 1.4).abs() < 1e-12);
        }
        // Too small to see: nothing.
        let laser = excalidraw_laser(&[[3.0, 4.0]]);
        assert!(laser.get_stroke_outline(Some(0.4)).is_empty());
    }

    #[test]
    fn two_points_are_a_capsule() {
        let laser = excalidraw_laser(&[[0.0, 0.0], [10.0, 0.0]]);
        let outline = laser.get_stroke_outline(None);
        // Half a circle around each point, closed.
        assert_eq!(outline.first(), outline.last());
        let line = [[0.0, 0.0, 1.0], [5.0, 0.0, 1.0]];
        for p in &outline {
            assert!((distance_to_line(*p, &line) - 1.4).abs() < 1e-12, "{p:?}");
        }
    }

    #[test]
    fn a_line_is_outlined_at_constant_width() {
        // A zigzag with sharp corners, which get rounded.
        let points: Vec<[f64; 2]> = (0..30)
            .map(|i| [f64::from(i) * 4.0, if i % 6 < 3 { 0.0 } else { 9.0 }])
            .collect();
        let laser = excalidraw_laser(&points);
        let smoothed: Vec<Point> = laser
            .stable_points
            .iter()
            .chain(&laser.tail_points)
            .copied()
            .collect();
        let outline = laser.get_stroke_outline(None);
        assert_eq!(outline.first(), outline.last());
        for p in &outline {
            assert!(distance_to_line(*p, &smoothed) < 1.4 + 1e-9, "{p:?}");
        }
        // Both ends are round: the outline passes 1.4 beyond them.
        let start = smoothed[0];
        assert!(
            outline
                .iter()
                .any(|p| (dist(*p, start) - 1.4).abs() < 1e-9 && p[0] < start[0] - 1.3)
        );
        let end = smoothed[smoothed.len() - 1];
        assert!(
            outline
                .iter()
                .any(|p| (dist(*p, end) - 1.4).abs() < 1e-9 && p[0] > end[0] + 1.3)
        );
    }

    #[test]
    fn simplified_outlines() {
        let points: Vec<[f64; 2]> = (0..30)
            .map(|i| [f64::from(i) * 4.0, f64::from(i % 5)])
            .collect();
        let mut laser = excalidraw_laser(&points);
        let full = laser.get_stroke_outline(None);
        laser.options.simplify = 0.5;
        let simplified = laser.get_stroke_outline(None);
        assert!(simplified.len() < full.len() && simplified.iter().all(|p| full.contains(p)));
        laser.options.simplify_phase = SimplifyPhase::Input;
        assert!(!laser.get_stroke_outline(None).is_empty());
    }

    #[test]
    fn douglas_peucker_keeps_corners() {
        let points = [
            [0.0, 0.0, 0.0],
            [1.0, 0.01, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 5.0, 0.0],
            [2.01, 6.0, 0.0],
            [2.0, 7.0, 0.0],
        ];
        assert_eq!(
            douglas_peucker(&points, 0.1),
            [points[0], points[2], points[5]]
        );
        assert_eq!(douglas_peucker(&points, 0.0), points);
    }
}
