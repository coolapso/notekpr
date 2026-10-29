//! Resizing and turning what is selected, with Excalidraw's handles
//! (`transformHandles.ts`, `resizeElements.ts`; MIT, see `PORTED.md`): a
//! square at each corner, one above for turning, and the edges themselves
//! for resizing one side.

use serde_json::{Value, json};

use crate::bounds;
use crate::geom::{self, Point};
use crate::scene::{Element, ElementType, num};

/// A handle's size on screen (mouse) and its gaps (`transformHandleSizes`,
/// `ROTATION_RESIZE_HANDLE_GAP`, margins).
pub const HANDLE_SIZE: f64 = 8.0;
const ROTATION_GAP: f64 = 16.0;
const MARGIN: f64 = 4.0;
const SPACING: f64 = 2.0;
/// How near an edge resizes its side (`SIDE_RESIZING_THRESHOLD`).
const SIDE_THRESHOLD: f64 = 2.0 * SPACING;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    N,
    S,
    E,
    W,
    NW,
    NE,
    SW,
    SE,
    Rotation,
}

impl Handle {
    /// The CSS cursor over it, for a box turned by `angle`.
    pub fn cursor(self, angle: f64) -> &'static str {
        if self == Handle::Rotation {
            return "grab";
        }
        const CURSORS: [&str; 4] = ["ns-resize", "nesw-resize", "ew-resize", "nwse-resize"];
        let base = match self {
            Handle::N | Handle::S => 0,
            Handle::NE | Handle::SW => 1,
            Handle::E | Handle::W => 2,
            _ => 3,
        };
        let steps = ((angle / (std::f64::consts::PI / 4.0)).round() as i64).rem_euclid(4) as usize;
        CURSORS[(base + steps) % 4]
    }
}

/// The box handles go around: `[x1, y1, x2, y2]` unrotated, its centre, and
/// how far it is turned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub bounds: [f64; 4],
    pub angle: f64,
}

impl Frame {
    pub fn center(&self) -> Point {
        let [x1, y1, x2, y2] = self.bounds;
        [(x1 + x2) / 2.0, (y1 + y2) / 2.0]
    }
}

/// The frame of a selection: one element's own box (turned with it), or the
/// box around several.
pub fn frame(selected: &[&Element]) -> Option<Frame> {
    match selected {
        [] => None,
        [one] => {
            let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(one);
            Some(Frame {
                bounds: [x1, y1, x2, y2],
                angle: one.angle(),
            })
        }
        many => Some(Frame {
            bounds: bounds::common_bounds(many.iter().copied()),
            angle: 0.0,
        }),
    }
}

/// The handles' squares (`[x, y, w, h]` before turning, and their centres
/// turned), at `zoom`. Lines of two points, and locked selections, have
/// none.
pub fn handles(f: &Frame, zoom: f64) -> Vec<(Handle, [f64; 4])> {
    let size = HANDLE_SIZE / zoom;
    let margin = MARGIN / zoom;
    let off = (HANDLE_SIZE - SPACING * 2.0) / (2.0 * zoom);
    let [x1, y1, x2, y2] = f.bounds;
    let w = x2 - x1;
    vec![
        (
            Handle::NW,
            [
                x1 - margin - size + off,
                y1 - margin - size + off,
                size,
                size,
            ],
        ),
        (
            Handle::NE,
            [x2 + margin - off, y1 - margin - size + off, size, size],
        ),
        (
            Handle::SW,
            [x1 - margin - size + off, y2 + margin - off, size, size],
        ),
        (
            Handle::SE,
            [x2 + margin - off, y2 + margin - off, size, size],
        ),
        (
            Handle::Rotation,
            [
                x1 + w / 2.0 - size / 2.0,
                y1 - margin - size + off - ROTATION_GAP / zoom,
                size,
                size,
            ],
        ),
    ]
}

/// The handle at `p`, if any: a corner or the turning one, else an edge.
pub fn handle_at(f: &Frame, p: Point, zoom: f64) -> Option<Handle> {
    let center = f.center();
    // In the frame's own (unturned) space.
    let [px, py] = geom::rotate(p, center, -f.angle);
    let slack = 4.0 / zoom;
    for (h, [x, y, w, hh]) in handles(f, zoom) {
        if px >= x - slack && px <= x + w + slack && py >= y - slack && py <= y + hh + slack {
            return Some(h);
        }
    }
    let [x1, y1, x2, y2] = f.bounds;
    let t = (SIDE_THRESHOLD + MARGIN) / zoom;
    let within_x = px > x1 && px < x2;
    let within_y = py > y1 && py < y2;
    if within_x && (py - y1).abs() <= t {
        return Some(Handle::N);
    }
    if within_x && (py - y2).abs() <= t {
        return Some(Handle::S);
    }
    if within_y && (px - x1).abs() <= t {
        return Some(Handle::W);
    }
    if within_y && (px - x2).abs() <= t {
        return Some(Handle::E);
    }
    None
}

/// How to resize: keep the proportions (Shift), resize around the centre
/// (Alt).
#[derive(Clone, Copy, Debug, Default)]
pub struct ResizeMode {
    pub keep_ratio: bool,
    pub from_center: bool,
}

/// The new box (in the frame's own space) when `handle` is dragged to `p`;
/// past its other side it is flipped (`x2 < x1`, or `y2 < y1`).
pub fn resized_box(f: &Frame, handle: Handle, p: Point, mode: ResizeMode) -> [f64; 4] {
    let center = f.center();
    let [px, py] = geom::rotate(p, center, -f.angle);
    let [mut x1, mut y1, mut x2, mut y2] = f.bounds;
    let (w0, h0) = (x2 - x1, y2 - y1);
    let (east, west, north, south) = match handle {
        Handle::E => (true, false, false, false),
        Handle::W => (false, true, false, false),
        Handle::N => (false, false, true, false),
        Handle::S => (false, false, false, true),
        Handle::NE => (true, false, true, false),
        Handle::NW => (false, true, true, false),
        Handle::SE => (true, false, false, true),
        Handle::SW => (false, true, false, true),
        Handle::Rotation => return f.bounds,
    };
    if east {
        x2 = px;
    }
    if west {
        x1 = px;
    }
    if north {
        y1 = py;
    }
    if south {
        y2 = py;
    }
    if mode.keep_ratio && w0 != 0.0 && h0 != 0.0 {
        let (sx, sy) = ((x2 - x1) / w0, (y2 - y1) / h0);
        let corner = (east || west) && (north || south);
        let (nw, nh) = if corner {
            // The larger change wins; each side keeps its direction.
            let s = sx.abs().max(sy.abs());
            (w0 * s * sx.signum(), h0 * s * sy.signum())
        } else if east || west {
            (w0 * sx, h0 * sx.abs())
        } else {
            (w0 * sy.abs(), h0 * sy)
        };
        if west {
            x1 = x2 - nw;
        } else {
            x2 = x1 + nw;
        }
        if north {
            y1 = y2 - nh;
        } else {
            y2 = y1 + nh;
        }
        // Dragging a side keeps the box centred across it.
        if !corner && (east || west) {
            let cy = (f.bounds[1] + f.bounds[3]) / 2.0;
            y1 = cy - nh / 2.0;
            y2 = cy + nh / 2.0;
        }
        if !corner && (north || south) {
            let cx = (f.bounds[0] + f.bounds[2]) / 2.0;
            x1 = cx - nw / 2.0;
            x2 = cx + nw / 2.0;
        }
    }
    if mode.from_center {
        let [cx, cy] = center;
        if east {
            x1 = 2.0 * cx - x2;
        }
        if west {
            x2 = 2.0 * cx - x1;
        }
        if south {
            y1 = 2.0 * cy - y2;
        }
        if north {
            y2 = 2.0 * cy - y1;
        }
    }
    [x1, y1, x2, y2]
}

fn bump_number(e: &mut Element, key: &str, x: f64) {
    e.set(key, num(x));
}

/// `originals` resized as the frame `f` is when `handle` goes to `p`; each
/// keeps its place in the frame. A box dragged past its other side flips.
pub fn resize(
    originals: &[Element],
    f: &Frame,
    handle: Handle,
    p: Point,
    mode: ResizeMode,
) -> Vec<Element> {
    let [ox1, oy1, ox2, oy2] = f.bounds;
    let (w0, h0) = (ox2 - ox1, oy2 - oy1);
    let [nx1, ny1, nx2, ny2] = resized_box(f, handle, p, mode);
    let (sx, sy) = (
        if w0 != 0.0 { (nx2 - nx1) / w0 } else { 1.0 },
        if h0 != 0.0 { (ny2 - ny1) / h0 } else { 1.0 },
    );
    // Where the new box sits, turned back: its unturned top left moves with
    // the centre.
    let old_center = f.center();
    let new_center_local = [(nx1 + nx2) / 2.0, (ny1 + ny2) / 2.0];
    let new_center = geom::rotate(new_center_local, old_center, f.angle);
    let shift = [
        new_center[0] - new_center_local[0],
        new_center[1] - new_center_local[1],
    ];
    originals
        .iter()
        .map(|o| {
            let mut e = o.clone();
            let [ex1, ey1, ex2, ey2, _, _] = bounds::absolute_coords(o);
            let (ew, eh) = (ex2 - ex1, ey2 - ey1);
            // The element's box in the new frame (sizes may go negative:
            // flipped).
            let mut x1 = nx1 + (ex1 - ox1) * sx;
            let mut y1 = ny1 + (ey1 - oy1) * sy;
            let (mut w, mut h) = (ew * sx, eh * sy);
            if w < 0.0 {
                x1 += w;
                w = -w;
            }
            if h < 0.0 {
                y1 += h;
                h = -h;
            }
            x1 += shift[0];
            y1 += shift[1];
            match o.kind() {
                ElementType::Line | ElementType::Arrow | ElementType::Freedraw => {
                    // Points scale; the element moves by its box's corner.
                    let pts = o.points();
                    let scaled: Vec<Value> = pts
                        .iter()
                        .map(|[px, py]| json!([num(px * sx), num(py * sy)]))
                        .collect();
                    let min_x = pts.iter().map(|p| p[0] * sx).fold(f64::INFINITY, f64::min);
                    let min_y = pts.iter().map(|p| p[1] * sy).fold(f64::INFINITY, f64::min);
                    e.set("points", Value::Array(scaled));
                    bump_number(&mut e, "x", x1 - min_x);
                    bump_number(&mut e, "y", y1 - min_y);
                    bump_number(&mut e, "width", w);
                    bump_number(&mut e, "height", h);
                }
                ElementType::Text => {
                    // Text keeps its proportions: the font grows with it.
                    let s = (h / eh.max(1e-9)).abs();
                    bump_number(&mut e, "fontSize", (o.font_size() * s).max(1.0));
                    bump_number(&mut e, "x", x1);
                    bump_number(&mut e, "y", y1);
                    bump_number(&mut e, "width", ew * s);
                    bump_number(&mut e, "height", eh * s);
                }
                _ => {
                    bump_number(&mut e, "x", x1);
                    bump_number(&mut e, "y", y1);
                    bump_number(&mut e, "width", w);
                    bump_number(&mut e, "height", h);
                }
            }
            // An image dragged past its side is mirrored.
            if o.kind() == ElementType::Image {
                let (fx, fy) = (
                    if sx < 0.0 { -1.0 } else { 1.0 },
                    if sy < 0.0 { -1.0 } else { 1.0 },
                );
                let scale = o.get("scale").and_then(Value::as_array).map(|s| {
                    (
                        s.first().and_then(Value::as_f64).unwrap_or(1.0),
                        s.get(1).and_then(Value::as_f64).unwrap_or(1.0),
                    )
                });
                let (a, b) = scale.unwrap_or((1.0, 1.0));
                e.set("scale", json!([num(a * fx), num(b * fy)]));
            }
            e
        })
        .collect()
}

/// `originals` turned around the frame's centre so the turning handle
/// points at `p` (`start`: where the drag began); Shift turns in 15° steps.
pub fn rotate(
    originals: &[Element],
    f: &Frame,
    start: Point,
    p: Point,
    snap: bool,
) -> Vec<Element> {
    let center = f.center();
    let angle_of = |q: Point| (q[1] - center[1]).atan2(q[0] - center[0]);
    let mut delta = angle_of(p) - angle_of(start);
    if snap {
        let step = std::f64::consts::PI / 12.0;
        let target = ((f.angle + delta) / step).round() * step;
        delta = target - f.angle;
    }
    let tau = std::f64::consts::TAU;
    originals
        .iter()
        .map(|o| {
            let mut e = o.clone();
            let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(o);
            let c = [(x1 + x2) / 2.0, (y1 + y2) / 2.0];
            let [ncx, ncy] = geom::rotate(c, center, delta);
            e.set("x", num(o.x() + ncx - c[0]));
            e.set("y", num(o.y() + ncy - c[1]));
            e.set("angle", num((o.angle() + delta).rem_euclid(tau)));
            e
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn el(v: serde_json::Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    fn rect() -> Element {
        el(json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50}))
    }

    #[test]
    fn handles_where_expected() {
        let f = frame(&[&rect()]).unwrap();
        assert_eq!(handle_at(&f, [-8.0, -8.0], 1.0), Some(Handle::NW));
        assert_eq!(handle_at(&f, [108.0, 58.0], 1.0), Some(Handle::SE));
        assert_eq!(handle_at(&f, [50.0, -28.0], 1.0), Some(Handle::Rotation));
        assert_eq!(handle_at(&f, [50.0, 51.0], 1.0), Some(Handle::S));
        assert_eq!(handle_at(&f, [-2.0, 25.0], 1.0), Some(Handle::W));
        assert_eq!(handle_at(&f, [50.0, 25.0], 1.0), None);
        assert_eq!(Handle::E.cursor(0.0), "ew-resize");
        assert_eq!(Handle::E.cursor(std::f64::consts::FRAC_PI_2), "ns-resize");
    }

    #[test]
    fn resizing() {
        let r = rect();
        let f = frame(&[&r]).unwrap();
        let out = resize(
            std::slice::from_ref(&r),
            &f,
            Handle::SE,
            [200.0, 100.0],
            ResizeMode::default(),
        );
        assert_eq!((out[0].width(), out[0].height()), (200.0, 100.0));
        let out = resize(
            std::slice::from_ref(&r),
            &f,
            Handle::W,
            [-50.0, 99.0],
            ResizeMode::default(),
        );
        assert_eq!(
            (out[0].x(), out[0].width(), out[0].height()),
            (-50.0, 150.0, 50.0)
        );
        // Past the other side: flipped, still positive.
        let out = resize(
            std::slice::from_ref(&r),
            &f,
            Handle::E,
            [-20.0, 10.0],
            ResizeMode::default(),
        );
        assert_eq!((out[0].x(), out[0].width()), (-20.0, 20.0));
        // Proportions kept.
        let out = resize(
            std::slice::from_ref(&r),
            &f,
            Handle::SE,
            [300.0, 60.0],
            ResizeMode {
                keep_ratio: true,
                from_center: false,
            },
        );
        assert_eq!((out[0].width(), out[0].height()), (300.0, 150.0));
        // From the centre.
        let out = resize(
            std::slice::from_ref(&r),
            &f,
            Handle::E,
            [110.0, 25.0],
            ResizeMode {
                keep_ratio: false,
                from_center: true,
            },
        );
        assert_eq!((out[0].x(), out[0].width()), (-10.0, 120.0));
    }

    #[test]
    fn several_resize_together() {
        let a = rect();
        let b = el(
            json!({"id": "b", "type": "ellipse", "x": 100, "y": 50, "width": 100, "height": 50}),
        );
        let f = frame(&[&a, &b]).unwrap();
        let out = resize(
            &[a, b],
            &f,
            Handle::SE,
            [400.0, 200.0],
            ResizeMode::default(),
        );
        assert_eq!(
            (out[1].x(), out[1].y(), out[1].width()),
            (200.0, 100.0, 200.0)
        );
    }

    #[test]
    fn turning() {
        let r = rect();
        let f = frame(&[&r]).unwrap();
        // From above the centre to its right: a quarter turn.
        let out = rotate(
            std::slice::from_ref(&r),
            &f,
            [50.0, -50.0],
            [150.0, 25.0],
            false,
        );
        assert!((out[0].angle() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert_eq!((out[0].x(), out[0].y()), (0.0, 0.0));
        // Snapped to 15°.
        let out = rotate(
            std::slice::from_ref(&r),
            &f,
            [50.0, -50.0],
            [150.0, 20.0],
            true,
        );
        let deg = out[0].angle().to_degrees();
        assert!((deg / 15.0 - (deg / 15.0).round()).abs() < 1e-9, "{deg}");
    }
}
