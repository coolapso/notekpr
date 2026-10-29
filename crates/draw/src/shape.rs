//! The hand-drawn shape of each element, as Excalidraw generates it
//! (`shape.ts`, the arrowheads of `bounds.ts`; MIT, see `PORTED.md`): the
//! rough options an element is drawn with, rounded corners, diamonds,
//! ellipses, lines and arrows with their arrowheads, elbow arrows.
//!
//! Numbers go into SVG path strings the way JavaScript prints them, and the
//! arithmetic follows Excalidraw's order, so an element comes out exactly as
//! Excalidraw draws it.

use crate::color;
use crate::freehand::{self, Freedraw, Variability};
use crate::geom::{self, Point};
use crate::js;
use crate::rough::{Drawable, FillStyle, Generator, Op, OpSet, OpSetType, OpType, Options};
use crate::scene::{Element, ElementType, roundness};

pub const DEFAULT_PROPORTIONAL_RADIUS: f64 = 0.25;
pub const DEFAULT_ADAPTIVE_RADIUS: f64 = 32.0;
/// How close a line's ends must be to close it into a shape (px).
pub const LINE_CONFIRM_THRESHOLD: f64 = 8.0;
pub const ELBOW_ARROW_CORNER_RADIUS: f64 = 16.0;

/// Sloppiness `cartoonist`.
const CARTOONIST: f64 = 2.0;

/// How elements are being drawn.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    pub dark: bool,
    /// For an export (or a preview): embeds show as grey boxes.
    pub exporting: bool,
    /// The canvas colour, which outline arrowheads are filled with.
    pub canvas_background: String,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            dark: false,
            exporting: true,
            canvas_background: color::palette::WHITE.into(),
        }
    }
}

/// What an element is drawn with.
#[derive(Clone, Debug)]
pub enum ElementShape {
    /// Drawn directly, not with rough: text, images, frames, sticky notes;
    /// and kinds this code doesn't know.
    None,
    /// Rough shapes: one for a rectangle, diamond or ellipse; a line's or
    /// arrow's shaft, then its arrowheads.
    Rough(Vec<Drawable>),
    /// A pen stroke: a rough fill when the stroke closes a loop, and the
    /// outline of the stroke as SVG path data, filled with the stroke colour.
    Freedraw {
        fill: Option<Box<Drawable>>,
        outline: String,
    },
}

fn n(x: f64) -> String {
    js::number_to_string(x)
}

/// `canChangeRoundness`.
pub fn can_change_roundness(kind: &ElementType) -> bool {
    matches!(
        kind,
        ElementType::Rectangle
            | ElementType::Iframe
            | ElementType::Embeddable
            | ElementType::Line
            | ElementType::Diamond
            | ElementType::StickyNote
            | ElementType::Image
    )
}

/// `adjustRoughness`: small shapes are drawn less rough.
pub fn adjust_roughness(e: &Element) -> f64 {
    let roughness = e.roughness();
    let max_size = js::max(e.width(), e.height());
    let min_size = js::min(e.width(), e.height());
    if (min_size >= 20.0 && max_size >= 50.0)
        || (min_size >= 15.0 && e.roundness().is_some() && can_change_roundness(&e.kind()))
        || (e.kind().is_linear() && max_size >= 50.0)
    {
        return roughness;
    }
    js::min(roughness / (if max_size < 10.0 { 3.0 } else { 2.0 }), 2.5)
}

/// `isPathALoop`: a line whose ends meet (within a few pixels) is a shape.
pub fn is_path_a_loop(points: &[Point]) -> bool {
    if points.len() >= 3 {
        let (first, last) = (points[0], points[points.len() - 1]);
        return geom::distance(first, last) <= LINE_CONFIRM_THRESHOLD;
    }
    false
}

/// `generateRoughOptions`: how rough draws an element.
pub fn rough_options(e: &Element, continuous_path: bool, dark: bool) -> Options {
    let stroke_width = e.stroke_width();
    let stroke_style = e.stroke_style();
    let mut options = Options {
        seed: js::to_int32(e.f64("seed").unwrap_or(1.0)),
        stroke_line_dash: match stroke_style {
            "dashed" => Some(vec![8.0, 8.0 + stroke_width]),
            "dotted" => Some(vec![1.5, 6.0 + stroke_width]),
            _ => None,
        },
        // Dashes and dots would lie on top of each other twice over.
        disable_multi_stroke: stroke_style != "solid",
        // …and look thinner: they're drawn a little wider.
        stroke_width: if stroke_style != "solid" {
            stroke_width + 0.5
        } else {
            stroke_width
        },
        fill_weight: stroke_width / 2.0,
        hachure_gap: stroke_width * 4.0,
        roughness: adjust_roughness(e),
        stroke: color::themed(e.stroke_color(), dark),
        preserve_vertices: continuous_path || e.roughness() < CARTOONIST,
        ..Options::default()
    };
    let background = e.background_color();
    match e.kind() {
        ElementType::Rectangle
        | ElementType::Iframe
        | ElementType::Embeddable
        | ElementType::Diamond
        | ElementType::Ellipse => {
            options.fill_style = FillStyle::from_name(e.fill_style());
            options.fill =
                (!color::is_transparent(background)).then(|| color::themed(background, dark));
            if e.kind() == ElementType::Ellipse {
                options.curve_fitting = 1.0;
            }
        }
        ElementType::Line | ElementType::Freedraw if is_path_a_loop(&e.points()) => {
            options.fill_style = FillStyle::from_name(e.fill_style());
            options.fill = (background != "transparent").then(|| color::themed(background, dark));
        }
        _ => {}
    }
    options
}

/// `getCornerRadius`: the radius of rounded corners on a side of length `x`.
pub fn corner_radius(x: f64, e: &Element) -> f64 {
    match e.roundness() {
        Some(r) if r.kind == roundness::PROPORTIONAL_RADIUS || r.kind == roundness::LEGACY => {
            x * DEFAULT_PROPORTIONAL_RADIUS
        }
        Some(r) if r.kind == roundness::ADAPTIVE_RADIUS => {
            let fixed = r.value.unwrap_or(DEFAULT_ADAPTIVE_RADIUS);
            let cutoff = fixed / DEFAULT_PROPORTIONAL_RADIUS;
            if x <= cutoff {
                x * DEFAULT_PROPORTIONAL_RADIUS
            } else {
                fixed
            }
        }
        _ => 0.0,
    }
}

/// `getDiamondPoints`: top, right, bottom, left (the +1 keeps them off 0,
/// which rough refuses).
pub fn diamond_points(e: &Element) -> [Point; 4] {
    let (w, h) = (e.width(), e.height());
    let top_x = (w / 2.0).floor() + 1.0;
    let right_y = (h / 2.0).floor() + 1.0;
    [[top_x, 0.0], [w, right_y], [top_x, h], [0.0, right_y]]
}

/// An embed's look when it can't show what it embeds (always, here: notekpr
/// loads nothing from elsewhere) — `modifyIframeLikeForRoughOptions`.
fn iframe_like(e: &Element, exporting: bool) -> Element {
    let mut e = e.clone();
    let transparent = |c: &str| color::is_transparent(c);
    if (exporting || e.kind() == ElementType::Embeddable)
        && transparent(e.background_color())
        && transparent(e.stroke_color())
    {
        e.set("roughness", serde_json::json!(0));
        e.set("backgroundColor", serde_json::json!("#d3d3d3"));
        e.set("fillStyle", serde_json::json!("solid"));
    } else if e.kind() == ElementType::Iframe {
        if transparent(e.stroke_color()) {
            e.set("strokeColor", serde_json::json!("#000000"));
        }
        if transparent(e.background_color()) {
            e.set("backgroundColor", serde_json::json!("#f4f4f6"));
        }
    }
    e
}

/// The rounded rectangle's outline (corners as quadratic curves).
fn rounded_rectangle_path(w: f64, h: f64, r: f64) -> String {
    format!(
        "M {r} 0 L {wr} 0 Q {w} 0, {w} {r} L {w} {hr} Q {w} {h}, {wr} {h} L {r} {h} Q 0 {h}, 0 {hr} L 0 {r} Q 0 0, {r} 0",
        r = n(r),
        w = n(w),
        h = n(h),
        wr = n(w - r),
        hr = n(h - r),
    )
}

/// The rounded diamond's outline.
fn rounded_diamond_path(p: [Point; 4], vr: f64, hr: f64) -> String {
    let [
        [top_x, top_y],
        [right_x, right_y],
        [bottom_x, bottom_y],
        [left_x, left_y],
    ] = p;
    // Excalidraw's template has line breaks and indentation in it; the path
    // parser treats them as spaces.
    format!(
        "M {} {} L {} {}\n            C {} {}, {} {}, {} {}\n            L {} {}\n            C {} {}, {} {}, {} {}\n            L {} {}\n            C {} {}, {} {}, {} {}\n            L {} {}\n            C {} {}, {} {}, {} {}",
        n(top_x + vr),
        n(top_y + hr),
        n(right_x - vr),
        n(right_y - hr),
        n(right_x),
        n(right_y),
        n(right_x),
        n(right_y),
        n(right_x - vr),
        n(right_y + hr),
        n(bottom_x + vr),
        n(bottom_y - hr),
        n(bottom_x),
        n(bottom_y),
        n(bottom_x),
        n(bottom_y),
        n(bottom_x - vr),
        n(bottom_y - hr),
        n(left_x + vr),
        n(left_y + hr),
        n(left_x),
        n(left_y),
        n(left_x),
        n(left_y),
        n(left_x + vr),
        n(left_y - hr),
        n(top_x - vr),
        n(top_y + hr),
        n(top_x),
        n(top_y),
        n(top_x),
        n(top_y),
        n(top_x + vr),
        n(top_y + hr),
    )
}

/// `generateElbowArrowShape`: an elbow arrow's path, its corners rounded.
pub fn elbow_arrow_path(route: &[Point], radius: f64) -> String {
    // A jog too short to see is drawn straight: rough would draw its two
    // corners as a knot.
    let is_jog = |i: usize| {
        if i < 2 || i + 2 > route.len() {
            return false;
        }
        let (a, b, c, d) = (route[i - 2], route[i - 1], route[i], route[i + 1]);
        geom::distance(b, c) < 1.0
            && geom::distance(a, b) >= 1.0
            && geom::distance(c, d) >= 1.0
            && (b[0] - a[0]) * (d[0] - c[0]) + (b[1] - a[1]) * (d[1] - c[1]) > 0.0
    };
    let points: Vec<Point> = route
        .iter()
        .enumerate()
        .filter(|(i, _)| !is_jog(*i) && !is_jog(i + 1))
        .map(|(_, p)| *p)
        .collect();
    if points.is_empty() {
        return String::new();
    }
    let mut sub: Vec<Point> = Vec::new();
    for i in 1..points.len().saturating_sub(1) {
        let (prev, point, next) = (points[i - 1], points[i], points[i + 1]);
        let prev_horizontal = geom::heading_is_horizontal(point, prev);
        let next_horizontal = geom::heading_is_horizontal(next, point);
        let corner = js::min(
            js::min(radius, geom::distance(points[i], next) / 2.0),
            geom::distance(points[i], prev) / 2.0,
        );
        let side = |horizontal: bool, other: Point| -> Point {
            if horizontal {
                if other[0] < point[0] {
                    [point[0] - corner, point[1]]
                } else {
                    [point[0] + corner, point[1]]
                }
            } else if other[1] < point[1] {
                [point[0], point[1] - corner]
            } else {
                [point[0], point[1] + corner]
            }
        };
        sub.push(side(prev_horizontal, prev));
        sub.push(point);
        sub.push(side(next_horizontal, next));
    }
    let mut d = vec![format!("M {} {}", n(points[0][0]), n(points[0][1]))];
    for c in sub.chunks(3) {
        d.push(format!("L {} {}", n(c[0][0]), n(c[0][1])));
        d.push(format!(
            "Q {} {}, {} {}",
            n(c[1][0]),
            n(c[1][1]),
            n(c[2][0]),
            n(c[2][1])
        ));
    }
    let last = points[points.len() - 1];
    d.push(format!("L {} {}", n(last[0]), n(last[1])));
    d.join(" ")
}

/// Which end of a line or arrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

/// Composite cardinality markers' parts, in multiples of the part's size
/// (negative: toward the tip).
const CARDINALITY_ONE_OR_MANY_BAR_OFFSET: f64 = -0.25;
const CARDINALITY_INNER_BAR_OFFSET: f64 = -0.5;
const CARDINALITY_ZERO_CIRCLE_OFFSET: f64 = 1.5;
const CARDINALITY_ZERO_CIRCLE_SCALE: f64 = 0.8;
const ARROW_ARROWHEAD_ANGLE: f64 = 20.0;
const ARROW_ARROWHEAD_WIDEST_ANGLE: f64 = 30.0;
const ARROW_ARROWHEAD_WIDEST_SIZE: f64 = 10.0;

/// `getArrowheadSize` (px).
pub fn arrowhead_size(arrowhead: &str) -> f64 {
    match arrowhead {
        "arrow" => 25.0,
        "diamond" | "diamond_outline" => 12.0,
        "cardinality_many" | "cardinality_one_or_many" | "cardinality_zero_or_many" => 15.0,
        "cardinality_one" | "cardinality_exactly_one" | "cardinality_zero_or_one" => 20.0,
        _ => 15.0,
    }
}

/// `getArrowheadAngle` (degrees) at `size`; an `arrow` opens wider as it
/// shrinks.
pub fn arrowhead_angle(arrowhead: &str, size: f64) -> f64 {
    match arrowhead {
        "bar" => 90.0,
        "arrow" => {
            let full = arrowhead_size(arrowhead);
            let shrinkage = ((full - size) / (full - ARROW_ARROWHEAD_WIDEST_SIZE)).clamp(0.0, 1.0);
            ARROW_ARROWHEAD_ANGLE
                + (ARROW_ARROWHEAD_WIDEST_ANGLE - ARROW_ARROWHEAD_ANGLE) * shrinkage
        }
        _ => 25.0,
    }
}

/// The last segment at an arrowhead's end, the one before, and how much the
/// arrow turns between them (`getArrowheadCorner`).
struct Corner {
    length: f64,
    prev_length: f64,
    turn: f64,
}

fn arrowhead_corner(e: &Element, points: &[Point], end: End) -> Option<Corner> {
    let len = points.len();
    let (tip, corner, prev) = match end {
        End::End if len >= 3 => (points[len - 1], points[len - 2], points[len - 3]),
        End::Start if len >= 3 => (points[0], points[1], points[2]),
        _ => return None,
    };
    let mut length = geom::distance(tip, corner);
    let mut prev_length = geom::distance(corner, prev);
    if length == 0.0 || prev_length == 0.0 {
        return None;
    }
    // Elbow arrows round their corners: the turn is halfway into it.
    if e.is_elbowed() {
        let rounding = js::min(
            js::min(ELBOW_ARROW_CORNER_RADIUS, length / 2.0),
            prev_length / 2.0,
        ) / 2.0;
        length -= rounding;
        prev_length -= rounding;
    }
    let cos =
        geom::dot(geom::vector(tip, corner), geom::vector(corner, prev)) / (length * prev_length);
    Some(Corner {
        length,
        prev_length,
        turn: cos.clamp(-1.0, 1.0).acos(),
    })
}

/// `getSidesClearSize`.
fn sides_clear_size(c: &Corner, side_angle: f64, clearance: f64) -> f64 {
    let angle = geom::radians(side_angle);
    if c.turn <= angle {
        return f64::INFINITY;
    }
    (c.length * js::sin(c.turn) - clearance) / js::sin(c.turn - angle)
}

/// `getCardinalityMarkerScale`.
fn cardinality_scale(marker: &str, c: &Corner, stroke_width: f64) -> f64 {
    let bar = |offset: f64| {
        let angle = geom::radians(arrowhead_angle(
            "cardinality_one",
            arrowhead_size("cardinality_one"),
        ));
        c.length
            / (arrowhead_size("cardinality_one")
                * (offset + js::cos(angle) + js::sin(angle) * (c.turn / 2.0).tan()))
    };
    let crows_foot = || c.length / arrowhead_size("cardinality_many");
    let circle = || {
        (c.length - ((stroke_width - 2.0) * CARDINALITY_ZERO_CIRCLE_SCALE) / 2.0 - stroke_width)
            / (arrowhead_size("circle_outline")
                * (CARDINALITY_ZERO_CIRCLE_OFFSET + CARDINALITY_ZERO_CIRCLE_SCALE / 2.0))
    };
    let min = |xs: &[f64]| xs.iter().fold(1.0, |m, x| js::min(m, *x));
    match marker {
        "cardinality_one" => min(&[bar(0.0)]),
        "cardinality_many" => min(&[crows_foot()]),
        "cardinality_one_or_many" => min(&[crows_foot(), bar(CARDINALITY_ONE_OR_MANY_BAR_OFFSET)]),
        "cardinality_exactly_one" => min(&[bar(CARDINALITY_INNER_BAR_OFFSET), bar(0.0)]),
        "cardinality_zero_or_one" => min(&[circle(), bar(CARDINALITY_INNER_BAR_OFFSET)]),
        "cardinality_zero_or_many" => min(&[crows_foot(), circle()]),
        _ => 1.0,
    }
}

/// `getArrowheadSizeBeyondLastSegment`: how large an arrowhead may grow when
/// the last segment is too short for it.
fn size_beyond_last_segment(e: &Element, points: &[Point], end: End, arrowhead: &str) -> f64 {
    let Some(c) = arrowhead_corner(e, points, end) else {
        return 0.0;
    };
    let max_size = (c.length + c.prev_length) / 2.0;
    let curved = e.roundness().is_some() && !e.is_elbowed();
    let marker = match end {
        End::Start => e.start_arrowhead(),
        End::End => e.end_arrowhead(),
    };
    if let Some(m) = marker.filter(|m| m.starts_with("cardinality_")) {
        return if curved {
            0.0
        } else {
            arrowhead_size(arrowhead) * cardinality_scale(m, &c, e.stroke_width())
        };
    }
    if arrowhead == "circle" || arrowhead == "circle_outline" {
        return max_size;
    }
    if curved {
        return 0.0;
    }
    match arrowhead {
        "bar" => js::min(max_size, c.length / (c.turn / 2.0).tan()),
        "diamond" | "diamond_outline" => c.length / 2.0,
        "triangle" | "triangle_outline" => js::min(
            max_size,
            sides_clear_size(
                &c,
                arrowhead_angle(arrowhead, arrowhead_size(arrowhead)),
                0.0,
            ),
        ),
        "arrow" => {
            let legible = arrowhead_size(arrowhead) / 2.0;
            js::min(
                js::min(
                    js::min(legible, max_size),
                    sides_clear_size(&c, arrowhead_angle(arrowhead, legible), e.stroke_width()),
                ),
                sides_clear_size(&c, arrowhead_angle(arrowhead, 0.0), e.stroke_width()),
            )
        }
        _ => 0.0,
    }
}

/// `getCurvePathOps`: the operations of a shape's outline.
fn curve_path_ops(d: &Drawable) -> &[Op] {
    d.sets
        .iter()
        .find(|s| s.kind == OpSetType::Path)
        .or(d.sets.first())
        .map_or(&[], |s| s.ops.as_slice())
}

/// `getArrowheadPoints`: where an arrowhead's corners are, for the shaft
/// `shaft` drawn of the arrow's `points`.
pub fn arrowhead_points(
    e: &Element,
    points: &[Point],
    shaft: &Drawable,
    end: End,
    arrowhead: &str,
    offset_multiplier: f64,
) -> Option<Vec<f64>> {
    let ops = curve_path_ops(shaft);
    if ops.is_empty() {
        return None;
    }
    let index = match end {
        End::Start => 1,
        End::End => ops.len() - 1,
    };
    let data = &ops.get(index)?.data;
    if data.len() != 6 {
        return None;
    }
    let (p1, p2, p3) = ([data[0], data[1]], [data[2], data[3]], [data[4], data[5]]);
    // The curve's start: the previous curve's end, or where it moved to.
    let prev = &ops[index - 1];
    let p0 = match prev.op {
        OpType::Move => [prev.data[0], prev.data[1]],
        OpType::BCurveTo => [prev.data[4], prev.data[5]],
        OpType::LineTo => [0.0, 0.0],
    };
    let equation = |t: f64, i: usize| {
        (1.0 - t).powf(3.0) * p3[i]
            + 3.0 * t * (1.0 - t).powf(2.0) * p2[i]
            + 3.0 * t.powf(2.0) * (1.0 - t) * p1[i]
            + p0[i] * t.powf(3.0)
    };
    let [x2, y2] = match end {
        End::Start => p0,
        End::End => p3,
    };
    let (x1, y1) = (equation(0.3, 0), equation(0.3, 1));
    let distance = js::hypot(x2 - x1, y2 - y1);
    let nx = (x2 - x1) / distance;
    let ny = (y2 - y1) / distance;
    let size = arrowhead_size(arrowhead);
    let length = {
        let len = points.len();
        let [cx, cy] = match end {
            End::End => points[len - 1],
            End::Start => points[0],
        };
        let [px, py] = if len > 1 {
            match end {
                End::End => points[len - 2],
                End::Start => points[1],
            }
        } else {
            [0.0, 0.0]
        };
        js::hypot(cx - px, cy - py)
    };
    let multiplier = if arrowhead == "diamond" || arrowhead == "diamond_outline" {
        0.25
    } else {
        0.5
    };
    let mut min_size = js::min(size, length * multiplier);
    if min_size < size {
        min_size = js::min(
            size,
            js::max(
                min_size,
                size_beyond_last_segment(e, points, end, arrowhead),
            ),
        );
    }
    let tx = x2 - nx * min_size * offset_multiplier;
    let ty = y2 - ny * min_size * offset_multiplier;
    let xs = tx - nx * min_size;
    let ys = ty - ny * min_size;
    if arrowhead == "circle" || arrowhead == "circle_outline" {
        let diameter = js::hypot(ys - ty, xs - tx) + e.stroke_width() - 2.0;
        return Some(vec![tx, ty, diameter]);
    }
    let angle = arrowhead_angle(arrowhead, min_size);
    if arrowhead == "cardinality_many" || arrowhead == "cardinality_one_or_many" {
        let [x3, y3] = geom::rotate([tx, ty], [xs, ys], geom::radians(-angle));
        let [x4, y4] = geom::rotate([tx, ty], [xs, ys], geom::radians(angle));
        return Some(vec![xs, ys, x3, y3, x4, y4]);
    }
    let [x3, y3] = geom::rotate([xs, ys], [tx, ty], (-angle * std::f64::consts::PI) / 180.0);
    let [x4, y4] = geom::rotate([xs, ys], [tx, ty], geom::radians(angle));
    if arrowhead == "diamond" || arrowhead == "diamond_outline" {
        // The corner opposite the tip.
        let len = points.len();
        let [ox, oy] = match end {
            End::Start => {
                let [px, py] = if len > 1 { points[1] } else { [0.0, 0.0] };
                geom::rotate(
                    [tx + min_size * 2.0, ty],
                    [tx, ty],
                    js::atan2(py - ty, px - tx),
                )
            }
            End::End => {
                let [px, py] = if len > 1 { points[len - 2] } else { [0.0, 0.0] };
                geom::rotate(
                    [tx - min_size * 2.0, ty],
                    [tx, ty],
                    js::atan2(ty - py, tx - px),
                )
            }
        };
        return Some(vec![tx, ty, x3, y3, ox, oy, x4, y4]);
    }
    Some(vec![tx, ty, x3, y3, x4, y4])
}

/// `getArrowheadLineOptions`: arrowheads drawn as lines are solid (dotted
/// ones get tighter dots), and at most a little rough.
fn arrowhead_line_options(e: &Element, options: &Options) -> Options {
    let mut o = options.clone();
    if e.stroke_style() == "dotted" {
        let w = e.stroke_width() - 1.0;
        o.stroke_line_dash = Some(vec![1.5, 6.0 + w - 1.0]);
    } else {
        o.stroke_line_dash = None;
    }
    o.roughness = js::min(1.0, o.roughness);
    o
}

/// `fillArrowheadOutline`: fills a small outline along its own stroke
/// (rough's own fill would wander off it).
fn fill_outline(mut outline: Drawable, fill: String) -> Drawable {
    let ops: Vec<Op> = outline
        .sets
        .iter()
        .flat_map(|s| s.ops.iter().cloned())
        .enumerate()
        .map(|(i, mut op)| {
            if i > 0 && op.op == OpType::Move {
                op.op = OpType::LineTo;
            }
            op
        })
        .collect();
    outline.options.fill = Some(fill);
    outline.sets.insert(
        0,
        OpSet {
            kind: OpSetType::FillPath,
            ops,
        },
    );
    outline
}

fn lines_to_tip(g: &Generator, p: Option<Vec<f64>>, o: &Options) -> Vec<Drawable> {
    match p {
        Some(p) if p.len() >= 6 => vec![
            g.line(p[2], p[3], p[0], p[1], o),
            g.line(p[4], p[5], p[0], p[1], o),
        ],
        _ => Vec::new(),
    }
}

fn cardinality_one(g: &Generator, p: Option<Vec<f64>>, o: &Options) -> Vec<Drawable> {
    match p {
        Some(p) if p.len() >= 6 => vec![g.line(p[2], p[3], p[4], p[5], o)],
        _ => Vec::new(),
    }
}

fn outline_circle(
    g: &Generator,
    options: &Options,
    stroke: &str,
    p: Option<Vec<f64>>,
    fill: String,
    scale: f64,
) -> Vec<Drawable> {
    let Some(p) = p.filter(|p| p.len() >= 3) else {
        return Vec::new();
    };
    let o = Options {
        stroke: stroke.into(),
        roughness: js::min(0.5, options.roughness),
        disable_multi_stroke: true,
        stroke_line_dash: None,
        ..options.clone()
    };
    vec![fill_outline(g.circle(p[0], p[1], p[2] * scale, &o), fill)]
}

/// `getArrowheadShapes`.
fn arrowhead_shapes(
    e: &Element,
    points: &[Point],
    shaft: &Drawable,
    end: End,
    arrowhead: &str,
    options: &Options,
    cfg: &RenderConfig,
) -> Vec<Drawable> {
    let g = Generator;
    let stroke = color::themed(e.stroke_color(), cfg.dark);
    let background = color::themed(&cfg.canvas_background, cfg.dark);
    let at = |kind: &str, offset: f64| arrowhead_points(e, points, shaft, end, kind, offset);
    let polygon = |corners: &[Point], fill: String| {
        let o = Options {
            roughness: js::min(1.0, options.roughness),
            disable_multi_stroke: true,
            preserve_vertices: true,
            stroke_line_dash: None,
            ..options.clone()
        };
        vec![fill_outline(g.polygon(corners, &o), fill)]
    };
    match arrowhead {
        "circle" | "circle_outline" => {
            let fill = if arrowhead == "circle_outline" {
                background
            } else {
                stroke.clone()
            };
            outline_circle(&g, options, &stroke, at(arrowhead, 0.0), fill, 1.0)
        }
        "triangle" | "triangle_outline" => match at(arrowhead, 0.0) {
            Some(p) if p.len() >= 6 => {
                let fill = if arrowhead == "triangle_outline" {
                    background
                } else {
                    stroke
                };
                polygon(&[[p[0], p[1]], [p[2], p[3]], [p[4], p[5]]], fill)
            }
            _ => Vec::new(),
        },
        "diamond" | "diamond_outline" => match at(arrowhead, 0.0) {
            Some(p) if p.len() >= 8 => {
                let fill = if arrowhead == "diamond_outline" {
                    background
                } else {
                    stroke
                };
                polygon(
                    &[[p[0], p[1]], [p[2], p[3]], [p[4], p[5]], [p[6], p[7]]],
                    fill,
                )
            }
            _ => Vec::new(),
        },
        "cardinality_one" => {
            cardinality_one(&g, at(arrowhead, 0.0), &arrowhead_line_options(e, options))
        }
        "cardinality_many" => {
            lines_to_tip(&g, at(arrowhead, 0.0), &arrowhead_line_options(e, options))
        }
        "cardinality_one_or_many" => {
            let o = arrowhead_line_options(e, options);
            let mut out = lines_to_tip(&g, at("cardinality_many", 0.0), &o);
            out.extend(cardinality_one(
                &g,
                at("cardinality_one", CARDINALITY_ONE_OR_MANY_BAR_OFFSET),
                &o,
            ));
            out
        }
        "cardinality_exactly_one" => {
            let o = arrowhead_line_options(e, options);
            let mut out =
                cardinality_one(&g, at("cardinality_one", CARDINALITY_INNER_BAR_OFFSET), &o);
            out.extend(cardinality_one(&g, at("cardinality_one", 0.0), &o));
            out
        }
        "cardinality_zero_or_one" => {
            let o = arrowhead_line_options(e, options);
            let mut out = outline_circle(
                &g,
                options,
                &stroke,
                at("circle_outline", CARDINALITY_ZERO_CIRCLE_OFFSET),
                background,
                CARDINALITY_ZERO_CIRCLE_SCALE,
            );
            out.extend(cardinality_one(
                &g,
                at("cardinality_one", CARDINALITY_INNER_BAR_OFFSET),
                &o,
            ));
            out
        }
        "cardinality_zero_or_many" => {
            let o = arrowhead_line_options(e, options);
            let mut out = lines_to_tip(&g, at("cardinality_many", 0.0), &o);
            out.extend(outline_circle(
                &g,
                options,
                &stroke,
                at("circle_outline", CARDINALITY_ZERO_CIRCLE_OFFSET),
                background,
                CARDINALITY_ZERO_CIRCLE_SCALE,
            ));
            out
        }
        // `arrow`, `bar`, and anything newer.
        _ => lines_to_tip(&g, at(arrowhead, 0.0), &arrowhead_line_options(e, options)),
    }
}

/// `_generateElementShape`: what rough draws for an element, in its own
/// coordinates (its top left corner at 0,0, before rotation).
pub fn element_shape(e: &Element, cfg: &RenderConfig) -> ElementShape {
    let g = Generator;
    let dark = cfg.dark;
    match e.kind() {
        ElementType::Rectangle | ElementType::Iframe | ElementType::Embeddable => {
            let shown = iframe_like(e, cfg.exporting);
            let (w, h) = (e.width(), e.height());
            let shape = if e.roundness().is_some() {
                let r = corner_radius(js::min(w, h), e);
                g.path(
                    &rounded_rectangle_path(w, h, r),
                    &rough_options(&shown, true, dark),
                )
            } else {
                Ok(g.rectangle(0.0, 0.0, w, h, &rough_options(&shown, false, dark)))
            };
            ElementShape::Rough(shape.into_iter().collect())
        }
        ElementType::Diamond => {
            let p = diamond_points(e);
            let shape = if e.roundness().is_some() {
                let [[top_x, top_y], [_, right_y], _, [left_x, _]] = p;
                let vr = corner_radius((top_x - left_x).abs(), e);
                let hr = corner_radius((right_y - top_y).abs(), e);
                g.path(
                    &rounded_diamond_path(p, vr, hr),
                    &rough_options(e, true, dark),
                )
                .ok()
            } else {
                Some(g.polygon(&p, &rough_options(e, false, dark)))
            };
            ElementShape::Rough(shape.into_iter().collect())
        }
        ElementType::Ellipse => ElementShape::Rough(vec![g.ellipse(
            e.width() / 2.0,
            e.height() / 2.0,
            e.width(),
            e.height(),
            &rough_options(e, false, dark),
        )]),
        ElementType::Line | ElementType::Arrow => {
            let options = rough_options(e, false, dark);
            let mut points = e.points();
            if points.is_empty() {
                points.push([0.0, 0.0]);
            }
            let shaft = if e.is_elbowed() {
                // Extreme points: not drawn (as Excalidraw).
                if !points
                    .iter()
                    .all(|p| p[0].abs() <= 1e6 && p[1].abs() <= 1e6)
                {
                    return ElementShape::Rough(Vec::new());
                }
                match g.path(
                    &elbow_arrow_path(&points, ELBOW_ARROW_CORNER_RADIUS),
                    &rough_options(e, true, dark),
                ) {
                    Ok(d) => d,
                    Err(_) => return ElementShape::Rough(Vec::new()),
                }
            } else if e.roundness().is_none() {
                if options.fill.is_some() {
                    g.polygon(&points, &options)
                } else {
                    g.linear_path(&points, &options)
                }
            } else {
                g.curve(&points, &options)
            };
            let mut shapes = vec![shaft];
            if e.kind() == ElementType::Arrow {
                for (end, head) in [
                    (End::Start, e.start_arrowhead()),
                    (End::End, e.end_arrowhead()),
                ] {
                    if let Some(head) = head {
                        let heads =
                            arrowhead_shapes(e, &points, &shapes[0], end, head, &options, cfg);
                        shapes.extend(heads);
                    }
                }
            }
            ElementShape::Rough(shapes)
        }
        ElementType::Freedraw => {
            let points = e.points();
            // A loop is filled along its smoothed centre line.
            let fill = is_path_a_loop(&points).then(|| {
                let o = Options {
                    stroke: "none".into(),
                    ..rough_options(e, false, dark)
                };
                Box::new(g.curve(&crate::rough::points_on_curve::simplify(&points, 0.75), &o))
            });
            ElementShape::Freedraw {
                fill,
                outline: freehand::get_free_draw_svg_path(&freedraw(e, &points, &e.pressures())),
            }
        }
        _ => ElementShape::None,
    }
}

/// A pen stroke's element as the stroke code takes it.
pub fn freedraw<'a>(e: &Element, points: &'a [Point], pressures: &'a [f64]) -> Freedraw<'a> {
    let options = e.get("strokeOptions");
    Freedraw {
        points,
        pressures,
        simulate_pressure: e.simulate_pressure(),
        stroke_width: e.stroke_width(),
        variability: match options
            .and_then(|o| o.get("variability"))
            .and_then(|v| v.as_str())
        {
            Some("constant") => Variability::Constant,
            _ => Variability::Variable,
        },
        streamline: options
            .and_then(|o| o.get("streamline"))
            .and_then(|v| v.as_f64()),
    }
}
