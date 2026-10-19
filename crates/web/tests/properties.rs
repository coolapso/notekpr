//! Property tests for the browser client's pure parts (run natively with
//! `cargo test -p coolpsonotes-web`): the live editor's syntax engine, table
//! keys, task toggling, the reading-view renderer and zip import. Random text
//! is biased towards Markdown syntax and multibyte characters; these are the
//! inputs behind the crashes found in review.

use coolpsonotes_web::editor::syntax::{self, TableKey};
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

/// Text content of rendered line HTML: tags removed, entities decoded.
fn text_of(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn boundaries(t: &str) -> Vec<usize> {
    t.char_indices().map(|(i, _)| i).chain([t.len()]).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// The editor's DOM text must always equal the Markdown source.
    #[test]
    fn rendered_lines_keep_the_source_text(t in text(160)) {
        let lines: Vec<&str> = t.split('\n').collect();
        let blocks = syntax::classify(&lines);
        prop_assert_eq!(blocks.len(), lines.len());
        for (line, block) in lines.iter().zip(&blocks) {
            let styled = syntax::style_line(line, block, &|t: &str| t.len() % 2 == 0);
            prop_assert_eq!(styled.flags.len(), line.len());
            let html = syntax::line_html(line, &styled);
            prop_assert_eq!(text_of(&html), *line);
        }
    }

    #[test]
    fn table_keys_land_on_char_boundaries(t in text(120), pick in any::<prop::sample::Index>(), back in any::<bool>()) {
        let b = boundaries(&t);
        let offset = b[pick.index(b.len())];
        for k in [syntax::table_tab(&t, offset, back), syntax::table_enter(&t, offset)].into_iter().flatten() {
            match k {
                TableKey::Move(o) => prop_assert!(t.is_char_boundary(o)),
                TableKey::Edit { from, to, insert, caret } => {
                    prop_assert!(from <= to && t.is_char_boundary(from) && t.is_char_boundary(to));
                    let mut after = t.clone();
                    after.replace_range(from..to, &insert);
                    prop_assert!(after.is_char_boundary(caret));
                }
            }
        }
    }

    #[test]
    fn toggling_a_task_only_flips_its_box(line in text(60)) {
        if let Some(toggled) = syntax::toggle_task(&line) {
            prop_assert_eq!(toggled.chars().count(), line.chars().count());
            let changed = line.chars().zip(toggled.chars()).filter(|(a, b)| a != b).count();
            prop_assert_eq!(changed, 1);
        }
    }

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
