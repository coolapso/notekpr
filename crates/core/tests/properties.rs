//! Property tests: random input, biased towards Markdown syntax, multibyte
//! text and the characters that trip byte-offset code. These would have
//! caught the multibyte slicing crashes found in review, and keep parsers,
//! diffs and merging honest. Run with `cargo test -p coolpsonotes-core`;
//! more cases: `PROPTEST_CASES=20000 cargo test -p coolpsonotes-core --test properties`.

use coolpsonotes_core::crypto::{self, Purpose};
use coolpsonotes_core::doc::NoteDoc;
use coolpsonotes_core::{markdown, paths, search, textdiff};
use proptest::prelude::*;

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
}
