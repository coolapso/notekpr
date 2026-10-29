//! What a picture of a drawing shows — its PNG and SVG exports and copies —
//! as Excalidraw decides it (`prepareElementsForExport` in `data/index.ts`,
//! `exportingFrame` in `scene/export.ts`; MIT, see `PORTED.md`): the whole
//! drawing, or what is selected. A frame selected on its own is exported
//! alone: what overlaps it, cut to it, the picture the frame's size,
//! without its outline and name.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::bounds::{self, Bounds};
use crate::edit::frame;
use crate::scene::{Element, Scene, num};

/// What a picture shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// Its elements, with the drawing's settings and files: also what goes
    /// inside the picture (Excalidraw's "Embed scene").
    pub scene: Scene,
    /// The frame exported alone (its id), if one is (`exportingFrame`).
    pub frame: Option<String>,
}

impl Part {
    /// The whole drawing.
    pub fn whole(scene: &Scene) -> Self {
        Self {
            scene: scene.clone(),
            frame: None,
        }
    }

    /// A frame alone: what overlaps it, the frame among it
    /// ([`frame::overlapping`]).
    pub fn frame(scene: &Scene, frame: &Element) -> Self {
        let ids: HashSet<&str> = frame::overlapping(scene, frame)
            .into_iter()
            .map(Element::id)
            .collect();
        Self {
            scene: subset(scene, &ids),
            frame: Some(frame.id().to_string()),
        }
    }

    /// What is selected, as Excalidraw's exports take it with "Only
    /// selected" (`prepareElementsForExport`): one frame alone (see
    /// [`Part::frame`]); one other element and its label; several, their
    /// labels and what is in the frames among them. With nothing selected,
    /// the whole drawing.
    pub fn selected(scene: &Scene, selection: &[String]) -> Self {
        let picked = selected_with_labels(scene, selection);
        match picked[..] {
            [] => return Self::whole(scene),
            [f] if frame::is_frame(f) => return Self::frame(scene, f),
            _ => {}
        }
        let mut ids: HashSet<&str> = picked.iter().map(|e| e.id()).collect();
        if picked.len() > 1 {
            // `includeElementsInFrames`, and the labels of what is in them.
            let frames: HashSet<&str> = picked
                .iter()
                .filter(|e| frame::is_frame(e))
                .map(|e| e.id())
                .collect();
            let children: Vec<&str> = scene
                .visible()
                .filter(|e| e.frame_id().is_some_and(|f| frames.contains(f)))
                .map(Element::id)
                .collect();
            ids.extend(children);
            let labels: Vec<&str> = scene
                .visible()
                .filter(|e| e.container_id().is_some_and(|c| ids.contains(c)))
                .map(Element::id)
                .collect();
            ids.extend(labels);
        }
        Self {
            scene: subset(scene, &ids),
            frame: None,
        }
    }

    /// The frame exported alone, if one is and it is there.
    pub fn exported_frame(&self) -> Option<&Element> {
        let id = self.frame.as_deref()?;
        self.scene
            .visible()
            .find(|e| e.id() == id && frame::is_frame(e))
    }

    /// What the picture of a frame alone frames: the frame, with no padding
    /// around it (`getCanvasSize([exportingFrame])`).
    pub fn frame_bounds(&self) -> Option<Bounds> {
        self.exported_frame().map(bounds::element_bounds)
    }

    /// The part for a renderer that shows frames as the editor does
    /// (outlined, named, what is in them cut to their round corners). A
    /// frame alone comes without the frames: none is outlined or named in
    /// its picture, whose edges are the frame's and cut everything to it;
    /// what was in a frame keeps its opacity (`getRenderOpacity` multiplies
    /// the frame's and the element's). Otherwise the part as it is.
    pub fn drawn(&self) -> Cow<'_, Scene> {
        if self.frame.is_none() {
            return Cow::Borrowed(&self.scene);
        }
        let frames: HashMap<&str, f64> = self
            .scene
            .visible()
            .filter(|e| frame::is_frame(e))
            .map(|e| (e.id(), e.opacity()))
            .collect();
        let elements = self
            .scene
            .visible()
            .filter(|e| !frame::is_frame(e))
            .map(|e| {
                let mut e = e.clone();
                let opacity = e.frame_id().and_then(|f| frames.get(f)).copied();
                if let Some(o) = opacity {
                    e.set("opacity", num(o * e.opacity() / 100.0));
                }
                e
            })
            .collect();
        Cow::Owned(Scene {
            elements,
            app_state: self.scene.app_state.clone(),
            files: self.scene.files.clone(),
        })
    }
}

/// The frame selected on its own, if that is what is selected: exported
/// alone.
pub fn selected_frame<'a>(scene: &'a Scene, selection: &[String]) -> Option<&'a Element> {
    match selected_with_labels(scene, selection)[..] {
        [f] if frame::is_frame(f) => Some(f),
        _ => None,
    }
}

/// `getSelectedElements(…, { includeBoundTextElement: true })`: the
/// selected elements there, and the labels of those, in stacking order.
fn selected_with_labels<'a>(scene: &'a Scene, selection: &[String]) -> Vec<&'a Element> {
    let selected: HashSet<&str> = selection.iter().map(String::as_str).collect();
    scene
        .visible()
        .filter(|e| {
            selected.contains(e.id()) || e.container_id().is_some_and(|c| selected.contains(c))
        })
        .collect()
}

/// The elements of `scene` named, in its stacking order.
fn subset(scene: &Scene, ids: &HashSet<&str>) -> Scene {
    Scene {
        elements: scene
            .visible()
            .filter(|e| ids.contains(e.id()))
            .cloned()
            .collect(),
        app_state: scene.app_state.clone(),
        files: scene.files.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::file::scene_from_json;
    use crate::restore::Ids;

    /// A frame at 100,100 (200 × 100), what is in it, what crosses its edge
    /// and what doesn't reach it.
    fn drawing() -> Scene {
        let el = |id: &str, kind: &str, x: f64, y: f64, w: f64, h: f64, more: Value| {
            let mut e = json!({"id": id, "type": kind, "x": x, "y": y, "width": w, "height": h});
            for (k, v) in more.as_object().unwrap() {
                e[k] = v.clone();
            }
            e
        };
        scene_from_json(
            &json!({"type": "excalidraw", "elements": [
                el("inside", "rectangle", 120.0, 120.0, 40.0, 40.0,
                   json!({"frameId": "f", "opacity": 80,
                          "boundElements": [{"id": "label", "type": "text"}]})),
                el("label", "text", 125.0, 130.0, 30.0, 20.0,
                   json!({"frameId": "f", "containerId": "inside", "text": "hi",
                          "originalText": "hi"})),
                el("f", "frame", 100.0, 100.0, 200.0, 100.0, json!({"name": "Plan", "opacity": 50})),
                // In no frame, across its right edge.
                el("across", "ellipse", 280.0, 150.0, 60.0, 30.0, json!({})),
                // Only touching its bottom edge.
                el("touching", "rectangle", 150.0, 200.0, 20.0, 20.0, json!({})),
                el("away", "rectangle", 500.0, 500.0, 20.0, 20.0, json!({})),
                // In another frame, over this one.
                el("g", "frame", 250.0, 50.0, 100.0, 100.0, json!({})),
                el("other", "rectangle", 260.0, 120.0, 30.0, 20.0, json!({"frameId": "g"})),
                el("gone", "rectangle", 130.0, 130.0, 10.0, 10.0, json!({"isDeleted": true})),
            ]}),
            &mut Ids::new(1),
        )
        .unwrap()
    }

    fn ids(part: &Part) -> Vec<&str> {
        part.scene.elements.iter().map(Element::id).collect()
    }

    fn sel(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_frame_alone_is_what_overlaps_it() {
        let s = drawing();
        let f = s.element("f").unwrap();
        // Not what only touches it, is far, is in another frame or deleted;
        // the other frame itself overlaps it.
        let part = Part::frame(&s, f);
        assert_eq!(ids(&part), ["inside", "label", "f", "across", "g"]);
        assert_eq!(part.frame.as_deref(), Some("f"));
        assert_eq!(part.frame_bounds(), Some([100.0, 100.0, 300.0, 200.0]));
        assert_eq!(part.scene.app_state, s.app_state);
    }

    #[test]
    fn what_is_selected() {
        let s = drawing();
        // Nothing (or only what isn't there): the whole drawing.
        assert_eq!(Part::selected(&s, &[]), Part::whole(&s));
        assert_eq!(Part::selected(&s, &sel(&["gone"])), Part::whole(&s));
        // A frame alone.
        let part = Part::selected(&s, &sel(&["f"]));
        assert_eq!(part, Part::frame(&s, s.element("f").unwrap()));
        assert_eq!(selected_frame(&s, &sel(&["f"])).map(Element::id), Some("f"));
        // One element: with its label.
        let part = Part::selected(&s, &sel(&["inside"]));
        assert_eq!(ids(&part), ["inside", "label"]);
        assert_eq!(part.frame, None);
        // Several: a frame comes with what is in it (and their labels).
        let part = Part::selected(&s, &sel(&["away", "g"]));
        assert_eq!(ids(&part), ["away", "g", "other"]);
        let part = Part::selected(&s, &sel(&["f", "away"]));
        assert_eq!(ids(&part), ["inside", "label", "f", "away"]);
        assert_eq!(part.frame, None);
        assert_eq!(selected_frame(&s, &sel(&["f", "away"])), None);
        assert_eq!(selected_frame(&s, &sel(&["inside"])), None);
    }

    #[test]
    fn drawn_without_frames() {
        let s = drawing();
        let part = Part::selected(&s, &sel(&["f"]));
        let drawn = part.drawn();
        let got: Vec<(&str, f64)> = drawn
            .elements
            .iter()
            .map(|e| (e.id(), e.opacity()))
            .collect();
        // Half the frame's opacity in what was in it.
        assert_eq!(got, [("inside", 40.0), ("label", 50.0), ("across", 100.0)]);
        // Anything else is drawn as it is.
        let part = Part::selected(&s, &sel(&["f", "away"]));
        assert_eq!(*part.drawn(), part.scene);
    }
}
