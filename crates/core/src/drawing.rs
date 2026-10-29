//! A drawing as a CRDT, so people can draw together and offline edits merge.
//!
//! Every object of a vault is a [`NoteDoc`] (a yrs document); a drawing is
//! one whose content is three maps instead of a note's text:
//!
//! * `elements`: element id → a map of the element's properties (Excalidraw's
//!   model, see `notekpr_draw::scene`). Each property merges on its own, so
//!   one person moving a shape while another recolours it keeps both. The
//!   stacking order is each element's fractional `index`, so it merges too.
//!   Elements are never removed, only marked `isDeleted`, as in Excalidraw:
//!   a deletion and someone's concurrent edit of the element don't fight.
//! * `settings`: the drawing's own settings (background, grid).
//! * `files`: pictures of image elements, by file id.
//!
//! Values are kept as yrs `Any`, numbers as JavaScript has them: whole ones
//! as integers, the rest as floats (`notekpr_draw::scene::num`).

use std::cmp::Ordering;
use std::sync::Arc;

use notekpr_draw::scene::{Element, Props, SAVED_APP_STATE, Scene, num};
use serde_json::Value;
use yrs::any::Number;
use yrs::{Any, Map, MapPrelim, MapRef, Out, ReadTxn, Transact, TransactionMut};

use crate::doc::{LOCAL_ORIGIN, NoteDoc};

const ELEMENTS: &str = "elements";
const SETTINGS: &str = "settings";
const FILES: &str = "files";

/// A drawing's root maps (elements, settings, pictures).
pub(crate) fn roots(doc: &yrs::Doc) -> [MapRef; 3] {
    [
        doc.get_or_insert_map(ELEMENTS),
        doc.get_or_insert_map(SETTINGS),
        doc.get_or_insert_map(FILES),
    ]
}

/// A JSON value as yrs keeps it.
pub fn to_any(v: &Value) -> Any {
    match v {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => match num(n.as_f64().unwrap_or(0.0)) {
            Value::Number(n) if n.is_i64() => Any::Number(Number::Int(n.as_i64().unwrap_or(0))),
            Value::Number(n) => Any::Number(Number::Float(n.as_f64().unwrap_or(0.0))),
            _ => Any::Null,
        },
        Value::String(s) => Any::String(Arc::from(s.as_str())),
        Value::Array(a) => Any::Array(a.iter().map(to_any).collect()),
        Value::Object(o) => Any::Map(Arc::new(
            o.iter().map(|(k, v)| (k.clone(), to_any(v))).collect(),
        )),
    }
}

/// A yrs value as JSON.
pub fn to_value(a: &Any) -> Value {
    match a {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(Number::Int(i)) => num(*i as f64),
        Any::Number(Number::Float(f)) => num(*f),
        Any::String(s) => Value::String(s.to_string()),
        // Not written by notekpr; kept readable.
        Any::Buffer(b) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
        Any::Array(a) => Value::Array(a.iter().map(to_value).collect()),
        Any::Map(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), to_value(v))).collect()),
    }
}

fn props_of<T: ReadTxn>(txn: &T, m: &MapRef) -> Props {
    m.iter(txn)
        .filter_map(|(k, v)| match v {
            Out::Any(a) => Some((k.to_string(), to_value(&a))),
            _ => None,
        })
        .collect()
}

fn element_prelim(props: &Props) -> MapPrelim {
    props.iter().map(|(k, v)| (k.as_str(), to_any(v))).collect()
}

/// Sets `props` on `m`, writing only what differs; with `exact`, also
/// removes properties `props` doesn't have.
fn write_props(txn: &mut TransactionMut, m: &MapRef, props: &Props, exact: bool) {
    for (k, v) in props {
        let new = to_any(v);
        let same = matches!(m.get(txn, k), Some(Out::Any(old)) if old == new);
        if !same {
            m.insert(txn, k.as_str(), new);
        }
    }
    if exact {
        let gone: Vec<String> = m
            .keys(txn)
            .filter(|k| !props.contains_key(*k))
            .map(str::to_string)
            .collect();
        for k in gone {
            m.remove(txn, &k);
        }
    }
}

/// Stacking order: by fractional index, elements without one last, ties by
/// id (two people may give two elements the same index at once).
fn stacking(a: &Element, b: &Element) -> Ordering {
    match (a.index(), b.index()) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| a.id().cmp(b.id()))
}

/// A drawing's view of an object document.
pub struct Drawing<'a> {
    doc: &'a NoteDoc,
}

impl NoteDoc {
    /// This object as a drawing (empty if it holds something else).
    pub fn drawing(&self) -> Drawing<'_> {
        Drawing { doc: self }
    }
}

impl Drawing<'_> {
    fn roots(&self) -> (MapRef, MapRef, MapRef) {
        let d = self.doc.doc();
        (
            d.get_or_insert_map(ELEMENTS),
            d.get_or_insert_map(SETTINGS),
            d.get_or_insert_map(FILES),
        )
    }

    fn edit(
        &self,
        f: impl FnOnce(&mut TransactionMut, &MapRef, &MapRef, &MapRef),
    ) -> Option<Vec<u8>> {
        let (elements, settings, files) = self.roots();
        {
            let mut txn = self.doc.doc().transact_mut_with(LOCAL_ORIGIN);
            f(&mut txn, &elements, &settings, &files);
        }
        self.doc.take_local_update()
    }

    /// The whole drawing, elements in stacking order.
    pub fn scene(&self) -> Scene {
        let (elements, settings, files) = self.roots();
        let txn = self.doc.doc().transact();
        let mut els: Vec<Element> = elements
            .iter(&txn)
            .filter_map(|(id, v)| match v {
                Out::YMap(m) => {
                    let mut props = props_of(&txn, &m);
                    // The map's key is the element's id, whatever it says.
                    props.insert("id".into(), Value::String(id.to_string()));
                    Element::from_props(props)
                }
                _ => None,
            })
            .collect();
        els.sort_by(stacking);
        Scene {
            elements: els,
            app_state: props_of(&txn, &settings),
            files: props_of(&txn, &files),
        }
    }

    pub fn element(&self, id: &str) -> Option<Element> {
        let (elements, _, _) = self.roots();
        let txn = self.doc.doc().transact();
        match elements.get(&txn, id)? {
            Out::YMap(m) => {
                let mut props = props_of(&txn, &m);
                props.insert("id".into(), Value::String(id.to_string()));
                Element::from_props(props)
            }
            _ => None,
        }
    }

    /// The text of the drawing's text elements, one per line: what search
    /// finds in it.
    pub fn text(&self) -> String {
        self.scene().texts().collect::<Vec<_>>().join("\n")
    }

    /// Whether the document holds a drawing at all.
    pub fn is_empty(&self) -> bool {
        let (elements, _, _) = self.roots();
        elements.len(&self.doc.doc().transact()) == 0
    }

    /// Makes the drawing `scene`, changing only what differs (one update,
    /// `None` if nothing did): elements are added or have properties
    /// changed; elements `scene` doesn't have are marked deleted. For a
    /// whole drawing coming from elsewhere: an imported file, the Markdown
    /// mirror.
    pub fn set_scene(&self, scene: &Scene) -> Option<Vec<u8>> {
        self.edit(|txn, elements, settings, files| {
            let mut ids = std::collections::HashSet::new();
            for e in &scene.elements {
                ids.insert(e.id().to_string());
                put(txn, elements, e);
            }
            let missing: Vec<(String, MapRef)> = elements
                .iter(txn)
                .filter(|(id, _)| !ids.contains(*id))
                .filter_map(|(id, v)| match v {
                    Out::YMap(m) => Some((id.to_string(), m)),
                    _ => None,
                })
                .collect();
            for (_, m) in missing {
                let deleted = matches!(m.get(txn, "isDeleted"), Some(Out::Any(Any::Bool(true))));
                if !deleted {
                    m.insert(txn, "isDeleted", true);
                }
            }
            let mut saved = Props::new();
            for k in SAVED_APP_STATE {
                if let Some(v) = scene.app_state.get(k) {
                    saved.insert(k.to_string(), v.clone());
                }
            }
            write_props(txn, settings, &saved, true);
            write_props(txn, files, &scene.files, false);
        })
    }

    /// [`set_scene`](Self::set_scene) as several updates of about `budget`
    /// bytes at most (the server refuses larger ones): changed elements in
    /// batches, then deletions and settings. An element larger than
    /// `budget` on its own still goes in one update.
    pub fn set_scene_split(&self, scene: &Scene, budget: usize) -> Vec<Vec<u8>> {
        let current: std::collections::HashMap<String, Props> = self
            .scene()
            .elements
            .into_iter()
            .map(|e| (e.id().to_string(), e.into_props()))
            .collect();
        let mut out = Vec::new();
        let mut batch: Vec<Element> = Vec::new();
        let mut size = 0;
        for e in &scene.elements {
            // As stored: numbers the way yrs keeps them.
            let canonical: Props = e
                .props()
                .iter()
                .map(|(k, v)| (k.clone(), to_value(&to_any(v))))
                .collect();
            if current.get(e.id()) == Some(&canonical) {
                continue;
            }
            // About the size of the JSON, plus what yrs adds per property
            // (the parent map's id, the key).
            let len =
                serde_json::to_string(&canonical).map_or(0, |s| s.len()) + 12 * canonical.len();
            if !batch.is_empty() && size + len > budget {
                out.extend(self.put(&batch));
                batch.clear();
                size = 0;
            }
            size += len;
            batch.push(e.clone());
        }
        if !batch.is_empty() {
            out.extend(self.put(&batch));
        }
        // What's left: elements no longer there, settings, files (each
        // picture is a reference, small).
        out.extend(self.set_scene(scene));
        out
    }

    /// The size the drawing takes, roughly (its whole state, encoded).
    pub fn size(&self) -> u64 {
        self.doc.encode_state().len() as u64
    }

    /// Adds elements or makes them as given (all their properties).
    pub fn put(&self, els: &[Element]) -> Option<Vec<u8>> {
        self.edit(|txn, elements, _, _| {
            for e in els {
                put(txn, elements, e);
            }
        })
    }

    /// Sets some properties of elements (id → properties); elements that
    /// aren't there are left alone.
    pub fn set(&self, changes: &[(&str, Props)]) -> Option<Vec<u8>> {
        self.edit(|txn, elements, _, _| {
            for (id, props) in changes {
                if let Some(Out::YMap(m)) = elements.get(txn, id) {
                    write_props(txn, &m, props, false);
                }
            }
        })
    }

    /// Sets some of the drawing's settings.
    pub fn set_settings(&self, props: &Props) -> Option<Vec<u8>> {
        self.edit(|txn, _, settings, _| write_props(txn, settings, props, false))
    }

    /// Adds pictures (file id → `mimeType`, `id`, `created`, …).
    pub fn put_files(&self, props: &Props) -> Option<Vec<u8>> {
        self.edit(|txn, _, _, files| write_props(txn, files, props, false))
    }
}

/// An element added, or made exactly as given.
fn put(txn: &mut TransactionMut, elements: &MapRef, e: &Element) {
    let mut props = e.props().clone();
    props.remove("id");
    match elements.get(txn, e.id()) {
        Some(Out::YMap(m)) => write_props(txn, &m, &props, true),
        _ => {
            elements.insert(txn, e.id(), element_prelim(&props));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notekpr_draw::file::read_drawing;
    use notekpr_draw::restore::Ids;
    use serde_json::json;

    const EXAMPLE: &str = include_str!("../../draw/tests/fixtures/example.excalidraw");

    fn el(v: Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    fn props(v: Value) -> Props {
        v.as_object().unwrap().clone()
    }

    /// Two documents that exchanged everything.
    fn sync(a: &NoteDoc, b: &NoteDoc) {
        let ua = a.encode_state();
        let ub = b.encode_state();
        a.apply_update(&ub).unwrap();
        b.apply_update(&ua).unwrap();
    }

    #[test]
    fn whole_drawing_survives() {
        let scene = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap().scene;
        let doc = NoteDoc::new();
        assert!(doc.drawing().is_empty());
        let update = doc.drawing().set_scene(&scene).unwrap();
        assert_eq!(doc.drawing().scene(), scene);
        // Nothing to do the second time.
        assert_eq!(doc.drawing().set_scene(&scene), None);
        // Another device gets the same drawing from the update.
        let other = NoteDoc::from_updates([update.as_slice()]).unwrap();
        assert_eq!(other.drawing().scene(), scene);
        // A note's text and a drawing don't see each other.
        assert_eq!(other.text(), "");
        assert!(other.drawing().text().contains("SearchBase Core"));
    }

    #[test]
    fn numbers_stay_as_javascript_has_them() {
        for v in [
            json!(3),
            json!(2.5),
            json!(-0.0),
            json!(1172987811),
            json!([1, 2.5, [3]]),
        ] {
            assert_eq!(to_value(&to_any(&v)), num_like(&v));
        }
        fn num_like(v: &Value) -> Value {
            match v {
                Value::Number(n) => num(n.as_f64().unwrap()),
                Value::Array(a) => Value::Array(a.iter().map(num_like).collect()),
                other => other.clone(),
            }
        }
    }

    #[test]
    fn concurrent_edits_of_one_shape_both_stay() {
        let a = NoteDoc::new();
        let rect = el(
            json!({"id": "r", "type": "rectangle", "x": 0, "y": 0, "strokeColor": "#000", "index": "a0"}),
        );
        a.drawing().put(&[rect]).unwrap();
        let b = NoteDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        // One moves it, the other recolours it.
        a.drawing()
            .set(&[("r", props(json!({"x": 50, "y": 60})))])
            .unwrap();
        b.drawing()
            .set(&[("r", props(json!({"strokeColor": "#e03131"})))])
            .unwrap();
        sync(&a, &b);
        for d in [&a, &b] {
            let r = d.drawing().element("r").unwrap();
            assert_eq!((r.x(), r.y(), r.stroke_color()), (50.0, 60.0, "#e03131"));
        }
    }

    #[test]
    fn deleting_and_editing_at_once() {
        let a = NoteDoc::new();
        a.drawing()
            .put(&[
                el(json!({"id": "r", "type": "ellipse", "index": "a0"})),
                el(json!({"id": "s", "type": "ellipse", "index": "a1"})),
            ])
            .unwrap();
        let b = NoteDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        // A deletes `r` by leaving it out; B moves it.
        let mut scene = a.drawing().scene();
        scene.elements.retain(|e| e.id() != "r");
        a.drawing().set_scene(&scene).unwrap();
        b.drawing().set(&[("r", props(json!({"x": 9})))]).unwrap();
        sync(&a, &b);
        let r = a.drawing().element("r").unwrap();
        assert!(r.is_deleted());
        assert_eq!(r.x(), 9.0);
        assert_eq!(a.drawing().scene(), b.drawing().scene());
    }

    #[test]
    fn stacking_order_merges() {
        let a = NoteDoc::new();
        a.drawing()
            .put(&[
                el(json!({"id": "x", "type": "rectangle", "index": "a0"})),
                el(json!({"id": "y", "type": "rectangle", "index": "a1"})),
                el(json!({"id": "z", "type": "rectangle", "index": "a2"})),
            ])
            .unwrap();
        let b = NoteDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        // A brings x to the front; B sends z to the back.
        a.drawing()
            .set(&[("x", props(json!({"index": "a3"})))])
            .unwrap();
        b.drawing()
            .set(&[("z", props(json!({"index": "Zz"})))])
            .unwrap();
        sync(&a, &b);
        let order: Vec<String> = a
            .drawing()
            .scene()
            .elements
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        assert_eq!(order, ["z", "y", "x"]);
        // Same index from two people: by id.
        a.drawing()
            .set(&[("y", props(json!({"index": "a3"})))])
            .unwrap();
        let order: Vec<String> = a
            .drawing()
            .scene()
            .elements
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        assert_eq!(order, ["z", "x", "y"]);
    }

    #[test]
    fn big_drawings_in_small_updates() {
        let scene = read_drawing(EXAMPLE, &mut Ids::new(1)).unwrap().scene;
        let doc = NoteDoc::new();
        let updates = doc.drawing().set_scene_split(&scene, 8_000);
        assert!(updates.len() > 5, "{}", updates.len());
        // Each fits, but for elements too large on their own.
        let biggest = scene
            .elements
            .iter()
            .map(|e| serde_json::to_string(e.props()).unwrap().len())
            .max()
            .unwrap();
        let sizes: Vec<usize> = updates.iter().map(Vec::len).collect();
        assert!(
            updates.iter().all(|u| u.len() <= 8_000.max(biggest)),
            "{sizes:?} biggest {biggest}"
        );
        let other = NoteDoc::new();
        for u in &updates {
            other.apply_update(u).unwrap();
        }
        assert_eq!(other.drawing().scene(), scene);
        assert!(doc.drawing().set_scene_split(&scene, 8_000).is_empty());
    }

    #[test]
    fn undo_takes_back_own_changes_only() {
        let mut a = NoteDoc::new();
        a.enable_drawing_undo_with_clock(|| 0);
        a.drawing()
            .put(&[el(
                json!({"id": "r", "type": "rectangle", "x": 0, "index": "a0"}),
            )])
            .unwrap();
        a.undo_boundary();
        a.drawing().set(&[("r", props(json!({"x": 50})))]).unwrap();
        a.undo_boundary();
        // Someone else recolours it meanwhile.
        let b = NoteDoc::from_updates([a.encode_state().as_slice()]).unwrap();
        let theirs = b
            .drawing()
            .set(&[("r", props(json!({"strokeColor": "#e03131"})))])
            .unwrap();
        a.apply_update(&theirs).unwrap();
        // Undo: the move is taken back, their colour stays.
        assert!(a.undo().is_some());
        let r = a.drawing().element("r").unwrap();
        assert_eq!((r.x(), r.stroke_color()), (0.0, "#e03131"));
        assert!(a.redo().is_some());
        assert_eq!(a.drawing().element("r").unwrap().x(), 50.0);
    }

    #[test]
    fn put_removes_properties_set_leaves_them() {
        let d = NoteDoc::new();
        d.drawing()
            .put(&[el(
                json!({"id": "a", "type": "text", "text": "hi", "link": "x"}),
            )])
            .unwrap();
        d.drawing()
            .put(&[el(json!({"id": "a", "type": "text", "text": "hi"}))])
            .unwrap();
        assert_eq!(d.drawing().element("a").unwrap().get("link"), None);
        d.drawing()
            .set(&[
                ("a", props(json!({"text": "ho"}))),
                ("nope", props(json!({"x": 1}))),
            ])
            .unwrap();
        assert_eq!(d.drawing().element("a").unwrap().text(), "ho");
        assert!(d.drawing().element("nope").is_none());
        d.drawing()
            .set_settings(&props(json!({"viewBackgroundColor": "#000"})))
            .unwrap();
        assert_eq!(d.drawing().scene().background(), "#000");
    }
}
