//! The dots fill: small sketchy circles along hachure lines. Ported from
//! rough.js 4.6.4 `fillers/dot-filler.ts` (MIT, see
//! `crates/draw/PORTED.md`).

use super::hachure::polygon_hachure_lines;
use super::line_length;
use crate::js;
use crate::rough::renderer::{ellipse, math_random, with_copy};
use crate::rough::{OpSet, OpSetType, Options, Point};

/// Fills `polygons` with dots `hachure_gap` apart, each placed at random
/// near its spot (`DotFiller.fillPolygons`). rough places them with
/// `Math.random`, so they differ on every run there; here they come from
/// the options' unseeded sequence.
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    with_copy(
        o,
        |o| o.hachure_angle = 0.0,
        |o| {
            let lines = polygon_hachure_lines(polygons, o);
            dots_on_lines(&lines, o)
        },
    )
}

fn dots_on_lines(lines: &[[Point; 2]], o: &mut Options) -> OpSet {
    let mut ops = Vec::new();
    let mut gap = o.hachure_gap;
    if gap < 0.0 {
        gap = o.stroke_width * 4.0;
    }
    gap = js::max(gap, 0.1);
    let mut fweight = o.fill_weight;
    if fweight < 0.0 {
        fweight = o.stroke_width / 2.0;
    }
    let ro = gap / 4.0;
    for line in lines {
        let length = line_length(line);
        let dl = length / gap;
        let count = dl.ceil() - 1.0;
        let offset = length - (count * gap);
        let x = ((line[0][0] + line[1][0]) / 2.0) - (gap / 4.0);
        let min_y = js::min(line[0][1], line[1][1]);
        let mut i = 0.0;
        while i < count {
            let y = min_y + offset + (i * gap);
            let cx = (x - ro) + math_random(o) * 2.0 * ro;
            let cy = (y - ro) + math_random(o) * 2.0 * ro;
            let el = ellipse(cx, cy, fweight, fweight, o);
            ops.extend(el.ops);
            i += 1.0;
        }
    }
    OpSet::new(OpSetType::FillSketch, ops)
}
