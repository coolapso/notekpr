//! The hand-drawn look: a port of rough.js 4.6.4 (MIT, see
//! `crates/draw/PORTED.md`), the version Excalidraw uses, with the small
//! libraries it is built on.
//!
//! The point of the port is that the same seed and options give the same
//! numbers as rough.js, so a drawing made in Excalidraw looks the same here.
//! The code therefore follows rough's closely — same steps, same order of
//! floating-point operations, same use of the random sequence — and prints
//! numbers the way JavaScript does ([`crate::js`]).
//!
//! The pieces, as in rough:
//!
//! * [`Generator`] turns shapes into a [`Drawable`]: sets of drawing
//!   operations ([`OpSet`], [`Op`]) for the outline and the fill;
//! * [`ops_to_path`] / [`to_paths`] turn those into SVG path data, and
//!   [`svg::draw`] into `<path>` elements;
//! * [`renderer`] draws the sketchy lines, curves and ellipses, and
//!   [`fillers`] the fills (hachure, cross-hatch, zigzag, dots, …);
//! * [`hachure_fill`], [`points_on_curve`], [`points_on_path`] and
//!   [`path_data`] are ports of hachure-fill 0.5.2, points-on-curve
//!   0.2.0/1.0.1, points-on-path 0.2.1 and path-data-parser 0.1.0.
//!
//! Where rough.js would throw, loop forever or recurse until the stack runs
//! out (malformed path data, non-finite coordinates, a negative step), the
//! port returns an error or stops early instead; those spots say so.

mod core;
pub mod fillers;
pub mod generator;
pub mod hachure_fill;
mod math;
pub mod path_data;
pub mod points_on_curve;
pub mod points_on_path;
pub mod renderer;
pub mod svg;

pub use self::core::{Drawable, FillStyle, Op, OpSet, OpSetType, OpType, Options, PathInfo, Shape};
pub use generator::{Generator, ops_to_path, to_paths};
pub use math::Random;
pub use path_data::PathError;

/// A point: `[x, y]`.
pub type Point = [f64; 2];
