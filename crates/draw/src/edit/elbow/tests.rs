use serde_json::json;

use super::*;

fn el(v: Value) -> Element {
    Element::from_props(v.as_object().unwrap().clone()).unwrap()
}

/// A 64×64 box (its binding gap is 6, a fraction of 0.09375: exact).
fn square(id: &str, x: f64, y: f64) -> Element {
    el(
        json!({"id": id, "type": "rectangle", "x": x, "y": y, "width": 64, "height": 64,
        "strokeWidth": 2, "angle": 0}),
    )
}

fn arrow(points: Value) -> Element {
    el(
        json!({"id": "arrow", "type": "arrow", "x": 0, "y": 0, "width": 0, "height": 0,
        "points": points, "elbowed": true, "fixedSegments": null,
        "startArrowhead": null, "endArrowhead": "arrow",
        "startBinding": null, "endBinding": null}),
    )
}

/// An elbow arrow from `a` to `b` at fixed points (fractions of their
/// boxes), routed.
fn connect(a: &Element, fa: [f64; 2], b: &Element, fb: [f64; 2]) -> (Element, Scene) {
    let start = fixed_global(a, fa);
    let end = fixed_global(b, fb);
    let mut e = arrow(json!([[0, 0], [end[0] - start[0], end[1] - start[1]]]));
    e.set("x", json!(start[0]));
    e.set("y", json!(start[1]));
    e.set(
        "startBinding",
        json!({"elementId": a.id(), "fixedPoint": fa, "mode": "orbit"}),
    );
    e.set(
        "endBinding",
        json!({"elementId": b.id(), "fixedPoint": fb, "mode": "orbit"}),
    );
    let scene = Scene {
        elements: vec![a.clone(), b.clone(), e.clone()],
        ..Scene::default()
    };
    let routed = route(&e, &scene, &[]);
    (routed, scene)
}

fn global(e: &Element) -> Vec<Point> {
    global_points(e)
}

/// Excalidraw's own test ("can properly generate orthogonal arrow
/// points"): an arrow attached to nothing, two turns halfway.
#[test]
fn free_arrows_turn_halfway() {
    let a = arrow(json!([[0, 0], [100, 100]]));
    let changes = Changes {
        points: Some(vec![[-45.0, -100.1], [45.0, 99.9]]),
        ..Changes::default()
    };
    for scene in [
        Scene::default(),
        Scene {
            elements: vec![a.clone()],
            ..Scene::default()
        },
    ] {
        let r = update(&a, &scene, &changes, false);
        assert_eq!(
            r.points(),
            vec![[0.0, 0.0], [0.0, 100.0], [90.0, 100.0], [90.0, 200.0]]
        );
        assert_eq!(
            (r.x(), r.y(), r.width(), r.height()),
            (-45.0, -100.1, 90.0, 200.0)
        );
        assert_eq!(r.get("fixedSegments"), Some(&Value::Null));
    }
}

#[test]
fn boxes_in_line_are_joined_straight_in_all_four_directions() {
    let a = square("a", 0.0, 0.0);
    // Right of it, left of it: one level segment between facing sides.
    let (r, _) = connect(
        &a,
        [1.09375, 0.25],
        &square("b", 192.0, 0.0),
        [-0.09375, 0.25],
    );
    assert_eq!(global(&r), vec![[70.0, 16.0], [186.0, 16.0]]);
    let (r, _) = connect(
        &a,
        [-0.09375, 0.25],
        &square("b", -192.0, 0.0),
        [1.09375, 0.25],
    );
    assert_eq!(global(&r), vec![[-6.0, 16.0], [-122.0, 16.0]]);
    // Below it, above it: one upright segment.
    let (r, _) = connect(
        &a,
        [0.75, 1.09375],
        &square("b", 0.0, 192.0),
        [0.75, -0.09375],
    );
    assert_eq!(global(&r), vec![[48.0, 70.0], [48.0, 186.0]]);
    let (r, _) = connect(
        &a,
        [0.75, -0.09375],
        &square("b", 0.0, -192.0),
        [0.75, 1.09375],
    );
    assert_eq!(global(&r), vec![[48.0, -6.0], [48.0, -122.0]]);
}

#[test]
fn one_corner_from_a_side_to_a_top() {
    // From the right side of one box to the top of one below and right of
    // it: out to the right, turning down onto it.
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 192.0);
    let (r, _) = connect(&a, [1.09375, 0.25], &b, [0.25, -0.09375]);
    assert_eq!(
        global(&r),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    assert_eq!(
        (r.x(), r.y(), r.width(), r.height()),
        (70.0, 16.0, 138.0, 170.0)
    );
    assert_eq!(r.points(), vec![[0.0, 0.0], [138.0, 0.0], [138.0, 170.0]]);
}

#[test]
fn two_corners_turn_halfway_or_leave_the_arrowhead_its_padding() {
    // Bottom of one to the top of one lower and further right.
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 192.0);
    let (r, _) = connect(&a, [0.25, 1.09375], &b, [0.25, -0.09375]);
    // 116 apart: halfway is 58, more than the padding the arrowhead needs.
    assert_eq!(
        global(&r),
        vec![[16.0, 70.0], [16.0, 128.0], [208.0, 128.0], [208.0, 186.0]]
    );
    // 76 apart: the leg to the arrowhead keeps the padding (40), the other
    // gets the rest.
    let b = square("b", 192.0, 152.0);
    let (r, _) = connect(&a, [0.25, 1.09375], &b, [0.25, -0.09375]);
    assert_eq!(
        global(&r),
        vec![[16.0, 70.0], [16.0, 106.0], [208.0, 106.0], [208.0, 146.0]]
    );
}

#[test]
fn routes_around_when_no_corner_fits() {
    // From the right side of one box to the right side of another further
    // right: out, around, and back into it from the right.
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 128.0);
    let (r, scene) = connect(&a, [1.09375, 0.25], &b, [1.09375, 0.75]);
    let g = global(&r);
    assert!(orthogonal(&g), "{g:?}");
    assert_eq!(g[0], [70.0, 16.0]);
    assert_eq!(g[g.len() - 1], [262.0, 176.0]);
    // Leaves to the right, enters going left.
    assert!(g[1][0] > g[0][0] && g[1][1] == g[0][1], "{g:?}");
    let [x, y] = g[g.len() - 2];
    assert!(x > 262.0 && y == 176.0, "{g:?}");
    // Clear of both boxes.
    for w in g.windows(2) {
        for id in ["a", "b"] {
            let s = scene.element(id).unwrap();
            let inner = [s.x() + 1.0, s.y() + 1.0, s.x() + 63.0, s.y() + 63.0];
            assert!(!crosses(w[0], w[1], inner), "{id}: {g:?}");
        }
    }
    // Past it by the padding: 36 for the arrowhead, 10 more.
    assert_eq!(
        g,
        vec![[70.0, 16.0], [302.0, 16.0], [302.0, 176.0], [262.0, 176.0]]
    );
}

#[test]
fn bound_arrows_route_again_when_a_shape_moves() {
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 192.0);
    let (r, mut scene) = connect(&a, [1.09375, 0.25], &b, [0.25, -0.09375]);
    scene.elements[2] = r.clone();
    let moved = square("b", 256.0, 192.0);
    let r = route(&r, &scene, &[moved]);
    assert_eq!(
        global(&r),
        vec![[70.0, 16.0], [272.0, 16.0], [272.0, 186.0]]
    );
    // Moved above the start's level: around again, still at right angles.
    let moved = square("b", 256.0, -192.0);
    let r = route(&r, &scene, &[moved]);
    let g = global(&r);
    assert!(orthogonal(&g), "{g:?}");
    assert_eq!((g[0], g[g.len() - 1]), ([70.0, 16.0], [272.0, -198.0]));
    assert!(g.len() > 3, "{g:?}");
}

/// An arrow with a segment moved by hand: from the right of `a` to the top
/// of `b`, its middle segment held at y = 100.
fn with_fixed_segment() -> (Element, Scene) {
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 192.0);
    let mut e = arrow(json!([[0, 0], [30, 0], [30, 84], [138, 84], [138, 170]]));
    e.set("x", json!(70));
    e.set("y", json!(16));
    e.set(
        "startBinding",
        json!({"elementId": "a", "fixedPoint": [1.09375, 0.25], "mode": "orbit"}),
    );
    e.set(
        "endBinding",
        json!({"elementId": "b", "fixedPoint": [0.25, -0.09375], "mode": "orbit"}),
    );
    e.set(
        "fixedSegments",
        json!([{"index": 3, "start": [30, 84], "end": [138, 84]}]),
    );
    e.set("startIsSpecial", json!(false));
    e.set("endIsSpecial", json!(false));
    let scene = Scene {
        elements: vec![a, b, e.clone()],
        ..Scene::default()
    };
    (e, scene)
}

#[test]
fn fixed_segments_stay_where_they_were_put() {
    let (e, scene) = with_fixed_segment();
    // Routed where it is: nothing to change.
    let r = route(&e, &scene, &[]);
    assert_eq!(r.points(), e.points());
    // The end's box moves right: the held segment stretches to it.
    let r = route(&e, &scene, &[square("b", 256.0, 192.0)]);
    assert_eq!(
        global(&r),
        vec![
            [70.0, 16.0],
            [100.0, 16.0],
            [100.0, 100.0],
            [272.0, 100.0],
            [272.0, 186.0]
        ]
    );
    assert_eq!(
        fixed_segments(&r).unwrap(),
        vec![FixedSegment {
            index: 3,
            start: [30.0, 84.0],
            end: [202.0, 84.0]
        }]
    );
    assert_eq!(r.get("startIsSpecial"), Some(&json!(false)));
}

#[test]
fn moving_a_segment_holds_it_and_letting_go_routes_anew() {
    let (e, scene) = with_fixed_segment();
    // The held segment dragged down by 30.
    let (r, index) = move_segment(&e, &scene, 3, [150.0, 130.0]);
    assert_eq!(index, 3);
    assert_eq!(
        global(&r),
        vec![
            [70.0, 16.0],
            [100.0, 16.0],
            [100.0, 130.0],
            [208.0, 130.0],
            [208.0, 186.0]
        ]
    );
    assert!(is_fixed(&r, 3) && !is_fixed(&r, 2));
    // A free segment dragged: it is held too, the other kept.
    let (r2, index) = move_segment(&r, &scene, 2, [120.0, 60.0]);
    assert_eq!(index, 2);
    let g = global(&r2);
    assert_eq!((g[1], g[2]), ([120.0, 16.0], [120.0, 130.0]));
    assert_eq!(fixed_segments(&r2).unwrap().len(), 2);
    // Let go of both: the simple route again.
    let r3 = release_segment(&r2, &scene, 2);
    assert_eq!(fixed_segments(&r3).unwrap().len(), 1);
    assert!(orthogonal(&global(&r3)));
    let r4 = release_segment(&r3, &scene, fixed_segments(&r3).unwrap()[0].index as usize);
    assert_eq!(
        global(&r4),
        vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]
    );
    assert_eq!(r4.get("fixedSegments"), Some(&Value::Null));
}

#[test]
fn segments_in_line_are_merged_when_tidied() {
    let (mut e, scene) = with_fixed_segment();
    // The held segment brought level with the start: three points in line.
    e.set(
        "points",
        json!([[0, 0], [30, 0], [30, 0], [138, 0], [138, 170]]),
    );
    e.set(
        "fixedSegments",
        json!([{"index": 3, "start": [30, 0], "end": [138, 0]}]),
    );
    let r = renormalize(&e, &scene);
    let g = global(&r);
    assert!(orthogonal(&g), "{g:?}");
    assert_eq!(g, vec![[70.0, 16.0], [208.0, 16.0], [208.0, 186.0]]);
}

#[test]
fn ends_snap_onto_outlines_and_side_middles() {
    let a = square("a", 0.0, 0.0);
    // Just inside the right side: onto it, a gap out.
    let e = arrow(json!([[63, 16], [200, 16]]));
    assert_eq!(fixed_point(&e, true, &a), [1.09375, 0.25]);
    // Near the middle of the bottom: its middle (never exactly a half).
    let e = arrow(json!([[34, 66], [34, 200]]));
    assert_eq!(fixed_point(&e, true, &a), [0.5001, 1.09375]);
    // Off a corner: to the end of the side it is nearer.
    let e = arrow(json!([[75, -10], [200, -10]]));
    assert_eq!(fixed_point(&e, true, &a), [1.09375, 0.0]);
}

#[test]
fn headings_from_the_sides_of_shapes() {
    let a = square("a", 0.0, 0.0);
    let b = aabb(&a, None);
    assert_eq!(heading_from_element(&a, b, [70.0, 16.0]), RIGHT);
    assert_eq!(heading_from_element(&a, b, [16.0, -6.0]), UP);
    assert_eq!(heading_from_element(&a, b, [-6.0, 40.0]), LEFT);
    assert_eq!(heading_from_element(&a, b, [40.0, 70.0]), DOWN);
    let mut d = a.clone();
    d.set("type", json!("diamond"));
    assert_eq!(heading_from_diamond(&d, b, [70.0, 32.0]), RIGHT);
    assert_eq!(heading_from_diamond(&d, b, [32.0, -6.0]), UP);
    assert_eq!(flip(UP), DOWN);
    assert_eq!(flip(LEFT), RIGHT);
}

#[test]
fn converting_arrows_between_types() {
    let a = square("a", 0.0, 0.0);
    let b = square("b", 192.0, 192.0);
    let sharp = el(json!({"id": "arrow", "type": "arrow", "x": 70, "y": 16,
        "points": [[0, 0], [138, 170]], "roundness": null, "elbowed": false,
        "startArrowhead": null, "endArrowhead": "arrow",
        "startBinding": {"elementId": "a", "fixedPoint": [0.5, 0.5], "mode": "orbit"},
        "endBinding": {"elementId": "b", "fixedPoint": [0.5, 0.5], "mode": "orbit"}}));
    let scene = Scene {
        elements: vec![a, b, sharp.clone()],
        ..Scene::default()
    };
    let elbow = set_type(&sharp, &scene, "elbow");
    assert!(elbow.is_elbowed() && elbow.roundness().is_none());
    let g = global(&elbow);
    assert!(g.len() > 2 && orthogonal(&g), "{g:?}");
    // Its ends snapped onto the outlines.
    assert_eq!(
        elbow.start_binding().unwrap().fixed_point,
        Some([1.09375, 0.25])
    );
    // Curved again: just its two ends, attached where they are.
    let round = set_type(&elbow, &scene, "round");
    assert!(!round.is_elbowed() && round.roundness().is_some());
    assert_eq!(round.points().len(), 2);
    assert_eq!(global(&round), vec![g[0], g[g.len() - 1]]);
    assert_eq!(round.end_binding().unwrap().mode.as_deref(), Some("orbit"));
}

#[test]
fn lone_bound_elbow_arrows_stay_put() {
    let (e, scene) = with_fixed_segment();
    let a = scene.element("a").unwrap().clone();
    let b = scene.element("b").unwrap().clone();
    assert!(stays(&e, std::slice::from_ref(&e)));
    assert!(stays(&e, &[e.clone(), a.clone()]));
    assert!(!stays(&e, &[e.clone(), a, b]));
    let free = arrow(json!([[0, 0], [10, 10]]));
    assert!(!stays(&free, std::slice::from_ref(&free)));
}

#[test]
fn segment_handles_at_the_middles() {
    let (e, _) = with_fixed_segment();
    let handles = segment_handles(&e, 1.0);
    assert_eq!(handles.len(), 4);
    assert_eq!(handles[2], (3, [154.0, 100.0]));
    assert_eq!(segment_at(&e, [150.0, 103.0], 1.0), Some(3));
    assert_eq!(segment_at(&e, [150.0, 130.0], 1.0), None);
}
