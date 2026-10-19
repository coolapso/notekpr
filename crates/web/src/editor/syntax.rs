//! Line-oriented Markdown styling for the live editor.
//!
//! Every character of the source is rendered (so DOM text always equals the
//! Markdown source and caret mapping stays exact); syntax characters get the
//! `mk` class, which CSS hides on lines the caret is not on. That gives an
//! Obsidian-style live preview without ever replacing text with widgets.
//!
//! Tables work the same way: each row is a line whose cells are wrapped in
//! spans that CSS lays out as table rows and cells, so columns line up while
//! the text stays the Markdown source.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Blank,
    Paragraph,
    Heading(u8),
    Quote,
    List {
        ordered: bool,
        task: Option<bool>,
    },
    Fence {
        lang: String,
    },
    Code {
        lang: String,
    },
    Hr,
    /// A row of a GFM table (header, delimiter row, body); `align` holds the
    /// alignment of every column, from the delimiter row.
    Table {
        row: Row,
        align: Vec<Align>,
    },
    /// Lines starting with `|` that don't form a table (no delimiter row).
    PipeText,
    FrontmatterFence,
    Frontmatter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Header,
    Delimiter,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

impl Align {
    fn class(self) -> &'static str {
        match self {
            Align::None | Align::Left => "td",
            Align::Center => "td ac",
            Align::Right => "td ar",
        }
    }
}

impl Block {
    pub fn class(&self) -> &'static str {
        match self {
            Block::Blank => "ln blank",
            Block::Paragraph => "ln",
            Block::Heading(1) => "ln h1",
            Block::Heading(2) => "ln h2",
            Block::Heading(3) => "ln h3",
            Block::Heading(4) => "ln h4",
            Block::Heading(5) => "ln h5",
            Block::Heading(_) => "ln h6",
            Block::Quote => "ln quote",
            Block::List {
                task: Some(true), ..
            } => "ln li task done",
            Block::List {
                task: Some(false), ..
            } => "ln li task",
            Block::List { ordered: true, .. } => "ln li ol",
            Block::List { .. } => "ln li",
            Block::Fence { .. } => "ln fence",
            Block::Code { .. } => "ln code",
            Block::Hr => "ln hr",
            Block::Table {
                row: Row::Header, ..
            } => "ln table th",
            Block::Table {
                row: Row::Delimiter,
                ..
            } => "ln table tdl",
            Block::Table { row: Row::Body, .. } => "ln table",
            Block::PipeText => "ln pipes",
            Block::FrontmatterFence => "ln fm fmf",
            Block::Frontmatter => "ln fm",
        }
    }
}

fn fence_of(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    for f in ["```", "~~~"] {
        if let Some(rest) = t.strip_prefix(f) {
            let ticks = f.chars().next().unwrap();
            let rest = rest.trim_start_matches(ticks);
            return Some((f, rest.trim()));
        }
    }
    None
}

fn is_hr(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

/// `(indent_bytes, marker_len_incl_space, ordered)` for list items.
pub fn list_marker(line: &str) -> Option<(usize, usize, bool)> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let b = rest.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' {
        return Some((indent, 2, false));
    }
    let digits = rest.bytes().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && b.len() > digits + 1
        && matches!(b[digits], b'.' | b')')
        && b[digits + 1] == b' '
    {
        return Some((indent, digits + 2, true));
    }
    None
}

/// `Some(checked)` if the list item text starts with a task box.
pub fn task_box(after_marker: &str) -> Option<bool> {
    let b = after_marker.as_bytes();
    if b.len() >= 3 && b[0] == b'[' && b[2] == b']' && (b.len() == 3 || b[3] == b' ') {
        match b[1] {
            b' ' => Some(false),
            b'x' | b'X' => Some(true),
            _ => None,
        }
    } else {
        None
    }
}

/// The line with its task box ticked or unticked: the box right after the
/// list marker (also inside block quotes), never a `[ ]` later in the text.
/// `None` if the line is not a task item.
pub fn toggle_task(line: &str) -> Option<String> {
    let mut p = 0;
    // Block quote markers: up to three spaces, `>`, an optional space.
    loop {
        let rest = &line[p..];
        let sp = rest.len() - rest.trim_start_matches(' ').len();
        if sp > 3 || !rest[sp..].starts_with('>') {
            break;
        }
        p += sp + 1;
        if line[p..].starts_with(' ') {
            p += 1;
        }
    }
    let (indent, mlen, _) = list_marker(&line[p..])?;
    let at = p + indent + mlen;
    let checked = task_box(&line[at..])?;
    let mut out = line.to_string();
    out.replace_range(at + 1..at + 2, if checked { " " } else { "x" });
    Some(out)
}

/// Byte offsets of the `|` that separate table cells (escaped `\|` don't).
pub fn table_pipes(line: &str) -> Vec<usize> {
    let b = line.as_bytes();
    (0..b.len())
        .filter(|&i| b[i] == b'|' && (i == 0 || b[i - 1] != b'\\'))
        .collect()
}

/// Byte ranges of a table row's cells, each starting at its leading `|`; the
/// first also covers leading whitespace and the last any trailing `|`.
pub fn table_cells(line: &str) -> Vec<(usize, usize)> {
    let pipes = table_pipes(line);
    let Some(&last) = pipes.last() else {
        return vec![];
    };
    let trailing_text = !line[last + 1..].trim().is_empty();
    let n = if trailing_text {
        pipes.len()
    } else {
        pipes.len() - 1
    }
    .max(1);
    (0..n)
        .map(|k| {
            let a = if k == 0 { 0 } else { pipes[k] };
            let b = if k + 1 < n { pipes[k + 1] } else { line.len() };
            (a, b)
        })
        .collect()
}

/// Text of each cell of a table row (trimmed, without the pipes).
pub fn table_cell_texts(line: &str) -> Vec<&str> {
    table_cells(line)
        .into_iter()
        .map(|(a, b)| {
            let c = line[a..b].trim();
            let c = c.strip_prefix('|').unwrap_or(c);
            let c = if c.ends_with('|') && !c.ends_with("\\|") {
                &c[..c.len() - 1]
            } else {
                c
            };
            c.trim()
        })
        .collect()
}

/// Column alignments if `line` is a table delimiter row (`| --- | :-: |`).
fn delimiter_row(line: &str) -> Option<Vec<Align>> {
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
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                (true, false) => Align::Left,
                (false, false) => Align::None,
            })
        })
        .collect()
}

fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// What Enter or Tab does inside a table.
#[derive(Debug, PartialEq, Eq)]
pub enum TableKey {
    Move(usize),
    Edit {
        from: usize,
        to: usize,
        insert: String,
        caret: usize,
    },
}

struct TableCtx<'a> {
    lines: Vec<&'a str>,
    starts: Vec<usize>,
    blocks: Vec<Block>,
    idx: usize,
}

/// The table row containing `offset`, if any.
fn table_ctx(text: &str, offset: usize) -> Option<TableCtx<'_>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let idx = match starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let blocks = classify(&lines);
    matches!(blocks.get(idx), Some(Block::Table { .. })).then_some(TableCtx {
        lines,
        starts,
        blocks,
        idx,
    })
}

/// Caret position inside a cell: after its pipe and one space.
fn cell_caret(line: &str, (a, _): (usize, usize)) -> usize {
    let p = line[a..].find('|').map(|x| a + x + 1).unwrap_or(a);
    if line[p..].starts_with(' ') { p + 1 } else { p }
}

fn new_row(cols: usize) -> String {
    format!("\n|{}", "  |".repeat(cols.max(1)))
}

/// Enter in a table: add an empty row below (below the delimiter row when on
/// the header) and move into its first cell. On an empty row, end the table:
/// the row becomes a blank line (Markdown would otherwise continue the table
/// with the next line) and the caret moves below it.
pub fn table_enter(text: &str, offset: usize) -> Option<TableKey> {
    let c = table_ctx(text, offset)?;
    let Block::Table { row, align } = &c.blocks[c.idx] else {
        return None;
    };
    let (ls, line) = (c.starts[c.idx], c.lines[c.idx]);
    if *row == Row::Body && table_cell_texts(line).iter().all(|t| t.is_empty()) {
        return Some(TableKey::Edit {
            from: ls,
            to: ls + line.len(),
            insert: "\n".into(),
            caret: ls + 1,
        });
    }
    let after = if *row == Row::Header {
        c.idx + 1
    } else {
        c.idx
    };
    let at = c.starts[after] + c.lines[after].len();
    Some(TableKey::Edit {
        from: at,
        to: at,
        insert: new_row(align.len()),
        caret: at + 3,
    })
}

/// Tab / Shift+Tab in a table: next / previous cell, continuing on the next or
/// previous row. Tab in the last cell of the last row adds a row.
pub fn table_tab(text: &str, offset: usize, back: bool) -> Option<TableKey> {
    let c = table_ctx(text, offset)?;
    let Block::Table { align, .. } = &c.blocks[c.idx] else {
        return None;
    };
    let (ls, line) = (c.starts[c.idx], c.lines[c.idx]);
    let cells = table_cells(line);
    let rel = offset - ls;
    let k = cells
        .iter()
        .position(|&(a, b)| rel >= a && rel < b)
        .unwrap_or(cells.len().saturating_sub(1));
    let row_at = |i: usize| match c.blocks.get(i) {
        Some(Block::Table { row, .. }) => Some(*row),
        _ => None,
    };
    let caret_in = |i: usize, last: bool| {
        let l = c.lines[i];
        let cs = table_cells(l);
        let cell = if last { cs.last() } else { cs.first() };
        TableKey::Move(c.starts[i] + cell.map(|&x| cell_caret(l, x)).unwrap_or(0))
    };
    if !back {
        if k + 1 < cells.len() {
            return Some(TableKey::Move(ls + cell_caret(line, cells[k + 1])));
        }
        let mut n = c.idx + 1;
        if row_at(n) == Some(Row::Delimiter) {
            n += 1;
        }
        if row_at(n) == Some(Row::Body) {
            return Some(caret_in(n, false));
        }
        let at = ls + line.len();
        return Some(TableKey::Edit {
            from: at,
            to: at,
            insert: new_row(align.len()),
            caret: at + 3,
        });
    }
    if k > 0 {
        return Some(TableKey::Move(ls + cell_caret(line, cells[k - 1])));
    }
    let mut p = c.idx.checked_sub(1)?;
    if row_at(p) == Some(Row::Delimiter) {
        p = p.checked_sub(1)?;
    }
    match row_at(p) {
        Some(Row::Header | Row::Body) => Some(caret_in(p, true)),
        _ => Some(TableKey::Move(offset)),
    }
}

pub fn classify(lines: &[&str]) -> Vec<Block> {
    let mut out = Vec::with_capacity(lines.len());
    let mut fence: Option<(String, String)> = None;
    let mut in_fm = lines.first().is_some_and(|l| l.trim_end() == "---");
    // Remaining rows of a table already classified.
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
        if let Some((f, lang)) = &fence {
            if let Some((g, rest)) = fence_of(line)
                && g == f
                && rest.is_empty()
            {
                out.push(Block::Fence { lang: lang.clone() });
                fence = None;
                continue;
            }
            out.push(Block::Code { lang: lang.clone() });
            continue;
        }
        if let Some((f, lang)) = fence_of(line) {
            fence = Some((f.to_string(), lang.to_string()));
            out.push(Block::Fence {
                lang: lang.to_string(),
            });
            continue;
        }
        if line.trim().is_empty() {
            out.push(Block::Blank);
        } else if let Some((lvl, _)) = coolpsonotes_core::markdown::heading(line) {
            out.push(Block::Heading(lvl));
        } else if is_hr(line) && !line.trim_start().starts_with("- ") {
            out.push(Block::Hr);
        } else if line.trim_start().starts_with('>') {
            out.push(Block::Quote);
        } else if let Some((indent, len, ordered)) = list_marker(line) {
            out.push(Block::List {
                ordered,
                task: task_box(&line[indent + len..]),
            });
        } else if is_table_line(line) {
            // A table: consecutive `|` lines whose second is a delimiter row
            // with as many cells as the header.
            let end = (i..lines.len())
                .find(|&j| !is_table_line(lines[j]))
                .unwrap_or(lines.len());
            let align = (i + 1 < end)
                .then(|| delimiter_row(lines[i + 1]))
                .flatten()
                .filter(|a| a.len() == table_cell_texts(line).len());
            match align {
                Some(align) => {
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
                }
                None => {
                    out.push(Block::PipeText);
                    queued.extend((i + 1..end).map(|_| Block::PipeText));
                }
            }
        } else {
            out.push(Block::Paragraph);
        }
    }
    out
}

// Per-byte style flags.
pub const MK: u32 = 1; // hideable syntax
pub const STRONG: u32 = 1 << 1;
pub const EM: u32 = 1 << 2;
pub const STRIKE: u32 = 1 << 3;
pub const HL: u32 = 1 << 4;
pub const CODE: u32 = 1 << 5;
pub const LINK: u32 = 1 << 6;
pub const URL: u32 = 1 << 7;
pub const WIKI: u32 = 1 << 8;
pub const BROKEN: u32 = 1 << 9;
pub const TAG: u32 = 1 << 10;
pub const TASK: u32 = 1 << 11;
pub const BULLET: u32 = 1 << 12;
pub const PIPE: u32 = 1 << 13;
pub const KW: u32 = 1 << 14;
pub const STR: u32 = 1 << 15;
pub const COMMENT: u32 = 1 << 16;
pub const NUM: u32 = 1 << 17;
pub const OLNUM: u32 = 1 << 18;
pub const HIDE_ALL: u32 = 1 << 19; // marker hidden even as part of a task item
pub const LANG: u32 = 1 << 20;
const PROTECT: u32 = 1 << 31;

pub fn classes(flags: u32) -> String {
    let mut c = Vec::new();
    for (bit, name) in [
        (MK, "mk"),
        (STRONG, "b"),
        (EM, "i"),
        (STRIKE, "s"),
        (HL, "hl"),
        (CODE, "c"),
        (LINK, "a"),
        (URL, "u"),
        (WIKI, "wl"),
        (BROKEN, "wlb"),
        (TAG, "tg"),
        (TASK, "tk"),
        (BULLET, "bl"),
        (PIPE, "tp"),
        (KW, "kw"),
        (STR, "st"),
        (COMMENT, "cm"),
        (NUM, "nu"),
        (OLNUM, "on"),
        (HIDE_ALL, "hx"),
        (LANG, "lg"),
    ] {
        if flags & bit != 0 {
            c.push(name);
        }
    }
    c.join(" ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Wiki(String),
    Url(String),
}

#[derive(Clone, Debug, Default)]
pub struct Styled {
    /// One entry per byte of the line.
    pub flags: Vec<u32>,
    /// Clickable ranges (byte offsets in the line).
    pub links: Vec<(usize, usize, Target)>,
    /// An image embed that fills the whole line (rendered as a preview widget).
    pub image: Option<String>,
    /// Table cells: byte range and CSS class (with the column's alignment).
    pub cells: Vec<(usize, usize, &'static str)>,
}

fn set(flags: &mut [u32], a: usize, b: usize, f: u32) {
    let n = flags.len();
    for x in &mut flags[a.min(n)..b.min(n).max(a.min(n))] {
        *x |= f;
    }
}

fn free(flags: &[u32], a: usize, b: usize) -> bool {
    flags[a..b.min(flags.len())]
        .iter()
        .all(|f| f & (PROTECT | MK) == 0)
}

/// Style one line. `resolves(target)` reports whether a wiki link exists.
pub fn style_line(line: &str, block: &Block, resolves: &dyn Fn(&str) -> bool) -> Styled {
    let n = line.len();
    let mut s = Styled {
        flags: vec![0; n],
        ..Default::default()
    };
    match block {
        Block::Code { lang } => {
            highlight_code(line, lang, &mut s.flags);
            return s;
        }
        Block::Fence { lang } => {
            let lang_at = line.find(lang.as_str()).filter(|_| !lang.is_empty());
            set(&mut s.flags, 0, n, MK);
            if let Some(i) = lang_at {
                for f in &mut s.flags[i..i + lang.len()] {
                    *f = LANG;
                }
            }
            return s;
        }
        Block::FrontmatterFence | Block::Frontmatter => return s,
        Block::Hr => {
            set(&mut s.flags, 0, n, MK);
            return s;
        }
        _ => {}
    }
    let mut start = 0;
    // Block prefixes.
    match block {
        Block::Heading(_) => {
            let t = line.trim_start_matches(' ');
            let lead = n - t.len();
            let hashes = t.bytes().take_while(|b| *b == b'#').count();
            let mut end = lead + hashes;
            while end < n && line.as_bytes()[end] == b' ' {
                end += 1;
            }
            set(&mut s.flags, 0, end, MK | PROTECT);
            start = end;
        }
        Block::Quote => {
            let mut i = 0;
            let b = line.as_bytes();
            while i < n && (b[i] == b'>' || b[i] == b' ') {
                i += 1;
            }
            set(&mut s.flags, 0, i, MK | PROTECT);
            start = i;
        }
        Block::List { ordered, task } => {
            if let Some((indent, len, _)) = list_marker(line) {
                let m = indent + len;
                if *ordered {
                    set(&mut s.flags, indent, m, OLNUM | PROTECT);
                } else if task.is_some() {
                    set(&mut s.flags, indent, m, HIDE_ALL | PROTECT);
                } else {
                    set(&mut s.flags, indent, m, BULLET | PROTECT);
                }
                start = m;
                if task.is_some() {
                    let e = (m + 4).min(n);
                    set(&mut s.flags, m, e, TASK | PROTECT);
                    start = e;
                }
            }
        }
        Block::Table { row, align } => {
            for p in table_pipes(line) {
                s.flags[p] |= MK | PIPE | PROTECT;
            }
            if *row == Row::Delimiter {
                set(&mut s.flags, 0, n, MK | PROTECT);
            }
            s.cells = table_cells(line)
                .into_iter()
                .enumerate()
                .map(|(k, (a, b))| (a, b, align.get(k).copied().unwrap_or(Align::None).class()))
                .collect();
        }
        Block::PipeText => {
            for (i, c) in line.char_indices() {
                if c == '|' {
                    s.flags[i] |= PIPE;
                }
            }
        }
        _ => {}
    }
    inline(line, start, &mut s, resolves);
    // A line that is only an image embed gets a preview widget.
    let body = line[start..].trim();
    if let Some(t) = body.strip_prefix("![[").and_then(|r| r.strip_suffix("]]")) {
        let t = t.split('|').next().unwrap_or(t);
        s.image = Some(t.to_string());
    } else if body.starts_with("![")
        && body.ends_with(')')
        && let Some(i) = body.find("](")
    {
        let target = &body[i + 2..body.len() - 1];
        if coolpsonotes_core::markdown::is_internal_target(target) {
            s.image = Some(coolpsonotes_core::markdown::percent_decode(target));
        }
    }
    for f in &mut s.flags {
        *f &= !PROTECT;
    }
    s
}

fn inline(line: &str, from: usize, s: &mut Styled, resolves: &dyn Fn(&str) -> bool) {
    let b = line.as_bytes();
    let n = b.len();
    // Escapes.
    let mut i = from;
    while i + 1 < n {
        if b[i] == b'\\' && b[i + 1].is_ascii_punctuation() {
            s.flags[i] |= MK | PROTECT;
            s.flags[i + 1] |= PROTECT;
            i += 2;
        } else {
            i += 1;
        }
    }
    // Code spans.
    let mut i = from;
    while i < n {
        if b[i] == b'`' && s.flags[i] & PROTECT == 0 {
            let run = b[i..].iter().take_while(|c| **c == b'`').count();
            let mut j = i + run;
            let mut close = None;
            while j < n {
                if b[j] == b'`' {
                    let r = b[j..].iter().take_while(|c| **c == b'`').count();
                    if r == run {
                        close = Some(j);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            if let Some(c) = close {
                set(&mut s.flags, i, i + run, MK | PROTECT);
                set(&mut s.flags, i + run, c, CODE | PROTECT);
                set(&mut s.flags, c, c + run, MK | PROTECT);
                i = c + run;
                continue;
            }
            i += run;
            continue;
        }
        i += 1;
    }
    // Wiki links and embeds.
    let mut i = from;
    while i + 1 < n {
        if b[i] == b'['
            && b[i + 1] == b'['
            && s.flags[i] & PROTECT == 0
            && let Some(rel) = line[i + 2..].find("]]")
        {
            let inner = &line[i + 2..i + 2 + rel];
            if !inner.is_empty() && !inner.contains('[') {
                let open = if i > from && b[i - 1] == b'!' {
                    i - 1
                } else {
                    i
                };
                let close = i + 2 + rel;
                let (target, alias_at) = match inner.find('|') {
                    Some(p) => (&inner[..p], Some(i + 2 + p)),
                    None => (inner, None),
                };
                let tname = target.split('#').next().unwrap_or(target).trim();
                let ok = tname.is_empty() || resolves(tname);
                set(&mut s.flags, open, i + 2, MK | PROTECT);
                let vis_start = match alias_at {
                    Some(p) => {
                        set(&mut s.flags, i + 2, p + 1, MK | PROTECT);
                        p + 1
                    }
                    None => i + 2,
                };
                set(
                    &mut s.flags,
                    vis_start,
                    close,
                    WIKI | PROTECT | if ok { 0 } else { BROKEN },
                );
                set(&mut s.flags, close, close + 2, MK | PROTECT);
                s.links
                    .push((vis_start, close, Target::Wiki(target.trim().to_string())));
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
    // Markdown links [text](url) and images ![alt](url).
    let mut i = from;
    while i < n {
        if b[i] == b'['
            && s.flags[i] & PROTECT == 0
            && let Some(close_rel) = line[i + 1..].find("](")
        {
            let text_end = i + 1 + close_rel;
            if let Some(paren_rel) = line[text_end + 2..].find(')') {
                let url_end = text_end + 2 + paren_rel;
                if free(&s.flags, i, url_end + 1) {
                    let open = if i > from && b[i - 1] == b'!' {
                        i - 1
                    } else {
                        i
                    };
                    let url = line[text_end + 2..url_end].trim().to_string();
                    set(&mut s.flags, open, i + 1, MK | PROTECT);
                    set(&mut s.flags, i + 1, text_end, LINK);
                    set(&mut s.flags, text_end, url_end + 1, MK | PROTECT);
                    let target = if coolpsonotes_core::markdown::is_internal_target(&url) {
                        Target::Wiki(coolpsonotes_core::markdown::percent_decode(&url))
                    } else {
                        Target::Url(url)
                    };
                    s.links.push((i + 1, text_end, target));
                    i = url_end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    // Bare URLs.
    let mut i = from;
    while i < n {
        if (b[i..].starts_with(b"https://") || b[i..].starts_with(b"http://"))
            && s.flags[i] & (PROTECT | MK) == 0
        {
            let end = line[i..]
                .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
                .map(|e| i + e)
                .unwrap_or(n);
            let mut end = end;
            while end > i && matches!(b[end - 1], b'.' | b',' | b')' | b';' | b':' | b'!' | b'?') {
                end -= 1;
            }
            set(&mut s.flags, i, end, URL | PROTECT);
            s.links
                .push((i, end, Target::Url(line[i..end].to_string())));
            i = end;
            continue;
        }
        i += 1;
    }
    // Emphasis-like delimiters, longest first.
    for (delim, flag, intraword) in [
        ("**", STRONG, true),
        ("__", STRONG, false),
        ("~~", STRIKE, true),
        ("==", HL, true),
        ("*", EM, true),
        ("_", EM, false),
    ] {
        let d = delim.len();
        let mut i = from;
        while i + d <= n {
            if &b[i..i + d] == delim.as_bytes() && free(&s.flags, i, i + d) {
                let after = line[i + d..].chars().next();
                let before = line[..i].chars().next_back();
                let opens = after.is_some_and(|c| !c.is_whitespace())
                    && (intraword || before.is_none_or(|c| !c.is_alphanumeric()))
                    && !(d == 1 && after == delim.chars().next());
                if opens {
                    let mut j = i + d + 1;
                    let mut found = None;
                    while j + d <= n {
                        if &b[j..j + d] == delim.as_bytes() && free(&s.flags, j, j + d) {
                            let prev = line[..j].chars().next_back();
                            let next = line[j + d..].chars().next();
                            let closes = prev.is_some_and(|c| !c.is_whitespace())
                                && (intraword || next.is_none_or(|c| !c.is_alphanumeric()))
                                && !(d == 1 && next == delim.chars().next());
                            if closes {
                                found = Some(j);
                                break;
                            }
                        }
                        j += 1;
                    }
                    if let Some(j) = found {
                        set(&mut s.flags, i, i + d, MK);
                        set(&mut s.flags, i + d, j, flag);
                        set(&mut s.flags, j, j + d, MK);
                        i = j + d;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    // Tags.
    let chars: Vec<(usize, char)> = line.char_indices().filter(|(i, _)| *i >= from).collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if c != '#' || s.flags[i] & (PROTECT | MK) != 0 {
            continue;
        }
        let prev_ok =
            i == from || k == 0 || chars[k - 1].1.is_whitespace() || chars[k - 1].1 == '(';
        if !prev_ok {
            continue;
        }
        let len: usize = line[i + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
            .map(char::len_utf8)
            .sum();
        if len > 0 && !line[i + 1..i + 1 + len].chars().all(|c| c.is_ascii_digit()) {
            set(&mut s.flags, i, i + 1 + len, TAG);
        }
    }
}

fn keywords(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "from",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "interface",
            "let",
            "new",
            "null",
            "of",
            "return",
            "static",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "yield",
        ],
        "py" | "python" => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "True", "try", "while", "with", "yield",
        ],
        "go" | "golang" => &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "fallthrough",
            "false",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "nil",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "true",
            "type",
            "var",
        ],
        "sh" | "bash" | "shell" | "zsh" | "console" => &[
            "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
            "in", "local", "return", "then", "until", "while", "echo", "cd", "sudo",
        ],
        "sql" => &[
            "select",
            "from",
            "where",
            "insert",
            "into",
            "values",
            "update",
            "set",
            "delete",
            "create",
            "table",
            "index",
            "join",
            "left",
            "right",
            "inner",
            "outer",
            "on",
            "and",
            "or",
            "not",
            "null",
            "group",
            "by",
            "order",
            "limit",
            "as",
            "primary",
            "key",
            "references",
            "SELECT",
            "FROM",
            "WHERE",
            "INSERT",
            "INTO",
            "VALUES",
            "UPDATE",
            "SET",
            "DELETE",
            "CREATE",
            "TABLE",
            "JOIN",
            "ON",
            "AND",
            "OR",
            "NOT",
            "NULL",
            "GROUP",
            "BY",
            "ORDER",
            "LIMIT",
            "AS",
        ],
        "c" | "cpp" | "c++" | "h" | "java" | "kotlin" | "cs" | "csharp" | "swift" => &[
            "auto",
            "bool",
            "break",
            "case",
            "char",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extern",
            "false",
            "final",
            "float",
            "for",
            "if",
            "import",
            "int",
            "long",
            "namespace",
            "new",
            "null",
            "nullptr",
            "private",
            "protected",
            "public",
            "return",
            "short",
            "static",
            "struct",
            "switch",
            "this",
            "true",
            "typedef",
            "using",
            "var",
            "void",
            "while",
            "fun",
            "val",
            "let",
            "func",
        ],
        "yaml" | "yml" | "toml" | "json" | "ini" => &["true", "false", "null", "yes", "no"],
        _ => &[],
    }
}

fn comment_prefixes(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "py" | "python" | "sh" | "bash" | "shell" | "zsh" | "yaml" | "yml" | "toml" | "ruby"
        | "rb" | "ini" | "dockerfile" | "make" | "makefile" | "r" | "console" => &["#"],
        "sql" | "lua" | "haskell" | "hs" => &["--"],
        "" | "text" | "txt" | "md" | "markdown" => &[],
        _ => &["//"],
    }
}

fn highlight_code(line: &str, lang: &str, flags: &mut [u32]) {
    let kws = keywords(lang);
    let comments = comment_prefixes(lang);
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if comments.iter().any(|c| line[i..].starts_with(c)) {
            set(flags, i, n, COMMENT);
            return;
        }
        let c = b[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            let mut j = i + 1;
            while j < n && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            set(flags, i, end, STR);
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
        {
            let mut j = i;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            set(flags, i, j, NUM);
            i = j;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if kws.contains(&&line[i..j]) {
                set(flags, i, j, KW);
            }
            i = j;
            continue;
        }
        // Skip multi-byte characters whole.
        i += line[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
}

/// Escape text for insertion as HTML.
pub fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Render a styled line to inner HTML. The text content of the result equals `line`.
pub fn line_html(line: &str, st: &Styled) -> String {
    if line.is_empty() {
        return "<br>".to_string();
    }
    let mut out = String::with_capacity(line.len() * 2);
    let link_at = |i: usize| st.links.iter().position(|(a, b, _)| i >= *a && i < *b);
    let cell_at = |i: usize| st.cells.iter().find(|c| c.0 == i);
    let mut i = 0;
    while i < line.len() {
        // Table cells wrap the runs inside them.
        if let Some((_, _, class)) = cell_at(i) {
            if i > 0 {
                out.push_str("</span>");
            }
            out.push_str("<span class=\"");
            out.push_str(class);
            out.push_str("\">");
        }
        let f = st.flags[i];
        let l = link_at(i);
        let mut j = i + line[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        while j < line.len() && st.flags[j] == f && link_at(j) == l && cell_at(j).is_none() {
            j += line[j..].chars().next().map(char::len_utf8).unwrap_or(1);
        }
        let text = &line[i..j];
        if f == 0 && l.is_none() {
            escape(text, &mut out);
        } else {
            out.push_str("<span class=\"");
            out.push_str(&classes(f));
            out.push('"');
            if let Some(k) = l {
                out.push_str(" data-link=\"");
                out.push_str(&k.to_string());
                out.push('"');
            }
            out.push('>');
            escape(text, &mut out);
            out.push_str("</span>");
        }
        i = j;
    }
    if !st.cells.is_empty() {
        out.push_str("</span>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_the_task_box_after_the_marker() {
        let cases = [
            ("- [x] buy [ ] boxes", Some("- [ ] buy [ ] boxes")),
            ("- [ ] buy [x] boxes", Some("- [x] buy [x] boxes")),
            ("1. [X] done", Some("1. [ ] done")),
            ("  * [ ] nested", Some("  * [x] nested")),
            ("> - [ ] quoted", Some("> - [x] quoted")),
            ("> > 2) [x] deep", Some("> > 2) [ ] deep")),
            ("- [ ] 日本 [x]", Some("- [x] 日本 [x]")),
            ("- [ ]", Some("- [x]")),
            ("text with [ ] box", None),
            ("- not a [ ] task", None),
            ("-[ ] no space", None),
            ("", None),
            (">", None),
        ];
        for (line, want) in cases {
            assert_eq!(toggle_task(line).as_deref(), want, "{line:?}");
        }
    }

    fn flags_of(line: &str) -> Styled {
        let b = classify(&[line]);
        style_line(line, &b[0], &|t| t == "Exists")
    }

    #[test]
    fn blocks() {
        let lines = [
            "# T",
            "",
            "- a",
            "- [x] done",
            "1. one",
            "```rust",
            "let x = 1;",
            "```",
            "> q",
            "---",
            "| a | b |",
        ];
        let k = classify(&lines);
        assert_eq!(k[0], Block::Heading(1));
        assert_eq!(k[1], Block::Blank);
        assert_eq!(
            k[2],
            Block::List {
                ordered: false,
                task: None
            }
        );
        assert_eq!(
            k[3],
            Block::List {
                ordered: false,
                task: Some(true)
            }
        );
        assert_eq!(
            k[4],
            Block::List {
                ordered: true,
                task: None
            }
        );
        assert!(matches!(k[5], Block::Fence { .. }));
        assert!(matches!(k[6], Block::Code { .. }));
        assert!(matches!(k[7], Block::Fence { .. }));
        assert_eq!(k[8], Block::Quote);
        assert_eq!(k[9], Block::Hr);
        assert_eq!(k[10], Block::PipeText);
    }

    #[test]
    fn tables() {
        let lines = [
            "| Name | Qty | Note |",
            "| :--- | --: | :-: |",
            "| a | 1 | x \\| y |",
            "| b | 2",
            "",
            "| not | a table |",
            "| x |",
        ];
        let k = classify(&lines);
        let align = vec![Align::Left, Align::Right, Align::Center];
        assert_eq!(
            k[0],
            Block::Table {
                row: Row::Header,
                align: align.clone()
            }
        );
        assert_eq!(
            k[1],
            Block::Table {
                row: Row::Delimiter,
                align: align.clone()
            }
        );
        assert_eq!(
            k[2],
            Block::Table {
                row: Row::Body,
                align: align.clone()
            }
        );
        assert_eq!(
            k[3],
            Block::Table {
                row: Row::Body,
                align
            }
        );
        assert_eq!(k[4], Block::Blank);
        assert_eq!(
            (k[5].clone(), k[6].clone()),
            (Block::PipeText, Block::PipeText)
        );
        assert_eq!(table_cell_texts(lines[2]), vec!["a", "1", "x \\| y"]);
        assert_eq!(table_cell_texts(lines[3]), vec!["b", "2"]);
        assert_eq!(table_cells("| a | b |"), vec![(0, 4), (4, 9)]);
        // Rendering keeps the source text and wraps each cell.
        let s = style_line(lines[0], &k[0], &|_| false);
        let html = line_html(lines[0], &s);
        assert_eq!(html.matches("<span class=\"td").count(), 3);
        assert!(html.contains("<span class=\"td ar\">") && html.contains("<span class=\"td ac\">"));
        let text: String = {
            let mut t = String::new();
            let mut in_tag = false;
            for c in html.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => t.push(c),
                    _ => {}
                }
            }
            t
        };
        assert_eq!(text, lines[0]);
        // A header with a different number of cells than the delimiter row is not a table.
        assert_eq!(classify(&["| a | b |", "| --- |"])[0], Block::PipeText);
    }

    #[test]
    fn table_keys() {
        let t = "intro\n| A | B |\n| --- | --- |\n| a1 | b1 |\nafter";
        let at = |s: &str| t.find(s).unwrap();
        // Tab: next cell, then the first cell of the next row (skipping the delimiter row).
        assert_eq!(
            table_tab(t, at("A |"), false),
            Some(TableKey::Move(at("B |")))
        );
        assert_eq!(
            table_tab(t, at("B |"), false),
            Some(TableKey::Move(at("a1")))
        );
        assert_eq!(
            table_tab(t, at("a1"), false),
            Some(TableKey::Move(at("b1")))
        );
        // Tab in the last cell adds a row.
        let end = at("b1 |") + 4;
        assert_eq!(
            table_tab(t, at("b1"), false),
            Some(TableKey::Edit {
                from: end,
                to: end,
                insert: "\n|  |  |".into(),
                caret: end + 3
            })
        );
        // Shift+Tab goes back, across rows.
        assert_eq!(
            table_tab(t, at("a1"), true),
            Some(TableKey::Move(at("B |")))
        );
        assert_eq!(
            table_tab(t, at("B |"), true),
            Some(TableKey::Move(at("A |")))
        );
        // Enter on the header adds a row below the delimiter row; on a body row, below it.
        let delim_end = at("| --- | --- |") + "| --- | --- |".len();
        assert!(
            matches!(table_enter(t, at("A")), Some(TableKey::Edit { from, .. }) if from == delim_end)
        );
        assert!(
            matches!(table_enter(t, at("a1")), Some(TableKey::Edit { from, .. }) if from == end)
        );
        // Enter on an empty row ends the table.
        let t2 = "| A |\n| - |\n|  |";
        assert_eq!(
            table_enter(t2, t2.len() - 1),
            Some(TableKey::Edit {
                from: 12,
                to: 16,
                insert: "\n".into(),
                caret: 13
            })
        );
        // Outside tables nothing happens.
        assert_eq!(table_tab(t, 1, false), None);
        assert_eq!(table_enter(t, t.len()), None);
    }

    #[test]
    fn inline_styles() {
        let s = flags_of("a **b** *c* `d` [[Exists]] [[Nope|alias]] #tag [x](https://e.x)");
        let line = "a **b** *c* `d` [[Exists]] [[Nope|alias]] #tag [x](https://e.x)";
        let at = |sub: &str| line.find(sub).unwrap();
        assert!(s.flags[at("**")] & MK != 0);
        assert!(s.flags[at("b**")] & STRONG != 0);
        assert!(s.flags[at("c*")] & EM != 0);
        assert!(s.flags[at("d`")] & CODE != 0);
        assert!(s.flags[at("Exists")] & WIKI != 0 && s.flags[at("Exists")] & BROKEN == 0);
        assert!(s.flags[at("alias")] & BROKEN != 0);
        assert!(s.flags[at("Nope")] & MK != 0);
        assert!(s.flags[at("#tag")] & TAG != 0);
        assert!(s.flags[at("x]")] & LINK != 0);
        assert_eq!(s.links.len(), 3);
        // HTML text content equals the source.
        let html = line_html(line, &s);
        let stripped: String = {
            let mut out = String::new();
            let mut in_tag = false;
            for c in html.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => out.push(c),
                    _ => {}
                }
            }
            out.replace("&amp;", "&")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
        };
        assert_eq!(stripped, line);
    }

    #[test]
    fn multibyte_text_never_panics() {
        for line in [
            "# Welcome 👋 **bold** ✓",
            "é*à*ü https://x.y/ü 🎉",
            "- [ ] tâche *ital* `cödé` [[Ñote]]",
            "👋👋 ==hl== ~~s~~ _u_",
        ] {
            let s = flags_of(line);
            assert_eq!(s.flags.len(), line.len());
            let _ = line_html(line, &s);
        }
    }

    #[test]
    fn snake_case_is_not_emphasis() {
        let s = flags_of("my_var_name and snake_case_word");
        assert!(s.flags.iter().all(|f| f & EM == 0));
    }
}
