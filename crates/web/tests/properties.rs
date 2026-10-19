//! Property tests for the browser client's pure parts (run natively with
//! `cargo test -p coolpsonotes-web`): the reading-view renderer and zip
//! import. Random text is biased towards Markdown syntax and multibyte
//! characters; these are the inputs behind the crashes found in review. The
//! live editor's syntax engine, table keys and task toggling are tested in
//! `crates/editor/tests/properties.rs`.

use coolpsonotes_web::{render, zipfile};
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
        let note = |s: &str| (s.len() % 3 == 0).then(|| "n1".to_string());
        let attachment = |s: &str| (s.len() % 2 == 0).then(|| "a1".to_string());
        let html = render::to_html(&t, &render::Ctx { note: &note, attachment: &attachment });
        prop_assert!(!html.contains("<script"));
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
