//! Saved shapes — Excalidraw's library: items (groups of elements) saved
//! from one drawing to drop into any other, the same in every app.
//!
//! They are the person's own and live in their Personal storage (the quick
//! notes' vault, [`crate::quick`]), in its index document: a root map
//! `library` beside the entries' `files`, item id → the item as JSON, as
//! Excalidraw has it without the id (`status`, `created`, `name`,
//! `elements`; [`notekpr_draw::library`]). So they are encrypted with that
//! vault's key, synced and merged like the index — two devices adding
//! items at once keep both —, takes no drawing's place, and the server
//! learns nothing it didn't (index updates are opaque to it). Releases
//! before it keep the map in what they encode without reading it.
//!
//! An item is one value: unlike a note's text it can't be spread over
//! several updates, so one larger than an update may carry is refused
//! ([`LibraryError::TooLarge`]) rather than queued for a server that would
//! refuse it. Items hold elements only, not pictures (Excalidraw's rule:
//! [`notekpr_draw::library::refusal`]).

use notekpr_draw::library::{is_duplicate, item_json, read_item, sort};
use notekpr_draw::restore::Ids;

use crate::doc::IndexDoc;

pub use notekpr_draw::file::LibraryItem;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LibraryError {
    /// Larger than one update may carry (`budget`: the most it may).
    #[error(
        "this library item is too large ({}): an item may take at most {} on this server",
        size_text(*.bytes),
        size_text(*.budget)
    )]
    TooLarge { bytes: usize, budget: usize },
    /// No item has this id, nor one starting so ([`find`]).
    #[error("no saved shape '{0}'")]
    NotFound(String),
    /// Several items' ids start so ([`find`]).
    #[error("'{0}' is the start of several saved shapes' ids: give more of it")]
    Ambiguous(String),
}

/// "820 KB", "1.5 MB".
fn size_text(bytes: usize) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{} KB", kb.ceil())
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

/// The library's items, newest first. Items that can't be read (a newer
/// release's, or broken) are left out.
pub fn items(index: &IndexDoc) -> Vec<LibraryItem> {
    let mut ids = Ids::new(1);
    let mut out: Vec<LibraryItem> = index
        .library_entries()
        .into_iter()
        .filter_map(|(id, json)| {
            let v: serde_json::Value = serde_json::from_str(&json).ok()?;
            read_item(&v, Some(&id), 0.0, &mut ids)
        })
        .collect();
    sort(&mut out);
    out
}

/// Add `item` (or replace the one with its id), unless it is larger than
/// `budget` bytes: what one local change may carry (`Limits::edit_budget`
/// in `notekpr_sync`).
pub fn add(
    index: &IndexDoc,
    item: &LibraryItem,
    budget: usize,
) -> Result<Option<Vec<u8>>, LibraryError> {
    let json = item_json(item).to_string();
    if json.len() > budget {
        return Err(LibraryError::TooLarge {
            bytes: json.len(),
            budget,
        });
    }
    Ok(index.put_library_entry(&item.id, &json))
}

/// Remove items by id.
pub fn remove<'a>(index: &IndexDoc, ids: impl IntoIterator<Item = &'a str>) -> Option<Vec<u8>> {
    index.remove_library_entries(ids)
}

/// Name the item kept under `id` (`None`, or only spaces: no name), on
/// one line. `Ok(None)`: it has that name already.
pub fn rename(
    index: &IndexDoc,
    id: &str,
    name: Option<&str>,
    budget: usize,
) -> Result<Option<Vec<u8>>, LibraryError> {
    let mut item = items(index)
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| LibraryError::NotFound(id.to_string()))?;
    let name = name
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|n| !n.is_empty());
    if item.name == name {
        return Ok(None);
    }
    item.name = name;
    add(index, &item, budget)
}

/// The items `refs` name — each an item's id, or the start of one (as
/// `notekpr shapes ls` shows the first 8 characters) — each once, in
/// `items`' order. A whole id wins over the start of a longer one.
pub fn find<'a, S: AsRef<str>>(
    items: &'a [LibraryItem],
    refs: &[S],
) -> Result<Vec<&'a LibraryItem>, LibraryError> {
    let mut wanted = std::collections::HashSet::new();
    for r in refs {
        let r = r.as_ref();
        if let Some(i) = items.iter().find(|i| i.id == r) {
            wanted.insert(i.id.as_str());
            continue;
        }
        let mut starting = items
            .iter()
            .filter(|i| !r.is_empty() && i.id.starts_with(r));
        match (starting.next(), starting.next()) {
            (Some(i), None) => {
                wanted.insert(i.id.as_str());
            }
            (Some(_), Some(_)) => return Err(LibraryError::Ambiguous(r.to_string())),
            (None, _) => return Err(LibraryError::NotFound(r.to_string())),
        }
    }
    Ok(items
        .iter()
        .filter(|i| wanted.contains(i.id.as_str()))
        .collect())
}

/// What [`import`] did.
#[derive(Debug, Default)]
pub struct Imported {
    /// One per item added, in order.
    pub updates: Vec<Vec<u8>>,
    pub added: usize,
    /// Items the library has already (the same elements), left out.
    pub already: usize,
    /// Items too large to keep ([`LibraryError::TooLarge`]), left out.
    pub too_large: usize,
}

/// Add items (from an `.excalidrawlib` file): those the library has already
/// are left out (`mergeLibraryItems`), as are those too large to keep.
pub fn import(index: &IndexDoc, new: Vec<LibraryItem>, budget: usize) -> Imported {
    let mut have = items(index);
    let mut out = Imported::default();
    for item in new {
        if is_duplicate(&have, &item) {
            out.already += 1;
            continue;
        }
        match add(index, &item, budget) {
            Ok(u) => {
                out.updates.extend(u);
                out.added += 1;
                have.push(item);
            }
            Err(_) => out.too_large += 1,
        }
    }
    out
}

/// The library as an `.excalidrawlib` file (Excalidraw imports it).
pub fn export(index: &IndexDoc) -> String {
    notekpr_draw::file::write_library(&items(index))
}

/// What [`merge`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Merged {
    /// One per change, in order.
    pub updates: Vec<Vec<u8>>,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    /// New in the file but saved already (the same elements), left out.
    pub already: usize,
    /// Too large to keep ([`LibraryError::TooLarge`]), left out.
    pub too_large: usize,
}

impl Merged {
    /// Whether the library changed.
    pub fn any(&self) -> bool {
        !self.updates.is_empty()
    }
}

/// A library file edited elsewhere — the Markdown folder's `Saved
/// shapes.excalidrawlib` — merged in shape by shape: what changed between
/// `base` (the shapes as the file held them when last written or read) and
/// `file` (what it holds now) is applied to the library, and shapes the
/// file didn't touch stay as they are here, changes made meanwhile
/// included. A shape new in the file is added unless saved already (as
/// [`import`]); one changed in it replaces the library's, whole; one taken
/// out of it is removed. Where both sides changed the same shape, the
/// file's change wins.
pub fn merge(
    index: &IndexDoc,
    base: &[LibraryItem],
    file: &[LibraryItem],
    budget: usize,
) -> Merged {
    use std::collections::{HashMap, HashSet};
    let mut out = Merged::default();
    let base: HashMap<&str, serde_json::Value> =
        base.iter().map(|i| (i.id.as_str(), item_json(i))).collect();
    let mut have = items(index);
    // (A file naming an id twice: the first one counts.)
    let mut seen: HashSet<&str> = HashSet::new();
    let file: Vec<&LibraryItem> = file.iter().filter(|i| seen.insert(i.id.as_str())).collect();
    // Taken out first: a shape whose ids were all made new in the file is
    // the new one, not one saved already.
    let gone: Vec<String> = have
        .iter()
        .filter(|h| base.contains_key(h.id.as_str()) && !seen.contains(h.id.as_str()))
        .map(|h| h.id.clone())
        .collect();
    if let Some(u) = remove(index, gone.iter().map(String::as_str)) {
        out.removed = gone.len();
        out.updates.push(u);
        have.retain(|h| !gone.contains(&h.id));
    }
    for item in file {
        let json = item_json(item);
        let was = base.get(item.id.as_str());
        if was == Some(&json) {
            continue;
        }
        // As it is here already (made so here too, or added elsewhere).
        let same_here = have.iter().any(|h| h.id == item.id && item_json(h) == json);
        if was.is_none() && (same_here || is_duplicate(&have, item)) {
            out.already += 1;
            continue;
        }
        if same_here {
            continue;
        }
        match add(index, item, budget) {
            Ok(None) => {}
            Ok(Some(u)) => {
                out.updates.push(u);
                if was.is_some() {
                    out.changed += 1;
                } else {
                    out.added += 1;
                    have.push(item.clone());
                }
            }
            Err(_) => out.too_large += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::doc::EntryKind;

    fn item(id: &str, created: f64, shape: &str) -> LibraryItem {
        let raw = json!([{"id": format!("{id}-el"), "type": shape, "width": 10, "height": 10}]);
        LibraryItem {
            id: id.into(),
            status: "unpublished".into(),
            created,
            name: Some(format!("{shape} {id}")),
            elements: notekpr_draw::restore::restore_elements(
                raw.as_array().unwrap(),
                &mut Ids::new(1),
            ),
        }
    }

    fn ids(index: &IndexDoc) -> Vec<String> {
        items(index).into_iter().map(|i| i.id).collect()
    }

    #[test]
    fn concurrent_adds_on_two_devices_both_stay() {
        let a = IndexDoc::new();
        let u = a
            .create("d", "Board.drawing", EntryKind::Drawing, 1)
            .unwrap();
        let b = IndexDoc::from_updates([u.as_slice()]).unwrap();
        let ua = add(&a, &item("one", 1.0, "rectangle"), 1 << 20)
            .unwrap()
            .unwrap();
        let ub = add(&b, &item("two", 2.0, "ellipse"), 1 << 20)
            .unwrap()
            .unwrap();
        a.apply_update(&ub).unwrap();
        b.apply_update(&ua).unwrap();
        for d in [&a, &b] {
            assert_eq!(ids(d), ["two", "one"], "newest first");
            // The file tree doesn't see it.
            assert_eq!(d.entries().len(), 1);
            assert_eq!(d.resolved_paths().len(), 1);
        }
        let two = &items(&a)[0];
        assert_eq!(two, &item("two", 2.0, "ellipse"));

        // Removing one, on one device, removes it on the other.
        let r = remove(&b, ["one"]).unwrap();
        a.apply_update(&r).unwrap();
        assert_eq!(ids(&a), ["two"]);
        assert!(remove(&a, ["one"]).is_none(), "nothing left to remove");

        // A snapshot (and anything built from one) keeps it.
        let c = IndexDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        assert_eq!(ids(&c), ["two"]);
    }

    #[test]
    fn an_item_larger_than_an_update_may_carry_is_refused() {
        let index = IndexDoc::new();
        let mut big = item("big", 1.0, "text");
        big.elements[0].set("text", json!("x".repeat(5000)));
        let err = add(&index, &big, 4096).unwrap_err();
        assert!(matches!(err, LibraryError::TooLarge { budget: 4096, .. }));
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(err.to_string().contains("at most 4 KB"), "{err}");
        assert!(index.library_entries().is_empty());
        assert_eq!(size_text(3 << 20), "3.0 MB");

        // An import keeps the others, and leaves out what it has already.
        let small = item("small", 2.0, "diamond");
        let i = import(&index, vec![big, small.clone(), small], 4096);
        assert_eq!((i.added, i.already, i.too_large), (1, 1, 1));
        assert_eq!(i.updates.len(), 1);
        assert_eq!(ids(&index), ["small"]);
        let again = import(&index, items(&index), 4096);
        assert_eq!((again.added, again.already), (0, 1));

        // Exported as Excalidraw's file, which reads back the same.
        let file = export(&index);
        let back = notekpr_draw::file::read_library(&file, 0.0, &mut Ids::new(9)).unwrap();
        assert_eq!(back, items(&index));
    }

    #[test]
    fn shapes_are_renamed_and_found_by_the_start_of_their_ids() {
        let index = IndexDoc::new();
        for (id, at) in [("abc123", 1.0), ("abd456", 2.0), ("abc", 3.0)] {
            add(&index, &item(id, at, "rectangle"), 1 << 20).unwrap();
        }
        let all = items(&index);
        let found = |refs: &[&str]| {
            find(&all, refs).map(|f| f.iter().map(|i| i.id.as_str()).collect::<Vec<_>>())
        };
        // A whole id wins; a start must be of one alone; in the library's
        // order, once each.
        assert_eq!(found(&["abc"]).unwrap(), ["abc"]);
        assert_eq!(
            found(&["abd", "abc1", "abd456"]).unwrap(),
            ["abd456", "abc123"]
        );
        assert_eq!(found(&["ab"]), Err(LibraryError::Ambiguous("ab".into())));
        assert_eq!(found(&["zz"]), Err(LibraryError::NotFound("zz".into())));
        assert_eq!(found(&[""]), Err(LibraryError::NotFound(String::new())));
        assert!(found(&[]).unwrap().is_empty());

        // Renamed: on one line; blank is no name; the same name changes
        // nothing.
        let u = rename(&index, "abc", Some("  Raised\n bed "), 1 << 20).unwrap();
        assert!(u.is_some());
        let name = |id: &str| items(&index).into_iter().find(|i| i.id == id).unwrap().name;
        assert_eq!(name("abc").as_deref(), Some("Raised bed"));
        assert_eq!(rename(&index, "abc", Some("Raised bed"), 1 << 20), Ok(None));
        rename(&index, "abc", Some("   "), 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(name("abc"), None);
        rename(&index, "abc", Some("Bed"), 1 << 20)
            .unwrap()
            .unwrap();
        rename(&index, "abc", None, 1 << 20).unwrap().unwrap();
        assert_eq!(name("abc"), None);
        assert_eq!(
            rename(&index, "nope", Some("x"), 1 << 20),
            Err(LibraryError::NotFound("nope".into()))
        );
        // Everything else stays.
        let after = items(&index);
        assert_eq!(after.len(), 3);
        assert_eq!(
            after.iter().find(|i| i.id == "abc").unwrap().elements,
            all[0].elements
        );
    }

    #[test]
    fn a_file_edited_elsewhere_merges_shape_by_shape() {
        let index = IndexDoc::new();
        for (id, at, shape) in [
            ("keep", 1.0, "rectangle"),
            ("edit", 2.0, "ellipse"),
            ("drop", 3.0, "diamond"),
        ] {
            add(&index, &item(id, at, shape), 1 << 20).unwrap();
        }
        // What the file held when written.
        let base = items(&index);
        // Meanwhile, here: one added, one renamed.
        add(&index, &item("new-here", 4.0, "line"), 1 << 20).unwrap();
        rename(&index, "keep", Some("Kept, renamed here"), 1 << 20).unwrap();

        // The file: "edit" renamed, "drop" taken out, a shape added, one of
        // Excalidraw's added that is saved already (other id, same
        // elements), and the same id twice.
        let mut file = base.clone();
        file.retain(|i| i.id != "drop");
        file.iter_mut().find(|i| i.id == "edit").unwrap().name = Some("Edited in the file".into());
        file.push(item("new-there", 5.0, "arrow"));
        let mut copy = item("new-here", 4.0, "line");
        copy.id = "copy-of-new-here".into();
        file.push(copy);
        file.push(item("new-there", 9.0, "text"));
        let m = merge(&index, &base, &file, 1 << 20);
        assert_eq!(
            (m.added, m.changed, m.removed, m.already, m.too_large),
            (1, 1, 1, 1, 0),
            "{m:?}"
        );
        assert!(m.any());
        let now = items(&index);
        let ids: Vec<&str> = now.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["new-there", "new-here", "edit", "keep"]);
        let named = |id: &str| now.iter().find(|i| i.id == id).unwrap().name.clone();
        assert_eq!(named("edit").as_deref(), Some("Edited in the file"));
        assert_eq!(
            named("keep").as_deref(),
            Some("Kept, renamed here"),
            "the file didn't touch it"
        );
        assert_eq!(
            now[0].elements[0].kind(),
            notekpr_draw::scene::ElementType::Arrow
        );

        // The same file again (nothing changed against itself): nothing.
        let base = file.clone();
        let again = merge(&index, &base, &file, 1 << 20);
        assert_eq!(again, Merged::default());

        // Both sides changed one shape: the file's wins; taken out of the
        // file while renamed here: gone.
        let base = items(&index);
        rename(&index, "edit", Some("Here"), 1 << 20).unwrap();
        rename(&index, "keep", Some("Here too"), 1 << 20).unwrap();
        let mut file = base.clone();
        file.iter_mut().find(|i| i.id == "edit").unwrap().name = Some("There".into());
        file.retain(|i| i.id != "keep");
        let m = merge(&index, &base, &file, 1 << 20);
        assert_eq!((m.changed, m.removed), (1, 1));
        let now = items(&index);
        assert_eq!(
            now.iter().find(|i| i.id == "edit").unwrap().name.as_deref(),
            Some("There")
        );
        assert!(now.iter().all(|i| i.id != "keep"));

        // Every id made new in the file: the shapes stay (under the new
        // ids), not "saved already".
        let base = items(&index);
        let file: Vec<LibraryItem> = base
            .iter()
            .map(|i| LibraryItem {
                id: format!("{}-2", i.id),
                ..i.clone()
            })
            .collect();
        let m = merge(&index, &base, &file, 1 << 20);
        assert_eq!((m.added, m.removed, m.already), (3, 3, 0));
        assert_eq!(items(&index).len(), 3);

        // Too large: left out, the rest goes in.
        let mut big = item("big", 10.0, "text");
        big.elements[0].set("text", json!("x".repeat(5000)));
        let base = items(&index);
        let mut file = base.clone();
        file.push(big);
        file.push(item("small", 11.0, "rectangle"));
        let m = merge(&index, &base, &file, 4096);
        assert_eq!((m.added, m.too_large), (1, 1));

        // An emptied file empties the library.
        let base = items(&index);
        let m = merge(&index, &base, &[], 1 << 20);
        assert_eq!(m.removed, 4);
        assert!(items(&index).is_empty());
    }

    #[test]
    fn index_documents_of_other_releases_keep_what_they_dont_know() {
        // An index from before the library: an old release's document has
        // only the entries' root. It applies library items it is sent and
        // keeps them in its state (its snapshots), never showing them.
        let old = yrs::Doc::new();
        let new = IndexDoc::new();
        new.create("n", "Board.drawing", EntryKind::Drawing, 1);
        add(&new, &item("kept", 1.0, "ellipse"), 1 << 20).unwrap();
        {
            use yrs::updates::decoder::Decode;
            use yrs::{ReadTxn, Transact};
            let _files = old.get_or_insert_map("files");
            let u = yrs::Update::decode_v1(&new.encode_state()).unwrap();
            old.transact_mut().apply_update(u).unwrap();
            let state = old
                .transact()
                .encode_state_as_update_v1(&yrs::StateVector::default());
            let again = IndexDoc::from_updates([state.as_slice()]).unwrap();
            assert_eq!(ids(&again), ["kept"]);
            assert_eq!(again.entries().len(), 1);
        }

        // A root a newer release adds is left alone here, and kept.
        let newer = yrs::Doc::new();
        {
            use yrs::{Map, ReadTxn, Transact};
            let boards = newer.get_or_insert_map("boards");
            boards.insert(&mut newer.transact_mut(), "b", "something new");
            let u = newer
                .transact()
                .encode_state_as_update_v1(&yrs::StateVector::default());
            new.apply_update(&u).unwrap();
        }
        assert_eq!(new.entries().len(), 1);
        assert_eq!(ids(&new), ["kept"]);
        let copy = yrs::Doc::new();
        {
            use yrs::updates::decoder::Decode;
            use yrs::{Map, Transact};
            let boards = copy.get_or_insert_map("boards");
            let u = yrs::Update::decode_v1(&new.encode_state()).unwrap();
            copy.transact_mut().apply_update(u).unwrap();
            assert_eq!(boards.len(&copy.transact()), 1);
        }

        // Values that aren't items are skipped.
        new.put_library_entry("junk", "not json");
        new.put_library_entry("empty", r#"{"elements":[]}"#);
        assert_eq!(ids(&new), ["kept"]);
    }

    #[test]
    fn an_index_without_a_library_encodes_as_before() {
        let old = yrs::Doc::new();
        let _ = old.get_or_insert_map("files");
        let new = IndexDoc::new();
        use yrs::{ReadTxn, Transact};
        assert_eq!(
            new.encode_state(),
            old.transact()
                .encode_state_as_update_v1(&yrs::StateVector::default())
        );
        assert!(items(&new).is_empty());
    }
}
