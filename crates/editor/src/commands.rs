//! Editing commands (pure): each looks at the state and returns the
//! transaction that does it. The view dispatches them; tests call them
//! directly.

use crate::callout;
use crate::change::{Assoc, ChangeSet};
use crate::color::{Color, Colors, Kind};
use crate::state::{EditorState, Origin, Selection, Transaction};
use crate::syntax::{self, Block, TableKey};
use crate::text::floor_char;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Bold,
    Italic,
    Strike,
    Highlight,
    /// A text color, or the default one.
    TextColor(Option<Color>),
    /// A background color, or none.
    Background(Option<Color>),
    Code,
    CodeBlock,
    Link,
    Heading(u8),
    Bullet,
    Numbered,
    Task,
    Quote,
    /// Make the selected lines a callout of this kind, or give the callout
    /// the caret is in this kind.
    Callout(callout::Kind),
    Rule,
    Table,
    Indent,
    Outdent,
    Undo,
    Redo,
}

/// Replace `[from, to)` with `insert` and put the selection at `caret`
/// (default: after the inserted text). Offsets are clamped onto character
/// boundaries; `None` if nothing would change.
pub fn replace(
    st: &EditorState,
    from: usize,
    to: usize,
    insert: &str,
    caret: Option<(usize, usize)>,
) -> Option<Transaction> {
    let doc = st.doc();
    let to = doc.floor(to);
    let from = doc.floor(from).min(to);
    if from == to && insert.is_empty() {
        return None;
    }
    let (a, h) = caret.unwrap_or((from + insert.len(), from + insert.len()));
    Some(
        Transaction::new(
            ChangeSet::replace(doc.len(), from, to, insert),
            Origin::Input,
        )
        .with_selection(Selection::new(a, h))
        .scrolling(),
    )
}

/// Replace the selection with `text`.
pub fn insert_text(st: &EditorState, text: &str) -> Option<Transaction> {
    let s = st.selection();
    replace(st, s.from(), s.to(), text, None)
}

/// Typing `data` over `[a, b)`. A space or punctuation starts a new undo
/// group, and right after `@today`, `@tomorrow` or `@yesterday` it puts the
/// day's date in the word's place (undo gives the word back).
pub fn type_text(st: &EditorState, a: usize, b: usize, data: &str) -> Option<Transaction> {
    let word_end = data == " " || data.chars().any(|c| ".,;:!?".contains(c));
    let ends_word = data
        .chars()
        .next()
        .is_some_and(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
    if a == b
        && ends_word
        && let Some(today) = st.today
        && let Some((at, days)) = finished_keyword(st, a)
    {
        let date = format!("@{}{data}", today.add_days(days));
        return replace(st, at, b, &date, None).map(|t| t.with_boundary(true, false));
    }
    replace(st, a, b, data, None).map(|t| t.with_boundary(word_end, false))
}

/// Whether byte `p` of line `li` (`line`) is in prose, where a date may be
/// written: not in code, a link, a URL, a tag, raw HTML or syntax.
fn prose_at(st: &EditorState, li: usize, line: &str, p: usize) -> bool {
    let block = st.block(li);
    if matches!(
        block,
        Block::Code { .. } | Block::Fence { .. } | Block::Html { .. }
    ) {
        return false;
    }
    let styled = syntax::style_line(line, block, &|_| true);
    use syntax::{CODE, HTML, LINK, MK, TAG, URL, WIKI};
    styled.inline_from.is_some_and(|f| p >= f)
        && styled
            .flags
            .get(p)
            .is_some_and(|f| f & (CODE | LINK | URL | WIKI | TAG | HTML | MK) == 0)
}

/// The `@…` typed right before `at` on its line, if it may become a date:
/// where its `@` is (in the document), and what follows it.
fn at_word(st: &EditorState, at: usize) -> Option<(usize, String)> {
    let doc = st.doc();
    let li = doc.line_of(at);
    let start = doc.line_start(li);
    let line = doc.line(li);
    let before = line.get(..at - start)?;
    let p = before.rfind('@')?;
    let typed = &before[p + 1..];
    (notekpr_core::dates::starts_date(before[..p].chars().next_back())
        && typed.len() <= 9
        && typed.bytes().all(|b| b.is_ascii_alphabetic())
        && prose_at(st, li, line, p))
    .then(|| (start + p, typed.to_string()))
}

/// `@today`, `@tomorrow` or `@yesterday` ending at `at`: where its `@` is,
/// and how many days from today.
fn finished_keyword(st: &EditorState, at: usize) -> Option<(usize, i64)> {
    let (p, typed) = at_word(st, at)?;
    notekpr_core::dates::KEYWORDS
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(&typed))
        .map(|&(_, days)| (p, days))
}

/// What is typed after an `@` right before the caret that may still become
/// a date (`@`, `@to`, `@tomorrow`): the view offers those days.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateQuery {
    /// Where the `@` is.
    pub at: usize,
    /// The days it may become: the word, and how many days from today.
    pub days: Vec<(&'static str, i64)>,
}

pub fn date_query(st: &EditorState) -> Option<DateQuery> {
    let s = st.selection();
    if !s.is_empty() || st.readonly {
        return None;
    }
    let (at, typed) = at_word(st, s.head)?;
    let days = notekpr_core::dates::keywords_starting(&typed);
    (!days.is_empty()).then_some(DateQuery { at, days })
}

/// Put `day` in place of the `@…` typed from `at` to the caret, with a
/// space after it unless one follows.
pub fn insert_date(
    st: &EditorState,
    at: usize,
    day: notekpr_core::dates::Date,
) -> Option<Transaction> {
    let caret = st.selection().head;
    if caret < at || !st.text()[at..].starts_with('@') {
        return None;
    }
    let space = if st.text()[caret..].starts_with([' ', '\n', '\t']) {
        ""
    } else {
        " "
    };
    replace(st, at, caret, &format!("@{day}{space}"), None).map(|t| t.with_boundary(true, true))
}

/// Pasted (or dropped) text over `[a, b)`: its own undo group. Pasted
/// right after an opening fence that nothing closes, the text goes below the
/// fence and the block is closed after it.
pub fn paste(st: &EditorState, a: usize, b: usize, text: &str) -> Option<Transaction> {
    let text = text.replace("\r\n", "\n");
    let doc = st.doc();
    let li = doc.line_of(a);
    let body = text.trim_start_matches('\n').trim_end_matches('\n');
    if a == b
        && a == doc.line_end(li)
        && !body.trim().is_empty()
        && let Some(close) = closing_fence(st, li)
    {
        let caret = a + 1 + body.len();
        return replace(
            st,
            a,
            b,
            &format!("\n{body}\n{close}"),
            Some((caret, caret)),
        )
        .map(|t| t.with_boundary(true, true));
    }
    replace(st, a, b, &text, None).map(|t| t.with_boundary(true, true))
}

/// The fence that closes the opening fence on line `li` (its indentation,
/// backticks or tildes), if nothing closes it: the note ends inside a code
/// block. (A fence typed above another block pairs with that block's
/// opening fence, which leaves the note ending inside a block too.)
fn closing_fence(st: &EditorState, li: usize) -> Option<String> {
    if !matches!(st.block(li), Block::Fence { open: true, .. }) {
        return None;
    }
    let last = st.doc().line_count().checked_sub(1)?;
    if !matches!(
        st.block(last),
        Block::Code { .. } | Block::Fence { open: true, .. }
    ) {
        return None;
    }
    let line = st.doc().line(li);
    let t = line.trim_start();
    let c = t.chars().next()?;
    let run = t.len() - t.trim_start_matches(c).len();
    Some(format!("{}{}", &line[..line.len() - t.len()], &t[..run]))
}

fn table_key(st: &EditorState, k: TableKey) -> Option<Transaction> {
    match k {
        TableKey::Move(o) => {
            Some(Transaction::select(st.doc().len(), Selection::cursor(o)).scrolling())
        }
        TableKey::Edit {
            from,
            to,
            insert,
            caret,
        } => replace(st, from, to, &insert, Some((caret, caret))),
    }
}

/// Enter on the empty item or quote line `li`: the list or quote ends there.
/// Below other text, the line stays as a blank line and the caret goes to a
/// new one after it: text typed right under a list item or quote would
/// continue it (Markdown's lazy continuation lines).
fn end_block(st: &EditorState, li: usize) -> Option<Transaction> {
    let doc = st.doc();
    let (ls, le) = (doc.line_start(li), doc.line_end(li));
    if li > 0 && !doc.line(li - 1).trim().is_empty() {
        replace(st, ls, le, "\n", Some((ls + 1, ls + 1)))
    } else {
        replace(st, ls, le, "", Some((ls, ls)))
    }
}

/// Enter: continue lists, quotes, code indentation and tables.
pub fn newline(st: &EditorState) -> Option<Transaction> {
    let sel = st.selection();
    let (a, b) = (sel.from(), sel.to());
    let doc = st.doc();
    let li = doc.line_of(a);
    let (ls, le) = (doc.line_start(li), doc.line_end(li));
    let line = doc.line(li);
    let group = |t: Option<Transaction>| t.map(|t| t.with_boundary(true, false));
    // At the end of an opening fence that nothing closes: close the block,
    // the caret on the empty line in between.
    if a == b
        && a == le
        && let Some(close) = closing_fence(st, li)
    {
        let caret = Some((a + 1, a + 1));
        return group(replace(st, a, a, &format!("\n\n{close}"), caret));
    }
    if matches!(st.block(li), Block::Code { .. }) {
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        return group(replace(st, a, b, &format!("\n{indent}"), None));
    }
    if let Some(k) = syntax::table_enter_in(st.syntax_doc(), a) {
        return group(table_key(st, k));
    }
    if let Some((indent, mlen, ordered)) = syntax::list_marker(line) {
        let content_start = indent + mlen;
        let task = syntax::task_box(&line[content_start..]);
        let body_start = content_start
            + if task.is_some() {
                4.min(line.len() - content_start)
            } else {
                0
            };
        if line[body_start..].trim().is_empty() && a >= ls + body_start {
            // Empty item: end the list (or outdent one level).
            if indent > 0 {
                let cut = indent.min(2);
                return group(replace(st, ls, ls + cut, "", Some((a - cut, a - cut))));
            }
            return group(end_block(st, li));
        }
        if a < ls + content_start {
            return group(replace(st, a, b, "\n", None));
        }
        let marker = &line[indent..indent + mlen];
        let next = if ordered {
            let n: u64 = marker
                .trim_end_matches([' ', '.', ')'])
                .parse()
                .unwrap_or(1);
            let sep = if marker.contains(')') { ")" } else { "." };
            format!("{}{sep} ", n + 1)
        } else {
            marker.to_string()
        };
        let task_part = if task.is_some() { "[ ] " } else { "" };
        return group(replace(
            st,
            a,
            b,
            &format!("\n{}{next}{task_part}", &line[..indent]),
            None,
        ));
    }
    if matches!(
        syntax::classify(&[line])[0],
        Block::Quote | Block::Callout(_)
    ) {
        let prefix: String = line
            .chars()
            .take_while(|c| *c == '>' || *c == ' ')
            .collect();
        if line[prefix.len()..].trim().is_empty() {
            return group(end_block(st, li));
        }
        return group(replace(st, a, b, &format!("\n{prefix}"), None));
    }
    group(replace(st, a, b, "\n", None))
}

/// Backspace right after a list marker removes the marker; in an empty quote
/// line, the quote marker. `None`: an ordinary backspace.
pub fn smart_backspace(st: &EditorState) -> Option<Transaction> {
    let sel = st.selection();
    let (a, b) = (sel.from(), sel.to());
    if a != b {
        return None;
    }
    let doc = st.doc();
    let ls = doc.line_start(doc.line_of(a));
    let text = doc.as_str();
    let line_to_caret = text.get(ls..a)?;
    if let Some((indent, mlen, _)) = syntax::list_marker(&text[ls..]) {
        let content_start = indent + mlen;
        let task = syntax::task_box(&text[ls + content_start..])
            .map(|_| 4)
            .unwrap_or(0);
        if line_to_caret.len() == content_start + task {
            return replace(st, ls + indent, a, "", Some((ls + indent, ls + indent)))
                .map(|t| t.with_boundary(true, false));
        }
    }
    if let Some(rest) = line_to_caret.strip_prefix('>')
        && rest.trim().is_empty()
    {
        return replace(st, ls, a, "", Some((ls, ls)));
    }
    None
}

fn prev_char(s: &str, i: usize) -> usize {
    let i = floor_char(s, i);
    s[..i]
        .char_indices()
        .next_back()
        .map(|(p, _)| p)
        .unwrap_or(0)
}

fn next_char(s: &str, i: usize) -> usize {
    let i = floor_char(s, i);
    s[i..]
        .chars()
        .next()
        .map(|c| i + c.len_utf8())
        .unwrap_or(s.len())
}

/// Backspace over the selection, the browser's target range or the
/// character before the caret.
pub fn delete_backward(st: &EditorState, target: Option<(usize, usize)>) -> Option<Transaction> {
    if let Some(t) = smart_backspace(st) {
        return Some(t);
    }
    let s = st.selection();
    let (a, b) = if !s.is_empty() {
        (s.from(), s.to())
    } else {
        target
            .filter(|(x, y)| x != y)
            .unwrap_or_else(|| (prev_char(st.text(), s.from()), s.from()))
    };
    replace(st, a, b, "", None)
}

/// Delete over the selection, the target range or the next character.
pub fn delete_forward(st: &EditorState, target: Option<(usize, usize)>) -> Option<Transaction> {
    let s = st.selection();
    let (a, b) = if !s.is_empty() {
        (s.from(), s.to())
    } else {
        target
            .filter(|(x, y)| x != y)
            .unwrap_or_else(|| (s.from(), next_char(st.text(), s.from())))
    };
    replace(st, a, b, "", None)
}

/// Other deletions (words, lines): the browser's target range.
pub fn delete_range(st: &EditorState, target: Option<(usize, usize)>) -> Option<Transaction> {
    let s = st.selection();
    let (a, b) = if !s.is_empty() {
        (s.from(), s.to())
    } else {
        target.unwrap_or((s.from(), s.to()))
    };
    if a == b {
        return None;
    }
    replace(st, a, b, "", None).map(|t| t.with_boundary(true, false))
}

/// Edits that toggle `open`/`close` around the selection `[a, b)` (or the
/// word at the caret), and the selection afterwards. Never slices `text`
/// inside a character, whatever the offsets.
pub fn wrap_toggle(
    text: &str,
    a: usize,
    b: usize,
    open: &str,
    close: &str,
) -> (ChangeSet, (usize, usize)) {
    let (a, b) = (floor_char(text, a), floor_char(text, b));
    let (a, b) = (a.min(b), a.max(b));
    let (ol, cl) = (open.len(), close.len());
    let len = text.len();
    let wrapped =
        a >= ol && text.get(a - ol..a) == Some(open) && text.get(b..b + cl) == Some(close);
    if wrapped {
        let c = ChangeSet::replace(len, a - ol, a, "").compose(&ChangeSet::replace(
            len - ol,
            b - ol,
            b - ol + cl,
            "",
        ));
        return (c, (a - ol, b - ol));
    }
    if a == b {
        // Wrap the word under the caret, or insert an empty pair.
        let (ws, we) = word_at(text, a);
        let c = if ws < we {
            ChangeSet::replace(len, ws, ws, open).compose(&ChangeSet::replace(
                len + ol,
                we + ol,
                we + ol,
                close,
            ))
        } else {
            ChangeSet::replace(len, a, a, &format!("{open}{close}"))
        };
        return (c, (a + ol, a + ol));
    }
    let c = ChangeSet::replace(len, a, a, open).compose(&ChangeSet::replace(
        len + ol,
        b + ol,
        b + ol,
        close,
    ));
    (c, (a + ol, b + ol))
}

/// The word around `a` (letters, digits, `_` and `-`; empty if none).
fn word_at(text: &str, a: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let ws = text[..a]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(a);
    let we = a + text[a..]
        .chars()
        .take_while(|c| is_word(*c))
        .map(char::len_utf8)
        .sum::<usize>();
    (ws, we)
}

/// Give the selection a text or background color, or with `None` take the
/// color away (see [`crate::color`] for how it's written). Without a
/// selection: the colored text at the caret, or else the word; with
/// neither, an empty pair of tags to type into. Elements that colored the
/// text are cut back or removed, so tags never pile up.
fn set_color(st: &EditorState, kind: Kind, color: Option<Color>) -> Option<Transaction> {
    let doc = st.doc();
    let sel = st.selection();
    let (mut a, mut b) = (sel.from(), sel.to());
    let style = |li: usize| syntax::style_line(doc.line(li), st.block(li), &|_| true);
    let caret = sel.is_empty().then_some(a);
    if sel.is_empty() {
        let li = doc.line_of(a);
        let ls = doc.line_start(li);
        let styled = style(li);
        let p = a - ls;
        styled.inline_from.filter(|f| p >= *f)?;
        let colored = styled
            .elements
            .iter()
            .filter(|e| e.colors.get(kind).is_some() && e.open.1 <= p && p <= e.close.0)
            .max_by_key(|e| e.open.0);
        if let Some(e) = colored {
            if e.open.1 == e.close.0 {
                // An empty one (typed into next): its tags change.
                let colors = e.colors.with(kind, color);
                let (open, close) = match colors.is_empty() {
                    true => (String::new(), String::new()),
                    false => (colors.span(), "</span>".to_string()),
                };
                let at = ls + e.open.0 + open.len();
                let reps = [
                    (ls + e.open.0, ls + e.open.1, open),
                    (ls + e.close.0, ls + e.close.1, close),
                ];
                let c = only_colors(st, &reps).ok()?;
                return Some(
                    Transaction::new(c, Origin::Command)
                        .with_selection(Selection::cursor(at))
                        .scrolling(),
                );
            }
            (a, b) = (ls + e.open.1, ls + e.close.0);
        } else {
            (a, b) = word_at(doc.as_str(), a);
            if a == b {
                // Not inside a tag or an atom, nor after a backslash.
                let html = |i: usize| styled.flags.get(i).is_some_and(|f| f & syntax::HTML != 0);
                let in_tag = p > 0 && html(p - 1) && html(p);
                let in_atom = styled.atoms.iter().any(|&(u, v)| u < p && p < v);
                if in_tag || in_atom || doc.line(li)[..p].ends_with('\\') {
                    return None;
                }
                let open = Colors::default().with(kind, Some(color?)).span();
                let at = a + open.len();
                // On a blank line it's like typing; elsewhere lines are read
                // as before (not before a table's first pipe).
                let pair = [(a, a, format!("{open}</span>"))];
                if *st.block(li) != Block::Blank {
                    only_colors(st, &pair).ok()?;
                }
                return replace(st, a, a, &pair[0].2, Some((at, at)));
            }
        }
    }
    let mut lines = vec![];
    let mut colored: Option<(usize, usize)> = None;
    for li in doc.line_of(a)..=doc.line_of(b) {
        let (ls, line) = (doc.line_start(li), doc.line(li));
        let (x, y) = (a.max(ls) - ls, b.min(ls + line.len()).max(ls) - ls);
        if x >= y {
            continue;
        }
        let Some((edits, (c0, c1))) = color_line(line, &style(li), x, y, kind, color) else {
            continue;
        };
        let edits: Vec<_> = edits
            .into_iter()
            .map(|(f, t, s)| (ls + f, ls + t, s))
            .collect();
        lines.push((li, edits));
        colored = Some((colored.map_or(ls + c0, |c| c.0), ls + c1));
    }
    let (c0, c1) = colored?;
    // Colors change nothing else. Tags taken away could join what they kept
    // apart (two backticks, `\` and `*`), or make the start of a line a list
    // item or a code fence: such a line stays as it is.
    let c = loop {
        let reps: Vec<_> = lines.iter().flat_map(|(_, e)| e.iter().cloned()).collect();
        if reps.is_empty() {
            return None;
        }
        match only_colors(st, &reps) {
            Ok(c) => break c,
            Err(first) => {
                let k = lines.iter().rposition(|(li, _)| *li <= first).unwrap_or(0);
                lines.remove(k);
            }
        }
    };
    let sel = match caret {
        Some(at) => Selection::cursor(c.map_pos(at, Assoc::After)),
        None => Selection::new(c.map_pos(c0, Assoc::After), c.map_pos(c1, Assoc::Before)),
    };
    Some(
        Transaction::new(c, Origin::Command)
            .with_selection(sel)
            .scrolling(),
    )
}

/// The change made of replacements `reps` (in order) if it changes only
/// colors: every line is read as before and shows the same text, each
/// character looking as before but for its colors. Otherwise the first line
/// it changes more.
fn only_colors(st: &EditorState, reps: &[(usize, usize, String)]) -> Result<ChangeSet, usize> {
    let c = ChangeSet::from_replacements(st.doc().len(), reps);
    let text = c.apply(st.text()).ok_or(0usize)?;
    let lines: Vec<&str> = text.split('\n').collect();
    let blocks = syntax::classify(&lines);
    let n = blocks.len().max(st.blocks().len());
    let changed = |i: usize| match (blocks.get(i), st.blocks().get(i)) {
        (Some(b), Some(old)) if b == old => {
            let was = st.doc().line(i);
            lines[i] != was && look(lines[i], b) != look(was, old)
        }
        _ => true,
    };
    match (0..n).find(|&i| changed(i)) {
        None => Ok(c),
        Some(first) => Err(first),
    }
}

/// What `line` shows: its characters outside the tags of its elements, each
/// with its look (but its colors) and its link.
fn look(line: &str, block: &Block) -> Vec<(char, syntax::Flags, Option<syntax::Target>)> {
    let s = syntax::style_line(line, block, &|_| true);
    let mut in_tag = vec![false; line.len()];
    for e in &s.elements {
        in_tag[e.open.0..e.open.1].fill(true);
        in_tag[e.close.0..e.close.1].fill(true);
    }
    line.char_indices()
        .filter(|(i, _)| !in_tag[*i])
        .map(|(i, ch)| {
            let link = s.links.iter().find(|l| l.0 <= i && i < l.1);
            (
                ch,
                s.flags[i] & !(syntax::FG | syntax::BG),
                link.map(|l| l.2.clone()),
            )
        })
        .collect()
}

/// A replacement `(from, to, insert)` in a line.
type LineEdit = (usize, usize, String);

/// The edits that color `[x, y)` of `line` (see [`set_color`]), and where
/// the colored text starts and ends; `None` if none of it can be colored.
///
/// The text is a sequence of characters with the tags of the line's
/// elements between them. Each character has the elements around it; the
/// colored ones get the wanted elements instead: those around them that
/// don't set this kind of color, the new color's element (outside the ones
/// that don't enclose all of the colored run), and what elements setting
/// both kinds of color keep of theirs. The tags between characters are
/// rewritten — closing what ends, opening what starts — only next to
/// colored text, so the rest of the line stays as it was and every element
/// stays properly nested.
fn color_line(
    line: &str,
    st: &syntax::Styled,
    x: usize,
    y: usize,
    kind: Kind,
    color: Option<Color>,
) -> Option<(Vec<LineEdit>, (usize, usize))> {
    let from = st.inline_from?;
    let els = &st.elements;
    let mut in_tag = vec![false; line.len()];
    for e in els {
        in_tag[e.open.0..e.open.1].fill(true);
        in_tag[e.close.0..e.close.1].fill(true);
    }
    let chars: Vec<usize> = line
        .char_indices()
        .map(|(i, _)| i)
        .filter(|&i| !in_tag[i])
        .collect();
    let end_of = |i: usize| i + line[i..].chars().next().map_or(1, char::len_utf8);
    // Not a table's pipes, nor tags that aren't paired (text to the editor,
    // but a browser reads them: tags put around them could pair with them).
    let colorable = |i: usize| i >= from && st.flags[i] & (syntax::PIPE | syntax::HTML) == 0;
    // Runs of selected text, split where text can't be colored, without
    // white space at their ends, and with whole atoms.
    let mut runs: Vec<Vec<usize>> = vec![vec![]];
    for &i in chars.iter().filter(|&&i| x <= i && i < y) {
        if colorable(i) {
            runs.last_mut().unwrap().push(i);
        } else if !runs.last().unwrap().is_empty() {
            runs.push(vec![]);
        }
    }
    let blank = |i: &&usize| line[**i..].starts_with(char::is_whitespace);
    // A line break: a tag after it would be escaped (elsewhere a backslash
    // is an atom with what follows it).
    let backslash = |i: &&usize| &line[**i..] == "\\";
    let mut runs: Vec<(usize, usize)> = runs
        .iter()
        .filter_map(|r| {
            let mut r0 = *r.iter().find(|i| !blank(i))?;
            let mut r1 = end_of(*r.iter().rfind(|i| !blank(i) && !backslash(i))?);
            // Atoms may overlap (`\***`).
            while let Some(&(u, v)) = st
                .atoms
                .iter()
                .find(|&&(u, v)| (u < r0 && r0 < v) || (u < r1 && r1 < v))
            {
                (r0, r1) = (r0.min(u), r1.max(v));
            }
            Some((r0, r1))
        })
        .collect();
    // Atoms may join runs.
    runs.dedup_by(|later, run| {
        let overlap = later.0 <= run.1;
        if overlap {
            run.1 = run.1.max(later.1);
        }
        overlap
    });
    let (first, last) = (runs.first()?.0, runs.last()?.1);
    let run_of = |i: usize| runs.iter().position(|r| r.0 <= i && i < r.1);
    // The elements around a character, outermost first.
    let around = |i: usize| {
        let mut v: Vec<usize> = (0..els.len())
            .filter(|&k| els[k].open.1 <= i && i < els[k].close.0)
            .collect();
        v.sort_by_key(|&k| els[k].open.0);
        v
    };
    let sets = |k: usize| els[k].colors.get(kind).is_some();
    // Per run: the elements that stay around all of it.
    let outer: Vec<Vec<usize>> = runs
        .iter()
        .map(|&(r0, r1)| {
            around(r0)
                .into_iter()
                .filter(|&k| !sets(k) && els[k].open.1 <= r0 && r1 <= els[k].close.0)
                .collect()
        })
        .collect();
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Item {
        /// An element as it is.
        Same(usize),
        /// An element that sets both kinds of color, keeping the other one.
        Other(usize),
        New,
    }
    let wanted = |i: usize| -> Vec<Item> {
        let els_around = around(i);
        let Some(r) = run_of(i) else {
            return els_around.into_iter().map(Item::Same).collect();
        };
        let (keep, inner): (Vec<usize>, Vec<usize>) =
            els_around.into_iter().partition(|k| outer[r].contains(k));
        let mut v: Vec<Item> = keep.into_iter().map(Item::Same).collect();
        if color.is_some() {
            v.push(Item::New);
        }
        for k in inner {
            if !sets(k) {
                v.push(Item::Same(k));
            } else if !els[k].colors.with(kind, None).is_empty() {
                v.push(Item::Other(k));
            }
        }
        v
    };
    let open = |it: Item| match it {
        Item::Same(k) => line[els[k].open.0..els[k].open.1].to_string(),
        Item::Other(k) => els[k].colors.with(kind, None).span(),
        Item::New => Colors::default().with(kind, color).span(),
    };
    let close = |it: Item| match it {
        Item::Same(k) => line[els[k].close.0..els[k].close.1].to_string(),
        Item::Other(_) | Item::New => "</span>".to_string(),
    };
    let mut edits = vec![];
    let mut prev: Option<usize> = None;
    for k in 0..=chars.len() {
        let next = chars.get(k).copied();
        if prev.and_then(run_of).is_some() || next.and_then(run_of).is_some() {
            let was = prev.map_or(vec![], wanted);
            let will = next.map_or(vec![], wanted);
            let same = was.iter().zip(&will).take_while(|(p, q)| p == q).count();
            let (g0, g1) = (prev.map_or(0, end_of), next.unwrap_or(line.len()));
            let mut tags: String = was[same..].iter().rev().map(|&it| close(it)).collect();
            // Elements without text stay, between what closes and what opens
            // (unless they hold a tag of another one: misnested).
            let whole = |e: &&syntax::Element| {
                let inside = |r: (usize, usize)| e.open.0 <= r.0 && r.1 <= e.close.1;
                els.iter().all(|f| inside(f.open) == inside(f.close))
            };
            let mut i = g0;
            while let Some(e) = els
                .iter()
                .filter(|e| e.open.0 >= i && e.close.1 <= g1)
                .filter(whole)
                .min_by_key(|e| (e.open.0, std::cmp::Reverse(e.close.1)))
            {
                tags.push_str(&line[e.open.0..e.close.1]);
                i = e.close.1;
            }
            tags.extend(will[same..].iter().map(|&it| open(it)));
            if tags != line[g0..g1] {
                edits.push((g0, g1, tags));
            }
        }
        prev = next;
    }
    Some((edits, (first, last)))
}

fn toggle_wrap(st: &EditorState, open: &str, close: &str) -> Option<Transaction> {
    let s = st.selection();
    let (c, (x, y)) = wrap_toggle(st.text(), s.from(), s.to(), open, close);
    Some(
        Transaction::new(c, Origin::Command)
            .with_selection(Selection::new(x, y))
            .scrolling(),
    )
}

/// The first line of the callout line `li` is in (the innermost one), if
/// it is in one.
fn callout_start(st: &EditorState, li: usize) -> Option<usize> {
    let Block::Callout(c) = st.block(li) else {
        return None;
    };
    let key = (c.kind, c.outer);
    (0..=li)
        .rev()
        .take_while(|&i| matches!(st.block(i), Block::Callout(_)))
        .find(|&i| matches!(st.block(i), Block::Callout(d) if d.title && (d.kind, d.outer) == key))
}

/// [`Command::Callout`]. In a callout, its type changes (in the case it was
/// written in: GitHub writes `[!NOTE]`), its title and folding stay.
/// Elsewhere the lines of the selection get quote markers below a first
/// line with the type, which shows as its title; a quote whose first line
/// is selected only gets that line. On an empty line the caret goes on the
/// callout's next line, ready for its text.
fn make_callout(st: &EditorState, kind: callout::Kind) -> Option<Transaction> {
    let doc = st.doc();
    let sel = st.selection();
    let (l0, l1) = (doc.line_of(sel.from()), doc.line_of(sel.to()));
    if let Some(t) = callout_start(st, l0) {
        let line = doc.line(t);
        let (_, at) = syntax::quote_prefix(line);
        let h = callout::header(&line[at..])?;
        if h.kind == kind {
            return None;
        }
        let caps =
            h.name.chars().any(char::is_uppercase) && !h.name.chars().any(char::is_lowercase);
        let name = if caps {
            kind.name().to_uppercase()
        } else {
            kind.name().to_string()
        };
        let from = doc.line_start(t) + at + 2;
        let changes = ChangeSet::replace(doc.len(), from, from + h.name.len(), &name);
        let sel = sel.map(&changes, Assoc::After);
        return Some(
            Transaction::new(changes, Origin::Command)
                .with_selection(sel)
                .with_boundary(true, false),
        );
    }
    let head = format!("> [!{}]", kind.name());
    let (s0, e1) = (doc.line_start(l0), doc.line_end(l1));
    if l0 == l1 && doc.slice(s0, e1).trim().is_empty() {
        let new = format!("{head}\n> ");
        let caret = s0 + new.len();
        return replace(st, s0, e1, &new, Some((caret, caret)))
            .map(|t| t.with_boundary(true, false));
    }
    let quoted = |i: usize| doc.line(i).trim_start().starts_with('>');
    let wrap = !((l0..=l1).all(quoted) && (l0 == 0 || !quoted(l0 - 1)));
    let reps: Vec<(usize, usize, String)> = (l0..=l1)
        .filter_map(|i| {
            let marker = if !wrap {
                ""
            } else if doc.line(i).trim().is_empty() {
                ">"
            } else {
                "> "
            };
            let ins = if i == l0 {
                format!("{head}\n{marker}")
            } else {
                marker.to_string()
            };
            let ls = doc.line_start(i);
            (!ins.is_empty()).then_some((ls, ls, ins))
        })
        .collect();
    let changes = ChangeSet::from_replacements(doc.len(), &reps);
    // The selected text stays selected.
    let sel = sel.map(&changes, Assoc::After);
    Some(
        Transaction::new(changes, Origin::Command)
            .with_selection(sel)
            .scrolling()
            .with_boundary(true, false),
    )
}

/// Apply `f` to every line touched by the selection.
fn map_lines(st: &EditorState, f: impl Fn(&str, usize) -> String) -> Option<Transaction> {
    let s = st.selection();
    let doc = st.doc();
    let (l0, l1) = (doc.line_of(s.from()), doc.line_of(s.to()));
    let (s0, e1) = (doc.line_start(l0), doc.line_end(l1));
    let old = doc.as_str().get(s0..e1)?;
    let new = old
        .split('\n')
        .enumerate()
        .map(|(i, l)| f(l, l0 + i))
        .collect::<Vec<_>>()
        .join("\n");
    if new == old {
        return None;
    }
    let delta = new.len() as isize - old.len() as isize;
    let caret = if l0 == l1 {
        ((s.to() as isize + delta).max(s0 as isize)) as usize
    } else {
        s0 + new.len()
    };
    replace(st, s0, e1, &new, Some((caret, caret))).map(|t| t.with_boundary(true, false))
}

fn strip_prefix(line: &str) -> (&str, &str) {
    if let Some((lvl, _)) = notekpr_core::markdown::heading(line) {
        let cut = line.find('#').unwrap_or(0) + lvl as usize;
        let cut = cut + line[cut..].chars().take_while(|c| *c == ' ').count();
        return (&line[..0], &line[cut..]);
    }
    if let Some((indent, mlen, _)) = syntax::list_marker(line) {
        let mut cut = indent + mlen;
        if syntax::task_box(&line[cut..]).is_some() {
            cut = (cut + 4).min(line.len());
        }
        return (&line[..indent], &line[cut..]);
    }
    if let Some(r) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
        return ("", r);
    }
    ("", line)
}

/// A toolbar or shortcut command (undo and redo are the host's).
pub fn command(st: &EditorState, cmd: Command) -> Option<Transaction> {
    let s = st.selection();
    let (a, b) = (s.from(), s.to());
    let tr = match cmd {
        Command::Bold => toggle_wrap(st, "**", "**"),
        Command::Italic => toggle_wrap(st, "*", "*"),
        Command::Strike => toggle_wrap(st, "~~", "~~"),
        Command::Highlight => toggle_wrap(st, "==", "=="),
        Command::TextColor(c) => set_color(st, Kind::Text, c),
        Command::Background(c) => set_color(st, Kind::Background, c),
        Command::Code => toggle_wrap(st, "`", "`"),
        Command::Link => {
            let sel = st.doc().slice(a, b).to_string();
            if sel.starts_with("http://") || sel.starts_with("https://") {
                replace(st, a, b, &format!("[]({sel})"), Some((a + 1, a + 1)))
            } else if sel.is_empty() {
                replace(st, a, b, "[[]]", Some((a + 2, a + 2)))
            } else {
                replace(
                    st,
                    a,
                    b,
                    &format!("[{sel}](https://)"),
                    Some((a + sel.len() + 3, a + sel.len() + 11)),
                )
            }
        }
        Command::CodeBlock => {
            let sel = st.doc().slice(a, b).to_string();
            let ls = st.doc().line_start(st.doc().line_of(a));
            let lead = if a > ls { "\n" } else { "" };
            let ins = format!("{lead}```\n{sel}\n```\n");
            let caret = a + lead.len() + 3;
            replace(st, a, b, &ins, Some((caret, caret)))
        }
        Command::Heading(n) => map_lines(st, |l, _| {
            let (_, body) = strip_prefix(l);
            let cur = notekpr_core::markdown::heading(l).map(|h| h.0);
            if n == 0 || cur == Some(n) {
                body.to_string()
            } else {
                format!("{} {body}", "#".repeat(n as usize))
            }
        }),
        Command::Bullet => map_lines(st, |l, _| {
            let (indent, body) = strip_prefix(l);
            let plain_bullet = syntax::list_marker(l)
                .is_some_and(|m| !m.2 && syntax::task_box(&l[m.0 + 2..]).is_none());
            if plain_bullet {
                format!("{indent}{body}")
            } else {
                format!("{indent}- {body}")
            }
        }),
        Command::Numbered => {
            let first = st.doc().line_of(a);
            map_lines(st, move |l, i| {
                let (indent, body) = strip_prefix(l);
                if syntax::list_marker(l).is_some_and(|m| m.2) {
                    format!("{indent}{body}")
                } else {
                    format!("{indent}{}. {body}", i - first + 1)
                }
            })
        }
        Command::Task => map_lines(st, |l, _| {
            let (indent, body) = strip_prefix(l);
            let is_task = syntax::list_marker(l)
                .is_some_and(|(i, m, _)| syntax::task_box(&l[i + m..]).is_some());
            if is_task {
                format!("{indent}{body}")
            } else {
                format!("{indent}- [ ] {body}")
            }
        }),
        Command::Quote => map_lines(st, |l, _| {
            if let Some(r) = l.strip_prefix("> ").or_else(|| l.strip_prefix('>')) {
                r.to_string()
            } else {
                format!("> {l}")
            }
        }),
        Command::Callout(kind) => make_callout(st, kind),
        Command::Indent => map_lines(st, |l, _| format!("  {l}")),
        Command::Outdent => map_lines(st, |l, _| {
            l.strip_prefix("  ")
                .or_else(|| l.strip_prefix('\t'))
                .or_else(|| l.strip_prefix(' '))
                .unwrap_or(l)
                .to_string()
        }),
        Command::Rule => replace(st, a, b, "\n\n---\n\n", None),
        Command::Table => {
            let t = "\n| Column 1 | Column 2 |\n| --- | --- |\n|  |  |\n";
            replace(st, a, b, t, Some((a + 2, a + 10)))
        }
        Command::Undo | Command::Redo => None,
    };
    tr.map(|mut t| {
        t.origin = Origin::Command;
        t
    })
}

/// Tab / Shift+Tab: table navigation, list indentation, or a tab character.
pub fn tab(st: &EditorState, back: bool) -> Option<Transaction> {
    let a = st.selection().from();
    if let Some(k) = syntax::table_tab_in(st.syntax_doc(), a, back) {
        return table_key(st, k);
    }
    let line = st.doc().line(st.doc().line_of(a));
    if syntax::list_marker(line).is_some() || back {
        command(
            st,
            if back {
                Command::Outdent
            } else {
                Command::Indent
            },
        )
    } else {
        insert_text(st, "\t")
    }
}

/// Tick or untick the task box of line `idx` (the selection stays).
pub fn toggle_task(st: &EditorState, idx: usize) -> Option<Transaction> {
    if idx >= st.doc().line_count() {
        return None;
    }
    let (start, line) = (st.doc().line_start(idx), st.doc().line(idx));
    let (indent, mlen, _) = syntax::list_marker(line)?;
    let at = start + indent + mlen + 1;
    let mark = match syntax::task_box(&line[indent + mlen..])? {
        true => " ",
        false => "x",
    };
    let sel = st.selection();
    replace(st, at, at + 1, mark, Some(sel.pair())).map(|mut t| {
        t.origin = Origin::Command;
        t.with_boundary(true, false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        text: &str,
        sel: (usize, usize),
        f: impl Fn(&EditorState) -> Option<Transaction>,
    ) -> (String, (usize, usize)) {
        let mut st = EditorState::new(text.to_string());
        st.set_selection(Selection::new(sel.0, sel.1));
        if let Some(t) = f(&st) {
            st.apply(t);
        }
        (st.text().to_string(), st.selection().pair())
    }

    #[test]
    fn typed_day_words_become_dates() {
        let today = notekpr_core::dates::Date::parse("2026-10-23").unwrap();
        let typing = |text: &str, data: &'static str| {
            let at = text.len();
            let mut st = EditorState::new(text.to_string());
            st.today = Some(today);
            st.set_selection(Selection::cursor(at));
            if let Some(t) = type_text(&st, at, at, data) {
                st.apply(t);
            }
            st.text().to_string()
        };
        assert_eq!(typing("Call @today", " "), "Call @2026-10-23 ");
        assert_eq!(typing("(@Tomorrow", ")"), "(@2026-10-24)");
        assert_eq!(
            typing("- [ ] since @yesterday", "."),
            "- [ ] since @2026-10-22."
        );
        // Still a word, inside a word, in code, a URL or a heading's syntax.
        for (text, data) in [
            ("@today", "s"),
            ("me@today", " "),
            ("`@today", " "),
            ("```\n@today", " "),
            ("https://x.org/@today", " "),
            ("@todays", " "),
        ] {
            assert_eq!(typing(text, data), format!("{text}{data}"), "{text}");
        }
        // Without a day (no view yet): text.
        let mut st = EditorState::new("@today".into());
        st.set_selection(Selection::cursor(6));
        let t = type_text(&st, 6, 6, " ").unwrap();
        st.apply(t);
        assert_eq!(st.text(), "@today ");
    }

    #[test]
    fn the_days_an_at_may_become() {
        let query = |text: &str| {
            let mut st = EditorState::new(text.to_string());
            st.set_selection(Selection::cursor(text.len()));
            date_query(&st).map(|q| (q.at, q.days.iter().map(|(w, _)| *w).collect::<Vec<_>>()))
        };
        assert_eq!(
            query("Due @"),
            Some((4, vec!["today", "tomorrow", "yesterday"]))
        );
        assert_eq!(query("Due @To"), Some((4, vec!["today", "tomorrow"])));
        assert_eq!(query("@y"), Some((0, vec!["yesterday"])));
        for none in ["Due @x", "a@to", "`@to", "@to day", "@2026-10-23 "] {
            assert_eq!(query(none), None, "{none}");
        }
        // Picked: the date, a space after it unless one follows.
        let day = notekpr_core::dates::Date::parse("2026-10-24").unwrap();
        assert_eq!(
            run("Due @tom", (8, 8), |st| insert_date(st, 4, day)),
            ("Due @2026-10-24 ".into(), (16, 16))
        );
        assert_eq!(
            run("Due @tom then", (8, 8), |st| insert_date(st, 4, day)),
            ("Due @2026-10-24 then".into(), (15, 15))
        );
    }

    #[test]
    fn enter_closes_a_code_block_nothing_closes() {
        // A new block: closed, the caret inside.
        assert_eq!(
            run("```py", (5, 5), newline),
            ("```py\n\n```".into(), (6, 6))
        );
        // Text below it was never meant to be code.
        assert_eq!(
            run("```py\nhello", (5, 5), newline),
            ("```py\n\n```\nhello".into(), (6, 6))
        );
        // The same fence (and indentation) closes it.
        assert_eq!(run("  ~~~~", (6, 6), newline).0, "  ~~~~\n\n  ~~~~");
        // Typed above another block, a fence pairs with that block's: closed.
        assert_eq!(
            run("```\n```\nx\n```", (3, 3), newline).0,
            "```\n\n```\n```\nx\n```"
        );
        // A closed block: Enter is Enter.
        assert_eq!(run("```py\nx\n```", (5, 5), newline).0, "```py\n\nx\n```");
        // Not at the end of the fence: Enter is Enter.
        assert_eq!(run("```py", (3, 3), newline).0, "```\npy");
    }

    #[test]
    fn pasting_after_an_open_fence_closes_the_block() {
        let paste_at = |text: &str, at: usize, t: &'static str| {
            run(text, (at, at), move |st| paste(st, at, at, t))
        };
        assert_eq!(
            paste_at("```sh", 5, "ls -la\r\npwd\n"),
            ("```sh\nls -la\npwd\n```".into(), (16, 16))
        );
        // A closed block, or elsewhere: pasted as it is.
        assert_eq!(paste_at("```sh\n```", 5, "ls").0, "```shls\n```");
        assert_eq!(paste_at("text", 4, "\nls").0, "text\nls");
    }

    #[test]
    fn enter_continues_and_ends_lists() {
        assert_eq!(
            run("- milk", (6, 6), newline),
            ("- milk\n- ".into(), (9, 9))
        );
        // Ending the list leaves a blank line: text right under an item
        // would continue it.
        assert_eq!(
            run("- milk\n- ", (9, 9), newline),
            ("- milk\n\n".into(), (8, 8))
        );
        assert_eq!(run("- ", (2, 2), newline), ("".into(), (0, 0)));
        assert_eq!(
            run("> said\n> ", (9, 9), newline),
            ("> said\n\n".into(), (8, 8))
        );
        assert_eq!(
            run("1. one", (6, 6), newline),
            ("1. one\n2. ".into(), (10, 10))
        );
        assert_eq!(
            run("- [x] done", (10, 10), newline),
            ("- [x] done\n- [ ] ".into(), (17, 17))
        );
        assert_eq!(
            run("  - sub\n  - ", (12, 12), newline),
            ("  - sub\n- ".into(), (10, 10))
        );
        assert_eq!(
            run("> quote", (7, 7), newline),
            ("> quote\n> ".into(), (10, 10))
        );
        assert_eq!(
            run("```\n  code\n```", (10, 10), newline),
            ("```\n  code\n  \n```".into(), (13, 13))
        );
        assert_eq!(run("plain", (2, 2), newline), ("pl\nain".into(), (3, 3)));
        // A callout continues from its first line and ends like a quote.
        assert_eq!(
            run("> [!tip] Title", (14, 14), newline),
            ("> [!tip] Title\n> ".into(), (17, 17))
        );
        assert_eq!(
            run("> [!tip]\n> a\n> ", (15, 15), newline),
            ("> [!tip]\n> a\n\n".into(), (14, 14))
        );
    }

    #[test]
    fn callouts_from_the_toolbar() {
        use callout::Kind;
        let make = |kind| move |st: &EditorState| command(st, Command::Callout(kind));
        // On an empty line: the callout, the caret on its next line.
        assert_eq!(
            run("a\n\nb", (2, 2), make(Kind::Tip)),
            ("a\n> [!tip]\n> \nb".into(), (13, 13))
        );
        // Selected lines become its text, still selected; blank lines stay in it.
        assert_eq!(
            run("x\none\n\ntwo\ny", (3, 9), make(Kind::Warning)),
            ("x\n> [!warning]\n> one\n>\n> two\ny".into(), (18, 27))
        );
        // A caret in a line: that line.
        assert_eq!(
            run("text", (2, 2), make(Kind::Note)),
            ("> [!note]\n> text".into(), (14, 14))
        );
        // A quote's first line gets the type, the quote isn't nested.
        assert_eq!(
            run("> said\n> more", (3, 3), make(Kind::Quote)).0,
            "> [!quote]\n> said\n> more"
        );
        // In a callout: another type, the title, folding and case kept.
        assert_eq!(
            run("> [!NOTE]- Title\n> text", (20, 20), make(Kind::Caution)),
            ("> [!CAUTION]- Title\n> text".into(), (23, 23))
        );
        assert_eq!(
            run("> [!faq]\n> why", (12, 12), make(Kind::Bug)).0,
            "> [!bug]\n> why"
        );
        // The same type: nothing to do.
        assert_eq!(
            run("> [!info]\n> x", (11, 11), make(Kind::Info)).0,
            "> [!info]\n> x"
        );
        // In a nested one: that one.
        assert_eq!(
            run(
                "> [!note]\n> > [!tip]\n> > x\n> y",
                (23, 23),
                make(Kind::Danger)
            )
            .0,
            "> [!note]\n> > [!danger]\n> > x\n> y"
        );
    }

    #[test]
    fn backspace_after_a_marker_removes_it() {
        assert_eq!(
            run("- ", (2, 2), |s| delete_backward(s, None)),
            ("".into(), (0, 0))
        );
        assert_eq!(
            run("- [ ] ", (6, 6), |s| delete_backward(s, None)),
            ("".into(), (0, 0))
        );
        assert_eq!(
            run("ab日", (5, 5), |s| delete_backward(s, None)),
            ("ab".into(), (2, 2))
        );
        assert_eq!(
            run("ab日", (0, 0), |s| delete_forward(s, None)),
            ("b日".into(), (0, 0))
        );
    }

    #[test]
    fn tables_by_keys() {
        let t = "| A | B |\n| --- | --- |\n| a1 | b1 |";
        let (out, sel) = run(t, (2, 2), |s| tab(s, false));
        assert_eq!((out.as_str(), sel), (t, (6, 6)));
        let (out, _) = run(t, (t.len() - 2, t.len() - 2), |s| tab(s, false));
        assert_eq!(out, format!("{t}\n|  |  |"));
        let (out, _) = run(t, (2, 2), newline);
        assert_eq!(out, "| A | B |\n| --- | --- |\n|  |  |\n| a1 | b1 |");
    }

    #[test]
    fn line_commands() {
        let (out, _) = run("a\nb", (0, 3), |s| command(s, Command::Bullet));
        assert_eq!(out, "- a\n- b");
        let (out, _) = run("- a\n- b", (0, 7), |s| command(s, Command::Numbered));
        assert_eq!(out, "1. a\n2. b");
        let (out, _) = run("title", (2, 2), |s| command(s, Command::Heading(2)));
        assert_eq!(out, "## title");
        let (out, _) = run("## title", (4, 4), |s| command(s, Command::Heading(2)));
        assert_eq!(out, "title");
        let (out, sel) = run("x 日本 y", (2, 8), |s| command(s, Command::Bold));
        assert_eq!((out.as_str(), sel), ("x **日本** y", (4, 10)));
        let (out, _) = run("- [ ] a\n- [x] b", (0, 0), |s| toggle_task(s, 1));
        assert_eq!(out, "- [ ] a\n- [ ] b");
        let (out, _) = run("- [ ] a", (0, 0), |s| toggle_task(s, 0));
        assert_eq!(out, "- [x] a");
    }

    #[test]
    fn floor_char_stays_on_boundaries() {
        let s = "a日b";
        assert_eq!(floor_char(s, 0), 0);
        assert_eq!(floor_char(s, 2), 1);
        assert_eq!(floor_char(s, 3), 1);
        assert_eq!(floor_char(s, 4), 4);
        assert_eq!(floor_char(s, 99), s.len());
        assert_eq!(prev_char(s, 3), 0);
        assert_eq!(next_char(s, 2), 4);
    }

    #[test]
    fn formatting_next_to_multibyte_text_never_panics() {
        let samples = [
            "日本語",
            "**日**",
            "a日*",
            "日~~x~~本",
            "é`ü`ö",
            "👍==👍==",
            "",
        ];
        let pairs = [
            ("**", "**"),
            ("*", "*"),
            ("~~", "~~"),
            ("==", "=="),
            ("`", "`"),
        ];
        for t in samples {
            for a in 0..=t.len() + 2 {
                for b in 0..=t.len() + 2 {
                    for (open, close) in pairs {
                        let (c, (x, y)) = wrap_toggle(t, a, b, open, close);
                        let out = c.apply(t).unwrap();
                        assert!(out.is_char_boundary(x.min(out.len())), "{t:?} {a} {b}");
                        assert!(out.is_char_boundary(y.min(out.len())), "{t:?} {a} {b}");
                    }
                }
            }
        }
    }

    const R: &str = "<span style=\"color: #e44045\">";
    const B: &str = "<span style=\"color: #3a7bdf\">";
    const Y: &str = "<span style=\"background-color: #ffd0004d\">";

    fn text_color(c: Option<Color>) -> impl Fn(&EditorState) -> Option<Transaction> {
        move |st| command(st, Command::TextColor(c))
    }

    fn background(c: Option<Color>) -> impl Fn(&EditorState) -> Option<Transaction> {
        move |st| command(st, Command::Background(c))
    }

    #[test]
    fn colors_the_selection_and_changes_or_removes_it() {
        let (red, blue, plain) = (
            text_color(Some(Color::Red)),
            text_color(Some(Color::Blue)),
            text_color(None),
        );
        let t = "a big word";
        let (out, sel) = run(t, (2, 5), &red);
        assert_eq!(out, format!("a {R}big</span> word"));
        assert_eq!(&out[sel.0..sel.1], "big");
        // The same color again changes nothing; another one changes the tag.
        assert_eq!(run(&out, sel, &red).0, out);
        let (blued, sel) = run(&out, sel, &blue);
        assert_eq!(blued, format!("a {B}big</span> word"));
        assert_eq!(run(&blued, sel, &plain).0, t);
        // With its tags selected too; white space at the ends stays out.
        assert_eq!(run(&out, (0, out.len()), &plain).0, t);
        assert_eq!(run(t, (1, 6), &red).0, format!("a {R}big</span> word"));
        // Text and background colors are separate.
        let (both, _) = run(&out, sel, background(Some(Color::Yellow)));
        assert_eq!(both, format!("a {R}{Y}big</span></span> word"));
        assert_eq!(
            run(&both, (0, both.len()), &plain).0,
            format!("a {Y}big</span> word")
        );
    }

    #[test]
    fn colors_part_of_colored_text() {
        let (blue, plain) = (text_color(Some(Color::Blue)), text_color(None));
        let t = format!("{R}abcde</span>");
        let o = R.len();
        assert_eq!(
            run(&t, (o + 2, o + 3), &blue).0,
            format!("{R}ab</span>{B}c</span>{R}de</span>")
        );
        assert_eq!(
            run(&t, (o + 2, o + 3), &plain).0,
            format!("{R}ab</span>c{R}de</span>")
        );
        assert_eq!(run(&t, (o, o + 2), &plain).0, format!("ab{R}cde</span>"));
        assert_eq!(
            run(&t, (o + 3, o + 5), &plain).0,
            format!("{R}abc</span>de")
        );
        // Over the end of it.
        let t = format!("{R}abc</span>def");
        let d = t.find('d').unwrap();
        assert_eq!(
            run(&t, (o + 2, d + 1), &blue).0,
            format!("{R}ab</span>{B}cd</span>ef")
        );
        // Over several: one element.
        let t = format!("{R}a</span> b {B}c</span>");
        assert_eq!(run(&t, (0, t.len()), &blue).0, format!("{B}a b c</span>"));
        // An element that sets both colors keeps the other one.
        let both = "<span style=\"color: red; background: yellow\">";
        let t = format!("{both}abc</span>");
        let o = both.len();
        assert_eq!(
            run(&t, (o + 1, o + 2), &plain).0,
            format!("{both}a</span>{Y}b</span>{both}c</span>")
        );
        assert_eq!(run(&t, (o, o + 3), &plain).0, format!("{Y}abc</span>"));
    }

    #[test]
    fn other_elements_stay_nested() {
        let red = text_color(Some(Color::Red));
        assert_eq!(
            run("<b>abc</b>def", (5, 11), &red).0,
            format!("<b>ab</b>{R}<b>c</b>d</span>ef")
        );
        assert_eq!(
            run("<b>abc</b>", (4, 5), &red).0,
            format!("<b>a{R}b</span>c</b>")
        );
        assert_eq!(
            run(
                &format!("{R}<b>abc</b></span>"),
                (R.len() + 4, R.len() + 5),
                text_color(None)
            )
            .0,
            format!("{R}<b>a</b></span><b>b</b>{R}<b>c</b></span>")
        );
    }

    #[test]
    fn the_caret_colors_its_colored_text_or_word() {
        let (red, blue) = (text_color(Some(Color::Red)), text_color(Some(Color::Blue)));
        // Other editors' tags count.
        let t = "<font color=\"red\">xy</font> z";
        let (out, sel) = run(t, (19, 19), &blue);
        assert_eq!(out, format!("{B}xy</span> z"));
        assert_eq!(sel, (B.len() + 1, B.len() + 1));
        let t = "<mark style=\"background: #FFB86CA6;\">x</mark>";
        assert_eq!(
            run(t, (37, 37), background(Some(Color::Green))).0,
            "<span style=\"background-color: #30a46c4d\">x</span>"
        );
        let (out, sel) = run("one two", (5, 5), &red);
        assert_eq!(out, format!("one {R}two</span>"));
        assert_eq!(sel, (5 + R.len(), 5 + R.len()));
        // No word: a pair to type into, which takes another color or goes.
        let (out, sel) = run("a  b", (2, 2), &red);
        assert_eq!(out, format!("a {R}</span> b"));
        assert_eq!(sel, (2 + R.len(), 2 + R.len()));
        let (out, sel) = run(&out, sel, &blue);
        assert_eq!(out, format!("a {B}</span> b"));
        assert_eq!(run(&out, sel, text_color(None)).0, "a  b");
        assert_eq!(run("a  b", (2, 2), text_color(None)).0, "a  b");
    }

    #[test]
    fn colors_keep_markdown_working() {
        let red = text_color(Some(Color::Red));
        let t = "# Title\n- item\n\n| a | b |\n| --- | --- |\n| c | d |\n```\ncode\n```";
        assert_eq!(
            run(t, (0, t.len()), &red).0,
            format!(
                "# {R}Title</span>\n- {R}item</span>\n\n| {R}a</span> | {R}b</span> |\n\
                 | --- | --- |\n| {R}c</span> | {R}d</span> |\n```\ncode\n```"
            )
        );
        // Code spans, links and the like are colored whole.
        assert_eq!(
            run("a `code` [[Note]] b", (4, 12), &red).0,
            format!("a {R}`code` [[Note]]</span> b")
        );
        assert_eq!(
            run("**bold**", (3, 5), &red).0,
            format!("**b{R}ol</span>d**")
        );
        // Whole lines of everyday Markdown take the color.
        for (line, want) in [
            (
                "Visit https://example.org today",
                format!("{R}Visit https://example.org today</span>"),
            ),
            (
                "See [[Note]], `code` and #tag",
                format!("{R}See [[Note]], `code` and #tag</span>"),
            ),
            (
                "**bold** and *it* \\*",
                format!("{R}**bold** and *it* \\*</span>"),
            ),
            ("- [ ] #tag first", format!("- [ ] {R}#tag first</span>")),
            ("> [a](b) &amp; c", format!("> {R}[a](b) &amp; c</span>")),
        ] {
            assert_eq!(run(line, (0, line.len()), &red).0, want, "{line}");
        }
    }

    /// Lines the property tests once found to break this: colors change
    /// only tags, never what a line shows or how lines are read.
    #[test]
    fn colors_leave_odd_lines_alone() {
        let shown = |st: &EditorState| -> Vec<String> {
            (0..st.doc().line_count())
                .map(|i| {
                    let line = st.doc().line(i);
                    let s = syntax::style_line(line, st.block(i), &|_| true);
                    let mut tag = vec![false; line.len()];
                    for e in &s.elements {
                        tag[e.open.0..e.open.1].fill(true);
                        tag[e.close.0..e.close.1].fill(true);
                    }
                    line.char_indices()
                        .filter(|(i, _)| !tag[*i])
                        .map(|(_, c)| c)
                        .collect()
                })
                .collect()
        };
        for t in [
            // An unpaired tag is text to the editor.
            "# <span style=\"color: #e44045\">",
            // An empty element holds a line's start.
            "<span style=\"color: #e44045\"></span><!-- c -->",
            // Misnested.
            "<mark style=\"background: gold\">**<b></mark></b>",
            // Two backticks would become a code span around a tag.
            "\tx\n**\n    ``<mark style=\"background: gold\"></span>word||",
            // A tag after a backslash would be escaped.
            "</span>**</span>\\*",
            "# \\***word</span></span>word",
            "a\\",
            // Not before a table's first pipe.
            "| ",
            // Taking tags away would make two backticks one run.
            "<span style=\"color: #e44045\">`<span style=\"color: #e44045\">\
             <span style=\"color: #e44045\">`<span style=\"color: #e44045\">`</span>",
        ] {
            let before = EditorState::new(t.to_string());
            let bounds: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
            // Every selection of short lines; of long ones, those at an end.
            let some = |x: usize, y: usize| t.len() < 60 || x == 0 || y == t.len() || x == y;
            for (k, &x) in bounds.iter().enumerate() {
                for &y in bounds[k..].iter().filter(|&&y| some(x, y)) {
                    for cmd in [
                        Command::TextColor(Some(Color::Gray)),
                        Command::TextColor(None),
                        Command::Background(Some(Color::Red)),
                    ] {
                        let mut st = EditorState::new(t.to_string());
                        st.set_selection(Selection::new(x, y));
                        if let Some(tr) = command(&st, cmd) {
                            st.apply(tr);
                        }
                        let what = format!("{t:?} {x}..{y} {cmd:?} → {:?}", st.text());
                        assert_eq!(st.blocks(), before.blocks(), "{what}");
                        assert_eq!(shown(&st), shown(&before), "{what}");
                    }
                }
            }
        }
    }

    #[test]
    fn wrap_toggle_wraps_and_unwraps() {
        let apply = |t: &str, c: &ChangeSet| c.apply(t).unwrap();
        let (c, sel) = wrap_toggle("x 日本 y", 2, 8, "**", "**");
        assert_eq!(apply("x 日本 y", &c), "x **日本** y");
        assert_eq!(sel, (4, 10));
        let (c, sel) = wrap_toggle("x **日本** y", 4, 10, "**", "**");
        assert_eq!(apply("x **日本** y", &c), "x 日本 y");
        assert_eq!(sel, (2, 8));
        // Caret inside a word wraps the word.
        let (c, _) = wrap_toggle("über", 2, 2, "*", "*");
        assert_eq!(apply("über", &c), "*über*");
        // Caret between spaces inserts an empty pair.
        let (c, sel) = wrap_toggle("日  本", 4, 4, "`", "`");
        assert_eq!(apply("日  本", &c), "日 `` 本");
        assert_eq!(sel, (5, 5));
    }
}
