//! A drawing as Excalidraw models it: a list of elements, each a flat set of
//! named properties, and a few drawing-wide settings.
//!
//! An element keeps *all* its properties as JSON values, including those
//! this code doesn't know: Excalidraw's format keeps growing, and a drawing
//! that goes through notekpr must come back out with nothing lost. Typed
//! accessors read the properties that drawing and editing need, with
//! Excalidraw's defaults when one is missing or malformed.

use serde_json::{Map, Value};

/// An element's or the drawing's properties, by name.
pub type Props = Map<String, Value>;

/// The `appState` keys Excalidraw saves with a drawing (its
/// `cleanAppStateForExport`): settings of the drawing, not of one viewer.
pub const SAVED_APP_STATE: [&str; 5] = [
    "gridSize",
    "gridStep",
    "gridModeEnabled",
    "viewBackgroundColor",
    "lockedMultiSelections",
];

/// What kind of element it is (`type`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ElementType {
    Rectangle,
    Diamond,
    Ellipse,
    Arrow,
    Line,
    Freedraw,
    Text,
    Image,
    Frame,
    MagicFrame,
    Embeddable,
    Iframe,
    StickyNote,
    Selection,
    /// A kind this code doesn't know (a newer Excalidraw's): kept, not drawn.
    Other(String),
}

impl ElementType {
    pub fn parse(s: &str) -> Self {
        match s {
            "rectangle" => Self::Rectangle,
            "diamond" => Self::Diamond,
            "ellipse" => Self::Ellipse,
            "arrow" => Self::Arrow,
            "line" => Self::Line,
            "freedraw" => Self::Freedraw,
            "text" => Self::Text,
            "image" => Self::Image,
            "frame" => Self::Frame,
            "magicframe" => Self::MagicFrame,
            "embeddable" => Self::Embeddable,
            "iframe" => Self::Iframe,
            "stickynote" => Self::StickyNote,
            "selection" => Self::Selection,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Rectangle => "rectangle",
            Self::Diamond => "diamond",
            Self::Ellipse => "ellipse",
            Self::Arrow => "arrow",
            Self::Line => "line",
            Self::Freedraw => "freedraw",
            Self::Text => "text",
            Self::Image => "image",
            Self::Frame => "frame",
            Self::MagicFrame => "magicframe",
            Self::Embeddable => "embeddable",
            Self::Iframe => "iframe",
            Self::StickyNote => "stickynote",
            Self::Selection => "selection",
            Self::Other(s) => s,
        }
    }

    /// Lines and arrows: shaped by their `points`.
    pub fn is_linear(&self) -> bool {
        matches!(self, Self::Arrow | Self::Line)
    }

    /// Shapes that can hold a text (a label) and have arrows attached.
    pub fn is_bindable(&self) -> bool {
        matches!(
            self,
            Self::Rectangle
                | Self::Diamond
                | Self::Ellipse
                | Self::Image
                | Self::Frame
                | Self::MagicFrame
                | Self::Embeddable
                | Self::Iframe
                | Self::StickyNote
                | Self::Text
        )
    }
}

/// How round an element's corners are (`roundness`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roundness {
    /// 1: legacy, 2: proportional radius, 3: adaptive radius.
    pub kind: u32,
    /// A fixed radius, if set.
    pub value: Option<f64>,
}

/// Excalidraw's `ROUNDNESS` kinds.
pub mod roundness {
    pub const LEGACY: u32 = 1;
    pub const PROPORTIONAL_RADIUS: u32 = 2;
    pub const ADAPTIVE_RADIUS: u32 = 3;
}

/// An arrow's end attached to a shape (`startBinding`, `endBinding`).
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub element_id: String,
    /// Where on the shape, as fractions of its width and height.
    pub fixed_point: Option<[f64; 2]>,
    /// `inside` (pointing into the shape) or `orbit` (stopping at its
    /// outline); older files have `focus` and `gap` instead.
    pub mode: Option<String>,
}

/// One of the elements attached to a shape (`boundElements`): its label or
/// an arrow.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundElement {
    pub id: String,
    /// `text` or `arrow`.
    pub kind: String,
}

/// One element of a drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    props: Props,
}

/// A number as JavaScript writes it to JSON: whole numbers without a
/// fraction, so they read back the same in both.
pub fn num(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        Value::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

fn point(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    let x = a.first()?.as_f64()?;
    let y = a.get(1)?.as_f64()?;
    (x.is_finite() && y.is_finite()).then_some([x, y])
}

impl Element {
    /// An element from its properties; `None` without a string `id` and
    /// `type`.
    pub fn from_props(props: Props) -> Option<Self> {
        props.get("id")?.as_str()?;
        props.get("type")?.as_str()?;
        Some(Self { props })
    }

    pub fn props(&self) -> &Props {
        &self.props
    }

    pub fn into_props(self) -> Props {
        self.props
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.props.get(key)
    }

    /// Sets a property (`Value::Null` keeps it, as `null`).
    pub fn set(&mut self, key: &str, value: Value) {
        self.props.insert(key.to_string(), value);
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.props.remove(key)
    }

    /// A finite number, if the property is one.
    pub fn f64(&self, key: &str) -> Option<f64> {
        self.props
            .get(key)
            .and_then(Value::as_f64)
            .filter(|x| x.is_finite())
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.props.get(key).and_then(Value::as_str)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.props.get(key).and_then(Value::as_bool)
    }

    pub fn id(&self) -> &str {
        self.str("id").unwrap_or_default()
    }

    pub fn kind(&self) -> ElementType {
        ElementType::parse(self.str("type").unwrap_or_default())
    }

    pub fn x(&self) -> f64 {
        self.f64("x").unwrap_or(0.0)
    }

    pub fn y(&self) -> f64 {
        self.f64("y").unwrap_or(0.0)
    }

    pub fn width(&self) -> f64 {
        self.f64("width").unwrap_or(0.0)
    }

    pub fn height(&self) -> f64 {
        self.f64("height").unwrap_or(0.0)
    }

    /// Rotation in radians, clockwise.
    pub fn angle(&self) -> f64 {
        self.f64("angle").unwrap_or(0.0)
    }

    /// The seed of its hand-drawn wobble.
    pub fn seed(&self) -> u32 {
        // Seeds are 31-bit in Excalidraw; anything else wraps like `| 0`.
        self.f64("seed").map_or(1, |s| s as i64 as u32)
    }

    pub fn version(&self) -> u64 {
        self.f64("version").map_or(1, |v| v as u64)
    }

    pub fn is_deleted(&self) -> bool {
        self.bool("isDeleted").unwrap_or(false)
    }

    /// Its place in the stacking order: a fractional index (a string that
    /// sorts between its neighbours').
    pub fn index(&self) -> Option<&str> {
        self.str("index")
    }

    pub fn stroke_color(&self) -> &str {
        self.str("strokeColor").unwrap_or("#1e1e1e")
    }

    pub fn background_color(&self) -> &str {
        self.str("backgroundColor").unwrap_or("transparent")
    }

    /// `hachure`, `cross-hatch`, `solid` or `zigzag`.
    pub fn fill_style(&self) -> &str {
        self.str("fillStyle").unwrap_or("solid")
    }

    pub fn stroke_width(&self) -> f64 {
        self.f64("strokeWidth").unwrap_or(2.0)
    }

    /// `solid`, `dashed` or `dotted`.
    pub fn stroke_style(&self) -> &str {
        self.str("strokeStyle").unwrap_or("solid")
    }

    /// Sloppiness: 0 architect, 1 artist, 2 cartoonist.
    pub fn roughness(&self) -> f64 {
        self.f64("roughness").unwrap_or(1.0)
    }

    /// 0–100.
    pub fn opacity(&self) -> f64 {
        self.f64("opacity").unwrap_or(100.0)
    }

    /// The groups it is in, innermost first.
    pub fn group_ids(&self) -> Vec<&str> {
        self.props
            .get("groupIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    pub fn frame_id(&self) -> Option<&str> {
        self.str("frameId")
    }

    pub fn roundness(&self) -> Option<Roundness> {
        let r = self.props.get("roundness")?.as_object()?;
        Some(Roundness {
            kind: r.get("type")?.as_u64()? as u32,
            value: r.get("value").and_then(Value::as_f64),
        })
    }

    /// The points of a line, arrow or pen stroke, relative to `x`/`y`.
    pub fn points(&self) -> Vec<[f64; 2]> {
        self.props
            .get("points")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(point).collect())
            .unwrap_or_default()
    }

    /// A pen stroke's pressure at each point (empty when simulated).
    pub fn pressures(&self) -> Vec<f64> {
        self.props
            .get("pressures")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|p| p.as_f64().unwrap_or(0.5)).collect())
            .unwrap_or_default()
    }

    pub fn simulate_pressure(&self) -> bool {
        self.bool("simulatePressure").unwrap_or(false)
    }

    pub fn text(&self) -> &str {
        self.str("text").unwrap_or_default()
    }

    pub fn original_text(&self) -> &str {
        self.str("originalText").unwrap_or_else(|| self.text())
    }

    pub fn font_size(&self) -> f64 {
        self.f64("fontSize").unwrap_or(20.0)
    }

    /// Excalidraw's font numbers (`FONT_FAMILY`), see [`crate::fonts`].
    pub fn font_family(&self) -> u32 {
        self.f64("fontFamily").map_or(5, |f| f as u32)
    }

    /// Line height as a multiple of the font size.
    pub fn line_height(&self) -> f64 {
        self.f64("lineHeight").unwrap_or(1.25)
    }

    /// `left`, `center` or `right`.
    pub fn text_align(&self) -> &str {
        self.str("textAlign").unwrap_or("left")
    }

    /// `top`, `middle` or `bottom`.
    pub fn vertical_align(&self) -> &str {
        self.str("verticalAlign").unwrap_or("top")
    }

    /// The shape or arrow a text is the label of.
    pub fn container_id(&self) -> Option<&str> {
        self.str("containerId")
    }

    pub fn bound_elements(&self) -> Vec<BoundElement> {
        self.props
            .get("boundElements")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|b| {
                        Some(BoundElement {
                            id: b.get("id")?.as_str()?.to_string(),
                            kind: b.get("type")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn binding(&self, key: &str) -> Option<Binding> {
        let b = self.props.get(key)?.as_object()?;
        Some(Binding {
            element_id: b.get("elementId")?.as_str()?.to_string(),
            fixed_point: b.get("fixedPoint").and_then(point),
            mode: b.get("mode").and_then(Value::as_str).map(str::to_string),
        })
    }

    pub fn start_binding(&self) -> Option<Binding> {
        self.binding("startBinding")
    }

    pub fn end_binding(&self) -> Option<Binding> {
        self.binding("endBinding")
    }

    /// `arrow`, `bar`, `dot`, `circle`, `triangle`, `diamond`, … or none.
    pub fn start_arrowhead(&self) -> Option<&str> {
        self.str("startArrowhead")
    }

    pub fn end_arrowhead(&self) -> Option<&str> {
        self.str("endArrowhead")
    }

    /// An arrow that goes around shapes at right angles.
    pub fn is_elbowed(&self) -> bool {
        self.bool("elbowed").unwrap_or(false)
    }

    pub fn link(&self) -> Option<&str> {
        self.str("link")
    }

    pub fn is_locked(&self) -> bool {
        self.bool("locked").unwrap_or(false)
    }

    /// An image's picture, in the drawing's files.
    pub fn file_id(&self) -> Option<&str> {
        self.str("fileId")
    }

    /// A frame's name.
    pub fn name(&self) -> Option<&str> {
        self.str("name")
    }
}

/// A whole drawing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// In stacking order, bottom first; deleted ones included (Excalidraw
    /// keeps them so edits merge).
    pub elements: Vec<Element>,
    /// The drawing's settings: the [`SAVED_APP_STATE`] keys.
    pub app_state: Props,
    /// Pictures of image elements, by id: `mimeType`, `id`, `dataURL`,
    /// `created`, `lastRetrieved`.
    pub files: Props,
}

impl Scene {
    pub fn element(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.id() == id)
    }

    /// The elements that are there (not deleted).
    pub fn visible(&self) -> impl Iterator<Item = &Element> {
        self.elements.iter().filter(|e| !e.is_deleted())
    }

    /// The canvas colour (`viewBackgroundColor`).
    pub fn background(&self) -> &str {
        self.app_state
            .get("viewBackgroundColor")
            .and_then(Value::as_str)
            .unwrap_or("#ffffff")
    }

    /// The text of every text element there, in stacking order: what
    /// search finds in a drawing.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.visible()
            .filter(|e| e.kind() == ElementType::Text)
            .map(Element::original_text)
            .filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn el(v: Value) -> Element {
        Element::from_props(v.as_object().unwrap().clone()).unwrap()
    }

    #[test]
    fn needs_id_and_type() {
        assert!(Element::from_props(json!({"id": "a"}).as_object().unwrap().clone()).is_none());
        assert!(
            Element::from_props(json!({"type": "text"}).as_object().unwrap().clone()).is_none()
        );
        assert!(
            Element::from_props(
                json!({"id": 1, "type": "text"})
                    .as_object()
                    .unwrap()
                    .clone()
            )
            .is_none()
        );
    }

    #[test]
    fn typed_reads_with_defaults() {
        let e = el(json!({
            "id": "a", "type": "arrow", "x": 10, "y": -2.5, "points": [[0, 0], [10, "x"], [5, 5]],
            "roundness": {"type": 2}, "seed": 1172987811,
            "startBinding": {"elementId": "b", "mode": "inside", "fixedPoint": [0.5, 1]},
            "boundElements": [{"id": "t", "type": "text"}, {"id": 3}],
            "groupIds": ["g1", "g2"], "frameId": null, "futureThing": {"kept": true}
        }));
        assert_eq!(e.kind(), ElementType::Arrow);
        assert!(e.kind().is_linear());
        assert_eq!((e.x(), e.y(), e.width()), (10.0, -2.5, 0.0));
        assert_eq!(e.points(), vec![[0.0, 0.0], [5.0, 5.0]]);
        assert_eq!(
            e.roundness(),
            Some(Roundness {
                kind: roundness::PROPORTIONAL_RADIUS,
                value: None
            })
        );
        assert_eq!(e.seed(), 1_172_987_811);
        let b = e.start_binding().unwrap();
        assert_eq!(b.element_id, "b");
        assert_eq!(b.fixed_point, Some([0.5, 1.0]));
        assert_eq!(e.end_binding(), None);
        assert_eq!(e.bound_elements().len(), 1);
        assert_eq!(e.group_ids(), vec!["g1", "g2"]);
        assert_eq!(e.frame_id(), None);
        assert_eq!(e.stroke_color(), "#1e1e1e");
        assert_eq!(e.get("futureThing"), Some(&json!({"kept": true})));
        assert_eq!(
            ElementType::parse("hexagon"),
            ElementType::Other("hexagon".into())
        );
        assert_eq!(ElementType::parse("hexagon").as_str(), "hexagon");
    }

    #[test]
    fn numbers_written_like_javascript() {
        assert_eq!(num(3.0).to_string(), "3");
        assert_eq!(num(-0.0).to_string(), "0");
        assert_eq!(num(2.5).to_string(), "2.5");
        assert_eq!(num(f64::NAN), Value::Null);
    }
}
