//! Frames, mostly as Excalidraw tests them (`frame.test.tsx`: a 150 frame
//! at 0,0 and rectangles around it, a mouse dragging them about).

use serde_json::json;

use super::*;
use crate::edit::{Mods, Tool};
use crate::svg::{self, SvgOptions};

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

/// Ten pixels a character.
fn m(_: &str, s: &str) -> f64 {
    s.chars().count() as f64 * 10.0
}

fn el(v: Value) -> Element {
    Element::from_props(v.as_object().unwrap().clone()).unwrap()
}

/// Excalidraw's test elements: `y` is `x`, `height` is `width`, unless
/// given.
fn element(kind: &str, id: &str, x: f64, w: f64, extra: Value) -> Element {
    let mut e = el(json!({
        "id": id, "type": kind, "x": x, "y": x, "width": w, "height": w, "angle": 0,
        "strokeWidth": 2, "backgroundColor": "transparent", "seed": 1, "roughness": 1,
        "groupIds": [], "frameId": null, "version": 1
    }));
    for (k, v) in extra.as_object().unwrap() {
        e.set(k, v.clone());
    }
    e
}

fn the_frame() -> Element {
    element("frame", "frame", 0.0, 150.0, json!({"roughness": 0}))
}

/// A scene of these, stacked in this order.
fn scene(elements: Vec<Element>) -> Scene {
    let keys = order::keys_between(None, None, elements.len()).unwrap();
    Scene {
        elements: elements
            .into_iter()
            .zip(keys)
            .map(|(mut e, k)| {
                e.set("index", json!(k));
                e
            })
            .collect(),
        ..Scene::default()
    }
}

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

fn editor() -> Editor {
    Editor {
        measure: m,
        ..Editor::default()
    }
}

fn down_move_up(
    ed: &mut Editor,
    scene: &mut Scene,
    from: Point,
    to: Point,
    rng: &mut Seq,
) -> Output {
    let o = ed.pointer_down(scene, from, None, Mods::default(), 1.0, rng);
    apply(scene, &o);
    let o = ed.pointer_move(scene, to, None, Mods::default(), 1.0, rng);
    apply(scene, &o);
    let o = ed.pointer_up(scene, to, rng);
    apply(scene, &o);
    o
}

fn click(ed: &mut Editor, scene: &mut Scene, at: Point, rng: &mut Seq) {
    down_move_up(ed, scene, at, at, rng);
}

/// `dragElementIntoFrame`: selected by its corner, dragged by its middle
/// to the frame's.
fn drag_into(ed: &mut Editor, scene: &mut Scene, id: &str, frame: &str, rng: &mut Seq) {
    let (e, f) = (
        scene.element(id).unwrap().clone(),
        scene.element(frame).unwrap().clone(),
    );
    click(ed, scene, [e.x(), e.y()], rng);
    let mid = |e: &Element| [e.x() + e.width() / 2.0, e.y() + e.height() / 2.0];
    down_move_up(ed, scene, mid(&e), mid(&f), rng);
}

fn ids(scene: &Scene) -> Vec<&str> {
    scene.visible().map(Element::id).collect()
}

fn frame_of<'a>(scene: &'a Scene, id: &str) -> Option<&'a str> {
    scene.element(id).unwrap().frame_id()
}

#[test]
fn an_element_around_a_frame_overlaps_it() {
    let around = element("rectangle", "r", -50.0, 250.0, json!({}));
    assert!(overlaps(&around, &the_frame()));
    let far = element("rectangle", "r", 400.0, 50.0, json!({}));
    assert!(!overlaps(&far, &the_frame()));
    let across = element("rectangle", "r", 100.0, 100.0, json!({}));
    assert!(intersects(&across, &the_frame()) && overlaps(&across, &the_frame()));
}

#[test]
fn a_new_frame_takes_a_group_it_matches_edge_to_edge() {
    // A hair outside, as snapping the frame to it leaves it.
    let a = element(
        "rectangle",
        "a",
        100.0 - 1e-8,
        100.0,
        json!({"y": 100, "height": 100, "strokeWidth": 4, "groupIds": ["group"]}),
    );
    let b = element(
        "rectangle",
        "b",
        200.0,
        100.0 + 1e-8,
        json!({"y": 100, "height": 100, "strokeWidth": 4, "groupIds": ["group"]}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![a, b]), Seq(0));
    ed.set_tool(Tool::Frame);
    down_move_up(&mut ed, &mut s, [100.0, 100.0], [300.0, 200.0], &mut rng);
    let f = s.visible().find(|e| is_frame(e)).unwrap().clone();
    assert_eq!((f.x(), f.y(), f.width()), (100.0, 100.0, 200.0));
    assert_eq!(
        (frame_of(&s, "a"), frame_of(&s, "b")),
        (Some(f.id()), Some(f.id()))
    );
    // Stacked just below it.
    assert_eq!(ids(&s), ["a", "b", f.id()]);
}

#[test]
fn a_resized_frame_takes_a_group_it_matches_edge_to_edge() {
    let a = element(
        "rectangle",
        "a",
        160.0,
        40.0,
        json!({"y": 0, "height": 100, "groupIds": ["group"]}),
    );
    let b = element(
        "rectangle",
        "b",
        200.0,
        50.0 + 1e-8,
        json!({"y": 0, "height": 100, "groupIds": ["group"]}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), a, b]), Seq(0));
    click(&mut ed, &mut s, [0.0, 0.0], &mut rng);
    assert_eq!(ed.selection, ["frame"]);
    down_move_up(&mut ed, &mut s, [150.0, 150.0], [250.0, 100.0], &mut rng);
    let f = s.element("frame").unwrap();
    assert_eq!((f.width(), f.height()), (250.0, 100.0));
    assert_eq!(
        (frame_of(&s, "a"), frame_of(&s, "b")),
        (Some("frame"), Some("frame"))
    );
    // Shrunk away from them, it lets them go.
    down_move_up(&mut ed, &mut s, [250.0, 100.0], [100.0, 100.0], &mut rng);
    assert_eq!((frame_of(&s, "a"), frame_of(&s, "b")), (None, None));
}

#[test]
fn children_are_clipped_to_their_frame_unless_dragged_out() {
    let child = element(
        "rectangle",
        "child",
        1e-7,
        150.0 - 2e-7,
        json!({"height": 100.0 - 2e-7, "frameId": "frame"}),
    );
    let s = scene(vec![child, the_frame()]);
    let ed = editor();
    assert_eq!(
        ed.clip_frame(&s, s.element("child").unwrap())
            .map(Element::id),
        Some("frame")
    );
    // Dragged out of it: shown whole while it goes.
    let out = element(
        "rectangle",
        "out",
        100.0,
        100.0,
        json!({"y": 0, "height": 50, "frameId": "frame"}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![out, the_frame()]), Seq(0));
    click(&mut ed, &mut s, [100.0, 25.0], &mut rng);
    assert_eq!(ed.selection, ["out"]);
    let o = ed.pointer_down(&s, [125.0, 25.0], None, Mods::default(), 1.0, &mut rng);
    apply(&mut s, &o);
    let o = ed.pointer_move(&s, [425.0, 25.0], None, Mods::default(), 1.0, &mut rng);
    apply(&mut s, &o);
    assert!(ed.dragging());
    assert_eq!(ed.clip_frame(&s, s.element("out").unwrap()), None);
    // Dropped there, it is out of the frame.
    let o = ed.pointer_up(&s, [425.0, 25.0], &mut rng);
    apply(&mut s, &o);
    assert_eq!(frame_of(&s, "out"), None);
}

/// Draws a rectangle from 20,20 to 40,40 (`frame.test.tsx`'s).
fn draw_rectangle(ed: &mut Editor, s: &mut Scene, rng: &mut Seq) -> String {
    ed.set_tool(Tool::Rectangle);
    let o = ed.pointer_down(s, [20.0, 20.0], None, Mods::default(), 1.0, rng);
    apply(s, &o);
    let o = ed.pointer_move(s, [40.0, 40.0], None, Mods::default(), 1.0, rng);
    apply(s, &o);
    // Where it goes is shown while it is drawn.
    let shown = ed.frame_to_highlight().map(str::to_string);
    let o = ed.pointer_up(s, [40.0, 40.0], rng);
    apply(s, &o);
    assert_eq!(ed.frame_to_highlight(), None);
    let id = ed.selection[0].clone();
    assert_eq!(shown.as_deref(), frame_of(s, &id));
    id
}

fn cover() -> Element {
    element(
        "rectangle",
        "cover",
        10.0,
        80.0,
        json!({"backgroundColor": "#ffc9c9"}),
    )
}

#[test]
fn new_elements_go_in_the_frame_where_made_unless_covered() {
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), cover()]), Seq(0));
    let id = draw_rectangle(&mut ed, &mut s, &mut rng);
    assert_eq!(frame_of(&s, &id), None);
    assert_eq!(ids(&s), ["frame", "cover", id.as_str()]);
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![cover(), the_frame()]), Seq(0));
    let id = draw_rectangle(&mut ed, &mut s, &mut rng);
    assert_eq!(frame_of(&s, &id), Some("frame"));
    // A locked frame over it doesn't take it (moved off its outline: ours
    // reaches further than Excalidraw's).
    let locked = element(
        "frame",
        "locked",
        5.0,
        80.0,
        json!({"locked": true, "roughness": 0}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), locked]), Seq(0));
    let id = draw_rectangle(&mut ed, &mut s, &mut rng);
    assert_eq!(frame_of(&s, &id), Some("frame"));
}

#[test]
fn hovering_with_a_tool_shows_the_frame() {
    let (mut ed, s, mut rng) = (editor(), scene(vec![the_frame()]), Seq(0));
    ed.set_tool(Tool::Rectangle);
    ed.pointer_move(&s, [20.0, 20.0], None, Mods::default(), 1.0, &mut rng);
    assert_eq!(ed.frame_to_highlight(), Some("frame"));
    ed.pointer_move(&s, [200.0, 200.0], None, Mods::default(), 1.0, &mut rng);
    assert_eq!(ed.frame_to_highlight(), None);
}

fn children() -> (Element, Element) {
    (
        element(
            "rectangle",
            "under",
            10.0,
            80.0,
            json!({"backgroundColor": "#ffc9c9", "frameId": "frame"}),
        ),
        element(
            "rectangle",
            "other",
            100.0,
            20.0,
            json!({"y": 20, "frameId": "frame"}),
        ),
    )
}

#[test]
fn new_children_stack_below_the_frame_or_above_its_top_child() {
    let (under, other) = children();
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![under, other, the_frame()]), Seq(0));
    let id = draw_rectangle(&mut ed, &mut s, &mut rng);
    assert_eq!(frame_of(&s, &id), Some("frame"));
    assert_eq!(ids(&s), ["under", "other", id.as_str(), "frame"]);
    let (under, other) = children();
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), under, other]), Seq(0));
    let id = draw_rectangle(&mut ed, &mut s, &mut rng);
    assert_eq!(frame_of(&s, &id), Some("frame"));
    assert_eq!(ids(&s), ["frame", "under", "other", id.as_str()]);
}

fn rect2() -> Element {
    element("rectangle", "rect2", 200.0, 50.0, json!({}))
}

#[test]
fn dragged_into_a_frame_joins_it() {
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![rect2(), the_frame()]), Seq(0));
    drag_into(&mut ed, &mut s, "rect2", "frame", &mut rng);
    assert_eq!(frame_of(&s, "rect2"), Some("frame"));
    assert_eq!(ids(&s), ["rect2", "frame"]);
    // Even when it is around the frame.
    let around = element("rectangle", "around", 220.0, 300.0, json!({"y": 20}));
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), around]), Seq(0));
    drag_into(&mut ed, &mut s, "around", "frame", &mut rng);
    assert_eq!(frame_of(&s, "around"), Some("frame"));
}

#[test]
fn dragged_from_one_frame_into_another() {
    let other = element(
        "frame",
        "otherFrame",
        300.0,
        150.0,
        json!({"y": 0, "roughness": 0}),
    );
    let child = element(
        "rectangle",
        "frameChild",
        50.0,
        20.0,
        json!({"frameId": "frame"}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), child, other]), Seq(0));
    drag_into(&mut ed, &mut s, "frameChild", "otherFrame", &mut rng);
    assert_eq!(frame_of(&s, "frameChild"), Some("otherFrame"));
}

#[test]
fn dragged_in_above_the_top_child() {
    let child = element(
        "rectangle",
        "frameChild",
        10.0,
        20.0,
        json!({"frameId": "frame"}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame(), child, rect2()]), Seq(0));
    drag_into(&mut ed, &mut s, "rect2", "frame", &mut rng);
    assert_eq!(frame_of(&s, "rect2"), Some("frame"));
    assert_eq!(ids(&s), ["frame", "frameChild", "rect2"]);
    let index = |id: &str| s.element(id).unwrap().index().unwrap().to_string();
    assert!(index("rect2") > index("frameChild") && index("rect2") > index("frame"));
}

#[test]
fn a_frame_moves_with_its_children() {
    let child = element(
        "rectangle",
        "child",
        20.0,
        50.0,
        json!({"frameId": "frame"}),
    );
    let away = element("rectangle", "away", 400.0, 50.0, json!({}));
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![child, away, the_frame()]), Seq(0));
    // Taken by its name.
    click(&mut ed, &mut s, [10.0, -10.0], &mut rng);
    assert_eq!(ed.selection, ["frame"]);
    down_move_up(&mut ed, &mut s, [10.0, -10.0], [110.0, 90.0], &mut rng);
    let at = |s: &Scene, id: &str| (s.element(id).unwrap().x(), s.element(id).unwrap().y());
    assert_eq!(
        (at(&s, "frame"), at(&s, "child"), at(&s, "away")),
        ((100.0, 100.0), (120.0, 120.0), (400.0, 400.0))
    );
    assert_eq!(frame_of(&s, "child"), Some("frame"));
    // And with the arrow keys.
    let o = ed.nudge(&s, 5.0, 0.0, &mut rng);
    apply(&mut s, &o);
    assert_eq!((at(&s, "frame").0, at(&s, "child").0), (105.0, 125.0));
}

#[test]
fn renamed_by_its_name() {
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![the_frame()]), Seq(0));
    assert_eq!(title(s.element("frame").unwrap()), "Frame");
    // "Frame" at ten pixels a character: 50 wide, 17.5 high, 3 above.
    assert_eq!(
        name_bounds(s.element("frame").unwrap(), 1.0, &m),
        [0.0, -20.5, 50.0, 17.5]
    );
    let o = ed.double_click(&s, [40.0, -10.0], Mods::default(), 1.0, &mut rng);
    assert_eq!(o.edit_text.as_deref(), Some("frame"));
    let o = ed.finish_text(&s, "  Plan \n", &m, &mut rng);
    apply(&mut s, &o);
    assert_eq!(title(s.element("frame").unwrap()), "Plan");
    // Not past its name.
    assert!(
        ed.double_click(&s, [60.0, -10.0], Mods::default(), 1.0, &mut rng)
            .edit_text
            .is_none()
    );
    // Enter renames the selected frame; emptied, it is "Frame" again.
    ed.selection = vec!["frame".into()];
    assert_eq!(
        ed.enter(&s, false, &mut rng).edit_text.as_deref(),
        Some("frame")
    );
    let o = ed.finish_text(&s, " ", &m, &mut rng);
    apply(&mut s, &o);
    assert_eq!(s.element("frame").unwrap().get("name"), Some(&Value::Null));
    assert_eq!(title(s.element("frame").unwrap()), "Frame");
}

fn frame_with_child() -> Scene {
    let child = element(
        "rectangle",
        "child",
        20.0,
        50.0,
        json!({"frameId": "frame"}),
    );
    scene(vec![child, the_frame()])
}

#[test]
fn deleted_frames_leave_their_children_cut_ones_take_them() {
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.selection = vec!["frame".into()];
    let o = ed.delete_selected(&s, &mut rng);
    apply(&mut s, &o);
    assert_eq!(ids(&s), ["child"]);
    assert_eq!(frame_of(&s, "child"), None);
    assert_eq!(ed.selection, ["child"]);
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.selection = vec!["frame".into()];
    let clip = ed.copy(&s).unwrap();
    assert!(clip.contains("\"child\""));
    let o = ed.cut(&s, &mut rng);
    apply(&mut s, &o);
    assert!(ids(&s).is_empty());
    // The eraser takes them too.
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.set_tool(Tool::Eraser);
    click(&mut ed, &mut s, [0.0, 75.0], &mut rng);
    assert!(ids(&s).is_empty());
}

#[test]
fn duplicates_and_pastes() {
    // A frame duplicated with its child: the copy's child is in the copy.
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.selection = vec!["frame".into()];
    let o = ed.duplicate(&s, &mut rng);
    apply(&mut s, &o);
    let copy = s
        .visible()
        .find(|e| is_frame(e) && e.id() != "frame")
        .unwrap()
        .id()
        .to_string();
    let copied_child = s
        .visible()
        .find(|e| e.kind() == ElementType::Rectangle && e.id() != "child")
        .unwrap();
    assert_eq!(copied_child.frame_id(), Some(copy.as_str()));
    // A child duplicated alone stays in its frame.
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.selection = vec!["child".into()];
    let o = ed.duplicate(&s, &mut rng);
    apply(&mut s, &o);
    assert!(
        s.visible()
            .filter(|e| e.kind() == ElementType::Rectangle)
            .all(|e| e.frame_id() == Some("frame"))
    );
    // Pasted into a frame: it joins it; elsewhere it is in none.
    let (mut ed, mut s, mut rng) = (editor(), frame_with_child(), Seq(0));
    ed.selection = vec!["child".into()];
    let clip = ed.copy(&s).unwrap();
    let pasted = crate::file::read_drawing(&clip, &mut crate::restore::Ids::new(9))
        .unwrap()
        .scene
        .elements;
    let o = ed.paste(&s, &pasted, [500.0, 500.0], &mut rng);
    apply(&mut s, &o);
    assert_eq!(s.element(&ed.selection[0]).unwrap().frame_id(), None);
    let o = ed.paste(&s, &pasted, [75.0, 75.0], &mut rng);
    apply(&mut s, &o);
    assert_eq!(
        s.element(&ed.selection[0]).unwrap().frame_id(),
        Some("frame")
    );
}

#[test]
fn box_selecting_takes_what_shows_and_the_frame() {
    // A child reaching out of its frame is selected by what shows of it.
    let child = element(
        "rectangle",
        "child",
        100.0,
        100.0,
        json!({"frameId": "frame"}),
    );
    let (mut ed, mut s, mut rng) = (editor(), scene(vec![child, the_frame()]), Seq(0));
    // (Clear of the frame's name, which a pointer takes from a little off.)
    down_move_up(&mut ed, &mut s, [-15.0, 160.0], [160.0, -15.0], &mut rng);
    assert_eq!(ed.selection.len(), 2);
    // Outside its frame it isn't there to be clicked.
    assert!(hit::element_at(&s, [190.0, 190.0], 1.0, false).is_none());
    assert_eq!(
        hit::element_at(&s, [100.0, 120.0], 1.0, false).map(Element::id),
        Some("child")
    );
}

#[test]
fn exported_clipped_and_named() {
    let child = element(
        "rectangle",
        "child",
        100.0,
        100.0,
        json!({"frameId": "frame"}),
    );
    let s = scene(vec![child, the_frame()]);
    let out = svg::export(
        &s,
        &SvgOptions {
            background: false,
            measure: m,
            ..SvgOptions::default()
        },
    );
    // The name ("Frame": 50 wide) above the frame is in the picture.
    assert!(out.contains(r#"viewBox="0 0 170 190.5""#), "{out}");
    assert!(out.contains(r#"<clipPath id="frame"><rect transform="translate(10 30.5) rotate(0 75 75)" width="150" height="150" rx="8" ry="8"></rect></clipPath>"#));
    assert!(out.contains(
        r#"<g clip-path="url(#frame)"><g stroke-linecap="round" transform="translate(110 130.5)"#
    ));
    assert!(out.contains(r##"<g transform="translate(10 10) rotate(0 25 8.75)"><text x="0" y="12.5302734375" font-family="Helvetica, sans-serif, Segoe UI Emoji" font-size="14px" fill="#999999" text-anchor="start" style="white-space: pre;" direction="ltr" dominant-baseline="alphabetic">Frame</text></g>"##), "{out}");
    // Before the frame, whose children are clipped to it.
    assert!(out.find(">Frame</text>") < out.find(r#"width="150px""#));
    // In dark mode, the editor's dark name colour.
    assert_eq!(crate::color::dark(NAME_COLOR_UNDARK), NAME_COLOR_DARK);
    let dark = svg::export(
        &s,
        &SvgOptions {
            dark: true,
            measure: m,
            ..SvgOptions::default()
        },
    );
    assert!(dark.contains(&format!(r#"fill="{NAME_COLOR_DARK}""#)));
    // A name wider than its frame is cut short.
    let narrow = element("frame", "narrow", 0.0, 60.0, json!({"name": "A long name"}));
    let labels = name_labels(&[&narrow], false, &m);
    assert_eq!(
        (labels["narrow"].text(), labels["narrow"].width()),
        ("A l...", 60.0)
    );
}
