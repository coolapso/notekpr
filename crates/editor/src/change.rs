//! Change sets: what a transaction does to the document.
//!
//! A [`ChangeSet`] is an operation list over the *whole* old document —
//! retain `n` bytes, delete `n` bytes, insert a string — in the style of
//! operational transformation. That gives the three things the editor needs
//! with small, well-known algorithms:
//!
//! * [`ChangeSet::compose`] — two consecutive changes as one (a command made of
//!   several steps, a native edit followed by a rebase);
//! * [`ChangeSet::transform`] — two concurrent changes made on the same text,
//!   each moved over the other (an IME composition and the remote edits that
//!   arrived while it was open);
//! * [`ChangeSet::map_pos`] — where a position (caret, remote cursor, a pending
//!   file drop) ends up after a change.
//!
//! Offsets are UTF-8 byte offsets, always on character boundaries. The host
//! (the note's CRDT) speaks [`Edit`] lists — sequential splices — and
//! [`ChangeSet::to_edits`] / [`ChangeSet::from_edits`] convert.

use coolpsonotes_core::textdiff::{self, Edit};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

/// Which side of an insertion made exactly at a position the position ends up on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    /// Stay before text inserted at the position (remote cursors).
    Before,
    /// Move past text inserted at the position (the local caret).
    After,
}

/// One contiguous replacement, in the coordinates of the old document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replacement<'a> {
    pub from: usize,
    pub to: usize,
    pub insert: &'a str,
}

/// A change from a document of `base_len()` bytes to one of `target_len()` bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    ops: Vec<Op>,
    base: usize,
    target: usize,
}

/// `s` split at `n`, moved back onto a character boundary (the algorithms
/// only split at boundaries when their inputs are valid; this keeps a bug from
/// becoming a panic).
fn split(s: &str, n: usize) -> (&str, &str) {
    let mut n = n.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    s.split_at(n)
}

impl ChangeSet {
    /// No change to a document of `len` bytes.
    pub fn empty(len: usize) -> ChangeSet {
        let mut c = ChangeSet::default();
        c.retain(len);
        c
    }

    /// Replace `[from, to)` of a document of `len` bytes with `insert`.
    pub fn replace(len: usize, from: usize, to: usize, insert: &str) -> ChangeSet {
        let to = to.min(len);
        let from = from.min(to);
        let mut c = ChangeSet::default();
        c.retain(from);
        c.insert(insert);
        c.delete(to - from);
        c.retain(len - to);
        c
    }

    /// Replacements `(from, to, insert)` of a document of `len` bytes, in
    /// order and not overlapping (out-of-order ones are skipped).
    pub fn from_replacements(len: usize, reps: &[(usize, usize, String)]) -> ChangeSet {
        let mut c = ChangeSet::default();
        let mut pos = 0;
        for (from, to, insert) in reps {
            if *from < pos || to < from || *to > len {
                continue;
            }
            c.retain(from - pos);
            c.insert(insert);
            c.delete(to - from);
            pos = *to;
        }
        c.retain(len - pos);
        c
    }

    /// The change that turns `old` into `new` (a fine-grained text diff).
    pub fn diff(old: &str, new: &str) -> ChangeSet {
        let edits = textdiff::diff(old, new);
        Self::from_monotonic(old.len(), &edits).unwrap_or_else(|| {
            // textdiff emits edits in document order; keep a fallback anyway.
            let pre = old
                .bytes()
                .zip(new.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            let mut pre = pre;
            while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
                pre -= 1;
            }
            ChangeSet::replace(old.len(), pre, old.len(), &new[pre..])
        })
    }

    /// Sequential edits (each relative to the text after the previous ones)
    /// on `text` as one change, or `None` if any of them leaves the text or
    /// splits a character.
    pub fn from_edits(text: &str, edits: &[Edit]) -> Option<ChangeSet> {
        if !edits_fit(text, edits) {
            return None;
        }
        if let Some(c) = Self::from_monotonic(text.len(), edits) {
            return Some(c);
        }
        let mut c = ChangeSet::empty(text.len());
        for e in edits {
            let len = c.target_len();
            let step = match e {
                Edit::Insert { at, text } => ChangeSet::replace(len, *at, *at, text),
                Edit::Delete { at, len: n } => ChangeSet::replace(len, *at, at + n, ""),
            };
            c = c.compose(&step);
        }
        Some(c)
    }

    /// Fast path for edits in document order (what diffs and our own
    /// [`ChangeSet::to_edits`] produce).
    fn from_monotonic(len: usize, edits: &[Edit]) -> Option<ChangeSet> {
        let mut c = ChangeSet::default();
        // Position in the evolving text, and how much of the old text is used.
        let (mut pos, mut used) = (0usize, 0usize);
        for e in edits {
            match e {
                Edit::Insert { at, text } => {
                    let skip = at.checked_sub(pos)?;
                    c.retain(skip);
                    used += skip;
                    c.insert(text);
                    pos = at + text.len();
                }
                Edit::Delete { at, len: n } => {
                    let skip = at.checked_sub(pos)?;
                    c.retain(skip);
                    used += skip;
                    c.delete(*n);
                    used += n;
                    pos = *at;
                }
            }
            if used > len {
                return None;
            }
        }
        c.retain(len - used);
        Some(c)
    }

    pub fn base_len(&self) -> usize {
        self.base
    }

    pub fn target_len(&self) -> usize {
        self.target
    }

    /// Whether the change leaves the document as it is.
    pub fn is_empty(&self) -> bool {
        self.ops.iter().all(|o| matches!(o, Op::Retain(_)))
    }

    /// Bytes of text this change inserts.
    pub fn inserted_len(&self) -> usize {
        self.ops
            .iter()
            .map(|o| match o {
                Op::Insert(s) => s.len(),
                _ => 0,
            })
            .sum()
    }

    // ------------------------------------------------------------ building

    fn retain(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.base += n;
        self.target += n;
        if let Some(Op::Retain(m)) = self.ops.last_mut() {
            *m += n;
        } else {
            self.ops.push(Op::Retain(n));
        }
    }

    fn delete(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.base += n;
        if let Some(Op::Delete(m)) = self.ops.last_mut() {
            *m += n;
        } else {
            self.ops.push(Op::Delete(n));
        }
    }

    /// Insertions go before deletions at the same position (one canonical form).
    fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.target += s.len();
        let n = self.ops.len();
        match self.ops.last_mut() {
            Some(Op::Insert(t)) => t.push_str(s),
            Some(Op::Delete(_)) => {
                if n >= 2
                    && let Op::Insert(t) = &mut self.ops[n - 2]
                {
                    t.push_str(s);
                } else {
                    self.ops.insert(n - 1, Op::Insert(s.to_string()));
                }
            }
            _ => self.ops.push(Op::Insert(s.to_string())),
        }
    }

    // ------------------------------------------------------------ using

    /// `text` with the change applied; `None` if the lengths don't match.
    pub fn apply(&self, text: &str) -> Option<String> {
        if text.len() != self.base {
            return None;
        }
        let mut out = String::with_capacity(self.target);
        let mut pos = 0;
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    out.push_str(text.get(pos..pos + n)?);
                    pos += n;
                }
                Op::Delete(n) => pos += n,
                Op::Insert(s) => out.push_str(s),
            }
        }
        Some(out)
    }

    /// The change as sequential splices (the host's format).
    pub fn to_edits(&self) -> Vec<Edit> {
        let mut out = Vec::new();
        let mut pos = 0;
        for op in &self.ops {
            match op {
                Op::Retain(n) => pos += n,
                Op::Delete(n) => out.push(Edit::Delete { at: pos, len: *n }),
                Op::Insert(s) => {
                    out.push(Edit::Insert {
                        at: pos,
                        text: s.clone(),
                    });
                    pos += s.len();
                }
            }
        }
        out
    }

    /// The replaced ranges, in document order and old-document coordinates.
    pub fn replacements(&self) -> Vec<Replacement<'_>> {
        let mut out = Vec::new();
        let mut cur: Option<Replacement> = None;
        let mut pos = 0;
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    out.extend(cur.take());
                    pos += n;
                }
                Op::Delete(n) => {
                    let r = cur.get_or_insert(Replacement {
                        from: pos,
                        to: pos,
                        insert: "",
                    });
                    pos += n;
                    r.to = pos;
                }
                Op::Insert(s) => {
                    let r = cur.get_or_insert(Replacement {
                        from: pos,
                        to: pos,
                        insert: "",
                    });
                    r.insert = s;
                }
            }
        }
        out.extend(cur);
        out
    }

    /// Where `pos` (in the old document) is in the new one. A position inside
    /// deleted text lands where the deletion was.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        let (mut old, mut new) = (0, 0);
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    if pos < old + n {
                        return new + (pos - old);
                    }
                    old += n;
                    new += n;
                }
                Op::Delete(n) => {
                    if pos < old + n {
                        return new;
                    }
                    old += n;
                }
                Op::Insert(s) => {
                    if pos == old && assoc == Assoc::Before {
                        return new;
                    }
                    new += s.len();
                }
            }
        }
        (new + pos.saturating_sub(old)).min(self.target)
    }

    /// This change followed by `next` (which applies to this change's result).
    pub fn compose(&self, next: &ChangeSet) -> ChangeSet {
        let mut out = ChangeSet::default();
        let mut a = self.ops.iter().cloned();
        let mut b = next.ops.iter().cloned();
        let (mut x, mut y) = (a.next(), b.next());
        loop {
            match (x.take(), y.take()) {
                (None, None) => break,
                (Some(Op::Delete(n)), o) => {
                    out.delete(n);
                    x = a.next();
                    y = o;
                }
                (o, Some(Op::Insert(s))) => {
                    out.insert(&s);
                    x = o;
                    y = b.next();
                }
                // Lengths don't match: a bug elsewhere. Keep what there is.
                (None, Some(_)) | (Some(_), None) => break,
                (Some(Op::Retain(n)), Some(Op::Retain(m))) => {
                    let k = n.min(m);
                    out.retain(k);
                    x = if n > k {
                        Some(Op::Retain(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Retain(m - k))
                    } else {
                        b.next()
                    };
                }
                (Some(Op::Insert(s)), Some(Op::Delete(m))) => {
                    // Inserted, then deleted again: nothing.
                    let l = s.len();
                    x = if l > m {
                        Some(Op::Insert(split(&s, m).1.to_string()))
                    } else {
                        a.next()
                    };
                    y = if m > l {
                        Some(Op::Delete(m - l))
                    } else {
                        b.next()
                    };
                }
                (Some(Op::Insert(s)), Some(Op::Retain(m))) => {
                    let l = s.len();
                    if l > m {
                        let (head, tail) = split(&s, m);
                        out.insert(head);
                        x = Some(Op::Insert(tail.to_string()));
                        y = b.next();
                    } else {
                        out.insert(&s);
                        x = a.next();
                        y = if m > l {
                            Some(Op::Retain(m - l))
                        } else {
                            b.next()
                        };
                    }
                }
                (Some(Op::Retain(n)), Some(Op::Delete(m))) => {
                    let k = n.min(m);
                    out.delete(k);
                    x = if n > k {
                        Some(Op::Retain(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Delete(m - k))
                    } else {
                        b.next()
                    };
                }
            }
        }
        out
    }

    /// Two changes made concurrently on the same text, each moved over the
    /// other: `(self', other')` with `self` then `other'` equal to `other`
    /// then `self'`. Where both insert at the same position, `self`'s text
    /// comes first.
    pub fn transform(&self, other: &ChangeSet) -> (ChangeSet, ChangeSet) {
        let (mut a2, mut b2) = (ChangeSet::default(), ChangeSet::default());
        let mut a = self.ops.iter().cloned();
        let mut b = other.ops.iter().cloned();
        let (mut x, mut y) = (a.next(), b.next());
        loop {
            match (x.take(), y.take()) {
                (None, None) => break,
                (Some(Op::Insert(s)), o) => {
                    a2.insert(&s);
                    b2.retain(s.len());
                    x = a.next();
                    y = o;
                }
                (o, Some(Op::Insert(s))) => {
                    a2.retain(s.len());
                    b2.insert(&s);
                    x = o;
                    y = b.next();
                }
                (None, Some(_)) | (Some(_), None) => break,
                (Some(Op::Retain(n)), Some(Op::Retain(m))) => {
                    let k = n.min(m);
                    a2.retain(k);
                    b2.retain(k);
                    x = if n > k {
                        Some(Op::Retain(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Retain(m - k))
                    } else {
                        b.next()
                    };
                }
                (Some(Op::Delete(n)), Some(Op::Delete(m))) => {
                    let k = n.min(m);
                    x = if n > k {
                        Some(Op::Delete(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Delete(m - k))
                    } else {
                        b.next()
                    };
                }
                (Some(Op::Delete(n)), Some(Op::Retain(m))) => {
                    let k = n.min(m);
                    a2.delete(k);
                    x = if n > k {
                        Some(Op::Delete(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Retain(m - k))
                    } else {
                        b.next()
                    };
                }
                (Some(Op::Retain(n)), Some(Op::Delete(m))) => {
                    let k = n.min(m);
                    b2.delete(k);
                    x = if n > k {
                        Some(Op::Retain(n - k))
                    } else {
                        a.next()
                    };
                    y = if m > k {
                        Some(Op::Delete(m - k))
                    } else {
                        b.next()
                    };
                }
            }
        }
        (a2, b2)
    }
}

/// Whether sequential edits stay inside `text` and on character boundaries
/// (anything else would panic or corrupt the CRDT).
pub fn edits_fit(text: &str, edits: &[Edit]) -> bool {
    let mut t = text.to_string();
    for e in edits {
        match e {
            Edit::Delete { at, len } => {
                let end = at.saturating_add(*len);
                if end > t.len() || !t.is_char_boundary(*at) || !t.is_char_boundary(end) {
                    return false;
                }
                t.replace_range(*at..end, "");
            }
            Edit::Insert { at, text } => {
                if *at > t.len() || !t.is_char_boundary(*at) {
                    return false;
                }
                t.insert_str(*at, text);
            }
        }
    }
    true
}

/// Move local edits made on `base` onto `current` (`base` plus changes made
/// elsewhere meanwhile), merging like concurrent edits: the local edits are
/// transformed over the others (whose text goes first where both inserted at
/// the same place). `None` if the edits don't fit `base`.
pub fn rebase(base: &str, current: &str, edits: &[Edit]) -> Option<Vec<Edit>> {
    let local = ChangeSet::from_edits(base, edits)?;
    let remote = ChangeSet::diff(base, current);
    let (_, moved) = remote.transform(&local);
    let out = moved.to_edits();
    edits_fit(current, &out).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(at: usize, t: &str) -> Edit {
        Edit::Insert { at, text: t.into() }
    }
    fn del(at: usize, len: usize) -> Edit {
        Edit::Delete { at, len }
    }

    #[test]
    fn edits_round_trip() {
        let t = "hello world";
        let edits = vec![del(0, 5), ins(0, "goodbye"), ins(13, "!")];
        let c = ChangeSet::from_edits(t, &edits).unwrap();
        assert_eq!(c.apply(t).unwrap(), "goodbye world!");
        assert_eq!(textdiff::apply(t, &c.to_edits()), "goodbye world!");
        // Out of order edits go through composition.
        let edits = vec![ins(6, "big "), ins(0, "a ")];
        let c = ChangeSet::from_edits(t, &edits).unwrap();
        assert_eq!(c.apply(t).unwrap(), "a hello big world");
        assert_eq!(textdiff::apply(t, &c.to_edits()), "a hello big world");
        assert!(ChangeSet::from_edits("日本", &[ins(1, "x")]).is_none());
    }

    #[test]
    fn positions_follow_changes() {
        // "hello world" → "hello brave new world!"
        let c =
            ChangeSet::from_edits("hello world", &[ins(6, "brave new "), ins(21, "!")]).unwrap();
        assert_eq!(c.map_pos(0, Assoc::After), 0);
        assert_eq!(c.map_pos(6, Assoc::After), 16);
        assert_eq!(c.map_pos(6, Assoc::Before), 6);
        assert_eq!(c.map_pos(11, Assoc::After), 22);
        assert_eq!(c.map_pos(11, Assoc::Before), 21);
        // "one two three" → "one three"
        let c = ChangeSet::replace(13, 4, 8, "");
        assert_eq!(c.map_pos(5, Assoc::After), 4);
        assert_eq!(c.map_pos(13, Assoc::After), 9);
        assert_eq!(c.map_pos(2, Assoc::After), 2);
        assert_eq!(c.map_pos(99, Assoc::After), 9);
    }

    #[test]
    fn compose_and_transform() {
        let t = "abc";
        let a = ChangeSet::replace(3, 1, 2, "XY"); // aXYc
        let b = ChangeSet::replace(4, 0, 1, ""); // XYc
        assert_eq!(a.compose(&b).apply(t).unwrap(), "XYc");
        // Concurrent: insert at the same position, self first.
        let a = ChangeSet::replace(3, 1, 1, "1");
        let b = ChangeSet::replace(3, 1, 1, "2");
        let (a2, b2) = a.transform(&b);
        let x = b2.apply(&a.apply(t).unwrap()).unwrap();
        let y = a2.apply(&b.apply(t).unwrap()).unwrap();
        assert_eq!(x, y);
        assert_eq!(x, "a12bc");
        // Overlapping deletions.
        let a = ChangeSet::replace(3, 0, 2, "");
        let b = ChangeSet::replace(3, 1, 3, "Z");
        let (a2, b2) = a.transform(&b);
        assert_eq!(
            b2.apply(&a.apply(t).unwrap()),
            a2.apply(&b.apply(t).unwrap())
        );
    }

    #[test]
    fn replacements_in_old_coordinates() {
        let c =
            ChangeSet::from_edits("one two three", &[del(4, 3), ins(4, "2"), ins(0, ">")]).unwrap();
        let r = c.replacements();
        assert_eq!(
            r,
            vec![
                Replacement {
                    from: 0,
                    to: 0,
                    insert: ">"
                },
                Replacement {
                    from: 4,
                    to: 7,
                    insert: "2"
                }
            ]
        );
    }

    #[test]
    fn composition_is_rebased_onto_remote_changes() {
        let base = "Shopping: milk\neggs";
        // Someone else appended a line while we were composing at the start.
        let current = "Shopping: milk\neggs\nbread";
        let local = textdiff::diff(base, "かなShopping: milk\neggs");
        let e = rebase(base, current, &local).unwrap();
        assert_eq!(
            textdiff::apply(current, &e),
            "かなShopping: milk\neggs\nbread"
        );
        // …or inserted text before our composition.
        let current = "Remote. Shopping: milk\neggs";
        let e = rebase(base, current, &[ins(15, "日本")]).unwrap();
        assert_eq!(
            textdiff::apply(current, &e),
            "Remote. Shopping: milk\n日本eggs"
        );
        // A replacement of text nobody else touched moves along too.
        let local = vec![del(10, 4), ins(10, "ミルク")];
        let e = rebase(base, current, &local).unwrap();
        assert_eq!(
            textdiff::apply(current, &e),
            "Remote. Shopping: ミルク\neggs"
        );
        let current = "Shopping: oat milk\neggs";
        let e = rebase(base, current, &local).unwrap();
        assert_eq!(textdiff::apply(current, &e), "Shopping: oat ミルク\neggs");
        // If the replaced text changed elsewhere too, both changes are kept,
        // the way concurrent edits merge in the CRDT: what is left of the
        // replaced text goes, the other side's insertion stays.
        let current = "Shopping: mi1k\neggs";
        let e = rebase(base, current, &local).unwrap();
        assert_eq!(textdiff::apply(current, &e), "Shopping: ミルク1\neggs");
        // Several separate edits move together.
        let local = vec![ins(0, "a"), ins(5, "b")];
        let current = "Shopping: milk\neggs!";
        let e = rebase(base, current, &local).unwrap();
        assert_eq!(textdiff::apply(current, &e), "aShopbping: milk\neggs!");
        // Edits that don't fit the base are refused.
        assert_eq!(rebase(base, current, &[del(0, 99)]), None);
    }

    #[test]
    fn edits_are_checked_against_the_text() {
        assert!(edits_fit("日本", &[ins(3, "x"), del(0, 3)]));
        assert!(edits_fit("", &[ins(0, "a")]));
        assert!(!edits_fit("日本", &[ins(1, "x")]));
        assert!(!edits_fit("日本", &[ins(7, "x")]));
        assert!(!edits_fit("日本", &[del(3, 4)]));
        assert!(!edits_fit("日本", &[del(0, 2)]));
        assert!(!edits_fit("ab", &[del(0, usize::MAX)]));
        // Later edits are checked against the text as it is by then.
        assert!(edits_fit("ab", &[ins(2, "cd"), del(3, 1)]));
        assert!(!edits_fit("ab", &[del(0, 2), ins(1, "x")]));
    }
}
