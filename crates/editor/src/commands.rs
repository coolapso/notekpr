//! Editing commands (pure): each looks at the state and returns the
//! transaction that does it. The view dispatches them; tests call them
//! directly.

use crate::change::ChangeSet;
use crate::state::{EditorState, Origin, Selection, Transaction};
use crate::syntax::{self, Block, TableKey};
use crate::text::floor_char;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Bold,
    Italic,
    Strike,
    Highlight,
    Code,
    CodeBlock,
    Link,
    Heading(u8),
    Bullet,
    Numbered,
    Task,
    Quote,
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

/// Typing `data` over `[a, b)`. A space or punctuation starts a new undo group.
pub fn type_text(st: &EditorState, a: usize, b: usize, data: &str) -> Option<Transaction> {
    let word_end = data == " " || data.chars().any(|c| ".,;:!?".contains(c));
    replace(st, a, b, data, None).map(|t| t.with_boundary(word_end, false))
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
            return group(replace(st, ls, le, "", Some((ls, ls))));
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
    if syntax::classify(&[line])[0] == Block::Quote {
        let prefix: String = line
            .chars()
            .take_while(|c| *c == '>' || *c == ' ')
            .collect();
        if line[prefix.len()..].trim().is_empty() {
            return group(replace(st, ls, le, "", Some((ls, ls))));
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

fn toggle_wrap(st: &EditorState, open: &str, close: &str) -> Option<Transaction> {
    let s = st.selection();
    let (c, (x, y)) = wrap_toggle(st.text(), s.from(), s.to(), open, close);
    Some(
        Transaction::new(c, Origin::Command)
            .with_selection(Selection::new(x, y))
            .scrolling(),
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
        assert_eq!(
            run("- milk\n- ", (9, 9), newline),
            ("- milk\n".into(), (7, 7))
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
