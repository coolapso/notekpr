use serde_json::json;

use super::*;
use crate::edit::{Editor, Mods, Output, Random, Tool};

/// Repeatable ids, seeds and time.
struct Seq(u32);

impl Random for Seq {
    fn id(&mut self) -> String {
        self.0 += 1;
        format!("id{:04}", self.0)
    }
    fn seed(&mut self) -> i32 {
        self.0 += 1;
        self.0 as i32 * 7919
    }
    fn now(&self) -> f64 {
        1_800_000_000_000.0
    }
}

/// The scene with `out` written to it, as the app does.
fn apply(scene: &mut Scene, out: &Output) {
    for c in &out.changed {
        match scene.elements.iter_mut().find(|e| e.id() == c.id()) {
            Some(e) => *e = c.clone(),
            None => scene.elements.push(c.clone()),
        }
    }
    scene
        .elements
        .sort_by(|a, b| a.index().cmp(&b.index()).then(a.id().cmp(b.id())));
}

/// The pointer pressed at `from` and moved to `to` (by way of the middle),
/// not let go yet.
fn press(ed: &mut Editor, scene: &mut Scene, from: Point, to: Point, mods: Mods, rng: &mut Seq) {
    let o = ed.pointer_down(scene, from, None, mods, 1.0, rng);
    apply(scene, &o);
    let mid = [(from[0] + to[0]) / 2.0, (from[1] + to[1]) / 2.0];
    let o = ed.pointer_move(scene, mid, None, mods, 1.0, rng);
    apply(scene, &o);
    let o = ed.pointer_move(scene, to, None, mods, 1.0, rng);
    apply(scene, &o);
}

fn release(ed: &mut Editor, scene: &mut Scene, at: Point, rng: &mut Seq) {
    let o = ed.pointer_up(scene, at, rng);
    apply(scene, &o);
}

fn drag(ed: &mut Editor, scene: &mut Scene, from: Point, to: Point, mods: Mods, rng: &mut Seq) {
    press(ed, scene, from, to, mods, rng);
    release(ed, scene, to, rng);
}

/// A rectangle drawn from `from` to `to`; its id (it is selected).
fn rect(ed: &mut Editor, scene: &mut Scene, from: Point, to: Point, rng: &mut Seq) -> String {
    ed.set_tool(Tool::Rectangle);
    drag(ed, scene, from, to, Mods::default(), rng);
    ed.selection[0].clone()
}

fn ctrl() -> Mods {
    Mods {
        ctrl: true,
        ..Mods::default()
    }
}

fn place(scene: &Scene, id: &str) -> [f64; 4] {
    let e = scene.element(id).unwrap();
    [e.x(), e.y(), e.width(), e.height()]
}

fn grid_on(scene: &mut Scene) {
    scene.app_state.extend(Grid::settings(true));
}

#[test]
fn grid_points_and_the_drawings_setting() {
    assert_eq!(grid_point([29.9, -10.0], Some(20.0)), [20.0, 0.0]);
    // Halves go up, as in JavaScript.
    assert_eq!(grid_point([30.0, 10.0], Some(20.0)), [40.0, 20.0]);
    assert_eq!(grid_point([-30.0, 3.0], Some(20.0)), [-20.0, 0.0]);
    assert_eq!(grid_point([29.9, 3.0], None), [29.9, 3.0]);
    // Off unless the drawing says it is on.
    let mut scene = Scene::default();
    assert_eq!(Grid::of(&scene), None);
    grid_on(&mut scene);
    assert_eq!(scene.app_state.get("gridModeEnabled"), Some(&json!(true)));
    assert_eq!(Grid::of(&scene), Some(DEFAULT_GRID));
    assert_eq!(grid_size(&scene, false), Some(20.0));
    assert_eq!(grid_size(&scene, true), None, "Ctrl held");
    // Sizes as Excalidraw reads them.
    scene.app_state.insert("gridSize".into(), json!(7.6));
    scene.app_state.insert("gridStep".into(), json!(0));
    assert_eq!(
        Grid::of(&scene),
        Some(Grid {
            size: 8.0,
            step: 1.0
        })
    );
    scene.app_state.insert("gridSize".into(), json!("big"));
    scene.app_state.insert("gridStep".into(), json!(1000));
    assert_eq!(
        Grid::of(&scene),
        Some(Grid {
            size: 20.0,
            step: 100.0
        })
    );
    // Saved with the drawing.
    let file = crate::file::write_drawing(&scene);
    let back = crate::file::read_drawing(&file, &mut crate::restore::Ids::new(1))
        .unwrap()
        .scene;
    assert!(Grid::of(&back).is_some());
    scene.app_state.extend(Grid::settings(false));
    assert_eq!(Grid::of(&scene), None);
    // Arrow keys: a square of the grid; Shift a pixel.
    assert_eq!(
        (nudge_step(Some(20.0), false), nudge_step(Some(20.0), true)),
        (20.0, 1.0)
    );
    assert_eq!(
        (nudge_step(None, false), nudge_step(None, true)),
        (1.0, 5.0)
    );
}

#[test]
fn ctrl_turns_snapping_the_other_way() {
    // On: Ctrl turns it off.
    assert!(objects_on(true, false, false));
    assert!(!objects_on(true, false, true));
    // Off: Ctrl turns it on, unless the grid is on (then Ctrl turns that off).
    assert!(!objects_on(false, false, false));
    assert!(objects_on(false, false, true));
    assert!(!objects_on(false, true, true));
}

#[test]
fn the_grid_places_new_shapes_moves_resizes_and_points() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    grid_on(&mut scene);
    let r = rect(&mut ed, &mut scene, [13.0, 27.0], [104.0, 66.0], &mut rng);
    assert_eq!(place(&scene, &r), [20.0, 20.0, 80.0, 40.0]);
    // Ctrl held: where the pointer is.
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [13.0, 127.0],
        [104.0, 166.0],
        ctrl(),
        &mut rng,
    );
    let free = ed.selection[0].clone();
    assert_eq!(place(&scene, &free), [13.0, 127.0, 91.0, 39.0]);
    // Moved: its corner goes on the grid.
    ed.selection = vec![r.clone()];
    drag(
        &mut ed,
        &mut scene,
        [50.0, 40.0],
        [83.0, 51.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &r), [60.0, 40.0, 80.0, 40.0]);
    // Ctrl: moved as the pointer was.
    drag(
        &mut ed,
        &mut scene,
        [70.0, 50.0],
        [73.0, 51.0],
        ctrl(),
        &mut rng,
    );
    assert_eq!(place(&scene, &r), [63.0, 41.0, 80.0, 40.0]);
    drag(
        &mut ed,
        &mut scene,
        [100.0, 60.0],
        [97.0, 59.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &r), [60.0, 40.0, 80.0, 40.0]);
    // Resized by its south-east corner: that corner on the grid.
    drag(
        &mut ed,
        &mut scene,
        [148.0, 88.0],
        [171.0, 99.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &r), [60.0, 40.0, 100.0, 60.0]);
    // A line's points.
    ed.set_tool(Tool::Line);
    drag(
        &mut ed,
        &mut scene,
        [3.0, 198.0],
        [57.0, 244.0],
        Mods::default(),
        &mut rng,
    );
    let line = scene.element(&ed.selection[0]).unwrap();
    assert_eq!((line.x(), line.y()), (0.0, 200.0));
    assert_eq!(line.points(), vec![[0.0, 0.0], [60.0, 40.0]]);
    // Pasted: their corner on the grid.
    assert_eq!(
        paste_offset(&scene, [0.0, 0.0, 40.0, 40.0], [33.0, 33.0]),
        [20.0, 20.0]
    );
    assert_eq!(
        paste_offset(&Scene::default(), [0.0, 0.0, 40.0, 40.0], [33.0, 33.0]),
        [13.0, 13.0]
    );
}

/// Snapping on, a box at (0, 0)–(100, 100) and a small one, selected, at
/// (300, 200)–(350, 250).
fn two_boxes() -> (Editor, Scene, Seq, String) {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.snap.to_objects = true;
    rect(&mut ed, &mut scene, [0.0, 0.0], [100.0, 100.0], &mut rng);
    let b = rect(
        &mut ed,
        &mut scene,
        [300.0, 200.0],
        [350.0, 250.0],
        &mut rng,
    );
    (ed, scene, rng, b)
}

#[test]
fn a_moved_box_snaps_to_another_ones_edges_and_centre() {
    let (mut ed, mut scene, mut rng, b) = two_boxes();
    // Its left side 5 from the other's: they line up.
    press(
        &mut ed,
        &mut scene,
        [325.0, 225.0],
        [30.0, 225.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 0.0);
    // Shown: a line through the corners that line up, across.
    let lines = ed.snap.lines().to_vec();
    assert!(
        lines.contains(&SnapLine::Points(vec![
            [0.0, 0.0],
            [0.0, 100.0],
            [0.0, 200.0],
            [0.0, 250.0]
        ])),
        "{lines:?}"
    );
    release(&mut ed, &mut scene, [30.0, 225.0], &mut rng);
    assert!(ed.snap.lines().is_empty(), "gone when it ends");
    // Its middle 4 below the other's: they line up.
    drag(
        &mut ed,
        &mut scene,
        [25.0, 225.0],
        [25.0, 54.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b), [0.0, 25.0, 50.0, 50.0]);
}

#[test]
fn ctrl_moves_without_snapping_and_snaps_when_it_is_off() {
    let (mut ed, mut scene, mut rng, b) = two_boxes();
    press(
        &mut ed,
        &mut scene,
        [325.0, 225.0],
        [30.0, 225.0],
        ctrl(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 5.0);
    assert!(ed.snap.lines().is_empty());
    release(&mut ed, &mut scene, [30.0, 225.0], &mut rng);
    // Off, Ctrl snaps.
    ed.snap.to_objects = false;
    drag(
        &mut ed,
        &mut scene,
        [30.0, 225.0],
        [32.0, 225.0],
        ctrl(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 0.0);
    // Unless the grid is on: then Ctrl turns that off too.
    grid_on(&mut scene);
    drag(
        &mut ed,
        &mut scene,
        [25.0, 225.0],
        [32.0, 226.0],
        ctrl(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b), [7.0, 201.0, 50.0, 50.0]);
}

#[test]
fn equal_gaps() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.snap.to_objects = true;
    // Two boxes, 200 apart; a third, lower and smaller, further right.
    rect(&mut ed, &mut scene, [0.0, 0.0], [100.0, 100.0], &mut rng);
    rect(&mut ed, &mut scene, [300.0, 0.0], [400.0, 100.0], &mut rng);
    let b = rect(&mut ed, &mut scene, [700.0, 20.0], [760.0, 60.0], &mut rng);
    // 203 right of the second: as far from it as the gap is wide.
    press(
        &mut ed,
        &mut scene,
        [730.0, 40.0],
        [633.0, 40.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 600.0);
    let gaps: Vec<&SnapLine> = ed
        .snap
        .lines()
        .iter()
        .filter(|l| matches!(l, SnapLine::Gap { .. }))
        .collect();
    assert_eq!(
        gaps,
        [
            &SnapLine::Gap {
                from: [100.0, 40.0],
                to: [300.0, 40.0],
                horizontal: true
            },
            &SnapLine::Gap {
                from: [400.0, 40.0],
                to: [600.0, 40.0],
                horizontal: true
            }
        ]
    );
    release(&mut ed, &mut scene, [633.0, 40.0], &mut rng);
    // In the middle of the gap.
    drag(
        &mut ed,
        &mut scene,
        [630.0, 40.0],
        [203.0, 40.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 170.0);
}

#[test]
fn a_new_box_starts_and_ends_lined_up() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.snap.to_objects = true;
    rect(&mut ed, &mut scene, [0.0, 0.0], [100.0, 100.0], &mut rng);
    ed.set_tool(Tool::Rectangle);
    // Over the canvas: where it would start, lined up with the box's right.
    let o = ed.pointer_move(&scene, [103.0, 150.0], None, Mods::default(), 1.0, &mut rng);
    assert!(o.changed.is_empty());
    assert_eq!(
        ed.snap.lines(),
        [
            SnapLine::Pointer([[100.0, 0.0], [100.0, 150.0]]),
            SnapLine::Pointer([[100.0, 100.0], [100.0, 150.0]])
        ]
    );
    // Drawn up to just below its bottom: both corners lined up.
    press(
        &mut ed,
        &mut scene,
        [103.0, 150.0],
        [160.0, 97.0],
        Mods::default(),
        &mut rng,
    );
    let id = scene.elements.last().unwrap().id().to_string();
    assert_eq!(place(&scene, &id), [100.0, 100.0, 57.0, 50.0]);
    assert!(!ed.snap.lines().is_empty());
    release(&mut ed, &mut scene, [160.0, 97.0], &mut rng);
    // Other tools show nothing.
    ed.set_tool(Tool::Arrow);
    ed.pointer_move(&scene, [103.0, 150.0], None, Mods::default(), 1.0, &mut rng);
    assert!(ed.snap.lines().is_empty());
}

#[test]
fn a_resized_side_snaps() {
    let (mut ed, mut scene, mut rng, b) = two_boxes();
    // The left side, to 4 right of the other box's right side.
    press(
        &mut ed,
        &mut scene,
        [299.0, 225.0],
        [103.0, 225.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b), [100.0, 200.0, 250.0, 50.0]);
    assert!(!ed.snap.lines().is_empty());
    release(&mut ed, &mut scene, [103.0, 225.0], &mut rng);
}

#[test]
fn only_what_is_in_view_counts() {
    let (mut ed, mut scene, mut rng, b) = two_boxes();
    // The view shows the small box only.
    ed.snap.view = Some([250.0, 150.0, 800.0, 600.0]);
    drag(
        &mut ed,
        &mut scene,
        [325.0, 225.0],
        [30.0, 225.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(place(&scene, &b)[0], 5.0);
}
