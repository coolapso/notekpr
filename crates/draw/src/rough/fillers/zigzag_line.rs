//! The zigzag-line fill: each hachure line drawn as a zigzag. Ported from
//! rough.js 4.6.4 `fillers/zigzag-line-filler.ts` (MIT, see
//! `crates/draw/PORTED.md`).

use std::f64::consts::PI;

use super::hachure::polygon_hachure_lines;
use super::{gap_or_default, line_length};
use crate::js;
use crate::rough::renderer::{double_line_fill_ops, with_copy};
use crate::rough::{Op, OpSet, OpSetType, Options, Point};

/// Fills `polygons` with zigzags of `zigzag_offset` along hachure lines
/// (`ZigZagLineFiller.fillPolygons`).
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    let gap = gap_or_default(o);
    let zo = if o.zigzag_offset < 0.0 {
        gap
    } else {
        o.zigzag_offset
    };
    with_copy(
        o,
        |o| o.hachure_gap = gap + zo,
        |o| {
            let lines = polygon_hachure_lines(polygons, o);
            OpSet::new(OpSetType::FillSketch, zigzag_lines(&lines, zo, o))
        },
    )
}

fn zigzag_lines(lines: &[[Point; 2]], zo: f64, o: &mut Options) -> Vec<Op> {
    let mut ops = Vec::new();
    for line in lines {
        let length = line_length(line);
        let count = js::round(length / (2.0 * zo));
        if !count.is_finite() {
            // A zigzag offset of 0: rough would loop forever.
            continue;
        }
        let (mut p1, mut p2) = (line[0], line[1]);
        if p1[0] > p2[0] {
            (p1, p2) = (line[1], line[0]);
        }
        let alpha = ((p2[1] - p1[1]) / (p2[0] - p1[0])).atan();
        let mut i = 0.0;
        while i < count {
            let lstart = i * 2.0 * zo;
            let lend = (i + 1.0) * 2.0 * zo;
            let dz = (2.0 * (zo * zo)).sqrt();
            let start = [
                p1[0] + (lstart * js::cos(alpha)),
                p1[1] + lstart * js::sin(alpha),
            ];
            let end = [
                p1[0] + (lend * js::cos(alpha)),
                p1[1] + (lend * js::sin(alpha)),
            ];
            let middle = [
                start[0] + dz * js::cos(alpha + PI / 4.0),
                start[1] + dz * js::sin(alpha + PI / 4.0),
            ];
            let up = double_line_fill_ops(start[0], start[1], middle[0], middle[1], o);
            let down = double_line_fill_ops(middle[0], middle[1], end[0], end[1], o);
            ops.extend(up);
            ops.extend(down);
            i += 1.0;
        }
    }
    ops
}
