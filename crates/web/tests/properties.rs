//! Property tests for the browser client's pure parts (run natively with
//! `cargo test -p notekpr-web`): the reading-view renderer and zip
//! import. Random text is biased towards Markdown syntax and multibyte
//! characters; these are the inputs behind the crashes found in review. The
//! live editor's syntax engine, table keys and task toggling are tested in
//! `crates/editor/tests/properties.rs`.

use notekpr_web::{render, zipfile};
use proptest::prelude::*;

const ALPHABET: &[char] = &[
    'a', 'b', 'Z', '1', ' ', ' ', '\n', '\n', '\t', '#', '#', '*', '_', '`', '`', '~', '=', '[',
    ']', '(', ')', '!', '|', '|', '\\', '/', ':', '.', '-', '-', '>', '%', '<', '&', '"', '\'',
    'x', 'é', '€', '増', '😀', '\u{a0}', '\u{301}',
];

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ALPHABET), 0..max)
        .prop_map(|v| v.into_iter().collect())
}

/// Text as a browser reads it from the HTML.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#13;", "\r")
        .replace("&amp;", "&")
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn rendering_never_panics(t in text(300)) {
        let note = |s: &str| s.len().is_multiple_of(3).then(|| "n1".to_string());
        let attachment = |s: &str| s.len().is_multiple_of(2).then(|| "a1".to_string());
        let html = render::to_html(&t, &render::Ctx::new(&note, &attachment));
        prop_assert!(!html.contains("<script"));
    }

    /// With comment highlights anywhere, the text of every element that says
    /// where it is in the source (`data-o`) is the source there, character
    /// for character: what a selection in the reading view is mapped with.
    #[test]
    fn offsets_point_at_the_text_they_hold(
        t in text(200),
        picks in prop::collection::vec((any::<prop::sample::Index>(), any::<prop::sample::Index>()), 0..4),
    ) {
        let bounds: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
        let comments: Vec<render::Highlight> = picks
            .iter()
            .enumerate()
            .map(|(k, (a, b))| {
                let (x, y) = (bounds[a.index(bounds.len())], bounds[b.index(bounds.len())]);
                render::Highlight { start: x.min(y), end: x.max(y), id: format!("t{k}"), open: k == 0 }
            })
            .collect();
        let none = |_: &str| None;
        let html = render::to_html(
            &t,
            &render::Ctx { comments: &comments, offsets: true, ..render::Ctx::new(&none, &none) },
        );
        for part in html.split("data-o=\"").skip(1) {
            let (n, rest) = part.split_once('"').unwrap();
            let at: usize = n.parse().unwrap();
            let inner = rest.split_once('>').unwrap().1;
            let shown = unescape(inner.split('<').next().unwrap());
            prop_assert_eq!(t.get(at..at + shown.len()), Some(shown.as_str()), "{}", html);
        }
    }

    #[test]
    fn zip_import_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
        let _ = zipfile::read(&bytes);
    }

    #[test]
    fn damaged_zips_fail_cleanly(flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8)) {
        let mut zip = zipfile::write(&[
            zipfile::Entry { name: "a.md".into(), data: b"hello world, hello zip".to_vec() },
            zipfile::Entry { name: "dir/b.png".into(), data: vec![0x89, b'P', b'N', b'G', 1, 2, 3] },
        ])
        .unwrap();
        for (i, v) in flips {
            let n = i.index(zip.len());
            zip[n] = v;
        }
        let _ = zipfile::read(&zip);
    }
}
