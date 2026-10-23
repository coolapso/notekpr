//! The editor's model under arbitrary text and edits: classifying lines
//! incrementally gives what classifying from scratch gives, the line index
//! follows every change, and styling a line never panics nor loses a byte.
//!
//! Input: the text, then (after a `\x01`) an edit script of three bytes per
//! edit: start, end (scaled to the text) and which snippet to insert.
#![no_main]
use notekpr_editor::change::ChangeSet;
use notekpr_editor::decorations::Decorations;
use notekpr_editor::syntax;
use notekpr_editor::text::{Text, floor_char};
use libfuzzer_sys::fuzz_target;

const SNIPPETS: &[&str] = &[
    "", "\n", "x", "# ", "```", "~~~\n", "- [ ] ", "1. ", "> ", "|", "| a | b |\n|---|---|\n",
    "**", "[[", "]]", "$$", "<!--", "-->", "\t", "é", "\n\n", "---\n", "    ",
    "<div>", "</div>\n", "<pre>", "</pre>", "<span>", "<img src=a.png>\n",
];

fuzz_target!(|data: &[u8]| {
    let (text, script) = match data.iter().position(|&b| b == 1) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[][..]),
    };
    let Ok(text) = std::str::from_utf8(text) else { return };

    let lines: Vec<&str> = text.split('\n').collect();
    let blocks = syntax::classify(&lines);
    assert_eq!(blocks.len(), lines.len());
    for (line, block) in lines.iter().zip(&blocks) {
        let styled = syntax::style_line(line, block, &|t: &str| t.len() % 2 == 0);
        assert_eq!(styled.flags.len(), line.len());
        let _ = syntax::line_html(line, &styled);
        let _ = syntax::list_marker(line);
        let _ = syntax::toggle_task(line);
        let _ = syntax::table_cells(line);
    }

    let mut doc = Text::new(text.to_string());
    let mut plain = text.to_string();
    let mut deco = Decorations::new(&doc);
    for e in script.chunks_exact(3).take(32) {
        let len = doc.len();
        let a = floor_char(doc.as_str(), e[0] as usize * (len + 1) / 256);
        let b = floor_char(doc.as_str(), e[1] as usize * (len + 1) / 256);
        let (from, to) = (a.min(b), a.max(b));
        let ins = SNIPPETS[e[2] as usize % SNIPPETS.len()];
        let change = ChangeSet::replace(len, from, to, ins);
        plain.replace_range(from..to, ins);
        let splices = doc.apply(&change);
        deco.update(&doc, &splices);
        assert_eq!(doc.as_str(), plain);
        let lines: Vec<&str> = plain.split('\n').collect();
        assert_eq!(doc.starts().len(), lines.len());
        assert_eq!(deco.blocks().to_vec(), syntax::classify(&lines));
    }
});
