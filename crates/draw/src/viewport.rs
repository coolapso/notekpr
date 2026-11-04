//! Where the canvas looks: scrolled and zoomed as in Excalidraw (`scrollX`,
//! `scrollY`, `zoom`), a point on the screen being
//! `(scene + scroll) × zoom`.

use crate::bounds::Bounds;
use crate::geom::Point;

/// Excalidraw's zoom limits.
pub const MIN_ZOOM: f64 = 0.1;
pub const MAX_ZOOM: f64 = 30.0;
/// The most a wheel event zooms, in pixels of the wheel (a tenth).
const WHEEL_STEP: f64 = 10.0;
/// How much one step of zooming changes it (`ZOOM_STEP`).
pub const ZOOM_STEP: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub zoom: f64,
    /// The canvas's size on screen (CSS pixels).
    pub width: f64,
    pub height: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            scroll_x: 0.0,
            scroll_y: 0.0,
            zoom: 1.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

/// A zoom within the limits.
pub fn clamp_zoom(z: f64) -> f64 {
    if z.is_finite() {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

impl Viewport {
    /// A point of the drawing on the screen (relative to the canvas).
    pub fn to_screen(&self, [x, y]: Point) -> Point {
        [
            (x + self.scroll_x) * self.zoom,
            (y + self.scroll_y) * self.zoom,
        ]
    }

    /// A point on the screen (relative to the canvas) in the drawing.
    pub fn to_scene(&self, [x, y]: Point) -> Point {
        [x / self.zoom - self.scroll_x, y / self.zoom - self.scroll_y]
    }

    /// Scrolled by a distance on screen.
    pub fn scrolled(&self, dx: f64, dy: f64) -> Self {
        Self {
            scroll_x: self.scroll_x + dx / self.zoom,
            scroll_y: self.scroll_y + dy / self.zoom,
            ..*self
        }
    }

    /// Zoomed by a wheel with Ctrl (or a touchpad's pinch, which comes as
    /// one) that went `delta_y` pixels, keeping what is under `at`: a
    /// mouse wheel's notch (some 100 pixels, or 3 lines) zooms by a tenth,
    /// a pinch's many small steps as far as the fingers go.
    pub fn wheel_zoomed(&self, delta_y: f64, at: Point) -> Self {
        let step = (-delta_y).clamp(-WHEEL_STEP, WHEEL_STEP) * 0.01;
        self.zoomed_at(self.zoom * step.exp(), at)
    }

    /// Zoomed to `zoom`, keeping what is under `at` (a point on screen)
    /// where it is (`getStateForZoom`).
    pub fn zoomed_at(&self, zoom: f64, [ax, ay]: Point) -> Self {
        let next = clamp_zoom(zoom);
        let base_x = self.scroll_x + (ax - ax / self.zoom);
        let base_y = self.scroll_y + (ay - ay / self.zoom);
        Self {
            scroll_x: base_x - (ax - ax / next),
            scroll_y: base_y - (ay - ay / next),
            zoom: next,
            ..*self
        }
    }

    /// The part of the drawing in view.
    pub fn visible(&self) -> Bounds {
        let [x1, y1] = self.to_scene([0.0, 0.0]);
        let [x2, y2] = self.to_scene([self.width, self.height]);
        [x1, y1, x2, y2]
    }

    /// Fits `content` in view with some room around it, zooming in no
    /// further than `max_zoom`; content centred.
    pub fn fitted(&self, content: Bounds, max_zoom: f64) -> Self {
        let [x1, y1, x2, y2] = content;
        let (w, h) = (x2 - x1, y2 - y1);
        if !(w.is_finite() && h.is_finite()) || self.width <= 0.0 || self.height <= 0.0 {
            return *self;
        }
        // Excalidraw keeps a tenth of the view free around it.
        let room = 0.1;
        let fit = |avail: f64, size: f64| {
            if size > 0.0 {
                avail * (1.0 - room) / size
            } else {
                f64::INFINITY
            }
        };
        let zoom = clamp_zoom(fit(self.width, w).min(fit(self.height, h)).min(max_zoom));
        Self {
            scroll_x: self.width / 2.0 / zoom - (x1 + w / 2.0),
            scroll_y: self.height / 2.0 / zoom - (y1 + h / 2.0),
            zoom,
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Viewport {
        Viewport {
            width: 800.0,
            height: 600.0,
            ..Viewport::default()
        }
    }

    #[test]
    fn a_wheel_zooms_by_a_tenth_a_notch() {
        let (v, at) = (view(), [400.0, 300.0]);
        // A notch: 100 pixels (Chromium), 120, 3 lines of 16 (Firefox).
        for notch in [100.0, 120.0, 48.0] {
            assert!((v.wheel_zoomed(-notch, at).zoom - 1.1052).abs() < 1e-3);
            assert!((v.wheel_zoomed(notch, at).zoom - 0.9048).abs() < 1e-3);
        }
        // A pinch: many small steps, each as far as it went.
        let mut p = v;
        for _ in 0..20 {
            p = p.wheel_zoomed(-2.0, at);
        }
        assert!((p.zoom - (0.4f64).exp()).abs() < 1e-9);
        // What is under the pointer stays there.
        let z = v.wheel_zoomed(-100.0, [100.0, 50.0]);
        let (a, b) = (v.to_scene([100.0, 50.0]), z.to_scene([100.0, 50.0]));
        assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
    }

    #[test]
    fn screen_and_scene_round_trip() {
        let v = Viewport {
            scroll_x: 10.0,
            scroll_y: -5.0,
            zoom: 2.0,
            ..view()
        };
        assert_eq!(v.to_screen([0.0, 0.0]), [20.0, -10.0]);
        assert_eq!(v.to_scene(v.to_screen([3.0, 4.0])), [3.0, 4.0]);
        assert_eq!(v.scrolled(20.0, 0.0).scroll_x, 20.0);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let v = view();
        let at = [200.0, 150.0];
        let under = v.to_scene(at);
        let z = v.zoomed_at(2.5, at);
        let again = z.to_scene(at);
        assert!((again[0] - under[0]).abs() < 1e-9 && (again[1] - under[1]).abs() < 1e-9);
        assert_eq!(v.zoomed_at(1000.0, at).zoom, MAX_ZOOM);
        assert_eq!(v.zoomed_at(0.0, at).zoom, MIN_ZOOM);
    }

    #[test]
    fn fits_content() {
        let v = view().fitted([100.0, 100.0, 500.0, 300.0], 1.0);
        let [x1, y1, x2, y2] = v.visible();
        assert!(x1 <= 100.0 && y1 <= 100.0 && x2 >= 500.0 && y2 >= 300.0);
        // Centred.
        let c = v.to_screen([300.0, 200.0]);
        assert!((c[0] - 400.0).abs() < 1e-9 && (c[1] - 300.0).abs() < 1e-9);
        // A huge drawing zooms out.
        let big = view().fitted([0.0, 0.0, 8000.0, 100.0], 1.0);
        assert!(big.zoom < 0.1 + 1e-9);
    }
}
