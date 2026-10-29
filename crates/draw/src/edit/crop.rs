//! Cropping pictures, as Excalidraw does (`cropElement.ts`; MIT, see
//! `PORTED.md`): while a picture is being cropped its handles crop it — the
//! picture stays where it is on the drawing, less of it shows — as far as
//! the picture reaches; dragging inside moves the picture within its crop.
//! The crop is kept in the picture's own pixels (`crop: {x, y, width,
//! height, naturalWidth, naturalHeight}`), `null` when all of it shows.

use serde_json::{Value, json};

use super::transform::Handle;
use crate::geom::{self, Point};
use crate::scene::{Element, num};

/// `MINIMAL_CROP_SIZE`.
pub const MIN_SIZE: f64 = 10.0;

/// An image's crop, in the picture's own pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub natural_width: f64,
    pub natural_height: f64,
}

/// The element's crop, if it has one.
pub fn crop_of(e: &Element) -> Option<Crop> {
    let c = e.get("crop")?.as_object()?;
    let n = |k: &str| c.get(k).and_then(Value::as_f64);
    Some(Crop {
        x: n("x")?,
        y: n("y")?,
        width: n("width")?,
        height: n("height")?,
        natural_width: n("naturalWidth")?,
        natural_height: n("naturalHeight")?,
    })
}

fn scale(e: &Element) -> (f64, f64) {
    let s = |i: usize| {
        e.get("scale")
            .and_then(|s| s.get(i))
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
    };
    (s(0), s(1))
}

/// How big the whole picture is at the element's scale
/// (`getUncroppedWidthAndHeight`).
pub fn uncropped_size(e: &Element) -> (f64, f64) {
    match crop_of(e) {
        Some(c) => (
            e.width() / (c.width / c.natural_width),
            e.height() / (c.height / c.natural_height),
        ),
        None => (e.width(), e.height()),
    }
}

/// Where the crop starts within the whole picture, at the element's scale,
/// as shown (flipped pictures crop from their other side)
/// (`getFlipAdjustedCropPosition`).
fn crop_position(e: &Element) -> (f64, f64) {
    let Some(c) = crop_of(e) else {
        return (0.0, 0.0);
    };
    let (sx, sy) = scale(e);
    let x = if sx == -1.0 {
        c.natural_width - c.width - c.x
    } else {
        c.x
    };
    let y = if sy == -1.0 {
        c.natural_height - c.height - c.y
    } else {
        c.y
    };
    let (w, h) = uncropped_size(e);
    (x / (c.natural_width / w), y / (c.natural_height / h))
}

fn center(e: &Element) -> Point {
    [e.x() + e.width() / 2.0, e.y() + e.height() / 2.0]
}

/// The element cropped to `[x1, y1, x2, y2]` (relative to it as shown)
/// (`cropElementToBounds`).
fn crop_to(e: &Element, natural: (f64, f64), [x1, y1, x2, y2]: [f64; 4]) -> Element {
    let c = crop_of(e);
    let scale_x = c.map_or(natural.0, |c| c.width) / e.width();
    let scale_y = c.map_or(natural.1, |c| c.height) / e.height();
    let (w, h) = (x2 - x1, y2 - y1);
    let [cx, cy] = geom::rotate(
        [e.x() + (x1 + x2) / 2.0, e.y() + (y1 + y2) / 2.0],
        center(e),
        e.angle(),
    );
    let (fx, fy) = scale(e);
    let crop = Crop {
        x: c.map_or(0.0, |c| c.x) + if fx == -1.0 { e.width() - x2 } else { x1 } * scale_x,
        y: c.map_or(0.0, |c| c.y) + if fy == -1.0 { e.height() - y2 } else { y1 } * scale_y,
        width: w * scale_x,
        height: h * scale_y,
        natural_width: natural.0,
        natural_height: natural.1,
    };
    let mut out = e.clone();
    out.set("x", num(cx - w / 2.0));
    out.set("y", num(cy - h / 2.0));
    out.set("width", num(w));
    out.set("height", num(h));
    let whole = (crop.width - natural.0).abs() < 1e-6 && (crop.height - natural.1).abs() < 1e-6;
    out.set("crop", if whole { Value::Null } else { to_value(&crop) });
    out
}

fn to_value(c: &Crop) -> Value {
    json!({
        "x": num(c.x), "y": num(c.y), "width": num(c.width), "height": num(c.height),
        "naturalWidth": num(c.natural_width), "naturalHeight": num(c.natural_height),
    })
}

/// The picture `original` (as at the drag's start) cropped as dragging
/// `handle` to `p` would resize it, as far as the picture reaches
/// (`cropElement`); `natural`: the picture's own size in pixels.
pub fn crop(
    original: &Element,
    handle: Handle,
    natural: (f64, f64),
    p: Point,
    keep_ratio: bool,
    from_center: bool,
) -> Element {
    let (width, height) = (original.width(), original.height());
    let (uw, uh) = uncropped_size(original);
    let (left, top) = crop_position(original);
    // `getNextSingleWidthAndHeightFromPointer`, for a box.
    let [rx, ry] = geom::rotate(p, center(original), -original.angle());
    let (x1, y1) = (original.x(), original.y());
    let (x2, y2) = (x1 + width, y1 + height);
    let name = handle_name(handle);
    let mut next_w = if name.contains('e') {
        rx - x1
    } else if name.contains('w') {
        x2 - rx
    } else {
        width
    };
    let mut next_h = if name.contains('s') {
        ry - y1
    } else if name.contains('n') {
        y2 - ry
    } else {
        height
    };
    if from_center {
        next_w = 2.0 * next_w - width;
        next_h = 2.0 * next_h - height;
    }
    // What stays put, as a fraction of the size.
    let anchor_x = if from_center || !(name.contains('e') || name.contains('w')) {
        0.5
    } else if name.contains('w') {
        1.0
    } else {
        0.0
    };
    let anchor_y = if from_center || !(name.contains('n') || name.contains('s')) {
        0.5
    } else if name.contains('n') {
        1.0
    } else {
        0.0
    };
    let max_size = |anchor: f64, offset: f64, size: f64, uncropped: f64| {
        let before = offset + anchor * size;
        let after = uncropped - before;
        if anchor == 0.0 {
            after
        } else if anchor == 1.0 {
            before
        } else {
            before.min(after) * 2.0
        }
    };
    let max_w = max_size(anchor_x, left, width, uw);
    let max_h = max_size(anchor_y, top, height, uh);
    let (min_w, min_h) = (MIN_SIZE.min(width), MIN_SIZE.min(height));
    if keep_ratio {
        let ratio = width / height;
        next_w = match name {
            "n" | "s" => next_h * ratio,
            "e" | "w" => next_w,
            _ => next_w.max(next_h * ratio),
        }
        .max(min_w.max(min_h * ratio))
        .min(max_w.min(max_h * ratio));
        next_h = next_w / ratio;
    } else {
        next_w = next_w.max(min_w).min(max_w);
        next_h = next_h.max(min_h).min(max_h);
    }
    let bx = anchor_x * (width - next_w);
    let by = anchor_y * (height - next_h);
    crop_to(original, natural, [bx, by, bx + next_w, by + next_h])
}

/// The picture moved within its crop by `delta` (on the drawing): the same
/// part of the drawing shows another part of the picture, as far as it
/// reaches.
pub fn pan(original: &Element, delta: Point) -> Element {
    let Some(c) = crop_of(original) else {
        return original.clone();
    };
    let [dx, dy] = geom::rotate(delta, [0.0, 0.0], -original.angle());
    let (fx, fy) = scale(original);
    let (px, py) = (c.width / original.width(), c.height / original.height());
    let crop = Crop {
        x: (c.x - dx * px * fx).clamp(0.0, c.natural_width - c.width),
        y: (c.y - dy * py * fy).clamp(0.0, c.natural_height - c.height),
        ..c
    };
    let mut out = original.clone();
    out.set("crop", to_value(&crop));
    out
}

/// The whole picture as it lies under the crop (`getUncroppedImageElement`):
/// what shows faded while cropping.
pub fn uncropped(e: &Element) -> Element {
    let Some(c) = crop_of(e) else {
        return e.clone();
    };
    let (w, h) = uncropped_size(e);
    let (fx, fy) = scale(e);
    let crop_x = if fx == -1.0 {
        c.natural_width - c.x.abs() - c.width
    } else {
        c.x
    };
    let crop_y = if fy == -1.0 {
        c.natural_height - c.y.abs() - c.height
    } else {
        c.y
    };
    let a = e.angle();
    let mid = center(e);
    let top_left = geom::rotate([e.x(), e.y()], mid, a);
    let (ux, uy) = (a.cos(), a.sin());
    let (vx, vy) = (-a.sin(), a.cos());
    let ox = -crop_x * w / c.natural_width;
    let oy = -crop_y * h / c.natural_height;
    let corner = [
        top_left[0] + ux * ox + vx * oy,
        top_left[1] + uy * ox + vy * oy,
    ];
    let whole_center = [
        corner[0] + ux * w / 2.0 + vx * h / 2.0,
        corner[1] + uy * w / 2.0 + vy * h / 2.0,
    ];
    let [x, y] = geom::rotate(corner, whole_center, -a);
    let mut out = e.clone();
    out.set("x", num(x));
    out.set("y", num(y));
    out.set("width", num(w));
    out.set("height", num(h));
    out.set("crop", Value::Null);
    out
}

fn handle_name(h: Handle) -> &'static str {
    match h {
        Handle::N => "n",
        Handle::S => "s",
        Handle::E => "e",
        Handle::W => "w",
        Handle::NE => "ne",
        Handle::NW => "nw",
        Handle::SE => "se",
        Handle::SW => "sw",
        Handle::Rotation => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Element {
        Element::from_props(
            json!({"id": "i", "type": "image", "x": 0, "y": 0, "width": 200, "height": 100,
                   "fileId": "f", "scale": [1, 1], "crop": null})
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap()
    }

    #[test]
    fn a_side_crops_and_the_rest_stays_put() {
        // A 400 × 200 picture shown at half size: the right side dragged in.
        let p = picture();
        let c = crop(&p, Handle::E, (400.0, 200.0), [150.0, 50.0], false, false);
        assert_eq!(
            (c.x(), c.y(), c.width(), c.height()),
            (0.0, 0.0, 150.0, 100.0)
        );
        let k = crop_of(&c).unwrap();
        assert_eq!((k.x, k.width, k.height), (0.0, 300.0, 200.0));
        // Then the left: what shows starts further in.
        let c2 = crop(&c, Handle::W, (400.0, 200.0), [50.0, 50.0], false, false);
        assert_eq!((c2.x(), c2.width()), (50.0, 100.0));
        assert_eq!(crop_of(&c2).unwrap().x, 100.0);
        // No further out than the picture reaches; all of it is no crop.
        let back = crop(&c2, Handle::W, (400.0, 200.0), [-500.0, 50.0], false, false);
        assert_eq!((back.x(), back.width()), (0.0, 150.0));
        let whole = crop(
            &back,
            Handle::E,
            (400.0, 200.0),
            [900.0, 50.0],
            false,
            false,
        );
        assert_eq!(whole.get("crop"), Some(&Value::Null));
        assert_eq!(whole.width(), 200.0);
    }

    #[test]
    fn panning_and_what_lies_under() {
        let p = picture();
        let c = crop(&p, Handle::SE, (400.0, 200.0), [100.0, 50.0], false, false);
        assert_eq!((c.width(), c.height()), (100.0, 50.0));
        // The picture moved left within the crop: more of its right shows.
        let moved = pan(&c, [-30.0, 0.0]);
        assert_eq!(crop_of(&moved).unwrap().x, 60.0);
        assert_eq!(crop_of(&pan(&c, [500.0, 0.0])).unwrap().x, 0.0);
        let under = uncropped(&moved);
        assert_eq!(
            (under.x(), under.width(), under.height()),
            (-30.0, 200.0, 100.0)
        );
    }
}
