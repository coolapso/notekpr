//! Shapes to drawables, and drawables to SVG path data. Ported from
//! rough.js 4.6.4 `generator.ts` (MIT, see `crates/draw/PORTED.md`).

use super::path_data::{self, PathError};
use super::points_on_curve::{curve_to_bezier, points_on_bezier_curves};
use super::points_on_path::points_on_segments;
use super::renderer::{self, with_copy};
use super::{Drawable, FillStyle, Op, OpSet, OpSetType, OpType, Options, PathInfo, Point, Shape};
use crate::js;

const NOS: &str = "none";

/// Makes [`Drawable`]s of shapes (`RoughGenerator`).
///
/// Each call starts from a copy of the options it is given, so its random
/// sequence starts from their seed (unless they carry one already started,
/// [`Options::randomizer`]). rough also lets a call leave the options out
/// and then draws with the generator's own, whose sequence carries on from
/// call to call; here a call always takes options.
#[derive(Clone, Copy, Debug, Default)]
pub struct Generator;

impl Generator {
    /// A straight line.
    pub fn line(&self, x1: f64, y1: f64, x2: f64, y2: f64, options: &Options) -> Drawable {
        let mut o = options.clone();
        let set = renderer::line(x1, y1, x2, y2, &mut o);
        drawable(Shape::Line, vec![set], o)
    }

    /// A rectangle with its top left corner at `(x, y)`.
    pub fn rectangle(
        &self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        options: &Options,
    ) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let outline = renderer::rectangle(x, y, width, height, &mut o);
        if o.has_fill() {
            let points = vec![
                [x, y],
                [x + width, y],
                [x + width, y + height],
                [x, y + height],
            ];
            if o.fill_style == FillStyle::Solid {
                paths.push(renderer::solid_fill_polygon(&[points], &mut o));
            } else {
                paths.push(renderer::pattern_fill_polygons(&mut [points], &mut o));
            }
        }
        if o.stroke != NOS {
            paths.push(outline);
        }
        drawable(Shape::Rectangle, paths, o)
    }

    /// An ellipse centred on `(x, y)`.
    pub fn ellipse(&self, x: f64, y: f64, width: f64, height: f64, options: &Options) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let params = renderer::generate_ellipse_params(width, height, &mut o);
        let response = renderer::ellipse_with_params(x, y, &mut o, &params);
        if o.has_fill() {
            if o.fill_style == FillStyle::Solid {
                // A second ellipse, with the following random numbers.
                let mut shape = renderer::ellipse_with_params(x, y, &mut o, &params).opset;
                shape.kind = OpSetType::FillPath;
                paths.push(shape);
            } else {
                paths.push(renderer::pattern_fill_polygons(
                    &mut [response.estimated_points],
                    &mut o,
                ));
            }
        }
        if o.stroke != NOS {
            paths.push(response.opset);
        }
        drawable(Shape::Ellipse, paths, o)
    }

    /// A circle centred on `(x, y)`.
    pub fn circle(&self, x: f64, y: f64, diameter: f64, options: &Options) -> Drawable {
        let mut ret = self.ellipse(x, y, diameter, diameter, options);
        ret.shape = Shape::Circle;
        ret
    }

    /// Straight lines through `points`.
    pub fn linear_path(&self, points: &[Point], options: &Options) -> Drawable {
        let mut o = options.clone();
        let set = renderer::linear_path(points, false, &mut o);
        drawable(Shape::LinearPath, vec![set], o)
    }

    /// Part of an ellipse centred on `(x, y)` from angle `start` to `stop`
    /// (radians, clockwise from the x axis); `closed` makes a wedge.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(
        &self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        start: f64,
        stop: f64,
        closed: bool,
        options: &Options,
    ) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let outline = renderer::arc(x, y, width, height, start, stop, closed, true, &mut o);
        if closed && o.has_fill() {
            if o.fill_style == FillStyle::Solid {
                let mut shape = with_copy(
                    &mut o,
                    |f| f.disable_multi_stroke = true,
                    |f| renderer::arc(x, y, width, height, start, stop, true, false, f),
                );
                shape.kind = OpSetType::FillPath;
                paths.push(shape);
            } else {
                paths.push(renderer::pattern_fill_arc(
                    x, y, width, height, start, stop, &mut o,
                ));
            }
        }
        if o.stroke != NOS {
            paths.push(outline);
        }
        drawable(Shape::Arc, paths, o)
    }

    /// A smooth curve through `points` (rough.js 4.6.4: one curve; see
    /// [`Generator::curves`] for several).
    pub fn curve(&self, points: &[Point], options: &Options) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let outline = renderer::curve(points, &mut o);
        if o.has_fill() && o.fill.as_deref() != Some(NOS) && points.len() >= 3 {
            if o.fill_style == FillStyle::Solid {
                let fill_shape =
                    with_copy(&mut o, solid_fill_changes, |f| renderer::curve(points, f));
                paths.push(OpSet::new(
                    OpSetType::FillPath,
                    merged_shape(fill_shape.ops),
                ));
            } else {
                let bcurve = curve_to_bezier(points, 0.0);
                let poly_points =
                    points_on_bezier_curves(&bcurve, 10.0, Some((1.0 + o.roughness) / 2.0));
                paths.push(renderer::pattern_fill_polygons(&mut [poly_points], &mut o));
            }
        }
        if o.stroke != NOS {
            paths.push(outline);
        }
        drawable(Shape::Curve, paths, o)
    }

    /// Several curves drawn as one shape, each continuing the last
    /// (rough's `curve` with `Point[][]`). rough.js 4.6.4 doesn't take
    /// several; this follows 4.6.6, the first version that does.
    pub fn curves(&self, curves: &[Vec<Point>], options: &Options) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let outline = renderer::curves(curves, &mut o);
        if o.has_fill() && o.fill.as_deref() != Some(NOS) {
            if o.fill_style == FillStyle::Solid {
                let fill_shape =
                    with_copy(&mut o, solid_fill_changes, |f| renderer::curves(curves, f));
                paths.push(OpSet::new(
                    OpSetType::FillPath,
                    merged_shape(fill_shape.ops),
                ));
            } else {
                let mut poly_points = Vec::new();
                let distance = Some((1.0 + o.roughness) / 2.0);
                for points in curves {
                    match points.len() {
                        0..3 => poly_points.extend_from_slice(points),
                        3 => {
                            let b =
                                curve_to_bezier(&[points[0], points[0], points[1], points[2]], 0.0);
                            poly_points.extend(points_on_bezier_curves(&b, 10.0, distance));
                        }
                        _ => {
                            let b = curve_to_bezier(points, 0.0);
                            poly_points.extend(points_on_bezier_curves(&b, 10.0, distance));
                        }
                    }
                }
                if !poly_points.is_empty() {
                    paths.push(renderer::pattern_fill_polygons(&mut [poly_points], &mut o));
                }
            }
        }
        if o.stroke != NOS {
            paths.push(outline);
        }
        drawable(Shape::Curve, paths, o)
    }

    /// A closed shape with straight sides through `points`.
    pub fn polygon(&self, points: &[Point], options: &Options) -> Drawable {
        let mut o = options.clone();
        let mut paths = Vec::new();
        let outline = renderer::linear_path(points, true, &mut o);
        if o.has_fill() {
            if o.fill_style == FillStyle::Solid {
                paths.push(renderer::solid_fill_polygon(&[points.to_vec()], &mut o));
            } else {
                paths.push(renderer::pattern_fill_polygons(
                    &mut [points.to_vec()],
                    &mut o,
                ));
            }
        }
        if o.stroke != NOS {
            paths.push(outline);
        }
        drawable(Shape::Polygon, paths, o)
    }

    /// The shape of SVG path data `d`; an error where rough would throw on
    /// data it can't read.
    pub fn path(&self, d: &str, options: &Options) -> Result<Drawable, PathError> {
        let mut o = options.clone();
        let mut paths = Vec::new();
        if d.is_empty() {
            return Ok(drawable(Shape::Path, paths, o));
        }
        let d = clean_path_data(d);
        let has_fill = o.has_fill() && !matches!(o.fill.as_deref(), Some("transparent" | NOS));
        let has_stroke = o.stroke != NOS;
        let simplification = o.simplification.filter(|s| js::truthy(*s));
        let simplified = simplification.is_some_and(|s| s < 1.0);
        let distance = if simplified {
            4.0 - 4.0 * simplification.unwrap_or(1.0)
        } else {
            (1.0 + o.roughness) / 2.0
        };
        // rough reads the data twice (points-on-path and its own path
        // drawing), the same way; once is enough.
        let segments = path_data::normalize(&path_data::absolutize(&path_data::parse_path(&d)?));
        let mut sets = points_on_segments(&segments, 1.0, distance);
        let shape = renderer::svg_path_segments(&segments, &mut o);
        if has_fill {
            if o.fill_style == FillStyle::Solid {
                if sets.len() == 1 {
                    let fill_shape = with_copy(&mut o, solid_fill_changes, |f| {
                        renderer::svg_path_segments(&segments, f)
                    });
                    paths.push(OpSet::new(
                        OpSetType::FillPath,
                        merged_shape(fill_shape.ops),
                    ));
                } else {
                    paths.push(renderer::solid_fill_polygon(&sets, &mut o));
                }
            } else {
                paths.push(renderer::pattern_fill_polygons(&mut sets, &mut o));
            }
        }
        if has_stroke {
            if simplified {
                for set in &sets {
                    paths.push(renderer::linear_path(set, false, &mut o));
                }
            } else {
                paths.push(shape);
            }
        }
        Ok(drawable(Shape::Path, paths, o))
    }
}

fn drawable(shape: Shape, sets: Vec<OpSet>, options: Options) -> Drawable {
    Drawable {
        shape,
        options,
        sets,
    }
}

/// What a solid fill drawn as a shape changes in the options: one stroke,
/// a little rougher.
fn solid_fill_changes(o: &mut Options) {
    o.disable_multi_stroke = true;
    o.roughness = if js::truthy(o.roughness) {
        o.roughness + o.fill_shape_roughness_gain
    } else {
        0.0
    };
}

/// rough's clean-up of path data before reading it:
/// `d.replace(/\n/g, ' ').replace(/(-\s)/g, '-').replace('/(\s\s)/g', ' ')`.
/// The last is a string, not a pattern, so it replaces the first literal
/// `/(ss)/g` (a `\s` in a string is just `s`), which real data never has.
fn clean_path_data(d: &str) -> String {
    let d = d.replace('\n', " ");
    let mut out = String::with_capacity(d.len());
    let mut chars = d.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '-' && chars.peek().is_some_and(|&n| js::is_space(n)) {
            chars.next();
        }
    }
    out.replacen("/(ss)/g", " ", 1)
}

/// The operations with every move but the first left out
/// (`_mergedShape`): one area to fill.
fn merged_shape(input: Vec<Op>) -> Vec<Op> {
    input
        .into_iter()
        .enumerate()
        .filter(|(i, op)| *i == 0 || op.op != OpType::Move)
        .map(|(_, op)| op)
        .collect()
}

/// SVG path data for a set of operations (`opsToPath`), each number
/// rounded to `fixed_decimals` digits after the point first if given
/// (`+n.toFixed(digits)`), and printed as JavaScript prints numbers.
pub fn ops_to_path(set: &OpSet, fixed_decimals: Option<u32>) -> String {
    let mut path = String::new();
    for item in &set.ops {
        let data: Vec<String> = item
            .data
            .iter()
            .map(|&d| js::number_to_string(fixed_decimals.map_or(d, |digits| js::fixed(d, digits))))
            .collect();
        match item.op {
            OpType::Move => {
                path.push_str(&format!("M{} {} ", data[0], data[1]));
            }
            OpType::BCurveTo => {
                path.push_str(&format!(
                    "C{} {}, {} {}, {} {} ",
                    data[0], data[1], data[2], data[3], data[4], data[5]
                ));
            }
            OpType::LineTo => {
                path.push_str(&format!("L{} {} ", data[0], data[1]));
            }
        }
    }
    path.trim().to_string()
}

/// The SVG paths of a drawable, with their stroke and fill (`toPaths`).
/// Like rough's, these print numbers in full whatever
/// `fixed_decimal_place_digits` says.
pub fn to_paths(drawable: &Drawable) -> Vec<PathInfo> {
    let o = &drawable.options;
    drawable
        .sets
        .iter()
        .map(|drawing| match drawing.kind {
            OpSetType::Path => PathInfo {
                d: ops_to_path(drawing, None),
                stroke: o.stroke.clone(),
                stroke_width: o.stroke_width,
                fill: NOS.into(),
            },
            OpSetType::FillPath => PathInfo {
                d: ops_to_path(drawing, None),
                stroke: NOS.into(),
                stroke_width: 0.0,
                fill: o.fill_or(NOS).into(),
            },
            OpSetType::FillSketch => fill_sketch(drawing, o),
        })
        .collect()
}

fn fill_sketch(drawing: &OpSet, o: &Options) -> PathInfo {
    let mut fweight = o.fill_weight;
    if fweight < 0.0 {
        fweight = o.stroke_width / 2.0;
    }
    PathInfo {
        d: ops_to_path(drawing, None),
        stroke: o.fill_or(NOS).into(),
        stroke_width: fweight,
        fill: NOS.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The segments of `d` as rough reads them.
    fn read_path(d: &str) -> Result<Vec<path_data::Segment>, PathError> {
        Ok(path_data::normalize(&path_data::absolutize(
            &path_data::parse_path(&clean_path_data(d))?,
        )))
    }

    fn seeded(seed: i32) -> Options {
        Options {
            seed,
            ..Options::default()
        }
    }

    #[test]
    fn ops_print_like_rough() {
        let set = OpSet::new(
            OpSetType::Path,
            vec![
                Op::new(OpType::Move, vec![1.0, -0.5]),
                Op::new(OpType::BCurveTo, vec![1.0, 2.0, 3.0, 4.0, 5.125, 6.0]),
                Op::new(OpType::LineTo, vec![0.1 + 0.2, 7.0]),
            ],
        );
        assert_eq!(
            ops_to_path(&set, None),
            "M1 -0.5 C1 2, 3 4, 5.125 6 L0.30000000000000004 7"
        );
        assert_eq!(
            ops_to_path(&set, Some(2)),
            "M1 -0.5 C1 2, 3 4, 5.13 6 L0.3 7"
        );
        assert_eq!(ops_to_path(&OpSet::new(OpSetType::Path, vec![]), None), "");
    }

    #[test]
    fn shapes_have_their_sets() {
        let g = Generator;
        let filled = Options {
            fill: Some("blue".into()),
            ..seeded(1)
        };
        let solid = Options {
            fill_style: FillStyle::Solid,
            ..filled.clone()
        };
        let kinds = |d: &Drawable| d.sets.iter().map(|s| s.kind).collect::<Vec<_>>();
        use OpSetType::*;
        assert_eq!(
            kinds(&g.rectangle(0.0, 0.0, 50.0, 30.0, &seeded(1))),
            [Path]
        );
        assert_eq!(
            kinds(&g.rectangle(0.0, 0.0, 50.0, 30.0, &filled)),
            [FillSketch, Path]
        );
        assert_eq!(
            kinds(&g.rectangle(0.0, 0.0, 50.0, 30.0, &solid)),
            [FillPath, Path]
        );
        assert_eq!(
            kinds(&g.ellipse(0.0, 0.0, 50.0, 30.0, &filled)),
            [FillSketch, Path]
        );
        assert_eq!(
            kinds(&g.ellipse(0.0, 0.0, 50.0, 30.0, &solid)),
            [FillPath, Path]
        );
        assert_eq!(g.circle(0.0, 0.0, 50.0, &solid).shape, Shape::Circle);
        let tri = [[0.0, 0.0], [40.0, 0.0], [20.0, 30.0]];
        assert_eq!(kinds(&g.polygon(&tri, &filled)), [FillSketch, Path]);
        assert_eq!(kinds(&g.curve(&tri, &filled)), [FillSketch, Path]);
        assert_eq!(kinds(&g.curve(&tri, &solid)), [FillPath, Path]);
        assert_eq!(kinds(&g.curve(&tri[..2], &solid)), [Path]);
        assert_eq!(
            kinds(&g.curves(&[tri.to_vec(), tri.to_vec()], &filled)),
            [FillSketch, Path]
        );
        assert_eq!(
            kinds(&g.arc(0.0, 0.0, 50.0, 50.0, 0.0, 1.0, true, &solid)),
            [FillPath, Path]
        );
        assert_eq!(
            kinds(&g.arc(0.0, 0.0, 50.0, 50.0, 0.0, 1.0, false, &solid)),
            [Path]
        );
        assert_eq!(kinds(&g.line(0.0, 0.0, 5.0, 5.0, &solid)), [Path]);
        assert_eq!(kinds(&g.linear_path(&tri, &solid)), [Path]);
        let no_stroke = Options {
            stroke: "none".into(),
            ..filled.clone()
        };
        assert_eq!(
            kinds(&g.rectangle(0.0, 0.0, 50.0, 30.0, &no_stroke)),
            [FillSketch]
        );
    }

    #[test]
    fn paths_fill_and_stroke() {
        let g = Generator;
        let d = "M0 0 L50 0 L50 50 Z";
        let kinds = |o: &Options| {
            g.path(d, o)
                .unwrap()
                .sets
                .iter()
                .map(|s| s.kind)
                .collect::<Vec<_>>()
        };
        use OpSetType::*;
        assert_eq!(kinds(&seeded(1)), [Path]);
        let filled = Options {
            fill: Some("red".into()),
            ..seeded(1)
        };
        assert_eq!(kinds(&filled), [FillSketch, Path]);
        let transparent = Options {
            fill: Some("transparent".into()),
            ..seeded(1)
        };
        assert_eq!(kinds(&transparent), [Path]);
        let solid = Options {
            fill_style: FillStyle::Solid,
            ..filled.clone()
        };
        assert_eq!(kinds(&solid), [FillPath, Path]);
        // Two subpaths: a solid fill of their points.
        let two = g
            .path("M0 0 L50 0 L50 50 Z M60 60 L70 60 L70 70 Z", &solid)
            .unwrap();
        assert_eq!(two.sets[0].kind, FillPath);
        assert_eq!(
            two.sets[0]
                .ops
                .iter()
                .filter(|op| op.op == OpType::Move)
                .count(),
            2
        );
        // Simplified: the points' polylines are the outline.
        let simple = Options {
            simplification: Some(0.5),
            ..seeded(1)
        };
        assert_eq!(kinds(&simple), [Path]);
        assert!(g.path("", &seeded(1)).unwrap().sets.is_empty());
        assert_eq!(g.path("M0 0 L", &seeded(1)), Err(PathError::EndedShort));
    }

    #[test]
    fn solid_fill_of_a_path_is_one_area() {
        let g = Generator;
        let solid = Options {
            fill: Some("red".into()),
            fill_style: FillStyle::Solid,
            ..seeded(3)
        };
        let drawn = g.path("M0 0 L50 0 L50 50 Z", &solid).unwrap();
        let fill = &drawn.sets[0];
        assert_eq!(fill.ops[0].op, OpType::Move);
        assert!(fill.ops[1..].iter().all(|op| op.op != OpType::Move));
    }

    #[test]
    fn path_data_is_cleaned_like_rough() {
        assert_eq!(clean_path_data("M0 0\nL- 5 -\t6"), "M0 0 L-5 -6");
        assert_eq!(clean_path_data("M-  5"), "M- 5");
        assert_eq!(read_path("M1 2\nL- 3 4").unwrap()[1].data, [-3.0, 4.0]);
    }

    #[test]
    fn to_paths_like_rough() {
        let g = Generator;
        let o = Options {
            fill: Some("red".into()),
            stroke: "blue".into(),
            stroke_width: 3.0,
            ..seeded(1)
        };
        let paths = to_paths(&g.rectangle(0.0, 0.0, 10.0, 10.0, &o));
        assert_eq!(paths.len(), 2);
        assert_eq!(
            (
                paths[0].stroke.as_str(),
                paths[0].stroke_width,
                paths[0].fill.as_str()
            ),
            ("red", 1.5, "none")
        );
        assert_eq!(
            (
                paths[1].stroke.as_str(),
                paths[1].stroke_width,
                paths[1].fill.as_str()
            ),
            ("blue", 3.0, "none")
        );
        let solid = Options {
            fill_style: FillStyle::Solid,
            ..o
        };
        let paths = to_paths(&g.rectangle(0.0, 0.0, 10.0, 10.0, &solid));
        assert_eq!(
            (
                paths[0].stroke.as_str(),
                paths[0].stroke_width,
                paths[0].fill.as_str()
            ),
            ("none", 0.0, "red")
        );
        assert!(paths[0].d.starts_with('M'));
    }
}
