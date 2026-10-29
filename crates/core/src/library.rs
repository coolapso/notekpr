//! The drawings library, as Excalidraw's: items (groups of elements) saved
//! from one drawing to drop into any other, the same in every app.
//!
//! It is the person's own and lives in their Drawings section
//! ([`crate::drawings`]), in the section's index document: a root map
//! `library` beside the entries' `files`, item id → the item as JSON, as
//! Excalidraw has it without the id (`status`, `created`, `name`,
//! `elements`; [`notekpr_draw::library`]). So it is encrypted with the
//! section's key, synced and merged like the index — two devices adding
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
            Err(LibraryError::TooLarge { .. }) => out.too_large += 1,
        }
    }
    out
}

/// The library as an `.excalidrawlib` file (Excalidraw imports it).
pub fn export(index: &IndexDoc) -> String {
    notekpr_draw::file::write_library(&items(index))
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
