//! The library from the editor (`actionAddToLibrary`): what is selected
//! becomes a new item, named after the first text in it. Putting items in
//! a drawing is a paste ([`Editor::paste`]): copies with new ids, laid out
//! on a grid when there are several, centred where asked.

use std::collections::HashSet;

use super::{Editor, Output, Random};
use crate::file::LibraryItem;
use crate::geom::Point;
use crate::scene::Scene;

impl Editor {
    /// What is selected — with labels, and what is in selected frames — as
    /// a new library item (the elements as they are, ids and all), or why
    /// it can't be one.
    pub fn library_item(
        &self,
        scene: &Scene,
        rng: &mut dyn Random,
    ) -> Result<LibraryItem, &'static str> {
        let ids: HashSet<String> = self
            .dragged_set(scene)
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        // In stacking order.
        let elements: Vec<_> = scene
            .visible()
            .filter(|e| ids.contains(e.id()))
            .cloned()
            .collect();
        if elements.is_empty() {
            return Err("Select something to add it to your saved shapes.");
        }
        if let Some(why) = crate::library::refusal(&elements) {
            return Err(why);
        }
        Ok(LibraryItem {
            id: rng.id(),
            status: "unpublished".into(),
            created: rng.now(),
            name: crate::library::name_from_text(&elements),
            elements,
        })
    }

    /// Puts library items into the drawing (`onInsertLibraryItems`, and
    /// items dropped on the canvas): each item's elements copied with ids of
    /// their own first (`duplicateElements` per item, so two items holding
    /// the same ids — saved twice from one drawing — stay apart, and
    /// attached arrows stay with their item), several laid out on a grid
    /// ([`crate::library::distribute_on_square_grid`]), stacked item after
    /// item, then pasted centred on `at`, selected.
    pub fn insert_library_items(
        &mut self,
        scene: &Scene,
        items: &[LibraryItem],
        at: Point,
        rng: &mut dyn Random,
    ) -> Output {
        // The copies so far: each item's stacking keys come after them.
        let mut copies = Scene::default();
        let mut apart = Vec::with_capacity(items.len());
        for item in items {
            let visible: Vec<_> = item
                .elements
                .iter()
                .filter(|e| !e.is_deleted())
                .cloned()
                .collect();
            let out = self.place_copies(&copies, &visible, [0.0, 0.0], rng);
            copies.elements.extend(out.changed.iter().cloned());
            apart.push(LibraryItem {
                elements: out.changed,
                ..item.clone()
            });
        }
        let elements = crate::library::distribute_on_square_grid(&apart);
        self.paste(scene, &elements, at, rng)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::restore::{Ids, restore_scene};

    struct Seq(u32);

    impl Random for Seq {
        fn id(&mut self) -> String {
            self.0 += 1;
            format!("id{:04}", self.0)
        }
        fn seed(&mut self) -> i32 {
            7
        }
        fn now(&self) -> f64 {
            1_800_000_000_000.0
        }
    }

    fn scene() -> Scene {
        let els = json!([
            {"id": "box", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50,
             "index": "a1", "boundElements": [{"id": "label", "type": "text"}]},
            {"id": "label", "type": "text", "x": 10, "y": 10, "width": 80, "height": 25,
             "index": "a2", "text": "Hi", "originalText": "Hi", "containerId": "box"},
            {"id": "other", "type": "ellipse", "x": 300, "y": 0, "width": 40, "height": 40, "index": "a3"},
            {"id": "pic", "type": "image", "x": 0, "y": 200, "width": 40, "height": 40, "index": "a4"},
        ]);
        restore_scene(els.as_array().unwrap(), None, None, &mut Ids::new(1))
    }

    #[test]
    fn the_selection_with_its_label_becomes_an_item() {
        let s = scene();
        let mut ed = Editor::default();
        assert!(
            ed.library_item(&s, &mut Seq(0)).is_err(),
            "nothing selected"
        );
        ed.selection = vec!["box".into()];
        let item = ed.library_item(&s, &mut Seq(0)).unwrap();
        let ids: Vec<&str> = item.elements.iter().map(|e| e.id()).collect();
        assert_eq!(ids, ["box", "label"]);
        assert_eq!(
            (item.id.as_str(), item.status.as_str()),
            ("id0001", "unpublished")
        );
        assert_eq!(item.created, 1_800_000_000_000.0);
        assert_eq!(item.name.as_deref(), Some("Hi"), "named after its text");
        ed.selection = vec!["other".into()];
        let plain = ed.library_item(&s, &mut Seq(0)).unwrap();
        assert_eq!(plain.name, None, "no text, no name");
        ed.selection = vec!["box".into()];

        // Put back in: copies with new ids, centred where asked.
        let out = ed.paste(&s, &item.elements, [1000.0, 1000.0], &mut Seq(10));
        assert_eq!(out.changed.len(), 2);
        assert!(
            out.changed
                .iter()
                .all(|e| !["box", "label"].contains(&e.id()))
        );
        let copy = out.changed.iter().find(|e| e.text().is_empty()).unwrap();
        assert_eq!((copy.x(), copy.y()), (950.0, 975.0));
    }

    #[test]
    fn several_items_go_in_side_by_side_each_with_ids_of_its_own() {
        let s = scene();
        let mut ed = Editor {
            selection: vec!["box".into()],
            ..Editor::default()
        };
        // The same shape saved twice: the same element ids in both.
        let one = ed.library_item(&s, &mut Seq(0)).unwrap();
        let two = LibraryItem {
            id: "again".into(),
            ..one.clone()
        };
        let out = ed.insert_library_items(&s, &[one.clone(), two], [500.0, 500.0], &mut Seq(100));
        assert_eq!(out.changed.len(), 4);
        let ids: HashSet<&str> = out.changed.iter().map(|e| e.id()).collect();
        assert_eq!(ids.len(), 4, "every copy has an id of its own");
        assert!(ids.iter().all(|i| !["box", "label"].contains(i)));
        // Each label is in its own box, and its box knows it.
        let boxes: Vec<_> = out.changed.iter().filter(|e| e.text().is_empty()).collect();
        let labels: Vec<_> = out
            .changed
            .iter()
            .filter(|e| !e.text().is_empty())
            .collect();
        for (b, l) in boxes.iter().zip(&labels) {
            assert_eq!(l.container_id(), Some(b.id()));
            assert_eq!(b.bound_elements()[0].id, l.id());
        }
        // Side by side, 50 apart, centred on the point together.
        assert_eq!(boxes[1].x() - boxes[0].x(), 150.0);
        assert_eq!(boxes[0].y(), boxes[1].y());
        assert_eq!((boxes[0].x() + boxes[1].x() + 100.0) / 2.0, 500.0);
        // Stacked item after item, above what was there; both selected.
        let index = |e: &crate::scene::Element| e.index().unwrap().to_string();
        let keys: Vec<String> = out.changed.iter().map(index).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(keys[0].as_str() > s.elements.iter().map(index).max().unwrap().as_str());
        assert_eq!(out.changed[0].id(), boxes[0].id());
        assert_eq!(out.changed[2].id(), boxes[1].id());
        assert_eq!(ed.selection.len(), 2, "the boxes (their labels come along)");
        // One alone goes in as a paste would put it.
        let alone = ed.insert_library_items(&s, &[one], [1000.0, 1000.0], &mut Seq(200));
        let copy = alone.changed.iter().find(|e| e.text().is_empty()).unwrap();
        assert_eq!((copy.x(), copy.y()), (950.0, 975.0));
    }

    #[test]
    fn pictures_stay_out() {
        let s = scene();
        let ed = Editor {
            selection: vec!["other".into(), "pic".into()],
            ..Editor::default()
        };
        let why = ed.library_item(&s, &mut Seq(0)).unwrap_err();
        assert!(why.contains("Pictures"), "{why}");
    }
}
