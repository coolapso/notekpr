//! Drawing a drawing on a `<canvas>`, as Excalidraw does
//! (`renderElement.ts`, rough's canvas renderer): the same shapes as the
//! SVG export, each element moved to its place and turned, at the
//! viewport's scroll and zoom. Shapes are kept until their element changes.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use notekpr_draw::bounds::{self, Bounds};
use notekpr_draw::color;
use notekpr_draw::fonts;
use notekpr_draw::rough::{Drawable, OpSetType, OpType, Shape};
use notekpr_draw::scene::{Element, ElementType, Scene};
use notekpr_draw::shape::{self, ElementShape, RenderConfig};
use notekpr_draw::viewport::Viewport;
use serde_json::Value;
use wasm_bindgen::JsValue;
use web_sys::{CanvasRenderingContext2d, CanvasWindingRule, Path2d};

/// `FRAME_STYLE`.
const FRAME_STROKE: &str = "#bbb";
const FRAME_RADIUS: f64 = 8.0;

fn hash_value(v: &Value, h: &mut impl Hasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => b.hash(h),
        Value::Number(n) => n.as_f64().unwrap_or(0.0).to_bits().hash(h),
        Value::String(s) => s.hash(h),
        Value::Array(a) => {
            a.len().hash(h);
            for x in a {
                hash_value(x, h);
            }
        }
        Value::Object(o) => {
            o.len().hash(h);
            for (k, x) in o {
                k.hash(h);
                hash_value(x, h);
            }
        }
    }
}

/// What an element looks like: all its properties, and the theme.
fn look(e: &Element, dark: bool, background: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (k, v) in e.props() {
        k.hash(&mut h);
        hash_value(v, &mut h);
    }
    dark.hash(&mut h);
    background.hash(&mut h);
    h.finish()
}

struct Cached {
    look: u64,
    shape: Rc<ElementShape>,
    bounds: Bounds,
    /// `[x1, y1, x2, y2, cx, cy]` before rotation.
    coords: [f64; 6],
}

/// Shapes and boxes of elements, kept until they change.
#[derive(Default)]
pub struct ShapeCache {
    map: HashMap<String, Cached>,
}

impl ShapeCache {
    fn get(&mut self, e: &Element, cfg: &RenderConfig) -> &Cached {
        let look = look(e, cfg.dark, &cfg.canvas_background);
        let fresh = self.map.get(e.id()).is_some_and(|c| c.look == look);
        if !fresh {
            self.map.insert(
                e.id().to_string(),
                Cached {
                    look,
                    shape: Rc::new(shape::element_shape(e, cfg)),
                    bounds: bounds::element_bounds(e),
                    coords: bounds::absolute_coords(e),
                },
            );
        }
        &self.map[e.id()]
    }

    /// Forgets elements no longer there.
    fn keep(&mut self, ids: &HashSet<&str>) {
        self.map.retain(|id, _| ids.contains(id.as_str()));
    }
}

fn draw_ops(ctx: &CanvasRenderingContext2d, ops: &[notekpr_draw::rough::Op]) {
    ctx.begin_path();
    for op in ops {
        let d = &op.data;
        match op.op {
            OpType::Move => ctx.move_to(d[0], d[1]),
            OpType::BCurveTo => ctx.bezier_curve_to(d[0], d[1], d[2], d[3], d[4], d[5]),
            OpType::LineTo => ctx.line_to(d[0], d[1]),
        }
    }
}

fn dash(ctx: &CanvasRenderingContext2d, dash: Option<&Vec<f64>>) {
    let arr = js_sys::Array::new();
    for d in dash.into_iter().flatten() {
        arr.push(&JsValue::from_f64(*d));
    }
    let _ = ctx.set_line_dash(&arr);
}

/// rough's `RoughCanvas.draw`.
fn draw_rough(ctx: &CanvasRenderingContext2d, d: &Drawable) {
    let o = &d.options;
    for set in &d.sets {
        ctx.save();
        match set.kind {
            OpSetType::Path => {
                let stroke = if o.stroke == "none" {
                    "transparent"
                } else {
                    &o.stroke
                };
                ctx.set_stroke_style_str(stroke);
                ctx.set_line_width(o.stroke_width);
                dash(ctx, o.stroke_line_dash.as_ref());
                if let Some(off) = o.stroke_line_dash_offset {
                    ctx.set_line_dash_offset(off);
                }
                draw_ops(ctx, &set.ops);
                ctx.stroke();
            }
            OpSetType::FillPath => {
                ctx.set_fill_style_str(o.fill.as_deref().unwrap_or_default());
                draw_ops(ctx, &set.ops);
                let rule = if matches!(d.shape, Shape::Curve | Shape::Polygon | Shape::Path) {
                    CanvasWindingRule::Evenodd
                } else {
                    CanvasWindingRule::Nonzero
                };
                ctx.fill_with_canvas_winding_rule(rule);
            }
            OpSetType::FillSketch => {
                let weight = if o.fill_weight < 0.0 {
                    o.stroke_width / 2.0
                } else {
                    o.fill_weight
                };
                dash(ctx, o.fill_line_dash.as_ref());
                if let Some(off) = o.fill_line_dash_offset {
                    ctx.set_line_dash_offset(off);
                }
                ctx.set_stroke_style_str(o.fill.as_deref().unwrap_or_default());
                ctx.set_line_width(weight);
                draw_ops(ctx, &set.ops);
                ctx.stroke();
            }
        }
        ctx.restore();
    }
}

fn draw_text(ctx: &CanvasRenderingContext2d, e: &Element, dark: bool) {
    let size = e.font_size();
    let line_height = size * e.line_height();
    ctx.set_font(&format!(
        "{}px {}",
        js_number(size),
        fonts::css_family(e.font_family())
    ));
    ctx.set_fill_style_str(&color::themed(e.stroke_color(), dark));
    let (align, x) = match e.text_align() {
        "center" => ("center", e.width() / 2.0),
        "right" => ("right", e.width()),
        _ => ("left", 0.0),
    };
    ctx.set_text_align(align);
    ctx.set_text_baseline("alphabetic");
    let offset = fonts::baseline_offset(e.font_family(), size, line_height);
    let text = e.text().replace("\r\n", "\n").replace('\r', "\n");
    for (i, line) in text.split('\n').enumerate() {
        let _ = ctx.fill_text(line, x, i as f64 * line_height + offset);
    }
}

fn js_number(x: f64) -> String {
    notekpr_draw::js::number_to_string(x)
}

/// A frame, and its name above it unless that is being typed.
fn draw_frame(ctx: &CanvasRenderingContext2d, e: &Element, dark: bool, zoom: f64, named: bool) {
    ctx.set_stroke_style_str(&color::themed(FRAME_STROKE, dark));
    ctx.set_line_width(2.0 / zoom);
    ctx.begin_path();
    let _ = ctx.round_rect_with_f64(0.0, 0.0, e.width(), e.height(), FRAME_RADIUS / zoom);
    ctx.stroke();
    if named {
        super::frames::draw_name(ctx, e, dark, zoom);
    }
}

/// A picture (`drawImageElement`): cropped, flipped and with rounded
/// corners as the element says; a grey box while it loads.
fn draw_image(
    ctx: &CanvasRenderingContext2d,
    e: &Element,
    img: Option<&web_sys::HtmlImageElement>,
    dark: bool,
) {
    let (w, h) = (e.width(), e.height());
    let Some(img) = img else {
        ctx.set_fill_style_str(&color::themed("#E7E7E7", dark));
        ctx.fill_rect(0.0, 0.0, w, h);
        return;
    };
    if e.roundness().is_some() {
        ctx.begin_path();
        let _ = ctx.round_rect_with_f64(0.0, 0.0, w, h, shape::corner_radius(w.min(h), e));
        ctx.clip();
    }
    let scale = |i: usize| {
        e.get("scale")
            .and_then(|s| s.get(i))
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
    };
    let (sx, sy) = (scale(0), scale(1));
    if sx != 1.0 || sy != 1.0 {
        let _ = ctx.translate(w / 2.0, h / 2.0);
        let _ = ctx.scale(sx, sy);
        let _ = ctx.translate(-w / 2.0, -h / 2.0);
    }
    let crop = |k: &str| e.get("crop").and_then(|c| c.get(k)).and_then(Value::as_f64);
    let (cx, cy, cw, ch) = match (crop("x"), crop("y"), crop("width"), crop("height")) {
        (Some(x), Some(y), Some(cw), Some(ch)) => (x, y, cw, ch),
        _ => (
            0.0,
            0.0,
            img.natural_width() as f64,
            img.natural_height() as f64,
        ),
    };
    let _ = ctx.draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
        img, cx, cy, cw, ch, 0.0, 0.0, w, h,
    );
}

/// A picture being cropped: the whole of it faded, then the part the crop
/// keeps over it (`renderElement`'s cropping).
pub fn draw_cropping(
    ctx: &CanvasRenderingContext2d,
    e: &Element,
    img: &web_sys::HtmlImageElement,
    view: &Viewport,
    pixel_ratio: f64,
    dark: bool,
) {
    let s = pixel_ratio * view.zoom;
    let at = |e: &Element| {
        ctx.set_transform(s, 0.0, 0.0, s, view.scroll_x * s, view.scroll_y * s)
            .ok();
        let (cx, cy) = (e.x() + e.width() / 2.0, e.y() + e.height() / 2.0);
        let _ = ctx.translate(cx, cy);
        let _ = ctx.rotate(e.angle());
        let _ = ctx.translate(-cx, -cy);
        let _ = ctx.translate(e.x(), e.y());
    };
    let whole = notekpr_draw::edit::crop::uncropped(e);
    ctx.save();
    at(&whole);
    ctx.set_global_alpha(0.3);
    draw_image(ctx, &whole, Some(img), dark);
    ctx.restore();
    ctx.save();
    at(e);
    draw_image(ctx, e, Some(img), dark);
    ctx.restore();
}

/// How a drawing is drawn on a canvas.
pub struct Look<'a> {
    /// The canvas's pixels per CSS pixel.
    pub pixel_ratio: f64,
    pub dark: bool,
    /// The grid under it (not in exports).
    pub grid: Option<notekpr_draw::edit::snap::Grid>,
    /// The image a picture element shows, once loaded.
    pub pictures: &'a dyn Fn(&Element) -> Option<web_sys::HtmlImageElement>,
}

/// Draws the drawing in the view, as `look` says (as the editor shows
/// it, if one: frames' names being typed, what is dragged into frames).
pub fn draw_scene(
    ctx: &CanvasRenderingContext2d,
    scene: &Scene,
    view: &Viewport,
    look: &Look,
    cache: &mut ShapeCache,
    editor: Option<&notekpr_draw::edit::Editor>,
) {
    let (pixel_ratio, dark, grid, pictures) =
        (look.pixel_ratio, look.dark, look.grid, look.pictures);
    let background = scene.background().to_string();
    ctx.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0).ok();
    let (w, h) = (view.width * pixel_ratio, view.height * pixel_ratio);
    if color::is_transparent(&background) {
        ctx.clear_rect(0.0, 0.0, w, h);
    } else {
        ctx.set_fill_style_str(&color::themed(&background, dark));
        ctx.fill_rect(0.0, 0.0, w, h);
    }
    if let Some(g) = grid {
        super::snapping::draw_grid(ctx, g, view, pixel_ratio, dark);
    }
    let s = pixel_ratio * view.zoom;
    ctx.set_transform(s, 0.0, 0.0, s, view.scroll_x * s, view.scroll_y * s)
        .ok();
    let cfg = RenderConfig {
        dark,
        exporting: false,
        canvas_background: background,
    };
    let visible: Vec<&Element> = scene.visible().collect();
    let ids: HashSet<&str> = visible.iter().map(|e| e.id()).collect();
    cache.keep(&ids);
    let by_id: HashMap<&str, &Element> = visible.iter().map(|e| (e.id(), *e)).collect();
    let [vx1, vy1, vx2, vy2] = view.visible();
    let time = super::sticky::local_time();
    let renaming = editor.and_then(|ed| ed.editing_text.as_deref());
    let mut draw = |e: &Element| {
        let c = cache.get(e, &cfg);
        let [bx1, by1, bx2, by2] = c.bounds;
        // Off screen (with room for thick strokes).
        let pad = e.stroke_width() * 4.0;
        if bx2 + pad < vx1 || bx1 - pad > vx2 || by2 + pad < vy1 || by1 - pad > vy2 {
            return;
        }
        let [x1, y1, x2, y2, _, _] = c.coords;
        let cx = (x2 - x1) / 2.0 - (e.x() - x1);
        let cy = (y2 - y1) / 2.0 - (e.y() - y1);
        let shape = c.shape.clone();
        ctx.save();
        // What is in a frame is seen only in it.
        if let Some(f) = super::frames::clip_frame(e, &by_id, scene, editor) {
            super::frames::clip(ctx, f, view.zoom);
        }
        let _ = ctx.translate(e.x(), e.y());
        if e.angle() != 0.0 {
            let _ = ctx.translate(cx, cy);
            let _ = ctx.rotate(e.angle());
            let _ = ctx.translate(-cx, -cy);
        }
        ctx.set_global_alpha(super::frames::opacity(e, &by_id));
        ctx.set_line_join("round");
        ctx.set_line_cap("round");
        match &*shape {
            ElementShape::Rough(shapes) => shapes.iter().for_each(|d| draw_rough(ctx, d)),
            ElementShape::Freedraw { fill, outline } => {
                if let Some(f) = fill {
                    draw_rough(ctx, f);
                }
                if let Ok(p) = Path2d::new_with_path_string(outline) {
                    ctx.set_fill_style_str(&color::themed(e.stroke_color(), dark));
                    ctx.fill_with_path_2d(&p);
                }
            }
            ElementShape::None => match e.kind() {
                ElementType::Text => draw_text(ctx, e, dark),
                ElementType::Image => draw_image(ctx, e, pictures(e).as_ref(), dark),
                ElementType::Frame | ElementType::MagicFrame => {
                    draw_frame(ctx, e, dark, view.zoom, renaming != Some(e.id()))
                }
                ElementType::StickyNote => super::sticky::draw(ctx, e, dark, &time),
                _ => {}
            },
        }
        ctx.restore();
    };
    let iframe_like =
        |e: &Element| matches!(e.kind(), ElementType::Iframe | ElementType::Embeddable);
    for e in visible.iter().filter(|e| !iframe_like(e)) {
        // A label is drawn with what it labels.
        if e.kind() == ElementType::Text && e.container_id().is_some_and(|c| by_id.contains_key(c))
        {
            continue;
        }
        draw(e);
        let label = e
            .bound_elements()
            .iter()
            .filter(|b| b.kind == "text")
            .find_map(|b| by_id.get(b.id.as_str()).copied());
        if let Some(t) = label {
            draw(t);
        }
    }
    for e in visible.iter().filter(|e| iframe_like(e)) {
        draw(e);
    }
}

/// Excalidraw's selection colour.
const SELECTION: &str = "#6965db";

/// What editing shows over the drawing: the selection's outlines and
/// handles, the box being drawn to select.
/// The handles on a selected line's or arrow's points, and halfway along
/// its segments (`renderLinearPointHandles`).
fn point_handles(ctx: &CanvasRenderingContext2d, h: &notekpr_draw::edit::PointHandles, z: f64) {
    let size = notekpr_draw::edit::points::HANDLE;
    let radius = if h.editing { size } else { size / 2.0 };
    let _ = ctx.set_line_dash(&js_sys::Array::new());
    ctx.set_line_width(1.0 / z);
    ctx.set_stroke_style_str("#5e5ad8");
    let circle = |[x, y]: [f64; 2], r: f64, fill: &str, stroke: bool| {
        ctx.set_fill_style_str(fill);
        ctx.begin_path();
        let _ = ctx.arc(x, y, r / z, 0.0, std::f64::consts::TAU);
        ctx.fill();
        if stroke {
            ctx.stroke();
        }
    };
    let n = h.points.len();
    for (i, p) in h.points.iter().enumerate() {
        if h.ends_only && i != 0 && i + 1 != n {
            continue;
        }
        let fill = if h.selected.contains(&i) {
            "rgba(134, 131, 226, 0.9)"
        } else {
            "rgba(255, 255, 255, 0.9)"
        };
        circle(*p, radius, fill, true);
    }
    for m in &h.midpoints {
        circle(*m, size / 2.0, "rgba(177, 151, 252, 0.7)", false);
    }
}

/// The shape an arrow's dragged end will attach to, outlined
/// (`renderBindingHighlightForBindableElement`): faint while it attaches to
/// the outline, strong once it attaches inside.
fn binding_highlight(ctx: &CanvasRenderingContext2d, e: &Element, inside: bool, z: f64) {
    let offset = e.stroke_width() / 2.0;
    let (x, y, w, h) = (
        e.x() - offset,
        e.y() - offset,
        e.width() + offset * 2.0,
        e.height() + offset * 2.0,
    );
    ctx.save();
    let (cx, cy) = (e.x() + e.width() / 2.0, e.y() + e.height() / 2.0);
    let _ = ctx.translate(cx, cy);
    let _ = ctx.rotate(e.angle());
    let _ = ctx.translate(-cx, -cy);
    let _ = ctx.set_line_dash(&js_sys::Array::new());
    ctx.set_line_width((e.stroke_width() * 1.75).clamp(2.5, 4.0) / z.max(0.25));
    ctx.set_stroke_style_str(if inside {
        "rgba(106, 189, 252, 1)"
    } else {
        "rgba(106, 189, 252, 0.5)"
    });
    ctx.begin_path();
    match e.kind() {
        ElementType::Ellipse => {
            let _ = ctx.ellipse(
                x + w / 2.0,
                y + h / 2.0,
                w / 2.0,
                h / 2.0,
                0.0,
                0.0,
                std::f64::consts::TAU,
            );
        }
        ElementType::Diamond => {
            let [t, r, b, l] = shape::diamond_points(e);
            ctx.move_to(e.x() + t[0], e.y() + t[1] - offset);
            ctx.line_to(e.x() + r[0] + offset, e.y() + r[1]);
            ctx.line_to(e.x() + b[0], e.y() + b[1] + offset);
            ctx.line_to(e.x() + l[0] - offset, e.y() + l[1]);
            ctx.close_path();
        }
        _ => {
            let radius = if e.roundness().is_some() {
                shape::corner_radius(w.min(h), e)
            } else {
                0.0
            };
            let _ = ctx.round_rect_with_f64(x, y, w, h, radius);
        }
    }
    ctx.stroke();
    ctx.restore();
}

/// Where an element's link shows: a small button just past its top right
/// corner, the same size whatever the zoom (`getLinkHandleFromCoords`).
pub fn link_box(e: &Element, zoom: f64) -> [f64; 4] {
    let [_, y1, x2, _] = bounds::element_bounds(e);
    let size = 18.0 / zoom;
    // Clear of the selection's corner handle.
    [x2 + 10.0 / zoom, y1 - size - 10.0 / zoom, size, size]
}

/// The links of elements that have one, as buttons (in the drawing's
/// coordinates).
pub fn draw_links(ctx: &CanvasRenderingContext2d, scene: &Scene, zoom: f64) {
    ctx.save();
    let _ = ctx.set_line_dash(&js_sys::Array::new());
    for e in scene
        .visible()
        .filter(|e| e.link().is_some_and(|l| !l.is_empty()))
    {
        let [x, y, w, h] = link_box(e, zoom);
        ctx.set_fill_style_str("#ffffff");
        ctx.set_stroke_style_str(SELECTION);
        ctx.set_line_width(1.0 / zoom);
        ctx.begin_path();
        let _ = ctx.round_rect_with_f64(x, y, w, h, 4.0 / zoom);
        ctx.fill();
        ctx.stroke();
        // An arrow out of a box: it goes elsewhere.
        ctx.set_line_width(1.5 / zoom);
        ctx.set_line_cap("round");
        let (l, t, r, b) = (x + w * 0.28, y + h * 0.28, x + w * 0.72, y + h * 0.72);
        ctx.begin_path();
        ctx.move_to(l, t);
        ctx.line_to(r, t);
        ctx.line_to(r, b);
        ctx.move_to(r, t);
        ctx.line_to(x + w * 0.25, y + h * 0.75);
        ctx.stroke();
    }
    ctx.restore();
}

pub fn draw_overlay(
    ctx: &CanvasRenderingContext2d,
    scene: &Scene,
    ed: &notekpr_draw::edit::Editor,
    view: &Viewport,
    pixel_ratio: f64,
) {
    use notekpr_draw::edit::transform;
    let z = view.zoom;
    let s = pixel_ratio * z;
    ctx.set_transform(s, 0.0, 0.0, s, view.scroll_x * s, view.scroll_y * s)
        .ok();
    ctx.save();
    ctx.set_stroke_style_str(SELECTION);
    ctx.set_line_width(1.0 / z);
    let dashed = |on: bool| {
        let arr = js_sys::Array::new();
        if on {
            arr.push(&JsValue::from_f64(8.0 / z));
            arr.push(&JsValue::from_f64(4.0 / z));
        }
        let _ = ctx.set_line_dash(&arr);
    };
    let outline = |[x1, y1, x2, y2]: [f64; 4], angle: f64| {
        let pad = 4.0 / z;
        let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        ctx.save();
        let _ = ctx.translate(cx, cy);
        let _ = ctx.rotate(angle);
        ctx.stroke_rect(
            x1 - cx - pad,
            y1 - cy - pad,
            x2 - x1 + pad * 2.0,
            y2 - y1 + pad * 2.0,
        );
        ctx.restore();
    };
    let selected = ed.selected(scene);
    let editing = ed.editing_text.as_deref();
    // A line or arrow without a box of its own shows its points instead.
    let points_only =
        selected.len() == 1 && selected[0].kind().is_linear() && ed.frame(scene).is_none();
    for e in &selected {
        if Some(e.id()) == editing || points_only {
            continue;
        }
        let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(e);
        dashed(selected.len() > 1);
        outline([x1, y1, x2, y2], e.angle());
    }
    if let Some(f) = ed.frame(scene)
        && editing.is_none()
    {
        if selected.len() > 1 {
            dashed(true);
            outline(f.bounds, f.angle);
        }
        let linear_two_points = selected.len() == 1
            && selected[0].kind().is_linear()
            && selected[0].points().len() <= 2;
        if !ed.busy() && !linear_two_points {
            dashed(false);
            let c = f.center();
            let handles = transform::handles(&f, z)
                .into_iter()
                .filter(|(h, _)| ed.can_use(scene, *h));
            for (h, [x, y, w, hh]) in handles {
                ctx.save();
                let _ = ctx.translate(c[0], c[1]);
                let _ = ctx.rotate(f.angle);
                let _ = ctx.translate(-c[0], -c[1]);
                ctx.set_fill_style_str("#ffffff");
                ctx.begin_path();
                if h == transform::Handle::Rotation {
                    let _ = ctx.arc(
                        x + w / 2.0,
                        y + hh / 2.0,
                        w / 2.0,
                        0.0,
                        std::f64::consts::TAU,
                    );
                } else {
                    let _ = ctx.round_rect_with_f64(x, y, w, hh, 2.0 / z);
                }
                ctx.fill();
                ctx.stroke();
                ctx.restore();
            }
        }
    }
    if let Some(h) = ed.binding_target()
        && let Some(e) = scene.element(&h.id)
    {
        binding_highlight(ctx, e, h.inside, z);
    }
    if editing.is_none()
        && let Some(h) = ed.point_handles(scene, z)
    {
        point_handles(ctx, &h, z);
    }
    // An elbow arrow's segments are moved by the handles at their middles.
    if let [a] = selected.as_slice()
        && a.is_elbowed()
    {
        dashed(false);
        ctx.set_fill_style_str("#ffffff");
        for (_, [x, y]) in notekpr_draw::edit::elbow::segment_handles(a, z) {
            ctx.begin_path();
            let _ = ctx.arc(x, y, 4.0 / z, 0.0, std::f64::consts::TAU);
            ctx.fill();
            ctx.stroke();
        }
    }
    if let Some([x1, y1, x2, y2]) = ed.selection_box() {
        dashed(false);
        ctx.set_fill_style_str("rgba(105, 101, 219, 0.1)");
        let (x, y, w, h) = (x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs());
        ctx.fill_rect(x, y, w, h);
        ctx.stroke_rect(x, y, w, h);
    }
    super::frames::draw_highlights(ctx, scene, ed, z);
    // What the eraser will take: shown faded over.
    for id in ed.erasing() {
        if let Some(e) = scene.element(&id) {
            let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(e);
            ctx.set_fill_style_str("rgba(255, 255, 255, 0.6)");
            ctx.fill_rect(x1, y1, x2 - x1, y2 - y1);
        }
    }
    super::snapping::draw_snaps(ctx, ed.snap.lines(), z, crate::drawings::is_dark());
    ctx.restore();
}
