use serde_json::json;

use super::*;

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

fn drag(
    ed: &mut Editor,
    scene: &mut Scene,
    from: Point,
    to: Point,
    mods: Mods,
    rng: &mut Seq,
) -> Output {
    let o = ed.pointer_down(scene, from, None, mods, 1.0, rng);
    apply(scene, &o);
    let o = ed.pointer_move(
        scene,
        [(from[0] + to[0]) / 2.0, (from[1] + to[1]) / 2.0],
        None,
        mods,
        1.0,
        rng,
    );
    apply(scene, &o);
    let o = ed.pointer_move(scene, to, None, mods, 1.0, rng);
    apply(scene, &o);
    let o = ed.pointer_up(scene, to, rng);
    apply(scene, &o);
    o
}

#[test]
fn drawing_a_rectangle_then_moving_it() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.set_tool(Tool::Rectangle);
    let o = drag(
        &mut ed,
        &mut scene,
        [10.0, 20.0],
        [110.0, 70.0],
        Mods::default(),
        &mut rng,
    );
    assert!(o.done && o.tool_changed);
    assert_eq!(ed.tool, Tool::Selection);
    let r = &scene.elements[0];
    assert_eq!(
        (r.kind(), r.x(), r.y(), r.width(), r.height()),
        (ElementType::Rectangle, 10.0, 20.0, 100.0, 50.0)
    );
    assert_eq!(ed.selection, vec![r.id().to_string()]);
    assert!(r.index().is_some() && r.roundness().is_some());
    // Excalidraw's properties, all there.
    for k in [
        "seed",
        "version",
        "versionNonce",
        "strokeColor",
        "boundElements",
        "groupIds",
        "updated",
    ] {
        assert!(r.get(k).is_some(), "{k}");
    }
    // Moving it: selected, it is grabbed anywhere in its box (empty as it is).
    let o = drag(
        &mut ed,
        &mut scene,
        [50.0, 40.0],
        [70.0, 60.0],
        Mods::default(),
        &mut rng,
    );
    assert!(o.done);
    assert_eq!((scene.elements[0].x(), scene.elements[0].y()), (30.0, 40.0));
    assert!(scene.elements[0].version() > 2);
    // A click in empty space clears the selection; a click with a shape
    // tool makes nothing.
    drag(
        &mut ed,
        &mut scene,
        [500.0, 500.0],
        [500.0, 500.0],
        Mods::default(),
        &mut rng,
    );
    assert!(ed.selection.is_empty());
    ed.set_tool(Tool::Ellipse);
    drag(
        &mut ed,
        &mut scene,
        [300.0, 300.0],
        [300.0, 300.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(scene.visible().count(), 1);
}

#[test]
fn shift_squares_and_alt_centres() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.set_tool(Tool::Ellipse);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [100.0, 40.0],
        Mods {
            shift: true,
            ..Mods::default()
        },
        &mut rng,
    );
    let e = &scene.elements[0];
    assert_eq!((e.width(), e.height()), (100.0, 100.0));
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [500.0, 500.0],
        [520.0, 510.0],
        Mods {
            alt: true,
            ..Mods::default()
        },
        &mut rng,
    );
    let r = scene.elements.last().unwrap();
    assert_eq!(
        (r.x(), r.y(), r.width(), r.height()),
        (480.0, 490.0, 40.0, 20.0)
    );
}

#[test]
fn box_selection_and_editing_commands() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    for (x, tool) in [
        (0.0, Tool::Rectangle),
        (200.0, Tool::Diamond),
        (400.0, Tool::Ellipse),
    ] {
        ed.set_tool(tool);
        drag(
            &mut ed,
            &mut scene,
            [x, 0.0],
            [x + 100.0, 100.0],
            Mods::default(),
            &mut rng,
        );
    }
    ed.set_tool(Tool::Selection);
    drag(
        &mut ed,
        &mut scene,
        [-10.0, -10.0],
        [310.0, 110.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(ed.selection.len(), 2);
    // Grouped, a click on one selects both.
    let o = ed.group(&scene, &mut rng);
    apply(&mut scene, &o);
    ed.selection.clear();
    drag(
        &mut ed,
        &mut scene,
        [0.0, 50.0],
        [0.0, 50.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(ed.selection.len(), 2);
    // Duplicated: two new, a little further, in a new group.
    let o = ed.duplicate(&scene, &mut rng);
    apply(&mut scene, &o);
    assert_eq!(scene.visible().count(), 5);
    let copy = scene.element(&ed.selection[0]).unwrap();
    assert_eq!((copy.x(), copy.y()), (10.0, 10.0));
    let originals_group = scene.elements[0].group_ids()[0].to_string();
    assert_ne!(copy.group_ids()[0], originals_group);
    // Deleted.
    let o = ed.delete_selected(&scene, &mut rng);
    apply(&mut scene, &o);
    assert_eq!(scene.visible().count(), 3);
    // Styled: only what is selected changes, and new elements follow.
    ed.select_all(&scene);
    assert_eq!(ed.selection.len(), 3);
    let o = ed.set_property(&scene, "strokeColor", json!("#e03131"), &mut rng);
    apply(&mut scene, &o);
    assert!(scene.visible().all(|e| e.stroke_color() == "#e03131"));
    assert_eq!(ed.style.stroke_color, "#e03131");
    // Aligned left: the group as one, the ellipse to its left side.
    let o = ed.align(&scene, "left", &mut rng);
    apply(&mut scene, &o);
    let lefts: Vec<f64> = scene
        .visible()
        .map(|e| bounds::element_bounds(e)[0])
        .collect();
    assert_eq!(lefts, vec![0.0, 200.0, 0.0]);
}

#[test]
fn stacking_order() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    for x in [0.0, 50.0, 100.0] {
        ed.set_tool(Tool::Rectangle);
        drag(
            &mut ed,
            &mut scene,
            [x, 0.0],
            [x + 100.0, 100.0],
            Mods::default(),
            &mut rng,
        );
    }
    let ids: Vec<String> = scene.elements.iter().map(|e| e.id().to_string()).collect();
    ed.selection = vec![ids[0].clone()];
    let o = ed.reorder(&scene, true, true, &mut rng);
    apply(&mut scene, &o);
    assert_eq!(scene.elements.last().unwrap().id(), ids[0]);
    ed.selection = vec![ids[2].clone()];
    let o = ed.reorder(&scene, false, false, &mut rng);
    apply(&mut scene, &o);
    let order: Vec<&str> = scene.elements.iter().map(Element::id).collect();
    assert_eq!(order, [ids[2].as_str(), ids[1].as_str(), ids[0].as_str()]);
}

#[test]
fn lines_arrows_pen_text_eraser() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    // A dragged arrow: two points, an arrowhead.
    ed.set_tool(Tool::Arrow);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [100.0, 50.0],
        Mods::default(),
        &mut rng,
    );
    let a = &scene.elements[0];
    assert_eq!(a.points(), vec![[0.0, 0.0], [100.0, 50.0]]);
    assert_eq!(a.end_arrowhead(), Some("arrow"));
    // A line clicked point by point, ended with Escape.
    ed.set_tool(Tool::Line);
    for p in [[200.0, 0.0], [300.0, 0.0], [300.0, 100.0]] {
        let o = ed.pointer_down(&scene, p, None, Mods::default(), 1.0, &mut rng);
        apply(&mut scene, &o);
        let o = ed.pointer_up(&scene, p, &mut rng);
        apply(&mut scene, &o);
    }
    let o = ed.pointer_move(&scene, [250.0, 150.0], None, Mods::default(), 1.0, &mut rng);
    apply(&mut scene, &o);
    let o = ed.escape(&scene, &mut rng);
    apply(&mut scene, &o);
    let l = scene
        .elements
        .iter()
        .find(|e| e.kind() == ElementType::Line)
        .unwrap();
    assert_eq!(l.points(), vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
    assert_eq!((l.x(), l.y()), (200.0, 0.0));
    // A pen stroke.
    ed.set_tool(Tool::Freedraw);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 200.0],
        [40.0, 230.0],
        Mods::default(),
        &mut rng,
    );
    let f = scene
        .elements
        .iter()
        .find(|e| e.kind() == ElementType::Freedraw)
        .unwrap();
    assert_eq!(f.points().len(), 3);
    assert!(f.simulate_pressure());
    // A text: made, edited, measured.
    ed.set_tool(Tool::Text);
    let o = ed.pointer_down(&scene, [0.0, 400.0], None, Mods::default(), 1.0, &mut rng);
    let id = o.edit_text.clone().unwrap();
    apply(&mut scene, &o);
    let o = ed.finish_text(
        &scene,
        "hello\nworld!",
        &|_, line| line.len() as f64 * 10.0,
        &mut rng,
    );
    apply(&mut scene, &o);
    let t = scene.element(&id).unwrap();
    assert_eq!(
        (t.text(), t.width(), t.height()),
        ("hello\nworld!", 60.0, 50.0)
    );
    // The eraser deletes what it passes over.
    ed.set_tool(Tool::Eraser);
    let o = drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [50.0, 25.0],
        Mods::default(),
        &mut rng,
    );
    assert!(o.done);
    assert!(scene.elements[0].is_deleted());
}

#[test]
fn resizing_and_turning_through_handles() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [100.0, 50.0],
        Mods::default(),
        &mut rng,
    );
    // The south-east corner.
    drag(
        &mut ed,
        &mut scene,
        [108.0, 58.0],
        [208.0, 108.0],
        Mods::default(),
        &mut rng,
    );
    let r = &scene.elements[0];
    assert_eq!((r.width(), r.height()), (200.0, 100.0));
    // The turning handle, to the right of the centre: a quarter turn.
    let f = ed.frame(&scene).unwrap();
    let rot = transform::handles(&f, 1.0)
        .into_iter()
        .find(|h| h.0 == Handle::Rotation)
        .unwrap()
        .1;
    let at = [rot[0] + rot[2] / 2.0, rot[1] + rot[3] / 2.0];
    drag(
        &mut ed,
        &mut scene,
        at,
        [400.0, 50.0],
        Mods::default(),
        &mut rng,
    );
    assert!((scene.elements[0].angle() - std::f64::consts::FRAC_PI_2).abs() < 0.01);
}

#[test]
fn copy_and_paste_with_new_ids() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [100.0, 100.0],
        Mods::default(),
        &mut rng,
    );
    ed.set_tool(Tool::Arrow);
    drag(
        &mut ed,
        &mut scene,
        [200.0, 0.0],
        [300.0, 0.0],
        Mods::default(),
        &mut rng,
    );
    // The arrow points at the rectangle.
    let (rect, arrow) = (
        scene.elements[0].id().to_string(),
        scene.elements[1].id().to_string(),
    );
    if let Some(a) = scene.elements.iter_mut().find(|e| e.id() == arrow) {
        a.set("startBinding", json!({"elementId": rect, "mode": "orbit"}));
    }
    ed.select_all(&scene);
    let clip = ed.copy(&scene).unwrap();
    // Read back as any clipboard text would be.
    let pasted = crate::file::read_drawing(&clip, &mut crate::restore::Ids::new(9))
        .unwrap()
        .scene
        .elements;
    let o = ed.paste(&scene, &pasted, [1000.0, 1000.0], &mut rng);
    apply(&mut scene, &o);
    assert_eq!(scene.visible().count(), 4);
    let copies: Vec<&Element> = scene.visible().filter(|e| ed.is_selected(e.id())).collect();
    assert_eq!(copies.len(), 2);
    let b = bounds::common_bounds(copies.iter().copied());
    // (A new seed wobbles the arrow a little differently.)
    assert!(((b[0] + b[2]) / 2.0 - 1000.0).abs() < 5.0);
    // The copied arrow points at the copied rectangle.
    let new_arrow = copies
        .iter()
        .find(|e| e.kind() == ElementType::Arrow)
        .unwrap();
    let new_rect = copies
        .iter()
        .find(|e| e.kind() == ElementType::Rectangle)
        .unwrap();
    assert_eq!(new_arrow.start_binding().unwrap().element_id, new_rect.id());
    assert!(copies.iter().all(|e| e.id() != rect && e.id() != arrow));
}

#[test]
fn labels_in_shapes() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.measure = |_, s| s.chars().count() as f64 * 10.0;
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [200.0, 60.0],
        Mods::default(),
        &mut rng,
    );
    let rect = scene.elements[0].id().to_string();
    // A double click inside makes its label.
    let o = ed.double_click(&scene, [100.0, 30.0], 1.0, &mut rng);
    let label = o.edit_text.clone().unwrap();
    apply(&mut scene, &o);
    assert_eq!(scene.element(&rect).unwrap().bound_elements()[0].id, label);
    let o = ed.finish_text(
        &scene,
        "hello",
        &|_, s| s.chars().count() as f64 * 10.0,
        &mut rng,
    );
    apply(&mut scene, &o);
    let t = scene.element(&label).unwrap();
    assert_eq!(t.container_id(), Some(rect.as_str()));
    assert_eq!((t.x(), t.width()), (75.0, 50.0));
    // A click on the label selects the shape; resizing it lays the label out again.
    ed.selection = vec![rect.clone()];
    drag(
        &mut ed,
        &mut scene,
        [208.0, 68.0],
        [408.0, 68.0],
        Mods::default(),
        &mut rng,
    );
    let t = scene.element(&label).unwrap();
    assert_eq!((t.x(), t.font_size()), (175.0, 20.0));
    // Long text wraps and the shape grows to hold it.
    ed.selection = vec![rect.clone()];
    let o = ed.enter(&scene, &mut rng);
    assert_eq!(o.edit_text.as_deref(), Some(label.as_str()));
    let long = "a long line of words that will not fit in one line of this box at all, not even in two lines of it";
    let o = ed.finish_text(
        &scene,
        long,
        &|_, s| s.chars().count() as f64 * 10.0,
        &mut rng,
    );
    apply(&mut scene, &o);
    let (t, r) = (
        scene.element(&label).unwrap(),
        scene.element(&rect).unwrap(),
    );
    assert!(t.text().contains('\n'));
    assert!(r.height() > 60.0);
    // Emptied, the label goes and the shape forgets it.
    ed.enter(&scene, &mut rng);
    let o = ed.finish_text(
        &scene,
        "  ",
        &|_, s| s.chars().count() as f64 * 10.0,
        &mut rng,
    );
    apply(&mut scene, &o);
    assert!(scene.element(&label).unwrap().is_deleted());
    assert!(scene.element(&rect).unwrap().bound_elements().is_empty());
}

#[test]
fn arrows_attach_and_follow() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [0.0, 0.0],
        [100.0, 100.0],
        Mods::default(),
        &mut rng,
    );
    ed.set_tool(Tool::Rectangle);
    drag(
        &mut ed,
        &mut scene,
        [300.0, 0.0],
        [400.0, 100.0],
        Mods::default(),
        &mut rng,
    );
    let (a, b) = (
        scene.elements[0].id().to_string(),
        scene.elements[1].id().to_string(),
    );
    ed.set_tool(Tool::Arrow);
    drag(
        &mut ed,
        &mut scene,
        [50.0, 50.0],
        [350.0, 50.0],
        Mods::default(),
        &mut rng,
    );
    let arrow = scene
        .elements
        .iter()
        .find(|e| e.kind() == ElementType::Arrow)
        .unwrap()
        .clone();
    assert_eq!(arrow.start_binding().unwrap().element_id, a);
    assert_eq!(arrow.end_binding().unwrap().element_id, b);
    assert!(
        scene
            .element(&a)
            .unwrap()
            .bound_elements()
            .iter()
            .any(|x| x.id == arrow.id())
    );
    // It stops at the outlines, not in the middles.
    assert!((arrow.x() - 106.0).abs() < 1e-6, "{}", arrow.x());
    // The right box moves down: the arrow's end follows.
    ed.selection = vec![b.clone()];
    drag(
        &mut ed,
        &mut scene,
        [350.0, 50.0],
        [350.0, 250.0],
        Mods::default(),
        &mut rng,
    );
    let moved = scene.element(arrow.id()).unwrap();
    let end = [
        moved.x() + moved.points()[1][0],
        moved.y() + moved.points()[1][1],
    ];
    assert!(end[1] > 150.0, "{end:?}");
    // Dragged on its own, it lets go.
    ed.selection = vec![arrow.id().to_string()];
    let mid = [(moved.x() + end[0]) / 2.0, (moved.y() + end[1]) / 2.0];
    drag(
        &mut ed,
        &mut scene,
        mid,
        [mid[0], mid[1] + 300.0],
        Mods::default(),
        &mut rng,
    );
    let loose = scene.element(arrow.id()).unwrap();
    assert!(loose.start_binding().is_none() && loose.end_binding().is_none());
}

/// Two boxes, and an elbow arrow drawn from the right side of the first to
/// the top of the second (its id).
fn elbow_between_boxes(ed: &mut Editor, scene: &mut Scene, rng: &mut Seq) -> String {
    for [x, y] in [[0.0, 0.0], [192.0, 192.0]] {
        ed.set_tool(Tool::Rectangle);
        drag(
            ed,
            scene,
            [x, y],
            [x + 64.0, y + 64.0],
            Mods::default(),
            rng,
        );
    }
    ed.style.arrow_type = "elbow".into();
    ed.set_tool(Tool::Arrow);
    drag(
        ed,
        scene,
        [63.0, 16.0],
        [208.0, 193.0],
        Mods::default(),
        rng,
    );
    scene
        .elements
        .iter()
        .find(|e| e.kind() == ElementType::Arrow)
        .unwrap()
        .id()
        .to_string()
}

fn route_of(scene: &Scene, id: &str) -> Vec<Point> {
    let a = scene.element(id).unwrap();
    a.points()
        .iter()
        .map(|p| [a.x() + p[0], a.y() + p[1]])
        .collect()
}

#[test]
fn elbow_arrows_are_routed_and_follow() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    let id = elbow_between_boxes(&mut ed, &mut scene, &mut rng);
    let (a, b) = (
        scene.elements[0].id().to_string(),
        scene.elements[1].id().to_string(),
    );
    let arrow = scene.element(&id).unwrap();
    assert!(arrow.is_elbowed());
    // Attached at the outlines, a gap away; out to the right, down onto it.
    let start = arrow.start_binding().unwrap();
    assert_eq!(
        (start.element_id.as_str(), start.fixed_point),
        (a.as_str(), Some([1.09375, 0.25]))
    );
    assert_eq!(arrow.end_binding().unwrap().element_id, b);
    assert_eq!(
        route_of(&scene, &id),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    // The second box moves right: routed again.
    ed.selection = vec![b.clone()];
    drag(
        &mut ed,
        &mut scene,
        [224.0, 224.0],
        [288.0, 224.0],
        Mods::default(),
        &mut rng,
    );
    assert_eq!(
        route_of(&scene, &id),
        vec![[70.0, 16.0], [272.0, 16.0], [272.0, 186.0]]
    );
    // Dragged on its own, attached as it is, it stays put.
    ed.selection = vec![id.clone()];
    let before = scene.element(&id).unwrap().clone();
    drag(
        &mut ed,
        &mut scene,
        [100.0, 16.0],
        [100.0, 300.0],
        Mods::default(),
        &mut rng,
    );
    let after = scene.element(&id).unwrap();
    assert_eq!((after.x(), after.y()), (before.x(), before.y()));
    assert!(after.start_binding().is_some() && after.end_binding().is_some());
    // Nor do the arrow keys move it.
    let o = ed.nudge(&scene, 10.0, 0.0, &mut rng);
    assert!(o.changed.is_empty());
}

#[test]
fn elbow_arrows_by_two_clicks() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    for [x, y] in [[0.0, 0.0], [192.0, 192.0]] {
        ed.set_tool(Tool::Rectangle);
        drag(
            &mut ed,
            &mut scene,
            [x, y],
            [x + 64.0, y + 64.0],
            Mods::default(),
            &mut rng,
        );
    }
    ed.style.arrow_type = "elbow".into();
    ed.set_tool(Tool::Arrow);
    let m = Mods::default();
    let o = ed.pointer_down(&scene, [63.0, 16.0], None, m, 1.0, &mut rng);
    apply(&mut scene, &o);
    let o = ed.pointer_up(&scene, [63.0, 16.0], &mut rng);
    apply(&mut scene, &o);
    let o = ed.pointer_move(&scene, [150.0, 150.0], None, m, 1.0, &mut rng);
    apply(&mut scene, &o);
    // The second click ends it there: no points in between.
    let o = ed.pointer_down(&scene, [208.0, 193.0], None, m, 1.0, &mut rng);
    apply(&mut scene, &o);
    assert!(o.done && !ed.busy());
    let id = ed.selection[0].clone();
    assert_eq!(
        route_of(&scene, &id),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    assert!(scene.element(&id).unwrap().end_binding().is_some());
}

#[test]
fn elbow_arrow_segments_are_moved_and_let_go() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    let id = elbow_between_boxes(&mut ed, &mut scene, &mut rng);
    ed.selection = vec![id.clone()];
    // The upright segment's handle, dragged left: it stays there, clear of
    // the box it enters.
    drag(
        &mut ed,
        &mut scene,
        [208.0, 101.0],
        [180.0, 101.0],
        Mods::default(),
        &mut rng,
    );
    let arrow = scene.element(&id).unwrap();
    assert_eq!(
        route_of(&scene, &id),
        vec![
            [70.0, 16.0],
            [180.0, 16.0],
            [180.0, 146.0],
            [208.0, 146.0],
            [208.0, 186.0]
        ]
    );
    assert!(elbow::is_fixed(arrow, 2));
    // A double click on its handle lets it go.
    let o = ed.double_click(&scene, [180.0, 81.0], 1.0, &mut rng);
    apply(&mut scene, &o);
    assert!(o.done && o.edit_text.is_none());
    assert_eq!(
        route_of(&scene, &id),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    assert_eq!(
        scene.element(&id).unwrap().get("fixedSegments"),
        Some(&Value::Null)
    );
}

#[test]
fn arrow_types_change() {
    let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0));
    let id = elbow_between_boxes(&mut ed, &mut scene, &mut rng);
    ed.selection = vec![id.clone()];
    let o = ed.set_property(&scene, "arrowType", json!("sharp"), &mut rng);
    apply(&mut scene, &o);
    let a = scene.element(&id).unwrap();
    assert!(!a.is_elbowed() && a.roundness().is_none() && a.get("arrowType").is_none());
    assert_eq!(route_of(&scene, &id), vec![[70.0, 16.0], [208.0, 186.0]]);
    let o = ed.set_property(&scene, "arrowType", json!("elbow"), &mut rng);
    apply(&mut scene, &o);
    assert_eq!(
        route_of(&scene, &id),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    assert_eq!(ed.style.arrow_type, "elbow");
    // An arrowhead changed: routed again to make room for it.
    let o = ed.set_property(&scene, "endArrowhead", json!("triangle"), &mut rng);
    assert!(o.changed[0].is_elbowed() && o.changed[0].points().len() == 3);
}
