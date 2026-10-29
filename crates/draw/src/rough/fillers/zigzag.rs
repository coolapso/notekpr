//! The zigzag fill: hachure lines joined end to end. Ported from rough.js
//! 4.6.4 `fillers/zigzag-filler.ts` (MIT, see `crates/draw/PORTED.md`).

use std::f64::consts::PI;

use super::hachure::{polygon_hachure_lines, render_lines};
use super::line_length;
use crate::js;
use crate::rough::renderer::with_copy;
use crate::rough::{OpSet, OpSetType, Options, Point};

/// Fills `polygons` with a zigzag: two lines per hachure line, from either
/// side of its start to its end (`ZigZagFiller.fillPolygons`).
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    let mut gap = o.hachure_gap;
    if gap < 0.0 {
        gap = o.stroke_width * 4.0;
    }
    gap = js::max(gap, 0.1);
    let lines = with_copy(
        o,
        |o2| o2.hachure_gap = gap,
        |o2| polygon_hachure_lines(polygons, o2),
    );
    let zig_zag_angle = (PI / 180.0) * o.hachure_angle;
    let mut zigzag_lines = Vec::new();
    let dgx = gap * 0.5 * js::cos(zig_zag_angle);
    let dgy = gap * 0.5 * js::sin(zig_zag_angle);
    for [p1, p2] in &lines {
        if js::truthy(line_length(&[*p1, *p2])) {
            zigzag_lines.push([[p1[0] - dgx, p1[1] + dgy], *p2]);
            zigzag_lines.push([[p1[0] + dgx, p1[1] - dgy], *p2]);
        }
    }
    let ops = render_lines(&zigzag_lines, o);
    OpSet::new(OpSetType::FillSketch, ops)
}
