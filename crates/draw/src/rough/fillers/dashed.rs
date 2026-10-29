//! The dashed fill: hachure lines cut into dashes. Ported from rough.js
//! 4.6.4 `fillers/dashed-filler.ts` (MIT, see `crates/draw/PORTED.md`).

use super::hachure::polygon_hachure_lines;
use super::{gap_or_default, line_length};
use crate::js;
use crate::rough::renderer::double_line_fill_ops;
use crate::rough::{Op, OpSet, OpSetType, Options, Point};

/// Fills `polygons` with dashes of `dash_offset` length and `dash_gap`
/// apart along the hachure lines, centred on each (`DashedFiller`).
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    let lines = polygon_hachure_lines(polygons, o);
    OpSet::new(OpSetType::FillSketch, dashed_line(&lines, o))
}

fn dashed_line(lines: &[[Point; 2]], o: &mut Options) -> Vec<Op> {
    let offset = if o.dash_offset < 0.0 {
        gap_or_default(o)
    } else {
        o.dash_offset
    };
    let gap = if o.dash_gap < 0.0 {
        gap_or_default(o)
    } else {
        o.dash_gap
    };
    let mut ops = Vec::new();
    for line in lines {
        let length = line_length(line);
        let count = (length / (offset + gap)).floor();
        if !count.is_finite() {
            // A dash and gap of 0: rough would loop forever.
            continue;
        }
        let start_offset = (length + gap - (count * (offset + gap))) / 2.0;
        let (mut p1, mut p2) = (line[0], line[1]);
        if p1[0] > p2[0] {
            (p1, p2) = (line[1], line[0]);
        }
        let alpha = ((p2[1] - p1[1]) / (p2[0] - p1[0])).atan();
        let mut i = 0.0;
        while i < count {
            let lstart = i * (offset + gap);
            let lend = lstart + offset;
            let start = [
                p1[0] + (lstart * js::cos(alpha)) + (start_offset * js::cos(alpha)),
                p1[1] + lstart * js::sin(alpha) + (start_offset * js::sin(alpha)),
            ];
            let end = [
                p1[0] + (lend * js::cos(alpha)) + (start_offset * js::cos(alpha)),
                p1[1] + (lend * js::sin(alpha)) + (start_offset * js::sin(alpha)),
            ];
            ops.extend(double_line_fill_ops(start[0], start[1], end[0], end[1], o));
            i += 1.0;
        }
    }
    ops
}
