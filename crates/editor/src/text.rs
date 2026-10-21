//! The document buffer: the Markdown source plus an index of line starts.
//!
//! Why not a rope: everything around the editor needs the text as one
//! contiguous `&str` — the host's `edit(base, edits)` contract and the CRDT
//! compare and diff whole texts, remote updates arrive as a whole new text,
//! and the Markdown helpers (tables, vim motions) slice it freely. A rope
//! would have to be flattened on every keystroke, losing its advantage. A
//! `String` with a line index gives O(1) slicing, O(log n) offset → line, and
//! an edit costs one `memmove` of the tail plus shifting the later line
//! starts — microseconds for notes of any realistic size (the server caps a
//! single update at a few MiB anyway).

use std::ops::Range;

use crate::change::ChangeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    s: String,
    /// Byte offset where each line starts; `starts[0] == 0`, never empty.
    starts: Vec<usize>,
}

impl Default for Text {
    fn default() -> Self {
        Text::new(String::new())
    }
}

/// Lines `old_start..old_start + old` of the old document became lines
/// `new_start..new_start + new` of the new one; the lines in between splices
/// are unchanged (only shifted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSplice {
    pub old_start: usize,
    pub old: usize,
    pub new_start: usize,
    pub new: usize,
}

impl LineSplice {
    pub fn new_range(&self) -> Range<usize> {
        self.new_start..self.new_start + self.new
    }
}

fn line_starts(s: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(s.match_indices('\n').map(|(i, _)| i + 1));
    v
}

/// `i` clamped to `s` and moved back onto a character boundary.
pub fn floor_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Byte offset in `s` of UTF-16 offset `u` (the DOM counts in UTF-16 units).
pub fn utf16_to_byte(s: &str, u: usize) -> usize {
    let mut acc = 0;
    for (i, c) in s.char_indices() {
        if acc >= u {
            return i;
        }
        acc += c.len_utf16();
    }
    s.len()
}

/// UTF-16 offset of byte offset `b` in `s`.
pub fn byte_to_utf16(s: &str, b: usize) -> usize {
    s[..floor_char(s, b)].chars().map(char::len_utf16).sum()
}

impl Text {
    pub fn new(s: String) -> Text {
        let starts = line_starts(&s);
        Text { s, starts }
    }

    pub fn as_str(&self) -> &str {
        &self.s
    }

    pub fn len(&self) -> usize {
        self.s.len()
    }

    pub fn is_empty(&self) -> bool {
        self.s.is_empty()
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn starts(&self) -> &[usize] {
        &self.starts
    }

    /// Start of line `i` (the end of the text past the last line).
    pub fn line_start(&self, i: usize) -> usize {
        self.starts.get(i).copied().unwrap_or(self.s.len())
    }

    /// End of line `i`, before its line break.
    pub fn line_end(&self, i: usize) -> usize {
        self.starts
            .get(i + 1)
            .map(|e| e - 1)
            .unwrap_or(self.s.len())
    }

    /// Line `i` without its line break (empty past the end).
    pub fn line(&self, i: usize) -> &str {
        if i >= self.starts.len() {
            return "";
        }
        &self.s[self.line_start(i)..self.line_end(i)]
    }

    /// The line containing byte `offset` (the last one for offsets past the end).
    pub fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// `i` clamped to the text and moved back onto a character boundary, so
    /// offsets from stale state can be used safely.
    pub fn floor(&self, i: usize) -> usize {
        floor_char(&self.s, i)
    }

    /// `[a, b)` clamped onto character boundaries.
    pub fn slice(&self, a: usize, b: usize) -> &str {
        let b = self.floor(b);
        &self.s[self.floor(a).min(b)..b]
    }

    /// Apply `change` (whose base must be this text) and say which lines it replaced.
    pub fn apply(&mut self, change: &ChangeSet) -> Vec<LineSplice> {
        if change.base_len() != self.s.len() || change.is_empty() {
            return vec![];
        }
        let reps = change.replacements();
        // Group replacements that share a line: one splice each.
        struct Group {
            l0: usize,
            l1: usize,
            /// Change in length caused by the group.
            delta: isize,
        }
        let mut groups: Vec<Group> = Vec::new();
        for r in &reps {
            let (l0, l1) = (self.line_of(r.from), self.line_of(r.to));
            let d = r.insert.len() as isize - (r.to - r.from) as isize;
            match groups.last_mut() {
                Some(g) if l0 <= g.l1 => {
                    g.l1 = g.l1.max(l1);
                    g.delta += d;
                }
                _ => groups.push(Group { l0, l1, delta: d }),
            }
        }
        // The new text.
        if reps.len() <= 4 {
            for r in reps.iter().rev() {
                self.s.replace_range(r.from..r.to, r.insert);
            }
        } else {
            let mut out = String::with_capacity(change.target_len());
            let mut pos = 0;
            for r in &reps {
                out.push_str(&self.s[pos..r.from]);
                out.push_str(r.insert);
                pos = r.to;
            }
            out.push_str(&self.s[pos..]);
            self.s = out;
        }
        // The new line index: old starts outside the groups (shifted), new
        // ones scanned inside them.
        let old = std::mem::take(&mut self.starts);
        let old_end = |i: usize| old.get(i + 1).map(|e| e - 1);
        let mut starts = Vec::with_capacity(old.len());
        let mut splices = Vec::with_capacity(groups.len());
        let mut next_old = 0;
        let mut delta: isize = 0;
        let shift = |v: usize, d: isize| (v as isize + d) as usize;
        for g in &groups {
            starts.extend(old[next_old..g.l0].iter().map(|&v| shift(v, delta)));
            let ns = shift(old[g.l0], delta);
            delta += g.delta;
            let ne = match old_end(g.l1) {
                Some(e) => shift(e, delta),
                None => self.s.len(),
            };
            let new_start = starts.len();
            starts.push(ns);
            starts.extend(self.s[ns..ne].match_indices('\n').map(|(i, _)| ns + i + 1));
            splices.push(LineSplice {
                old_start: g.l0,
                old: g.l1 - g.l0 + 1,
                new_start,
                new: starts.len() - new_start,
            });
            next_old = g.l1 + 1;
        }
        starts.extend(old[next_old..].iter().map(|&v| shift(v, delta)));
        self.starts = starts;
        debug_assert_eq!(self.starts, line_starts(&self.s));
        splices
    }

    pub fn into_string(self) -> String {
        self.s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let t = Text::new("a\nbc\n\nd".into());
        assert_eq!(t.line_count(), 4);
        assert_eq!(
            (t.line(0), t.line(1), t.line(2), t.line(3)),
            ("a", "bc", "", "d")
        );
        assert_eq!(t.line(9), "");
        assert_eq!(t.line_of(0), 0);
        assert_eq!(t.line_of(1), 0);
        assert_eq!(t.line_of(2), 1);
        assert_eq!(t.line_of(5), 2);
        assert_eq!(t.line_of(7), 3);
        assert_eq!(t.line_of(99), 3);
        assert_eq!(Text::new(String::new()).line_count(), 1);
        let t = Text::new("a日b".into());
        assert_eq!(
            (t.floor(2), t.floor(3), t.floor(4), t.floor(99)),
            (1, 1, 4, 5)
        );
        assert_eq!(t.slice(2, 99), "日b");
        let s = "a😀b";
        assert_eq!(
            (
                utf16_to_byte(s, 1),
                utf16_to_byte(s, 3),
                utf16_to_byte(s, 9)
            ),
            (1, 5, 6)
        );
        assert_eq!(
            (
                byte_to_utf16(s, 1),
                byte_to_utf16(s, 3),
                byte_to_utf16(s, 5)
            ),
            (1, 1, 3)
        );
    }

    #[test]
    fn splices() {
        let mut t = Text::new("one\ntwo\nthree\nfour".into());
        // Type in line 1.
        let s = t.apply(&ChangeSet::replace(18, 5, 5, "X"));
        assert_eq!(t.as_str(), "one\ntXwo\nthree\nfour");
        assert_eq!(
            s,
            vec![LineSplice {
                old_start: 1,
                old: 1,
                new_start: 1,
                new: 1
            }]
        );
        // Enter in line 0, and join lines 2 and 3 in one change.
        let c = ChangeSet::from_edits(
            t.as_str(),
            &[
                notekpr_core::textdiff::Edit::Delete { at: 14, len: 1 },
                notekpr_core::textdiff::Edit::Insert {
                    at: 2,
                    text: "\n".into(),
                },
            ],
        )
        .unwrap();
        let s = t.apply(&c);
        assert_eq!(t.as_str(), "on\ne\ntXwo\nthreefour");
        assert_eq!(
            s,
            vec![
                LineSplice {
                    old_start: 0,
                    old: 1,
                    new_start: 0,
                    new: 2
                },
                LineSplice {
                    old_start: 2,
                    old: 2,
                    new_start: 3,
                    new: 1
                }
            ]
        );
        assert_eq!(t.starts(), line_starts(t.as_str()).as_slice());
    }
}
