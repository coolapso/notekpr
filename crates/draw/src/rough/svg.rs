//! Drawables as SVG `<path>` elements. Ported from rough.js 4.6.4 `svg.ts`
//! (MIT, see `crates/draw/PORTED.md`).

use super::generator::ops_to_path;
use super::{Drawable, OpSet, OpSetType, Options, Shape};
use crate::js;

/// The `<path>` elements `RoughSVG.draw` makes for a drawable, as a browser
/// serializes them: attributes in the order rough sets them, numbers printed
/// as JavaScript prints them, path data rounded to
/// `fixed_decimal_place_digits`. Without rough's `<g>` around them (the
/// caller adds its own, with its transform).
pub fn draw(drawable: &Drawable) -> String {
    let o = &drawable.options;
    let precision = o.fixed_decimal_place_digits;
    let mut out = String::new();
    for drawing in &drawable.sets {
        match drawing.kind {
            OpSetType::Path => {
                let mut attrs = vec![
                    ("d", ops_to_path(drawing, precision)),
                    ("stroke", o.stroke.clone()),
                    ("stroke-width", js::number_to_string(o.stroke_width)),
                    ("fill", "none".into()),
                ];
                dash_attrs(
                    &mut attrs,
                    o.stroke_line_dash.as_deref(),
                    o.stroke_line_dash_offset,
                );
                push_path(&mut out, &attrs);
            }
            OpSetType::FillPath => {
                let mut attrs = vec![
                    ("d", ops_to_path(drawing, precision)),
                    ("stroke", "none".into()),
                    ("stroke-width", "0".into()),
                    ("fill", o.fill_or("").into()),
                ];
                if matches!(drawable.shape, Shape::Curve | Shape::Polygon) {
                    attrs.push(("fill-rule", "evenodd".into()));
                }
                push_path(&mut out, &attrs);
            }
            OpSetType::FillSketch => push_path(&mut out, &fill_sketch(drawing, o)),
        }
    }
    out
}

fn fill_sketch(drawing: &OpSet, o: &Options) -> Vec<(&'static str, String)> {
    let mut fweight = o.fill_weight;
    if fweight < 0.0 {
        fweight = o.stroke_width / 2.0;
    }
    let mut attrs = vec![
        ("d", ops_to_path(drawing, o.fixed_decimal_place_digits)),
        ("stroke", o.fill_or("").into()),
        ("stroke-width", js::number_to_string(fweight)),
        ("fill", "none".into()),
    ];
    dash_attrs(
        &mut attrs,
        o.fill_line_dash.as_deref(),
        o.fill_line_dash_offset,
    );
    attrs
}

/// `stroke-dasharray` if there is a dash pattern (even an empty one, as an
/// empty array is true in JavaScript), `stroke-dashoffset` if the offset
/// isn't 0.
fn dash_attrs(attrs: &mut Vec<(&'static str, String)>, dash: Option<&[f64]>, offset: Option<f64>) {
    if let Some(dash) = dash {
        let list: Vec<String> = dash.iter().map(|&x| js::number_to_string(x)).collect();
        attrs.push(("stroke-dasharray", list.join(" ").trim().to_string()));
    }
    if let Some(offset) = offset.filter(|x| js::truthy(*x)) {
        attrs.push(("stroke-dashoffset", js::number_to_string(offset)));
    }
}

fn push_path(out: &mut String, attrs: &[(&str, String)]) {
    out.push_str("<path");
    for (name, value) in attrs {
        out.push_str(&format!(" {name}=\"{}\"", escape(value)));
    }
    out.push_str("></path>");
}

/// An attribute value as browsers serialize it (the characters both the
/// HTML and the XML serializer escape).
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rough::{FillStyle, Generator, Op, OpType};

    fn drawable(shape: Shape, sets: Vec<OpSet>, options: Options) -> Drawable {
        Drawable {
            shape,
            options,
            sets,
        }
    }

    fn set(kind: OpSetType) -> OpSet {
        OpSet {
            kind,
            ops: vec![
                Op {
                    op: OpType::Move,
                    data: vec![1.004, 2.0],
                },
                Op {
                    op: OpType::LineTo,
                    data: vec![3.5, 4.125],
                },
            ],
        }
    }

    #[test]
    fn stroke_paths() {
        let o = Options {
            stroke: "#1e1e1e".into(),
            stroke_width: 2.5,
            fixed_decimal_place_digits: Some(2),
            ..Options::default()
        };
        let svg = draw(&drawable(
            Shape::Path,
            vec![set(OpSetType::Path)],
            o.clone(),
        ));
        assert_eq!(
            svg,
            r##"<path d="M1 2 L3.5 4.13" stroke="#1e1e1e" stroke-width="2.5" fill="none"></path>"##
        );
        let dashed = Options {
            stroke_line_dash: Some(vec![8.0, 10.5]),
            stroke_line_dash_offset: Some(3.0),
            fixed_decimal_place_digits: None,
            ..o
        };
        let svg = draw(&drawable(Shape::Path, vec![set(OpSetType::Path)], dashed));
        assert_eq!(
            svg,
            r##"<path d="M1.004 2 L3.5 4.125" stroke="#1e1e1e" stroke-width="2.5" fill="none" stroke-dasharray="8 10.5" stroke-dashoffset="3"></path>"##
        );
    }

    #[test]
    fn fill_paths() {
        let o = Options {
            fill: Some("red".into()),
            ..Options::default()
        };
        let svg = draw(&drawable(
            Shape::Polygon,
            vec![set(OpSetType::FillPath)],
            o.clone(),
        ));
        assert_eq!(
            svg,
            r#"<path d="M1.004 2 L3.5 4.125" stroke="none" stroke-width="0" fill="red" fill-rule="evenodd"></path>"#
        );
        let svg = draw(&drawable(
            Shape::Rectangle,
            vec![set(OpSetType::FillPath)],
            o.clone(),
        ));
        assert!(!svg.contains("fill-rule"));
        let sketch = Options {
            fill_line_dash: Some(vec![]),
            fill_line_dash_offset: Some(0.0),
            ..o
        };
        let svg = draw(&drawable(
            Shape::Rectangle,
            vec![set(OpSetType::FillSketch)],
            sketch,
        ));
        assert_eq!(
            svg,
            r#"<path d="M1.004 2 L3.5 4.125" stroke="red" stroke-width="0.5" fill="none" stroke-dasharray=""></path>"#
        );
    }

    #[test]
    fn a_drawable_draws_fill_then_stroke() {
        let o = Options {
            seed: 1,
            fill: Some("a\"b<&>".into()),
            fill_style: FillStyle::Solid,
            ..Options::default()
        };
        let svg = draw(&Generator.rectangle(0.0, 0.0, 10.0, 10.0, &o));
        assert_eq!(svg.matches("<path").count(), 2);
        assert!(svg.starts_with("<path d=\"M"));
        assert!(svg.contains(r#"fill="a&quot;b&lt;&amp;&gt;""#));
        assert!(svg.ends_with(r##"stroke="#000" stroke-width="1" fill="none"></path>"##));
    }
}
