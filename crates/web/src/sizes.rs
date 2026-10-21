//! Keeping what we send within the server's size limits.
//!
//! The server refuses a push if any update's payload is larger than
//! `max_update_bytes` (from `/info`) and closes the WebSocket for frames
//! larger than about twice that. A refused push is resent forever, so one
//! oversized update would block a vault's sync for good: pushes are split by
//! size, local changes are split (or refused) before they become updates,
//! and updates that can never be stored are held back instead of sent.

use notekpr_core::protocol::PushUpdate;
use notekpr_core::textdiff::Edit;

/// The limit used until the server told us its own.
pub const DEFAULT_MAX_UPDATE: usize = 2 << 20;

/// Most updates sent in one push (the server accepts more).
pub const MAX_PUSH_COUNT: usize = 200;

/// Room for the CRDT encoding and the encryption envelope around inserted text.
const OVERHEAD: usize = 4096;

/// How many bytes of text one local change may insert so that its encrypted
/// update stays below `max_update`.
pub fn insert_budget(max_update: usize) -> usize {
    max_update.saturating_sub(OVERHEAD).max(1024)
}

/// Bytes of text a list of edits inserts.
pub fn inserted_bytes(edits: &[Edit]) -> usize {
    edits
        .iter()
        .map(|e| match e {
            Edit::Insert { text, .. } => text.len(),
            Edit::Delete { .. } => 0,
        })
        .sum()
}

/// Split sequential edits into groups that each insert at most `budget`
/// bytes, cutting large insertions at character boundaries. Applying the
/// groups one after another (one update each) has the same result as
/// applying all edits at once.
pub fn split_edits(edits: Vec<Edit>, budget: usize) -> Vec<Vec<Edit>> {
    let budget = budget.max(4);
    let mut groups: Vec<Vec<Edit>> = Vec::new();
    let mut cur: Vec<Edit> = Vec::new();
    let mut size = 0;
    for e in edits {
        match e {
            Edit::Delete { .. } => cur.push(e),
            Edit::Insert { at, text } => {
                let mut at = at;
                let mut rest = text.as_str();
                while !rest.is_empty() {
                    if size >= budget {
                        groups.push(std::mem::take(&mut cur));
                        size = 0;
                    }
                    let mut cut = (budget - size).min(rest.len());
                    while !rest.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    if cut == 0 {
                        if cur.is_empty() {
                            // A character larger than the budget: take it whole.
                            cut = rest.chars().next().map(char::len_utf8).unwrap_or(1);
                        } else {
                            groups.push(std::mem::take(&mut cur));
                            size = 0;
                            continue;
                        }
                    }
                    cur.push(Edit::Insert {
                        at,
                        text: rest[..cut].to_string(),
                    });
                    size += cut;
                    at += cut;
                    rest = &rest[cut..];
                }
            }
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
}

/// Split updates into pushes of at most [`MAX_PUSH_COUNT`] updates and about
/// `max_update` payload bytes (base64 in JSON keeps a full push well below
/// the server's frame limit). Updates larger than `max_update` on their own
/// can never be stored; they are returned separately, in order.
pub fn batches(ups: Vec<PushUpdate>, max_update: usize) -> (Vec<Vec<PushUpdate>>, Vec<PushUpdate>) {
    let mut out: Vec<Vec<PushUpdate>> = Vec::new();
    let mut oversized = Vec::new();
    let mut cur: Vec<PushUpdate> = Vec::new();
    let mut size = 0;
    for u in ups {
        let n = u.payload.len();
        if n > max_update {
            oversized.push(u);
            continue;
        }
        if !cur.is_empty() && (size + n > max_update || cur.len() >= MAX_PUSH_COUNT) {
            out.push(std::mem::take(&mut cur));
            size = 0;
        }
        size += n;
        cur.push(u);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    (out, oversized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notekpr_core::textdiff;

    fn up(id: usize, len: usize) -> PushUpdate {
        PushUpdate {
            object_id: "o".into(),
            client_update_id: id.to_string(),
            payload: vec![0; len],
            signature: None,
        }
    }

    #[test]
    fn batches_respect_size_count_and_order() {
        let ups: Vec<PushUpdate> = (0..500).map(|i| up(i, 10)).collect();
        let (b, over) = batches(ups, 1 << 20);
        assert!(over.is_empty());
        assert_eq!(
            b.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![200, 200, 100]
        );

        let ups = vec![up(0, 600), up(1, 600), up(2, 1001), up(3, 400), up(4, 1000)];
        let (b, over) = batches(ups, 1000);
        let ids: Vec<Vec<String>> = b
            .iter()
            .map(|x| x.iter().map(|u| u.client_update_id.clone()).collect())
            .collect();
        assert_eq!(ids, vec![vec!["0"], vec!["1", "3"], vec!["4"]]);
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].client_update_id, "2");
        for x in &b {
            assert!(x.iter().map(|u| u.payload.len()).sum::<usize>() <= 1000);
        }
    }

    #[test]
    fn split_edits_is_equivalent_and_bounded() {
        let old = "keep this line\nand this one\n".repeat(20);
        let new = format!(
            "{}{}",
            "日本語のテキストと emoji 👍 ".repeat(300),
            old.replace("this", "that")
        );
        for budget in [4, 5, 7, 64, 1000, 1 << 20] {
            let edits = textdiff::diff(&old, &new);
            let groups = split_edits(edits.clone(), budget);
            let mut t = old.clone();
            for g in &groups {
                assert!(inserted_bytes(g) <= budget.max(4), "budget {budget}");
                t = textdiff::apply(&t, g);
            }
            assert_eq!(t, new, "budget {budget}");
            assert_eq!(t, textdiff::apply(&old, &edits));
        }
        // One big paste becomes several updates.
        let big = vec![Edit::Insert {
            at: 0,
            text: "ü".repeat(5000),
        }];
        let groups = split_edits(big, 3001);
        assert_eq!(groups.len(), 4);
        assert!(groups.iter().all(|g| inserted_bytes(g) <= 3001));
    }

    #[test]
    fn budget_leaves_room_for_overhead() {
        assert_eq!(insert_budget(2 << 20), (2 << 20) - 4096);
        assert_eq!(insert_budget(100), 1024);
        assert_eq!(
            inserted_bytes(&[
                Edit::Delete { at: 0, len: 5 },
                Edit::Insert {
                    at: 0,
                    text: "日本".into()
                }
            ]),
            6
        );
    }
}
