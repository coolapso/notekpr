//! Turning local text changes into updates the server can store.
//!
//! The server refuses any update larger than its `max_update_bytes`. A huge
//! paste or import must therefore become several updates instead of one that
//! is refused forever: edits are grouped so that each group's update stays
//! within a byte budget, cutting long insertions at character boundaries.

use notekpr_core::doc::NoteDoc;
use notekpr_core::textdiff::{self, Edit};

/// Assumed encoding cost of one edit besides its inserted text (CRDT item
/// headers, ids, lengths).
const EDIT_OVERHEAD: usize = 32;

/// Split sequential edits into groups that each cost at most `budget` bytes
/// (inserted text plus a small overhead per edit), cutting large insertions at
/// character boundaries. Offsets refer to the text after the previous edits,
/// so applying the groups one after another — one update each — has the same
/// result as applying all edits at once.
pub fn split_edits(edits: &[Edit], budget: usize) -> Vec<Vec<Edit>> {
    let budget = budget.max(EDIT_OVERHEAD + 4);
    let mut groups: Vec<Vec<Edit>> = Vec::new();
    let mut cur: Vec<Edit> = Vec::new();
    let mut size = 0;
    let mut close = |cur: &mut Vec<Edit>, size: &mut usize| {
        if !cur.is_empty() {
            groups.push(std::mem::take(cur));
        }
        *size = 0;
    };
    for e in edits {
        match e {
            Edit::Delete { .. } => {
                if size + EDIT_OVERHEAD > budget {
                    close(&mut cur, &mut size);
                }
                cur.push(e.clone());
                size += EDIT_OVERHEAD;
            }
            Edit::Insert { at, text } => {
                let mut at = *at;
                let mut rest = text.as_str();
                while !rest.is_empty() {
                    if size + EDIT_OVERHEAD >= budget {
                        close(&mut cur, &mut size);
                    }
                    let room = budget - size - EDIT_OVERHEAD;
                    let mut cut = room.min(rest.len());
                    while !rest.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    if cut == 0 {
                        if cur.is_empty() {
                            // One character larger than the budget: take it whole.
                            cut = rest.chars().next().map(char::len_utf8).unwrap_or(1);
                        } else {
                            close(&mut cur, &mut size);
                            continue;
                        }
                    }
                    cur.push(Edit::Insert {
                        at,
                        text: rest[..cut].to_string(),
                    });
                    size += EDIT_OVERHEAD + cut;
                    at += cut;
                    rest = &rest[cut..];
                }
            }
        }
    }
    close(&mut cur, &mut size);
    groups
}

/// Apply `edits` to a note as one or more updates within `budget` bytes each.
pub fn apply_chunked(doc: &NoteDoc, edits: &[Edit], budget: usize) -> Vec<Vec<u8>> {
    split_edits(edits, budget)
        .iter()
        .filter_map(|g| doc.apply_edits(g))
        .collect()
}

/// Replace a note's text (diffing against the current text) as one or more
/// updates within `budget` bytes each. Empty when nothing changed.
pub fn set_text(doc: &NoteDoc, text: &str, budget: usize) -> Vec<Vec<u8>> {
    let edits = textdiff::diff(&doc.text(), text);
    apply_chunked(doc, &edits, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inserted(g: &[Edit]) -> usize {
        g.iter()
            .map(|e| match e {
                Edit::Insert { text, .. } => text.len() + EDIT_OVERHEAD,
                Edit::Delete { .. } => EDIT_OVERHEAD,
            })
            .sum()
    }

    #[test]
    fn a_huge_paste_becomes_several_updates_with_the_same_result() {
        let doc = NoteDoc::new();
        let first = doc.set_text("start\n").unwrap();
        let big = "é€x".repeat(20_000);
        let want = format!("start\n{big}\nend");
        let ups = set_text(&doc, &want, 4096);
        assert!(ups.len() > 10);
        for u in &ups {
            assert!(u.len() <= 4096 + 1024, "update of {} bytes", u.len());
        }
        // Another client applying them in order sees the same text.
        let other = NoteDoc::new();
        other.apply_update(&first).unwrap();
        for u in &ups {
            other.apply_update(u).unwrap();
        }
        assert_eq!(other.text(), want);
    }

    #[test]
    fn groups_respect_the_budget_and_char_boundaries() {
        let edits = vec![
            Edit::Insert {
                at: 0,
                text: "😀".repeat(100),
            },
            Edit::Delete { at: 10, len: 3 },
            Edit::Insert {
                at: 3,
                text: "abc".into(),
            },
        ];
        for g in split_edits(&edits, 128) {
            assert!(inserted(&g) <= 128 || g.len() == 1, "{g:?}");
            for e in &g {
                if let Edit::Insert { text, .. } = e {
                    assert!(!text.is_empty());
                }
            }
        }
    }

    #[test]
    fn nothing_changed_means_no_update() {
        let doc = NoteDoc::new();
        doc.set_text("same");
        assert!(set_text(&doc, "same", 4096).is_empty());
    }
}
