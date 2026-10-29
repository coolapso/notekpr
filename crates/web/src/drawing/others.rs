//! The others drawing along, as Excalidraw shows collaborators: their
//! pointers with their names, what they selected outlined in their colour,
//! and the laser pointer's fading trail — theirs and one's own
//! (`laserTrails.ts`, `animatedTrail.ts`; MIT, see `crates/draw/PORTED.md`).
//! It all comes with presence (encrypted like it), never through the
//! drawing itself.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use notekpr_core::engine::DrawingPointer;
use notekpr_draw::bounds;
use notekpr_draw::freehand::laser::{LaserPointer, LaserPointerOptions, SizeMappingDetails};
use notekpr_draw::scene::Scene;
use notekpr_draw::viewport::Viewport;
use web_sys::CanvasRenderingContext2d;

use super::Board;

/// Someone else on the drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct Other {
    /// Their connection (one person may be there twice).
    pub conn: u64,
    pub name: String,
    pub color: String,
    pub pointer: Option<DrawingPointer>,
    pub selected: Vec<String>,
}

/// The laser's colour (`DEFAULT_LASER_COLOR`).
pub const LASER_COLOR: &str = "red";

fn now() -> f64 {
    js_sys::Date::now()
}

/// A point's size as the trail fades: after a second, and towards its
/// tail (`LaserTrails.getTrailOptions`). The points' pressure is the time
/// they were added.
fn fading(c: SizeMappingDetails) -> f64 {
    const DECAY_TIME: f64 = 1000.0;
    const DECAY_LENGTH: f64 = 50.0;
    let ease_out = |k: f64| 1.0 - (1.0 - k).powi(3);
    let t = (1.0 - (now() - c.pressure) / DECAY_TIME).max(0.0);
    let left = (c.total_length - c.current_index) as f64;
    let l = (DECAY_LENGTH - DECAY_LENGTH.min(left)) / DECAY_LENGTH;
    ease_out(l).min(ease_out(t))
}

fn options() -> LaserPointerOptions {
    LaserPointerOptions {
        simplify: 0.0,
        streamline: 0.4,
        size_mapping: fading,
        ..LaserPointerOptions::default()
    }
}

/// One person's laser: the stroke being drawn and those fading
/// (`AnimatedTrail`).
#[derive(Default)]
pub struct Trail {
    current: Option<LaserPointer>,
    past: Vec<LaserPointer>,
}

impl Trail {
    pub fn start(&mut self, [x, y]: [f64; 2]) {
        self.end();
        let mut l = LaserPointer::new(options());
        l.add_point([x, y, now()]);
        self.current = Some(l);
    }

    pub fn add(&mut self, [x, y]: [f64; 2]) {
        if let Some(l) = self.current.as_mut() {
            l.add_point([x, y, now()]);
        }
    }

    pub fn end(&mut self) {
        if let Some(mut l) = self.current.take() {
            l.close();
            l.options.keep_head = false;
            self.past.push(l);
        }
    }

    pub fn drawing(&self) -> bool {
        self.current.is_some()
    }

    /// The outlines to fill now (in the drawing), dropping strokes that
    /// faded away; empty when there's nothing left.
    fn outlines(&mut self, zoom: f64) -> Vec<Vec<[f64; 2]>> {
        let size = options().size / zoom;
        let outline = |l: &LaserPointer| -> Vec<[f64; 2]> {
            l.get_stroke_outline(Some(size))
                .iter()
                .map(|p| [p[0], p[1]])
                .collect()
        };
        self.past.retain(|l| !outline(l).is_empty());
        self.past
            .iter()
            .chain(self.current.iter())
            .map(outline)
            .filter(|o| !o.is_empty())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_none() && self.past.is_empty()
    }
}

/// The lasers on a board: one's own and the others', by connection.
#[derive(Default)]
pub struct Trails {
    pub own: Trail,
    pub theirs: HashMap<u64, (String, Trail)>,
}

impl Trails {
    /// Follows the others' pointers: a laser held down draws.
    pub fn follow(&mut self, others: &[Other]) {
        self.theirs
            .retain(|conn, _| others.iter().any(|o| o.conn == *conn));
        for o in others {
            let (_, t) = self
                .theirs
                .entry(o.conn)
                .or_insert_with(|| (o.color.clone(), Trail::default()));
            match o.pointer {
                Some(p) if p.laser && p.down => {
                    if t.drawing() {
                        t.add([p.x, p.y]);
                    } else {
                        t.start([p.x, p.y]);
                    }
                }
                _ => t.end(),
            }
        }
    }

    /// Whether some trail is still there (the board keeps drawing frames).
    pub fn active(&self) -> bool {
        !self.own.is_empty() || self.theirs.values().any(|(_, t)| !t.is_empty())
    }
}

fn fill_outline(ctx: &CanvasRenderingContext2d, outline: &[[f64; 2]], color: &str) {
    let Some(first) = outline.first() else {
        return;
    };
    ctx.set_fill_style_str(color);
    ctx.begin_path();
    ctx.move_to(first[0], first[1]);
    for p in &outline[1..] {
        ctx.line_to(p[0], p[1]);
    }
    ctx.close_path();
    ctx.fill();
}

/// Draws the lasers (in the drawing's coordinates: the context is set to
/// the view).
pub fn draw_trails(ctx: &CanvasRenderingContext2d, trails: &mut Trails, zoom: f64) {
    for o in trails.own.outlines(zoom) {
        fill_outline(ctx, &o, LASER_COLOR);
    }
    for (color, t) in trails.theirs.values_mut() {
        for o in t.outlines(zoom) {
            fill_outline(ctx, &o, color);
        }
    }
}

/// Draws what the others selected, outlined in their colour (in the
/// drawing's coordinates).
pub fn draw_selections(ctx: &CanvasRenderingContext2d, scene: &Scene, others: &[Other], zoom: f64) {
    ctx.save();
    ctx.set_line_width(1.0 / zoom);
    let dash = js_sys::Array::new();
    dash.push(&(4.0 / zoom).into());
    dash.push(&(4.0 / zoom).into());
    let _ = ctx.set_line_dash(&dash);
    let pad = 6.0 / zoom;
    for o in others {
        ctx.set_stroke_style_str(&o.color);
        for e in o.selected.iter().filter_map(|id| scene.element(id)) {
            if e.is_deleted() {
                continue;
            }
            let [x1, y1, x2, y2, cx, cy] = bounds::absolute_coords(e);
            ctx.save();
            let _ = ctx.translate(cx, cy);
            let _ = ctx.rotate(e.angle());
            ctx.stroke_rect(
                x1 - cx - pad,
                y1 - cy - pad,
                x2 - x1 + pad * 2.0,
                y2 - y1 + pad * 2.0,
            );
            ctx.restore();
        }
    }
    ctx.restore();
}

/// Draws the others' pointers with their names (in screen pixels: the
/// context is unscaled but for `pixel_ratio`), as Excalidraw's
/// `renderRemoteCursors`; one out of view sits at its edge, faded.
pub fn draw_pointers(
    ctx: &CanvasRenderingContext2d,
    others: &[Other],
    view: &Viewport,
    pixel_ratio: f64,
) {
    ctx.save();
    ctx.set_transform(pixel_ratio, 0.0, 0.0, pixel_ratio, 0.0, 0.0)
        .ok();
    let _ = ctx.set_line_dash(&js_sys::Array::new());
    for o in others {
        let Some(p) = o.pointer else { continue };
        let [sx, sy] = view.to_screen([p.x, p.y]);
        let (x, y) = (
            sx.clamp(0.0, (view.width - 12.0).max(0.0)),
            sy.clamp(0.0, (view.height - 16.0).max(0.0)),
        );
        let outside = x != sx || y != sy;
        ctx.set_global_alpha(if outside { 0.6 } else { 1.0 });
        // The pointer.
        ctx.set_fill_style_str(&o.color);
        ctx.set_stroke_style_str("#ffffff");
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(x, y);
        ctx.line_to(x - 1.0, y + 14.0);
        ctx.line_to(x + 3.0, y + 9.0);
        ctx.line_to(x + 9.0, y + 10.0);
        ctx.close_path();
        ctx.fill();
        ctx.stroke();
        // The name, on a tag of their colour.
        if !outside {
            ctx.set_font("600 12px sans-serif");
            let w = ctx.measure_text(&o.name).map(|m| m.width()).unwrap_or(40.0);
            let (tx, ty) = (x + 9.0, y + 14.0);
            ctx.begin_path();
            let _ = ctx.round_rect_with_f64(tx, ty, w + 10.0, 18.0, 6.0);
            ctx.fill();
            ctx.set_fill_style_str("#ffffff");
            ctx.set_text_baseline("middle");
            ctx.set_text_align("left");
            let _ = ctx.fill_text(&o.name, tx + 5.0, ty + 9.0);
        }
    }
    ctx.restore();
}

thread_local! {
    /// The boards open now.
    static OPEN: RefCell<Vec<Weak<Board>>> = const { RefCell::new(Vec::new()) };
}

/// A board is open (presence reaches it).
pub fn register(board: &Rc<Board>) {
    OPEN.with(|o| {
        let mut o = o.borrow_mut();
        o.retain(|w| w.upgrade().is_some());
        o.push(Rc::downgrade(board));
    });
}

/// Who else is on the drawing `id` of `vault` now (from presence).
pub fn show(vault: &str, id: &str, others: Vec<Other>) {
    let boards: Vec<Rc<Board>> =
        OPEN.with(|o| o.borrow().iter().filter_map(Weak::upgrade).collect());
    for b in boards {
        if b.vault == vault && b.id == id {
            b.set_others(others.clone());
        }
    }
}
