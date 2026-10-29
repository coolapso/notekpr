//! Sticky notes on the canvas, as Excalidraw's `renderElement.ts` draws
//! them: the shadow, the paper, a faint shadow along its edge, and the day
//! it was made in its footer (the shapes are `notekpr_draw::sticky`'s).

use notekpr_draw::color;
use notekpr_draw::js::number_to_string;
use notekpr_draw::scene::Element;
use notekpr_draw::sticky::{self, LocalTime, PathCommand};
use wasm_bindgen::JsValue;
use web_sys::CanvasRenderingContext2d;

/// The viewer's clock, for the dates on notes.
pub fn local_time() -> LocalTime {
    LocalTime {
        now: js_sys::Date::now(),
        utc_offset: |ms| -js_sys::Date::new(&JsValue::from_f64(ms)).get_timezone_offset(),
    }
}

fn trace(ctx: &CanvasRenderingContext2d, commands: &[PathCommand]) {
    ctx.begin_path();
    for c in commands {
        match *c {
            PathCommand::Move([x, y]) => ctx.move_to(x, y),
            PathCommand::Line([x, y]) => ctx.line_to(x, y),
            PathCommand::Quadratic {
                control: [cx, cy],
                to: [x, y],
            } => ctx.quadratic_curve_to(cx, cy, x, y),
        }
    }
    ctx.close_path();
}

fn black(opacity: f64) -> String {
    format!("rgba(0, 0, 0, {})", number_to_string(opacity))
}

/// Draws a note in its own coordinates (moved and turned already).
pub fn draw(ctx: &CanvasRenderingContext2d, e: &Element, dark: bool, time: &LocalTime) {
    ctx.save();
    ctx.set_fill_style_str(&black(sticky::SHADOW_OPACITY));
    trace(ctx, &sticky::path_commands(e, true));
    ctx.fill();
    let paper = sticky::path_commands(e, false);
    ctx.set_fill_style_str(&color::themed(e.background_color(), dark));
    trace(ctx, &paper);
    ctx.fill();
    // The edge's shadow falls inside the paper only.
    ctx.save();
    trace(ctx, &paper);
    ctx.clip();
    ctx.set_line_width(sticky::EDGE_SHADOW_WIDTH * 2.0);
    ctx.set_stroke_style_str(&black(sticky::EDGE_SHADOW_OPACITY));
    trace(ctx, &paper);
    ctx.stroke();
    ctx.restore();
    if let Some(f) = sticky::footer(e, time) {
        ctx.set_font(&format!(
            "{}px {}",
            number_to_string(sticky::FOOTER_FONT_SIZE),
            sticky::FOOTER_FONT
        ));
        ctx.set_text_align("right");
        ctx.set_text_baseline("alphabetic");
        ctx.set_fill_style_str(&color::themed(e.stroke_color(), dark));
        ctx.set_global_alpha(ctx.global_alpha() * sticky::FOOTER_OPACITY);
        let _ = ctx.fill_text(&f.text, f.x, f.y);
    }
    ctx.restore();
}
