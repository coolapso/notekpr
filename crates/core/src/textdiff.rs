//! Turn "old text → new text" into a minimal-ish sequence of splices so that
//! edits made outside the CRDT (a file saved by Neovim, a full-content PUT from
//! an agent) become fine-grained CRDT operations that merge with concurrent
//! edits instead of overwriting them.

use similar::{Algorithm, DiffOp, capture_diff_slices};

/// A splice applied to the evolving text. Offsets are UTF-8 byte offsets into
/// the text as it is after all previous edits in the list have been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Delete { at: usize, len: usize },
    Insert { at: usize, text: String },
}

/// Above this many characters on both sides we diff line-by-line first.
const CHAR_DIFF_LIMIT: usize = 4000;

pub fn diff(old: &str, new: &str) -> Vec<Edit> {
    let prefix = common_prefix(old, new);
    let suffix = common_suffix(&old[prefix..], &new[prefix..]);
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];
    let mut edits = Vec::new();
    let mut pos = prefix;
    diff_region(old_mid, new_mid, &mut pos, &mut edits, true);
    edits
}

fn diff_region(old: &str, new: &str, pos: &mut usize, edits: &mut Vec<Edit>, allow_lines: bool) {
    if old.is_empty() && new.is_empty() {
        return;
    }
    if old.is_empty() || new.is_empty() {
        replace(old, new, pos, edits);
        return;
    }
    let old_chars = old.chars().count();
    let new_chars = new.chars().count();
    if old_chars <= CHAR_DIFF_LIMIT && new_chars <= CHAR_DIFF_LIMIT {
        char_diff(old, new, pos, edits);
    } else if allow_lines {
        line_diff(old, new, pos, edits);
    } else {
        replace(old, new, pos, edits);
    }
}

fn replace(old: &str, new: &str, pos: &mut usize, edits: &mut Vec<Edit>) {
    if !old.is_empty() {
        edits.push(Edit::Delete {
            at: *pos,
            len: old.len(),
        });
    }
    if !new.is_empty() {
        edits.push(Edit::Insert {
            at: *pos,
            text: new.to_string(),
        });
        *pos += new.len();
    }
}

fn char_diff(old: &str, new: &str, pos: &mut usize, edits: &mut Vec<Edit>) {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let a_off = byte_offsets(old);
    let b_off = byte_offsets(new);
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                *pos += a_off[old_index + len] - a_off[old_index]
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => edits.push(Edit::Delete {
                at: *pos,
                len: a_off[old_index + old_len] - a_off[old_index],
            }),
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                let text = &new[b_off[new_index]..b_off[new_index + new_len]];
                edits.push(Edit::Insert {
                    at: *pos,
                    text: text.to_string(),
                });
                *pos += text.len();
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                edits.push(Edit::Delete {
                    at: *pos,
                    len: a_off[old_index + old_len] - a_off[old_index],
                });
                let text = &new[b_off[new_index]..b_off[new_index + new_len]];
                edits.push(Edit::Insert {
                    at: *pos,
                    text: text.to_string(),
                });
                *pos += text.len();
            }
        }
    }
}

fn line_diff(old: &str, new: &str, pos: &mut usize, edits: &mut Vec<Edit>) {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let join = |l: &[&str]| l.concat();
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                *pos += a[old_index..old_index + len]
                    .iter()
                    .map(|l| l.len())
                    .sum::<usize>()
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => replace(&join(&a[old_index..old_index + old_len]), "", pos, edits),
            DiffOp::Insert {
                new_index, new_len, ..
            } => replace("", &join(&b[new_index..new_index + new_len]), pos, edits),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let o = join(&a[old_index..old_index + old_len]);
                let n = join(&b[new_index..new_index + new_len]);
                diff_region(&o, &n, pos, edits, false);
            }
        }
    }
}

fn byte_offsets(s: &str) -> Vec<usize> {
    let mut v: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    v.push(s.len());
    v
}

fn common_prefix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (ca, cb) in a.chars().zip(b.chars()) {
        if ca != cb {
            break;
        }
        n += ca.len_utf8();
    }
    n
}

fn common_suffix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (ca, cb) in a.chars().rev().zip(b.chars().rev()) {
        if ca != cb {
            break;
        }
        n += ca.len_utf8();
    }
    n.min(a.len()).min(b.len())
}

/// Apply edits to a plain string (used for tests and previews).
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut s = text.to_string();
    for e in edits {
        match e {
            Edit::Delete { at, len } => {
                s.replace_range(*at..*at + *len, "");
            }
            Edit::Insert { at, text } => s.insert_str(*at, text),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(a: &str, b: &str) {
        let e = diff(a, b);
        assert_eq!(apply(a, &e), b, "edits {e:?}");
    }

    #[test]
    fn diffs_roundtrip() {
        check("", "");
        check("", "abc");
        check("abc", "");
        check("hello world", "hello brave new world");
        check("aaa", "aa");
        check("héllo wörld ✓", "hallo wörld ✗!");
        check("line1\nline2\nline3\n", "line1\nline two\nline3\nline4\n");
        let big_a: String = (0..3000).map(|i| format!("line number {i}\n")).collect();
        let mut big_b = big_a.replace("line number 100\n", "changed 100\n");
        big_b.push_str("tail\n");
        check(&big_a, &big_b);
        let e = diff(&big_a, &big_b);
        assert!(e.len() <= 8, "large diffs stay small: {e:?}");
    }

    #[test]
    fn minimal_for_single_insert() {
        assert_eq!(
            diff("abc", "abXc"),
            vec![Edit::Insert {
                at: 2,
                text: "X".into()
            }]
        );
    }
}
