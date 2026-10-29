//! Editing a drawing, as Excalidraw does (`App.tsx` and its actions; MIT,
//! see `PORTED.md`): tools, the style new elements get, selecting, making,
//! moving, resizing, turning, erasing, and the everyday commands.
//!
//! Pure: the app feeds pointer events (in drawing coordinates) and
//! commands; each answers with the elements that changed ([`Output`]),
//! which the app writes to the drawing's CRDT — while a gesture goes on as
//! a preview, at its end for good.

pub mod binding;
pub mod frame;
pub mod hit;
pub mod label;
pub mod new;
pub mod transform;

use std::collections::{BTreeSet, HashMap};

use serde_json::{Value, json};

use crate::bounds;
use crate::geom::{self, Point};
use crate::order;
use crate::scene::{Element, ElementType, Scene, num};
use transform::{Frame, Handle, ResizeMode};

/// What new elements are made with: Excalidraw's `currentItem…`.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub stroke_color: String,
    pub background_color: String,
    pub fill_style: String,
    pub stroke_width: f64,
    pub stroke_style: String,
    /// Sloppiness: 0, 1 or 2.
    pub roughness: f64,
    pub opacity: f64,
    /// Rounded edges (rectangles, diamonds, lines).
    pub round_edges: bool,
    pub font_family: u32,
    pub font_size: f64,
    pub text_align: String,
    pub start_arrowhead: Option<String>,
    pub end_arrowhead: Option<String>,
    /// `sharp`, `round` (curved) or `elbow`.
    pub arrow_type: String,
    /// Pen strokes: `variable` (with pressure) or `constant`.
    pub stroke_variability: String,
    /// Sticky notes' own colours: their ink and their paper.
    pub sticky_stroke_color: String,
    pub sticky_background_color: String,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            stroke_color: crate::color::palette::BLACK.into(),
            background_color: "transparent".into(),
            fill_style: "solid".into(),
            stroke_width: 2.0,
            stroke_style: "solid".into(),
            roughness: 1.0,
            opacity: 100.0,
            round_edges: true,
            font_family: crate::fonts::DEFAULT,
            font_size: 20.0,
            text_align: "left".into(),
            start_arrowhead: None,
            end_arrowhead: Some("arrow".into()),
            arrow_type: "round".into(),
            stroke_variability: "variable".into(),
            sticky_stroke_color: crate::sticky::INK.into(),
            sticky_background_color: crate::sticky::BACKGROUND.into(),
        }
    }
}

/// The tools of the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Selection,
    Hand,
    Rectangle,
    Diamond,
    Ellipse,
    Arrow,
    Line,
    Freedraw,
    Text,
    StickyNote,
    Image,
    Eraser,
    Frame,
    Laser,
}

impl Tool {
    /// The keyboard shortcuts Excalidraw has for them.
    pub fn from_key(key: &str) -> Option<Tool> {
        Some(match key.to_lowercase().as_str() {
            "v" | "1" => Tool::Selection,
            "h" => Tool::Hand,
            "r" | "2" => Tool::Rectangle,
            "d" | "3" => Tool::Diamond,
            "o" | "4" => Tool::Ellipse,
            "a" | "5" => Tool::Arrow,
            "l" | "6" => Tool::Line,
            "p" | "7" => Tool::Freedraw,
            "t" | "8" => Tool::Text,
            "n" => Tool::StickyNote,
            "9" => Tool::Image,
            "e" | "0" => Tool::Eraser,
            "f" => Tool::Frame,
            "k" => Tool::Laser,
            _ => return None,
        })
    }

    fn shape(self) -> Option<ElementType> {
        Some(match self {
            Tool::Rectangle => ElementType::Rectangle,
            Tool::Diamond => ElementType::Diamond,
            Tool::Ellipse => ElementType::Ellipse,
            Tool::Frame => ElementType::Frame,
            Tool::StickyNote => ElementType::StickyNote,
            _ => return None,
        })
    }
}

/// Where new ids, seeds and the time come from (the app's randomness).
pub trait Random {
    /// An id like Excalidraw's (21 characters).
    fn id(&mut self) -> String;
    /// A random 31-bit number.
    fn seed(&mut self) -> i32;
    /// Milliseconds since 1970.
    fn now(&self) -> f64;
}

/// Keys held during a pointer event.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    /// Ctrl or ⌘.
    pub ctrl: bool,
}

/// What an event changed.
#[derive(Debug, Default)]
pub struct Output {
    /// Elements to write, whole (new ones too).
    pub changed: Vec<Element>,
    /// The gesture is over: write them for good (one undo step).
    pub done: bool,
    /// Open text editing for this text element.
    pub edit_text: Option<String>,
    /// The tool went back to the selection tool.
    pub tool_changed: bool,
}

/// A drag in progress.
#[derive(Clone, Debug)]
enum Gesture {
    /// Drawing a new box-like element from `origin`.
    Creating {
        id: String,
        origin: Point,
    },
    /// A new line or arrow whose last point follows the pointer; `dragged`
    /// once it was (then it ends with the pointer's release).
    Linear {
        id: String,
        points: Vec<Point>,
        dragged: bool,
    },
    /// A pen stroke.
    Pen {
        id: String,
        points: Vec<Point>,
        pressures: Vec<f64>,
    },
    Moving {
        start: Point,
        originals: Vec<Element>,
        moved: bool,
    },
    Resizing {
        handle: Handle,
        frame: Frame,
        originals: Vec<Element>,
        offset: Point,
    },
    Rotating {
        start: Point,
        frame: Frame,
        originals: Vec<Element>,
    },
    BoxSelecting {
        start: Point,
        current: Point,
        before: Vec<String>,
    },
    Erasing {
        ids: BTreeSet<String>,
    },
}

/// The editor's state for one open drawing.
#[derive(Clone, Debug)]
pub struct Editor {
    pub tool: Tool,
    /// The tool stays after drawing something (Excalidraw's lock).
    pub tool_locked: bool,
    pub style: Style,
    /// Selected elements, in the order selected.
    pub selection: Vec<String>,
    gesture: Option<Gesture>,
    /// The text being edited.
    pub editing_text: Option<String>,
    /// Measures a line of text in a CSS font (the app's, with the browser).
    pub measure: fn(&str, &str) -> f64,
    /// The zoom of the last pointer event (how near counts).
    zoom: f64,
    /// The frame where what is drawn or dragged goes (shown).
    frame_to_highlight: Option<String>,
}

/// A guess at a line's width without a browser: about half the font size a
/// character.
pub fn guess_width(font: &str, line: &str) -> f64 {
    let size: f64 = font
        .split("px")
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(20.0);
    line.chars().count() as f64 * size * 0.55
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            tool: Tool::Selection,
            tool_locked: false,
            style: Style::default(),
            selection: Vec::new(),
            gesture: None,
            editing_text: None,
            measure: guess_width,
            zoom: 1.0,
            frame_to_highlight: None,
        }
    }
}

/// Marks an element as changed (`bumpVersion`): a new version and nonce.
pub fn bump(e: &mut Element, rng: &mut dyn Random) {
    e.set("version", num(e.version() as f64 + 1.0));
    e.set("versionNonce", json!(rng.seed()));
    e.set("updated", num(rng.now()));
}

/// Arrows attached to what `out` changed follow it (they are added).
fn arrows_follow(scene: &Scene, out: &mut Output, rng: &mut dyn Random) {
    let n = out.changed.len();
    binding::update_arrows(scene, &mut out.changed);
    for e in &mut out.changed[n..] {
        bump(e, rng);
    }
}

/// A stacking index above every element.
fn top_index(scene: &Scene) -> Option<String> {
    let last = scene.elements.iter().filter_map(Element::index).max();
    order::key_between(last, None)
}

/// The outermost group an element is in.
fn outer_group(e: &Element) -> Option<&str> {
    e.group_ids().last().copied()
}

impl Editor {
    pub fn is_selected(&self, id: &str) -> bool {
        self.selection.iter().any(|s| s == id)
    }

    /// The selected elements that are there, in stacking order.
    pub fn selected<'a>(&self, scene: &'a Scene) -> Vec<&'a Element> {
        scene
            .visible()
            .filter(|e| self.is_selected(e.id()))
            .collect()
    }

    /// Whether a drag is going on.
    pub fn busy(&self) -> bool {
        self.gesture.is_some()
    }

    /// The box being drawn to select, if one is.
    pub fn selection_box(&self) -> Option<[f64; 4]> {
        match &self.gesture {
            Some(Gesture::BoxSelecting { start, current, .. }) => {
                Some([start[0], start[1], current[0], current[1]])
            }
            _ => None,
        }
    }

    /// The elements an eraser drag will delete.
    pub fn erasing(&self) -> Vec<String> {
        match &self.gesture {
            Some(Gesture::Erasing { ids }) => ids.iter().cloned().collect(),
            _ => Vec::new(),
        }
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.gesture = None;
        self.frame_to_highlight = None;
        if tool != Tool::Selection {
            self.selection.clear();
        }
    }

    /// Selects `e` and the rest of its outermost group.
    fn select_with_group(&mut self, scene: &Scene, e: &Element, add: bool) {
        if !add {
            self.selection.clear();
        }
        let ids: Vec<String> = match outer_group(e) {
            Some(g) => scene
                .visible()
                .filter(|o| o.group_ids().contains(&g))
                .map(|o| o.id().to_string())
                .collect(),
            None => vec![e.id().to_string()],
        };
        for id in ids {
            if !self.is_selected(&id) {
                self.selection.push(id);
            }
        }
    }

    /// The selected elements with their labels (they go along).
    fn moving_set(&self, scene: &Scene) -> Vec<Element> {
        let mut ids: BTreeSet<String> = self.selection.iter().cloned().collect();
        for e in self.selected(scene) {
            for b in e.bound_elements() {
                if b.kind == "text" {
                    ids.insert(b.id);
                }
            }
        }
        scene
            .visible()
            .filter(|e| ids.contains(e.id()) && !e.is_locked())
            .cloned()
            .collect()
    }

    /// The selection's frame, unless it is locked.
    pub fn frame(&self, scene: &Scene) -> Option<Frame> {
        let selected = self.selected(scene);
        if selected.iter().any(|e| e.is_locked()) {
            return None;
        }
        transform::frame(&selected)
    }

    /// The pointer went down at `p` (drawing coordinates).
    pub fn pointer_down(
        &mut self,
        scene: &Scene,
        p: Point,
        pressure: Option<f64>,
        mods: Mods,
        zoom: f64,
        rng: &mut dyn Random,
    ) -> Output {
        self.zoom = zoom;
        let mut out = Output::default();
        // A line or arrow made point by point: each click adds one.
        if let Some(Gesture::Linear {
            id,
            points,
            dragged: false,
        }) = &mut self.gesture
        {
            let n = points.len();
            let id = id.clone();
            if n >= 3 && geom::distance(points[n - 2], p) < 10.0 / zoom {
                // Clicking the last point again ends it.
                points.pop();
                let mut s2 = scene.clone();
                if let Some(e) = s2.elements.iter_mut().find(|e| e.id() == id) {
                    new::normalize_points(e, points);
                }
                return self.finish_linear(&s2, &id, rng);
            }
            // The click places the point; the next one follows the pointer.
            points[n - 1] = p;
            points.push(p);
            if let Some(mut e) = scene.element(&id).cloned() {
                new::normalize_points(&mut e, points);
                bump(&mut e, rng);
                out.changed.push(e);
            }
            return out;
        }
        match self.tool {
            Tool::Hand | Tool::Laser | Tool::Image => {}
            Tool::Selection => {
                if let Some(f) = self.frame(scene)
                    && let Some(h) =
                        transform::handle_at(&f, p, zoom).filter(|h| self.can_use(scene, *h))
                {
                    let originals = self.moving_set(scene);
                    self.gesture = Some(if h == Handle::Rotation {
                        Gesture::Rotating {
                            start: p,
                            frame: f,
                            originals,
                        }
                    } else {
                        // The pointer is a little off the corner it took:
                        // the corner keeps that distance.
                        let [x1, y1, x2, y2] = f.bounds;
                        let local = geom::rotate(p, f.center(), -f.angle);
                        let corner = [
                            match h {
                                Handle::W | Handle::NW | Handle::SW => x1,
                                Handle::E | Handle::NE | Handle::SE => x2,
                                _ => local[0],
                            },
                            match h {
                                Handle::N | Handle::NW | Handle::NE => y1,
                                Handle::S | Handle::SW | Handle::SE => y2,
                                _ => local[1],
                            },
                        ];
                        let offset = [corner[0] - local[0], corner[1] - local[1]];
                        Gesture::Resizing {
                            handle: h,
                            frame: f,
                            originals,
                            offset,
                        }
                    });
                    return out;
                }
                // Inside the selection's box drags it.
                let in_selection = self.frame(scene).is_some_and(|f| {
                    let [x, y] = geom::rotate(p, f.center(), -f.angle);
                    let [x1, y1, x2, y2] = f.bounds;
                    x >= x1 && x <= x2 && y >= y1 && y <= y2
                });
                match hit::element_at(scene, p, zoom, false) {
                    Some(e) if !in_selection || self.is_selected(e.id()) => {
                        if mods.shift && self.is_selected(e.id()) {
                            let gone: Vec<String> = match outer_group(e) {
                                Some(g) => scene
                                    .visible()
                                    .filter(|o| o.group_ids().contains(&g))
                                    .map(|o| o.id().to_string())
                                    .collect(),
                                None => vec![e.id().to_string()],
                            };
                            self.selection.retain(|s| !gone.contains(s));
                            return out;
                        }
                        if !self.is_selected(e.id()) {
                            self.select_with_group(scene, e, mods.shift);
                        }
                        self.gesture = Some(Gesture::Moving {
                            start: p,
                            originals: self.dragged_set(scene),
                            moved: false,
                        });
                    }
                    _ if in_selection => {
                        self.gesture = Some(Gesture::Moving {
                            start: p,
                            originals: self.dragged_set(scene),
                            moved: false,
                        });
                    }
                    _ => {
                        let before = if mods.shift {
                            self.selection.clone()
                        } else {
                            Vec::new()
                        };
                        self.selection = before.clone();
                        self.gesture = Some(Gesture::BoxSelecting {
                            start: p,
                            current: p,
                            before,
                        });
                    }
                }
            }
            Tool::Eraser => {
                let mut ids = BTreeSet::new();
                if let Some(e) = hit::element_at(scene, p, zoom, false) {
                    ids.insert(e.id().to_string());
                }
                self.gesture = Some(Gesture::Erasing { ids });
            }
            Tool::Text => {
                // On a text: edit it; elsewhere: a new one.
                if let Some(e) =
                    hit::element_at(scene, p, zoom, false).filter(|e| e.kind() == ElementType::Text)
                {
                    self.selection = vec![e.id().to_string()];
                    self.editing_text = Some(e.id().to_string());
                    out.edit_text = Some(e.id().to_string());
                    return out;
                }
                let mut t = new::text(p[0], p[1], "", &self.style, &|_, _| 0.0, rng);
                // Centred on the pointer's line, as Excalidraw places it.
                let line =
                    self.style.font_size * crate::fonts::get(self.style.font_family).line_height;
                t.set("y", num(p[1] - line / 2.0));
                self.place_new(scene, &mut t, [p[0], p[1] - line / 2.0]);
                let id = t.id().to_string();
                self.selection = vec![id.clone()];
                self.editing_text = Some(id.clone());
                out.changed.push(t);
                out.edit_text = Some(id);
            }
            Tool::Freedraw => {
                let mut e = new::freedraw(p[0], p[1], pressure, &self.style, rng);
                self.place_new(scene, &mut e, p);
                self.gesture = Some(Gesture::Pen {
                    id: e.id().to_string(),
                    points: vec![p],
                    pressures: pressure.into_iter().collect(),
                });
                self.selection.clear();
                out.changed.push(e);
            }
            Tool::Arrow | Tool::Line => {
                let kind = if self.tool == Tool::Arrow {
                    ElementType::Arrow
                } else {
                    ElementType::Line
                };
                let mut e = new::linear(kind, p[0], p[1], &self.style, rng);
                self.place_new(scene, &mut e, p);
                new::normalize_points(&mut e, &[p, p]);
                self.gesture = Some(Gesture::Linear {
                    id: e.id().to_string(),
                    points: vec![p, p],
                    dragged: false,
                });
                self.selection.clear();
                out.changed.push(e);
            }
            tool => {
                let kind = tool.shape().expect("a shape tool");
                let mut e = new::shape(kind, p[0], p[1], &self.style, rng);
                self.place_new(scene, &mut e, p);
                self.gesture = Some(Gesture::Creating {
                    id: e.id().to_string(),
                    origin: p,
                });
                self.selection.clear();
                out.changed.push(e);
            }
        }
        out
    }

    /// The pointer moved to `p` (with a button down, or while a line is
    /// made point by point).
    pub fn pointer_move(
        &mut self,
        scene: &Scene,
        p: Point,
        pressure: Option<f64>,
        mods: Mods,
        zoom: f64,
        rng: &mut dyn Random,
    ) -> Output {
        self.zoom = zoom;
        let mut out = Output::default();
        let Some(g) = self.gesture.as_mut() else {
            self.hover_frame(scene, p);
            return out;
        };
        match g {
            Gesture::Creating { id, origin } => {
                let Some(mut e) = scene.element(id).cloned() else {
                    return out;
                };
                let (mut w, mut h) = (p[0] - origin[0], p[1] - origin[1]);
                // Notes are drawn square; Shift frees them.
                if mods.shift != crate::sticky::is_note(&e) {
                    let s = w.abs().max(h.abs());
                    w = s * w.signum();
                    h = s * h.signum();
                }
                let (x, y, w, h) = if mods.alt {
                    (
                        origin[0] - w.abs(),
                        origin[1] - h.abs(),
                        w.abs() * 2.0,
                        h.abs() * 2.0,
                    )
                } else {
                    (
                        origin[0].min(origin[0] + w),
                        origin[1].min(origin[1] + h),
                        w.abs(),
                        h.abs(),
                    )
                };
                e.set("x", num(x));
                e.set("y", num(y));
                e.set("width", num(w));
                e.set("height", num(h));
                bump(&mut e, rng);
                out.changed.push(e);
            }
            Gesture::Linear {
                id,
                points,
                dragged,
            } => {
                let Some(mut e) = scene.element(id).cloned() else {
                    return out;
                };
                let n = points.len();
                let anchor = points[n - 2];
                let mut q = p;
                if mods.shift {
                    // In steps of 15°.
                    let (dx, dy) = (p[0] - anchor[0], p[1] - anchor[1]);
                    let len = dx.hypot(dy);
                    let step = std::f64::consts::PI / 12.0;
                    let a = (dy.atan2(dx) / step).round() * step;
                    q = [anchor[0] + len * a.cos(), anchor[1] + len * a.sin()];
                }
                points[n - 1] = q;
                if n == 2 && geom::distance(points[0], q) > 4.0 / zoom {
                    *dragged = true;
                }
                new::normalize_points(&mut e, points);
                bump(&mut e, rng);
                out.changed.push(e);
            }
            Gesture::Pen {
                id,
                points,
                pressures,
            } => {
                let Some(mut e) = scene.element(id).cloned() else {
                    return out;
                };
                points.push(p);
                if let Some(pr) = pressure {
                    pressures.push(pr);
                }
                let [ox, oy] = points[0];
                let rel: Vec<Value> = points
                    .iter()
                    .map(|[x, y]| json!([num(x - ox), num(y - oy)]))
                    .collect();
                let (mut x0, mut y0, mut x1, mut y1) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for [x, y] in points.iter() {
                    x0 = x0.min(x - ox);
                    y0 = y0.min(y - oy);
                    x1 = x1.max(x - ox);
                    y1 = y1.max(y - oy);
                }
                e.set("points", Value::Array(rel));
                e.set(
                    "pressures",
                    json!(pressures.iter().map(|p| num(*p)).collect::<Vec<_>>()),
                );
                e.set("width", num(x1 - x0));
                e.set("height", num(y1 - y0));
                bump(&mut e, rng);
                out.changed.push(e);
            }
            Gesture::Moving {
                start,
                originals,
                moved,
            } => {
                let (dx, dy) = (p[0] - start[0], p[1] - start[1]);
                if !*moved && dx.hypot(dy) * zoom < 2.0 {
                    return out;
                }
                *moved = true;
                // Shift moves along one axis.
                let (dx, dy) = if mods.shift {
                    if dx.abs() > dy.abs() {
                        (dx, 0.0)
                    } else {
                        (0.0, dy)
                    }
                } else {
                    (dx, dy)
                };
                let ids: Vec<&str> = originals.iter().map(Element::id).collect();
                let mut changed: Vec<Element> = Vec::new();
                for o in originals.iter() {
                    let mut e = o.clone();
                    e.set("x", num(o.x() + dx));
                    e.set("y", num(o.y() + dy));
                    // An arrow dragged without what it is attached to lets go.
                    if o.kind() == ElementType::Arrow {
                        for k in ["startBinding", "endBinding"] {
                            let attached_to = o
                                .get(k)
                                .and_then(|b| b.get("elementId"))
                                .and_then(Value::as_str);
                            if attached_to.is_some_and(|t| !ids.contains(&t)) {
                                e.set(k, Value::Null);
                            }
                        }
                    }
                    changed.push(e);
                }
                binding::update_arrows(scene, &mut changed);
                for mut e in changed {
                    bump(&mut e, rng);
                    out.changed.push(e);
                }
                self.drag_frame(scene, p);
            }
            Gesture::Resizing {
                handle,
                frame,
                originals,
                offset,
            } => {
                // Labels aren't scaled: they are laid out in their shapes again.
                let ids: Vec<&str> = originals.iter().map(Element::id).collect();
                let shapes: Vec<Element> = originals
                    .iter()
                    .filter(|e| e.container_id().is_none_or(|c| !ids.contains(&c)))
                    .cloned()
                    .collect();
                // A text keeps its proportions, a note's corner too unless Shift.
                let mode = ResizeMode {
                    keep_ratio: (mods.shift
                        != crate::sticky::proportional_by_default(&shapes, *handle))
                        || shapes.iter().any(|e| e.kind() == ElementType::Text),
                    from_center: mods.alt,
                };
                // The pointer in the frame's space, plus the offset, turned back.
                let local = geom::rotate(p, frame.center(), -frame.angle);
                let target = geom::rotate(
                    [local[0] + offset[0], local[1] + offset[1]],
                    frame.center(),
                    frame.angle,
                );
                let measure = &self.measure;
                let mut changed =
                    crate::sticky::resize_alone(originals, frame, *handle, target, mode, measure)
                        .unwrap_or_else(|| {
                            let mut c = transform::resize(&shapes, frame, *handle, target, mode);
                            crate::sticky::after_resize(originals, &mut c, *handle, mode, measure);
                            c
                        });
                self.relayout(scene, &mut changed);
                binding::update_arrows(scene, &mut changed);
                for mut e in changed {
                    bump(&mut e, rng);
                    out.changed.push(e);
                }
            }
            Gesture::Rotating {
                start,
                frame,
                originals,
            } => {
                let mut changed = transform::rotate(originals, frame, *start, p, mods.shift);
                binding::update_arrows(scene, &mut changed);
                for mut e in changed {
                    bump(&mut e, rng);
                    out.changed.push(e);
                }
            }
            Gesture::BoxSelecting {
                start,
                current,
                before,
            } => {
                *current = p;
                let inside = hit::elements_in(scene, [start[0], start[1], p[0], p[1]]);
                let mut sel = before.clone();
                let mut groups = Vec::new();
                for e in inside {
                    if let Some(g) = outer_group(e) {
                        groups.push(g.to_string());
                    }
                    if !sel.iter().any(|s| s == e.id()) {
                        sel.push(e.id().to_string());
                    }
                }
                // A group is selected whole.
                for e in scene.visible() {
                    if outer_group(e).is_some_and(|g| groups.iter().any(|x| x == g))
                        && !sel.iter().any(|s| s == e.id())
                    {
                        sel.push(e.id().to_string());
                    }
                }
                self.selection = sel;
            }
            Gesture::Erasing { ids } => {
                if let Some(e) = hit::element_at(scene, p, zoom, false) {
                    ids.insert(e.id().to_string());
                }
            }
        }
        out
    }

    /// The pointer went up at `p`.
    pub fn pointer_up(&mut self, scene: &Scene, p: Point, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        self.frame_to_highlight = None;
        let Some(g) = self.gesture.take() else {
            return out;
        };
        match g {
            Gesture::Creating { id, origin } => {
                let Some(e) = scene.element(&id) else {
                    return out;
                };
                if crate::sticky::is_note(e) {
                    return self.finish_sticky_note(scene, e, origin, rng);
                }
                if e.width() < 1.0 || e.height() < 1.0 {
                    // A click, not a drag: nothing made.
                    let mut gone = e.clone();
                    gone.set("isDeleted", json!(true));
                    bump(&mut gone, rng);
                    out.changed.push(gone);
                    out.done = true;
                    return out;
                }
                // A frame takes in what is wholly in it.
                if frame::is_frame(e) {
                    for mut c in self.frame_drawn(scene, e) {
                        bump(&mut c, rng);
                        out.changed.push(c);
                    }
                }
                self.selection = vec![id];
                out.done = true;
                if !self.tool_locked {
                    self.tool = Tool::Selection;
                    out.tool_changed = true;
                }
            }
            Gesture::Linear {
                id,
                mut points,
                dragged,
            } => {
                if dragged || points.len() > 2 {
                    if !dragged {
                        // Point by point: keep going until it is ended.
                        self.gesture = Some(Gesture::Linear {
                            id,
                            points,
                            dragged,
                        });
                        return out;
                    }
                    points.dedup();
                    return self.finish_linear(scene, &id, rng);
                }
                // A click: the line goes on point by point.
                points[1] = p;
                self.gesture = Some(Gesture::Linear {
                    id,
                    points,
                    dragged: false,
                });
            }
            Gesture::Pen { .. } => out.done = true,
            Gesture::Moving { moved, .. } => {
                // Dropped in a frame, or out of one.
                if moved {
                    for mut c in self.dropped(scene, p) {
                        bump(&mut c, rng);
                        out.changed.push(c);
                    }
                }
                out.done = moved;
            }
            Gesture::Resizing { .. } => {
                for mut c in self.frames_resized(scene) {
                    bump(&mut c, rng);
                    out.changed.push(c);
                }
                out.done = true;
            }
            Gesture::Rotating { .. } => out.done = true,
            Gesture::BoxSelecting { .. } => {}
            Gesture::Erasing { ids } => {
                let mut gone = Vec::new();
                // A frame's children go with it.
                let children = frame::erased_children(scene, &ids);
                for e in scene
                    .visible()
                    .filter(|e| ids.contains(e.id()) || children.contains(e.id()))
                {
                    gone.push(e.id().to_string());
                    // Labels go with what they label.
                    for b in e.bound_elements() {
                        if b.kind == "text" {
                            gone.push(b.id);
                        }
                    }
                }
                for e in scene.visible().filter(|e| gone.iter().any(|g| g == e.id())) {
                    let mut d = e.clone();
                    d.set("isDeleted", json!(true));
                    bump(&mut d, rng);
                    out.changed.push(d);
                }
                out.done = !out.changed.is_empty();
            }
        }
        out
    }

    /// Ends a line or arrow made point by point (Escape, Enter, or clicking
    /// its last point again).
    fn finish_linear(&mut self, scene: &Scene, id: &str, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        self.gesture = None;
        let Some(e) = scene.element(id) else {
            return out;
        };
        let mut e = e.clone();
        let mut pts: Vec<Point> = e
            .points()
            .iter()
            .map(|[x, y]| [x + e.x(), y + e.y()])
            .collect();
        pts.dedup();
        if pts.len() < 2 {
            e.set("isDeleted", json!(true));
        } else {
            new::normalize_points(&mut e, &pts);
            self.selection = vec![id.to_string()];
            // An arrow's ends attach to the shapes they are on.
            if e.kind() == ElementType::Arrow {
                let (a, start) = binding::attach(&e, true, pts[0], scene, self.zoom);
                let (a, end) = binding::attach(&a, false, pts[pts.len() - 1], scene, self.zoom);
                let mut shapes: Vec<Element> = start.into_iter().collect();
                if let Some(end) = end {
                    match shapes.iter_mut().find(|s| s.id() == end.id()) {
                        // Both ends on one shape: one list with it.
                        Some(s) => *s = end,
                        None => shapes.push(end),
                    }
                }
                e = binding::follow(&a, scene, &shapes);
                for mut s in shapes {
                    bump(&mut s, rng);
                    out.changed.push(s);
                }
            }
        }
        bump(&mut e, rng);
        out.changed.push(e);
        out.done = true;
        if !self.tool_locked {
            self.tool = Tool::Selection;
            out.tool_changed = true;
        }
        out
    }

    /// Ends drawing a sticky note: sized as the click or drag made it, then
    /// its text is typed at once (unless the tool stays).
    fn finish_sticky_note(
        &mut self,
        scene: &Scene,
        e: &Element,
        origin: Point,
        rng: &mut dyn Random,
    ) -> Output {
        let font = (self.style.font_size, self.style.font_family);
        let mut note = crate::sticky::finish_drawn(e, origin, self.zoom, font);
        bump(&mut note, rng);
        let id = note.id().to_string();
        let mut out = Output {
            changed: vec![note.clone()],
            done: true,
            ..Output::default()
        };
        if self.tool_locked {
            self.selection.clear();
            return out;
        }
        self.tool = Tool::Selection;
        out.tool_changed = true;
        let mut s2 = scene.clone();
        if let Some(slot) = s2.elements.iter_mut().find(|x| x.id() == id) {
            *slot = note;
        }
        let label = self.edit_label(&s2, &id, rng);
        for c in label.changed {
            crate::sticky::upsert(&mut out.changed, c);
        }
        out.edit_text = label.edit_text;
        out
    }

    /// Escape: a line made point by point ends; else the selection clears.
    pub fn escape(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        if let Some(Gesture::Linear { id, mut points, .. }) = self.gesture.take() {
            points.pop();
            let mut out = Output::default();
            if let Some(mut e) = scene.element(&id).cloned() {
                new::normalize_points(&mut e, &points);
                out.changed.push(e);
            }
            // The element now has its points without the pointer's.
            let mut s2 = scene.clone();
            for c in &out.changed {
                if let Some(slot) = s2.elements.iter_mut().find(|x| x.id() == c.id()) {
                    *slot = c.clone();
                }
            }
            let mut fin = self.finish_linear(&s2, &id, rng);
            fin.changed.splice(0..0, out.changed);
            return fin;
        }
        self.gesture = None;
        self.frame_to_highlight = None;
        self.selection.clear();
        Output::default()
    }

    /// Ends text editing with `text` (measured with `measure`); an empty
    /// text is deleted.
    pub fn finish_text(
        &mut self,
        scene: &Scene,
        text: &str,
        measure: new::Measure,
        rng: &mut dyn Random,
    ) -> Output {
        let mut out = Output::default();
        let Some(id) = self.editing_text.take() else {
            return out;
        };
        let Some(e) = scene.element(&id) else {
            return out;
        };
        if frame::is_frame(e) {
            let mut f = frame::renamed(e, text);
            bump(&mut f, rng);
            out.changed.push(f);
            out.done = true;
            return out;
        }
        let mut e = e.clone();
        let text = text.replace("\r\n", "\n");
        let container = e
            .container_id()
            .and_then(|c| scene.element(c))
            .filter(|c| !c.is_deleted());
        if let Some(c) = container {
            if text.trim().is_empty() {
                // An empty label goes, and its container forgets it.
                e.set("isDeleted", json!(true));
                let mut c = c.clone();
                let kept: Vec<Value> = c
                    .bound_elements()
                    .iter()
                    .filter(|b| b.id != id)
                    .map(|b| json!({"id": b.id, "type": b.kind}))
                    .collect();
                c.set(
                    "boundElements",
                    if kept.is_empty() {
                        Value::Null
                    } else {
                        Value::Array(kept)
                    },
                );
                bump(&mut c, rng);
                out.changed.push(c);
            } else {
                e.set("originalText", json!(text));
                let (t, grown) = label::layout(c, &e, &self.measure);
                e = t;
                if let Some(mut g) = grown {
                    bump(&mut g, rng);
                    out.changed.push(g);
                }
            }
            bump(&mut e, rng);
            out.changed.push(e);
            out.done = true;
            return out;
        }
        if text.trim().is_empty() {
            e.set("isDeleted", json!(true));
            self.selection.retain(|s| *s != id);
        } else {
            let (w, h) = new::text_size(
                &text,
                e.font_size(),
                e.font_family(),
                e.line_height(),
                measure,
            );
            // Centred and right-aligned texts grow from their side.
            let dx = match e.text_align() {
                "center" => (e.width() - w) / 2.0,
                "right" => e.width() - w,
                _ => 0.0,
            };
            if e.width() > 0.0 {
                e.set("x", num(e.x() + dx));
            }
            e.set("text", json!(text));
            e.set("originalText", json!(text));
            e.set("width", num(w));
            e.set("height", num(h));
        }
        bump(&mut e, rng);
        out.changed.push(e);
        out.done = true;
        if !self.tool_locked && self.tool == Tool::Text {
            self.tool = Tool::Selection;
            out.tool_changed = true;
        }
        out
    }

    /// A double click: edits the text or label there (a shape's label is
    /// made if it has none), or starts a new text.
    pub fn double_click(
        &mut self,
        scene: &Scene,
        p: Point,
        zoom: f64,
        rng: &mut dyn Random,
    ) -> Output {
        self.gesture = None;
        match hit::element_at(scene, p, zoom, false) {
            Some(e) if e.kind() == ElementType::Text => self.edit_text(e.id()),
            Some(e) if label::can_hold(e) => {
                let id = e.id().to_string();
                self.edit_label(scene, &id, rng)
            }
            // On a frame's name: it is renamed.
            Some(e) if frame::is_frame(e) && frame::on_name(e, p, zoom, &self.measure, 0.0) => {
                self.edit_text(e.id())
            }
            Some(_) => Output::default(),
            None if hit::container_at(scene, p, zoom).is_some() => {
                let id = hit::container_at(scene, p, zoom)
                    .expect("checked")
                    .id()
                    .to_string();
                self.edit_label(scene, &id, rng)
            }
            None => {
                let tool = self.tool;
                self.tool = Tool::Text;
                let out = self.pointer_down(scene, p, None, Mods::default(), zoom, rng);
                self.tool = tool;
                out
            }
        }
    }

    /// Enter: edits the selected text, or the selected shape's label.
    pub fn enter(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let selected = self.selected(scene);
        match selected.as_slice() {
            [e] if e.kind() == ElementType::Text => {
                let id = e.id().to_string();
                self.edit_text(&id)
            }
            [e] if label::can_hold(e) => {
                let id = e.id().to_string();
                self.edit_label(scene, &id, rng)
            }
            [e] if frame::is_frame(e) => {
                let id = e.id().to_string();
                self.edit_text(&id)
            }
            _ => Output::default(),
        }
    }

    fn edit_text(&mut self, id: &str) -> Output {
        self.selection = vec![id.to_string()];
        self.editing_text = Some(id.to_string());
        Output {
            edit_text: Some(id.to_string()),
            ..Output::default()
        }
    }

    /// Edits a shape's or arrow's label, making it if there's none.
    pub fn edit_label(&mut self, scene: &Scene, container: &str, rng: &mut dyn Random) -> Output {
        let Some(c) = scene.element(container) else {
            return Output::default();
        };
        let existing = c
            .bound_elements()
            .iter()
            .filter(|b| b.kind == "text")
            .find_map(|b| scene.element(&b.id))
            .filter(|t| !t.is_deleted())
            .map(|t| t.id().to_string());
        if let Some(id) = existing {
            let out = self.edit_text(&id);
            self.selection = vec![c.id().to_string()];
            return out;
        }
        let (t, mut c2) = label::new_label(c, &self.style, rng);
        let (mut t, _) = label::layout(&c2, &t, &self.measure);
        // Just above its container in the stacking order.
        let next = scene
            .elements
            .iter()
            .filter_map(Element::index)
            .filter(|i| Some(*i) > c.index())
            .min();
        t.set(
            "index",
            order::key_between(c.index(), next).map_or(Value::Null, Value::String),
        );
        bump(&mut c2, rng);
        let id = t.id().to_string();
        self.selection = vec![c.id().to_string()];
        self.editing_text = Some(id.clone());
        Output {
            changed: vec![c2, t],
            done: false,
            edit_text: Some(id),
            tool_changed: false,
        }
    }

    /// Lays out again the labels of containers among `changed` (they
    /// changed size), and labels among them (their font changed).
    fn relayout(&self, scene: &Scene, changed: &mut Vec<Element>) {
        let containers: Vec<Element> = changed
            .iter()
            .filter(|e| label::can_hold(e))
            .cloned()
            .collect();
        for c in containers {
            for b in c.bound_elements().iter().filter(|b| b.kind == "text") {
                let current = changed
                    .iter()
                    .find(|e| e.id() == b.id)
                    .cloned()
                    .or_else(|| scene.element(&b.id).cloned());
                let Some(t) = current.filter(|t| !t.is_deleted()) else {
                    continue;
                };
                let (t, _) = label::layout(&c, &t, &self.measure);
                match changed.iter_mut().find(|e| e.id() == t.id()) {
                    Some(slot) => *slot = t,
                    None => changed.push(t),
                }
            }
        }
        let labels: Vec<Element> = changed
            .iter()
            .filter(|e| e.kind() == ElementType::Text && e.container_id().is_some())
            .cloned()
            .collect();
        for t in labels {
            let Some(c) = t.container_id().and_then(|c| {
                changed
                    .iter()
                    .find(|e| e.id() == c)
                    .cloned()
                    .or_else(|| scene.element(c).cloned())
            }) else {
                continue;
            };
            let (t, grown) = label::layout(&c, &t, &self.measure);
            if let Some(slot) = changed.iter_mut().find(|e| e.id() == t.id()) {
                *slot = t;
            }
            if let Some(g) = grown {
                match changed.iter_mut().find(|e| e.id() == g.id()) {
                    Some(slot) => *slot = g,
                    None => changed.push(g),
                }
            }
        }
    }

    /// Deletes what is selected (with labels).
    pub fn delete_selected(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let mut changed = self.moving_set(scene);
        for e in &mut changed {
            e.set("isDeleted", json!(true));
        }
        self.selection.clear();
        // A deleted frame's children stay, selected.
        self.release_children(scene, &mut changed);
        binding::update_arrows(scene, &mut changed);
        for mut e in changed {
            bump(&mut e, rng);
            out.changed.push(e);
        }
        out.done = !out.changed.is_empty();
        out
    }

    pub fn select_all(&mut self, scene: &Scene) {
        self.selection = scene
            .visible()
            .filter(|e| e.container_id().is_none() && !e.is_locked())
            .map(|e| e.id().to_string())
            .collect();
    }

    /// Moves what is selected by `dx`, `dy` (arrow keys).
    pub fn nudge(&mut self, scene: &Scene, dx: f64, dy: f64, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let mut changed = self.dragged_set(scene);
        for e in &mut changed {
            e.set("x", num(e.x() + dx));
            e.set("y", num(e.y() + dy));
        }
        binding::update_arrows(scene, &mut changed);
        for mut e in changed {
            bump(&mut e, rng);
            out.changed.push(e);
        }
        out.done = !out.changed.is_empty();
        out
    }

    /// Copies of what is selected, a little further (Ctrl+D), selected
    /// instead; groups and labels copied with new ids.
    pub fn duplicate(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let originals = self.dragged_set(scene);
        self.place_copies(scene, &originals, [10.0, 10.0], rng)
    }

    /// What is selected, as Excalidraw puts it on the clipboard (with labels
    /// and the pictures it shows).
    pub fn copy(&self, scene: &Scene) -> Option<String> {
        let set = self.dragged_set(scene);
        if set.is_empty() {
            return None;
        }
        let refs: Vec<&Element> = set.iter().collect();
        Some(crate::file::write_clipboard(&refs, &scene.files))
    }

    /// Pastes elements (from the clipboard: Excalidraw's or ours), centred
    /// on `at`; they get new ids and are selected.
    pub fn paste(
        &mut self,
        scene: &Scene,
        pasted: &[Element],
        at: Point,
        rng: &mut dyn Random,
    ) -> Output {
        let visible: Vec<Element> = pasted.iter().filter(|e| !e.is_deleted()).cloned().collect();
        if visible.is_empty() {
            return Output::default();
        }
        let [x1, y1, x2, y2] = bounds::common_bounds(visible.iter());
        let offset = [at[0] - (x1 + x2) / 2.0, at[1] - (y1 + y2) / 2.0];
        self.tool = Tool::Selection;
        let mut out = self.place_copies(scene, &visible, offset, rng);
        self.paste_into_frame(scene, &mut out.changed, at);
        out.tool_changed = true;
        out
    }

    /// Copies of `originals` moved by `offset`, on top, with new ids (and
    /// what refers to them following), selected.
    fn place_copies(
        &mut self,
        scene: &Scene,
        originals: &[Element],
        offset: Point,
        rng: &mut dyn Random,
    ) -> Output {
        let mut out = Output::default();
        if originals.is_empty() {
            return out;
        }
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut groups: HashMap<String, String> = HashMap::new();
        for o in originals {
            ids.insert(o.id().to_string(), rng.id());
            for g in o.group_ids() {
                if !groups.contains_key(g) {
                    groups.insert(g.to_string(), rng.id());
                }
            }
        }
        let mut index = scene
            .elements
            .iter()
            .filter_map(Element::index)
            .max()
            .map(str::to_string);
        let mut selection = Vec::new();
        let mut ordered: Vec<&Element> = originals.iter().collect();
        ordered.sort_by(|a, b| a.index().cmp(&b.index()));
        for o in ordered {
            let mut e = o.clone();
            let new_id = ids[o.id()].clone();
            e.set("id", json!(new_id));
            e.set("x", num(o.x() + offset[0]));
            e.set("y", num(o.y() + offset[1]));
            e.set("seed", json!(rng.seed()));
            e.set("version", json!(1));
            e.set("isDeleted", json!(false));
            index = order::key_between(index.as_deref(), None);
            e.set("index", index.clone().map_or(Value::Null, Value::String));
            let gids: Vec<Value> = o.group_ids().iter().map(|g| json!(groups[*g])).collect();
            e.set("groupIds", Value::Array(gids));
            // References inside the copies point at the copies; the rest go.
            let container = o.container_id().and_then(|c| ids.get(c)).cloned();
            if o.container_id().is_some() {
                e.set("containerId", container.map_or(Value::Null, Value::String));
            }
            let bound: Vec<Value> = o
                .bound_elements()
                .iter()
                .filter_map(|b| ids.get(&b.id).map(|n| json!({"id": n, "type": b.kind})))
                .collect();
            e.set(
                "boundElements",
                if bound.is_empty() {
                    Value::Null
                } else {
                    Value::Array(bound)
                },
            );
            for k in ["startBinding", "endBinding"] {
                let rebound = o.get(k).and_then(Value::as_object).and_then(|b| {
                    let target = ids.get(b.get("elementId")?.as_str()?)?;
                    let mut b = b.clone();
                    b.insert("elementId".into(), json!(target));
                    Some(Value::Object(b))
                });
                if o.get(k).is_some() {
                    e.set(k, rebound.unwrap_or(Value::Null));
                }
            }
            // In a frame copied with it, the copy is in the frame's copy.
            if let Some(f) = o.frame_id().and_then(|f| ids.get(f)) {
                e.set("frameId", json!(f));
            }
            bump(&mut e, rng);
            if o.container_id().is_none_or(|c| !ids.contains_key(c)) {
                selection.push(new_id);
            }
            out.changed.push(e);
        }
        self.selection = selection;
        out.done = true;
        out
    }

    /// Sets a property on what is selected (and on new elements): a style
    /// change from the properties panel.
    pub fn set_property(
        &mut self,
        scene: &Scene,
        key: &str,
        value: Value,
        rng: &mut dyn Random,
    ) -> Output {
        match key {
            "strokeColor" | "backgroundColor" => {
                let selected = self.selected(scene);
                crate::sticky::pick_color(
                    &mut self.style,
                    self.tool,
                    &selected,
                    scene,
                    key,
                    &value,
                );
            }
            "fillStyle" => self.style.fill_style = value.as_str().unwrap_or_default().to_string(),
            "strokeWidth" => self.style.stroke_width = value.as_f64().unwrap_or(2.0),
            "strokeStyle" => {
                self.style.stroke_style = value.as_str().unwrap_or_default().to_string()
            }
            "roughness" => self.style.roughness = value.as_f64().unwrap_or(1.0),
            "opacity" => self.style.opacity = value.as_f64().unwrap_or(100.0),
            "fontFamily" => self.style.font_family = value.as_f64().unwrap_or(5.0) as u32,
            "fontSize" => self.style.font_size = value.as_f64().unwrap_or(20.0),
            "textAlign" => self.style.text_align = value.as_str().unwrap_or("left").to_string(),
            "startArrowhead" => self.style.start_arrowhead = value.as_str().map(str::to_string),
            "endArrowhead" => self.style.end_arrowhead = value.as_str().map(str::to_string),
            _ => {}
        }
        let mut out = Output::default();
        let selected: Vec<String> = self.selection.clone();
        let mut changed = Vec::new();
        for e in scene.visible() {
            // A shape's label takes its text properties.
            let label_of_selected = e
                .container_id()
                .is_some_and(|c| selected.iter().any(|s| s == c));
            if !(selected.iter().any(|s| s == e.id()) || label_of_selected) || e.is_locked() {
                continue;
            }
            let applies = match key {
                "fontFamily" | "fontSize" | "textAlign" => e.kind() == ElementType::Text,
                "startArrowhead" | "endArrowhead" => e.kind() == ElementType::Arrow,
                "backgroundColor" | "fillStyle" => e.kind() != ElementType::Text,
                _ => !label_of_selected || key == "strokeColor" || key == "opacity",
            };
            if !applies {
                continue;
            }
            let mut c = e.clone();
            c.set(key, value.clone());
            if key == "fontFamily" {
                let lh = crate::fonts::get(value.as_f64().unwrap_or(5.0) as u32).line_height;
                c.set("lineHeight", num(lh));
            }
            crate::sticky::keep_property(e, &mut c, scene, key);
            if c == *e {
                continue;
            }
            changed.push(c);
        }
        if matches!(key, "fontFamily" | "fontSize") {
            self.relayout(scene, &mut changed);
        }
        for mut c in changed {
            bump(&mut c, rng);
            out.changed.push(c);
        }
        out.done = !out.changed.is_empty();
        out
    }

    /// Rounded or sharp edges for what is selected (and new elements).
    pub fn set_round_edges(&mut self, scene: &Scene, round: bool, rng: &mut dyn Random) -> Output {
        self.style.round_edges = round;
        let mut out = Output::default();
        for e in self.selected(scene) {
            if !crate::shape::can_change_roundness(&e.kind()) || e.is_locked() {
                continue;
            }
            let value = if round {
                let kind = if matches!(e.kind(), ElementType::Rectangle | ElementType::Image) {
                    crate::scene::roundness::ADAPTIVE_RADIUS
                } else {
                    crate::scene::roundness::PROPORTIONAL_RADIUS
                };
                json!({"type": kind})
            } else {
                Value::Null
            };
            let mut c = e.clone();
            c.set("roundness", value);
            bump(&mut c, rng);
            out.changed.push(c);
        }
        out.done = !out.changed.is_empty();
        out
    }

    /// Moves what is selected in the stacking order: one step (`forward`),
    /// or all the way (`all`).
    pub fn reorder(
        &mut self,
        scene: &Scene,
        forward: bool,
        all: bool,
        rng: &mut dyn Random,
    ) -> Output {
        let mut out = Output::default();
        let moving: BTreeSet<String> = self
            .dragged_set(scene)
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        if moving.is_empty() {
            return out;
        }
        let order: Vec<&Element> = scene.elements.iter().collect();
        let others: Vec<&Element> = order
            .iter()
            .copied()
            .filter(|e| !moving.contains(e.id()))
            .collect();
        let picked: Vec<&Element> = order
            .iter()
            .copied()
            .filter(|e| moving.contains(e.id()))
            .collect();
        // Where they go among the others: their neighbours' indices.
        let (below, above) = if all {
            if forward {
                (others.last().and_then(|e| e.index()), None)
            } else {
                (None, others.first().and_then(|e| e.index()))
            }
        } else {
            let first = order
                .iter()
                .position(|e| moving.contains(e.id()))
                .unwrap_or(0);
            let last = order
                .iter()
                .rposition(|e| moving.contains(e.id()))
                .unwrap_or(0);
            if forward {
                // Past the next element that is there above the topmost.
                let next = order[last + 1..]
                    .iter()
                    .position(|e| !e.is_deleted() && !moving.contains(e.id()));
                match next {
                    Some(k) => {
                        let at = others
                            .iter()
                            .position(|o| o.id() == order[last + 1 + k].id())
                            .unwrap_or(0);
                        (
                            others[at].index(),
                            others.get(at + 1).and_then(|e| e.index()),
                        )
                    }
                    None => return out,
                }
            } else {
                let prev = order[..first]
                    .iter()
                    .rposition(|e| !e.is_deleted() && !moving.contains(e.id()));
                match prev {
                    Some(k) => {
                        let at = others
                            .iter()
                            .position(|o| o.id() == order[k].id())
                            .unwrap_or(0);
                        (
                            at.checked_sub(1).and_then(|i| others[i].index()),
                            others[at].index(),
                        )
                    }
                    None => return out,
                }
            }
        };
        let Some(keys) = order::keys_between(below, above, picked.len()) else {
            return out;
        };
        for (e, k) in picked.iter().zip(keys) {
            let mut c = (*e).clone();
            c.set("index", json!(k));
            bump(&mut c, rng);
            out.changed.push(c);
        }
        out.done = true;
        out
    }

    /// Groups what is selected (Ctrl+G).
    pub fn group(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let selected = self.selected(scene);
        if selected.len() < 2 {
            return out;
        }
        let g = rng.id();
        for e in selected {
            let mut c = e.clone();
            let mut ids: Vec<Value> = e.group_ids().iter().map(|x| json!(x)).collect();
            ids.push(json!(g));
            c.set("groupIds", Value::Array(ids));
            bump(&mut c, rng);
            out.changed.push(c);
        }
        out.done = true;
        out
    }

    /// Ungroups what is selected: its outermost groups (Ctrl+Shift+G).
    pub fn ungroup(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let groups: BTreeSet<String> = self
            .selected(scene)
            .iter()
            .filter_map(|e| outer_group(e).map(str::to_string))
            .collect();
        for e in self.selected(scene) {
            let ids: Vec<&str> = e.group_ids();
            if ids.last().is_some_and(|g| groups.contains(*g)) {
                let mut c = e.clone();
                let kept: Vec<Value> = ids[..ids.len() - 1].iter().map(|x| json!(x)).collect();
                c.set("groupIds", Value::Array(kept));
                bump(&mut c, rng);
                out.changed.push(c);
            }
        }
        out.done = !out.changed.is_empty();
        out
    }

    /// Locks or unlocks what is selected (Ctrl+Shift+L).
    pub fn toggle_lock(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let selected = self.selected(scene);
        let lock = !selected.iter().all(|e| e.is_locked());
        for e in selected {
            let mut c = e.clone();
            c.set("locked", json!(lock));
            bump(&mut c, rng);
            out.changed.push(c);
        }
        if lock {
            self.selection.clear();
        }
        out.done = !out.changed.is_empty();
        out
    }

    /// Lines up what is selected: `left`, `center`, `right`, `top`,
    /// `middle` or `bottom` of the selection's box.
    pub fn align(&mut self, scene: &Scene, how: &str, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let units = self.units(scene);
        if units.len() < 2 {
            return out;
        }
        let all: Vec<&Element> = units.iter().flatten().collect();
        let [x1, y1, x2, y2] = bounds::common_bounds(all.iter().copied());
        for unit in &units {
            let [ux1, uy1, ux2, uy2] = bounds::common_bounds(unit.iter());
            let (dx, dy) = match how {
                "left" => (x1 - ux1, 0.0),
                "right" => (x2 - ux2, 0.0),
                "center" => ((x1 + x2) / 2.0 - (ux1 + ux2) / 2.0, 0.0),
                "top" => (0.0, y1 - uy1),
                "bottom" => (0.0, y2 - uy2),
                _ => (0.0, (y1 + y2) / 2.0 - (uy1 + uy2) / 2.0),
            };
            if dx == 0.0 && dy == 0.0 {
                continue;
            }
            for e in unit {
                let mut c = e.clone();
                c.set("x", num(e.x() + dx));
                c.set("y", num(e.y() + dy));
                bump(&mut c, rng);
                out.changed.push(c);
            }
        }
        arrows_follow(scene, &mut out, rng);
        out.done = !out.changed.is_empty();
        out
    }

    /// Spaces what is selected evenly, across (`horizontal`) or down.
    pub fn distribute(&mut self, scene: &Scene, horizontal: bool, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let mut units: Vec<(Vec<Element>, [f64; 4])> = self
            .units(scene)
            .into_iter()
            .map(|u| {
                let b = bounds::common_bounds(u.iter());
                (u, b)
            })
            .collect();
        if units.len() < 3 {
            return out;
        }
        let (lo, hi) = if horizontal { (0, 2) } else { (1, 3) };
        units.sort_by(|a, b| a.1[lo].total_cmp(&b.1[lo]));
        let start = units.iter().map(|u| u.1[lo]).fold(f64::INFINITY, f64::min);
        let end = units
            .iter()
            .map(|u| u.1[hi])
            .fold(f64::NEG_INFINITY, f64::max);
        let total: f64 = units.iter().map(|u| u.1[hi] - u.1[lo]).sum();
        let gap = (end - start - total) / (units.len() - 1) as f64;
        let mut at = start;
        for (unit, b) in &units {
            let d = at - b[lo];
            at += b[hi] - b[lo] + gap;
            if d.abs() < 1e-9 {
                continue;
            }
            for e in unit {
                let mut c = e.clone();
                if horizontal {
                    c.set("x", num(e.x() + d));
                } else {
                    c.set("y", num(e.y() + d));
                }
                bump(&mut c, rng);
                out.changed.push(c);
            }
        }
        arrows_follow(scene, &mut out, rng);
        out.done = !out.changed.is_empty();
        out
    }

    /// What is selected, as units that move together: a group, or an
    /// element with its label.
    fn units(&self, scene: &Scene) -> Vec<Vec<Element>> {
        let set = self.moving_set(scene);
        let mut units: Vec<(Option<String>, Vec<Element>)> = Vec::new();
        for e in &set {
            if e.container_id()
                .is_some_and(|c| set.iter().any(|o| o.id() == c))
            {
                continue;
            }
            let key = outer_group(e).map(str::to_string);
            let mut members = vec![e.clone()];
            for b in e.bound_elements() {
                if let Some(t) = set.iter().find(|o| o.id() == b.id) {
                    members.push(t.clone());
                }
            }
            match key
                .as_ref()
                .and_then(|k| units.iter_mut().find(|u| u.0.as_deref() == Some(k)))
            {
                Some(u) => u.1.extend(members),
                None => units.push((key, members)),
            }
        }
        units.into_iter().map(|u| u.1).collect()
    }
}

#[cfg(test)]
mod tests;
