//! The library from the editor (`actionAddToLibrary`): what is selected
//! becomes a new item. Putting an item in a drawing is a paste
//! ([`Editor::paste`]): copies with new ids, centred where asked.

use std::collections::HashSet;

use super::{Editor, Random};
use crate::file::LibraryItem;
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
            name: None,
            elements,
        })
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
