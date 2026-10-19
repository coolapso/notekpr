//! Optional Vim keybindings (normal, insert and visual modes with the most
//! common motions and operators). Off by default; enabled in settings.

use web_sys::KeyboardEvent;

use super::floor_char;

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
    Yank(String),
}

#[derive(Clone, Copy, Default)]
struct Mods {
    ctrl: bool,
    meta: bool,
    alt: bool,
}

#[derive(Default)]
pub struct Vim {
    pub insert: bool,
    pending: String,
    count: String,
    visual: Option<usize>,
    register: String,
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
            "^" => {
                s + t[s..e]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(char::len_utf8)
                    .sum::<usize>()
            }
            "$" | "End" => e,
            "G" => line_bounds(t, t.len()).0,
            "gg" => 0,
            _ => return None,
        })
    }

    pub fn key(&mut self, ev: &KeyboardEvent, t: &str, sel: (usize, usize)) -> Option<Action> {
        let mods = Mods {
            ctrl: ev.ctrl_key(),
            meta: ev.meta_key(),
            alt: ev.alt_key(),
        };
        self.handle(ev.key(), mods, t, sel)
    }

    /// Handle one key. Offsets kept between keys (the visual-mode anchor)
    /// and the selection may be stale after remote edits, so they are
    /// clamped to the current text first.
    fn handle(&mut self, key: String, mods: Mods, t: &str, sel: (usize, usize)) -> Option<Action> {
        let sel = (floor_char(t, sel.0), floor_char(t, sel.1));
        if let Some(a) = self.visual {
            self.visual = Some(floor_char(t, a));
        }
        if self.insert {
            if key == "Escape" || (mods.ctrl && key == "[") {
                self.insert = false;
                let (s, _) = line_bounds(t, sel.1);
                let p = if sel.1 > s { prev(t, sel.1) } else { sel.1 };
                return Some(Action::Move(p, p));
            }
            return None;
        }
        if mods.ctrl && key == "r" {
            return Some(Action::Redo);
        }
        if mods.ctrl || mods.meta || mods.alt {
            return None;
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
        let head = sel.1.min(t.len());
        if key == "Escape" {
            self.pending.clear();
            self.count.clear();
            self.visual = None;
            return Some(Action::Move(head, head));
        }
        if key.chars().all(|c| c.is_ascii_digit())
            && !(key == "0" && self.count.is_empty())
            && self.pending.is_empty()
        {
            self.count.push_str(&key);
            return Some(Action::Consumed);
        }
        let n: usize = self.count.parse().unwrap_or(1).clamp(1, 10_000);
        self.count.clear();

        // Visual mode.
        if let Some(anchor) = self.visual {
            let combo = if key == "g" && self.pending == "g" {
                "gg".to_string()
            } else {
                key.clone()
            };
            if key == "g" && self.pending.is_empty() {
                self.pending = "g".into();
                return Some(Action::Consumed);
            }
            self.pending.clear();
            if let Some(mut to) = self.motion(&combo, t, head) {
                for _ in 1..n {
                    to = self.motion(&combo, t, to).unwrap_or(to);
                }
                return Some(Action::Move(anchor, to));
            }
            let (a, b) = (anchor.min(head), next(t, anchor.max(head)).min(t.len()));
            self.visual = None;
            return Some(match key.as_str() {
                "d" | "x" => {
                    self.register = t[a..b].to_string();
                    Action::Edit {
                        from: a,
                        to: b,
                        insert: String::new(),
                        caret: a,
                    }
                }
                "c" => {
                    self.register = t[a..b].to_string();
                    self.insert = true;
                    Action::Edit {
                        from: a,
                        to: b,
                        insert: String::new(),
                        caret: a,
                    }
                }
                "y" => {
                    self.register = t[a..b].to_string();
                    Action::Yank(self.register.clone())
                }
                _ => Action::Move(head, head),
            });
        }

        let combo = format!("{}{}", self.pending, key);
        // Operators awaiting a motion.
        if let Some(op) = self.pending.chars().next().filter(|c| "dcy".contains(*c)) {
            self.pending.clear();
            let (s, e) = line_bounds(t, head);
            if combo.len() == 2 && combo.chars().nth(1) == Some(op) {
                // dd / cc / yy: whole lines.
                let mut end = e;
                for _ in 1..n {
                    if end >= t.len() {
                        break;
                    }
                    end = line_bounds(t, end + 1).1;
                }
                let full_end = if end < t.len() { end + 1 } else { end };
                let start = if full_end == t.len() && s > 0 && op == 'd' {
                    s - 1
                } else {
                    s
                };
                self.register = format!("{}\n", &t[s..end]);
                return Some(match op {
                    'y' => Action::Yank(self.register.clone()),
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
                });
            }
            let mkey = &combo[1..];
            let mut to = self.motion(if mkey == "w" && op == 'c' { "e" } else { mkey }, t, head)?;
            for _ in 1..n {
                to = self.motion(mkey, t, to).unwrap_or(to);
            }
            if mkey == "e" || (mkey == "w" && op == 'c') {
                to = next(t, to);
            }
            let (a, b) = (head.min(to), head.max(to).min(t.len()));
            self.register = t[a..b].to_string();
            return Some(match op {
                'y' => Action::Yank(self.register.clone()),
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
            });
        }
        if combo == "g" {
            self.pending = "g".into();
            return Some(Action::Consumed);
        }
        self.pending.clear();
        if let Some(mut to) = self.motion(&combo, t, head) {
            for _ in 1..n {
                to = self.motion(&combo, t, to).unwrap_or(to);
            }
            return Some(Action::Move(to, to));
        }
        let (s, e) = line_bounds(t, head);
        Some(match key.as_str() {
            "d" | "c" | "y" => {
                self.pending = key.clone();
                Action::Consumed
            }
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
                let p = self.motion("^", t, head).unwrap_or(s);
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
                    return Some(Action::Consumed);
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
                    return Some(Action::Consumed);
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
            "p" | "P" => {
                let r = self.register.repeat(n);
                if r.is_empty() {
                    return Some(Action::Consumed);
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
            "v" => {
                self.visual = Some(head);
                Action::Move(head, next(t, head).min(e.max(head)))
            }
            "Backspace" => {
                let p = if head > 0 { prev(t, head) } else { 0 };
                Action::Move(p, p)
            }
            _ => Action::Consumed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(v: &mut Vim, key: &str, t: &str, sel: (usize, usize)) -> Option<Action> {
        v.handle(key.to_string(), Mods::default(), t, sel)
    }

    #[test]
    fn visual_anchor_survives_remote_shrinking() {
        let mut v = Vim::default();
        press(&mut v, "v", "hello wonderful world", (15, 15));
        assert_eq!(v.visual, Some(15));
        // A remote edit shrank the text below the anchor.
        match press(&mut v, "d", "hello", (2, 2)) {
            Some(Action::Edit { from, to, .. }) => assert_eq!((from, to), (2, 5)),
            _ => panic!("expected a deletion"),
        }
        assert_eq!(v.visual, None);
        // Anchor and selection inside a multibyte character.
        press(&mut v, "v", "日本語です", (12, 12));
        match press(&mut v, "y", "日本語", (4, 5)) {
            Some(Action::Yank(s)) => assert_eq!(s, "本語"),
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
        ];
        for t in texts {
            for off in 0..t.len() + 3 {
                let mut v = Vim::default();
                for k in keys {
                    let _ = press(&mut v, k, t, (off, off + 1));
                }
            }
        }
    }
}
