//! Parity with Excalidraw: shapes drawn here must give exactly the path data
//! Excalidraw's SVG export gives them —
//!
//! * the ellipses, rounded rectangles and arrow shafts of a drawing made on
//!   excalidraw.com (`tests/fixtures/example.*`, its export);
//! * the hachure-filled rectangle, diamond and ellipse of Excalidraw's own
//!   export tests (`tests/fixtures/export-test.svg`, from its snapshots).
//!
//! The few bits of Excalidraw's `shape.ts` this needs (the rough options of
//! an element, its corner radius, the rounded rectangle's path, the
//! diamond's corners) are ported here, from Excalidraw (MIT, see
//! `crates/draw/PORTED.md`).

use serde_json::Value;

use super::{FillStyle, Generator, Options, Point, svg};
use crate::js;

const DRAWING: &str = include_str!("../../tests/fixtures/example.excalidraw");
const EXPORT: &str = include_str!("../../tests/fixtures/example.svg");
/// Groups rough drew in Excalidraw's own export tests (filled shapes).
const EXPORT_TEST: &str = include_str!("../../tests/fixtures/export-test.svg");

/// `MAX_DECIMALS_FOR_SVG_EXPORT`: the precision of Excalidraw's SVG export.
const SVG_DECIMALS: u32 = 2;

fn num(e: &Value, key: &str) -> f64 {
    e[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} of {}", e["id"]))
}

fn text<'a>(e: &'a Value, key: &str) -> &'a str {
    e[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} of {}", e["id"]))
}

/// `isTransparent`: a colour with alpha 0.
fn is_transparent(color: &str) -> bool {
    color == "transparent" || (color.len() == 9 && color.starts_with('#') && color.ends_with("00"))
}

/// `adjustRoughness`: small shapes are drawn less rough.
fn adjust_roughness(e: &Value) -> f64 {
    let roughness = num(e, "roughness");
    let max_size = js::max(num(e, "width"), num(e, "height"));
    let min_size = js::min(num(e, "width"), num(e, "height"));
    let kind = text(e, "type");
    let can_change_roundness = matches!(
        kind,
        "rectangle" | "iframe" | "embeddable" | "line" | "diamond" | "stickynote" | "image"
    );
    let is_linear = matches!(kind, "arrow" | "line");
    let has_roundness = !e["roundness"].is_null();
    if (min_size >= 20.0 && max_size >= 50.0)
        || (min_size >= 15.0 && has_roundness && can_change_roundness)
        || (is_linear && max_size >= 50.0)
    {
        return roughness;
    }
    js::min(roughness / (if max_size < 10.0 { 3.0 } else { 2.0 }), 2.5)
}

/// `generateRoughOptions` for rectangles, diamonds, ellipses and arrows (the
/// dark mode filter on colours left out: only path data is compared).
fn rough_options(e: &Value, continuous_path: bool) -> Options {
    let stroke_width = num(e, "strokeWidth");
    let stroke_style = text(e, "strokeStyle");
    let background = text(e, "backgroundColor");
    let mut options = Options {
        seed: js::to_int32(num(e, "seed")),
        stroke_line_dash: match stroke_style {
            "dashed" => Some(vec![8.0, 8.0 + stroke_width]),
            "dotted" => Some(vec![1.5, 6.0 + stroke_width]),
            _ => None,
        },
        disable_multi_stroke: stroke_style != "solid",
        stroke_width: if stroke_style != "solid" {
            stroke_width + 0.5
        } else {
            stroke_width
        },
        fill_weight: stroke_width / 2.0,
        hachure_gap: stroke_width * 4.0,
        roughness: adjust_roughness(e),
        stroke: text(e, "strokeColor").into(),
        // ROUGHNESS.cartoonist is 2.
        preserve_vertices: continuous_path || num(e, "roughness") < 2.0,
        ..Options::default()
    };
    match text(e, "type") {
        "rectangle" | "diamond" | "ellipse" => {
            options.fill_style = FillStyle::from_name(text(e, "fillStyle"));
            options.fill = (!is_transparent(background)).then(|| background.into());
            if text(e, "type") == "ellipse" {
                options.curve_fitting = 1.0;
            }
        }
        "arrow" => {}
        other => panic!("options for {other} aren't ported"),
    }
    options
}

/// `getCornerRadius` (utils.ts).
fn corner_radius(x: f64, e: &Value) -> f64 {
    const DEFAULT_PROPORTIONAL_RADIUS: f64 = 0.25;
    const DEFAULT_ADAPTIVE_RADIUS: f64 = 32.0;
    match e["roundness"]["type"].as_f64() {
        // LEGACY, PROPORTIONAL_RADIUS
        Some(1.0 | 2.0) => x * DEFAULT_PROPORTIONAL_RADIUS,
        // ADAPTIVE_RADIUS
        Some(3.0) => {
            let fixed_radius_size = e["roundness"]["value"]
                .as_f64()
                .unwrap_or(DEFAULT_ADAPTIVE_RADIUS);
            let cutoff_size = fixed_radius_size / DEFAULT_PROPORTIONAL_RADIUS;
            if x <= cutoff_size {
                x * DEFAULT_PROPORTIONAL_RADIUS
            } else {
                fixed_radius_size
            }
        }
        _ => 0.0,
    }
}

/// `getDiamondPoints` (bounds.ts): top, right, bottom, left. The +1 keeps
/// them off 0.
fn diamond_points(e: &Value) -> [Point; 4] {
    let (w, h) = (num(e, "width"), num(e, "height"));
    let top_x = (w / 2.0).floor() + 1.0;
    let right_y = (h / 2.0).floor() + 1.0;
    [[top_x, 0.0], [w, right_y], [top_x, h], [0.0, right_y]]
}

/// The shape Excalidraw generates for a rectangle, an ellipse or a sharp
/// diamond (`_generateElementShape`), as SVG, at the export's precision.
fn excalidraw_svg(e: &Value) -> String {
    let g = Generator;
    let (w, h) = (num(e, "width"), num(e, "height"));
    let mut drawable = match text(e, "type") {
        "ellipse" => g.ellipse(w / 2.0, h / 2.0, w, h, &rough_options(e, false)),
        "rectangle" if !e["roundness"].is_null() => {
            let r = corner_radius(js::min(w, h), e);
            let n = js::number_to_string;
            let path = format!(
                "M {r} 0 L {w_r} 0 Q {w} 0, {w} {r} L {w} {h_r} Q {w} {h}, {w_r} {h} L {r} {h} Q 0 {h}, 0 {h_r} L 0 {r} Q 0 0, {r} 0",
                r = n(r),
                w = n(w),
                h = n(h),
                w_r = n(w - r),
                h_r = n(h - r),
            );
            g.path(&path, &rough_options(e, true))
                .expect("Excalidraw's path data")
        }
        "rectangle" => g.rectangle(0.0, 0.0, w, h, &rough_options(e, false)),
        "diamond" if e["roundness"].is_null() => {
            g.polygon(&diamond_points(e), &rough_options(e, false))
        }
        other => panic!("{other}'s shape isn't ported"),
    };
    // `roughSVGDrawWithPrecision`.
    drawable.options.fixed_decimal_place_digits = Some(SVG_DECIMALS);
    svg::draw(&drawable)
}

/// The `d` attributes of the `<path>` elements in `svg`, in order.
fn path_data(svg: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(i) = rest.find("<path") {
        rest = &rest[i..];
        let end = rest.find('>').expect("end of <path>");
        let tag = &rest[..end];
        if let Some(d) = tag.find(" d=\"") {
            let value = &tag[d + 4..];
            out.push(&value[..value.find('"').expect("end of d")]);
        }
        rest = &rest[end..];
    }
    out
}

/// The contents of the export's `<g>` groups for element `e` drawn by
/// rough: Excalidraw gives them `stroke-linecap="round"` and
/// `transform="translate(x y) rotate(degrees cx cy)"`, with the centre
/// computed from the element's corners (`renderElementToSvg`), which isn't
/// always exactly half the size.
fn exported_groups(e: &Value) -> Vec<&'static str> {
    let (x1, y1) = (num(e, "x"), num(e, "y"));
    let (x2, y2) = (x1 + num(e, "width"), y1 + num(e, "height"));
    let cx = (x2 - x1) / 2.0 - (num(e, "x") - x1);
    let cy = (y2 - y1) / 2.0 - (num(e, "y") - y1);
    let rotate = format!(
        " rotate(0 {} {})\"",
        js::number_to_string(cx),
        js::number_to_string(cy)
    );
    let mut groups = Vec::new();
    let mut from = 0;
    while let Some(i) = EXPORT[from..].find(&rotate) {
        let at = from + i;
        let start = EXPORT[..at].rfind("<g ").expect("<g> of the transform");
        let open_end = at + EXPORT[at..].find('>').expect("end of <g>");
        let close = open_end + EXPORT[open_end..].find("</g>").expect("</g>");
        if EXPORT[start..open_end].contains("stroke-linecap=\"round\"") {
            groups.push(&EXPORT[open_end + 1..close]);
        }
        from = close;
    }
    groups
}

/// The first difference between two path data strings, for a readable
/// failure.
fn first_difference(ours: &str, theirs: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (ours.split(' ').collect(), theirs.split(' ').collect());
    match a.iter().zip(&b).position(|(x, y)| x != y) {
        Some(i) => format!("token {i}: ours {:?}, Excalidraw's {:?}", a[i], b[i]),
        None => format!("lengths: ours {} tokens, Excalidraw's {}", a.len(), b.len()),
    }
}

#[test]
fn ellipses_and_rectangles_match_excalidraws_export() {
    let drawing: Value = serde_json::from_str(DRAWING).expect("the drawing");
    let elements = drawing["elements"].as_array().expect("elements");
    let mut checked = Vec::new();
    let mut failures = Vec::new();
    for e in elements {
        let kind = text(e, "type");
        if !matches!(kind, "ellipse" | "rectangle") || e["isDeleted"].as_bool() == Some(true) {
            continue;
        }
        assert_eq!(
            num(e, "angle"),
            0.0,
            "the export is matched by an unrotated transform"
        );
        let groups = exported_groups(e);
        assert_eq!(groups.len(), 1, "one group in the export for {}", e["id"]);
        let theirs = path_data(groups[0]);
        assert!(!theirs.is_empty(), "paths in the export for {}", e["id"]);
        let svg = excalidraw_svg(e);
        let ours = path_data(&svg);
        assert_eq!(ours.len(), theirs.len(), "paths of {kind} {}", e["id"]);
        for (o, t) in ours.iter().zip(&theirs) {
            if o != t {
                failures.push(format!("{kind} {}: {}", e["id"], first_difference(o, t)));
            }
        }
        checked.push(kind);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(
        checked.iter().filter(|k| **k == "ellipse").count(),
        3,
        "ellipses checked"
    );
    assert_eq!(
        checked.iter().filter(|k| **k == "rectangle").count(),
        2,
        "rectangles checked"
    );
}

/// An element of Excalidraw's `elementFixture.ts`, `size` × `size`.
fn fixture(kind: &str, size: f64) -> Value {
    serde_json::json!({
        "id": "vWrqOAfkind2qcm7LDAGZ",
        "type": kind,
        "x": 414,
        "y": 237,
        "width": size,
        "height": size,
        "angle": 0,
        "strokeColor": "#000000",
        "backgroundColor": "#15aabf",
        "fillStyle": "hachure",
        "strokeWidth": 1,
        "strokeStyle": "solid",
        "roughness": 1,
        "roundness": null,
        "seed": 1_041_657_908,
    })
}

#[test]
fn hachure_fills_match_excalidraws_export_tests() {
    // The groups in the fixture, in order: a rectangle (214 × 214), a
    // diamond and an ellipse (100 × 100); each a hachure fill, then the
    // outline.
    let mut groups = Vec::new();
    let mut rest = EXPORT_TEST;
    while let Some(i) = rest.find("<g stroke-linecap=\"round\"") {
        let end = rest[i..].find("</g>").expect("</g>");
        groups.push(&rest[i..i + end]);
        rest = &rest[i + end..];
    }
    let elements = [
        fixture("rectangle", 214.0),
        fixture("diamond", 100.0),
        fixture("ellipse", 100.0),
    ];
    assert_eq!(groups.len(), elements.len());
    for (e, group) in elements.iter().zip(groups) {
        let svg = excalidraw_svg(e);
        let (ours, theirs) = (path_data(&svg), path_data(group));
        assert_eq!(theirs.len(), 2);
        assert_eq!(ours.len(), theirs.len(), "paths of the {}", e["type"]);
        for (o, t) in ours.iter().zip(&theirs) {
            assert!(o == t, "{}: {}", e["type"], first_difference(o, t));
        }
    }
}

#[test]
fn arrow_shafts_match_excalidraws_export() {
    // A round arrow's shaft is `generator.curve` through its points (the
    // arrowheads are lines whose ends need more of Excalidraw). Their
    // transform's centre comes from the curve's bounds, so the shaft is
    // looked up by its path data instead: the export must have a path with
    // exactly this data.
    let drawing: Value = serde_json::from_str(DRAWING).expect("the drawing");
    let mut checked = 0;
    for e in drawing["elements"].as_array().expect("elements") {
        if text(e, "type") != "arrow" || e["isDeleted"].as_bool() == Some(true) {
            continue;
        }
        assert!(!e["roundness"].is_null() && e["elbowed"].as_bool() != Some(true));
        let points: Vec<Point> = e["points"]
            .as_array()
            .expect("points")
            .iter()
            .map(|p| [p[0].as_f64().expect("x"), p[1].as_f64().expect("y")])
            .collect();
        let mut drawable = Generator.curve(&points, &rough_options(e, false));
        drawable.options.fixed_decimal_place_digits = Some(SVG_DECIMALS);
        let svg = svg::draw(&drawable);
        let ours = path_data(&svg);
        assert_eq!(ours.len(), 1);
        assert!(
            EXPORT.contains(&format!("<path d=\"{}\"", ours[0])),
            "arrow {}: {} is not in the export",
            e["id"],
            ours[0]
        );
        checked += 1;
    }
    assert_eq!(checked, 8, "arrows checked");
}

#[test]
fn rounded_rectangles_follow_the_corner_radius() {
    let e: Value = serde_json::json!({ "roundness": { "type": 3 } });
    assert_eq!(corner_radius(100.0, &e), 25.0);
    assert_eq!(corner_radius(200.0, &e), 32.0);
    let e: Value = serde_json::json!({ "roundness": { "type": 3, "value": 10 } });
    assert_eq!(corner_radius(200.0, &e), 10.0);
    let e: Value = serde_json::json!({ "roundness": { "type": 2 } });
    assert_eq!(corner_radius(200.0, &e), 50.0);
    assert_eq!(
        corner_radius(200.0, &serde_json::json!({ "roundness": null })),
        0.0
    );
}
