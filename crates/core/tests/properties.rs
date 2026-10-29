//! Property tests: random input, biased towards Markdown syntax, multibyte
//! text and the characters that trip byte-offset code. These would have
//! caught the multibyte slicing crashes found in review, and keep parsers,
//! diffs and merging honest. Run with `cargo test -p notekpr-core`;
//! more cases: `PROPTEST_CASES=20000 cargo test -p notekpr-core --test properties`.

use notekpr_core::crypto::{self, Purpose};
use notekpr_core::doc::NoteDoc;
use notekpr_core::{markdown, paths, search, textdiff};
use notekpr_draw::scene::{Element, Scene};
use proptest::prelude::*;
use serde_json::json;

/// Characters that matter to the code under test, plus multibyte ones.
const ALPHABET: &[char] = &[
    'a',
    'b',
    'Z',
    '1',
    ' ',
    ' ',
    '\n',
    '\n',
    '\t',
    '#',
    '#',
    '*',
    '_',
    '`',
    '~',
    '=',
    '[',
    ']',
    '(',
    ')',
    '!',
    '|',
    '\\',
    '/',
    ':',
    '.',
    '-',
    '>',
    '%',
    '%',
    '2',
    '0',
    'F',
    '+',
    '^',
    '$',
    '{',
    '}',
    '"',
    '\'',
    '<',
    '&',
    ',',
    'é',
    'ß',
    '€',
    '増',
    '日',
    '😀',
    '👍',
    '\u{1f3fd}',
    '\u{a0}',
    '\u{200b}',
    '\u{301}',
];

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ALPHABET), 0..max)
        .prop_map(|v| v.into_iter().collect())
}

/// 512 cases per property by default; `PROPTEST_CASES` overrides.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn markdown_analysis_never_panics_and_spans_are_valid(t in text(200)) {
        let info = markdown::analyze(&t);
        for l in &info.links {
            prop_assert!(l.start <= l.end && l.end <= t.len());
            prop_assert!(t.get(l.start..l.end).is_some(), "link span not on char boundaries");
        }
        let _ = markdown::frontmatter_len(&t);
        for line in t.lines() {
            let _ = markdown::heading(line);
        }
        let _ = markdown::rewrite_links(&t, "New name", |_| true);
    }

    #[test]
    fn percent_decoding_never_panics(t in text(60)) {
        let _ = markdown::percent_decode(&t);
        let _ = markdown::is_internal_target(&t);
    }

    #[test]
    fn diffs_reproduce_the_new_text(old in text(120), new in text(120)) {
        let edits = textdiff::diff(&old, &new);
        prop_assert_eq!(textdiff::apply(&old, &edits), new);
    }

    #[test]
    fn notes_take_any_text_and_converge(base in text(60), a in text(60), b in text(60)) {
        let x = NoteDoc::new();
        let up = x.set_text(&base);
        let y = NoteDoc::new();
        if let Some(u) = &up {
            y.apply_update(u).unwrap();
        }
        prop_assert_eq!(y.text(), base.clone());
        // Concurrent edits on both sides, then exchanged: both end up equal,
        // and each side's own text was taken exactly before merging.
        let ua = x.set_text(&a);
        prop_assert_eq!(x.text(), a.clone());
        let ub = y.set_text(&b);
        prop_assert_eq!(y.text(), b.clone());
        if let Some(u) = &ua { y.apply_update(u).unwrap(); }
        if let Some(u) = &ub { x.apply_update(u).unwrap(); }
        prop_assert_eq!(x.text(), y.text());
        // A full state reproduces the merged text.
        let z = NoteDoc::from_updates([x.encode_state().as_slice()]).unwrap();
        prop_assert_eq!(z.text(), x.text());
    }

    #[test]
    fn normalized_paths_are_safe(t in text(80)) {
        if let Ok(p) = paths::normalize(&t) {
            prop_assert!(!p.is_empty() && !p.starts_with('/') && !p.ends_with('/'));
            for seg in p.split('/') {
                prop_assert!(!seg.is_empty() && seg != "." && seg != "..");
                prop_assert!(!seg.chars().any(char::is_control));
            }
            // Normalizing is idempotent.
            prop_assert_eq!(paths::normalize(&p).unwrap(), p.clone());
            let _ = paths::title(&p);
            let _ = paths::parent(&p);
        }
        let _ = paths::normalize_note(&t);
    }

    #[test]
    fn search_never_panics(q in text(40), body in text(200)) {
        let query = search::parse_query(&q);
        let _ = search::score(&query, "Folder/Note.md", &body, &[]);
    }

    #[test]
    fn opening_random_bytes_fails_cleanly(bytes in prop::collection::vec(any::<u8>(), 0..120)) {
        let key = [7u8; 32];
        let _ = crypto::sym_epoch(&bytes);
        prop_assert!(crypto::open_sym(&key, Purpose::Update, &[b"v", b"o"], &bytes).is_err());
    }

    #[test]
    fn concurrent_drawing_edits_converge_and_keep_both(
        edits_a in prop::collection::vec((0usize..4, 0usize..3, -500i32..500), 0..12),
        edits_b in prop::collection::vec((0usize..4, 0usize..3, -500i32..500), 0..12),
        added_a in 0usize..3,
        added_b in 0usize..3,
    ) {
        // Four shapes, then each side changes properties and adds shapes.
        let start = scene((0..4).map(|i| shape(&format!("s{i}"), i as f64 * 10.0)).collect());
        let base = NoteDoc::default();
        base.drawing().set_scene(&start);
        let state = base.encode_state();
        let x = NoteDoc::from_updates([state.as_slice()]).unwrap();
        let y = NoteDoc::from_updates([state.as_slice()]).unwrap();
        let edit = |doc: &NoteDoc, edits: &[(usize, usize, i32)], added: usize, who: &str| {
            let mut s = doc.drawing().scene();
            for (i, prop, v) in edits {
                let key = ["x", "y", "strokeColor"][*prop];
                let value = if key == "strokeColor" { json!(format!("#{:06x}", v.unsigned_abs())) } else { json!(v) };
                s.elements[*i].set(key, value);
            }
            for k in 0..added {
                s.elements.push(shape(&format!("{who}{k}"), 1000.0 + k as f64));
            }
            doc.drawing().set_scene(&s)
        };
        let ua = edit(&x, &edits_a, added_a, "a");
        let ub = edit(&y, &edits_b, added_b, "b");
        if let Some(u) = &ua { y.apply_update(u).unwrap(); }
        if let Some(u) = &ub { x.apply_update(u).unwrap(); }
        let (sx, sy) = (x.drawing().scene(), y.drawing().scene());
        prop_assert_eq!(&sx, &sy);
        // What each added is there; what only one side changed is its value.
        prop_assert_eq!(sx.visible().count(), 4 + added_a + added_b);
        for (i, prop, v) in &edits_a {
            let key = ["x", "y", "strokeColor"][*prop];
            let touched_by_b = edits_b.iter().any(|(j, q, _)| j == i && q == prop);
            let last = edits_a.iter().rev().find(|(j, q, _)| j == i && q == prop).map(|e| e.2);
            if !touched_by_b && last == Some(*v) {
                let e = sx.element(&format!("s{i}")).unwrap();
                let want = if key == "strokeColor" { json!(format!("#{:06x}", v.unsigned_abs())) } else { json!(v) };
                prop_assert_eq!(e.get(key), Some(&want));
            }
        }
    }
}

fn shape(id: &str, x: f64) -> Element {
    Element::from_props(
        json!({"id": id, "type": "rectangle", "x": x, "y": 0, "width": 10, "height": 10,
               "index": format!("a{id}")})
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap()
}

fn scene(elements: Vec<Element>) -> Scene {
    Scene {
        elements,
        ..Scene::default()
    }
}
