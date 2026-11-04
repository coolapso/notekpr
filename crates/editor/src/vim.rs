//! Optional Vim keybindings (normal, insert and visual modes with the most
//! common motions and operators). Off by default; enabled in settings.
//!
//! Pure: keys come in as `KeyboardEvent.key` strings plus modifiers, actions
//! go out; the view turns them into transactions, and measures what depends
//! on the layout (half a screen, lines as they wrap) before calling
//! [`Vim::moved`].

use crate::text::floor_char;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Consumed,
    Move(usize, usize),
    Edit {
        from: usize,
        to: usize,
        insert: String,
        caret: usize,
    },
    Undo,
    Redo,
    /// Copy the text; the cursor goes to the offset.
    Yank(String, usize),
    /// Ctrl+D / Ctrl+U: scroll half a screen down or up, the cursor with it.
    HalfPage {
        down: bool,
    },
    /// gj / gk: `count` lines down or up as they show (a wrapped line shows
    /// as several).
    DisplayLines {
        down: bool,
        count: usize,
    },
    /// Ctrl+O / Ctrl+I with no jumps left in this text: the previous
    /// (`back`) or next place in the application's history.
    History {
        back: bool,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub meta: bool,
    pub alt: bool,
}

/// Visual mode: where it started, and whether whole lines are selected (`V`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visual {
    anchor: usize,
    lines: bool,
}

/// Most jumps remembered.
const JUMPS: usize = 100;

#[derive(Default)]
pub struct Vim {
    pub insert: bool,
    pending: String,
    count: String,
    visual: Option<Visual>,
    register: String,
    /// The selection last shown, and the cursor it was shown for: in visual
    /// mode the selection also covers the character under the cursor (or
    /// whole lines), so the cursor isn't simply its head.
    shown: Option<((usize, usize), usize)>,
    /// Where jumps (gg, G, the application moving the cursor) were made
    /// from, oldest first, and the one Ctrl+O and Ctrl+I are at (the
    /// list's length: none, past the newest).
    jumps: Vec<usize>,
    jump: usize,
}

fn line_bounds(t: &str, i: usize) -> (usize, usize) {
    let i = floor_char(t, i);
    let s = t[..i].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let e = t[i..].find('\n').map(|p| i + p).unwrap_or(t.len());
    (s, e)
}

fn next(t: &str, i: usize) -> usize {
    t[i..]
        .chars()
        .next()
        .map(|c| i + c.len_utf8())
        .unwrap_or(t.len())
}

fn prev(t: &str, i: usize) -> usize {
    t[..i]
        .char_indices()
        .next_back()
        .map(|(p, _)| p)
        .unwrap_or(0)
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_fwd(t: &str, mut i: usize) -> usize {
    let start = t[i..].chars().next().map(class);
    while i < t.len() && t[i..].chars().next().map(class) == start && start != Some(0) {
        i = next(t, i);
    }
    while i < t.len() && t[i..].chars().next().is_some_and(|c| c.is_whitespace()) {
        i = next(t, i);
    }
    i
}

fn word_end(t: &str, mut i: usize) -> usize {
    i = next(t, i);
    while i < t.len() && t[i..].chars().next().is_some_and(|c| c.is_whitespace()) {
        i = next(t, i);
    }
    let cl = t[i..].chars().next().map(class);
    while i < t.len() {
        let n = next(t, i);
        if n >= t.len() || t[n..].chars().next().map(class) != cl {
            break;
        }
        i = n;
    }
    i
}

fn word_back(t: &str, mut i: usize) -> usize {
    while i > 0
        && t[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace())
    {
        i = prev(t, i);
    }
    let cl = t[..i].chars().next_back().map(class);
    while i > 0 && t[..i].chars().next_back().map(class) == cl {
        i = prev(t, i);
    }
    i
}

fn column(t: &str, i: usize) -> usize {
    let (s, _) = line_bounds(t, i);
    t[s..i].chars().count()
}

fn at_column(t: &str, line_start: usize, col: usize) -> usize {
    let (_, e) = line_bounds(t, line_start);
    let mut i = line_start;
    for _ in 0..col {
        if i >= e {
            break;
        }
        i = next(t, i);
    }
    i.min(e)
}

fn line_down(t: &str, i: usize) -> usize {
    let col = column(t, i);
    let (_, e) = line_bounds(t, i);
    if e >= t.len() {
        i
    } else {
        at_column(t, e + 1, col)
    }
}

fn line_up(t: &str, i: usize) -> usize {
    let col = column(t, i);
    let (s, _) = line_bounds(t, i);
    if s == 0 {
        i
    } else {
        let (ps, _) = line_bounds(t, s - 1);
        at_column(t, ps, col)
    }
}

/// Where the first character that isn't a space is on the line at `i`.
fn first_non_blank(t: &str, i: usize) -> usize {
    let (s, e) = line_bounds(t, i);
    s + t[s..e]
        .chars()
        .take_while(|c| c.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>()
}

/// The start of line `n` (from 0), or of the last line.
fn nth_line(t: &str, n: usize) -> usize {
    let mut s = 0;
    for _ in 0..n {
        match t[s..].find('\n') {
            Some(p) => s += p + 1,
            None => break,
        }
    }
    s
}

/// Motions that make an operator work on whole lines.
fn linewise(motion: &str) -> bool {
    matches!(motion, "j" | "k" | "ArrowDown" | "ArrowUp" | "G" | "gg")
}

/// Motions remembered as jumps (Ctrl+O comes back).
fn is_jump(motion: &str) -> bool {
    matches!(motion, "G" | "gg")
}

/// `>` / `<` on the lines from `s` to `e` (a line's start, a line's end),
/// `times` times: two spaces more or less, as the editor's Tab does.
fn indent(t: &str, s: usize, e: usize, out: bool, times: usize) -> Action {
    let lines: Vec<String> = t[s..e]
        .split('\n')
        .map(|l| {
            let mut l = l.to_string();
            for _ in 0..times {
                l = if out {
                    l.strip_prefix("  ")
                        .or_else(|| l.strip_prefix('\t'))
                        .or_else(|| l.strip_prefix(' '))
                        .unwrap_or(&l)
                        .to_string()
                } else if l.is_empty() {
                    l
                } else {
                    format!("  {l}")
                };
            }
            l
        })
        .collect();
    let insert = lines.join("\n");
    if insert == t[s..e] {
        return Action::Consumed;
    }
    let caret = s + insert.len() - insert.trim_start_matches([' ', '\t']).len();
    Action::Edit {
        from: s,
        to: e,
        insert,
        caret,
    }
}

impl Vim {
    fn motion(&self, key: &str, t: &str, i: usize) -> Option<usize> {
        let (s, e) = line_bounds(t, i);
        Some(match key {
            "h" | "ArrowLeft" => {
                if i > s {
                    prev(t, i)
                } else {
                    i
                }
            }
            "l" | "ArrowRight" => {
                if i < e {
                    next(t, i)
                } else {
                    i
                }
            }
            "j" | "ArrowDown" => line_down(t, i),
            "k" | "ArrowUp" => line_up(t, i),
            "w" => word_fwd(t, i),
            "e" => word_end(t, i),
            "b" => word_back(t, i),
            "0" | "Home" => s,
            "^" => first_non_blank(t, i),
            "$" | "End" => e,
            "G" => first_non_blank(t, line_bounds(t, t.len()).0),
            "gg" => first_non_blank(t, 0),
            _ => return None,
        })
    }

    /// `motion` `n` times; with a count, G and gg go to that line.
    fn motion_n(&self, key: &str, n: usize, counted: bool, t: &str, from: usize) -> Option<usize> {
        if counted && matches!(key, "G" | "gg") {
            return Some(first_non_blank(t, nth_line(t, n - 1)));
        }
        let mut to = self.motion(key, t, from)?;
        for _ in 1..n {
            to = self.motion(key, t, to).unwrap_or(to);
        }
        Some(to)
    }

    pub fn key(&mut self, key: &str, mods: Mods, t: &str, sel: (usize, usize)) -> Option<Action> {
        self.handle(key.to_string(), mods, t, sel)
    }

    /// Where the cursor is, given the selection shown (`sel`, anchor and
    /// head): the one it was shown for, or else (someone clicked) its head.
    pub fn cursor(&self, t: &str, sel: (usize, usize)) -> usize {
        match self.shown {
            Some((s, c)) if s == sel => floor_char(t, c),
            _ => floor_char(t, sel.1),
        }
    }

    /// The cursor moved to `to` (the view measured where): the selection
    /// to show, anchor and head. In visual mode it covers the character
    /// under the cursor, or whole lines.
    pub fn moved(&mut self, t: &str, to: usize) -> (usize, usize) {
        let to = floor_char(t, to);
        let sel = match self.visual {
            None => (to, to),
            Some(Visual {
                anchor,
                lines: true,
            }) => {
                let (a, b) = (line_bounds(t, anchor), line_bounds(t, to));
                if to >= anchor { (a.0, b.1) } else { (a.1, b.0) }
            }
            Some(Visual {
                anchor,
                lines: false,
            }) => {
                if to >= anchor {
                    (anchor, next(t, to))
                } else {
                    (next(t, anchor), to)
                }
            }
        };
        self.shown = Some((sel, to));
        sel
    }

    fn show(&mut self, t: &str, to: usize) -> Action {
        let (a, h) = self.moved(t, to);
        Action::Move(a, h)
    }

    /// A jump from `from` (gg, G, or the application moving the cursor,
    /// e.g. to a heading): Ctrl+O comes back to it. One per line.
    pub fn jumped(&mut self, t: &str, from: usize) {
        let from = floor_char(t, from);
        let line = line_bounds(t, from).0;
        self.jumps
            .retain(|&j| line_bounds(t, floor_char(t, j)).0 != line);
        self.jumps.push(from);
        if self.jumps.len() > JUMPS {
            self.jumps.remove(0);
        }
        self.jump = self.jumps.len();
    }

    fn jump_back(&mut self, t: &str, cur: usize) -> Action {
        if self.jump >= self.jumps.len() {
            // Ctrl+I comes back here.
            self.jumped(t, cur);
            self.jump = self.jumps.len() - 1;
        }
        if self.jump == 0 {
            return Action::History { back: true };
        }
        self.jump -= 1;
        let to = self.jumps[self.jump];
        self.show(t, to)
    }

    fn jump_forward(&mut self, t: &str) -> Action {
        if self.jump + 1 >= self.jumps.len() {
            return Action::History { back: false };
        }
        self.jump += 1;
        let to = self.jumps[self.jump];
        self.show(t, to)
    }

    /// Move the offsets kept between keys with a change to the text
    /// (`map` maps an offset of the old text to the new one).
    pub fn map(&mut self, map: impl Fn(usize) -> usize) {
        for j in &mut self.jumps {
            *j = map(*j);
        }
        if let Some(v) = &mut self.visual {
            v.anchor = map(v.anchor);
        }
        if let Some(((a, h), c)) = self.shown {
            self.shown = Some(((map(a), map(h)), map(c)));
        }
    }

    /// Handle one key. Offsets kept between keys (the visual-mode anchor)
    /// and the selection may be stale after remote edits, so they are
    /// clamped to the current text first.
    fn handle(&mut self, key: String, mods: Mods, t: &str, sel: (usize, usize)) -> Option<Action> {
        let sel = (floor_char(t, sel.0), floor_char(t, sel.1));
        if let Some(v) = &mut self.visual {
            v.anchor = floor_char(t, v.anchor);
        }
        if self.insert {
            if key == "Escape" || (mods.ctrl && key == "[") {
                self.insert = false;
                let (s, _) = line_bounds(t, sel.1);
                let p = if sel.1 > s { prev(t, sel.1) } else { sel.1 };
                return Some(self.show(t, p));
            }
            return None;
        }
        let cur = self.cursor(t, sel);
        if mods.ctrl && !mods.meta && !mods.alt {
            let a = match key.as_str() {
                "r" => Action::Redo,
                "d" | "u" => Action::HalfPage { down: key == "d" },
                "o" => self.jump_back(t, cur),
                "i" => self.jump_forward(t),
                "[" => return self.handle("Escape".into(), Mods::default(), t, sel),
                _ => return None,
            };
            self.pending.clear();
            self.count.clear();
            return Some(a);
        }
        if mods.ctrl || mods.meta || mods.alt {
            return None;
        }
        if key == "Tab" {
            self.pending.clear();
            self.count.clear();
            return Some(self.jump_forward(t));
        }
        if key.len() > 1
            && !matches!(
                key.as_str(),
                "Escape"
                    | "ArrowLeft"
                    | "ArrowRight"
                    | "ArrowUp"
                    | "ArrowDown"
                    | "Home"
                    | "End"
                    | "Backspace"
            )
        {
            return None;
        }
        if key == "Escape" {
            self.pending.clear();
            self.count.clear();
            self.visual = None;
            return Some(self.show(t, cur));
        }
        if key.chars().all(|c| c.is_ascii_digit())
            && !(key == "0" && self.count.is_empty())
            && self.pending.is_empty()
        {
            self.count.push_str(&key);
            return Some(Action::Consumed);
        }
        // g waits for the next key (gg, gj, gk), keeping the count.
        if key == "g" && !self.pending.ends_with('g') {
            self.pending.push('g');
            return Some(Action::Consumed);
        }
        let counted = !self.count.is_empty();
        let n: usize = self.count.parse().unwrap_or(1).clamp(1, 10_000);
        self.count.clear();
        let pending = std::mem::take(&mut self.pending);
        let (op, combo) = match pending.chars().next() {
            Some(c) if "dcy<>".contains(c) => (Some(c), format!("{}{key}", &pending[1..])),
            _ => (None, format!("{pending}{key}")),
        };
        if combo == "gj" || combo == "gk" {
            if op.is_some() {
                return Some(Action::Consumed);
            }
            return Some(Action::DisplayLines {
                down: combo == "gj",
                count: n,
            });
        }
        if let Some(v) = self.visual {
            return Some(self.visual_key(v, &combo, n, counted, t, cur));
        }
        if let Some(op) = op {
            return self.operator(op, &combo, n, counted, t, cur);
        }
        // An operator waits for its motion, keeping the count (3dd, 3>>).
        if matches!(key.as_str(), "d" | "c" | "y" | ">" | "<") {
            self.pending = key;
            if counted {
                self.count = n.to_string();
            }
            return Some(Action::Consumed);
        }
        if let Some(to) = self.motion_n(&combo, n, counted, t, cur) {
            if is_jump(&combo) {
                self.jumped(t, cur);
            }
            return Some(self.show(t, to));
        }
        Some(self.command(&key, n, t, cur))
    }

    /// An operator (d, c, y, >, <) and the key after it: doubled for whole
    /// lines (dd, >>), or a motion.
    fn operator(
        &mut self,
        op: char,
        combo: &str,
        n: usize,
        counted: bool,
        t: &str,
        cur: usize,
    ) -> Option<Action> {
        let (s, e) = line_bounds(t, cur);
        if combo.chars().eq([op]) {
            let mut end = e;
            for _ in 1..n {
                if end >= t.len() {
                    break;
                }
                end = line_bounds(t, end + 1).1;
            }
            return Some(self.on_lines(op, t, s, end, cur));
        }
        let motion = if combo == "w" && op == 'c' {
            "e"
        } else {
            combo
        };
        let mut to = self.motion(motion, t, cur)?;
        if counted && matches!(combo, "G" | "gg") {
            to = self.motion_n(combo, n, true, t, cur)?;
        } else {
            for _ in 1..n {
                to = self.motion(combo, t, to).unwrap_or(to);
            }
        }
        if linewise(combo) || op == '>' || op == '<' {
            let (a, b) = (cur.min(to), cur.max(to));
            return Some(self.on_lines(op, t, line_bounds(t, a).0, line_bounds(t, b).1, a));
        }
        if combo == "e" || (combo == "w" && op == 'c') {
            to = next(t, to);
        }
        let (a, b) = (cur.min(to), cur.max(to).min(t.len()));
        self.register = t[a..b].to_string();
        Some(match op {
            'y' => Action::Yank(self.register.clone(), a),
            'c' => {
                self.insert = true;
                Action::Edit {
                    from: a,
                    to: b,
                    insert: String::new(),
                    caret: a,
                }
            }
            _ => Action::Edit {
                from: a,
                to: b,
                insert: String::new(),
                caret: a,
            },
        })
    }

    /// An operator on whole lines, from `s` (a line's start) to `end` (a
    /// line's end); a yank leaves the cursor at `cur`.
    fn on_lines(&mut self, op: char, t: &str, s: usize, end: usize, cur: usize) -> Action {
        if op == '>' || op == '<' {
            return indent(t, s, end, op == '<', 1);
        }
        let full_end = if end < t.len() { end + 1 } else { end };
        let start = if full_end == t.len() && s > 0 && op == 'd' {
            s - 1
        } else {
            s
        };
        self.register = format!("{}\n", &t[s..end]);
        match op {
            'y' => Action::Yank(self.register.clone(), cur),
            'c' => {
                self.insert = true;
                Action::Edit {
                    from: s,
                    to: end,
                    insert: String::new(),
                    caret: s,
                }
            }
            _ => Action::Edit {
                from: start,
                to: full_end,
                insert: String::new(),
                caret: start.min(t.len()),
            },
        }
    }

    /// J: join `lines` lines (at least two) starting with the one at `at`:
    /// the next one's indentation goes, and a space goes between them.
    fn join(&mut self, t: &str, at: usize, lines: usize) -> Action {
        let (s, mut end) = line_bounds(t, at);
        let mut text = t[s..end].to_string();
        let mut caret = None;
        for _ in 1..lines.max(2) {
            if end >= t.len() {
                break;
            }
            let (ns, ne) = line_bounds(t, end + 1);
            let more = t[ns..ne].trim_start_matches([' ', '\t']);
            caret = Some(s + text.len());
            if !(text.is_empty()
                || text.ends_with([' ', '\t'])
                || more.is_empty()
                || more.starts_with(')'))
            {
                text.push(' ');
            }
            text.push_str(more);
            end = ne;
        }
        let Some(caret) = caret else {
            return Action::Consumed;
        };
        Action::Edit {
            from: s,
            to: end,
            insert: text,
            caret,
        }
    }

    /// A key in visual mode: a motion moves the cursor (the selection
    /// follows); an operator works on the selection and ends visual mode.
    fn visual_key(
        &mut self,
        v: Visual,
        combo: &str,
        n: usize,
        counted: bool,
        t: &str,
        cur: usize,
    ) -> Action {
        if let Some(to) = self.motion_n(combo, n, counted, t, cur) {
            if is_jump(combo) {
                self.jumped(t, cur);
            }
            return self.show(t, to);
        }
        match combo {
            "v" | "V" => {
                let lines = combo == "V";
                self.visual = (v.lines != lines).then_some(Visual { lines, ..v });
                return self.show(t, cur);
            }
            // The other end.
            "o" => {
                self.visual = Some(Visual { anchor: cur, ..v });
                return self.show(t, v.anchor);
            }
            _ => {}
        }
        self.visual = None;
        let (lo, hi) = (v.anchor.min(cur), v.anchor.max(cur));
        let (ls, le) = (line_bounds(t, lo).0, line_bounds(t, hi).1);
        match combo {
            ">" | "<" => indent(t, ls, le, combo == "<", n),
            "J" => {
                let lines = t[ls..le].matches('\n').count() + 1;
                self.join(t, ls, lines)
            }
            "D" | "X" => self.on_lines('d', t, ls, le, lo),
            "Y" => self.on_lines('y', t, ls, le, lo),
            "C" | "S" | "R" => self.on_lines('c', t, ls, le, lo),
            "d" | "x" | "y" | "c" if v.lines => {
                let op = if combo == "x" {
                    'd'
                } else {
                    combo.chars().next().unwrap_or('d')
                };
                self.on_lines(op, t, ls, le, lo)
            }
            "d" | "x" | "c" | "y" => {
                let b = next(t, hi).min(t.len());
                self.register = t[lo..b].to_string();
                match combo {
                    "y" => Action::Yank(self.register.clone(), lo),
                    "c" => {
                        self.insert = true;
                        Action::Edit {
                            from: lo,
                            to: b,
                            insert: String::new(),
                            caret: lo,
                        }
                    }
                    _ => Action::Edit {
                        from: lo,
                        to: b,
                        insert: String::new(),
                        caret: lo,
                    },
                }
            }
            _ => self.show(t, cur),
        }
    }

    /// A key in normal mode that isn't a motion.
    fn command(&mut self, key: &str, n: usize, t: &str, head: usize) -> Action {
        let (s, e) = line_bounds(t, head);
        match key {
            "i" => {
                self.insert = true;
                Action::Move(head, head)
            }
            "a" => {
                self.insert = true;
                let p = if head < e { next(t, head) } else { head };
                Action::Move(p, p)
            }
            "I" => {
                self.insert = true;
                let p = first_non_blank(t, head);
                Action::Move(p, p)
            }
            "A" => {
                self.insert = true;
                Action::Move(e, e)
            }
            "o" => {
                self.insert = true;
                Action::Edit {
                    from: e,
                    to: e,
                    insert: "\n".into(),
                    caret: e + 1,
                }
            }
            "O" => {
                self.insert = true;
                Action::Edit {
                    from: s,
                    to: s,
                    insert: "\n".into(),
                    caret: s,
                }
            }
            "x" => {
                if head >= e {
                    return Action::Consumed;
                }
                let mut b = head;
                for _ in 0..n {
                    if b < e {
                        b = next(t, b);
                    }
                }
                self.register = t[head..b].to_string();
                Action::Edit {
                    from: head,
                    to: b,
                    insert: String::new(),
                    caret: head,
                }
            }
            "X" => {
                if head <= s {
                    return Action::Consumed;
                }
                let a = prev(t, head);
                Action::Edit {
                    from: a,
                    to: head,
                    insert: String::new(),
                    caret: a,
                }
            }
            "D" | "C" => {
                self.register = t[head..e].to_string();
                if key == "C" {
                    self.insert = true;
                }
                Action::Edit {
                    from: head,
                    to: e,
                    insert: String::new(),
                    caret: head,
                }
            }
            "J" => self.join(t, head, n),
            "p" | "P" => {
                let r = self.register.repeat(n);
                if r.is_empty() {
                    return Action::Consumed;
                }
                if r.ends_with('\n') {
                    if key == "p" {
                        let at = if e < t.len() { e + 1 } else { e };
                        let ins = if e >= t.len() {
                            format!("\n{}", r.trim_end_matches('\n'))
                        } else {
                            r.clone()
                        };
                        let caret = if e >= t.len() { at + 1 } else { at };
                        Action::Edit {
                            from: at,
                            to: at,
                            insert: ins,
                            caret,
                        }
                    } else {
                        Action::Edit {
                            from: s,
                            to: s,
                            insert: r,
                            caret: s,
                        }
                    }
                } else {
                    let at = if key == "p" && head < e {
                        next(t, head)
                    } else {
                        head
                    };
                    let caret = at + r.len() - r.chars().last().map(char::len_utf8).unwrap_or(0);
                    Action::Edit {
                        from: at,
                        to: at,
                        insert: r,
                        caret,
                    }
                }
            }
            "u" => Action::Undo,
            "v" | "V" => {
                self.visual = Some(Visual {
                    anchor: head,
                    lines: key == "V",
                });
                self.show(t, head)
            }
            "Backspace" => {
                let p = if head > 0 { prev(t, head) } else { 0 };
                Action::Move(p, p)
            }
            _ => Action::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(v: &mut Vim, key: &str, t: &str, sel: (usize, usize)) -> Option<Action> {
        v.handle(key.to_string(), Mods::default(), t, sel)
    }

    const CTRL: Mods = Mods {
        ctrl: true,
        meta: false,
        alt: false,
    };

    /// Press `keys` (one character each, `^x` for Ctrl+x) on `t` with the
    /// caret at `at`, applying moves and edits as the view does: the text,
    /// the selection shown, and the actions.
    fn run(t: &str, at: usize, keys: &[&str]) -> (String, (usize, usize), Vec<Action>) {
        let mut v = Vim::default();
        let (mut t, mut sel) = (t.to_string(), (at, at));
        let mut out = vec![];
        for k in keys {
            let a = match k.strip_prefix('^') {
                Some(k) => v.handle(k.to_string(), CTRL, &t, sel),
                None => press(&mut v, k, &t, sel),
            };
            match &a {
                Some(Action::Move(a, h)) => sel = (*a, *h),
                Some(Action::Edit {
                    from,
                    to,
                    insert,
                    caret,
                }) => {
                    t.replace_range(from..to, insert);
                    sel = (*caret, *caret);
                }
                Some(Action::Yank(_, c)) => sel = v.moved(&t, *c),
                _ => {}
            }
            out.extend(a);
        }
        (t, sel, out)
    }

    #[test]
    fn join_lines() {
        let t = "one\n  two\nthree)\n)four\n\nfive";
        let (got, sel, _) = run(t, 1, &["J"]);
        assert_eq!(got, "one two\nthree)\n)four\n\nfive");
        assert_eq!(sel, (3, 3), "the cursor is where they joined");
        // A count joins that many lines; no space before ")".
        let (got, ..) = run(t, 0, &["3", "J"]);
        assert_eq!(got, "one two three)\n)four\n\nfive");
        let (got, ..) = run("a)\n)b\n\nc", 0, &["J", "J", "J"]);
        assert_eq!(got, "a))b c");
        // The last line has nothing to join.
        let (got, ..) = run("only", 0, &["J"]);
        assert_eq!(got, "only");
        // In visual mode: the selected lines.
        let (got, ..) = run("a\nb\nc\nd", 0, &["V", "j", "j", "J"]);
        assert_eq!(got, "a b c\nd");
    }

    #[test]
    fn visual_lines() {
        let t = "one\ntwo\nthree\nfour";
        let (_, sel, _) = run(t, 5, &["V"]);
        assert_eq!(sel, (4, 7), "V selects the whole line");
        let (_, sel, _) = run(t, 5, &["V", "j"]);
        assert_eq!(sel, (4, 13));
        let (_, sel, _) = run(t, 5, &["V", "k"]);
        assert_eq!(sel, (7, 0), "upwards, anchored at the line's end");
        let (got, sel, _) = run(t, 5, &["V", "j", "d"]);
        assert_eq!(got, "one\nfour");
        assert_eq!(sel, (4, 4));
        let (_, _, acts) = run(t, 5, &["V", "j", "y"]);
        assert_eq!(acts.last(), Some(&Action::Yank("two\nthree\n".into(), 5)));
        // Pasted as lines.
        let (got, ..) = run(t, 5, &["V", "y", "j", "p"]);
        assert_eq!(got, "one\ntwo\nthree\ntwo\nfour");
        // v and V switch between characters and lines; again leaves.
        let (_, sel, _) = run(t, 5, &["V", "v"]);
        assert_eq!(sel, (5, 6));
        let (_, sel, _) = run(t, 5, &["V", "V"]);
        assert_eq!(sel, (5, 5));
        // o: the other end.
        let (_, sel, _) = run(t, 5, &["v", "l", "o"]);
        assert_eq!(sel, (7, 5));
    }

    #[test]
    fn character_visual_mode_covers_the_cursor() {
        let t = "hello world";
        let (_, sel, _) = run(t, 0, &["v", "l"]);
        assert_eq!(sel, (0, 2), "two characters: the anchor's and the cursor's");
        let (got, ..) = run(t, 0, &["v", "l", "d"]);
        assert_eq!(got, "llo world");
        let (_, sel, _) = run(t, 4, &["v", "h", "h"]);
        assert_eq!(sel, (5, 2), "backwards, the anchor's character stays in");
        let (_, sel, _) = run(t, 0, &["v", "e", "Escape"]);
        assert_eq!(sel, (4, 4), "Escape leaves the cursor where it was");
    }

    #[test]
    fn indent_and_outdent() {
        let t = "a\n- b\n\n  c";
        let (got, sel, _) = run(t, 0, &[">", ">"]);
        assert_eq!(got, "  a\n- b\n\n  c");
        assert_eq!(sel, (2, 2), "the cursor goes to the first non-blank");
        let (got, ..) = run(t, 0, &["3", ">", ">"]);
        assert_eq!(got, "  a\n  - b\n\n  c", "empty lines stay empty");
        let (got, ..) = run(t, 0, &[">", "j"]);
        assert_eq!(got, "  a\n  - b\n\n  c");
        let (got, ..) = run(t, 8, &["<", "<"]);
        assert_eq!(got, "a\n- b\n\nc");
        let (got, ..) = run(t, 0, &["<", "<"]);
        assert_eq!(got, t, "nothing to outdent");
        let (got, ..) = run(t, 0, &["V", "j", "2", ">"]);
        assert_eq!(
            got, "    a\n    - b\n\n  c",
            "a count in visual mode: that many times"
        );
    }

    #[test]
    fn linewise_motions_take_whole_lines() {
        let t = "one\ntwo\nthree\nfour";
        let (got, ..) = run(t, 5, &["d", "j"]);
        assert_eq!(got, "one\nfour");
        let (got, ..) = run(t, 5, &["d", "k"]);
        assert_eq!(got, "three\nfour");
        let (got, ..) = run(t, 5, &["d", "G"]);
        assert_eq!(got, "one");
        let (got, ..) = run(t, 5, &["d", "g", "g"]);
        assert_eq!(got, "three\nfour");
        let (_, _, acts) = run(t, 5, &["y", "j"]);
        assert_eq!(acts.last(), Some(&Action::Yank("two\nthree\n".into(), 5)));
        let (got, ..) = run(t, 0, &["2", "d", "d"]);
        assert_eq!(got, "three\nfour");
    }

    #[test]
    fn counted_g_goes_to_that_line() {
        let t = "one\n  two\nthree";
        let (_, sel, _) = run(t, 0, &["2", "G"]);
        assert_eq!(sel, (6, 6), "line 2, its first non-blank");
        let (_, sel, _) = run(t, 0, &["3", "g", "g"]);
        assert_eq!(sel, (10, 10));
        let (_, sel, _) = run(t, 0, &["9", "G"]);
        assert_eq!(sel, (10, 10), "past the end: the last line");
        let (_, sel, _) = run(t, 12, &["g", "g"]);
        assert_eq!(sel, (0, 0));
    }

    #[test]
    fn layout_moves_go_to_the_view() {
        let (_, _, acts) = run("a\nb", 0, &["^d", "^u", "g", "j", "3", "g", "k"]);
        assert_eq!(
            acts,
            vec![
                Action::HalfPage { down: true },
                Action::HalfPage { down: false },
                Action::Consumed,
                Action::DisplayLines {
                    down: true,
                    count: 1
                },
                Action::Consumed,
                Action::Consumed,
                Action::DisplayLines {
                    down: false,
                    count: 3
                },
            ]
        );
        // In visual mode the view's measure moves the cursor, and the
        // selection follows.
        let mut v = Vim::default();
        let t = "one\ntwo\nthree";
        press(&mut v, "V", t, (1, 1));
        assert_eq!(v.moved(t, 9), (0, 13));
        assert_eq!(v.cursor(t, (0, 13)), 9);
        assert_eq!(v.cursor(t, (2, 2)), 2, "the selection changed: its head");
    }

    #[test]
    fn jumps_go_back_and_forth() {
        let t = "zero\none\ntwo\nthree\nfour";
        let mut v = Vim::default();
        let mut sel = (5, 5);
        let key = |v: &mut Vim, k: &str, mods: Mods, sel: &mut (usize, usize)| {
            let a = v.handle(k.to_string(), mods, t, *sel);
            if let Some(Action::Move(a, h)) = a {
                *sel = (a, h);
            }
            a
        };
        let none = Mods::default();
        key(&mut v, "G", none, &mut sel);
        assert_eq!(sel, (19, 19));
        key(&mut v, "k", none, &mut sel);
        key(&mut v, "g", none, &mut sel);
        key(&mut v, "g", none, &mut sel);
        assert_eq!(sel, (0, 0));
        // Back: where gg was pressed, then where G was.
        key(&mut v, "o", CTRL, &mut sel);
        assert_eq!(sel, (13, 13));
        key(&mut v, "o", CTRL, &mut sel);
        assert_eq!(sel, (5, 5));
        // No jumps left in this text: the previous place in the app.
        assert_eq!(
            key(&mut v, "o", CTRL, &mut sel),
            Some(Action::History { back: true })
        );
        assert_eq!(sel, (5, 5));
        // Forward again, Tab too, then past the newest.
        key(&mut v, "i", CTRL, &mut sel);
        assert_eq!(sel, (13, 13));
        key(&mut v, "Tab", none, &mut sel);
        assert_eq!(sel, (0, 0));
        assert_eq!(
            key(&mut v, "i", CTRL, &mut sel),
            Some(Action::History { back: false })
        );
        // Jumps move with edits made before them.
        let mut v = Vim::default();
        v.jumped(t, 10);
        v.map(|p| if p >= 5 { p + 3 } else { p });
        let (mut sel, t2) = ((0, 0), "zero\nNEWone\ntwo\nthree\nfour");
        if let Some(Action::Move(a, h)) = v.handle("o".into(), CTRL, t2, sel) {
            sel = (a, h);
        }
        assert_eq!(sel, (13, 13));
    }

    #[test]
    fn visual_anchor_survives_remote_shrinking() {
        let mut v = Vim::default();
        press(&mut v, "v", "hello wonderful world", (15, 15));
        assert_eq!(v.visual.map(|v| v.anchor), Some(15));
        // A remote edit shrank the text below the anchor.
        match press(&mut v, "d", "hello", (2, 2)) {
            Some(Action::Edit { from, to, .. }) => assert_eq!((from, to), (2, 5)),
            _ => panic!("expected a deletion"),
        }
        assert_eq!(v.visual, None);
        // Anchor and selection inside a multibyte character.
        press(&mut v, "v", "日本語です", (12, 12));
        match press(&mut v, "y", "日本語", (4, 5)) {
            Some(Action::Yank(s, _)) => assert_eq!(s, "本語"),
            _ => panic!("expected a yank"),
        }
        press(&mut v, "v", "日本語です", (12, 12));
        assert!(matches!(
            press(&mut v, "l", "日本", (1, 1)),
            Some(Action::Move(6, 3))
        ));
    }

    #[test]
    fn stale_offsets_never_panic() {
        let texts = ["", "a", "日本語", "x\n日\n", "👍 a 👍"];
        let keys = [
            "v", "h", "l", "j", "k", "w", "e", "b", "0", "^", "$", "x", "X", "d", "d", "c", "w",
            "y", "y", "p", "P", "o", "Escape", "D", "v", "G", "y", "a", "Escape", "I", "Escape",
            "V", "j", ">", "J", "V", "k", "<", "o", "d", ">", ">", "<", "<", "J", "3", "J", "g",
            "j", "g", "k", "5", "G", "g", "g", "V", "y", "d", "j", "Tab",
        ];
        for t in texts {
            for off in 0..t.len() + 3 {
                let mut v = Vim::default();
                for k in keys {
                    let _ = press(&mut v, k, t, (off, off + 1));
                }
                for k in ["o", "i", "d", "u", "o", "o", "i"] {
                    let _ = v.handle(k.to_string(), CTRL, t, (off, off + 1));
                }
            }
        }
    }
}
