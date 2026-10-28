//! rough's types: options, drawing operations and drawables. Ported from
//! rough.js 4.6.4 `core.ts` (MIT, see `crates/draw/PORTED.md`).

use super::math::Random;

/// How a closed shape is filled (`fillStyle`). rough treats a name it
/// doesn't know as hachure, and so does [`FillStyle::from_name`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillStyle {
    /// Parallel sketchy lines at `hachure_angle`.
    #[default]
    Hachure,
    /// One filled path.
    Solid,
    /// Hachure lines joined into a zigzag.
    Zigzag,
    /// Hachure lines in two directions.
    CrossHatch,
    /// Small sketchy circles along hachure lines.
    Dots,
    /// Dashed hachure lines.
    Dashed,
    /// Each hachure line drawn as a zigzag.
    ZigzagLine,
}

impl FillStyle {
    /// The style rough uses for `name`: hachure for an unknown one.
    pub fn from_name(name: &str) -> FillStyle {
        match name {
            "solid" => FillStyle::Solid,
            "zigzag" => FillStyle::Zigzag,
            "cross-hatch" => FillStyle::CrossHatch,
            "dots" => FillStyle::Dots,
            "dashed" => FillStyle::Dashed,
            "zigzag-line" => FillStyle::ZigzagLine,
            _ => FillStyle::Hachure,
        }
    }

    /// rough's name for the style.
    pub fn name(self) -> &'static str {
        match self {
            FillStyle::Hachure => "hachure",
            FillStyle::Solid => "solid",
            FillStyle::Zigzag => "zigzag",
            FillStyle::CrossHatch => "cross-hatch",
            FillStyle::Dots => "dots",
            FillStyle::Dashed => "dashed",
            FillStyle::ZigzagLine => "zigzag-line",
        }
    }
}

/// Drawing options, with rough's defaults (`RoughGenerator`'s
/// `defaultOptions`) as [`Default`].
///
/// rough merges the options of a call over the generator's defaults; here
/// they are always complete: start from `Options::default()` and change what
/// differs. Optional JavaScript values (`fill`, `simplification`, …) are
/// [`Option`]s; where rough tests a value for truth (`if (o.fill)`), an
/// empty string or 0 counts as unset, as in JavaScript.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub max_randomness_offset: f64,
    pub roughness: f64,
    pub bowing: f64,
    pub stroke: String,
    pub stroke_width: f64,
    pub curve_fitting: f64,
    pub curve_tightness: f64,
    pub curve_step_count: f64,
    pub fill: Option<String>,
    pub fill_style: FillStyle,
    pub fill_weight: f64,
    pub hachure_angle: f64,
    pub hachure_gap: f64,
    pub simplification: Option<f64>,
    pub dash_offset: f64,
    pub dash_gap: f64,
    pub zigzag_offset: f64,
    /// The random sequence's seed; 0 means "not seeded" (rough then uses
    /// `Math.random`, here [`Random`]'s own deterministic sequence). A
    /// JavaScript number becomes one with [`crate::js::to_int32`], as
    /// `Math.imul` converts it.
    pub seed: i32,
    pub stroke_line_dash: Option<Vec<f64>>,
    pub stroke_line_dash_offset: Option<f64>,
    pub fill_line_dash: Option<Vec<f64>>,
    pub fill_line_dash_offset: Option<f64>,
    pub disable_multi_stroke: bool,
    pub disable_multi_stroke_fill: bool,
    pub preserve_vertices: bool,
    /// Digits after the point in SVG path data ([`super::svg::draw`]); `None`
    /// prints numbers in full.
    pub fixed_decimal_place_digits: Option<u32>,
    pub fill_shape_roughness_gain: f64,
    /// The random sequence while drawing. rough keeps it on the options
    /// object and starts it from `seed` on first use; a copy of the options
    /// shares it, except where rough deliberately starts a new one (the
    /// second stroke of a curve). `None` until the first number is drawn.
    pub randomizer: Option<Random>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_randomness_offset: 2.0,
            roughness: 1.0,
            bowing: 1.0,
            stroke: "#000".into(),
            stroke_width: 1.0,
            curve_fitting: 0.95,
            curve_tightness: 0.0,
            curve_step_count: 9.0,
            fill: None,
            fill_style: FillStyle::Hachure,
            fill_weight: -1.0,
            hachure_angle: -41.0,
            hachure_gap: -1.0,
            simplification: None,
            dash_offset: -1.0,
            dash_gap: -1.0,
            zigzag_offset: -1.0,
            seed: 0,
            stroke_line_dash: None,
            stroke_line_dash_offset: None,
            fill_line_dash: None,
            fill_line_dash_offset: None,
            disable_multi_stroke: false,
            disable_multi_stroke_fill: false,
            preserve_vertices: false,
            fixed_decimal_place_digits: None,
            fill_shape_roughness_gain: 0.8,
            randomizer: None,
        }
    }
}

impl Options {
    /// `o.fill` as a condition: set and not empty.
    pub(crate) fn has_fill(&self) -> bool {
        self.fill.as_deref().is_some_and(|f| !f.is_empty())
    }

    /// `o.fill || fallback`.
    pub(crate) fn fill_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self.fill.as_deref() {
            Some(f) if !f.is_empty() => f,
            _ => fallback,
        }
    }
}

/// What a drawing operation does (`OpType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpType {
    /// Move to `data[0..2]`.
    Move,
    /// A cubic Bézier curve: control points `data[0..4]`, end `data[4..6]`.
    BCurveTo,
    /// A straight line to `data[0..2]`.
    LineTo,
}

/// One drawing operation (`Op`).
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub op: OpType,
    pub data: Vec<f64>,
}

impl Op {
    pub(crate) fn new(op: OpType, data: Vec<f64>) -> Op {
        Op { op, data }
    }
}

/// What a set of operations is for (`OpSetType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpSetType {
    /// An outline, stroked.
    Path,
    /// A filled area.
    FillPath,
    /// Fill lines, stroked in the fill colour.
    FillSketch,
}

/// A set of operations drawn together (`OpSet`). rough's type also has
/// `size` and `path`, which 4.6.4 never sets.
#[derive(Clone, Debug, PartialEq)]
pub struct OpSet {
    pub kind: OpSetType,
    pub ops: Vec<Op>,
}

impl OpSet {
    pub(crate) fn new(kind: OpSetType, ops: Vec<Op>) -> OpSet {
        OpSet { kind, ops }
    }
}

/// Which generator call made a drawable (`Drawable.shape`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Line,
    Rectangle,
    Ellipse,
    Circle,
    LinearPath,
    Arc,
    Curve,
    Polygon,
    Path,
}

impl Shape {
    /// rough's name for the shape.
    pub fn name(self) -> &'static str {
        match self {
            Shape::Line => "line",
            Shape::Rectangle => "rectangle",
            Shape::Ellipse => "ellipse",
            Shape::Circle => "circle",
            Shape::LinearPath => "linearPath",
            Shape::Arc => "arc",
            Shape::Curve => "curve",
            Shape::Polygon => "polygon",
            Shape::Path => "path",
        }
    }
}

/// A generated shape: its operation sets (fill first, then outline) and the
/// options it was made with (`Drawable`).
#[derive(Clone, Debug, PartialEq)]
pub struct Drawable {
    pub shape: Shape,
    pub options: Options,
    pub sets: Vec<OpSet>,
}

/// One SVG path of a drawable, as `generator.toPaths` gives it (`PathInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct PathInfo {
    pub d: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub fill: String,
}
