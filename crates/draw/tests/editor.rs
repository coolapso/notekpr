//! Property tests of the editor: random gestures and commands — drawing
//! with every tool, dragging (also with Shift, Alt and Ctrl), labels,
//! points, deleting, grouping, copying — leave a drawing that holds
//! together: numbers are numbers, ids are unique, stacking keys valid,
//! labels and attached arrows point at what is there and back.
//! More cases: `PROPTEST_CASES=5000 cargo test -p notekpr-draw --test editor`.

use std::collections::HashSet;

use notekpr_draw::edit::{Editor, Mods, Output, Random, Tool};
use notekpr_draw::order;
use notekpr_draw::scene::{ElementType, Scene};
use proptest::prelude::*;

struct Seq(u32, f64);

impl Random for Seq {
    fn id(&mut self) -> String {
        self.0 += 1;
        format!("id{:05}", self.0)
    }
    fn seed(&mut self) -> i32 {
        self.0 += 1;
        (self.0 as i32).wrapping_mul(7919)
    }
    fn now(&self) -> f64 {
        self.1
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

#[derive(Clone, Debug)]
enum Op {
    Tool(usize),
    Drag([i16; 4], u8),
    Click([i16; 2], u8),
    DoubleClick([i16; 2], bool),
    Text(String),
    Enter(bool),
    Escape,
    Delete,
    Duplicate,
    SelectAll,
    Group,
    Ungroup,
    Nudge(i8, i8),
    Copy([i16; 2]),
    Wait,
}

const TOOLS: [Tool; 11] = [
    Tool::Selection,
    Tool::Rectangle,
    Tool::Diamond,
    Tool::Ellipse,
    Tool::Arrow,
    Tool::Line,
    Tool::Freedraw,
    Tool::Text,
    Tool::Eraser,
    Tool::Frame,
    Tool::StickyNote,
];

fn point() -> impl Strategy<Value = [i16; 2]> {
    [-300i16..300, -300i16..300]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..TOOLS.len()).prop_map(Op::Tool),
        6 => ([-300i16..300, -300i16..300, -300i16..300, -300i16..300], 0u8..8).prop_map(|(p, m)| Op::Drag(p, m)),
        3 => (point(), 0u8..8).prop_map(|(p, m)| Op::Click(p, m)),
        1 => (point(), any::<bool>()).prop_map(|(p, c)| Op::DoubleClick(p, c)),
        1 => "[a-z ]{0,12}".prop_map(Op::Text),
        1 => any::<bool>().prop_map(Op::Enter),
        1 => Just(Op::Escape),
        1 => Just(Op::Delete),
        1 => Just(Op::Duplicate),
        1 => Just(Op::SelectAll),
        1 => Just(Op::Group),
        1 => Just(Op::Ungroup),
        1 => (-3i8..3, -3i8..3).prop_map(|(x, y)| Op::Nudge(x, y)),
        1 => point().prop_map(Op::Copy),
        1 => Just(Op::Wait),
    ]
}

fn mods(m: u8) -> Mods {
    Mods {
        shift: m & 1 != 0,
        alt: m & 2 != 0,
        ctrl: m & 4 != 0,
    }
}

fn p([x, y]: [i16; 2]) -> [f64; 2] {
    [x as f64, y as f64]
}

/// Ten pixels a character.
fn measure(_: &str, s: &str) -> f64 {
    s.chars().count() as f64 * 10.0
}

fn run(ed: &mut Editor, scene: &mut Scene, rng: &mut Seq, op: &Op) {
    let out = match op {
        Op::Tool(t) => {
            ed.set_tool(TOOLS[*t]);
            Output::default()
        }
        Op::Drag([a, b, c, d], m) => {
            let m = mods(*m);
            let o = ed.pointer_down(scene, p([*a, *b]), None, m, 1.0, rng);
            apply(scene, &o);
            let mid = [(*a as f64 + *c as f64) / 2.0, (*b as f64 + *d as f64) / 2.0];
            let o = ed.pointer_move(scene, mid, Some(0.5), m, 1.0, rng);
            apply(scene, &o);
            let o = ed.pointer_move(scene, p([*c, *d]), Some(0.5), m, 1.0, rng);
            apply(scene, &o);
            ed.pointer_up(scene, p([*c, *d]), rng)
        }
        Op::Click(at, m) => {
            let o = ed.pointer_down(scene, p(*at), None, mods(*m), 1.0, rng);
            apply(scene, &o);
            ed.pointer_up(scene, p(*at), rng)
        }
        Op::DoubleClick(at, ctrl) => ed.double_click(
            scene,
            p(*at),
            Mods {
                ctrl: *ctrl,
                ..Mods::default()
            },
            1.0,
            rng,
        ),
        Op::Text(t) => {
            if ed.editing_text.is_some() {
                ed.finish_text(scene, t, &measure, rng)
            } else {
                Output::default()
            }
        }
        Op::Enter(ctrl) => ed.enter(scene, *ctrl, rng),
        Op::Escape => ed.escape(scene, rng),
        Op::Delete => ed.delete_selected(scene, rng),
        Op::Duplicate => ed.duplicate(scene, rng),
        Op::SelectAll => {
            ed.select_all(scene);
            Output::default()
        }
        Op::Group => ed.group(scene, rng),
        Op::Ungroup => ed.ungroup(scene, rng),
        Op::Nudge(x, y) => ed.nudge(scene, *x as f64, *y as f64, rng),
        Op::Copy(at) => match ed.copy(scene) {
            Some(text) => {
                let mut ids = notekpr_draw::restore::Ids::new(rng.0 as u64);
                let d = notekpr_draw::file::read_drawing(&text, &mut ids).unwrap();
                ed.paste(scene, &d.scene.elements, p(*at), rng)
            }
            None => Output::default(),
        },
        Op::Wait => {
            rng.1 += 800.0;
            Output::default()
        }
    };
    apply(scene, &out);
}

/// What a drawing must keep, whatever was done to it.
fn holds_together(scene: &Scene) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut keys = HashSet::new();
    for e in &scene.elements {
        if !ids.insert(e.id().to_string()) {
            return Err(format!("{} twice", e.id()));
        }
        if let Some(k) = e.index() {
            if !order::is_valid(k) {
                return Err(format!("{}: index {k:?}", e.id()));
            }
            if !e.is_deleted() && !keys.insert(k.to_string()) {
                return Err(format!("{}: index {k} taken", e.id()));
            }
        }
    }
    for e in scene.visible() {
        let nums = [e.x(), e.y(), e.width(), e.height(), e.angle()];
        if nums.iter().any(|n| !n.is_finite()) {
            return Err(format!("{}: {nums:?}", e.id()));
        }
        if e.width() < 0.0 || e.height() < 0.0 {
            return Err(format!("{}: size {} × {}", e.id(), e.width(), e.height()));
        }
        if e.points().iter().flatten().any(|n| !n.is_finite()) {
            return Err(format!("{}: points", e.id()));
        }
        // A label's container is there and lists it.
        if let Some(c) = e.container_id() {
            let Some(c) = scene.element(c).filter(|c| !c.is_deleted()) else {
                return Err(format!("{}: label of a container not there", e.id()));
            };
            if !c.bound_elements().iter().any(|b| b.id == e.id()) {
                return Err(format!("{}: {} doesn't list its label", e.id(), c.id()));
            }
        }
        // An arrow's ends are attached to what is there, which lists it.
        if e.kind() == ElementType::Arrow {
            for b in [e.start_binding(), e.end_binding()].into_iter().flatten() {
                let Some(t) = scene.element(&b.element_id).filter(|t| !t.is_deleted()) else {
                    return Err(format!(
                        "{}: attached to {} not there",
                        e.id(),
                        b.element_id
                    ));
                };
                if !t.bound_elements().iter().any(|x| x.id == e.id()) {
                    return Err(format!("{}: {} doesn't list it", e.id(), t.id()));
                }
            }
        }
        if let Some(f) = e.frame_id()
            && scene
                .element(f)
                .filter(|f| !f.is_deleted() && f.kind() == ElementType::Frame)
                .is_none()
        {
            return Err(format!("{}: in frame {f} not there", e.id()));
        }
    }
    Ok(())
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn editing_keeps_a_drawing_whole(ops in prop::collection::vec(op(), 1..40)) {
        let (mut ed, mut scene, mut rng) = (Editor::default(), Scene::default(), Seq(0, 1_800_000_000_000.0));
        for (i, o) in ops.iter().enumerate() {
            run(&mut ed, &mut scene, &mut rng, o);
            if let Err(why) = holds_together(&scene) {
                prop_assert!(false, "after step {i} ({o:?}): {why}");
            }
        }
    }
}
