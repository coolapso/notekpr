//! The stacking order of a drawing's elements: each has a fractional index
//! (`index`), a string that sorts between its neighbours'. Moving an element
//! up or down changes only its own index, so two people reordering at once
//! both keep their change — the order is the sort of the indices.
//!
//! Ported from fractional-indexing 3.3.0 as Excalidraw vendors it (CC0),
//! keys in base 62 like Excalidraw's.

use std::collections::HashSet;

use crate::scene::{Element, num};
use serde_json::Value;

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ZERO: u8 = DIGITS[0];

fn digit(c: u8) -> Option<usize> {
    DIGITS.iter().position(|&d| d == c)
}

/// The smallest integer part: below it there is nothing.
fn smallest_integer() -> String {
    format!("A{}", "0".repeat(26))
}

fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        // Drop the common prefix, `a` padded with zeros as we go.
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(ZERO) == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or_default(), Some(&b[n..])));
            return out;
        }
    }
    // The first digits (or lack of one) differ.
    let digit_a = a.first().and_then(|&c| digit(c)).unwrap_or(0);
    let digit_b = b.map_or(DIGITS.len(), |b| {
        b.first().and_then(|&c| digit(c)).unwrap_or(0)
    });
    if digit_b - digit_a > 1 {
        let mid = crate::js::round(0.5 * (digit_a + digit_b) as f64) as usize;
        return vec![DIGITS[mid]];
    }
    // Consecutive first digits.
    if let Some(b) = b
        && b.len() > 1
    {
        return b[..1].to_vec();
    }
    let mut out = vec![DIGITS[digit_a]];
    out.extend(midpoint(a.get(1..).unwrap_or_default(), None));
    out
}

fn integer_length(head: u8) -> Option<usize> {
    match head {
        b'a'..=b'z' => Some((head - b'a') as usize + 2),
        b'A'..=b'Z' => Some((b'Z' - head) as usize + 2),
        _ => None,
    }
}

fn integer_part(key: &[u8]) -> Option<&[u8]> {
    let n = integer_length(*key.first()?)?;
    (n <= key.len()).then(|| &key[..n])
}

/// Whether `key` is a fractional index (`validateOrderKey`).
pub fn is_valid(key: &str) -> bool {
    let k = key.as_bytes();
    if key == smallest_integer() || !k.iter().all(|&c| digit(c).is_some()) {
        return false;
    }
    let Some(int) = integer_part(k) else {
        return false;
    };
    k[int.len()..].last() != Some(&ZERO)
}

fn increment_integer(x: &[u8]) -> Option<Vec<u8>> {
    let head = x[0];
    let mut digs = x[1..].to_vec();
    let mut carry = true;
    for d in digs.iter_mut().rev() {
        if !carry {
            break;
        }
        let next = digit(*d)? + 1;
        if next == DIGITS.len() {
            *d = ZERO;
        } else {
            *d = DIGITS[next];
            carry = false;
        }
    }
    if carry {
        if head == b'Z' {
            return Some(vec![b'a', ZERO]);
        }
        if head == b'z' {
            return None;
        }
        let h = head + 1;
        if h > b'a' {
            digs.push(ZERO);
        } else {
            digs.pop();
        }
        let mut out = vec![h];
        out.extend(digs);
        return Some(out);
    }
    let mut out = vec![head];
    out.extend(digs);
    Some(out)
}

fn decrement_integer(x: &[u8]) -> Option<Vec<u8>> {
    let head = x[0];
    let last = *DIGITS.last().expect("digits");
    let mut digs = x[1..].to_vec();
    let mut borrow = true;
    for d in digs.iter_mut().rev() {
        if !borrow {
            break;
        }
        match digit(*d)? {
            0 => *d = last,
            n => {
                *d = DIGITS[n - 1];
                borrow = false;
            }
        }
    }
    if borrow {
        if head == b'a' {
            return Some(vec![b'Z', last]);
        }
        if head == b'A' {
            return None;
        }
        let h = head - 1;
        if h < b'Z' {
            digs.push(last);
        } else {
            digs.pop();
        }
        let mut out = vec![h];
        out.extend(digs);
        return Some(out);
    }
    let mut out = vec![head];
    out.extend(digs);
    Some(out)
}

/// A key between `a` and `b` (either may be open: `None`); `None` if the
/// keys are invalid or out of order, or there's no room (practically never).
pub fn key_between(a: Option<&str>, b: Option<&str>) -> Option<String> {
    if a.is_some_and(|a| !is_valid(a)) || b.is_some_and(|b| !is_valid(b)) {
        return None;
    }
    if let (Some(a), Some(b)) = (a, b)
        && a >= b
    {
        return None;
    }
    let out = match (a.map(str::as_bytes), b.map(str::as_bytes)) {
        (None, None) => vec![b'a', ZERO],
        (None, Some(b)) => {
            let ib = integer_part(b)?;
            let fb = &b[ib.len()..];
            if ib == smallest_integer().as_bytes() {
                let mut out = ib.to_vec();
                out.extend(midpoint(&[], Some(fb)));
                out
            } else if ib < b {
                ib.to_vec()
            } else {
                decrement_integer(ib)?
            }
        }
        (Some(a), None) => {
            let ia = integer_part(a)?;
            let fa = &a[ia.len()..];
            match increment_integer(ia) {
                Some(i) => i,
                None => {
                    let mut out = ia.to_vec();
                    out.extend(midpoint(fa, None));
                    out
                }
            }
        }
        (Some(a), Some(b)) => {
            let ia = integer_part(a)?;
            let fa = &a[ia.len()..];
            let ib = integer_part(b)?;
            let fb = &b[ib.len()..];
            if ia == ib {
                let mut out = ia.to_vec();
                out.extend(midpoint(fa, Some(fb)));
                out
            } else {
                let i = increment_integer(ia)?;
                if i.as_slice() < b {
                    i
                } else {
                    let mut out = ia.to_vec();
                    out.extend(midpoint(fa, None));
                    out
                }
            }
        }
    };
    String::from_utf8(out).ok()
}

/// `n` keys in order between `a` and `b` (`generateNKeysBetween`).
pub fn keys_between(a: Option<&str>, b: Option<&str>, n: usize) -> Option<Vec<String>> {
    match n {
        0 => Some(Vec::new()),
        1 => Some(vec![key_between(a, b)?]),
        _ if b.is_none() => {
            let mut c = key_between(a, b)?;
            let mut out = vec![c.clone()];
            for _ in 1..n {
                c = key_between(Some(&c), b)?;
                out.push(c.clone());
            }
            Some(out)
        }
        _ if a.is_none() => {
            let mut c = key_between(a, b)?;
            let mut out = vec![c.clone()];
            for _ in 1..n {
                c = key_between(a, Some(&c))?;
                out.push(c.clone());
            }
            out.reverse();
            Some(out)
        }
        _ => {
            let mid = n / 2;
            let c = key_between(a, b)?;
            let mut out = keys_between(a, Some(&c), mid)?;
            out.push(c.clone());
            out.extend(keys_between(Some(&c), b, n - mid - 1)?);
            Some(out)
        }
    }
}

/// Gives every element whose index doesn't fit its place in the list (none,
/// malformed, or out of order) a new one between its neighbours', keeping
/// the list's order and as many indices as possible. Returns how many
/// changed (their `version` goes up, as Excalidraw does).
///
/// The indices kept are the longest run (not necessarily adjacent) that is
/// already in order: as few elements as possible get new ones.
pub fn sync_invalid_indices(elements: &mut [Element]) -> usize {
    let index = |e: &Element| e.index().filter(|i| is_valid(i)).map(str::to_string);
    let indices: Vec<Option<String>> = elements.iter().map(index).collect();
    let n = indices.len();
    let keep = longest_increasing(&indices);
    let mut changed = 0;
    let mut i = 0;
    while i < n {
        if keep[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && !keep[i] {
            i += 1;
        }
        let lower = start.checked_sub(1).and_then(|p| indices[p].as_deref());
        let upper = indices.get(i).and_then(Option::as_deref).filter(|_| i < n);
        let fresh = keys_between(lower, upper, i - start).expect("kept indices are in order");
        for (e, key) in elements[start..i].iter_mut().zip(fresh) {
            e.set("index", Value::String(key));
            e.set("version", num(e.version() as f64 + 1.0));
            changed += 1;
        }
    }
    changed
}

/// Gives the elements in `moved` (by id) new indices between those of the
/// neighbours that stayed (`syncMovedIndices`), their `version` up. If that
/// can't keep the whole list in order (the neighbours' indices aren't), it
/// repairs every index out of place instead ([`sync_invalid_indices`]).
/// Returns how many changed.
pub fn sync_moved_indices(elements: &mut [Element], moved: &HashSet<String>) -> usize {
    let n = elements.len();
    let mut fresh: Vec<Option<String>> = vec![None; n];
    let mut i = 0;
    while i < n {
        if !moved.contains(elements[i].id()) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && moved.contains(elements[i].id()) {
            i += 1;
        }
        let lower = start.checked_sub(1).and_then(|p| elements[p].index());
        let upper = elements.get(i).and_then(Element::index);
        let Some(keys) = keys_between(lower, upper, i - start) else {
            return sync_invalid_indices(elements);
        };
        for (slot, key) in fresh[start..i].iter_mut().zip(keys) {
            *slot = Some(key);
        }
    }
    let index = |k: usize| fresh[k].as_deref().or(elements[k].index());
    let in_order =
        (0..n).all(|k| index(k).is_some_and(is_valid)) && (1..n).all(|k| index(k - 1) < index(k));
    if !in_order {
        return sync_invalid_indices(elements);
    }
    let mut changed = 0;
    for (e, key) in elements.iter_mut().zip(fresh) {
        if let Some(key) = key {
            e.set("index", Value::String(key));
            e.set("version", num(e.version() as f64 + 1.0));
            changed += 1;
        }
    }
    changed
}

/// Which of `indices` form a longest strictly increasing subsequence
/// (patience sorting: O(n log n)); missing ones never do.
fn longest_increasing(indices: &[Option<String>]) -> Vec<bool> {
    // tails[k]: the position ending the best run of length k + 1 so far.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; indices.len()];
    for (i, key) in indices.iter().enumerate() {
        let Some(key) = key else {
            continue;
        };
        let at = tails.partition_point(|&t| indices[t].as_ref().expect("in a run") < key);
        prev[i] = at.checked_sub(1).map(|p| tails[p]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut keep = vec![false; indices.len()];
    let mut at = tails.last().copied();
    while let Some(i) = at {
        keep[i] = true;
        at = prev[i];
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_like_the_original() {
        // From fractional-indexing's own tests.
        let k = |a: Option<&str>, b: Option<&str>| key_between(a, b);
        assert_eq!(k(None, None).as_deref(), Some("a0"));
        assert_eq!(k(None, Some("a0")).as_deref(), Some("Zz"));
        assert_eq!(k(None, Some("Zz")).as_deref(), Some("Zy"));
        assert_eq!(k(Some("a0"), None).as_deref(), Some("a1"));
        assert_eq!(k(Some("a1"), None).as_deref(), Some("a2"));
        assert_eq!(k(Some("a0"), Some("a1")).as_deref(), Some("a0V"));
        assert_eq!(k(Some("a1"), Some("a2")).as_deref(), Some("a1V"));
        assert_eq!(k(Some("a0V"), Some("a1")).as_deref(), Some("a0l"));
        assert_eq!(k(Some("Zz"), Some("a0")).as_deref(), Some("ZzV"));
        assert_eq!(k(Some("Zz"), Some("a1")).as_deref(), Some("a0"));
        assert_eq!(k(None, Some("Y00")).as_deref(), Some("Xzzz"));
        assert_eq!(k(Some("bzz"), None).as_deref(), Some("c000"));
        assert_eq!(k(Some("a0"), Some("a0V")).as_deref(), Some("a0G"));
        assert_eq!(k(Some("a0"), Some("a0G")).as_deref(), Some("a08"));
        assert_eq!(k(Some("b125"), Some("b129")).as_deref(), Some("b127"));
        assert_eq!(k(Some("a0"), Some("a1V")).as_deref(), Some("a1"));
        assert_eq!(k(Some("Zz"), Some("a01")).as_deref(), Some("a0"));
        assert_eq!(k(None, Some("a0V")).as_deref(), Some("a0"));
        assert_eq!(k(None, Some("b999")).as_deref(), Some("b99"));
        assert_eq!(
            k(None, Some("A00000000000000000000000000")).as_deref(),
            None
        );
        assert_eq!(
            k(None, Some("A000000000000000000000000001")).as_deref(),
            Some("A000000000000000000000000000V")
        );
        assert_eq!(
            k(Some("zzzzzzzzzzzzzzzzzzzzzzzzzzy"), None).as_deref(),
            Some("zzzzzzzzzzzzzzzzzzzzzzzzzzz")
        );
        assert_eq!(
            k(Some("zzzzzzzzzzzzzzzzzzzzzzzzzzz"), None).as_deref(),
            Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzV")
        );
        assert_eq!(k(Some("a00"), None), None);
        assert_eq!(k(Some("a00"), Some("a1")), None);
        assert_eq!(k(Some("0"), Some("1")), None);
        assert_eq!(k(Some("a1"), Some("a0")), None);
    }

    #[test]
    fn n_keys_like_the_original() {
        let n = |a: Option<&str>, b: Option<&str>, n| keys_between(a, b, n).unwrap().join(" ");
        assert_eq!(n(None, None, 5), "a0 a1 a2 a3 a4");
        assert_eq!(n(Some("a4"), None, 10), "a5 a6 a7 a8 a9 aA aB aC aD aE");
        assert_eq!(n(None, Some("a0"), 5), "Zv Zw Zx Zy Zz");
        let many = keys_between(Some("a0"), Some("a2"), 20).unwrap();
        assert_eq!(many.len(), 20);
        assert!(many.windows(2).all(|w| w[0] < w[1]));
        assert!(many[0].as_str() > "a0" && many[19].as_str() < "a2");
        assert!(many.iter().all(|k| is_valid(k)));
    }

    fn els(indices: &[Option<&str>]) -> Vec<Element> {
        indices
            .iter()
            .enumerate()
            .map(|(i, idx)| {
                let mut v = json!({"id": format!("e{i}"), "type": "rectangle", "version": 1});
                if let Some(idx) = idx {
                    v["index"] = json!(idx);
                }
                Element::from_props(v.as_object().unwrap().clone()).unwrap()
            })
            .collect()
    }

    fn sorted(els: &[Element]) -> bool {
        els.windows(2)
            .all(|w| w[0].index().unwrap() < w[1].index().unwrap())
    }

    #[test]
    fn repairs_keep_good_indices() {
        let mut e = els(&[Some("a0"), Some("a1"), Some("a2")]);
        assert_eq!(sync_invalid_indices(&mut e), 0);

        let mut e = els(&[None, None, None]);
        assert_eq!(sync_invalid_indices(&mut e), 3);
        assert!(sorted(&e));
        assert_eq!(e[0].version(), 2);

        let mut e = els(&[Some("a0"), None, Some("bad!"), Some("a1")]);
        assert_eq!(sync_invalid_indices(&mut e), 2);
        assert!(sorted(&e));
        assert_eq!(e[0].index(), Some("a0"));
        assert_eq!(e[3].index(), Some("a1"));

        // One out of place: only it moves.
        let mut e = els(&[Some("a0"), Some("a1"), Some("Zz"), Some("a3"), Some("a4")]);
        assert_eq!(sync_invalid_indices(&mut e), 1);
        assert!(sorted(&e));
        assert_eq!(e[2].index().map(|i| i > "a1" && i < "a3"), Some(true));

        // Duplicates.
        let mut e = els(&[Some("a1"), Some("a1"), Some("a1")]);
        sync_invalid_indices(&mut e);
        assert!(sorted(&e));
    }

    #[test]
    fn moved_ones_fit_between_their_neighbours() {
        let moved = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        // e1 moved down from the top: only it changes.
        let mut e = els(&[Some("a0"), Some("a5"), Some("a1"), Some("a3")]);
        assert_eq!(sync_moved_indices(&mut e, &moved(&["e1"])), 1);
        assert!(sorted(&e));
        assert_eq!(e[1].index().map(|i| i > "a0" && i < "a1"), Some(true));
        assert_eq!(e[1].version(), 2);
        assert_eq!(e[2].version(), 1);
        // At the ends, several together.
        let mut e = els(&[Some("a5"), Some("a6"), Some("a0"), Some("a1")]);
        assert_eq!(sync_moved_indices(&mut e, &moved(&["e0", "e1"])), 2);
        assert!(sorted(&e));
        // Neighbours out of order: everything out of place is repaired.
        let mut e = els(&[Some("a3"), Some("a9"), Some("a1"), Some("a2")]);
        sync_moved_indices(&mut e, &moved(&["e1"]));
        assert!(sorted(&e));
    }
}
