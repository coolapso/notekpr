//! Pattern fills: hachure lines and the styles built on them. Ported from
//! rough.js 4.6.4 `fillers/` and `geometry.ts` (MIT, see
//! `crates/draw/PORTED.md`).
//!
//! Each filler takes the polygons to fill and returns a
//! [`OpSetType::FillSketch`](super::OpSetType::FillSketch) set of sketchy
//! lines. Like rough's, they rotate the polygons' points in place and back
//! (see [`super::hachure_fill::hachure_lines`]).

pub mod dashed;
pub mod dots;
pub mod hachure;
pub mod hatch;
pub mod zigzag;
pub mod zigzag_line;

use super::{FillStyle, OpSet, Options, Point};

/// Fills `polygons` in `o.fill_style` (`getFiller(o).fillPolygons`);
/// [`FillStyle::Solid`] isn't a pattern and fills with hachure here, as in
/// rough.
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    match o.fill_style {
        FillStyle::Hachure | FillStyle::Solid => hachure::fill_polygons(polygons, o),
        FillStyle::CrossHatch => hatch::fill_polygons(polygons, o),
        FillStyle::Zigzag => zigzag::fill_polygons(polygons, o),
        FillStyle::Dots => dots::fill_polygons(polygons, o),
        FillStyle::Dashed => dashed::fill_polygons(polygons, o),
        FillStyle::ZigzagLine => zigzag_line::fill_polygons(polygons, o),
    }
}

/// The length of a line (`lineLength`).
pub(crate) fn line_length(line: &[Point; 2]) -> f64 {
    let [p1, p2] = line;
    // `Math.pow(d, 2)` is exactly `d * d`.
    let dx = p1[0] - p2[0];
    let dy = p1[1] - p2[1];
    (dx * dx + dy * dy).sqrt()
}

/// The gap between fill lines: `hachure_gap`, or 4 × the stroke width when
/// that is negative (the default).
fn gap_or_default(o: &Options) -> f64 {
    if o.hachure_gap < 0.0 {
        o.stroke_width * 4.0
    } else {
        o.hachure_gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rough::{OpSetType, OpType};

    fn square() -> Vec<Point> {
        vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]
    }

    fn options(style: FillStyle) -> Options {
        Options {
            seed: 12345,
            fill_style: style,
            fill: Some("red".into()),
            ..Options::default()
        }
    }

    #[test]
    fn every_style_draws_lines_inside() {
        for style in [
            FillStyle::Hachure,
            FillStyle::Solid,
            FillStyle::CrossHatch,
            FillStyle::Zigzag,
            FillStyle::Dots,
            FillStyle::Dashed,
            FillStyle::ZigzagLine,
        ] {
            let mut polygons = vec![square()];
            let set = fill_polygons(&mut polygons, &mut options(style));
            assert_eq!(set.kind, OpSetType::FillSketch, "{style:?}");
            assert!(set.ops.len() > 10, "{style:?}: {}", set.ops.len());
            assert_eq!(set.ops[0].op, OpType::Move, "{style:?}");
            for op in &set.ops {
                for xy in op.data.chunks(2) {
                    // Sketchy lines stray a little past the edges.
                    assert!(
                        (-12.0..=112.0).contains(&xy[0]) && (-12.0..=112.0).contains(&xy[1]),
                        "{style:?}: {xy:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn same_seed_same_fill() {
        for style in [
            FillStyle::Hachure,
            FillStyle::CrossHatch,
            FillStyle::Zigzag,
            FillStyle::Dashed,
        ] {
            let a = fill_polygons(&mut [square()], &mut options(style));
            let b = fill_polygons(&mut [square()], &mut options(style));
            assert_eq!(a, b, "{style:?}");
        }
    }

    #[test]
    fn cross_hatch_is_two_hachures() {
        let one = fill_polygons(&mut [square()], &mut options(FillStyle::Hachure));
        let two = fill_polygons(&mut [square()], &mut options(FillStyle::CrossHatch));
        // The first pass is the hachure fill itself.
        assert_eq!(two.ops[..one.ops.len()], one.ops[..]);
        assert!(two.ops.len() > one.ops.len() * 3 / 2);
    }

    #[test]
    fn line_length_is_euclidean() {
        assert_eq!(line_length(&[[0.0, 0.0], [3.0, 4.0]]), 5.0);
    }
}
