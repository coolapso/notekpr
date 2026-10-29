//! Snapping, as Excalidraw does it (`snapping.ts`, and `getGridPoint` of its
//! `common` package; MIT, see `PORTED.md`): to the drawing's grid, and to
//! other elements — what is moved, resized or drawn lines up with their
//! corners, middles and centres, or keeps the gaps between them equal. Also
//! the lines that show what snapped, for the app to draw.

use std::collections::HashMap;

use serde_json::Value;

use super::transform::{Frame, Handle};
use crate::bounds::{self, Bounds};
use crate::geom::{self, Point};
use crate::js;
use crate::scene::{Element, ElementType, Props, Scene};

/// How near counts, on screen (`SNAP_DISTANCE`).
const SNAP_DISTANCE: f64 = 8.0;

/// At most this many pairs of elements are looked at for gaps, each way
/// (`VISIBLE_GAPS_LIMIT_PER_AXIS`).
const GAPS_LIMIT: usize = 99_999;

// ------------------------------------------------------------------ the grid

/// A drawing's grid: a line every `size`, a bold one every `step` lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub size: f64,
    pub step: f64,
}

/// Excalidraw's (`DEFAULT_GRID_SIZE`, `DEFAULT_GRID_STEP`).
pub const DEFAULT_GRID: Grid = Grid {
    size: 20.0,
    step: 5.0,
};

/// A grid setting as Excalidraw reads it: a whole number from 1 to 100
/// (`getNormalizedGridSize`, `getNormalizedGridStep`).
fn normalized(v: Option<&Value>, default: f64) -> f64 {
    let x = v
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(default);
    js::round(x).clamp(1.0, 100.0)
}

impl Grid {
    /// The drawing's grid, if it is on (`gridModeEnabled`, `gridSize`,
    /// `gridStep`: settings of the drawing, the same for everyone).
    pub fn of(scene: &Scene) -> Option<Grid> {
        let s = &scene.app_state;
        if s.get("gridModeEnabled").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        Some(Grid {
            size: normalized(s.get("gridSize"), DEFAULT_GRID.size),
            step: normalized(s.get("gridStep"), DEFAULT_GRID.step),
        })
    }

    /// The settings that turn a drawing's grid on or off.
    pub fn settings(on: bool) -> Props {
        let mut p = Props::new();
        p.insert("gridModeEnabled".into(), Value::Bool(on));
        p
    }
}

/// The size of the grid things snap to (`getEffectiveGridSize`): none when
/// it is off, or while Ctrl is held.
pub fn grid_size(scene: &Scene, ctrl: bool) -> Option<f64> {
    if ctrl {
        None
    } else {
        Grid::of(scene).map(|g| g.size)
    }
}

/// `getGridPoint`: the grid's crossing nearest to `p` (`p` without a grid).
pub fn grid_point([x, y]: Point, grid: Option<f64>) -> Point {
    match grid {
        Some(g) if g != 0.0 => [js::round(x / g) * g, js::round(y / g) * g],
        _ => [x, y],
    }
}

/// How far an arrow key moves what is selected: a square of the grid when
/// it is on (Shift: 1), else 1 (Shift: 5).
pub fn nudge_step(grid: Option<f64>, shift: bool) -> f64 {
    match (grid, shift) {
        (Some(_), true) => 1.0,
        (Some(g), false) => g,
        (None, true) => 5.0,
        (None, false) => 1.0,
    }
}

/// How far pasted elements, whose box is `b`, move to be centred on `at`;
/// with the grid on, their corner goes on it instead (Excalidraw's
/// `addElementsFromPasteOrLibrary`).
pub fn paste_offset(scene: &Scene, [x1, y1, x2, y2]: Bounds, at: Point) -> Point {
    match grid_size(scene, false) {
        Some(g) => {
            let [gx, gy] = grid_point(
                [at[0] - (x2 - x1).abs() / 2.0, at[1] - (y2 - y1).abs() / 2.0],
                Some(g),
            );
            [gx - x1, gy - y1]
        }
        None => [at[0] - (x1 + x2) / 2.0, at[1] - (y1 + y2) / 2.0],
    }
}

// ------------------------------------------------------------------ elements

/// Whether to snap to other elements now (`isSnappingEnabled`): when it is
/// on and Ctrl isn't held, or when it is off, Ctrl is held and the grid is
/// off.
pub fn objects_on(to_objects: bool, grid_on: bool, ctrl: bool) -> bool {
    (to_objects && !ctrl) || (!to_objects && ctrl && !grid_on)
}

/// What shows a snap (`SnapLine`), in the drawing's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum SnapLine {
    /// Points in a line: drawn through, a cross on each (`points`).
    Points(Vec<Point>),
    /// From a corner to the pointer, which lines up with it (`pointer`).
    Pointer([Point; 2]),
    /// One of the gaps that are equal: a marked line across it (`gap`).
    Gap {
        from: Point,
        to: Point,
        horizontal: bool,
    },
}

/// The room between two elements side by side, or one above the other
/// (`Gap`).
#[derive(Clone, Debug, PartialEq)]
struct Gap {
    start_bounds: Bounds,
    end_bounds: Bounds,
    /// The sides that face each other.
    start_side: [Point; 2],
    end_side: [Point; 2],
    /// Where both are, along the gap's sides.
    overlap: [f64; 2],
    length: f64,
}

/// How something keeps a gap (`GapSnap["direction"]`): in the middle of
/// one, or the same distance on one side.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GapDirection {
    CenterHorizontal,
    CenterVertical,
    SideLeft,
    SideRight,
    SideTop,
    SideBottom,
}

/// A way to snap, and how far it moves (`Snap`).
#[derive(Clone, Debug)]
enum Snap {
    /// A point of what moves, and the one it lines up with.
    Point { points: [Point; 2], offset: f64 },
    Gap {
        direction: GapDirection,
        gap: Gap,
        offset: f64,
    },
}

impl Snap {
    fn offset(&self) -> f64 {
        match self {
            Snap::Point { offset, .. } | Snap::Gap { offset, .. } => *offset,
        }
    }
}

/// The nearest snaps so far, across and down, and how near they are
/// (`nearestSnapsX`, `nearestSnapsY`, `minOffset`).
struct Nearest {
    x: Vec<Snap>,
    y: Vec<Snap>,
    min: [f64; 2],
}

impl Nearest {
    fn new(distance: f64) -> Self {
        Self {
            x: Vec::new(),
            y: Vec::new(),
            min: [distance; 2],
        }
    }

    /// Keeps `snap` across (`axis` 0) or down (1) if it is as near as the
    /// nearest (farther ones go); whether it was.
    fn offer(&mut self, axis: usize, snap: Snap) -> bool {
        let d = snap.offset().abs();
        if d > self.min[axis] {
            return false;
        }
        let list = if axis == 0 { &mut self.x } else { &mut self.y };
        if d < self.min[axis] {
            list.clear();
        }
        list.push(snap);
        self.min[axis] = d;
        true
    }

    /// How far to move to snap: the nearest snap's, across and down.
    fn offset(&self) -> Point {
        [
            self.x.first().map_or(0.0, Snap::offset),
            self.y.first().map_or(0.0, Snap::offset),
        ]
    }
}

/// Excalidraw's `round` here: to 6 decimals.
fn round(x: f64) -> f64 {
    js::round(x * 1e6) / 1e6
}

/// `rangesOverlap`.
fn overlap([a0, a1]: [f64; 2], [b0, b1]: [f64; 2]) -> bool {
    if a0 <= b0 {
        return a1 >= b0;
    }
    if a0 >= b0 {
        return b1 >= a0;
    }
    false
}

/// `rangeIntersection`.
fn intersection([a0, a1]: [f64; 2], [b0, b1]: [f64; 2]) -> Option<[f64; 2]> {
    let (start, end) = (js::max(a0, b0), js::min(a1, b1));
    (start <= end).then_some([start, end])
}

/// `getElementsCorners`: an element's corners — the middles of its sides
/// for diamonds and ellipses, unless `bounding_box` — turned with it, and
/// its centre (unless `omit_center`); for several, their box's. Moved by
/// `offset`, rounded.
fn corners(
    elements: &[&Element],
    [dx, dy]: Point,
    bounding_box: bool,
    omit_center: bool,
) -> Vec<Point> {
    let points = match elements {
        [] => Vec::new(),
        [e] => {
            let [x1, y1, x2, y2, cx, cy] = bounds::absolute_coords(e);
            let (x1, y1, x2, y2) = (x1 + dx, y1 + dy, x2 + dx, y2 + dy);
            let center = [cx + dx, cy + dy];
            let (half_w, half_h) = ((x2 - x1) / 2.0, (y2 - y1) / 2.0);
            let turned = |p: Point| geom::rotate(p, center, e.angle());
            let mut out = if matches!(e.kind(), ElementType::Diamond | ElementType::Ellipse)
                && !bounding_box
            {
                vec![
                    turned([x1, y1 + half_h]),
                    turned([x1 + half_w, y1]),
                    turned([x2, y1 + half_h]),
                    turned([x1 + half_w, y2]),
                ]
            } else {
                vec![
                    turned([x1, y1]),
                    turned([x2, y1]),
                    turned([x1, y2]),
                    turned([x2, y2]),
                ]
            };
            if !omit_center {
                out.push(center);
            }
            out
        }
        many => {
            let [x1, y1, x2, y2] = bounds::common_bounds(many.iter().copied());
            let (x1, y1, x2, y2) = (x1 + dx, y1 + dy, x2 + dx, y2 + dy);
            let mut out = vec![[x1, y1], [x2, y1], [x1, y2], [x2, y2]];
            if !omit_center {
                out.push([x1 + (x2 - x1) / 2.0, y1 + (y2 - y1) / 2.0]);
            }
            out
        }
    };
    points
        .into_iter()
        .map(|[x, y]| [round(x), round(y)])
        .collect()
}

/// A shape's label, if it has one there (`getBoundTextElement`).
fn label_of<'a>(scene: &'a Scene, e: &Element) -> Option<&'a Element> {
    let id = e
        .bound_elements()
        .into_iter()
        .find(|b| b.kind == "text")?
        .id;
    scene.element(&id).filter(|t| !t.is_deleted())
}

/// The elements to snap to: those there, in `view` (if given), but not
/// among `moving` (`getVisibleAndNonSelectedElements`).
fn reference_elements<'a>(
    scene: &'a Scene,
    moving: &[&str],
    view: Option<Bounds>,
) -> Vec<&'a Element> {
    scene
        .visible()
        .filter(|e| !moving.contains(&e.id()))
        .filter(|e| {
            // `isElementInViewport`.
            view.is_none_or(|[vx1, vy1, vx2, vy2]| {
                let [x1, y1, x2, y2] = bounds::element_bounds(e);
                vx1 <= x2 && vy1 <= y2 && vx2 >= x1 && vy2 >= y1
            })
        })
        .collect()
}

/// The elements as they move together: by their outermost group, each with
/// its label (`getMaximumGroups`); labels on their own left out.
fn maximum_groups<'a>(scene: &'a Scene, elements: &[&'a Element]) -> Vec<Vec<&'a Element>> {
    let mut groups: Vec<Vec<&Element>> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for e in elements {
        let key = e.group_ids().last().copied().unwrap_or(e.id());
        let i = *at.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        if let Some(t) = label_of(scene, e) {
            groups[i].push(t);
        }
        groups[i].push(e);
    }
    groups
        .into_iter()
        .filter(|g| {
            !(g.len() == 1 && g[0].kind() == ElementType::Text && g[0].container_id().is_some())
        })
        .collect()
}

/// What the other elements offer to snap to, found once a gesture
/// (`SnapCache`): their corners, middles and centres
/// (`getReferenceSnapPoints`), and the gaps between them
/// (`getVisibleGaps`).
#[derive(Clone, Debug, Default)]
struct References {
    points: Vec<Point>,
    horizontal_gaps: Vec<Gap>,
    vertical_gaps: Vec<Gap>,
}

impl References {
    /// Those of the elements there (in `view`, if given) but `moving`; the
    /// gaps only with `gaps` (moving needs them, resizing and drawing not).
    fn new(scene: &Scene, moving: &[&str], view: Option<Bounds>, gaps: bool) -> Self {
        let elements = reference_elements(scene, moving, view);
        let groups = maximum_groups(scene, &elements);
        let points = groups
            .iter()
            .flat_map(|g| corners(g, [0.0, 0.0], false, false))
            .collect();
        let mut refs = References {
            points,
            ..Default::default()
        };
        if gaps {
            let mut boxes: Vec<Bounds> = groups
                .iter()
                .map(|g| bounds::common_bounds(g.iter().copied()).map(round))
                .collect();
            refs.horizontal_gaps = horizontal_gaps(&mut boxes);
            refs.vertical_gaps = vertical_gaps(&mut boxes);
        }
        refs
    }
}

/// The gaps between boxes side by side (they overlap down).
fn horizontal_gaps(boxes: &mut [Bounds]) -> Vec<Gap> {
    js::sort_by(boxes, |a, b| a[0] - b[0]);
    let mut gaps = Vec::new();
    let mut c = 0;
    'outer: for (i, start) in boxes.iter().enumerate() {
        for end in &boxes[i + 1..] {
            c += 1;
            if c > GAPS_LIMIT {
                break 'outer;
            }
            let [_, start_min_y, start_max_x, start_max_y] = *start;
            let [end_min_x, end_min_y, _, end_max_y] = *end;
            let (a, b) = ([start_min_y, start_max_y], [end_min_y, end_max_y]);
            if start_max_x < end_min_x && overlap(a, b) {
                gaps.push(Gap {
                    start_bounds: *start,
                    end_bounds: *end,
                    start_side: [[start_max_x, start_min_y], [start_max_x, start_max_y]],
                    end_side: [[end_min_x, end_min_y], [end_min_x, end_max_y]],
                    length: end_min_x - start_max_x,
                    overlap: intersection(a, b).expect("they overlap"),
                });
            }
        }
    }
    gaps
}

/// The gaps between boxes one above the other (they overlap across).
fn vertical_gaps(boxes: &mut [Bounds]) -> Vec<Gap> {
    js::sort_by(boxes, |a, b| a[1] - b[1]);
    let mut gaps = Vec::new();
    let mut c = 0;
    'outer: for (i, start) in boxes.iter().enumerate() {
        for end in &boxes[i + 1..] {
            c += 1;
            if c > GAPS_LIMIT {
                break 'outer;
            }
            let [start_min_x, _, start_max_x, start_max_y] = *start;
            let [end_min_x, end_min_y, end_max_x, _] = *end;
            let (a, b) = ([start_min_x, start_max_x], [end_min_x, end_max_x]);
            if start_max_y < end_min_y && overlap(a, b) {
                gaps.push(Gap {
                    start_bounds: *start,
                    end_bounds: *end,
                    start_side: [[start_min_x, start_max_y], [start_max_x, start_max_y]],
                    end_side: [[end_min_x, end_min_y], [end_max_x, end_min_y]],
                    length: end_min_y - start_max_y,
                    overlap: intersection(a, b).expect("they overlap"),
                });
            }
        }
    }
    gaps
}

/// `getPointSnaps`: each of `points` lined up with the nearest of `refs`,
/// across and down.
fn point_snaps(points: &[Point], refs: &[Point], near: &mut Nearest) {
    for this in points {
        for other in refs {
            let offset_x = other[0] - this[0];
            let offset_y = other[1] - this[1];
            near.offer(
                0,
                Snap::Point {
                    points: [*this, *other],
                    offset: offset_x,
                },
            );
            near.offer(
                1,
                Snap::Point {
                    points: [*this, *other],
                    offset: offset_y,
                },
            );
        }
    }
}

/// `getGapSnaps`: the box `b` (of what moves) in the middle of a gap it
/// fits in, or as far from one end of a gap as its other end is.
fn gap_snaps(b: Bounds, refs: &References, near: &mut Nearest) {
    let [min_x, min_y, max_x, max_y] = b.map(round);
    let center_x = (min_x + max_x) / 2.0;
    let center_y = (min_y + max_y) / 2.0;
    let gap = |direction, gap: &Gap, offset| Snap::Gap {
        direction,
        gap: gap.clone(),
        offset,
    };
    for g in &refs.horizontal_gaps {
        if !overlap([min_y, max_y], g.overlap) {
            continue;
        }
        let gap_mid_x = g.start_side[0][0] + g.length / 2.0;
        let center_offset = round(gap_mid_x - center_x);
        let larger = g.length > max_x - min_x;
        if larger && near.offer(0, gap(GapDirection::CenterHorizontal, g, center_offset)) {
            continue;
        }
        // On its right: as far from its end as its start is.
        let distance_to_end = min_x - g.end_bounds[2];
        let side_right = round(g.length - distance_to_end);
        if near.offer(0, gap(GapDirection::SideRight, g, side_right)) {
            continue;
        }
        let distance_to_start = g.start_bounds[0] - max_x;
        let side_left = round(distance_to_start - g.length);
        near.offer(0, gap(GapDirection::SideLeft, g, side_left));
    }
    for g in &refs.vertical_gaps {
        if !overlap([min_x, max_x], g.overlap) {
            continue;
        }
        let gap_mid_y = g.start_side[0][1] + g.length / 2.0;
        let center_offset = round(gap_mid_y - center_y);
        let larger = g.length > max_y - min_y;
        if larger && near.offer(1, gap(GapDirection::CenterVertical, g, center_offset)) {
            continue;
        }
        let distance_to_start = g.start_bounds[1] - max_y;
        let side_top = round(distance_to_start - g.length);
        if near.offer(1, gap(GapDirection::SideTop, g, side_top)) {
            continue;
        }
        let distance_to_end = round(min_y - g.end_bounds[3]);
        let side_bottom = g.length - distance_to_end;
        near.offer(1, gap(GapDirection::SideBottom, g, side_bottom));
    }
}

/// The box around `elements`, moved by `offset` (`getDraggedElementsBounds`).
fn moved_bounds(elements: &[&Element], [dx, dy]: Point) -> Bounds {
    let [x1, y1, x2, y2] = bounds::common_bounds(elements.iter().copied());
    [x1 + dx, y1 + dy, x2 + dx, y2 + dy]
}

/// `snapDraggedElements`: how much further `selected`, moved by `drag`,
/// goes to snap; and the lines showing it.
fn snap_drag(
    selected: &[&Element],
    refs: &References,
    drag: Point,
    zoom: f64,
) -> (Point, Vec<SnapLine>) {
    let mut near = Nearest::new(SNAP_DISTANCE / zoom);
    point_snaps(
        &corners(selected, drag, false, false),
        &refs.points,
        &mut near,
    );
    gap_snaps(moved_bounds(selected, drag), refs, &mut near);
    let offset = near.offset();
    // Where it snapped: what lines up there exactly.
    let mut near = Nearest::new(0.0);
    let drag = [round(drag[0] + offset[0]), round(drag[1] + offset[1])];
    point_snaps(
        &corners(selected, drag, false, false),
        &refs.points,
        &mut near,
    );
    gap_snaps(moved_bounds(selected, drag), refs, &mut near);
    let mut lines = point_lines(&near);
    let gaps: Vec<&Snap> = near.x.iter().chain(&near.y).collect();
    lines.extend(gap_lines(moved_bounds(selected, drag), &gaps));
    (offset, lines)
}

/// `snapResizingElements`: how much further the `handle` of a selection
/// whose box was `b`, dragged to `at`, goes to snap.
fn snap_resize(b: Bounds, handle: Handle, at: Point, refs: &References, zoom: f64) -> Point {
    let [mut x1, mut y1, mut x2, mut y2] = b;
    match handle {
        Handle::E | Handle::NE | Handle::SE => x2 = at[0],
        Handle::W | Handle::NW | Handle::SW => x1 = at[0],
        _ => {}
    }
    match handle {
        Handle::N | Handle::NE | Handle::NW => y1 = at[1],
        Handle::S | Handle::SE | Handle::SW => y2 = at[1],
        _ => {}
    }
    let points: Vec<Point> = match handle {
        Handle::E => vec![[x2, y1], [x2, y2]],
        Handle::W => vec![[x1, y1], [x1, y2]],
        Handle::N => vec![[x1, y1], [x2, y1]],
        Handle::S => vec![[x1, y2], [x2, y2]],
        Handle::NE => vec![[x2, y1]],
        Handle::NW => vec![[x1, y1]],
        Handle::SE => vec![[x2, y2]],
        Handle::SW => vec![[x1, y2]],
        Handle::Rotation => Vec::new(),
    };
    let mut near = Nearest::new(SNAP_DISTANCE / zoom);
    point_snaps(&points, &refs.points, &mut near);
    near.offset()
}

/// The lines showing the corners of the box `b` (a resized selection, a new
/// element) lined up exactly with others'.
fn box_lines(b: Bounds, refs: &References) -> Vec<SnapLine> {
    let [x1, y1, x2, y2] = b.map(round);
    let mut near = Nearest::new(0.0);
    point_snaps(
        &[[x1, y1], [x1, y2], [x2, y1], [x2, y2]],
        &refs.points,
        &mut near,
    );
    point_lines(&near)
}

/// `getSnapLinesAtPointer`: the pointer lined up with the corners, middles
/// and centres of elements (in `view`, if given): how far it moves to, and
/// the lines from them to it.
fn at_pointer(scene: &Scene, p: Point, view: Option<Bounds>, zoom: f64) -> (Point, Vec<SnapLine>) {
    let mut min = [SNAP_DISTANCE / zoom; 2];
    let mut vertical: Vec<[Point; 2]> = Vec::new();
    let mut horizontal: Vec<[Point; 2]> = Vec::new();
    for e in reference_elements(scene, &[], view) {
        for c in corners(&[e], [0.0, 0.0], false, false) {
            let offset_x = c[0] - p[0];
            if offset_x.abs() <= min[0].abs() {
                if offset_x.abs() < min[0].abs() {
                    vertical.clear();
                }
                vertical.push([c, [c[0], p[1]]]);
                min[0] = offset_x;
            }
            let offset_y = c[1] - p[1];
            if offset_y.abs() <= min[1].abs() {
                if offset_y.abs() < min[1].abs() {
                    horizontal.clear();
                }
                horizontal.push([c, [p[0], c[1]]]);
                min[1] = offset_y;
            }
        }
    }
    let offset = [
        vertical.first().map_or(0.0, |l| l[0][0] - p[0]),
        horizontal.first().map_or(0.0, |l| l[0][1] - p[1]),
    ];
    let lines = vertical
        .into_iter()
        .chain(horizontal)
        .map(SnapLine::Pointer)
        .collect();
    (offset, lines)
}

/// `createPointSnapLines`: the points that line up, one line across each
/// place they do.
fn point_lines(near: &Nearest) -> Vec<SnapLine> {
    let mut lines = Vec::new();
    for axis in [0, 1] {
        let snaps = if axis == 0 { &near.x } else { &near.y };
        // By where the moving point is.
        let mut by_place: Vec<(f64, Vec<Point>)> = Vec::new();
        for s in snaps {
            let Snap::Point { points, .. } = s else {
                continue;
            };
            let key = round(points[0][axis]);
            let i = match by_place.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    by_place.push((key, Vec::new()));
                    by_place.len() - 1
                }
            };
            by_place[i]
                .1
                .extend(points.iter().map(|[x, y]| [round(*x), round(*y)]));
        }
        for (key, points) in by_place {
            let mut points: Vec<Point> = points
                .into_iter()
                .map(|[x, y]| if axis == 0 { [key, y] } else { [x, key] })
                .collect();
            let along = 1 - axis;
            js::sort_by(&mut points, |a, b| a[along] - b[along]);
            let mut unique: Vec<Point> = Vec::new();
            for p in points {
                if !unique.contains(&p) {
                    unique.push(p);
                }
            }
            lines.push(SnapLine::Points(unique));
        }
    }
    lines
}

/// `createGapSnapLines`: the gaps kept equal, as marked lines, for what
/// moves with the box `b`.
fn gap_lines(b: Bounds, snaps: &[&Snap]) -> Vec<SnapLine> {
    let [min_x, min_y, max_x, max_y] = b;
    let mut lines: Vec<(Point, Point, bool)> = Vec::new();
    for s in snaps {
        let Snap::Gap { direction, gap, .. } = s else {
            continue;
        };
        let [start_min_x, start_min_y, start_max_x, start_max_y] = gap.start_bounds;
        let [end_min_x, end_min_y, end_max_x, end_max_y] = gap.end_bounds;
        let across = intersection([min_y, max_y], gap.overlap);
        let down = intersection([min_x, max_x], gap.overlap);
        let mid = |[a, b]: [f64; 2]| (a + b) / 2.0;
        match direction {
            GapDirection::CenterHorizontal => {
                if let Some(r) = across {
                    let y = mid(r);
                    lines.push(([gap.start_side[0][0], y], [min_x, y], true));
                    lines.push(([max_x, y], [gap.end_side[0][0], y], true));
                }
            }
            GapDirection::CenterVertical => {
                if let Some(r) = down {
                    let x = mid(r);
                    lines.push(([x, gap.start_side[0][1]], [x, min_y], false));
                    lines.push(([x, max_y], [x, gap.end_side[0][1]], false));
                }
            }
            GapDirection::SideRight => {
                if let Some(r) = across {
                    let y = mid(r);
                    lines.push(([start_max_x, y], [end_min_x, y], true));
                    lines.push(([end_max_x, y], [min_x, y], true));
                }
            }
            GapDirection::SideLeft => {
                if let Some(r) = across {
                    let y = mid(r);
                    lines.push(([max_x, y], [start_min_x, y], true));
                    lines.push(([start_max_x, y], [end_min_x, y], true));
                }
            }
            GapDirection::SideTop => {
                if let Some(r) = down {
                    let x = mid(r);
                    lines.push(([x, max_y], [x, start_min_y], false));
                    lines.push(([x, start_max_y], [x, end_min_y], false));
                }
            }
            GapDirection::SideBottom => {
                if let Some(r) = down {
                    let x = mid(r);
                    lines.push(([x, start_max_y], [x, end_min_y], false));
                    lines.push(([x, end_max_y], [x, min_y], false));
                }
            }
        }
    }
    // Rounded, each once.
    let mut out: Vec<SnapLine> = Vec::new();
    for (from, to, horizontal) in lines {
        let (from, to) = (from.map(round), to.map(round));
        let seen = out
            .iter()
            .any(|l| matches!(l, SnapLine::Gap { from: f, to: t, .. } if *f == from && *t == to));
        if !seen {
            out.push(SnapLine::Gap {
                from,
                to,
                horizontal,
            });
        }
    }
    out
}

/// What the editor knows about snapping: one's choice, what is in view, and
/// what the gesture going on snaps to.
#[derive(Clone, Debug, Default)]
pub struct Snapping {
    /// Snap to other elements (Excalidraw's `objectsSnapModeEnabled`): each
    /// person's own choice, off at first.
    pub to_objects: bool,
    /// The part of the drawing in view: only elements there are snapped to
    /// (`None`: all).
    pub view: Option<Bounds>,
    references: Option<References>,
    lines: Vec<SnapLine>,
    /// How far a new element's first corner moves to snap
    /// (`originSnapOffset`).
    origin: Point,
}

impl Snapping {
    /// The lines showing what snaps (`snapLines`), to draw over the
    /// drawing.
    pub fn lines(&self) -> &[SnapLine] {
        &self.lines
    }

    /// Forgets what a gesture snapped to (it starts or ends).
    pub fn clear(&mut self) {
        self.references = None;
        self.lines.clear();
        self.origin = [0.0, 0.0];
    }

    /// Whether to snap to elements in an event with or without Ctrl.
    fn objects(&self, scene: &Scene, ctrl: bool) -> bool {
        objects_on(self.to_objects, Grid::of(scene).is_some(), ctrl)
    }

    /// What the other elements offer, found once a gesture.
    fn references(&mut self, scene: &Scene, moving: &[&str], gaps: bool) -> &References {
        let view = self.view;
        self.references
            .get_or_insert_with(|| References::new(scene, moving, view, gaps))
    }

    /// The pointer at `p` with a tool that draws boxes: where a new one
    /// would start, lined up with what is there (`getSnapLinesAtPointer`).
    pub(super) fn hover(&mut self, scene: &Scene, p: Point, ctrl: bool, zoom: f64) {
        if self.objects(scene, ctrl) {
            (self.origin, self.lines) = at_pointer(scene, p, self.view, zoom);
        } else {
            self.origin = [0.0, 0.0];
            self.lines.clear();
        }
    }

    /// How far a new element's first corner moved to snap.
    pub(super) fn origin(&self) -> Point {
        self.origin
    }

    /// Where the far corner of a new element `id` goes, the pointer being at
    /// `p` and its first corner at `origin` (before that moved to snap): on
    /// the grid, then lined up with others (`snapNewElement`).
    pub(super) fn new_corner(
        &mut self,
        scene: &Scene,
        id: &str,
        origin: Point,
        p: Point,
        ctrl: bool,
        zoom: f64,
    ) -> Point {
        let q = grid_point(p, grid_size(scene, ctrl));
        if !self.objects(scene, ctrl) {
            return q;
        }
        let o = self.origin;
        let refs = self.references(scene, &[id], false);
        let corner = [
            origin[0] + o[0] + (q[0] - origin[0]),
            origin[1] + o[1] + (q[1] - origin[1]),
        ];
        let mut near = Nearest::new(SNAP_DISTANCE / zoom);
        point_snaps(&[corner], &refs.points, &mut near);
        let [dx, dy] = near.offset();
        [q[0] + dx, q[1] + dy]
    }

    /// Where the `handle` of a selection (`moving`, in its `frame`) goes,
    /// dragged to `at`: on the grid, then lined up with others
    /// (`snapResizingElements`; not when one element is turned).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resized(
        &mut self,
        scene: &Scene,
        moving: &[Element],
        frame: &Frame,
        handle: Handle,
        at: Point,
        ctrl: bool,
        zoom: f64,
    ) -> Point {
        let at = grid_point(at, grid_size(scene, ctrl));
        if !self.objects(scene, ctrl) || moving.is_empty() || frame.angle.abs() > 0.01 {
            self.lines.clear();
            return at;
        }
        let ids: Vec<&str> = moving.iter().map(Element::id).collect();
        let refs = self.references(scene, &ids, false);
        let [dx, dy] = snap_resize(frame.bounds, handle, at, refs, zoom);
        [at[0] + dx, at[1] + dy]
    }

    /// Shows the corners of the box `b` (what was resized or drawn) that
    /// line up with others'.
    pub(super) fn show_box(&mut self, scene: &Scene, b: Bounds, ctrl: bool) {
        self.lines = match &self.references {
            Some(refs) if self.objects(scene, ctrl) => box_lines(b, refs),
            _ => Vec::new(),
        };
    }

    /// How far `originals` go, dragged by `drag`: lined up with others
    /// (`snapDraggedElements`); else, across or down, their corner on the
    /// grid (`dragSelectedElements`).
    pub(super) fn moved(
        &mut self,
        scene: &Scene,
        originals: &[Element],
        drag: Point,
        ctrl: bool,
        zoom: f64,
    ) -> Point {
        let ids: Vec<&str> = originals.iter().map(Element::id).collect();
        // What is selected: labels go with their shapes.
        let selected: Vec<&Element> = originals
            .iter()
            .filter(|e| e.container_id().is_none_or(|c| !ids.contains(&c)))
            .collect();
        let grid = grid_size(scene, ctrl);
        let objects = self.objects(scene, ctrl);
        if selected.is_empty() || (grid.is_none() && !objects) {
            self.lines.clear();
            return drag;
        }
        let (drag, snapped) = if objects {
            let drag = [round(drag[0]), round(drag[1])];
            let refs = self.references(scene, &ids, true);
            let (offset, lines) = snap_drag(&selected, refs, drag, zoom);
            self.lines = lines;
            (drag, offset)
        } else {
            self.lines.clear();
            (drag, [0.0, 0.0])
        };
        // `calculateOffset`.
        let [x, y, _, _] = bounds::common_bounds(selected.iter().copied());
        let mut next = [x + drag[0] + snapped[0], y + drag[1] + snapped[1]];
        let on_grid = grid_point([x + drag[0], y + drag[1]], grid);
        for axis in [0, 1] {
            if snapped[axis] == 0.0 {
                next[axis] = on_grid[axis];
            }
        }
        [next[0] - x, next[1] - y]
    }
}

#[cfg(test)]
mod tests;
