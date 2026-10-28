//! Pen ("freedraw") strokes, drawn as Excalidraw draws them: the freedraw
//! geometry of Excalidraw's `packages/element/src/shape.ts`, MIT, see
//! `crates/draw/PORTED.md`.
//!
//! A freedraw element is filled, not stroked: its outline is a polygon
//! around the pen's path, traced by perfect-freehand ([`perfect`]) for
//! pressure-sensitive strokes, or by Excalidraw's laser pointer ([`laser`])
//! for constant-width ones, and written as SVG path data with
//! [`get_svg_path_from_stroke`] — exactly the text Excalidraw's SVG export
//! writes, so the same drawing makes the same file.

pub mod laser;
pub mod perfect;
pub mod vec;

use std::f64::consts::PI;

use crate::js;
use laser::{LaserPointer, LaserPointerOptions};
use perfect::{InputPoint, StrokeOptions, get_stroke, get_stroke_points};

/// The `streamline` of strokes that don't set one (`DEFAULT_STROKE_STREAMLINE`).
pub const DEFAULT_STROKE_STREAMLINE: f64 = 0.5;

/// Tuning of pressure-sensitive strokes (`VARIABLE_WIDTH_FREEDRAW`): found
/// by eye in Excalidraw, not derived.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariableWidthFreedraw {
    /// perfect-freehand's `size` relative to the stroke width.
    pub size_factor: f64,
    pub thinning: f64,
    pub smoothing: f64,
}

pub const VARIABLE_WIDTH_FREEDRAW: VariableWidthFreedraw = VariableWidthFreedraw {
    size_factor: 4.25,
    thinning: 0.6,
    smoothing: 0.5,
};

/// Tuning of constant-width strokes (`CONSTANT_WIDTH_FREEDRAW`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConstantWidthFreedraw {
    /// The laser pointer's `size` relative to the stroke width.
    pub size_factor: f64,
    /// How far (px) the outline may move when simplified for hit testing.
    pub collision_simplify_tolerance: f64,
}

pub const CONSTANT_WIDTH_FREEDRAW: ConstantWidthFreedraw = ConstantWidthFreedraw {
    size_factor: 1.4,
    collision_simplify_tolerance: 0.2,
};

/// How a stroke's width varies (`strokeOptions.variability`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variability {
    /// With pressure (`"variable"`; also when unknown or absent).
    #[default]
    Variable,
    /// Not at all (`"constant"`).
    Constant,
}

/// What the stroke geometry needs from a freedraw element.
#[derive(Clone, Copy, Debug)]
pub struct Freedraw<'a> {
    /// The pen's path, relative to the element's position.
    pub points: &'a [[f64; 2]],
    /// The pen's pressure at each point (may be shorter, or empty).
    pub pressures: &'a [f64],
    /// Whether pressure comes from the pen's speed instead of `pressures`.
    pub simulate_pressure: bool,
    pub stroke_width: f64,
    /// `strokeOptions.variability`.
    pub variability: Variability,
    /// `strokeOptions.streamline`.
    pub streamline: Option<f64>,
}

/// `getFreedrawStreamline`.
fn get_freedraw_streamline(element: &Freedraw) -> f64 {
    element.streamline.unwrap_or(DEFAULT_STROKE_STREAMLINE)
}

/// `getVariableWidthFreedrawOutline`: the outline of a pressure-sensitive
/// stroke, with perfect-freehand (Excalidraw's original freedraw look).
pub fn get_variable_width_freedraw_outline(element: &Freedraw) -> Vec<[f64; 2]> {
    let input_points: Vec<InputPoint> = if element.simulate_pressure {
        element.points.iter().map(|&p| p.into()).collect()
    } else if !element.points.is_empty() {
        element
            .points
            .iter()
            .enumerate()
            .map(|(i, &point)| InputPoint {
                point,
                pressure: element.pressures.get(i).copied(),
            })
            .collect()
    } else {
        // No points: a dot.
        vec![[0.0, 0.0, 0.5].into()]
    };
    get_stroke(
        &input_points,
        &StrokeOptions {
            simulate_pressure: element.simulate_pressure,
            size: element.stroke_width * VARIABLE_WIDTH_FREEDRAW.size_factor,
            thinning: VARIABLE_WIDTH_FREEDRAW.thinning,
            smoothing: VARIABLE_WIDTH_FREEDRAW.smoothing,
            streamline: get_freedraw_streamline(element),
            // easeOutSine
            easing: |t| js::sin((t * PI) / 2.0),
            last: true,
            ..StrokeOptions::default()
        },
    )
}

/// `createLaserPointer`: the laser pointer that traces constant-width strokes.
fn create_laser_pointer(element: &Freedraw) -> LaserPointer {
    LaserPointer::new(LaserPointerOptions {
        size: element.stroke_width * CONSTANT_WIDTH_FREEDRAW.size_factor,
        streamline: get_freedraw_streamline(element),
        simplify: 0.0,
        size_mapping: |details| js::max(0.1, details.pressure),
        ..LaserPointerOptions::default()
    })
}

/// `getConstantWidthFreedrawOutline`: the outline of a constant-width
/// stroke, with the laser pointer, every point at pressure 1.
pub fn get_constant_width_freedraw_outline(element: &Freedraw) -> Vec<[f64; 2]> {
    let mut laser_pointer = create_laser_pointer(element);
    for &[x, y] in element.points {
        laser_pointer.add_point([x, y, 1.0]);
    }
    laser_pointer
        .get_stroke_outline(None)
        .into_iter()
        .map(|[x, y, _]| [x, y])
        .collect()
}

/// `getFreedrawOutlinePoints`: the outline polygon of a freedraw element.
pub fn get_freedraw_outline_points(element: &Freedraw) -> Vec<[f64; 2]> {
    match element.variability {
        Variability::Constant => get_constant_width_freedraw_outline(element),
        Variability::Variable => get_variable_width_freedraw_outline(element),
    }
}

/// `getFreedrawMaxStrokeRadius`: how far at most a stroke's ink reaches past
/// its points. Both generators scale their size by at most 1, except that
/// perfect-freehand's strokes shorter than 3px get a start cap drawn from
/// the last point, up to 3px further.
pub fn get_freedraw_max_stroke_radius(element: &Freedraw) -> f64 {
    match element.variability {
        Variability::Constant => element.stroke_width * CONSTANT_WIDTH_FREEDRAW.size_factor,
        Variability::Variable => element.stroke_width * VARIABLE_WIDTH_FREEDRAW.size_factor + 3.0,
    }
}

/// `getFreedrawStrokeCenterPoints`: the smoothed path the stroke is drawn
/// around, relative to the element. What a bucket fill should follow: the
/// raw points can be 20px apart, and the drawn stroke cuts between them.
/// (Constant-width strokes smooth with the laser pointer instead; this is
/// close enough for them, under the stroke's width.)
pub fn get_freedraw_stroke_center_points(element: &Freedraw) -> Vec<[f64; 2]> {
    get_stroke_points(
        element.points,
        &StrokeOptions {
            size: element.stroke_width * VARIABLE_WIDTH_FREEDRAW.size_factor,
            streamline: get_freedraw_streamline(element),
            last: true,
            ..StrokeOptions::default()
        },
    )
    .into_iter()
    .map(|p| p.point)
    .collect()
}

/// `getFreeDrawSvgPath`: the SVG path data of a freedraw element's stroke,
/// the `d` of its `<path>` in Excalidraw's SVG export (and what its canvas
/// fills).
pub fn get_free_draw_svg_path(element: &Freedraw) -> String {
    get_svg_path_from_stroke(&get_freedraw_outline_points(element))
}

/// `getSvgPathFromStroke`: an outline polygon as SVG path data, smoothed
/// with quadratic curves through the midpoints of its edges.
///
/// Like Excalidraw, it writes each number as JavaScript prints it and then
/// cuts (not rounds) the text after two decimals.
pub fn get_svg_path_from_stroke(points: &[[f64; 2]]) -> String {
    if points.is_empty() {
        return String::new();
    }
    let med = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let max = points.len() - 1;
    // `["M", points[0], "Q", …].join(" ")`
    let mut parts = vec![
        "M".to_string(),
        js::array_to_string(&points[0]),
        "Q".to_string(),
    ];
    for (i, &point) in points.iter().enumerate() {
        parts.push(js::array_to_string(&point));
        if i == max {
            parts.push(js::array_to_string(&med(point, points[0])));
            parts.push("L".to_string());
            parts.push(js::array_to_string(&points[0]));
            parts.push("Z".to_string());
        } else {
            parts.push(js::array_to_string(&med(point, points[i + 1])));
        }
    }
    to_fixed_precision(&parts.join(" "))
}

/// `.replace(TO_FIXED_PRECISION, "$1")`, where `TO_FIXED_PRECISION` is
/// `/(\s?[A-Z]?,?-?[0-9]*\.[0-9]{0,2})(([0-9]|e|-)*)/g`: wherever a number
/// has a decimal point, the digits after the second decimal are cut, along
/// with any exponent (`1.5e-7` becomes `1.5`; `1.5e+21` becomes `1.5+21`).
///
/// The pattern's parts before the point match disjoint characters, so a
/// match can be found without backtracking: from each position, take each
/// optional part if it's there, then digits, then the point must follow.
fn to_fixed_precision(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        if let Some((kept_end, cut_end)) = match_to_fixed_precision(s, i) {
            out.push_str(&s[i..kept_end]);
            i = cut_end;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// The pattern matched at byte `start` of `s`: where its first group (kept)
/// ends, and where the whole match (the rest is cut) ends.
fn match_to_fixed_precision(s: &str, start: usize) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut j = start;
    // `\s?`
    if let Some(c) = s[j..].chars().next()
        && js::is_space(c)
    {
        j += c.len_utf8();
    }
    // `[A-Z]?`, `,?`, `-?`
    if b.get(j).is_some_and(u8::is_ascii_uppercase) {
        j += 1;
    }
    if b.get(j) == Some(&b',') {
        j += 1;
    }
    if b.get(j) == Some(&b'-') {
        j += 1;
    }
    // `[0-9]*\.`
    while b.get(j).is_some_and(u8::is_ascii_digit) {
        j += 1;
    }
    if b.get(j) != Some(&b'.') {
        return None;
    }
    j += 1;
    // `[0-9]{0,2}`
    let mut decimals = 0;
    while decimals < 2 && b.get(j).is_some_and(u8::is_ascii_digit) {
        j += 1;
        decimals += 1;
    }
    let kept_end = j;
    // `([0-9]|e|-)*`
    while b
        .get(j)
        .is_some_and(|&c| c.is_ascii_digit() || c == b'e' || c == b'-')
    {
        j += 1;
    }
    Some((kept_end, j))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke<'a>(points: &'a [[f64; 2]], variability: Variability) -> Freedraw<'a> {
        Freedraw {
            points,
            pressures: &[],
            simulate_pressure: true,
            stroke_width: 2.0,
            variability,
            streamline: None,
        }
    }

    #[test]
    fn numbers_cut_after_two_decimals() {
        for (path, cut) in [
            ("M 1.23456,-7.891 Q", "M 1.23,-7.89 Q"),
            ("12,3 0.1,-0.5", "12,3 0.1,-0.5"),
            ("-0.000001234,5.5", "-0.00,5.5"),
            // Exponents go too: the pattern cuts digits, `e` and `-`.
            ("1.5e-7,2.345e-8", "1.5,2.34"),
            ("1.5e+21,1e-7", "1.5+21,1e-7"),
            ("L 99.999999,Z", "L 99.99,Z"),
            ("", ""),
            ("é 1.234", "é 1.23"),
        ] {
            assert_eq!(to_fixed_precision(path), cut, "{path}");
        }
    }

    #[test]
    fn svg_path_from_stroke() {
        assert_eq!(get_svg_path_from_stroke(&[]), "");
        assert_eq!(
            get_svg_path_from_stroke(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]),
            "M 0,0 Q 0,0 5,0 10,0 10,5 10,10 5,5 L 0,0 Z"
        );
        assert_eq!(
            get_svg_path_from_stroke(&[[0.123456, -1.0 / 3.0], [2.0, 1e-7]]),
            "M 0.12,-0.33 Q 0.12,-0.33 1.06,-0.16 2,1e-7 1.06,-0.16 L 0.12,-0.33 Z"
        );
    }

    #[test]
    fn variability_picks_the_generator() {
        let points = [[0.0, 0.0], [10.0, 5.0], [20.0, 0.0], [30.0, 8.0]];
        let constant = get_freedraw_outline_points(&stroke(&points, Variability::Constant));
        let variable = get_freedraw_outline_points(&stroke(&points, Variability::Variable));
        assert_ne!(constant, variable);
        assert_eq!(
            constant,
            get_constant_width_freedraw_outline(&stroke(&points, Variability::Constant))
        );
        assert_eq!(
            variable,
            get_variable_width_freedraw_outline(&stroke(&points, Variability::Constant))
        );
        assert_eq!(
            get_free_draw_svg_path(&stroke(&points, Variability::Variable)),
            get_svg_path_from_stroke(&variable)
        );
    }

    #[test]
    fn strokes_stay_within_their_max_radius() {
        let points: Vec<[f64; 2]> = (0..40)
            .map(|i| [f64::from(i) * 3.0, (f64::from(i) * 0.4).sin() * 20.0])
            .collect();
        for variability in [Variability::Constant, Variability::Variable] {
            let element = stroke(&points, variability);
            let max = get_freedraw_max_stroke_radius(&element);
            let outline = get_freedraw_outline_points(&element);
            assert!(!outline.is_empty());
            for p in &outline {
                let nearest = points
                    .iter()
                    .map(|&q| vec::dist(*p, q))
                    .fold(f64::INFINITY, f64::min);
                // The stroke runs between the points, at most 3px from one.
                assert!(nearest <= max + 3.0, "{variability:?} {p:?}");
            }
        }
        assert_eq!(
            get_freedraw_max_stroke_radius(&stroke(&[], Variability::Constant)),
            2.8
        );
        assert_eq!(
            get_freedraw_max_stroke_radius(&stroke(&[], Variability::Variable)),
            11.5
        );
    }

    #[test]
    fn pressures_and_empty_strokes() {
        let points = [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]];
        let pen = |pressures: &'static [f64]| Freedraw {
            pressures,
            simulate_pressure: false,
            ..stroke(&points, Variability::Variable)
        };
        let width = |outline: Vec<[f64; 2]>| outline.iter().fold(0.0f64, |w, p| w.max(p[1].abs()));
        let light = width(get_freedraw_outline_points(&pen(&[0.1, 0.1, 0.1, 0.1])));
        let heavy = width(get_freedraw_outline_points(&pen(&[0.9, 0.9, 0.9, 0.9])));
        assert!(light < heavy, "{light} {heavy}");
        // Missing pressures count as 0.5 (0.25 for the first point).
        assert_eq!(
            get_freedraw_outline_points(&pen(&[])),
            get_freedraw_outline_points(&pen(&[0.25, 0.5, 0.5, 0.5]))
        );
        // No points: nothing, or a dot at 0,0 when pressures are recorded.
        assert!(get_freedraw_outline_points(&stroke(&[], Variability::Variable)).is_empty());
        assert!(get_freedraw_outline_points(&stroke(&[], Variability::Constant)).is_empty());
        let dot = get_freedraw_outline_points(&Freedraw {
            points: &[],
            ..pen(&[])
        });
        assert!(!dot.is_empty() && dot.iter().all(|&p| vec::len(p) < 11.5));
    }

    #[test]
    fn center_points_follow_the_smoothed_path() {
        let points = [
            [0.0, 0.0],
            [10.0, 0.0],
            [20.0, 10.0],
            [30.0, 10.0],
            [40.0, 0.0],
        ];
        let center = get_freedraw_stroke_center_points(&stroke(&points, Variability::Constant));
        assert_eq!(center.first(), Some(&[0.0, 0.0]));
        assert_eq!(center.last(), Some(&[40.0, 0.0]));
        let smoother = get_freedraw_stroke_center_points(&Freedraw {
            streamline: Some(0.9),
            ..stroke(&points, Variability::Constant)
        });
        assert_ne!(center, smoother);
    }

    /// The value of attribute `name` in the tag text `tag`.
    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let key = format!(" {name}=\"");
        let start = tag.find(&key)? + key.len();
        Some(&tag[start..start + tag[start..].find('"')?])
    }

    /// The freedraw strokes of an Excalidraw SVG export, in order: each
    /// element's `<g transform="…" stroke="none">` and the `d` of its last
    /// `<path>` (the stroke; a fill would come first).
    fn svg_freedraw_paths(svg: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut rest = svg;
        while let Some(start) = rest.find("<g ") {
            rest = &rest[start..];
            let tag_end = rest.find('>').expect("tag end");
            let tag = &rest[..tag_end];
            rest = &rest[tag_end + 1..];
            if attr(tag, "stroke") != Some("none") {
                continue;
            }
            let body = &rest[..rest.find("</g>").expect("group end")];
            let path = &body[body.rfind("<path ").expect("a path")..];
            out.push((
                attr(tag, "transform").expect("transform").to_string(),
                attr(path, "d").expect("path data").to_string(),
            ));
        }
        out
    }

    /// The `transform` Excalidraw's SVG export gives an element without its
    /// translation: `rotate(degrees cx cy)` around the centre of its points.
    fn rotation(element: &serde_json::Value, points: &[[f64; 2]]) -> String {
        let x = element["x"].as_f64().expect("x");
        let y = element["y"].as_f64().expect("y");
        let angle = element["angle"].as_f64().expect("angle");
        let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
        let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &[px, py] in points {
            min_x = js::min(min_x, px);
            min_y = js::min(min_y, py);
            max_x = js::max(max_x, px);
            max_y = js::max(max_y, py);
        }
        let (x1, y1, x2, y2) = (min_x + x, min_y + y, max_x + x, max_y + y);
        let cx = (x2 - x1) / 2.0 - (x - x1);
        let cy = (y2 - y1) / 2.0 - (y - y1);
        let degree = (180.0 * angle) / PI;
        format!(
            "rotate({} {} {})",
            js::number_to_string(degree),
            js::number_to_string(cx),
            js::number_to_string(cy)
        )
    }

    #[test]
    fn same_paths_as_excalidraw_svg_export() {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/example.excalidraw"))
                .expect("drawing");
        let svg = include_str!("../../tests/fixtures/example.svg");
        let elements: Vec<&serde_json::Value> = file["elements"]
            .as_array()
            .expect("elements")
            .iter()
            .filter(|e| e["type"] == "freedraw" && e["isDeleted"] != true)
            .collect();
        let exported = svg_freedraw_paths(svg);
        assert_eq!(elements.len(), 49);
        assert_eq!(exported.len(), elements.len());

        let mut matching = 0;
        let mut first_mismatch = None;
        for (i, (element, (transform, d))) in elements.iter().zip(&exported).enumerate() {
            let points: Vec<[f64; 2]> = element["points"]
                .as_array()
                .expect("points")
                .iter()
                .map(|p| [p[0].as_f64().expect("x"), p[1].as_f64().expect("y")])
                .collect();
            let pressures: Vec<f64> = element["pressures"]
                .as_array()
                .expect("pressures")
                .iter()
                .map(|p| p.as_f64().expect("pressure"))
                .collect();
            let options = &element["strokeOptions"];
            let freedraw = Freedraw {
                points: &points,
                pressures: &pressures,
                simulate_pressure: element["simulatePressure"] == true,
                stroke_width: element["strokeWidth"].as_f64().expect("stroke width"),
                variability: if options["variability"] == "constant" {
                    Variability::Constant
                } else {
                    Variability::Variable
                },
                streamline: options["streamline"].as_f64(),
            };
            // The SVG's element is this one: same centre of rotation.
            let rotate = rotation(element, &points);
            assert!(
                transform.ends_with(&rotate),
                "element {i}: {transform}, not {rotate}"
            );
            let ours = get_free_draw_svg_path(&freedraw);
            if ours == *d {
                matching += 1;
            } else if first_mismatch.is_none() {
                let at = ours
                    .bytes()
                    .zip(d.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                first_mismatch = Some(format!(
                    "element {i} ({}), from byte {at}:\n ours: {}\n svg:  {}",
                    element["id"],
                    &ours[at.saturating_sub(40)..(at + 80).min(ours.len())],
                    &d[at.saturating_sub(40)..(at + 80).min(d.len())],
                ));
            }
        }
        assert_eq!(
            matching,
            elements.len(),
            "{matching} of {} paths match; first difference in {}",
            elements.len(),
            first_mismatch.unwrap_or_default()
        );
    }
}
