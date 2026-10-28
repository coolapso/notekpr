//! The sketchy strokes: lines, curves, ellipses, arcs and SVG paths drawn
//! with rough's randomness, and the fills built on them. Ported from
//! rough.js 4.6.4 `renderer.ts` (MIT, see `crates/draw/PORTED.md`).
//!
//! Every random number comes from the options' sequence ([`Options::randomizer`]),
//! drawn in exactly rough's order: the arguments of a call are evaluated
//! left to right, and so are the elements of an array literal, so the code
//! keeps rough's expressions and their order as they are.

use std::f64::consts::PI;

use super::path_data::{self, PathError, Segment};
use super::{Op, OpSet, OpSetType, OpType, Options, Point, Random, fillers};
use crate::js;

/// A straight sketchy line (two strokes unless `disable_multi_stroke`).
pub fn line(x1: f64, y1: f64, x2: f64, y2: f64, o: &mut Options) -> OpSet {
    OpSet::new(OpSetType::Path, double_line(x1, y1, x2, y2, o, false))
}

/// Sketchy lines through `points`, back to the first one if `close`.
pub fn linear_path(points: &[Point], close: bool, o: &mut Options) -> OpSet {
    let len = points.len();
    if len > 2 {
        let mut ops = Vec::new();
        for i in 0..len - 1 {
            ops.extend(double_line(
                points[i][0],
                points[i][1],
                points[i + 1][0],
                points[i + 1][1],
                o,
                false,
            ));
        }
        if close {
            ops.extend(double_line(
                points[len - 1][0],
                points[len - 1][1],
                points[0][0],
                points[0][1],
                o,
                false,
            ));
        }
        OpSet::new(OpSetType::Path, ops)
    } else if len == 2 {
        line(points[0][0], points[0][1], points[1][0], points[1][1], o)
    } else {
        OpSet::new(OpSetType::Path, Vec::new())
    }
}

/// A closed [`linear_path`].
pub fn polygon(points: &[Point], o: &mut Options) -> OpSet {
    linear_path(points, true, o)
}

/// The outline of a rectangle.
pub fn rectangle(x: f64, y: f64, width: f64, height: f64, o: &mut Options) -> OpSet {
    let points = [
        [x, y],
        [x + width, y],
        [x + width, y + height],
        [x, y + height],
    ];
    polygon(&points, o)
}

/// A smooth sketchy curve through `points`: two strokes, the second with a
/// sequence of its own (seed + 1).
pub fn curve(points: &[Point], o: &mut Options) -> OpSet {
    let mut o1 = curve_with_offset(points, 1.0 * (1.0 + o.roughness * 0.2), o);
    if !o.disable_multi_stroke {
        let mut altered = clone_options_alter_seed(o);
        let o2 = curve_with_offset(points, 1.5 * (1.0 + o.roughness * 0.22), &mut altered);
        o1.extend(o2);
    }
    OpSet::new(OpSetType::Path, o1)
}

/// Several curves drawn as one, each continuing from where the last ended
/// (rough's `curve` with `Point[][]`). rough.js 4.6.4 takes one curve only;
/// this follows 4.6.6, the first version with several.
pub fn curves(lists: &[Vec<Point>], o: &mut Options) -> OpSet {
    let Some(first) = lists.first() else {
        return OpSet::new(OpSetType::Path, Vec::new());
    };
    let mut o1 = curve_with_offset(first, 1.0 * (1.0 + o.roughness * 0.2), o);
    let mut o2 = if o.disable_multi_stroke {
        Vec::new()
    } else {
        curve_with_offset(
            first,
            1.5 * (1.0 + o.roughness * 0.22),
            &mut clone_options_alter_seed(o),
        )
    };
    for points in &lists[1..] {
        if !points.is_empty() {
            let underlay = curve_with_offset(points, 1.0 * (1.0 + o.roughness * 0.2), o);
            let overlay = if o.disable_multi_stroke {
                Vec::new()
            } else {
                curve_with_offset(
                    points,
                    1.5 * (1.0 + o.roughness * 0.22),
                    &mut clone_options_alter_seed(o),
                )
            };
            o1.extend(underlay.into_iter().filter(|op| op.op != OpType::Move));
            o2.extend(overlay.into_iter().filter(|op| op.op != OpType::Move));
        }
    }
    o1.extend(o2);
    OpSet::new(OpSetType::Path, o1)
}

/// The outline of an ellipse centred on `(x, y)`.
pub fn ellipse(x: f64, y: f64, width: f64, height: f64, o: &mut Options) -> OpSet {
    let params = generate_ellipse_params(width, height, o);
    ellipse_with_params(x, y, o, &params).opset
}

/// The size of an ellipse after rough's randomness, and the angle between
/// its points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EllipseParams {
    pub increment: f64,
    pub rx: f64,
    pub ry: f64,
}

/// An ellipse's outline, and the points on it (for a fill).
#[derive(Clone, Debug, PartialEq)]
pub struct EllipseResult {
    pub opset: OpSet,
    pub estimated_points: Vec<Point>,
}

/// [`EllipseParams`] for an ellipse of `width` × `height`: more points for
/// a bigger ellipse, radii varied by `1 - curve_fitting`.
pub fn generate_ellipse_params(width: f64, height: f64, o: &mut Options) -> EllipseParams {
    let psq = (PI * 2.0 * ((sq(width / 2.0) + sq(height / 2.0)) / 2.0).sqrt()).sqrt();
    let step_count = js::max(
        o.curve_step_count,
        (o.curve_step_count / 200f64.sqrt()) * psq,
    )
    .ceil();
    let increment = (PI * 2.0) / step_count;
    let mut rx = (width / 2.0).abs();
    let mut ry = (height / 2.0).abs();
    let curve_fit_randomness = 1.0 - o.curve_fitting;
    rx += offset_opt(rx * curve_fit_randomness, o, 1.0);
    ry += offset_opt(ry * curve_fit_randomness, o, 1.0);
    EllipseParams { increment, rx, ry }
}

/// The ellipse of `params` centred on `(x, y)`: a curve through points
/// around it, going a little past the start, twice unless
/// `disable_multi_stroke` or `roughness` is 0.
pub fn ellipse_with_params(
    x: f64,
    y: f64,
    o: &mut Options,
    params: &EllipseParams,
) -> EllipseResult {
    let inner = offset(0.4, 1.0, o, 1.0);
    let overlap = params.increment * offset(0.1, inner, o, 1.0);
    let (ap1, cp1) = compute_ellipse_points(
        params.increment,
        x,
        y,
        params.rx,
        params.ry,
        1.0,
        overlap,
        o,
    );
    let mut o1 = curve_ops(&ap1, o);
    if !o.disable_multi_stroke && o.roughness != 0.0 {
        let (ap2, _) =
            compute_ellipse_points(params.increment, x, y, params.rx, params.ry, 1.5, 0.0, o);
        let o2 = curve_ops(&ap2, o);
        o1.extend(o2);
    }
    EllipseResult {
        estimated_points: cp1,
        opset: OpSet::new(OpSetType::Path, o1),
    }
}

/// Part of an ellipse centred on `(x, y)`, from angle `start` to `stop`
/// (radians); `closed` adds lines to the centre, sketchy if `rough_closure`.
#[allow(clippy::too_many_arguments)]
pub fn arc(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    start: f64,
    stop: f64,
    closed: bool,
    rough_closure: bool,
    o: &mut Options,
) -> OpSet {
    let cx = x;
    let cy = y;
    let mut rx = (width / 2.0).abs();
    let mut ry = (height / 2.0).abs();
    rx += offset_opt(rx * 0.01, o, 1.0);
    ry += offset_opt(ry * 0.01, o, 1.0);
    let (strt, stp) = normalize_arc_angles(start, stop);
    let ellipse_inc = (PI * 2.0) / o.curve_step_count;
    let arc_inc = js::min(ellipse_inc / 2.0, (stp - strt) / 2.0);
    let mut ops = arc_ops(arc_inc, cx, cy, rx, ry, strt, stp, 1.0, o);
    if !o.disable_multi_stroke {
        let o2 = arc_ops(arc_inc, cx, cy, rx, ry, strt, stp, 1.5, o);
        ops.extend(o2);
    }
    if closed {
        if rough_closure {
            let to_start =
                double_line(cx, cy, cx + rx * strt.cos(), cy + ry * strt.sin(), o, false);
            let to_stop = double_line(cx, cy, cx + rx * stp.cos(), cy + ry * stp.sin(), o, false);
            ops.extend(to_start);
            ops.extend(to_stop);
        } else {
            ops.push(Op::new(OpType::LineTo, vec![cx, cy]));
            ops.push(Op::new(
                OpType::LineTo,
                vec![cx + rx * strt.cos(), cy + ry * strt.sin()],
            ));
        }
    }
    OpSet::new(OpSetType::Path, ops)
}

/// Start and stop angles moved to start in `[0, 2π)`; more than a full
/// turn is one turn.
fn normalize_arc_angles(start: f64, stop: f64) -> (f64, f64) {
    let mut strt = start;
    let mut stp = stop;
    while strt < 0.0 {
        let next = strt + PI * 2.0;
        if next == strt {
            // Too large to move (or -∞): rough would loop forever.
            break;
        }
        strt = next;
        stp += PI * 2.0;
    }
    if (stp - strt) > (PI * 2.0) {
        strt = 0.0;
        stp = PI * 2.0;
    }
    (strt, stp)
}

/// The sketchy outline of SVG path data `path`.
pub fn svg_path(path: &str, o: &mut Options) -> Result<OpSet, PathError> {
    let segments = path_data::normalize(&path_data::absolutize(&path_data::parse_path(path)?));
    Ok(svg_path_segments(&segments, o))
}

/// [`svg_path`] of path data already normalized to `M`, `L`, `C`, `Z`.
pub(crate) fn svg_path_segments(segments: &[Segment], o: &mut Options) -> OpSet {
    let mut ops = Vec::new();
    let mut first = [0.0, 0.0];
    let mut current = [0.0, 0.0];
    for Segment { key, data } in segments {
        match key {
            'M' => {
                current = [data[0], data[1]];
                first = [data[0], data[1]];
            }
            'L' => {
                ops.extend(double_line(
                    current[0], current[1], data[0], data[1], o, false,
                ));
                current = [data[0], data[1]];
            }
            'C' => {
                let [x1, y1, x2, y2, x, y] = [data[0], data[1], data[2], data[3], data[4], data[5]];
                ops.extend(bezier_to(x1, y1, x2, y2, x, y, current, o));
                current = [x, y];
            }
            'Z' => {
                ops.extend(double_line(
                    current[0], current[1], first[0], first[1], o, false,
                ));
                current = [first[0], first[1]];
            }
            _ => {}
        }
    }
    OpSet::new(OpSetType::Path, ops)
}

/// A solid fill of `polygons`, its corners moved a little.
pub fn solid_fill_polygon(polygons: &[Vec<Point>], o: &mut Options) -> OpSet {
    let mut ops = Vec::new();
    for points in polygons {
        if !points.is_empty() {
            let offset = js::or(o.max_randomness_offset, 0.0);
            let len = points.len();
            if len > 2 {
                let x = points[0][0] + offset_opt(offset, o, 1.0);
                let y = points[0][1] + offset_opt(offset, o, 1.0);
                ops.push(Op::new(OpType::Move, vec![x, y]));
                for p in &points[1..] {
                    let x = p[0] + offset_opt(offset, o, 1.0);
                    let y = p[1] + offset_opt(offset, o, 1.0);
                    ops.push(Op::new(OpType::LineTo, vec![x, y]));
                }
            }
        }
    }
    OpSet::new(OpSetType::FillPath, ops)
}

/// A pattern fill of `polygons` in the options' fill style. The fillers
/// rotate the points in place and back, as rough's do (see
/// [`super::hachure_fill::hachure_lines`]).
pub fn pattern_fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    fillers::fill_polygons(polygons, o)
}

/// A pattern fill of an arc's wedge.
pub fn pattern_fill_arc(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    start: f64,
    stop: f64,
    o: &mut Options,
) -> OpSet {
    let cx = x;
    let cy = y;
    let mut rx = (width / 2.0).abs();
    let mut ry = (height / 2.0).abs();
    rx += offset_opt(rx * 0.01, o, 1.0);
    ry += offset_opt(ry * 0.01, o, 1.0);
    let (strt, stp) = normalize_arc_angles(start, stop);
    let increment = (stp - strt) / o.curve_step_count;
    let mut points = Vec::new();
    let mut angle = strt;
    while angle <= stp {
        points.push([cx + rx * angle.cos(), cy + ry * angle.sin()]);
        if increment.is_nan() || increment <= 0.0 {
            // rough would loop forever on a step that doesn't move.
            break;
        }
        angle += increment;
    }
    points.push([cx + rx * stp.cos(), cy + ry * stp.sin()]);
    points.push([cx, cy]);
    pattern_fill_polygons(&mut [points], o)
}

/// A random offset in `[-x, x)`, scaled by the roughness (`randOffset`).
pub fn rand_offset(x: f64, o: &mut Options) -> f64 {
    offset_opt(x, o, 1.0)
}

/// A random offset in `[min, max)`, scaled by the roughness
/// (`randOffsetWithRange`).
pub fn rand_offset_with_range(min: f64, max: f64, o: &mut Options) -> f64 {
    offset(min, max, o, 1.0)
}

/// A sketchy fill line: two strokes unless `disable_multi_stroke_fill`
/// (`doubleLineFillOps`).
pub fn double_line_fill_ops(x1: f64, y1: f64, x2: f64, y2: f64, o: &mut Options) -> Vec<Op> {
    double_line(x1, y1, x2, y2, o, true)
}

/// `Object.assign({}, o, changes)`: a copy of the options with `change`
/// applied, drawn with by `draw`. JavaScript copies the reference to the
/// random sequence, so the copy and `o` share it — if `o` has one yet.
pub(crate) fn with_copy<R>(
    o: &mut Options,
    change: impl FnOnce(&mut Options),
    draw: impl FnOnce(&mut Options) -> R,
) -> R {
    let mut copy = o.clone();
    change(&mut copy);
    let result = draw(&mut copy);
    if o.randomizer.is_some() {
        o.randomizer = copy.randomizer;
    }
    result
}

/// A copy of the options with a sequence of its own, seeded one higher
/// (`cloneOptionsAlterSeed`), for a curve's second stroke.
fn clone_options_alter_seed(o: &Options) -> Options {
    let mut result = o.clone();
    result.randomizer = None;
    if o.seed != 0 {
        result.seed = o.seed.wrapping_add(1);
    }
    result
}

/// The next number of the options' sequence, started from their seed on
/// first use (`random`).
pub(crate) fn random(o: &mut Options) -> f64 {
    let seed = o.seed;
    o.randomizer
        .get_or_insert_with(|| Random::new(seed))
        .next_f64()
}

/// Where rough calls `Math.random()`: the options' unseeded sequence (see
/// [`Random`]). Starting the options' sequence here leaves its seeded
/// numbers as they would be.
pub(crate) fn math_random(o: &mut Options) -> f64 {
    let seed = o.seed;
    o.randomizer
        .get_or_insert_with(|| Random::new(seed))
        .math_random()
}

fn offset(min: f64, max: f64, o: &mut Options, roughness_gain: f64) -> f64 {
    o.roughness * roughness_gain * ((random(o) * (max - min)) + min)
}

fn offset_opt(x: f64, o: &mut Options, roughness_gain: f64) -> f64 {
    offset(-x, x, o, roughness_gain)
}

/// `Math.pow(x, 2)`, which is exactly `x * x` (fdlibm special-cases it).
fn sq(x: f64) -> f64 {
    x * x
}

fn double_line(x1: f64, y1: f64, x2: f64, y2: f64, o: &mut Options, filling: bool) -> Vec<Op> {
    let single_stroke = if filling {
        o.disable_multi_stroke_fill
    } else {
        o.disable_multi_stroke
    };
    let mut o1 = line_ops(x1, y1, x2, y2, o, true, false);
    if single_stroke {
        return o1;
    }
    let o2 = line_ops(x1, y1, x2, y2, o, true, true);
    o1.extend(o2);
    o1
}

/// One stroke of a sketchy line: a Bézier curve bowed a little to one side,
/// its ends moved a little (`_line`); the `overlay` stroke moves them less.
fn line_ops(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    o: &mut Options,
    move_to: bool,
    overlay: bool,
) -> Vec<Op> {
    let length_sq = sq(x1 - x2) + sq(y1 - y2);
    let length = length_sq.sqrt();
    let roughness_gain = if length < 200.0 {
        1.0
    } else if length > 500.0 {
        0.4
    } else {
        (-0.0016668) * length + 1.233334
    };
    let mut offset = js::or(o.max_randomness_offset, 0.0);
    if (offset * offset * 100.0) > length_sq {
        offset = length / 10.0;
    }
    let half_offset = offset / 2.0;
    let diverge_point = 0.2 + random(o) * 0.2;
    let mut mid_disp_x = o.bowing * o.max_randomness_offset * (y2 - y1) / 200.0;
    let mut mid_disp_y = o.bowing * o.max_randomness_offset * (x1 - x2) / 200.0;
    mid_disp_x = offset_opt(mid_disp_x, o, roughness_gain);
    mid_disp_y = offset_opt(mid_disp_y, o, roughness_gain);
    let preserve_vertices = o.preserve_vertices;
    // The overlay stroke uses half the offset ("randomHalf"), the first the
    // full one ("randomFull").
    let off = if overlay { half_offset } else { offset };
    let mut ops = Vec::new();
    if move_to {
        let x = x1
            + if preserve_vertices {
                0.0
            } else {
                offset_opt(off, o, roughness_gain)
            };
        let y = y1
            + if preserve_vertices {
                0.0
            } else {
                offset_opt(off, o, roughness_gain)
            };
        ops.push(Op::new(OpType::Move, vec![x, y]));
    }
    let data = vec![
        mid_disp_x + x1 + (x2 - x1) * diverge_point + offset_opt(off, o, roughness_gain),
        mid_disp_y + y1 + (y2 - y1) * diverge_point + offset_opt(off, o, roughness_gain),
        mid_disp_x + x1 + 2.0 * (x2 - x1) * diverge_point + offset_opt(off, o, roughness_gain),
        mid_disp_y + y1 + 2.0 * (y2 - y1) * diverge_point + offset_opt(off, o, roughness_gain),
        x2 + if preserve_vertices {
            0.0
        } else {
            offset_opt(off, o, roughness_gain)
        },
        y2 + if preserve_vertices {
            0.0
        } else {
            offset_opt(off, o, roughness_gain)
        },
    ];
    ops.push(Op::new(OpType::BCurveTo, data));
    ops
}

/// The points moved by up to `offset`, the ends doubled, as a curve. An
/// empty list is no curve (rough 4.6.4 throws).
fn curve_with_offset(points: &[Point], offset: f64, o: &mut Options) -> Vec<Op> {
    if points.is_empty() {
        return Vec::new();
    }
    let mut ps = Vec::with_capacity(points.len() + 2);
    let moved = |p: Point, o: &mut Options| -> Point {
        let x = p[0] + offset_opt(offset, o, 1.0);
        let y = p[1] + offset_opt(offset, o, 1.0);
        [x, y]
    };
    ps.push(moved(points[0], o));
    ps.push(moved(points[0], o));
    for i in 1..points.len() {
        ps.push(moved(points[i], o));
        if i == points.len() - 1 {
            ps.push(moved(points[i], o));
        }
    }
    curve_ops(&ps, o)
}

/// A Catmull-Rom-like curve through `points[1..len - 1]` (the first and
/// last only steer it) as Bézier curves (`_curve`). rough's `closePoint`
/// argument is always null in 4.6.4, so it is left out.
fn curve_ops(points: &[Point], o: &mut Options) -> Vec<Op> {
    let len = points.len();
    let mut ops = Vec::new();
    if len > 3 {
        let s = 1.0 - o.curve_tightness;
        ops.push(Op::new(OpType::Move, vec![points[1][0], points[1][1]]));
        let mut i = 1;
        while i + 2 < len {
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
            ops.push(Op::new(
                OpType::BCurveTo,
                vec![b1[0], b1[1], b2[0], b2[1], b3[0], b3[1]],
            ));
            i += 1;
        }
    } else if len == 3 {
        ops.push(Op::new(OpType::Move, vec![points[1][0], points[1][1]]));
        ops.push(Op::new(
            OpType::BCurveTo,
            vec![
                points[1][0],
                points[1][1],
                points[2][0],
                points[2][1],
                points[2][0],
                points[2][1],
            ],
        ));
    } else if len == 2 {
        ops.extend(double_line(
            points[0][0],
            points[0][1],
            points[1][0],
            points[1][1],
            o,
            false,
        ));
    }
    ops
}

/// Points around an ellipse for [`curve_ops`] (all of them), and the ones
/// on it (`_computeEllipsePoints`). With roughness, each point is moved by
/// up to `offset` and the curve starts at a random angle and goes on past
/// the start by `overlap`.
#[allow(clippy::too_many_arguments)]
fn compute_ellipse_points(
    increment: f64,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    offset: f64,
    overlap: f64,
    o: &mut Options,
) -> (Vec<Point>, Vec<Point>) {
    let core_only = o.roughness == 0.0;
    let mut core_points = Vec::new();
    let mut all_points = Vec::new();
    if core_only {
        let increment = increment / 4.0;
        all_points.push([cx + rx * (-increment).cos(), cy + ry * (-increment).sin()]);
        let mut angle = 0.0;
        while angle <= PI * 2.0 {
            let p = [cx + rx * f64::cos(angle), cy + ry * f64::sin(angle)];
            core_points.push(p);
            all_points.push(p);
            if increment.is_nan() || increment <= 0.0 {
                // rough would loop forever on a step that doesn't move.
                break;
            }
            angle += increment;
        }
        all_points.push([cx + rx * f64::cos(0.0), cy + ry * f64::sin(0.0)]);
        all_points.push([cx + rx * increment.cos(), cy + ry * increment.sin()]);
    } else {
        let rad_offset = offset_opt(0.5, o, 1.0) - (PI / 2.0);
        let x = offset_opt(offset, o, 1.0) + cx + 0.9 * rx * (rad_offset - increment).cos();
        let y = offset_opt(offset, o, 1.0) + cy + 0.9 * ry * (rad_offset - increment).sin();
        all_points.push([x, y]);
        let end_angle = PI * 2.0 + rad_offset - 0.01;
        let mut angle = rad_offset;
        while angle < end_angle {
            let x = offset_opt(offset, o, 1.0) + cx + rx * angle.cos();
            let y = offset_opt(offset, o, 1.0) + cy + ry * angle.sin();
            core_points.push([x, y]);
            all_points.push([x, y]);
            if increment.is_nan() || increment <= 0.0 {
                break;
            }
            angle += increment;
        }
        let x =
            offset_opt(offset, o, 1.0) + cx + rx * (rad_offset + PI * 2.0 + overlap * 0.5).cos();
        let y =
            offset_opt(offset, o, 1.0) + cy + ry * (rad_offset + PI * 2.0 + overlap * 0.5).sin();
        all_points.push([x, y]);
        let x = offset_opt(offset, o, 1.0) + cx + 0.98 * rx * (rad_offset + overlap).cos();
        let y = offset_opt(offset, o, 1.0) + cy + 0.98 * ry * (rad_offset + overlap).sin();
        all_points.push([x, y]);
        let x = offset_opt(offset, o, 1.0) + cx + 0.9 * rx * (rad_offset + overlap * 0.5).cos();
        let y = offset_opt(offset, o, 1.0) + cy + 0.9 * ry * (rad_offset + overlap * 0.5).sin();
        all_points.push([x, y]);
    }
    (all_points, core_points)
}

/// One stroke of an arc (`_arc`).
#[allow(clippy::too_many_arguments)]
fn arc_ops(
    increment: f64,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    strt: f64,
    stp: f64,
    offset: f64,
    o: &mut Options,
) -> Vec<Op> {
    let rad_offset = strt + offset_opt(0.1, o, 1.0);
    let mut points = Vec::new();
    let x = offset_opt(offset, o, 1.0) + cx + 0.9 * rx * (rad_offset - increment).cos();
    let y = offset_opt(offset, o, 1.0) + cy + 0.9 * ry * (rad_offset - increment).sin();
    points.push([x, y]);
    let mut angle = rad_offset;
    while angle <= stp {
        let x = offset_opt(offset, o, 1.0) + cx + rx * angle.cos();
        let y = offset_opt(offset, o, 1.0) + cy + ry * angle.sin();
        points.push([x, y]);
        if increment.is_nan() || increment <= 0.0 {
            // rough would loop forever on a step that doesn't move.
            break;
        }
        angle += increment;
    }
    points.push([cx + rx * stp.cos(), cy + ry * stp.sin()]);
    points.push([cx + rx * stp.cos(), cy + ry * stp.sin()]);
    curve_ops(&points, o)
}

/// A sketchy cubic curve from `current` (`_bezierTo`): twice unless
/// `disable_multi_stroke`, the second starting a little off.
#[allow(clippy::too_many_arguments)]
fn bezier_to(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x: f64,
    y: f64,
    current: Point,
    o: &mut Options,
) -> Vec<Op> {
    let mut ops = Vec::new();
    let ros = [
        js::or(o.max_randomness_offset, 1.0),
        js::or(o.max_randomness_offset, 1.0) + 0.3,
    ];
    let iterations = if o.disable_multi_stroke { 1 } else { 2 };
    let preserve_vertices = o.preserve_vertices;
    for (i, &ro) in ros.iter().enumerate().take(iterations) {
        if i == 0 {
            ops.push(Op::new(OpType::Move, vec![current[0], current[1]]));
        } else {
            let mx = current[0]
                + if preserve_vertices {
                    0.0
                } else {
                    offset_opt(ros[0], o, 1.0)
                };
            let my = current[1]
                + if preserve_vertices {
                    0.0
                } else {
                    offset_opt(ros[0], o, 1.0)
                };
            ops.push(Op::new(OpType::Move, vec![mx, my]));
        }
        let f = if preserve_vertices {
            [x, y]
        } else {
            let fx = x + offset_opt(ro, o, 1.0);
            let fy = y + offset_opt(ro, o, 1.0);
            [fx, fy]
        };
        let data = vec![
            x1 + offset_opt(ro, o, 1.0),
            y1 + offset_opt(ro, o, 1.0),
            x2 + offset_opt(ro, o, 1.0),
            y2 + offset_opt(ro, o, 1.0),
            f[0],
            f[1],
        ];
        ops.push(Op::new(OpType::BCurveTo, data));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(seed: i32) -> Options {
        Options {
            seed,
            ..Options::default()
        }
    }

    #[test]
    fn a_line_is_two_strokes() {
        let set = line(0.0, 0.0, 100.0, 0.0, &mut seeded(1));
        let kinds: Vec<OpType> = set.ops.iter().map(|op| op.op).collect();
        assert_eq!(
            kinds,
            [
                OpType::Move,
                OpType::BCurveTo,
                OpType::Move,
                OpType::BCurveTo
            ]
        );
        // Ends stay within the offset (2 × roughness) of the line's ends.
        for op in &set.ops {
            let n = op.data.len();
            assert!(
                op.data[n - 2].abs() <= 100.0 + 2.0 && op.data[n - 1].abs() <= 2.0,
                "{op:?}"
            );
        }
        let single = line(
            0.0,
            0.0,
            100.0,
            0.0,
            &mut Options {
                disable_multi_stroke: true,
                ..seeded(1)
            },
        );
        assert_eq!(single.ops.len(), 2);
    }

    #[test]
    fn same_seed_same_line_and_roughness_zero_is_straight() {
        let a = line(3.0, 4.0, 50.0, 60.0, &mut seeded(42));
        let b = line(3.0, 4.0, 50.0, 60.0, &mut seeded(42));
        assert_eq!(a, b);
        assert_ne!(a, line(3.0, 4.0, 50.0, 60.0, &mut seeded(43)));
        let straight = line(
            0.0,
            0.0,
            10.0,
            0.0,
            &mut Options {
                roughness: 0.0,
                ..seeded(1)
            },
        );
        assert_eq!(straight.ops[0].data, [0.0, 0.0]);
        assert_eq!(straight.ops[1].data[4..], [10.0, 0.0]);
        assert!(
            straight.ops[1]
                .data
                .iter()
                .skip(1)
                .step_by(2)
                .all(|y| *y == 0.0)
        );
    }

    #[test]
    fn preserve_vertices_keeps_the_ends() {
        let mut o = Options {
            preserve_vertices: true,
            ..seeded(7)
        };
        let set = line(1.0, 2.0, 30.0, 40.0, &mut o);
        assert_eq!(set.ops[0].data, [1.0, 2.0]);
        assert_eq!(set.ops[1].data[4..], [30.0, 40.0]);
        assert_eq!(set.ops[2].data, [1.0, 2.0]);
    }

    #[test]
    fn random_numbers_are_drawn_in_rough_order() {
        // `_line` draws: divergePoint, midDispX, midDispY, then the ends.
        let mut o = seeded(5);
        let mut r = Random::new(5);
        let ops = line_ops(0.0, 0.0, 10.0, 0.0, &mut o, true, false);
        let diverge = 0.2 + r.next_f64() * 0.2;
        // offset is length / 10 = 1 for a line this short. The bow of a
        // horizontal line is 0 across x and 1 × 2 × -10 / 200 = -0.1 across
        // y, each then offset in [-bow, bow) as `_offsetOpt` does.
        let mid_x = 1.0 * 1.0 * ((r.next_f64() * (0.0 - -0.0)) + -0.0);
        let mid_y = 1.0 * 1.0 * ((r.next_f64() * (-0.1 - 0.1)) + 0.1);
        let off = |r: &mut Random| 1.0 * 1.0 * ((r.next_f64() * (1.0 - -1.0)) + -1.0);
        let mx = 0.0 + off(&mut r);
        let my = 0.0 + off(&mut r);
        assert_eq!(ops[0].data, [mx, my]);
        let c0 = mid_x + 0.0 + (10.0 - 0.0) * diverge + off(&mut r);
        let c1 = mid_y + 0.0 + (0.0 - 0.0) * diverge + off(&mut r);
        assert_eq!(ops[1].data[..2], [c0, c1]);
    }

    #[test]
    fn curves_and_ellipses_close_up() {
        let pts = [[0.0, 0.0], [50.0, 30.0], [100.0, 0.0], [150.0, 30.0]];
        let set = curve(&pts, &mut seeded(3));
        assert_eq!(set.ops.iter().filter(|op| op.op == OpType::Move).count(), 2);
        let e = ellipse_with_params(
            50.0,
            50.0,
            &mut seeded(3),
            &EllipseParams {
                increment: PI / 6.0,
                rx: 40.0,
                ry: 20.0,
            },
        );
        assert_eq!(e.estimated_points.len(), 12);
        for p in &e.estimated_points {
            let d = ((p[0] - 50.0) / 40.0).powi(2) + ((p[1] - 50.0) / 20.0).powi(2);
            assert!((d - 1.0).abs() < 0.2, "{p:?}");
        }
        // Roughness 0: one stroke, points exactly on the ellipse.
        let mut o = Options {
            roughness: 0.0,
            ..seeded(3)
        };
        let e = ellipse_with_params(
            0.0,
            0.0,
            &mut o,
            &EllipseParams {
                increment: PI / 2.0,
                rx: 10.0,
                ry: 10.0,
            },
        );
        assert_eq!(
            e.opset
                .ops
                .iter()
                .filter(|op| op.op == OpType::Move)
                .count(),
            1
        );
        assert_eq!(e.estimated_points[0], [10.0, 0.0]);
    }

    #[test]
    fn several_curves_continue_each_other() {
        let a = vec![[0.0, 0.0], [10.0, 10.0], [20.0, 0.0]];
        let b = vec![[20.0, 0.0], [30.0, 10.0], [40.0, 0.0]];
        let set = curves(&[a.clone(), b], &mut seeded(9));
        // One move per stroke: the second curve continues the first.
        assert_eq!(set.ops.iter().filter(|op| op.op == OpType::Move).count(), 2);
        // A single list draws like `curve`.
        assert_eq!(
            curves(std::slice::from_ref(&a), &mut seeded(9)),
            curve(&a, &mut seeded(9))
        );
        assert!(curves(&[], &mut seeded(9)).ops.is_empty());
    }

    #[test]
    fn arcs_and_wedges() {
        let set = arc(
            0.0,
            0.0,
            100.0,
            100.0,
            0.0,
            PI / 2.0,
            true,
            true,
            &mut seeded(4),
        );
        assert!(set.ops.len() > 4);
        let set = arc(
            0.0,
            0.0,
            100.0,
            100.0,
            -PI / 2.0,
            0.0,
            true,
            false,
            &mut seeded(4),
        );
        let last = set.ops.last().unwrap();
        assert_eq!(last.op, OpType::LineTo);
        // A start and stop that are equal give no endless loop.
        let set = arc(
            0.0,
            0.0,
            100.0,
            100.0,
            1.0,
            1.0,
            false,
            false,
            &mut seeded(4),
        );
        assert!(!set.ops.is_empty());
        let fill = pattern_fill_arc(0.0, 0.0, 100.0, 100.0, 0.0, PI, &mut seeded(4));
        assert_eq!(fill.kind, OpSetType::FillSketch);
        assert!(!fill.ops.is_empty());
    }

    #[test]
    fn svg_paths_draw_each_command() {
        let set = svg_path("M0 0 L100 0 C100 50 50 100 0 100 Z", &mut seeded(2)).unwrap();
        let kinds: Vec<OpType> = set.ops.iter().map(|op| op.op).collect();
        // Each line and the close: 2 strokes of move + curve; the curve 2 too.
        assert_eq!(kinds.len(), 3 * 4);
        assert!(
            kinds
                .chunks(2)
                .all(|c| c == [OpType::Move, OpType::BCurveTo])
        );
        // The curve's first stroke starts exactly at the current point.
        assert_eq!(set.ops[4].data, [100.0, 0.0]);
        assert!(svg_path("M0 0 L", &mut seeded(2)).is_err());
    }

    #[test]
    fn solid_fill_moves_corners_a_little() {
        let set = solid_fill_polygon(
            &[
                vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
                vec![[1.0, 1.0]],
            ],
            &mut seeded(1),
        );
        assert_eq!(set.kind, OpSetType::FillPath);
        assert_eq!(set.ops.len(), 3);
        assert!((set.ops[1].data[0] - 10.0).abs() <= 2.0);
    }

    #[test]
    fn copies_share_the_sequence_once_started() {
        let mut o = seeded(11);
        random(&mut o);
        let mut expected = o.clone();
        with_copy(&mut o, |c| c.roughness = 2.0, random);
        random(&mut expected);
        assert_eq!(o.randomizer, expected.randomizer);
        // Not started yet: the copy starts its own, `o` stays without.
        let mut fresh = seeded(11);
        with_copy(&mut fresh, |_| {}, random);
        assert!(fresh.randomizer.is_none());
    }
}
