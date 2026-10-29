//! The grid under a drawing and the lines over it that show what snapped,
//! drawn as Excalidraw draws them (`staticScene.ts`'s `strokeGrid`,
//! `renderSnaps.ts`; MIT, see `crates/draw/PORTED.md`); and the board's
//! switches for both. The grid is the drawing's: everyone drawing it sees
//! it. Snapping to other elements is each person's own, kept in this
//! browser (as Excalidraw keeps it).

use std::rc::Rc;

use leptos::prelude::{UpdateUntracked, WithUntracked};
use notekpr_draw::color;
use notekpr_draw::edit::snap::{Grid, SnapLine};
use notekpr_draw::geom::Point;
use notekpr_draw::js;
use notekpr_draw::viewport::Viewport;
use wasm_bindgen::JsValue;
use web_sys::CanvasRenderingContext2d;

use super::Board;
use crate::state::{self, step, ui};

/// The grid's colours (`GridLineColor`): every `step`th line, the others.
const GRID_BOLD: &str = "#dddddd";
const GRID_REGULAR: &str = "#e5e5e5";

/// What snapped (`SNAP_COLOR_LIGHT`, `SNAP_COLOR_DARK`).
const SNAP_LIGHT: &str = "#ff6b6b";
const SNAP_DARK: &str = "#ff9090";

fn dash(ctx: &CanvasRenderingContext2d, dash: &[f64]) {
    let arr = js_sys::Array::new();
    for d in dash {
        arr.push(&JsValue::from_f64(*d));
    }
    let _ = ctx.set_line_dash(&arr);
}

/// `strokeGrid`: the grid in view, the canvas having `pixel_ratio` pixels a
/// CSS pixel. Lines a device pixel wide or more cover whole pixels.
pub fn draw_grid(
    ctx: &CanvasRenderingContext2d,
    grid: Grid,
    view: &Viewport,
    pixel_ratio: f64,
    dark: bool,
) {
    let Grid { size, step } = grid;
    let zoom = view.zoom;
    // In the drawing's units, shifted by the scroll, as Excalidraw draws it.
    ctx.set_transform(pixel_ratio * zoom, 0.0, 0.0, pixel_ratio * zoom, 0.0, 0.0)
        .ok();
    let (scroll_x, scroll_y) = (view.scroll_x, view.scroll_y);
    let (width, height) = (view.width / zoom, view.height / zoom);
    let offset_x = (scroll_x % size) - size;
    let offset_y = (scroll_y % size) - size;
    let on_screen = size * zoom;
    let space = 1.0 / zoom;
    let device = zoom * pixel_ratio;
    // Where a line goes and how wide it is, at most `max` CSS pixels.
    let snap = |position: f64, max: f64| -> (f64, f64) {
        let wide = js::min(pixel_ratio, max * device);
        if wide < 1.0 {
            return (position, wide / device);
        }
        let whole = js::round(wide);
        let center = if whole % 2.0 != 0.0 { 0.5 } else { 0.0 };
        (
            (js::round(position * device - center) + center) / device,
            whole / device,
        )
    };
    let (bold, regular) = (
        color::themed(GRID_BOLD, dark),
        color::themed(GRID_REGULAR, dark),
    );
    // One line across (`vertical`: down) at `at`, if it is drawn.
    let line = |vertical: bool, at: f64| {
        let scroll = if vertical { scroll_x } else { scroll_y };
        let is_bold = step > 1.0 && js::round(at - scroll) % (step * size) == 0.0;
        // Zoomed far out, only the bold ones.
        if !is_bold && on_screen < 10.0 {
            return;
        }
        let (position, line_width) = snap(at, if is_bold { 4.0 } else { 1.0 });
        ctx.set_line_width(line_width);
        if is_bold {
            dash(ctx, &[]);
        } else {
            dash(ctx, &[line_width * 3.0, space + (line_width + space)]);
        }
        ctx.begin_path();
        ctx.set_stroke_style_str(if is_bold { &bold } else { &regular });
        if vertical {
            ctx.move_to(position, offset_y - size);
            ctx.line_to(position, (offset_y + height + size * 2.0).ceil());
        } else {
            ctx.move_to(offset_x - size, position);
            ctx.line_to((offset_x + width + size * 2.0).ceil(), position);
        }
        ctx.stroke();
    };
    ctx.save();
    let mut x = offset_x;
    while x < offset_x + width + size * 2.0 {
        line(true, x);
        x += size;
    }
    let mut y = offset_y;
    while y < offset_y + height + size * 2.0 {
        line(false, y);
        y += size;
    }
    ctx.restore();
}

/// `renderSnaps`: the lines showing what snapped, `ctx` drawing in the
/// drawing's coordinates at `zoom`.
pub fn draw_snaps(ctx: &CanvasRenderingContext2d, lines: &[SnapLine], zoom: f64, dark: bool) {
    if lines.is_empty() {
        return;
    }
    let segment = |[x1, y1]: Point, [x2, y2]: Point| {
        ctx.begin_path();
        ctx.line_to(x1, y1);
        ctx.line_to(x2, y2);
        ctx.stroke();
    };
    let cross = |[x, y]: Point| {
        let s = 2.0 / zoom;
        ctx.begin_path();
        ctx.move_to(x - s, y - s);
        ctx.line_to(x + s, y + s);
        ctx.move_to(x + s, y - s);
        ctx.line_to(x - s, y + s);
        ctx.stroke();
    };
    // `drawGapLine`: |–––––||–––––| — ends, the line, a mark in its middle.
    let gap = |from: Point, to: Point, horizontal: bool| {
        let full = 8.0 / zoom;
        let (half, quarter) = (full / 2.0, full / 4.0);
        if horizontal {
            let mid = [(from[0] + to[0]) / 2.0, from[1]];
            segment([from[0], from[1] - full], [from[0], from[1] + full]);
            for dx in [-quarter, quarter] {
                segment([mid[0] + dx, mid[1] - half], [mid[0] + dx, mid[1] + half]);
            }
            segment([to[0], to[1] - full], [to[0], to[1] + full]);
        } else {
            let mid = [from[0], (from[1] + to[1]) / 2.0];
            segment([from[0] - full, from[1]], [from[0] + full, from[1]]);
            for dy in [-quarter, quarter] {
                segment([mid[0] - half, mid[1] + dy], [mid[0] + half, mid[1] + dy]);
            }
            segment([to[0] - full, to[1]], [to[0] + full, to[1]]);
        }
        segment(from, to);
    };
    ctx.save();
    ctx.set_stroke_style_str(if dark { SNAP_DARK } else { SNAP_LIGHT });
    ctx.set_line_width(1.0 / zoom);
    dash(ctx, &[]);
    for l in lines {
        match l {
            SnapLine::Points(points) => {
                if let (Some(first), Some(last)) = (points.first(), points.last()) {
                    segment(*first, *last);
                }
                points.iter().for_each(|p| cross(*p));
            }
            SnapLine::Pointer([corner, pointer]) => {
                cross(*corner);
                segment(*corner, *pointer);
            }
            SnapLine::Gap {
                from,
                to,
                horizontal,
            } => gap(*from, *to, *horizontal),
        }
    }
    ctx.restore();
}

/// Snapping to other elements as last chosen in this browser.
pub fn remembered() -> bool {
    ui().ui_prefs.with_untracked(|p| p.drawing_snap)
}

impl Board {
    /// Whether the drawing's grid is on.
    pub fn grid_on(&self) -> bool {
        Grid::of(&self.scene()).is_some()
    }

    /// Turns the drawing's grid on or off (Ctrl+'), for everyone drawing
    /// it. Turning it on turns one's snapping to elements off, as in
    /// Excalidraw.
    pub fn toggle_grid(self: &Rc<Self>) {
        if !self.writable() {
            return;
        }
        let on = !self.grid_on();
        let (v, id) = (self.vault.clone(), self.id.clone());
        step(|c| {
            let Some(d) = c.replicas.get_mut(&v).and_then(|r| r.notes.get_mut(&id)) else {
                return;
            };
            // A step of its own to undo.
            d.undo_boundary();
            let u = d.drawing().set_settings(&Grid::settings(on));
            d.undo_boundary();
            if let Some(u) = u {
                state::local_update(c, &v, &id, u);
            }
        });
        if on {
            self.set_snapping(false);
        }
        self.reread();
        self.schedule();
        self.changed();
    }

    /// Whether one snaps to other elements.
    pub fn snapping(&self) -> bool {
        self.editor.borrow().snap.to_objects
    }

    /// Snaps to other elements, or not (Alt+S): one's own choice, kept in
    /// this browser. The drawing's grid stays as it is: it is everyone's.
    pub fn set_snapping(&self, on: bool) {
        self.editor.borrow_mut().snap.to_objects = on;
        // Nothing shown depends on it: kept without a word to the pages
        // (they would read the drawing again).
        ui().ui_prefs.update_untracked(|p| {
            if p.drawing_snap != on {
                p.drawing_snap = on;
                crate::storage::set_ui_prefs(p);
            }
        });
        self.changed();
    }
}
