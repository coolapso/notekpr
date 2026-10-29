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
        Some("IFrame elements cannot be saved as shapes.")
    } else if has(ElementType::Embeddable) {
        Some("Embeddable elements cannot be saved as shapes.")
    } else if has(ElementType::Image) {
        Some("Pictures can't be saved as shapes yet.")
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

/// The longest name [`name_from_text`] makes, in characters (the ellipsis
/// included).
pub const NAME_FROM_TEXT_MAX: usize = 40;

/// A name for a new item from the first text in it (not Excalidraw's: its
/// items are named only when published): its words on one line, cut at a
/// word to at most [`NAME_FROM_TEXT_MAX`] characters. `None` for an item
/// without text.
pub fn name_from_text(elements: &[Element]) -> Option<String> {
    let text = elements
        .iter()
        .filter(|e| e.kind() == ElementType::Text && !e.is_deleted())
        .map(|e| {
            e.original_text()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .find(|t| !t.is_empty())?;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= NAME_FROM_TEXT_MAX {
        return Some(text);
    }
    // Room for the ellipsis.
    let n = NAME_FROM_TEXT_MAX - 1;
    let head: String = chars[..n].iter().collect();
    // At a word, unless that leaves too little.
    let cut = match head.rfind(' ') {
        _ if chars[n] == ' ' => head.as_str(),
        Some(i) if head[..i].chars().count() >= NAME_FROM_TEXT_MAX / 2 => &head[..i],
        _ => head.as_str(),
    };
    Some(format!("{}…", cut.trim_end()))
}

/// Several items' elements laid out side by side on a square grid
/// (Excalidraw's `distributeLibraryItemsOnSquareGrid`), to put them into a
/// drawing at once: `⌈√n⌉` items a row, 50 apart, each centred in its
/// row's height and its column's width, the first one's box at 0, 0. The
/// elements come back in item order, moved only.
pub fn distribute_on_square_grid(items: &[LibraryItem]) -> Vec<Element> {
    const PADDING: f64 = 50.0;
    let per_row = (items.len() as f64).sqrt().ceil().max(1.0) as usize;
    // `getCommonBoundingBox`: min x, min y, width, height.
    let boxes: Vec<[f64; 4]> = items
        .iter()
        .map(|i| {
            let [x1, y1, x2, y2] = crate::bounds::common_bounds(&i.elements);
            [x1, y1, x2 - x1, y2 - y1]
        })
        .collect();
    let max_height_of_row = |row: usize| {
        boxes
            .iter()
            .skip(row * per_row)
            .take(per_row)
            .fold(0.0, |acc: f64, b| crate::js::max(acc, b[3]))
    };
    let max_width_of_col = |col: usize| {
        boxes
            .iter()
            .enumerate()
            .filter(|(i, _)| i % per_row == col)
            .fold(0.0, |acc: f64, (_, b)| crate::js::max(acc, b[2]))
    };
    let mut out = Vec::new();
    let (mut col_offset_x, mut row_offset_y) = (0.0, 0.0);
    let mut max_height_of_this_row = 0.0;
    let (mut col, mut row) = (0, 0);
    for (index, (item, b)) in items.iter().zip(&boxes).enumerate() {
        if index > 0 && index % per_row == 0 {
            row_offset_y += max_height_of_this_row + PADDING;
            col_offset_x = 0.0;
            col = 0;
            row += 1;
        }
        if col == 0 {
            max_height_of_this_row = max_height_of_row(row);
        }
        let max_width_of_this_col = max_width_of_col(col);
        let [min_x, min_y, width, height] = *b;
        let center_x = (max_width_of_this_col - width) / 2.0;
        let center_y = (max_height_of_this_row - height) / 2.0;
        for e in &item.elements {
            let mut e = e.clone();
            e.set("x", num(e.x() + col_offset_x + center_x - min_x));
            e.set("y", num(e.y() + row_offset_y + center_y - min_y));
            out.push(e);
        }
        col_offset_x += max_width_of_this_col + PADDING;
        col += 1;
    }
    out
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

    fn boxes(id: &str, els: &[(&str, f64, f64, f64, f64)]) -> LibraryItem {
        let raw: Vec<Value> = els
            .iter()
            .map(|(id, x, y, w, h)| {
                json!({"id": id, "type": "rectangle", "x": x, "y": y, "width": w, "height": h})
            })
            .collect();
        LibraryItem {
            id: id.into(),
            status: "unpublished".into(),
            created: 1.0,
            name: None,
            elements: restore::restore_elements(&raw, &mut Ids::new(1)),
        }
    }

    /// Excalidraw's own test (`library.test.tsx`,
    /// `distributeLibraryItemsOnSquareGrid()`), number for number.
    #[test]
    fn items_are_laid_out_on_a_square_grid() {
        const PADDING: f64 = 50.0;
        let (w1, h1) = (100.0, 100.0);
        let (w2, h2) = (100.0, 80.0);
        let (w3, h3) = (40.0, 50.0);
        let (x4, y4, w4, h4) = (0.0, 0.0, 50.0, 50.0);
        let (x5, y5, w5, h5) = (40.0, 0.0, 70.0, 100.0);
        let items = [
            boxes("1", &[("id1", 0.0, 0.0, w1, h1)]),
            boxes("2", &[("id2", -100.0, -50.0, w2, h2)]),
            boxes("3", &[("id3", -100.0, -50.0, w3, h3)]),
            boxes("4", &[("id4", x4, y4, w4, h4), ("id5", x5, y5, w5, h5)]),
        ];
        let out = distribute_on_square_grid(&items);
        let ids: Vec<&str> = out.iter().map(|e| e.id()).collect();
        assert_eq!(ids, ["id1", "id2", "id3", "id4", "id5"], "item by item");
        let at = |id: &str| {
            let e = out.iter().find(|e| e.id() == id).unwrap();
            (e.x(), e.y())
        };
        // [id4, id5]'s box is 110 wide.
        let w45 = (x5 + w5) - x4;
        assert_eq!(at("id1"), (0.0, 0.0));
        assert_eq!(
            at("id2"),
            (w1 + PADDING + (w45 - w2) / 2.0, (h1 - h2).abs() / 2.0)
        );
        assert_eq!(
            at("id3"),
            (
                (w1 - w3).abs() / 2.0,
                h1.max(h2) + PADDING + (h3 - h4.max(h5)).abs() / 2.0
            )
        );
        assert_eq!(at("id4"), (w1.max(w2) + PADDING, h1.max(h2) + PADDING));
        assert_eq!(
            at("id5"),
            (
                w1.max(w2) + PADDING + (x5 - x4).abs(),
                h1.max(h2) + PADDING + (y5 - y4).abs()
            )
        );
        // Sizes stay; one item alone starts at 0, 0.
        assert!(
            out.iter()
                .zip(items.iter().flat_map(|i| &i.elements))
                .all(|(a, b)| a.width() == b.width())
        );
        let one = distribute_on_square_grid(&items[1..2]);
        assert_eq!((one[0].x(), one[0].y()), (0.0, 0.0));
        assert!(distribute_on_square_grid(&[]).is_empty());
    }

    #[test]
    fn a_name_comes_from_the_first_text() {
        let els = |texts: &[&str]| {
            let raw: Vec<Value> = std::iter::once(json!({"id": "r", "type": "rectangle"}))
                .chain(texts.iter().enumerate().map(
                    |(i, t)| json!({"id": format!("t{i}"), "type": "text", "text": t, "originalText": t}),
                ))
                .collect();
            restore::restore_elements(&raw, &mut Ids::new(1))
        };
        assert_eq!(name_from_text(&els(&[])), None);
        assert_eq!(name_from_text(&els(&["", "  "])), None);
        assert_eq!(
            name_from_text(&els(&["", "Raised\n  bed", "Path"])).as_deref(),
            Some("Raised bed")
        );
        // Long: cut at a word.
        let long = "Water the tomatoes every morning before it gets too hot outside";
        let name = name_from_text(&els(&[long])).unwrap();
        assert_eq!(name, "Water the tomatoes every morning before…");
        assert!(name.chars().count() <= NAME_FROM_TEXT_MAX);
        // One long word: cut anywhere.
        let word = "x".repeat(60);
        let name = name_from_text(&els(&[&word])).unwrap();
        assert_eq!(name.chars().count(), NAME_FROM_TEXT_MAX);
        assert!(name.ends_with('…'));
    }
}
