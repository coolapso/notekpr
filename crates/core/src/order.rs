//! The order of the entries in a folder, as the sidebar shows them: folders
//! first, then notes and attachments; within each group the entries someone
//! put in place by hand (by their order key), then the rest A–Z.
//!
//! An order key is a short string of base-62 digits compared bytewise, like
//! the digits of a fraction: between any two keys there is another, so moving
//! one entry usually rewrites only its own key. Keys live in the vault index
//! next to the path, so moves merge like any other change of the index. Two
//! entries that end up with the same key (moved to the same place on two
//! devices at once) fall back to A–Z between themselves.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::doc::{Entry, EntryKind};
use crate::paths;

const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Longest order key. Longer ones (written elsewhere) count as no key; a
/// group gets fresh short keys before one would grow past it.
pub const MAX_KEY_LEN: usize = 32;

/// A key as stored: base-62 digits, not ending in `0` (so there is always
/// room before it), at most [`MAX_KEY_LEN`] long.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_KEY_LEN
        && !k.ends_with('0')
        && k.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn digit(b: u8) -> usize {
    match b {
        b'0'..=b'9' => (b - b'0') as usize,
        b'A'..=b'Z' => (b - b'A') as usize + 10,
        _ => (b - b'a') as usize + 36,
    }
}

/// A key strictly between `a` ("" = the start) and `b` (`None` = the end).
/// Both must be valid (or empty) with `a < b`.
fn midpoint(mut a: &[u8], mut b: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if let Some(bb) = b {
            // Keep the common prefix (`a` padded with zeros).
            let n = bb
                .iter()
                .enumerate()
                .take_while(|&(i, &c)| a.get(i).copied().unwrap_or(b'0') == c)
                .count();
            if n > 0 {
                out.extend_from_slice(&bb[..n]);
                a = a.get(n..).unwrap_or_default();
                b = Some(&bb[n..]);
                continue;
            }
        }
        let da = a.first().map_or(0, |&c| digit(c));
        let db = b.map_or(DIGITS.len(), |bb| digit(bb[0]));
        if db - da > 1 {
            out.push(DIGITS[(da + db).div_ceil(2)]);
            return out;
        }
        // Neighbouring digits: a shorter key of `b`'s first digit fits if
        // `b` goes on; otherwise continue after `a`'s first digit.
        if let Some(bb) = b.filter(|bb| bb.len() > 1) {
            out.push(bb[0]);
            return out;
        }
        out.push(DIGITS[da]);
        a = a.get(1..).unwrap_or_default();
        b = None;
    }
}

/// A key that sorts after `a` and before `b` (`None`: the start or the
/// end). `None` if either isn't a valid key or `a` isn't before `b`.
pub fn key_between(a: Option<&str>, b: Option<&str>) -> Option<String> {
    if a.is_some_and(|a| !valid_key(a)) || b.is_some_and(|b| !valid_key(b)) {
        return None;
    }
    if let (Some(a), Some(b)) = (a, b)
        && a >= b
    {
        return None;
    }
    let k = midpoint(a.unwrap_or("").as_bytes(), b.map(str::as_bytes));
    String::from_utf8(k).ok()
}

/// `n` short keys, in order, all after `a` and before `b`.
fn spread(a: &[u8], b: Option<&[u8]>, n: usize, out: &mut Vec<String>) {
    if n == 0 {
        return;
    }
    let mid = midpoint(a, b);
    let left = n / 2;
    spread(a, Some(&mid), left, out);
    out.push(String::from_utf8_lossy(&mid).into_owned());
    spread(&mid, b, n - left - 1, out);
}

/// What an entry is sorted by among its siblings.
#[derive(Clone, Copy, Debug)]
pub struct SortKey<'a> {
    pub folder: bool,
    pub order: Option<&'a str>,
    /// Its name as shown: a note's title, otherwise its file name.
    pub name: &'a str,
}

/// Folders first; then entries with an order key, by it; then by name.
pub fn compare(a: SortKey, b: SortKey) -> Ordering {
    b.folder
        .cmp(&a.folder)
        .then_with(|| match (a.order, b.order) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(b.name))
}

/// One entry of a folder. A folder that only exists as the parent of others
/// has no entry (and so no icon or order key) until something needs one.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    pub path: String,
    pub entry: Option<Entry>,
}

impl Child {
    pub fn is_folder(&self) -> bool {
        self.entry
            .as_ref()
            .is_none_or(|e| e.kind == EntryKind::Folder)
    }

    /// The name shown for it: a note's or drawing's title, otherwise its
    /// file name.
    pub fn name(&self) -> &str {
        match &self.entry {
            Some(e) if matches!(e.kind, EntryKind::Note | EntryKind::Drawing) => e.title(),
            _ => paths::file_name(&self.path),
        }
    }

    pub fn order(&self) -> Option<&str> {
        self.entry.as_ref()?.order.as_deref()
    }

    pub fn sort_key(&self) -> SortKey<'_> {
        SortKey {
            folder: self.is_folder(),
            order: self.order(),
            name: self.name(),
        }
    }
}

/// The children of every folder ("" = the top of the vault), each list in
/// order. `entries` are live entries with their display paths.
pub fn tree(entries: &[Entry]) -> BTreeMap<String, Vec<Child>> {
    let mut folders: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    let mut out: BTreeMap<String, Vec<Child>> = BTreeMap::new();
    for e in entries {
        if e.kind == EntryKind::Folder {
            folders.insert(e.path.clone(), Some(e.clone()));
        } else {
            out.entry(paths::parent(&e.path).to_string())
                .or_default()
                .push(Child {
                    path: e.path.clone(),
                    entry: Some(e.clone()),
                });
        }
        let mut p = paths::parent(&e.path);
        while !p.is_empty() {
            folders.entry(p.to_string()).or_insert(None);
            p = paths::parent(p);
        }
    }
    for (path, entry) in folders {
        out.entry(paths::parent(&path).to_string())
            .or_default()
            .push(Child { path, entry });
    }
    for list in out.values_mut() {
        list.sort_by(|a, b| compare(a.sort_key(), b.sort_key()));
    }
    out
}

/// The order keys that put an entry at position `at` of its group (the
/// folders, or the notes and attachments, of the folder it lands in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The moved entry's key.
    pub key: String,
    /// New keys for others of the group (by their index in `siblings`) when
    /// they needed one to make room: when it goes after an entry without a
    /// key, or between two with the same key.
    pub others: Vec<(usize, String)>,
}

/// Where an entry goes: `siblings` are the keys of its group in order,
/// without the entry itself; `at` is its new position among them.
pub fn place(siblings: &[Option<&str>], at: usize) -> Placement {
    let at = at.min(siblings.len());
    // Entries with a key come first, so after one with a key (or at the
    // start) a key below the next one with a key is exactly position `at`.
    let next = siblings.get(at).copied().flatten();
    let fits = match at.checked_sub(1).map(|i| siblings[i]) {
        None => key_between(None, next),
        Some(Some(prev)) => key_between(Some(prev), next),
        Some(None) => None,
    };
    if let Some(key) = fits.filter(|k| k.len() <= MAX_KEY_LEN) {
        return Placement {
            key,
            others: Vec::new(),
        };
    }
    // Make room: fresh keys for the whole group, in the order shown.
    let mut keys = Vec::with_capacity(siblings.len() + 1);
    spread(b"", None, siblings.len() + 1, &mut keys);
    let key = keys.remove(at);
    let others = siblings
        .iter()
        .zip(keys)
        .enumerate()
        .filter(|(_, (old, new))| **old != Some(new.as_str()))
        .map(|(i, (_, new))| (i, new))
        .collect();
    Placement { key, others }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str, kind: EntryKind, order: Option<&str>) -> Entry {
        Entry {
            id: id.into(),
            path: path.into(),
            kind,
            created: 0,
            modified: 0,
            trashed: None,
            purged: None,
            size: None,
            mime: None,
            icon: None,
            order: order.map(Into::into),
        }
    }

    #[test]
    fn keys_between() {
        assert_eq!(key_between(None, None).as_deref(), Some("V"));
        assert_eq!(key_between(Some("V"), None).as_deref(), Some("l"));
        assert_eq!(key_between(None, Some("V")).as_deref(), Some("G"));
        assert_eq!(key_between(Some("z"), None).as_deref(), Some("zV"));
        assert_eq!(key_between(None, Some("1")).as_deref(), Some("0V"));
        assert_eq!(key_between(Some("4"), Some("5")).as_deref(), Some("4V"));
        assert_eq!(key_between(Some("4"), Some("5V")).as_deref(), Some("5"));
        assert_eq!(key_between(Some("1"), Some("1V")).as_deref(), Some("1G"));
        // Not in order, the same, or not keys at all.
        assert_eq!(key_between(Some("b"), Some("a")), None);
        assert_eq!(key_between(Some("a"), Some("a")), None);
        assert_eq!(key_between(Some("a0"), None), None);
        assert_eq!(key_between(Some("ä"), None), None);
        assert_eq!(key_between(None, Some("")), None);
    }

    #[test]
    fn repeated_inserts_stay_ordered_and_valid() {
        // Always at the start, always at the end, always in the same gap.
        let mut first = key_between(None, None).unwrap();
        let mut last = first.clone();
        let lo = first.clone();
        let mut hi = key_between(Some(&lo), None).unwrap();
        for _ in 0..100 {
            let f = key_between(None, Some(&first)).unwrap();
            let l = key_between(Some(&last), None).unwrap();
            let m = key_between(Some(&lo), Some(&hi)).unwrap();
            for k in [&f, &l, &m] {
                assert!(valid_key(k), "{k}");
            }
            assert!(f < first && l > last && lo < m && m < hi);
            (first, last, hi) = (f, l, m);
        }
    }

    #[test]
    fn spread_is_ordered_and_short() {
        for n in [1, 2, 3, 10, 61, 62, 63, 500] {
            let mut keys = Vec::new();
            spread(b"", None, n, &mut keys);
            assert_eq!(keys.len(), n);
            assert!(keys.windows(2).all(|w| w[0] < w[1]), "{n}");
            assert!(keys.iter().all(|k| valid_key(k) && k.len() <= 3), "{n}");
        }
    }

    #[test]
    fn sorting() {
        let entries = [
            entry("b", "b.md", EntryKind::Note, None),
            entry("a", "A note.md", EntryKind::Note, None),
            entry("z", "zz.md", EntryKind::Note, Some("V")),
            entry("y", "y.png", EntryKind::Attachment, Some("G")),
            entry("f", "Folder", EntryKind::Folder, None),
            entry("i", "Implicit/x.md", EntryKind::Note, None),
            entry("w", "Work", EntryKind::Folder, Some("k")),
        ];
        let tree = tree(&entries);
        let names: Vec<&str> = tree[""].iter().map(Child::name).collect();
        // Folders first (with a key first), then notes and attachments
        // (with a key first, by key), then the rest A–Z.
        assert_eq!(
            names,
            ["Work", "Folder", "Implicit", "y.png", "zz", "A note", "b"]
        );
        assert!(tree[""][2].entry.is_none() && tree[""][2].is_folder());
        assert_eq!(tree["Implicit"].len(), 1);
        // The same key: by name.
        let a = SortKey {
            folder: false,
            order: Some("V"),
            name: "b",
        };
        let b = SortKey { name: "A", ..a };
        assert_eq!(compare(a, b), Ordering::Greater);
    }

    /// Apply a placement to `keys` (inserting the moved one at `at`) and
    /// return the order the result sorts in, by original index (the moved
    /// one is `usize::MAX`).
    fn sorted_after(keys: &[Option<&str>], at: usize) -> Vec<usize> {
        let p = place(keys, at);
        let mut keys: Vec<(usize, Option<String>)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (i, k.map(Into::into)))
            .collect();
        for (i, k) in p.others {
            keys[i].1 = Some(k);
        }
        keys.push((usize::MAX, Some(p.key)));
        // Unkeyed ones keep their order (it stands for A–Z).
        keys.sort_by(|(i, a), (j, b)| match (a, b) {
            (Some(x), Some(y)) => x.cmp(y).then(i.cmp(j)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => i.cmp(j),
        });
        keys.into_iter().map(|(i, _)| i).collect()
    }

    #[test]
    fn placing() {
        let m = usize::MAX;
        // Nothing ordered yet: the first one placed is the only key written.
        let p = place(&[None, None, None], 0);
        assert!(p.others.is_empty());
        assert_eq!(sorted_after(&[None, None, None], 0), [m, 0, 1, 2]);
        // After one without a key: the ones before it get keys too.
        assert_eq!(place(&[None, None, None], 2).others.len(), 3);
        assert_eq!(sorted_after(&[None, None, None], 2), [0, 1, m, 2]);
        assert_eq!(sorted_after(&[None, None, None], 3), [0, 1, 2, m]);
        // Between two keys, and after the last key before unkeyed ones.
        let keys = [Some("G"), Some("V"), None];
        assert!(place(&keys, 1).others.is_empty());
        assert_eq!(sorted_after(&keys, 1), [0, m, 1, 2]);
        assert!(place(&keys, 2).others.is_empty());
        assert_eq!(sorted_after(&keys, 2), [0, 1, m, 2]);
        // Between two with the same key.
        let keys = [Some("G"), Some("V"), Some("V")];
        assert_eq!(sorted_after(&keys, 2), [0, 1, m, 2]);
        // Positions past the end mean the end.
        assert_eq!(sorted_after(&[Some("V")], 7), [0, m]);
        assert_eq!(sorted_after(&[], 0), [m]);
    }

    #[test]
    fn placing_never_grows_keys_too_long() {
        // Always between the first two: keys grow, then the group is respread.
        let mut keys: Vec<String> = vec!["G".into(), "V".into()];
        for _ in 0..500 {
            let refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
            let p = place(&refs, 1);
            for (i, k) in p.others {
                keys[i] = k;
            }
            keys.insert(1, p.key);
            assert!(keys.iter().all(|k| valid_key(k)));
            assert!(keys.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
