//! What shows of the window. On an iPhone an on-screen keyboard doesn't
//! make the page shorter, as it does on Android (the viewport tag's
//! `interactive-widget=resizes-content`, which iOS ignores): it covers the
//! bottom of the page, and the browser moves the page up to keep the caret
//! in view, taking the note's header out of sight.
//!
//! So the page follows the visual viewport, the part of the window that
//! shows: the body is as tall as that (`--visible-h`, through `--app-h` in
//! style.css) and starts where that does (`--visible-top`), and the
//! window's own scroll is undone: the page never scrolls otherwise (`.main`
//! does, inside it). Where what shows is the whole window (Android, a
//! computer) this changes nothing.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::{state, util};

/// Where the page goes, in CSS pixels: its top, from the top of the
/// window, and its height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Visible {
    pub top: f64,
    pub height: f64,
}

/// Pinch-zoomed: what shows is a part of the page, and moving it around
/// is the person's doing, which the page leaves alone.
pub fn zoomed(scale: f64) -> bool {
    (scale - 1.0).abs() > 0.01
}

impl Visible {
    /// From the visual viewport: its height, its scale and how far it is
    /// from the top of the window (`offsetTop`). `None` for values that
    /// make no sense.
    pub fn of(height: f64, scale: f64, offset_top: f64) -> Option<Visible> {
        let sane = height.is_finite() && height > 0.0 && scale.is_finite() && scale > 0.0;
        if !sane || !offset_top.is_finite() {
            return None;
        }
        Some(Visible {
            top: if zoomed(scale) {
                0.0
            } else {
                offset_top.max(0.0)
            },
            // In the page's pixels: zoomed in, the visual viewport's own
            // are bigger.
            height: height * scale,
        })
    }
}

/// Smaller changes (CSS pixels) are none: browsers report fractions that
/// wobble.
const WOBBLE: f64 = 0.5;

/// Follows what shows, one change at a time.
#[derive(Default)]
pub struct Follower {
    last: Option<Visible>,
}

/// What to do about a change.
#[derive(Debug, PartialEq)]
pub struct Change {
    /// Where the page goes now, if that moved.
    pub set: Option<Visible>,
    /// Less of the page shows than before (a keyboard opened): what has
    /// the focus goes back into view.
    pub reveal: bool,
}

impl Follower {
    pub fn update(&mut self, now: Visible) -> Change {
        let Some(last) = self.last else {
            self.last = Some(now);
            return Change {
                set: Some(now),
                reveal: false,
            };
        };
        let moved =
            (now.top - last.top).abs() >= WOBBLE || (now.height - last.height).abs() >= WOBBLE;
        if !moved {
            return Change {
                set: None,
                reveal: false,
            };
        }
        self.last = Some(now);
        Change {
            set: Some(now),
            reveal: now.height < last.height,
        }
    }
}

/// Follow what shows of the window from now on (once, as the app starts).
pub fn install() {
    let window = util::window();
    let Some(vv) = window.visual_viewport() else {
        // An older browser: the page is as tall as the window (100dvh).
        return;
    };
    let follower = Rc::new(RefCell::new(Follower::default()));
    let on_change = {
        let (vv, follower) = (vv.clone(), follower.clone());
        Closure::<dyn FnMut()>::new(move || follow(&vv, &follower))
    };
    let f = on_change.as_ref().unchecked_ref();
    let _ = vv.add_event_listener_with_callback("resize", f);
    let _ = vv.add_event_listener_with_callback("scroll", f);
    let _ = window.add_event_listener_with_callback("scroll", f);
    on_change.forget();
    follow(&vv, &follower);
}

fn follow(vv: &web_sys::VisualViewport, follower: &RefCell<Follower>) {
    let window = util::window();
    let scrolled = |v: Result<f64, _>| v.is_ok_and(|v: f64| v != 0.0);
    if !zoomed(vv.scale()) && (scrolled(window.scroll_x()) || scrolled(window.scroll_y())) {
        window.scroll_to_with_x_and_y(0.0, 0.0);
    }
    let Some(now) = Visible::of(vv.height(), vv.scale(), vv.offset_top()) else {
        return;
    };
    let change = follower.borrow_mut().update(now);
    if let Some(v) = change.set {
        util::set_root_var("--visible-top", &format!("{:.1}px", v.top));
        util::set_root_var("--visible-h", &format!("{:.1}px", v.height));
    }
    if change.reveal {
        reveal_focus();
    }
}

/// Scroll what has the focus back into view: the note's caret, or a
/// field.
fn reveal_focus() {
    if let Some(ed) = state::step(|c| c.editor.as_ref().map(|(_, _, e)| e.clone()))
        && ed.has_focus()
    {
        ed.reveal_caret();
        return;
    }
    let Some(field) = util::document()
        .active_element()
        .filter(|e| matches!(e.tag_name().as_str(), "INPUT" | "TEXTAREA"))
    else {
        return;
    };
    let opts = web_sys::ScrollIntoViewOptions::new();
    opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
    field.scroll_into_view_with_scroll_into_view_options(&opts);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(top: f64, height: f64) -> Visible {
        Visible { top, height }
    }

    #[test]
    fn the_whole_window_shows_unzoomed() {
        assert_eq!(Visible::of(800.0, 1.0, 0.0), Some(at(0.0, 800.0)));
    }

    #[test]
    fn a_keyboard_leaves_less_and_moves_it() {
        // An iPhone's keyboard: what shows is shorter, further down.
        assert_eq!(Visible::of(370.0, 1.0, 150.0), Some(at(150.0, 370.0)));
    }

    #[test]
    fn zoomed_in_the_page_stays_put() {
        // Pinch-zoomed to twice the size and moved around in: the page
        // keeps its height in its own pixels and stays at the top.
        assert_eq!(Visible::of(400.0, 2.0, 200.0), Some(at(0.0, 800.0)));
        assert_eq!(Visible::of(1600.0, 0.5, 30.0), Some(at(0.0, 800.0)));
        // A zoom that wobbles around 1 isn't one.
        assert!(!zoomed(1.0001));
    }

    #[test]
    fn nonsense_is_ignored() {
        assert_eq!(Visible::of(f64::NAN, 1.0, 0.0), None);
        assert_eq!(Visible::of(0.0, 1.0, 0.0), None);
        assert_eq!(Visible::of(800.0, 0.0, 0.0), None);
        assert_eq!(Visible::of(800.0, 1.0, f64::INFINITY), None);
        assert_eq!(Visible::of(800.0, 1.0, -4.0), Some(at(0.0, 800.0)));
    }

    #[test]
    fn the_first_look_sets_without_revealing() {
        let mut f = Follower::default();
        assert_eq!(
            f.update(at(0.0, 800.0)),
            Change {
                set: Some(at(0.0, 800.0)),
                reveal: false
            }
        );
    }

    #[test]
    fn a_keyboard_opening_reveals_and_closing_does_not() {
        let mut f = Follower::default();
        f.update(at(0.0, 800.0));
        let opened = f.update(at(0.0, 420.0));
        assert_eq!(opened.set, Some(at(0.0, 420.0)));
        assert!(opened.reveal);
        let closed = f.update(at(0.0, 800.0));
        assert_eq!(closed.set, Some(at(0.0, 800.0)));
        assert!(!closed.reveal);
    }

    #[test]
    fn moving_alone_does_not_reveal() {
        // Scrolling the note past its end moves what shows: the page
        // follows, the note stays where it was scrolled to.
        let mut f = Follower::default();
        f.update(at(0.0, 420.0));
        let moved = f.update(at(60.0, 420.0));
        assert_eq!(moved.set, Some(at(60.0, 420.0)));
        assert!(!moved.reveal);
    }

    #[test]
    fn wobbles_change_nothing_but_add_up() {
        let mut f = Follower::default();
        f.update(at(0.0, 800.0));
        assert_eq!(
            f.update(at(0.2, 799.7)),
            Change {
                set: None,
                reveal: false
            }
        );
        // Measured from what was last set, so small steps still count
        // once they add up.
        assert_eq!(f.update(at(0.0, 799.4)).set, Some(at(0.0, 799.4)));
    }
}
