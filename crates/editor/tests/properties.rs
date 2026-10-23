//! Property tests for the editor's document model (native: `cargo test -p
//! notekpr-editor`). Random text is biased towards Markdown syntax and
//! multibyte characters — the inputs behind the crashes found in review.
//! `PROPTEST_CASES` raises the number of cases.
#![cfg(not(target_arch = "wasm32"))]

use notekpr_core::textdiff;
use notekpr_editor::change::{Assoc, ChangeSet};
use notekpr_editor::code::{self, Carry, FenceInfo, Indent};
use notekpr_editor::decorations::Decorations;
use notekpr_editor::html::{self, BlockKind};
use notekpr_editor::state::{EditorState, Origin, Selection, Transaction, merge_native};
use notekpr_editor::syntax::{self, Block, TableKey};
use notekpr_editor::text::Text;
use notekpr_editor::{commands, edits_fit, rebase};
use proptest::prelude::*;
use std::rc::Rc;

const ALPHABET: &[char] = &[
    'a', 'b', 'Z', '1', ' ', ' ', '\n', '\n', '\t', '#', '#', '*', '_', '`', '`', '~', '=', '[',
    ']', '(', ')', '!', '|', '|', '\\', '/', ':', '.', '-', '-', '>', '%', '<', '&', '"', '\'',
    'x', 'é', '€', '増', '😀', '\u{a0}', '\u{301}',
];

/// Lines that matter for block classification across lines.
const LINES: &[&str] = &[
    "```",
    "```rust",
    "```c title=a.c hl=2,4-5",
    "```python ln=false hl:1",
    "```yaml vl=false",
    "/* open",
    "close */",
    "    indented",
    "\tx",
    "  ",
    "\"\"\"",
    "~~~",
    "---",
    "...",
    "| a | b |",
    "| - | - |",
    "| :-- | --: |",
    "a | b",
    "- | -",
    "| x |",
    "",
    "text",
    "# h",
    "> q",
    ">",
    "- item",
    "- ",
    "  indented",
    "1. one",
    "    ```",
    "<div>",
    "</div>",
    "<details open>",
    "<!-- c",
    "-->",
    "<!-- x -->",
    "<pre>",
    "</pre>",
    "<img src=\"a.png\" width=300>",
    "<span>",
    "<b>x</b>",
    "<?php",
    "<!X",
    ">",
];

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ALPHABET), 0..max)
        .prop_map(|v| v.into_iter().collect())
}

/// Text made of whole lines, many of them block syntax.
fn lines_text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            3 => prop::sample::select(LINES).prop_map(str::to_string),
            1 => text(12).prop_map(|t| t.replace('\n', " ")),
        ],
        1..max,
    )
    .prop_map(|v| v.join("\n"))
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

fn boundaries(t: &str) -> Vec<usize> {
    t.char_indices().map(|(i, _)| i).chain([t.len()]).collect()
}

/// A random replacement on `t`: `(from, to, insert)` on character boundaries.
fn edit_on(t: &str, a: prop::sample::Index, b: prop::sample::Index) -> (usize, usize) {
    let bs = boundaries(t);
    let (x, y) = (bs[a.index(bs.len())], bs[b.index(bs.len())]);
    (x.min(y), x.max(y))
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

/// Pieces of HTML, many of them hostile, for the sanitizer.
const HTML_PIECES: &[&str] = &[
    "<script>",
    "</script>",
    "<style>",
    "</style>",
    "<b>",
    "</b>",
    "<i>",
    "</div>",
    "<div>",
    "<a href=\"javascript:alert(1)\">",
    "<a href=x title='t\"'>",
    "</a>",
    "<img src=x onerror=alert(1)>",
    "<img src=\"a.png\" width=30 alt=\"&lt;\">",
    "<div style=\"position:fixed\" class=\"wl\" id=app data-note=1>",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "<svg>",
    "</svg>",
    "<math>",
    "<textarea>",
    "</textarea>",
    "<title>",
    "</title>",
    "<plaintext>",
    "<noscript>",
    "</noscript>",
    "<iframe srcdoc=x>",
    "</iframe>",
    "<details open>",
    "<summary>",
    "&lt;",
    "&#60;",
    "&#x3C;script",
    "&amp;",
    "&",
    "\"",
    "'",
    "=",
    "\r",
    "&#13;",
    "<",
    ">",
    "/",
    " ",
    "\n",
    "<td colspan=3 rowspan=x>",
    "<table>",
    "<tr>",
    "<p align=center onclick=x>",
    "\0",
    "<xmp>",
    "</xmp>",
    "<template>",
    "</",
    "<!DOCTYPE html>",
    "<?xml?>",
    "<br/>",
    "<ol start=-2 reversed type=I>",
    "<span lang=en dir=rtl>",
    "<b <i>",
    "<a\nhref=x>",
    "<form action=/x>",
    "<input name=pw>",
    "<object data=x>",
    "<embed src=x>",
];

fn html_soup() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => prop::sample::select(HTML_PIECES).prop_map(str::to_string),
            1 => text(6),
        ],
        0..40,
    )
    .prop_map(|v| v.concat())
}

/// `html` sanitized, links and images written out with their targets.
fn sanitize(src: &str) -> String {
    let link = |href: &str, attrs: &str| {
        let mut o = String::from("<a href=\"");
        syntax::escape(href, &mut o);
        format!("{o}\"{attrs}>")
    };
    let image = |src: &str, alt: &str, attrs: &str| {
        let (mut s, mut a) = (String::new(), String::new());
        syntax::escape(src, &mut s);
        syntax::escape(alt, &mut a);
        format!("<img src=\"{s}\" alt=\"{a}\"{attrs}>")
    };
    html::sanitize(
        src,
        &html::Urls {
            link: &link,
            image: &image,
        },
    )
}

/// The tokens a browser reads from `html` (outside raw text, which the
/// sanitizer's output never opens).
fn tokens(html: &str) -> Vec<html5ever::tokenizer::Token> {
    use html5ever::tendril::StrTendril;
    use html5ever::tokenizer::*;
    struct Collect(std::cell::RefCell<Vec<Token>>);
    impl TokenSink for Collect {
        type Handle = ();
        fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
            self.0.borrow_mut().push(token);
            TokenSinkResult::Continue
        }
    }
    let tok = Tokenizer::new(Collect(Default::default()), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.0.into_inner()
}

/// The one-pass classifier the editor used before blocks were computed
/// incrementally — the specification [`syntax::classify`] must still meet.
fn reference_classify(lines: &[&str]) -> Vec<Block> {
    use syntax::{Row, list_marker, table_cell_texts, task_box};
    fn fence_of(line: &str) -> Option<(&str, &str)> {
        let t = line.trim_start();
        if line.len() - t.len() > 3 {
            return None;
        }
        for f in ["```", "~~~"] {
            if let Some(rest) = t.strip_prefix(f) {
                let ticks = f.chars().next().unwrap();
                return Some((f, rest.trim_start_matches(ticks).trim()));
            }
        }
        None
    }
    let is_hr = |line: &str| {
        let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        t.len() >= 3
            && (t.chars().all(|c| c == '-')
                || t.chars().all(|c| c == '*')
                || t.chars().all(|c| c == '_'))
    };
    let is_table_line = |line: &str| line.trim_start().starts_with('|');
    let continues_table = |line: &str| {
        is_table_line(line)
            || (!syntax::table_pipes(line).is_empty()
                && !line.trim().is_empty()
                && notekpr_core::markdown::heading(line).is_none()
                && !line.trim_start().starts_with('>')
                && list_marker(line).is_none())
    };
    let delimiter_row = |line: &str| -> Option<Vec<syntax::Align>> {
        let cells = table_cell_texts(line);
        if cells.is_empty() {
            return None;
        }
        cells
            .iter()
            .map(|c| {
                let (l, r) = (c.starts_with(':'), c.ends_with(':'));
                let dashes = c.trim_matches(':');
                if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                    return None;
                }
                Some(match (l, r) {
                    (true, true) => syntax::Align::Center,
                    (false, true) => syntax::Align::Right,
                    (true, false) => syntax::Align::Left,
                    (false, false) => syntax::Align::None,
                })
            })
            .collect()
    };
    let table_start = |i: usize| -> Option<Vec<syntax::Align>> {
        let (line, next) = (lines[i], *lines.get(i + 1)?);
        if !is_table_line(line)
            && (syntax::table_pipes(line).is_empty() || syntax::table_pipes(next).is_empty())
        {
            return None;
        }
        delimiter_row(next).filter(|a| a.len() == table_cell_texts(line).len())
    };
    let mut out = Vec::with_capacity(lines.len());
    // The open fence: its characters, options, lines so far, carry and guides.
    let mut fence: Option<(String, Rc<FenceInfo>, u32, Carry, Indent)> = None;
    let mut in_fm = lines.first().is_some_and(|l| l.trim_end() == "---");
    // After a list item (false) or quote (true) with text: lines that start
    // no block continue it.
    let mut lazy: Option<bool> = None;
    let continues = |line: &str| {
        let t = line.trim_start();
        !t.is_empty()
            && fence_of(line).is_none()
            && notekpr_core::markdown::heading(line).is_none()
            && !(is_hr(line) && !t.starts_with("- "))
            && !t.starts_with('>')
            && list_marker(line).is_none()
            && !BlockKind::start(line).is_some_and(BlockKind::interrupts_paragraph)
    };
    // The open HTML block, and where it ends.
    let mut html: Option<BlockKind> = None;
    let html_last = |kind: BlockKind, i: usize| match lines.get(i + 1) {
        None => true,
        Some(next) if kind.ends_at_blank() => next.trim().is_empty(),
        Some(_) => kind.ends_on(lines[i]),
    };
    let mut queued: std::collections::VecDeque<Block> = Default::default();
    for (i, line) in lines.iter().enumerate() {
        if let Some(b) = queued.pop_front() {
            out.push(b);
            continue;
        }
        if in_fm {
            if i == 0 {
                out.push(Block::FrontmatterFence);
                continue;
            }
            if line.trim_end() == "---" || line.trim_end() == "..." {
                in_fm = false;
                out.push(Block::FrontmatterFence);
            } else {
                out.push(Block::Frontmatter);
            }
            continue;
        }
        if let Some((f, info, n, carry, indent)) = &mut fence {
            if let Some((g, rest)) = fence_of(line)
                && g == f
                && rest.is_empty()
            {
                out.push(Block::Fence {
                    info: info.clone(),
                    open: false,
                });
                fence = None;
                continue;
            }
            *n += 1;
            let (guides, next) = indent.line(line, lines.get(i + 1).copied());
            *indent = next;
            out.push(Block::Code {
                info: info.clone(),
                carry: *carry,
                hl: info.highlighted(*n),
                guides: if info.guides { guides } else { (0, 0) },
            });
            *carry = code::carry_after(line, &info.lang, *carry);
            continue;
        }
        if let Some(kind) = html {
            let last = html_last(kind, i);
            out.push(Block::Html { kind, last });
            if last {
                html = None;
            }
            continue;
        }
        if let Some(quote) = lazy {
            if continues(line) {
                out.push(Block::Lazy { quote });
                continue;
            }
            lazy = None;
        }
        if let Some((f, info)) = fence_of(line) {
            let info = Rc::new(FenceInfo::parse(info));
            fence = Some((
                f.to_string(),
                info.clone(),
                0,
                Carry::None,
                Indent::default(),
            ));
            out.push(Block::Fence { info, open: true });
            continue;
        }
        if line.trim().is_empty() {
            out.push(Block::Blank);
        } else if let Some((lvl, _)) = notekpr_core::markdown::heading(line) {
            out.push(Block::Heading(lvl));
        } else if is_hr(line) && !line.trim_start().starts_with("- ") {
            out.push(Block::Hr);
        } else if line.trim_start().starts_with('>') {
            out.push(Block::Quote);
            if !line.trim_start_matches(['>', ' ', '\t']).trim().is_empty() {
                lazy = Some(true);
            }
        } else if let Some((indent, len, ordered)) = list_marker(line) {
            out.push(Block::List {
                ordered,
                task: task_box(&line[indent + len..]),
            });
            if !line[indent + len..].trim().is_empty() {
                lazy = Some(false);
            }
        } else if let Some(kind) = BlockKind::start(line).filter(|k| {
            k.interrupts_paragraph()
                || !matches!(out.last(), Some(Block::Paragraph | Block::PipeText))
        }) {
            let last = html_last(kind, i);
            out.push(Block::Html { kind, last });
            if !last {
                html = Some(kind);
            }
        } else if let Some(align) = table_start(i) {
            let end = (i + 2..lines.len())
                .find(|&j| !continues_table(lines[j]))
                .unwrap_or(lines.len());
            out.push(Block::Table {
                row: Row::Header,
                align: align.clone(),
            });
            queued.push_back(Block::Table {
                row: Row::Delimiter,
                align: align.clone(),
            });
            for _ in i + 2..end {
                queued.push_back(Block::Table {
                    row: Row::Body,
                    align: align.clone(),
                });
            }
        } else if is_table_line(line) {
            let end = (i..lines.len())
                .find(|&j| !is_table_line(lines[j]))
                .unwrap_or(lines.len());
            out.push(Block::PipeText);
            queued.extend((i + 1..end).map(|_| Block::PipeText));
        } else {
            out.push(Block::Paragraph);
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// The line state machine classifies exactly like the one-pass classifier.
    #[test]
    fn classification_matches_the_reference(t in prop_oneof![lines_text(24), text(160)]) {
        let lines: Vec<&str> = t.split('\n').collect();
        prop_assert_eq!(syntax::classify(&lines), reference_classify(&lines));
    }

    /// Re-classifying around edits gives what classifying from scratch gives,
    /// and every line whose block changed is reported for redrawing.
    #[test]
    fn incremental_blocks_match_a_full_classification(
        t in lines_text(24),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<prop::sample::Index>(), prop_oneof![lines_text(3), text(6)]), 1..6),
    ) {
        let mut doc = Text::new(t);
        let mut deco = Decorations::new(&doc);
        for (a, b, ins) in edits {
            let before: Vec<Block> = deco.blocks().to_vec();
            let (from, to) = edit_on(doc.as_str(), a, b);
            let change = ChangeSet::replace(doc.len(), from, to, &ins);
            let splices = doc.apply(&change);
            let dirty = deco.update(&doc, &splices);
            let lines: Vec<&str> = doc.as_str().split('\n').collect();
            prop_assert_eq!(deco.blocks().to_vec(), syntax::classify(&lines));
            prop_assert_eq!(doc.starts().len(), lines.len());
            // Unchanged lines (outside the splice) that changed block are redrawn.
            let Some(&sp) = splices.first() else {
                prop_assert!(dirty.is_empty());
                continue;
            };
            let delta = sp.new as isize - sp.old as isize;
            for (i, blk) in deco.blocks().iter().enumerate() {
                if sp.new_range().contains(&i) {
                    prop_assert!(dirty.iter().any(|r| r.contains(&i)));
                    continue;
                }
                let old_i = if i < sp.new_start { i } else { (i as isize - delta) as usize };
                if before.get(old_i) != Some(blk) {
                    prop_assert!(dirty.iter().any(|r| r.contains(&i)), "line {} not redrawn", i);
                }
            }
        }
    }

    #[test]
    fn line_index_follows_changes(t in text(120), edits in prop::collection::vec((any::<prop::sample::Index>(), any::<prop::sample::Index>(), text(8)), 1..8)) {
        let mut doc = Text::new(t.clone());
        let mut plain = t;
        let mut all = ChangeSet::empty(doc.len());
        for (a, b, ins) in edits {
            let (from, to) = edit_on(doc.as_str(), a, b);
            let c = ChangeSet::replace(doc.len(), from, to, &ins);
            plain.replace_range(from..to, &ins);
            doc.apply(&c);
            all = all.compose(&c);
            prop_assert_eq!(doc.as_str(), plain.as_str());
            prop_assert_eq!(doc.clone(), Text::new(plain.clone()));
        }
        let _ = all;
    }

    /// Composition, edit lists and diffs all describe the same change.
    #[test]
    fn change_sets_compose_and_convert(t in text(80), e1 in (any::<prop::sample::Index>(), any::<prop::sample::Index>(), text(6)), e2 in (any::<prop::sample::Index>(), any::<prop::sample::Index>(), text(6))) {
        let (f1, t1) = edit_on(&t, e1.0, e1.1);
        let c1 = ChangeSet::replace(t.len(), f1, t1, &e1.2);
        let mid = c1.apply(&t).unwrap();
        let (f2, t2) = edit_on(&mid, e2.0, e2.1);
        let c2 = ChangeSet::replace(mid.len(), f2, t2, &e2.2);
        let end = c2.apply(&mid).unwrap();
        let both = c1.compose(&c2);
        prop_assert_eq!(both.apply(&t).unwrap(), end.clone());
        let edits = both.to_edits();
        prop_assert!(edits_fit(&t, &edits));
        prop_assert_eq!(textdiff::apply(&t, &edits), end.clone());
        prop_assert_eq!(ChangeSet::from_edits(&t, &edits).unwrap().apply(&t).unwrap(), end.clone());
        prop_assert_eq!(ChangeSet::diff(&t, &end).apply(&t).unwrap(), end.clone());
        for p in boundaries(&t) {
            let q = both.map_pos(p, Assoc::After);
            prop_assert!(end.is_char_boundary(q));
            let q = both.map_pos(p, Assoc::Before);
            prop_assert!(end.is_char_boundary(q));
        }
    }

    /// Concurrent changes converge whichever is applied first, and rebasing
    /// local edits keeps both sides' text.
    #[test]
    fn concurrent_changes_converge(t in text(80), e1 in (any::<prop::sample::Index>(), any::<prop::sample::Index>(), text(6)), e2 in (any::<prop::sample::Index>(), any::<prop::sample::Index>(), text(6))) {
        let (f1, t1) = edit_on(&t, e1.0, e1.1);
        let (f2, t2) = edit_on(&t, e2.0, e2.1);
        let a = ChangeSet::replace(t.len(), f1, t1, &e1.2);
        let b = ChangeSet::replace(t.len(), f2, t2, &e2.2);
        let (a2, b2) = a.transform(&b);
        let x = b2.apply(&a.apply(&t).unwrap()).unwrap();
        let y = a2.apply(&b.apply(&t).unwrap()).unwrap();
        prop_assert_eq!(&x, &y);
        prop_assert!(x.contains(e1.2.as_str()) && x.contains(e2.2.as_str()));
        // The host-facing rebase and the editor's native merge agree.
        let theirs = b.apply(&t).unwrap();
        let moved = rebase(&t, &theirs, &a.to_edits()).unwrap();
        let (tr, after) = merge_native(&t, a.clone(), Some(theirs.clone()));
        let merged = tr.changes.apply(&t).unwrap();
        prop_assert_eq!(textdiff::apply(&theirs, &moved), merged.clone());
        prop_assert_eq!(after.apply(&a.apply(&t).unwrap()).unwrap(), merged);
    }

    /// The editor's DOM text must always equal the Markdown source.
    #[test]
    fn rendered_lines_keep_the_source_text(t in text(160)) {
        let lines: Vec<&str> = t.split('\n').collect();
        let blocks = syntax::classify(&lines);
        prop_assert_eq!(blocks.len(), lines.len());
        for (line, block) in lines.iter().zip(&blocks) {
            let styled = syntax::style_line(line, block, &|t: &str| t.len().is_multiple_of(2));
            prop_assert_eq!(styled.flags.len(), line.len());
            let html = syntax::line_html(line, &styled);
            prop_assert_eq!(text_of(&html), *line);
        }
    }

    #[test]
    fn table_keys_land_on_char_boundaries(t in prop_oneof![text(120), lines_text(12)], pick in any::<prop::sample::Index>(), back in any::<bool>()) {
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

    /// Commands on any text and selection keep the text and the selection on
    /// character boundaries.
    #[test]
    fn commands_never_split_characters(t in prop_oneof![text(80), lines_text(10)], a in any::<prop::sample::Index>(), b in any::<prop::sample::Index>(), which in 0usize..24) {
        use commands::Command::*;
        let (x, y) = edit_on(&t, a, b);
        let mut st = EditorState::new(t);
        st.set_selection(Selection::new(x, y));
        let cmds = [Bold, Italic, Strike, Highlight, Code, CodeBlock, Link, Heading(1), Heading(0), Bullet, Numbered, Task, Quote, Rule, Table, Indent, Outdent];
        let tr = match which {
            n if n < cmds.len() => commands::command(&st, cmds[n]),
            17 => commands::newline(&st),
            18 => commands::delete_backward(&st, None),
            19 => commands::delete_forward(&st, None),
            20 => commands::tab(&st, false),
            21 => commands::tab(&st, true),
            22 => commands::toggle_task(&st, st.doc().line_of(x)),
            _ => commands::type_text(&st, x, y, "é"),
        };
        if let Some(tr) = tr {
            let applied = st.apply(tr);
            prop_assert!(st.text().is_char_boundary(applied.selection.anchor));
            prop_assert!(st.text().is_char_boundary(applied.selection.head));
            let lines: Vec<&str> = st.text().split('\n').collect();
            prop_assert_eq!(st.blocks().to_vec(), syntax::classify(&lines));
        }
    }

    /// Whatever goes in, a browser reads only allowed tags with allowed
    /// attributes, and text, from what comes out (no comments, doctypes or
    /// anything that switches it into raw text), and sanitizing that again
    /// changes nothing.
    #[test]
    fn sanitized_html_has_only_allowed_markup(src in html_soup()) {
        use html5ever::tokenizer::Token::*;
        let out = sanitize(&src);
        for t in tokens(&out) {
            match t {
                TagToken(tag) => {
                    prop_assert!(html::is_allowed(&tag.name, None), "{} in {:?}", &*tag.name, out);
                    for a in &tag.attrs {
                        let key: &str = &a.name.local;
                        prop_assert!(html::is_allowed(&tag.name, Some(key)), "{} in {:?}", key, out);
                    }
                }
                CommentToken(_) | DoctypeToken(_) | NullCharacterToken => {
                    prop_assert!(false, "{:?}", out)
                }
                CharacterTokens(_) | EOFToken | ParseError(_) => {}
            }
        }
        prop_assert_eq!(sanitize(&out), out);
    }

    /// A line the view doesn't redraw after an edit renders as before: same
    /// text, block and HTML preview (an HTML block's preview, on its last
    /// line, shows lines above it).
    #[test]
    fn lines_not_redrawn_render_as_before(
        t in lines_text(24),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<prop::sample::Index>(), prop_oneof![lines_text(3), text(6)]), 1..6),
    ) {
        let mut st = EditorState::new(t);
        let render = |st: &EditorState, i: usize| {
            (st.doc().line(i).to_string(), st.block(i).clone(), st.html_source(i))
        };
        for (a, b, ins) in edits {
            let before: Vec<_> = (0..st.doc().line_count()).map(|i| render(&st, i)).collect();
            let (from, to) = edit_on(st.text(), a, b);
            let change = ChangeSet::replace(st.doc().len(), from, to, &ins);
            let applied = st.apply(Transaction::new(change, Origin::Input));
            let mut shift = 0isize;
            let mut splices = applied.splices.iter().peekable();
            for i in 0..st.doc().line_count() {
                while let Some(sp) = splices.next_if(|sp| sp.new_start + sp.new <= i && !sp.new_range().contains(&i)) {
                    shift += sp.new as isize - sp.old as isize;
                }
                let spliced = applied.splices.iter().any(|sp| sp.new_range().contains(&i));
                let redrawn = applied.redraw.iter().any(|r| r.contains(&i));
                if !spliced && !redrawn {
                    let old = (i as isize - shift) as usize;
                    prop_assert_eq!(&render(&st, i), &before[old], "line {}", i);
                }
            }
        }
    }
}
