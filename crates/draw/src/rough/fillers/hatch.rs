//! The cross-hatch fill: hachure, then hachure at a right angle to it.
//! Ported from rough.js 4.6.4 `fillers/hatch-filler.ts` (MIT, see
//! `crates/draw/PORTED.md`).

use super::hachure;
use crate::rough::renderer::with_copy;
use crate::rough::{OpSet, Options, Point};

/// Fills `polygons` with two hachures, the second turned by 90°
/// (`HatchFiller.fillPolygons`). The second pass sees the polygons as the
/// first left them (rotated there and back).
pub fn fill_polygons(polygons: &mut [Vec<Point>], o: &mut Options) -> OpSet {
    let mut set = hachure::fill_polygons(polygons, o);
    let angle = o.hachure_angle + 90.0;
    let set2 = with_copy(
        o,
        |o2| o2.hachure_angle = angle,
        |o2| hachure::fill_polygons(polygons, o2),
    );
    set.ops.extend(set2.ops);
    set
}
