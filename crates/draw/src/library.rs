//! The library, as Excalidraw's (`data/library.ts`): items — groups of
//! elements — saved from one drawing to drop into any other. An item is
//! Excalidraw's `LibraryItem` JSON; `.excalidrawlib` files of them are read
//! and written by [`crate::file`]. Where apps keep the library is theirs to
//! say (notekpr: `notekpr_core::library`).

use serde_json::{Value, json};

use crate::file::LibraryItem;
use crate::restore::{self, Fresh};
use crate::scene::{Element, ElementType, num};

/// An item as JSON, without its id (kept apart, as the key it is stored
/// under; [`crate::file::write_library`] adds it).
pub fn item_json(item: &LibraryItem) -> Value {
    let mut v = json!({
        "status": item.status,
        "elements": item.elements.iter().map(Element::props).collect::<Vec<_>>(),
        "created": num(item.created),
    });
    if let Some(name) = &item.name {
        v["name"] = json!(name);
    }
    v
}

/// An item from its JSON (an entry of an `.excalidrawlib` file's
/// `libraryItems`, or as kept), its elements restored
/// (`restoreLibraryItems`). `id`: the id it is kept under, if any (else
/// the JSON's, else a new one); `now`: when it was made, when the JSON
/// doesn't say. `None` for an item without elements.
pub fn read_item(
    v: &Value,
    id: Option<&str>,
    now: f64,
    fresh: &mut dyn Fresh,
) -> Option<LibraryItem> {
    let elements = restore::restore_elements(v.get("elements")?.as_array()?, fresh);
    if elements.is_empty() {
        return None;
    }
    let id = match id.or_else(|| v.get("id").and_then(Value::as_str)) {
        Some(id) => id.to_string(),
        None => fresh.id(),
    };
    Some(LibraryItem {
        id,
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unpublished")
            .to_string(),
        created: v
            .get("created")
            .and_then(Value::as_f64)
            .filter(|c| *c != 0.0)
            .unwrap_or(now),
        name: v.get("name").and_then(Value::as_str).map(str::to_string),
        elements,
    })
}

/// Why elements can't go in the library, if they can't: Excalidraw keeps
/// pictures and embedded web pages out (`LIBRARY_DISABLED_TYPES`, with its
/// messages) — an item holds elements only, not the files they'd show.
pub fn refusal(elements: &[Element]) -> Option<&'static str> {
    let has = |k: ElementType| elements.iter().any(|e| e.kind() == k);
    if has(ElementType::Iframe) {
        Some("IFrame elements cannot be added to the library.")
    } else if has(ElementType::Embeddable) {
        Some("Embeddable elements cannot be added to the library.")
    } else if has(ElementType::Image) {
        Some("Pictures can't be added to the library yet.")
    } else {
        None
    }
}

/// Whether `items` hold `item` already: one with the same elements (ids and
/// `versionNonce`), in the same order (`isUniqueItem`). Importing a library
/// twice adds nothing the second time.
pub fn is_duplicate(items: &[LibraryItem], item: &LibraryItem) -> bool {
    let nonce = |e: &Element| e.get("versionNonce").and_then(Value::as_f64);
    items.iter().any(|i| {
        i.elements.len() == item.elements.len()
            && i.elements
                .iter()
                .zip(&item.elements)
                .all(|(a, b)| a.id() == b.id() && nonce(a) == nonce(b))
    })
}

/// The order items are shown in: newest first, as Excalidraw adds them.
pub fn sort(items: &mut [LibraryItem]) {
    items.sort_by(|a, b| {
        b.created
            .total_cmp(&a.created)
            .then_with(|| a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restore::Ids;

    fn item(id: &str, created: f64, els: &[(&str, i64)]) -> LibraryItem {
        let raw: Vec<Value> = els
            .iter()
            .map(|(id, nonce)| json!({"id": id, "type": "rectangle", "versionNonce": nonce}))
            .collect();
        LibraryItem {
            id: id.into(),
            status: "unpublished".into(),
            created,
            name: None,
            elements: restore::restore_elements(&raw, &mut Ids::new(1)),
        }
    }

    #[test]
    fn items_read_back_as_written() {
        let mut a = item("a", 5.0, &[("r", 1), ("s", 2)]);
        a.name = Some("Boxes".into());
        let v = item_json(&a);
        assert!(v.get("id").is_none(), "the id is kept apart");
        assert_eq!(v["name"], "Boxes");
        let back = read_item(&v, Some("a"), 9.0, &mut Ids::new(2)).unwrap();
        assert_eq!(back, a);
        // What a file says wins; what it leaves out is made up.
        let bare = json!({"elements": [{"id": "x", "type": "ellipse"}]});
        let b = read_item(&bare, None, 7.0, &mut Ids::new(3)).unwrap();
        assert_eq!((b.status.as_str(), b.created), ("unpublished", 7.0));
        assert_eq!(b.id.len(), 21);
        assert!(read_item(&json!({"elements": []}), Some("e"), 1.0, &mut Ids::new(1)).is_none());
        assert!(read_item(&json!({}), Some("e"), 1.0, &mut Ids::new(1)).is_none());
    }

    #[test]
    fn duplicates_are_the_same_elements_in_order() {
        let have = vec![item("a", 1.0, &[("r", 1), ("s", 2)])];
        assert!(is_duplicate(
            &have,
            &item("other id", 9.0, &[("r", 1), ("s", 2)])
        ));
        assert!(!is_duplicate(&have, &item("a", 1.0, &[("s", 2), ("r", 1)])));
        assert!(!is_duplicate(&have, &item("a", 1.0, &[("r", 1), ("s", 3)])));
        assert!(!is_duplicate(&have, &item("a", 1.0, &[("r", 1)])));
    }

    #[test]
    fn pictures_stay_out_and_newest_come_first() {
        let raw =
            |t: &str| restore::restore_elements(&[json!({"id": "x", "type": t})], &mut Ids::new(1));
        assert!(refusal(&raw("rectangle")).is_none());
        assert!(refusal(&raw("image")).unwrap().contains("Pictures"));
        assert!(refusal(&raw("embeddable")).is_some());
        let mut items = vec![
            item("b", 1.0, &[("r", 1)]),
            item("c", 3.0, &[("r", 1)]),
            item("a", 1.0, &[("r", 1)]),
        ];
        sort(&mut items);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }
}
