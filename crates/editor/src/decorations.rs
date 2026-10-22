//! Decorations: the Markdown block of every line, kept up to date
//! incrementally.
//!
//! A line's block depends on the state the lines above leave
//! ([`LineState`]), the line and the line after it ([`syntax::step`]). After
//! an edit only the edited lines, the line before them (it may have become a
//! table header) and the lines after them up to the first one whose start
//! state is what it was before need classifying again. Typing in a paragraph
//! touches one or two lines; opening a code fence re-classifies to the end of
//! the note, as it must.
//!
//! The inline styling of a line (bold, links, tables cells, …) depends on the
//! line and its block only, so the view re-styles exactly the lines reported
//! here.

use std::ops::Range;

use crate::syntax::{self, Block, LineState};
use crate::text::{LineSplice, Text};

#[derive(Clone, Debug, Default)]
pub struct Decorations {
    /// State at the start of each line.
    states: Vec<LineState>,
    blocks: Vec<Block>,
}

/// Merge overlapping or touching ranges (input in any order).
pub fn merge_ranges(mut v: Vec<Range<usize>>) -> Vec<Range<usize>> {
    v.retain(|r| r.start < r.end);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

impl Decorations {
    pub fn new(doc: &Text) -> Decorations {
        let n = doc.line_count();
        let mut d = Decorations {
            states: Vec::with_capacity(n),
            blocks: Vec::with_capacity(n),
        };
        let mut state = LineState::Start;
        for i in 0..n {
            let next = (i + 1 < n).then(|| doc.line(i + 1));
            let (b, s) = syntax::step(&state, doc.line(i), next);
            d.states.push(state);
            d.blocks.push(b);
            state = s;
        }
        d
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn block(&self, i: usize) -> &Block {
        self.blocks.get(i).unwrap_or(&Block::Blank)
    }

    /// Follow `doc` after it changed by `splices`. Returns the lines (of the
    /// new document) that need rendering again: every spliced line, and the
    /// lines whose block changed.
    pub fn update(&mut self, doc: &Text, splices: &[LineSplice]) -> Vec<Range<usize>> {
        if splices.is_empty() {
            return vec![];
        }
        for sp in splices.iter().rev() {
            let r = sp.old_start..sp.old_start + sp.old;
            self.states
                .splice(r.clone(), std::iter::repeat_n(LineState::Normal, sp.new));
            self.blocks
                .splice(r, std::iter::repeat_n(Block::Blank, sp.new));
        }
        let n = doc.line_count();
        debug_assert_eq!(self.blocks.len(), n);
        if self.blocks.len() != n {
            // Out of step (a bug): start over rather than misrender.
            *self = Decorations::new(doc);
            return std::iter::once(0..n).collect();
        }
        self.states[0] = LineState::Start;
        let mut dirty: Vec<Range<usize>> = splices.iter().map(LineSplice::new_range).collect();
        // Lines below `done` are final.
        let mut done = 0;
        for sp in splices {
            let end = sp.new_start + sp.new;
            let mut i = sp.new_start.saturating_sub(1).max(done);
            while i < n {
                let next = (i + 1 < n).then(|| doc.line(i + 1));
                let (b, s) = syntax::step(&self.states[i], doc.line(i), next);
                if self.blocks[i] != b {
                    dirty.push(i..i + 1);
                    self.blocks[i] = b;
                }
                i += 1;
                if i >= n {
                    break;
                }
                // Past the edit, an unchanged start state means the rest is
                // classified as before.
                if i >= end && self.states[i] == s {
                    break;
                }
                self.states[i] = s;
            }
            done = done.max(i);
        }
        merge_ranges(dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ChangeSet;

    fn check(before: &str, from: usize, to: usize, insert: &str) -> Vec<Range<usize>> {
        let mut doc = Text::new(before.to_string());
        let mut d = Decorations::new(&doc);
        let old_blocks = d.blocks().to_vec();
        let splices = doc.apply(&ChangeSet::replace(before.len(), from, to, insert));
        let dirty = d.update(&doc, &splices);
        let lines: Vec<&str> = doc.as_str().split('\n').collect();
        assert_eq!(
            d.blocks(),
            syntax::classify(&lines).as_slice(),
            "{before:?}"
        );
        // Every line whose block differs from before (where it has a
        // counterpart) is reported.
        let delta = doc.line_count() as isize - old_blocks.len() as isize;
        for (i, b) in d.blocks().iter().enumerate() {
            let old = if splices.iter().any(|s| s.new_range().contains(&i)) {
                None
            } else if i < splices[0].new_start {
                old_blocks.get(i)
            } else {
                old_blocks.get((i as isize - delta) as usize)
            };
            if old != Some(b) {
                assert!(
                    dirty.iter().any(|r| r.contains(&i)),
                    "line {i} in {before:?}"
                );
            }
        }
        dirty
    }

    #[test]
    fn typing_in_a_paragraph_touches_one_line() {
        let t = "# T\n\npara\nmore\n\n- a";
        assert_eq!(check(t, 12, 12, "x"), vec![3..4]);
    }

    #[test]
    fn opening_a_fence_reclassifies_below() {
        let t = "a\nb\nc\nd";
        let dirty = check(t, 0, 0, "```\n");
        assert_eq!(dirty, vec![0..5]);
        // Closing it again.
        check("```\na\nb", 8, 8, "\n```");
    }

    #[test]
    fn a_block_comment_rehighlights_the_code_below_it() {
        let t = "```c\na\nb\nc\n```\nafter";
        // "a" becomes "/* a": the rest of the block is comment, not "after".
        assert_eq!(check(t, 5, 5, "/* "), vec![1..4]);
        // Typing where nothing is open touches one line.
        assert_eq!(check(t, 7, 7, "x"), vec![2..3]);
        // Highlighted lines move with the lines above them.
        let dirty = check("```c hl=2\na\nb\nc\n```", 10, 10, "z\n");
        assert!(dirty.iter().any(|r| r.contains(&3)), "{dirty:?}");
    }

    #[test]
    fn a_delimiter_row_makes_the_line_above_a_header() {
        let t = "| a | b |\nx\ny";
        let dirty = check(t, 10, 11, "| - | - |");
        assert!(dirty.iter().any(|r| r.contains(&0)));
        check("| a | b |\n| - | - |\n| 1 | 2 |\nafter", 10, 19, "");
        check("---\ntitle: x\n---\nbody", 0, 3, "");
        check("a\n---\nb", 0, 0, "---\n");
    }
}
