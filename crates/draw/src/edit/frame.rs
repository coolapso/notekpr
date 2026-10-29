//! Frames, as Excalidraw has them (`frame.ts`, and where `App.tsx` and its
//! actions use it; MIT, see `PORTED.md`): what is in a frame — its
//! children, by their `frameId` — joins it when made or dropped in it,
//! leaves it when dragged out, moves with it and is clipped to it; and the
//! frame's name above it.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Value, json};

use super::new::Measure;
use super::transform::Handle;
use super::{Editor, Gesture, Output, Random, Tool, binding, bump, hit};
use crate::bounds::{self, Bounds};
use crate::fonts;
use crate::geom::{self, Point};
use crate::js;
use crate::order;
use crate::scene::{Element, ElementType, Scene, num};
use crate::shape;

/// `FRAME_STYLE`: a frame's outline…
pub const STROKE: &str = "#bbb";
pub const STROKE_WIDTH: f64 = 2.0;
pub const RADIUS: f64 = 8.0;
/// …and its name: this many screen pixels above it, this size whatever
/// the zoom.
pub const NAME_OFFSET_Y: f64 = 3.0;
pub const NAME_FONT_SIZE: f64 = 14.0;
pub const NAME_LINE_HEIGHT: f64 = 1.25;
pub const NAME_COLOR: &str = "#999999";
pub const NAME_COLOR_DARK: &str = "#7a7a7a";
/// What dark mode turns into [`NAME_COLOR_DARK`] (`removeDarkModeFilter`):
/// an export's names are texts, which dark mode turns.
const NAME_COLOR_UNDARK: &str = "#868686";
/// Where what is drawn or dragged will go is shown in this colour.
pub const HIGHLIGHT: &str = "#0076ff";
/// `PRECISION`: a frame holds what reaches a hair beyond its edges.
const PRECISION: f64 = 10e-5;

pub fn is_frame(e: &Element) -> bool {
    matches!(e.kind(), ElementType::Frame | ElementType::MagicFrame)
}

/// `getFrameLikeTitle`: its name, else "Frame".
pub fn title(e: &Element) -> &str {
    match e.get("name") {
        Some(Value::String(name)) => name,
        _ if e.kind() == ElementType::MagicFrame => "AI Frame",
        _ => "Frame",
    }
}

/// A frame renamed: its name without spaces around it, none if empty
/// (`resetEditingFrame`).
pub fn renamed(frame: &Element, name: &str) -> Element {
    let mut f = frame.clone();
    let name = name.replace(['\r', '\n'], " ");
    let name = name.trim();
    f.set(
        "name",
        if name.is_empty() {
            Value::Null
        } else {
            json!(name)
        },
    );
    f
}

/// The font names are shown in on screen (the app's own: Assistant).
pub fn name_font(size: f64) -> String {
    format!(
        "{}px {}",
        js::number_to_string(size),
        fonts::css_family(fonts::ASSISTANT)
    )
}

/// Where a frame's name is on screen, in the drawing at `zoom`
/// (`frameNameBoundsCache`): `[x, y, width, height]`, no wider than the
/// frame.
pub fn name_bounds(frame: &Element, zoom: f64, measure: Measure) -> [f64; 4] {
    let h = NAME_FONT_SIZE * NAME_LINE_HEIGHT;
    let w = js::min(
        measure(&name_font(NAME_FONT_SIZE), title(frame)),
        frame.width() * zoom,
    );
    [
        frame.x(),
        frame.y() - (h + NAME_OFFSET_Y) / zoom,
        w / zoom,
        h / zoom,
    ]
}

/// Whether `p` is on a frame's name, or within `slack` of it.
pub fn on_name(frame: &Element, p: Point, zoom: f64, measure: Measure, slack: f64) -> bool {
    let [x, y, w, h] = name_bounds(frame, zoom, measure);
    p[0] >= x - slack && p[0] <= x + w + slack && p[1] >= y - slack && p[1] <= y + h + slack
}

/// The frame an element is in, if that is there (`getContainingFrame`).
pub fn containing<'a>(scene: &'a Scene, e: &Element) -> Option<&'a Element> {
    scene
        .element(e.frame_id()?)
        .filter(|f| is_frame(f) && !f.is_deleted())
}

/// Whether `p` is outside the frame an element is in: it isn't seen
/// there, so isn't hit there.
pub fn clipped_away(scene: &Scene, e: &Element, p: Point) -> bool {
    containing(scene, e).is_some_and(|f| {
        let [x1, y1, x2, y2] = bounds::element_bounds(f);
        !(p[0] >= x1 && p[0] <= x2 && p[1] >= y1 && p[1] <= y2)
    })
}

/// An element's outline as straight segments in the drawing
/// (`getElementLineSegments`; curves and round corners as short lines and
/// square corners).
fn outline(e: &Element) -> Vec<[Point; 2]> {
    let [x1, y1, x2, y2, cx, cy] = bounds::absolute_coords(e);
    let (x, y) = (e.x(), e.y());
    let at = |[px, py]: Point| [x + px, y + py];
    let (points, closed): (Vec<Point>, bool) = match e.kind() {
        ElementType::Line | ElementType::Arrow => {
            (hit::linear_path(e).into_iter().map(at).collect(), false)
        }
        ElementType::Freedraw => (e.points().into_iter().map(at).collect(), false),
        ElementType::Diamond => (shape::diamond_points(e).into_iter().map(at).collect(), true),
        ElementType::Ellipse => {
            let (rx, ry) = ((x2 - x1) / 2.0, (y2 - y1) / 2.0);
            let points = (0..64)
                .map(|i| {
                    let a = f64::from(i) * std::f64::consts::TAU / 64.0;
                    [cx + rx * a.cos(), cy + ry * a.sin()]
                })
                .collect();
            (points, true)
        }
        _ => (vec![[x1, y1], [x2, y1], [x2, y2], [x1, y2]], true),
    };
    let points: Vec<Point> = points
        .into_iter()
        .map(|p| geom::rotate(p, [cx, cy], e.angle()))
        .collect();
    let mut out: Vec<[Point; 2]> = points.windows(2).map(|w| [w[0], w[1]]).collect();
    if closed && points.len() > 2 {
        out.push([points[points.len() - 1], points[0]]);
    }
    out
}

/// Whether two segments meet.
fn crosses([a, b]: [Point; 2], [c, d]: [Point; 2]) -> bool {
    let side = |o: Point, p: Point, q: Point| {
        (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0])
    };
    let within = |p: Point, q: Point, r: Point| {
        r[0] >= p[0].min(q[0])
            && r[0] <= p[0].max(q[0])
            && r[1] >= p[1].min(q[1])
            && r[1] <= p[1].max(q[1])
    };
    let (d1, d2, d3, d4) = (side(c, d, a), side(c, d, b), side(a, b, c), side(a, b, d));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    (d1 == 0.0 && within(c, d, a))
        || (d2 == 0.0 && within(c, d, b))
        || (d3 == 0.0 && within(a, b, c))
        || (d4 == 0.0 && within(a, b, d))
}

/// `isElementIntersectingFrame`: whether its outline crosses the frame's.
pub fn intersects(e: &Element, frame: &Element) -> bool {
    let edges = outline(frame);
    outline(e)
        .iter()
        .any(|s| edges.iter().any(|f| crosses(*s, *f)))
}

fn contains(outer: Bounds, inner: Bounds) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

/// `getFrameInnerBounds`.
fn inner_bounds(frame: &Element) -> Bounds {
    let [x1, y1, x2, y2] = bounds::element_bounds(frame);
    [
        x1 - PRECISION,
        y1 - PRECISION,
        x2 + PRECISION,
        y2 + PRECISION,
    ]
}

/// `elementsAreInFrameBounds`: whether all of them are within the frame.
pub fn in_bounds<'a>(elements: impl IntoIterator<Item = &'a Element>, frame: &Element) -> bool {
    contains(inner_bounds(frame), bounds::common_bounds(elements))
}

/// `elementOverlapsWithFrame`: within it, across its edge, or around it.
pub fn overlaps(e: &Element, frame: &Element) -> bool {
    in_bounds([e], frame)
        || intersects(e, frame)
        || contains(bounds::element_bounds(e), bounds::element_bounds(frame))
}

/// `getElementsOverlappingFrame`: what a frame exported alone shows — the
/// elements whose box meets the frame's (the frame among them), but those
/// in another frame, which aren't seen there.
pub fn overlapping<'a>(scene: &'a Scene, frame: &Element) -> Vec<&'a Element> {
    let [fx1, fy1, fx2, fy2] = bounds::element_bounds(frame);
    scene
        .visible()
        .filter(|e| {
            e.frame_id()
                .filter(|f| !f.is_empty())
                .is_none_or(|f| f == frame.id())
        })
        .filter(|e| {
            let [x1, y1, x2, y2] = bounds::element_bounds(e);
            x1 < fx2 && x2 > fx1 && y1 < fy2 && y2 > fy1
        })
        .collect()
}

/// What of an element shows: its box, cut to its frame's while it overlaps
/// it (as `elementsOverlappingBBox` selects).
pub fn shown_bounds(scene: &Scene, e: &Element) -> Bounds {
    let b = bounds::element_bounds(e);
    match containing(scene, e) {
        Some(f) if overlaps(e, f) => {
            let fb = bounds::element_bounds(f);
            [
                js::max(b[0], fb[0]),
                js::max(b[1], fb[1]),
                js::min(b[2], fb[2]),
                js::min(b[3], fb[3]),
            ]
        }
        _ => b,
    }
}

fn outer_group(e: &Element) -> Option<&str> {
    e.group_ids().last().copied()
}

/// What is in any of `groups`.
fn group_members<'a>(scene: &'a Scene, groups: &[&str]) -> Vec<&'a Element> {
    scene
        .visible()
        .filter(|e| e.group_ids().iter().any(|g| groups.contains(g)))
        .collect()
}

/// `omitGroupsContainingFrameLikes`: what isn't in a group with a frame.
fn without_frame_groups<'a>(scene: &Scene, elements: Vec<&'a Element>) -> Vec<&'a Element> {
    elements
        .into_iter()
        .filter(|e| {
            outer_group(e).is_none_or(|g| !group_members(scene, &[g]).into_iter().any(is_frame))
        })
        .collect()
}

/// `getCommonFrameId`: the frame all of them are in, if one.
pub fn common_frame<'a>(elements: &[&'a Element]) -> Option<&'a str> {
    let mut common = None;
    for e in elements {
        let f = e.frame_id().filter(|_| !is_frame(e))?;
        if common.is_some_and(|c| c != f) {
            return None;
        }
        common = Some(f);
    }
    common
}

fn label<'a>(scene: &'a Scene, e: &Element) -> Option<&'a Element> {
    e.bound_elements()
        .iter()
        .filter(|b| b.kind == "text")
        .find_map(|b| scene.element(&b.id))
        .filter(|t| !t.is_deleted())
}

/// The frame something put at `p` goes in (`getTopLayerFrameAtSceneCoords`):
/// the topmost unlocked frame there, unless something above it and not in
/// it covers it there. `excluding`: what is being dragged; `current`: the
/// frame that is in.
pub fn at<'a>(
    scene: &'a Scene,
    p: Point,
    zoom: f64,
    excluding: &[String],
    current: Option<&str>,
) -> Option<&'a Element> {
    let frames: Vec<&Element> = scene
        .visible()
        .filter(|f| is_frame(f) && !f.is_locked())
        .filter(|f| {
            let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(f);
            p[0] >= x1 && p[0] <= x2 && p[1] >= y1 && p[1] <= y2
        })
        .collect();
    let top = *frames.last()?;
    let hit = scene
        .elements
        .iter()
        .rev()
        .filter(|e| !e.is_deleted() && !excluding.iter().any(|x| x == e.id()))
        .find(|e| hit::hits(e, p, zoom) && !clipped_away(scene, e, p));
    let Some(hit) = hit else {
        return Some(top);
    };
    if is_frame(hit) && !hit.is_locked() {
        return Some(top);
    }
    let place = |id: &str| scene.elements.iter().position(|e| e.id() == id);
    if place(hit.id()) <= place(top.id()) {
        return Some(top);
    }
    // A child dragged under something covering it stays in its frame.
    let find = |id: &str| frames.iter().find(|f| f.id() == id).copied();
    current
        .and_then(find)
        .or_else(|| hit.frame_id().and_then(find))
}

/// Where a frame's new children go in the stacking order
/// (`getFrameChildrenInsertionIndex`): above its topmost child, or just
/// below it.
fn insertion_index(elements: &[&Element], frame: &str) -> Option<usize> {
    elements.iter().enumerate().rev().find_map(|(i, e)| {
        if e.id() == frame {
            Some(i)
        } else if e.frame_id() == Some(frame) {
            Some(i + 1)
        } else {
            None
        }
    })
}

/// `n` stacking indices for new children of `frame`, given what stays.
fn child_keys(others: &[&Element], frame: &str, n: usize) -> Option<Vec<String>> {
    let at = insertion_index(others, frame)?;
    let below = at.checked_sub(1).and_then(|i| others[i].index());
    let above = others.get(at).and_then(|e| e.index());
    order::keys_between(below, above, n)
}

/// `addElementsToFrame`: `elements` and their labels joined to `frame` —
/// not frames, nor other frames' children among them — and stacked just
/// below the frame or above its topmost child. Returns what changed.
pub fn add(scene: &Scene, elements: &[&Element], frame: &Element) -> Vec<Element> {
    let common = common_frame(elements);
    let other_frames: HashSet<&str> = elements
        .iter()
        .filter(|e| is_frame(e) && e.id() != frame.id())
        .map(|e| e.id())
        .collect();
    let mut adding: Vec<Element> = Vec::new();
    let mut push = |e: &Element| {
        if !adding.iter().any(|a| a.id() == e.id()) {
            adding.push(e.clone());
        }
    };
    for e in without_frame_groups(scene, elements.to_vec()) {
        if is_frame(e) || e.frame_id().is_some_and(|f| other_frames.contains(f)) {
            continue;
        }
        push(e);
        if let Some(t) = label(scene, e) {
            push(t);
        }
    }
    let joining: HashSet<String> = adding
        .iter()
        .filter(|e| e.frame_id() != Some(frame.id()))
        .map(|e| e.id().to_string())
        .collect();
    for e in &mut adding {
        e.set("frameId", json!(frame.id()));
    }
    // Moved within their frame: not stacked again.
    let restack = !adding.is_empty() && common != Some(frame.id());
    let ids: HashSet<&str> = adding.iter().map(Element::id).collect();
    let others: Vec<&Element> = scene
        .elements
        .iter()
        .filter(|e| !ids.contains(e.id()))
        .collect();
    let keys = restack
        .then(|| child_keys(&others, frame.id(), adding.len()))
        .flatten();
    match keys {
        Some(keys) => {
            for (e, k) in adding.iter_mut().zip(keys) {
                e.set("index", json!(k));
            }
            adding
        }
        None => adding
            .into_iter()
            .filter(|e| joining.contains(e.id()))
            .collect(),
    }
}

/// `removeElementsFromFrame`: out of their frames, with their labels.
fn remove(scene: &Scene, elements: &[&Element]) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    for e in elements.iter().filter(|e| e.frame_id().is_some()) {
        for x in std::iter::once(*e).chain(label(scene, e)) {
            if !out.iter().any(|o| o.id() == x.id()) {
                let mut c = x.clone();
                c.set("frameId", Value::Null);
                out.push(c);
            }
        }
    }
    out
}

/// What is wholly in a frame and may be its child
/// (`getElementsCompletelyInFrame`): not a frame, not another frame's
/// child, not in a group with a frame.
fn completely_in<'a>(scene: &'a Scene, frame: &Element) -> Vec<&'a Element> {
    let inner = inner_bounds(frame);
    let inside: Vec<&Element> = scene
        .visible()
        .filter(|e| !e.is_locked() && e.container_id().is_none() && e.id() != frame.id())
        .filter(|e| contains(inner, shown_bounds(scene, e)))
        .collect();
    without_frame_groups(scene, inside)
        .into_iter()
        .filter(|e| match containing(scene, e) {
            Some(f) => f.id() == frame.id(),
            None => !is_frame(e),
        })
        .collect()
}

/// `omitPartialGroups`: only groups wholly in the frame.
fn whole_groups<'a>(
    scene: &Scene,
    elements: Vec<&'a Element>,
    frame: &Element,
) -> Vec<&'a Element> {
    let mut omitted: HashMap<String, bool> = HashMap::new();
    elements
        .into_iter()
        .filter(|e| {
            let groups = e.group_ids();
            if groups.is_empty() {
                return true;
            }
            let omit = groups.iter().any(|g| omitted.get(*g) == Some(&true))
                || !in_bounds(group_members(scene, &groups), frame);
            for g in groups {
                omitted.insert(g.to_string(), omit);
            }
            !omit
        })
        .collect()
}

/// `getElementsInResizingFrame`: what a frame holds once drawn or resized
/// — what is wholly in it, what of its children still reaches into it,
/// and whole groups — labels left out (they come with what they label).
pub fn in_resized<'a>(scene: &'a Scene, frame: &Element) -> Vec<&'a Element> {
    let prev: Vec<&Element> = scene
        .visible()
        .filter(|e| e.frame_id() == Some(frame.id()) && e.container_id().is_none())
        .collect();
    let mut completely = completely_in(scene, frame);
    let frame_box = bounds::element_bounds(frame);
    for e in &prev {
        if contains(bounds::element_bounds(e), frame_box)
            && !completely.iter().any(|c| c.id() == e.id())
        {
            completely.push(e);
        }
    }
    let partly: Vec<&Element> = prev
        .iter()
        .copied()
        .filter(|e| !completely.iter().any(|c| c.id() == e.id()))
        .collect();
    let mut keep: HashSet<String> = completely
        .iter()
        .flat_map(|e| e.group_ids())
        .map(str::to_string)
        .collect();
    let mut next: Vec<&Element> = prev.clone();
    for e in &partly {
        if !intersects(e, frame) {
            if e.group_ids().is_empty() {
                next.retain(|n| n.id() != e.id());
            }
        } else {
            keep.extend(e.group_ids().into_iter().map(str::to_string));
        }
    }
    for e in partly.iter().filter(|e| !e.group_ids().is_empty()) {
        if !e.group_ids().iter().any(|g| keep.contains(*g)) {
            next.retain(|n| n.id() != e.id());
        }
    }
    let put = |e: &'a Element, next: &mut Vec<&'a Element>| {
        if !next.iter().any(|n| n.id() == e.id()) {
            next.push(e);
        }
    };
    for e in completely.iter().filter(|e| e.group_ids().is_empty()) {
        put(e, &mut next);
    }
    // Groups wholly in it come in whole.
    let groups: HashSet<&str> = completely.iter().filter_map(|e| outer_group(e)).collect();
    for g in groups {
        let members = group_members(scene, &[g]);
        if in_bounds(members.iter().copied(), frame) {
            for m in members {
                put(m, &mut next);
            }
        }
    }
    // In the stacking order.
    scene
        .visible()
        .filter(|e| e.container_id().is_none() && next.iter().any(|n| n.id() == e.id()))
        .collect()
}

/// Whether an element (with its group) goes in a frame it is dropped on
/// (`isElementInFrame`): it overlaps it, or something of its group does —
/// unless its group holds a frame.
fn unit_overlaps(scene: &Scene, e: &Element, frame: &Element) -> bool {
    let groups = e.group_ids();
    if groups.is_empty() {
        return overlaps(e, frame);
    }
    let members = group_members(scene, &groups);
    !members.iter().any(|m| is_frame(m)) && members.iter().any(|m| overlaps(m, frame))
}

/// Puts changed elements into a scene.
fn patch(scene: &mut Scene, changed: &[Element]) {
    for c in changed {
        if let Some(slot) = scene.elements.iter_mut().find(|e| e.id() == c.id()) {
            *slot = c.clone();
        }
    }
}

fn upsert(changed: &mut Vec<Element>, e: Element) {
    crate::sticky::upsert(changed, e);
}

/// Frames' names in an export, as texts (`addFrameLabelsAsTextElements`):
/// in Helvetica above each frame, cut with "..." to its width; by frame.
pub fn name_labels(frames: &[&Element], dark: bool, measure: Measure) -> HashMap<String, Element> {
    let font = format!(
        "{}px {}",
        js::number_to_string(NAME_FONT_SIZE),
        fonts::css_family(fonts::HELVETICA)
    );
    frames
        .iter()
        .map(|f| {
            let mut text = title(f).to_string();
            let lines = text.split('\n').count() as f64;
            let height = lines * NAME_FONT_SIZE * NAME_LINE_HEIGHT;
            let mut width = text
                .split('\n')
                .map(|l| measure(&font, l))
                .fold(0.0, f64::max);
            if width > f.width() {
                // From the right, a character at a time; nothing if even
                // the dots don't fit.
                let chars: Vec<char> = text.chars().collect();
                text = (0..=chars.len())
                    .rev()
                    .map(|i| format!("{}...", chars[..i].iter().collect::<String>()))
                    .find(|t| measure(&font, t) <= f.width())
                    .unwrap_or_default();
                width = f.width();
            }
            let props = json!({
                "id": format!("{}-name", f.id()), "type": "text",
                "x": num(f.x()), "y": num(f.y() - NAME_OFFSET_Y - height),
                "width": num(width), "height": num(height), "angle": 0,
                "strokeColor": if dark { NAME_COLOR_UNDARK } else { NAME_COLOR },
                "opacity": 100, "text": text, "originalText": text,
                "fontSize": num(NAME_FONT_SIZE), "fontFamily": fonts::HELVETICA,
                "lineHeight": num(NAME_LINE_HEIGHT), "textAlign": "left", "verticalAlign": "top",
            });
            let label = Element::from_props(props.as_object().cloned().unwrap_or_default())
                .expect("has an id and a type");
            (f.id().to_string(), label)
        })
        .collect()
}

/// The box an export frames: everything not inside a frame there, and the
/// frames' names (`labels`).
pub fn export_bounds<'a>(
    scene: &'a Scene,
    labels: impl IntoIterator<Item = &'a Element>,
) -> Bounds {
    bounds::common_bounds(bounds::root_elements(scene).into_iter().chain(labels))
}

impl Editor {
    /// The frame shown highlighted: where what is drawn or dragged goes.
    pub fn frame_to_highlight(&self) -> Option<&str> {
        self.frame_to_highlight.as_deref()
    }

    /// Whether the selection is being dragged (what is dragged shows as in
    /// the frame it would be dropped in).
    pub fn dragging(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Moving { moved: true, .. }))
    }

    /// The frame an element is shown clipped to (`shouldApplyFrameClip`):
    /// its own, or while it is dragged, the one it would be dropped in if
    /// it reaches into it. A label goes with what it labels.
    pub fn clip_frame<'a>(&self, scene: &'a Scene, e: &'a Element) -> Option<&'a Element> {
        let owner = e.container_id().and_then(|c| scene.element(c)).unwrap_or(e);
        let dragged = self.dragging()
            && self.is_selected(owner.id())
            && !owner.frame_id().is_some_and(|f| self.is_selected(f));
        if dragged {
            return self
                .frame_to_highlight()
                .and_then(|f| scene.element(f))
                .filter(|f| overlaps(owner, f));
        }
        containing(scene, owner)
    }

    /// What will go in the frames being drawn or resized (shown
    /// highlighted).
    pub fn frame_highlights<'a>(&self, scene: &'a Scene) -> Vec<&'a Element> {
        match &self.gesture {
            Some(Gesture::Creating { id, .. }) => scene
                .element(id)
                .filter(|f| is_frame(f))
                .map(|f| in_resized(scene, f))
                .unwrap_or_default(),
            Some(Gesture::Resizing { .. }) => self
                .selected(scene)
                .into_iter()
                .filter(|f| is_frame(f))
                .flat_map(|f| in_resized(scene, f))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether a handle can be used on what is selected: a lone frame isn't
    /// turned (`getTransformHandles`).
    pub fn can_use(&self, scene: &Scene, handle: Handle) -> bool {
        handle != Handle::Rotation || !matches!(self.selected(scene).as_slice(), [f] if is_frame(f))
    }

    /// A new element made at `p`: in the frame there, if one, stacked with
    /// its children (`insertNewElement`); else on top.
    pub(super) fn place_new(&mut self, scene: &Scene, e: &mut Element, p: Point) {
        let frame = (!is_frame(e))
            .then(|| at(scene, p, self.zoom, &[], None))
            .flatten();
        let all: Vec<&Element> = scene.elements.iter().collect();
        let index = frame
            .and_then(|f| child_keys(&all, f.id(), 1))
            .and_then(|k| k.into_iter().next())
            .or_else(|| super::top_index(scene));
        e.set("frameId", frame.map_or(Value::Null, |f| json!(f.id())));
        e.set("index", index.map_or(Value::Null, Value::String));
        self.frame_to_highlight = frame.map(|f| f.id().to_string());
    }

    /// The pointer moved with no button down: with a tool that makes
    /// elements, where they would go is shown.
    pub(super) fn hover_frame(&mut self, scene: &Scene, p: Point) {
        let makes = !matches!(
            self.tool,
            Tool::Selection | Tool::Hand | Tool::Eraser | Tool::Laser | Tool::Frame | Tool::Image
        );
        self.frame_to_highlight = makes
            .then(|| at(scene, p, self.zoom, &[], None))
            .flatten()
            .map(|f| f.id().to_string());
    }

    /// The frame the selection would be dropped in at `p`: none when a
    /// frame is dragged.
    fn drop_target<'a>(&self, scene: &'a Scene, p: Point) -> Option<&'a Element> {
        let selected = self.selected(scene);
        if selected.iter().any(|e| is_frame(e)) {
            return None;
        }
        at(
            scene,
            p,
            self.zoom,
            &self.selection,
            common_frame(&selected),
        )
    }

    /// While the selection is dragged: where it would be dropped is shown.
    pub(super) fn drag_frame(&mut self, scene: &Scene, p: Point) {
        self.frame_to_highlight = self.drop_target(scene, p).map(|f| f.id().to_string());
    }

    /// The selection dropped at `p` after a drag
    /// (`handleSelectionDragOnPointerUp`,
    /// `updateFrameMembershipOfSelectedElements`): what overlaps the frame
    /// there joins it, what left its frame leaves it. Returns what changed.
    pub(super) fn dropped(&mut self, scene: &Scene, p: Point) -> Vec<Element> {
        self.frame_to_highlight = None;
        let target = self.drop_target(scene, p);
        let selected = self.selected(scene);
        // In a frame dragged with it, it stays there.
        let stays = |s: &Scene, e: &Element| {
            e.frame_id().is_some_and(|f| self.is_selected(f))
                || target.is_some_and(|t| unit_overlaps(s, e, t))
        };
        let mut changed = Vec::new();
        if let Some(t) = target.filter(|t| !self.is_selected(t.id())) {
            let joining: Vec<&Element> = selected
                .iter()
                .copied()
                .filter(|e| stays(scene, e))
                .collect();
            changed = add(scene, &joining, t);
        }
        let mut after = scene.clone();
        patch(&mut after, &changed);
        let leaving: Vec<&Element> = after
            .visible()
            .filter(|e| self.is_selected(e.id()) && !is_frame(e) && e.frame_id().is_some())
            .filter(|e| !stays(&after, e))
            .collect();
        for e in remove(&after, &leaving) {
            upsert(&mut changed, e);
        }
        changed
    }

    /// A frame just drawn: what is wholly in it joins it
    /// (`getElementsInNewFrame`).
    pub(super) fn frame_drawn(&self, scene: &Scene, frame: &Element) -> Vec<Element> {
        let inside = whole_groups(scene, completely_in(scene, frame), frame);
        add(scene, &inside, frame)
    }

    /// The selected frames were resized: what they hold now
    /// (`replaceAllElementsInFrame`).
    pub(super) fn frames_resized(&self, scene: &Scene) -> Vec<Element> {
        let mut after = scene.clone();
        let mut changed: Vec<Element> = Vec::new();
        for f in self.selected(scene).into_iter().filter(|e| is_frame(e)) {
            let next: Vec<String> = in_resized(&after, f)
                .iter()
                .map(|e| e.id().to_string())
                .collect();
            let children: Vec<&Element> = after
                .visible()
                .filter(|e| e.frame_id() == Some(f.id()))
                .collect();
            let out = remove(&after, &children);
            patch(&mut after, &out);
            let joining: Vec<&Element> = after
                .visible()
                .filter(|e| next.iter().any(|n| n == e.id()))
                .collect();
            let added = add(&after, &joining, f);
            patch(&mut after, &added);
            for e in out.into_iter().chain(added) {
                upsert(&mut changed, e);
            }
        }
        changed.retain(|c| scene.element(c.id()) != Some(c));
        changed
    }

    /// What a drag, the arrow keys, a copy, a duplicate or the stacking
    /// order take: the selection with its labels, and the children of the
    /// frames in it (with theirs).
    pub(super) fn dragged_set(&self, scene: &Scene) -> Vec<Element> {
        let set = self.moving_set(scene);
        let frames: HashSet<&str> = set
            .iter()
            .filter(|e| is_frame(e))
            .map(Element::id)
            .collect();
        if frames.is_empty() {
            return set;
        }
        let mut ids: HashSet<String> = set.iter().map(|e| e.id().to_string()).collect();
        for e in scene.visible() {
            if e.frame_id().is_some_and(|f| frames.contains(f)) {
                ids.insert(e.id().to_string());
                if let Some(t) = label(scene, e) {
                    ids.insert(t.id().to_string());
                }
            }
        }
        scene
            .visible()
            .filter(|e| ids.contains(e.id()))
            .cloned()
            .collect()
    }

    /// Children of the frames among `deleted` that aren't deleted with
    /// them: they stay where they are, out of the frame, and are selected
    /// instead (`deleteSelectedElements`).
    pub(super) fn release_children(&mut self, scene: &Scene, deleted: &mut Vec<Element>) {
        let frames: HashSet<String> = deleted
            .iter()
            .filter(|e| is_frame(e))
            .map(|e| e.id().to_string())
            .collect();
        let gone: HashSet<String> = deleted.iter().map(|e| e.id().to_string()).collect();
        let children: Vec<&Element> = scene
            .visible()
            .filter(|e| e.frame_id().is_some_and(|f| frames.contains(f)) && !gone.contains(e.id()))
            .collect();
        for e in children {
            let mut c = e.clone();
            c.set("frameId", Value::Null);
            if e.container_id().is_none() {
                self.selection.push(e.id().to_string());
            }
            deleted.push(c);
        }
    }

    /// Deletes what is selected for a cut: frames with their children (on
    /// the clipboard with them, they go with them).
    pub fn cut(&mut self, scene: &Scene, rng: &mut dyn Random) -> Output {
        let mut out = Output::default();
        let mut changed = self.dragged_set(scene);
        for e in &mut changed {
            e.set("isDeleted", json!(true));
        }
        binding::update_arrows(scene, &mut changed);
        for mut e in changed {
            bump(&mut e, rng);
            out.changed.push(e);
        }
        self.selection.clear();
        out.done = !out.changed.is_empty();
        out
    }

    /// Pasted elements (`App.duplicate.ts`): frames not pasted with them
    /// are forgotten, and what overlaps the frame at `p` joins it.
    pub(super) fn paste_into_frame(&self, scene: &Scene, pasted: &mut Vec<Element>, p: Point) {
        let ids: HashSet<String> = pasted.iter().map(|e| e.id().to_string()).collect();
        for e in pasted.iter_mut() {
            if e.frame_id().is_some_and(|f| !ids.contains(f)) {
                e.set("frameId", Value::Null);
            }
        }
        let Some(frame) = at(scene, p, self.zoom, &[], None) else {
            return;
        };
        let mut with = scene.clone();
        with.elements.extend(pasted.iter().cloned());
        let copies: Vec<&Element> = with
            .elements
            .iter()
            .filter(|e| ids.contains(e.id()) && e.container_id().is_none())
            .collect();
        let joining = eligible(&with, copies, frame);
        for e in add(&with, &joining, frame) {
            upsert(pasted, e);
        }
    }
}

/// The children of erased frames: erased with them.
pub(super) fn erased_children(scene: &Scene, erased: &BTreeSet<String>) -> HashSet<String> {
    scene
        .visible()
        .filter(|e| e.frame_id().is_some_and(|f| erased.contains(f)))
        .map(|e| e.id().to_string())
        .collect()
}

/// `filterElementsEligibleAsFrameChildren`: of `elements`, what overlaps
/// the frame (a group if any of it does) — not frames, nor their children.
fn eligible<'a>(scene: &Scene, elements: Vec<&'a Element>, frame: &Element) -> Vec<&'a Element> {
    let elements = without_frame_groups(scene, elements);
    let frames: HashSet<&str> = elements
        .iter()
        .filter(|e| is_frame(e) && e.id() != frame.id())
        .map(|e| e.id())
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<&Element> = Vec::new();
    for e in &elements {
        if is_frame(e) || e.frame_id().is_some_and(|f| frames.contains(f)) {
            continue;
        }
        match outer_group(e) {
            Some(g) => {
                if seen.insert(g) {
                    let members: Vec<&Element> = elements
                        .iter()
                        .copied()
                        .filter(|m| m.group_ids().contains(&g))
                        .collect();
                    if members.iter().any(|m| overlaps(m, frame)) {
                        out.extend(members);
                    }
                }
            }
            None => {
                if overlaps(e, frame) {
                    out.push(e);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
