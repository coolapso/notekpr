//! The hachure fill: parallel sketchy lines. Ported from rough.js 4.6.4
//! `fillers/hachure-filler.ts` and `fillers/scan-line-hachure.ts` (MIT, see
//! `crates/draw/PORTED.md`).

use crate::js;
use crate::rough::hachure_fill::hachure_lines;
use crate::rough::renderer::{double_line_fill_ops, math_random};
use crate::rough::{Op, OpSet, OpSetType, Options, Point};

/// Fills `polygons` with sketchy lines at `hachure_angle`, `hachure_gap`
/// apart (`HachureFiller.fillPolygons`).
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    let lines = polygon_hachure_lines(polygons, o);
    let ops = render_lines(&lines, o);
    OpSet::new(OpSetType::FillSketch, ops)
}

/// The straight hachure lines for `polygons` (`polygonHachureLines`). With
/// roughness 1 or more, three times in ten the lines are drawn with a scan
/// step of the gap instead of 1, which shifts them a little.
pub fn polygon_hachure_lines(polygons: &mut [Vec<Point>], o: &mut Options) -> Vec<[Point; 2]> {
    let angle = o.hachure_angle + 90.0;
    let mut gap = o.hachure_gap;
    if gap < 0.0 {
        gap = o.stroke_width * 4.0;
    }
    gap = js::max(gap, 0.1);
    let mut skip_offset = 1.0;
    if o.roughness >= 1.0 {
        // `o.randomizer?.next() || Math.random()`: the sequence only if it
        // has started, and Math.random for a 0 from it too.
        let r = match o.randomizer.as_mut().map(|r| r.next_f64()) {
            Some(x) if js::truthy(x) => x,
            _ => math_random(o),
        };
        if r > 0.7 {
            skip_offset = gap;
        }
    }
    hachure_lines(polygons, gap, angle, js::or(skip_offset, 1.0))
}

/// Each line as a sketchy fill line (`renderLines`).
pub(crate) fn render_lines(lines: &[[Point; 2]], o: &mut Options) -> Vec<Op> {
    let mut ops = Vec::new();
    for [p1, p2] in lines {
        ops.extend(double_line_fill_ops(p1[0], p1[1], p2[0], p2[1], o));
    }
    ops
}
