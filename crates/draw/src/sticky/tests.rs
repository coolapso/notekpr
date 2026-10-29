//! Excalidraw's own tests of sticky notes (`stickyNote.test.ts`), with its
//! test measure (ten pixels a character), so the numbers are its numbers.

use serde_json::json;

use super::*;

/// Excalidraw's test canvas: ten pixels a character.
fn m(_: &str, s: &str) -> f64 {
    s.chars().count() as f64 * 10.0
}

fn el(v: Value) -> Element {
    Element::from_props(v.as_object().unwrap().clone()).unwrap()
}

const STICKY_FONT_SIZE: f64 = 28.0;

/// `createStickyWithText`: a 250 note at 100,100, its label in 28px,
/// centred, laid out.
fn note_with(text: &str) -> (Element, Element) {
    let note = el(json!({
        "id": "n", "type": "stickynote", "x": 100, "y": 100, "width": 250, "height": 250,
        "baseHeight": 250, "seed": 1, "roughness": 1, "angle": 0,
        "boundElements": [{"id": "t", "type": "text"}]
    }));
    let label = el(json!({
        "id": "t", "type": "text", "x": 225, "y": 225, "text": text, "originalText": text,
        "fontSize": 28, "baseFontSize": 28, "fontFamily": 5, "lineHeight": 1.25,
        "textAlign": "center", "verticalAlign": "middle", "containerId": "n"
    }));
    let (n, t) = layout(&note, Some(&label), &Intent::default(), &m);
    (n, t.unwrap())
}

fn overflowing() -> String {
    vec!["abcdefghijklmnopqrstuvwx"; 40].join("\n")
}

/// `resizeSingleElement` by a handle dragged to `to`.
fn resize(
    note: &Element,
    label: &Element,
    handle: Handle,
    to: Point,
    keep_ratio: bool,
) -> (Element, Element) {
    let f = transform::frame(&[note]).unwrap();
    let mode = ResizeMode {
        keep_ratio,
        from_center: false,
    };
    let out = resize_alone(&[note.clone(), label.clone()], &f, handle, to, mode, &m).unwrap();
    (out[0].clone(), out[1].clone())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn downscales_the_font_to_fit_before_growing() {
    let (n, t) = note_with(&["abcdefghij"; 7].join("\n"));
    assert_eq!(
        (n.width(), n.height(), n.f64("baseHeight")),
        (250.0, 250.0, Some(250.0))
    );
    assert!(t.x() - n.x() >= PADDING && t.y() - n.y() >= PADDING);
    // Seven lines in the 198 above the footer: 22 is the largest on the
    // grid down from 28 that fits.
    assert_eq!((t.font_size(), base_font_size(&t)), (22.0, 28.0));
}

#[test]
fn grows_down_at_the_smallest_font_and_shrinks_back() {
    let (n, t) = note_with(&overflowing());
    assert_eq!(t.font_size(), MIN_FONT_SIZE);
    // 24 characters don't fit in 218: each line is cut in two.
    assert_eq!(t.text().lines().count(), 80);
    assert_eq!((n.height(), n.y()), (80.0 * 20.0 + BODY_INSET_Y, 100.0));
    let mut short = t.clone();
    short.set("originalText", json!("short"));
    let (n, t) = layout(&n, Some(&short), &Intent::default(), &m);
    let t = t.unwrap();
    assert_eq!((n.height(), n.y()), (250.0, 100.0));
    assert_eq!(t.font_size(), STICKY_FONT_SIZE);
}

#[test]
fn user_font_size_below_the_minimum_is_kept() {
    let (n, mut t) = note_with("short");
    t.set("fontSize", json!(15));
    t.set("baseFontSize", json!(15));
    let (_, t) = layout(&n, Some(&t), &Intent::default(), &m);
    assert_eq!(t.unwrap().font_size(), 15.0);
}

#[test]
fn font_ceilings_are_numbers_in_range() {
    assert_eq!(normalize_font_size(f64::NAN), FALLBACK_FONT_SIZE);
    assert_eq!(normalize_font_size(f64::INFINITY), FALLBACK_FONT_SIZE);
    assert_eq!(normalize_font_size(f64::NEG_INFINITY), FALLBACK_FONT_SIZE);
    assert_eq!(normalize_font_size(1e20), MAX_FONT_SIZE);
    assert_eq!(normalize_font_size(0.0), 1.0);
    assert_eq!(normalize_font_size(24.0), 24.0);
    // A huge ceiling still ends.
    let (n, t) = note_with("some text that must wrap");
    for ceiling in [1e20, f64::INFINITY] {
        let intent = Intent {
            base_font_size: Some(ceiling),
            ..Intent::default()
        };
        let (n, t) = layout(&n, Some(&t), &intent, &m);
        let t = t.unwrap();
        assert!(t.font_size().is_finite() && t.font_size() <= MAX_FONT_SIZE);
        assert!(n.height().is_finite());
    }
}

#[test]
fn lowered_odd_and_fractional_ceilings() {
    let (n, t) = note_with("short");
    let at = |size: f64, text: Option<&str>| {
        let mut t = t.clone();
        if let Some(s) = text {
            t.set("originalText", json!(s));
        }
        let intent = Intent {
            base_font_size: Some(size),
            ..Intent::default()
        };
        layout(&n, Some(&t), &intent, &m).1.unwrap()
    };
    assert_eq!(
        (at(20.0, None).font_size(), base_font_size(&at(20.0, None))),
        (20.0, 20.0)
    );
    assert_eq!(at(27.0, None).font_size(), 27.0);
    assert_eq!(at(27.5, None).font_size(), 27.5);
    let fitted = at(27.0, Some(&["abcdefghij"; 7].join("\n"))).font_size();
    assert!(fitted < 27.0);
    assert!(fitted == MIN_FONT_SIZE || (27.0 - fitted) % 2.0 == 0.0);
}

#[test]
fn north_resize_pinned_by_its_text_keeps_the_bottom() {
    let (n, t) = note_with(&overflowing());
    let bottom = n.y() + n.height();
    let (r, _) = resize(&n, &t, Handle::N, [225.0, n.y() + 100.0], false);
    assert!(close(r.height(), n.height()) && close(r.y(), n.y()));
    assert!(close(r.y() + r.height(), bottom));
    assert_eq!(r.f64("baseHeight"), Some(n.height() - 100.0));
}

#[test]
fn corner_resize_pinned_by_its_text_remembers_the_asked_height() {
    let (n, t) = note_with(&overflowing());
    let (r, rt) = resize(&n, &t, Handle::SE, [220.0, 350.0], false);
    assert_eq!(r.width(), 120.0);
    // Its text keeps it taller, but delete the text and it is 250 again.
    assert_eq!(r.f64("baseHeight"), Some(250.0));
    let mut asked = n.clone();
    asked.set("width", json!(120));
    asked.set("baseHeight", json!(250));
    let (expected, _) = layout(&asked, Some(&t), &Intent::default(), &m);
    assert!(close(r.height(), expected.height()) && r.height() > 250.0);
    assert_eq!(r.y(), 100.0);
    assert_eq!((rt.font_size(), base_font_size(&rt)), (t.font_size(), 28.0));
}

#[test]
fn proportional_resize_scales_note_base_and_font() {
    let (n, t) = note_with("short");
    let (r, rt) = resize(&n, &t, Handle::E, [600.0, 225.0], true);
    assert_eq!(
        (r.width(), r.height(), r.f64("baseHeight")),
        (500.0, 500.0, Some(500.0))
    );
    assert_eq!((base_font_size(&rt), rt.font_size()), (56.0, 56.0));
    // Shift let go in the same gesture: from where it began again.
    let (r, rt) = resize(&n, &t, Handle::SE, [500.0, 400.0], false);
    assert_eq!((r.width(), r.f64("baseHeight")), (400.0, Some(300.0)));
    assert_eq!((base_font_size(&rt), rt.font_size()), (28.0, 28.0));
    let (r, _) = resize(&n, &t, Handle::E, [450.0, 225.0], false);
    assert_eq!(r.f64("baseHeight"), Some(250.0));
}

#[test]
fn width_only_resize_keeps_the_base_height() {
    let (n, t) = note_with(&overflowing());
    assert!(n.height() > 250.0);
    let (r, _) = resize(&n, &t, Handle::E, [500.0, 300.0], false);
    assert_eq!(
        (r.width(), r.f64("baseHeight"), r.y()),
        (400.0, Some(250.0), 100.0)
    );
    assert!(r.height() > 250.0);
}

#[test]
fn smallest_notes() {
    // A 25px line: the floor wins across, the footer adds down.
    assert_eq!(
        min_size(20.0, fonts::EXCALIFONT),
        (MIN_SIZE, 25.0 + BODY_INSET_Y)
    );
    // A 60px line.
    assert_eq!(
        min_size(48.0, fonts::EXCALIFONT),
        (60.0 + PADDING * 2.0, 60.0 + BODY_INSET_Y)
    );
    // Never resized below one line at its ceiling and the footer.
    let (n, t) = note_with("A");
    let (r, _) = resize(&n, &t, Handle::SE, [140.0, 140.0], false);
    assert_eq!(
        (r.width(), r.f64("baseHeight"), r.height()),
        (75.0, Some(87.0), 87.0)
    );
    // Kept square when shrunk proportionally below it.
    let (r, _) = resize(&n, &t, Handle::SE, [160.0, 160.0], true);
    assert!(close(r.width(), r.height()) && r.height() >= 87.0);
}

#[test]
fn flipped_past_the_far_side() {
    let (n, t) = note_with("A");
    // The corner dragged up past the top: mirrored over the top edge.
    let (r, rt) = resize(&n, &t, Handle::SE, [500.0, -300.0], true);
    assert_eq!(
        (r.width(), r.height(), r.x(), r.y()),
        (400.0, 400.0, 100.0, -300.0)
    );
    assert!(close(
        base_font_size(&rt),
        STICKY_FONT_SIZE * (400.0 / 250.0)
    ));
    assert_eq!(rt.angle(), 0.0);
    assert!(rt.y() > r.y() && rt.y() + rt.height() < r.y() + r.height());
    // The east edge dragged 300 past the west one.
    let (r, _) = resize(&n, &t, Handle::E, [-200.0, 225.0], false);
    assert_eq!((r.width(), r.height(), r.x()), (300.0, 250.0, -200.0));
    // Just past it: the smallest width, mirrored.
    let (r, _) = resize(&n, &t, Handle::E, [90.0, 225.0], false);
    assert_eq!((r.width(), r.x()), (MIN_SIZE, 100.0 - MIN_SIZE));
}

#[test]
fn turned_note_grows_from_its_top_edge() {
    let (mut n, mut t) = note_with("short");
    n.set("angle", json!(0.6));
    t.set("angle", json!(0.6));
    let top = |e: &Element| {
        geom::rotate(
            [e.x() + e.width() / 2.0, e.y()],
            [e.x() + e.width() / 2.0, e.y() + e.height() / 2.0],
            e.angle(),
        )
    };
    t.set("originalText", json!(overflowing()));
    let (grown, _) = layout(&n, Some(&t), &Intent::default(), &m);
    assert!(grown.height() > n.height());
    let (a, b) = (top(&n), top(&grown));
    assert!(close(a[0], b[0]) && close(a[1], b[1]), "{a:?} {b:?}");
}

#[test]
fn middle_label_centred_in_the_whole_note_at_any_angle() {
    for angle in [
        0.0,
        std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_2,
    ] {
        let (mut n, t) = note_with("Balanced");
        n.set("angle", json!(angle));
        let (n, t) = layout(&n, Some(&t), &Intent::default(), &m);
        let t = t.unwrap();
        let c = [n.x() + n.width() / 2.0, n.y() + n.height() / 2.0];
        let [x, y] = geom::rotate(
            [t.x() + t.width() / 2.0, t.y() + t.height() / 2.0],
            c,
            -angle,
        );
        assert!(close(x, c[0]) && close(y, c[1]), "{angle}");
    }
}

#[test]
fn bottom_label_above_the_footer_at_any_angle() {
    for angle in [
        0.0,
        std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_2,
    ] {
        let (mut n, mut t) = note_with("Last line");
        n.set("angle", json!(angle));
        t.set("verticalAlign", json!("bottom"));
        let (n, t) = layout(&n, Some(&t), &Intent::default(), &m);
        let t = t.unwrap();
        let c = [n.x() + n.width() / 2.0, n.y() + n.height() / 2.0];
        let [_, y] = geom::rotate(
            [t.x() + t.width() / 2.0, t.y() + t.height() / 2.0],
            c,
            -angle,
        );
        assert!(close(
            y + t.height() / 2.0,
            n.y() + n.height() - PADDING - FOOTER_HEIGHT
        ));
        assert_eq!((n.f64("baseHeight"), n.height()), (Some(250.0), 250.0));
    }
}

#[test]
fn middle_label_pushed_up_only_once_it_reaches_the_footer() {
    let (n, t) = note_with("Tall");
    let padded = n.height() - PADDING * 2.0;
    let body_bottom = n.y() + n.height() - PADDING - FOOTER_HEIGHT;
    let mut short = t.clone();
    short.set("height", json!(100));
    assert!(close(
        label_position(&n, &short)[1],
        n.y() + PADDING + (padded - 100.0) / 2.0
    ));
    let mut tall = t.clone();
    let h = padded - FOOTER_HEIGHT;
    tall.set("height", json!(h));
    assert!(close(label_position(&n, &tall)[1], body_bottom - h));
    // The label's room ends above the footer.
    assert_eq!(label::max_height(&n, 0.0), n.height() - BODY_INSET_Y);
}

const NOW: f64 = 1_788_868_800_000.0; // 8 Sep 2026, noon (UTC)

fn utc() -> LocalTime {
    LocalTime {
        now: NOW,
        utc_offset: |_| 0.0,
    }
}

#[test]
fn dates_short_while_the_year_is_this_one() {
    let t = utc();
    assert_eq!(
        date_label(Some(1_788_782_400_000.0), false, &t).as_deref(),
        Some("7 Sep")
    );
    assert_eq!(
        date_label(Some(1_767_182_400_000.0), false, &t).as_deref(),
        Some("31 Dec 2025")
    );
    assert_eq!(
        date_label(Some(1_767_182_400_000.0), true, &t).as_deref(),
        Some("31 Dec")
    );
    // A creator's clock ahead of ours is still a date.
    assert_eq!(
        date_label(Some(1_798_804_800_000.0), false, &t).as_deref(),
        Some("1 Jan 2027")
    );
    assert_eq!(
        date_label(Some(-43_200_000.0), false, &t).as_deref(),
        Some("31 Dec 1969")
    );
    for created in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(8.64e15 + 1.0),
    ] {
        assert_eq!(date_label(created, false, &t), None);
    }
    // In the viewer's time zone: half past eleven at night in UTC is the
    // next day an hour east.
    let east = LocalTime {
        now: NOW,
        utc_offset: |_| 60.0,
    };
    assert_eq!(
        date_label(Some(1_767_223_800_000.0), false, &east).as_deref(),
        Some("1 Jan")
    );
}

#[test]
fn footer_by_width_without_measuring() {
    let footer_at = |w: f64, h: f64, created: Value| {
        let e = el(
            json!({"id": "n", "type": "stickynote", "width": w, "height": h, "created": created}),
        );
        footer(&e, &utc())
    };
    let created = json!(1_748_606_400_000.0_f64); // 30 May 2025
    assert_eq!(
        footer_at(250.0, 250.0, created.clone()),
        Some(Footer {
            text: "30 May 2025".into(),
            x: 250.0 - PADDING,
            y: 250.0 - FOOTER_BASELINE
        })
    );
    let year = PADDING * 2.0 + FOOTER_YEAR_WIDTH;
    assert_eq!(
        footer_at(year, 250.0, created.clone()).unwrap().text,
        "30 May 2025"
    );
    assert_eq!(
        footer_at(year - 1.0, 250.0, created.clone()).unwrap().text,
        "30 May"
    );
    assert_eq!(
        footer_at(MIN_SIZE, MIN_SIZE, created.clone()).unwrap().text,
        "30 May"
    );
    // None on the note being drawn, nor without a date.
    assert_eq!(footer_at(0.0, 0.0, created), None);
    assert_eq!(footer_at(250.0, 250.0, Value::Null), None);
}

#[test]
fn paper_shapes() {
    let plain = el(
        json!({"id": "n", "type": "stickynote", "width": 250, "height": 250, "roughness": 0, "seed": 7}),
    );
    // An architect's square paper: four straight sides.
    assert_eq!(
        path_data(&path_commands(&plain, false)),
        "M 0 0 L 250 0 L 250 250 L 0 250 Z"
    );
    // Its shadow a little down and right.
    assert_eq!(
        path_data(&path_commands(&plain, true)),
        "M 3 3 L 253 3 L 253 253 L 3 253 Z"
    );
    // Rounded: smaller corners than a rectangle's, a curve at each.
    let mut round = plain.clone();
    round.set("roundness", json!({"type": 2}));
    assert_eq!(corner_radius(&round), 10.0);
    let c = path_commands(&round, false);
    assert_eq!(c[0], PathCommand::Move([10.0, 0.0]));
    assert_eq!(
        c.iter()
            .filter(|c| matches!(c, PathCommand::Quadratic { .. }))
            .count(),
        4
    );
    // An artist's corners wobble, the same for the same seed.
    let mut artist = plain.clone();
    artist.set("roughness", json!(1));
    let a = path_commands(&artist, false);
    assert_eq!(a, path_commands(&artist, false));
    assert_ne!(a, path_commands(&plain, false));
    for p in a.iter().map(PathCommand::end) {
        assert!(
            p.iter()
                .all(|v| (v.abs() <= 1.5) || ((v - 250.0).abs() <= 1.5)),
            "{p:?}"
        );
    }
    // A cartoonist's has one corner lifted: three curves there, one at
    // each of the others.
    let mut cartoon = plain.clone();
    cartoon.set("roughness", json!(2));
    let c = path_commands(&cartoon, false);
    assert_eq!(
        c.iter()
            .filter(|c| matches!(c, PathCommand::Quadratic { .. }))
            .count(),
        6
    );
    assert_eq!(
        c.iter()
            .filter(|c| matches!(c, PathCommand::Line(_)))
            .count(),
        4
    );
}

#[test]
fn seeded_random_is_mulberry32() {
    // mulberry32(1)'s first numbers, as JavaScript computes them.
    let mut r = SeededRandom::new(1.0);
    let first: Vec<u32> = (0..3)
        .map(|_| (r.next() * 4_294_967_296.0) as u32)
        .collect();
    assert_eq!(first, [2_693_262_067, 11_749_833, 2_265_367_787]);
}

#[test]
fn notes_in_svg() {
    let (mut n, _) = note_with("hi");
    n.set("created", json!(1_788_782_400_000.0_f64));
    n.set("backgroundColor", json!("#ffdf6b"));
    let (clip, g) = svg(&n, "translate(1 2) rotate(0 125 125)", 0.5, false, &utc());
    assert!(clip.starts_with(
        r#"<clipPath id="sticky-note-clipPath-n" clipPathUnits="userSpaceOnUse"><path d="M "#
    ));
    assert!(
        g.starts_with(r#"<g transform="translate(1 2) rotate(0 125 125)" opacity="0.5"><path d=""#)
    );
    assert!(g.contains(r##"fill="#000" fill-opacity="0.16" stroke="none""##));
    assert!(g.contains(r##"fill="#ffdf6b" stroke="none""##));
    assert!(g.contains(r##"fill="none" stroke="#000" stroke-opacity="0.08" stroke-width="1" clip-path="url(#sticky-note-clipPath-n)""##));
    assert!(g.ends_with(r##"<text x="234" y="236" font-family="Helvetica, Arial, sans-serif" font-size="12px" text-anchor="end" direction="ltr" fill="#1e1e1e" fill-opacity="1">7 Sep</text></g>"##));
    let (_, dark) = svg(&n, "", 1.0, true, &utc());
    assert!(dark.contains(&format!(
        r#"fill="{}" stroke="none""#,
        color::dark("#ffdf6b")
    )));
    assert!(dark.starts_with(r#"<g transform=""><path"#));
}

/// Notes in the editor: drawing, typing, styling, resizing.
mod editing {
    use super::*;
    use crate::edit::{Editor, Mods, Output, Random, Tool};

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
            NOW
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

    fn gesture(
        ed: &mut Editor,
        scene: &mut Scene,
        from: Point,
        to: Point,
        mods: Mods,
        rng: &mut Seq,
    ) -> Output {
        let o = ed.pointer_down(scene, from, None, mods, 1.0, rng);
        apply(scene, &o);
        let o = ed.pointer_move(scene, to, None, mods, 1.0, rng);
        apply(scene, &o);
        let o = ed.pointer_up(scene, to, rng);
        apply(scene, &o);
        o
    }

    fn editor() -> Editor {
        let mut ed = Editor::default();
        ed.measure = m;
        ed
    }

    fn notes(scene: &Scene) -> Vec<&Element> {
        scene.visible().filter(|e| is_note(e)).collect()
    }

    #[test]
    fn a_click_places_a_note_and_its_text_is_typed_at_once() {
        let (mut ed, mut scene, mut rng) = (editor(), Scene::default(), Seq(0));
        assert_eq!(Tool::from_key("n"), Some(Tool::StickyNote));
        ed.set_tool(Tool::StickyNote);
        let o = gesture(
            &mut ed,
            &mut scene,
            [500.0, 500.0],
            [502.0, 501.0],
            Mods::default(),
            &mut rng,
        );
        let n = notes(&scene)[0].clone();
        assert_eq!(
            (n.x(), n.y(), n.width(), n.height()),
            (375.0, 375.0, 250.0, 250.0)
        );
        assert_eq!(n.f64("baseHeight"), Some(250.0));
        assert_eq!(
            (n.background_color(), n.fill_style()),
            (BACKGROUND, "solid")
        );
        assert_eq!(ed.tool, Tool::Selection);
        let label = o.edit_text.expect("its text is edited");
        let o = ed.finish_text(&scene, "Buy milk", &m, &mut rng);
        apply(&mut scene, &o);
        let t = scene.element(&label).unwrap();
        assert_eq!(t.container_id(), Some(n.id()));
        assert_eq!(
            (t.font_size(), base_font_size(t), t.width()),
            (20.0, 20.0, 80.0)
        );
        // Centred in the whole note.
        assert_eq!(t.x() + t.width() / 2.0, 500.0);
        assert_eq!(t.y() + t.height() / 2.0, 500.0);
    }

    #[test]
    fn a_drag_draws_a_square_note_shift_a_free_one_never_too_small() {
        let (mut ed, mut scene, mut rng) = (editor(), Scene::default(), Seq(0));
        ed.tool_locked = true;
        ed.set_tool(Tool::StickyNote);
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        gesture(
            &mut ed,
            &mut scene,
            [0.0, 0.0],
            [300.0, 120.0],
            Mods::default(),
            &mut rng,
        );
        gesture(
            &mut ed,
            &mut scene,
            [0.0, 500.0],
            [300.0, 620.0],
            shift,
            &mut rng,
        );
        // Dragged up and left, smaller than a note can be.
        gesture(
            &mut ed,
            &mut scene,
            [1000.0, 1000.0],
            [970.0, 980.0],
            Mods::default(),
            &mut rng,
        );
        let sizes: Vec<_> = notes(&scene)
            .iter()
            .map(|n| (n.x(), n.y(), n.width(), n.height()))
            .collect();
        assert_eq!(
            sizes,
            [
                (0.0, 0.0, 300.0, 300.0),
                (0.0, 500.0, 300.0, 120.0),
                // One 20px line (25 high) and the footer: 77.
                (923.0, 923.0, 77.0, 77.0)
            ]
        );
        // The tool stays: no text typed, nothing selected.
        assert_eq!(ed.tool, Tool::StickyNote);
        assert!(ed.selection.is_empty() && ed.editing_text.is_none());
    }

    #[test]
    fn notes_have_their_own_colours_never_transparent() {
        let (mut ed, mut scene, mut rng) = (editor(), Scene::default(), Seq(0));
        ed.set_tool(Tool::StickyNote);
        let o = ed.set_property(&scene, "backgroundColor", json!("#ffc9c9"), &mut rng);
        assert!(o.changed.is_empty());
        assert_eq!(
            (
                ed.style.sticky_background_color.as_str(),
                ed.style.background_color.as_str()
            ),
            ("#ffc9c9", "transparent")
        );
        gesture(
            &mut ed,
            &mut scene,
            [0.0, 0.0],
            [0.0, 0.0],
            Mods::default(),
            &mut rng,
        );
        let o = ed.finish_text(&scene, "hello", &m, &mut rng);
        apply(&mut scene, &o);
        let n = notes(&scene)[0].id().to_string();
        assert_eq!(scene.element(&n).unwrap().background_color(), "#ffc9c9");
        ed.selection = vec![n.clone()];
        // Transparent paper isn't a note's: its default instead.
        let o = ed.set_property(&scene, "backgroundColor", json!("transparent"), &mut rng);
        apply(&mut scene, &o);
        assert_eq!(scene.element(&n).unwrap().background_color(), BACKGROUND);
        let o = ed.set_property(&scene, "strokeColor", json!("#e03131"), &mut rng);
        apply(&mut scene, &o);
        let note = scene.element(&n).unwrap();
        let label = label_of(&scene, note).unwrap();
        // The note's ink is its text's.
        assert_eq!(
            (note.stroke_color(), label.stroke_color()),
            ("#e03131", "#e03131")
        );
        assert_eq!(
            (
                ed.style.sticky_stroke_color.as_str(),
                ed.style.stroke_color.as_str()
            ),
            ("#e03131", INK)
        );
        let o = ed.set_property(&scene, "strokeColor", json!("transparent"), &mut rng);
        apply(&mut scene, &o);
        let note = scene.element(&n).unwrap();
        let label = label_of(&scene, note).unwrap();
        assert_eq!((note.stroke_color(), label.stroke_color()), (INK, INK));
    }

    #[test]
    fn a_font_size_is_its_labels_ceiling() {
        let (mut ed, mut scene, mut rng) = (editor(), Scene::default(), Seq(0));
        ed.set_tool(Tool::StickyNote);
        gesture(
            &mut ed,
            &mut scene,
            [0.0, 0.0],
            [0.0, 0.0],
            Mods::default(),
            &mut rng,
        );
        let text = ["abcdefghij"; 7].join("\n");
        let o = ed.finish_text(&scene, &text, &m, &mut rng);
        apply(&mut scene, &o);
        let n = notes(&scene)[0].id().to_string();
        ed.selection = vec![n.clone()];
        let o = ed.set_property(&scene, "fontSize", json!(36), &mut rng);
        apply(&mut scene, &o);
        let note = scene.element(&n).unwrap();
        let t = label_of(&scene, note).unwrap();
        // Seven lines fit in 198 at 22: shrunk from the 36 asked.
        assert_eq!((base_font_size(t), t.font_size()), (36.0, 22.0));
        assert_eq!(note.height(), 250.0);
    }

    #[test]
    fn handles_resize_a_note_and_its_corners_keep_its_shape() {
        let (mut ed, mut scene, mut rng) = (editor(), Scene::default(), Seq(0));
        ed.set_tool(Tool::StickyNote);
        gesture(
            &mut ed,
            &mut scene,
            [125.0, 125.0],
            [125.0, 125.0],
            Mods::default(),
            &mut rng,
        );
        let o = ed.finish_text(&scene, "short", &m, &mut rng);
        apply(&mut scene, &o);
        let n = notes(&scene)[0].id().to_string();
        ed.selection = vec![n.clone()];
        // The south-east corner (just off it, as a handle is).
        gesture(
            &mut ed,
            &mut scene,
            [258.0, 258.0],
            [508.0, 308.0],
            Mods::default(),
            &mut rng,
        );
        let note = scene.element(&n).unwrap();
        assert_eq!(
            (note.x(), note.y(), note.width(), note.height()),
            (0.0, 0.0, 500.0, 500.0)
        );
        assert_eq!(base_font_size(label_of(&scene, note).unwrap()), 40.0);
        // Shift frees it.
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        gesture(
            &mut ed,
            &mut scene,
            [508.0, 508.0],
            [308.0, 158.0],
            shift,
            &mut rng,
        );
        let note = scene.element(&n).unwrap();
        assert_eq!(
            (note.width(), note.height(), note.f64("baseHeight")),
            (300.0, 150.0, Some(150.0))
        );
        let t = label_of(&scene, note).unwrap();
        assert_eq!(base_font_size(t), 40.0);
        assert!(t.y() > note.y() && t.y() + t.height() < note.y() + note.height());
    }
}
