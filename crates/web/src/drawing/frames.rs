//! Frames on the canvas, as Excalidraw shows them: what is in a frame
//! clipped to it, the frame's name above it (the same size whatever the
//! zoom, cut with "…" to the frame's width), the box its name is typed in,
//! and where what is drawn or dragged will go, highlighted (the rules are
//! `notekpr_draw::edit::frame`'s).

use std::collections::HashMap;

use notekpr_draw::bounds;
use notekpr_draw::color;
use notekpr_draw::edit::Editor;
use notekpr_draw::edit::frame::{self, NAME_FONT_SIZE, NAME_LINE_HEIGHT, NAME_OFFSET_Y};
use notekpr_draw::fonts;
use notekpr_draw::scene::{Element, Scene};
use notekpr_draw::viewport::Viewport;
use web_sys::CanvasRenderingContext2d;

use super::editing::TextBox;

/// The room around a name being typed (`FRAME_NAME_EDIT_PADDING`).
const NAME_EDIT_PADDING: f64 = 6.0;

fn name_color(dark: bool) -> &'static str {
    if dark {
        frame::NAME_COLOR_DARK
    } else {
        frame::NAME_COLOR
    }
}

/// The frame an element is shown clipped to: its own (a label's: its
/// container's), or while it is dragged, the editor's say.
pub fn clip_frame<'a>(
    e: &'a Element,
    by_id: &HashMap<&str, &'a Element>,
    scene: &'a Scene,
    editor: Option<&Editor>,
) -> Option<&'a Element> {
    let owner = e
        .container_id()
        .and_then(|c| by_id.get(c).copied())
        .unwrap_or(e);
    if let Some(ed) = editor.filter(|ed| ed.dragging() && ed.is_selected(owner.id())) {
        return ed.clip_frame(scene, owner);
    }
    owner
        .frame_id()
        .and_then(|f| by_id.get(f).copied())
        .filter(|f| frame::is_frame(f))
}

/// What a frame's children are drawn with: their opacity and its.
pub fn opacity(e: &Element, by_id: &HashMap<&str, &Element>) -> f64 {
    let frame = e
        .frame_id()
        .and_then(|f| by_id.get(f))
        .filter(|f| frame::is_frame(f))
        .map_or(100.0, |f| f.opacity());
    (frame * e.opacity()) / 10000.0
}

/// Clips what follows to a frame (`frameClip`), in drawing coordinates.
pub fn clip(ctx: &CanvasRenderingContext2d, f: &Element, zoom: f64) {
    ctx.begin_path();
    let _ = ctx.round_rect_with_f64(f.x(), f.y(), f.width(), f.height(), frame::RADIUS / zoom);
    ctx.clip();
}

/// A frame's name above it, in the frame's own coordinates.
pub fn draw_name(ctx: &CanvasRenderingContext2d, f: &Element, dark: bool, zoom: f64) {
    ctx.set_font(&frame::name_font(NAME_FONT_SIZE / zoom));
    ctx.set_fill_style_str(name_color(dark));
    ctx.set_text_align("left");
    ctx.set_text_baseline("alphabetic");
    let width = |s: &str| ctx.measure_text(s).map_or(0.0, |m| m.width());
    let mut name = frame::title(f).to_string();
    if width(&name) > f.width() {
        let chars: Vec<char> = name.chars().collect();
        name = (0..chars.len())
            .rev()
            .map(|i| format!("{}…", chars[..i].iter().collect::<String>()))
            .find(|t| width(t) <= f.width())
            .unwrap_or_default();
    }
    // Its line ends this far above the frame; the baseline in it.
    let line = NAME_FONT_SIZE * NAME_LINE_HEIGHT;
    let baseline = fonts::baseline_offset(fonts::ASSISTANT, NAME_FONT_SIZE, line);
    let _ = ctx.fill_text(&name, 0.0, (baseline - line - NAME_OFFSET_Y) / zoom);
}

/// Where a frame's name is typed: over where it is shown.
pub fn name_box(f: &Element, view: &Viewport, dark: bool) -> TextBox {
    let [left, top] = view.to_screen([f.x(), f.y()]);
    let line = NAME_FONT_SIZE * NAME_LINE_HEIGHT;
    TextBox {
        left: left - NAME_EDIT_PADDING,
        top: top - NAME_OFFSET_Y - line - NAME_EDIT_PADDING,
        width: 0.0,
        height: line,
        font_size: NAME_FONT_SIZE,
        line_height: NAME_LINE_HEIGHT,
        font_family: fonts::css_family(fonts::ASSISTANT),
        color: name_color(dark).to_string(),
        align: "left".into(),
        text: frame::title(f).to_string(),
        angle: 0.0,
        frame_name: true,
    }
}

/// Where what is drawn or dragged goes: the frame, and what a frame being
/// drawn or resized will take in.
pub fn draw_highlights(ctx: &CanvasRenderingContext2d, scene: &Scene, ed: &Editor, zoom: f64) {
    let blue = color::themed(frame::HIGHLIGHT, crate::drawings::is_dark());
    ctx.save();
    ctx.set_stroke_style_str(&blue);
    let _ = ctx.set_line_dash(&js_sys::Array::new());
    if let Some(f) = ed.frame_to_highlight().and_then(|id| scene.element(id)) {
        let [x1, y1, x2, y2, cx, cy] = bounds::absolute_coords(f);
        ctx.save();
        ctx.set_line_width(frame::STROKE_WIDTH / zoom);
        let _ = ctx.translate(cx, cy);
        let _ = ctx.rotate(f.angle());
        ctx.begin_path();
        let _ = ctx.round_rect_with_f64(x1 - cx, y1 - cy, x2 - x1, y2 - y1, frame::RADIUS / zoom);
        ctx.stroke();
        ctx.restore();
    }
    ctx.set_line_width(1.0 / zoom);
    let pad = 4.0 / zoom;
    for e in ed.frame_highlights(scene) {
        let [x1, y1, x2, y2] = bounds::element_bounds(e);
        ctx.stroke_rect(x1 - pad, y1 - pad, x2 - x1 + pad * 2.0, y2 - y1 + pad * 2.0);
    }
    ctx.restore();
}
