//! A drawing as an SVG image, as Excalidraw exports it (`export.ts`,
//! `staticSvgScene.ts`; MIT, see `PORTED.md`): the same frame, the same
//! groups and transforms, the same path data at two decimals. Used for
//! previews of drawings in notes, and for exporting them.
//!
//! Fonts aren't fetched: [`SvgOptions::font_faces`] takes the `@font-face`
//! rules to put in the image (an image file needs its fonts inside it); an
//! SVG shown in the page uses the page's fonts.

use std::collections::HashMap;
use std::fmt::Write;

use crate::bounds;
use crate::color;
use crate::edit::frame;
use crate::embed;
use crate::fonts;
use crate::js;
use crate::rough::{self, Drawable};
use crate::scene::{Element, ElementType, Scene};
use crate::shape::{self, ElementShape, RenderConfig};
use crate::sticky;

/// `MAX_DECIMALS_FOR_SVG_EXPORT`.
const DECIMALS: u32 = 2;
/// `DEFAULT_EXPORT_PADDING` (px).
pub const DEFAULT_PADDING: f64 = 10.0;
/// `BOUND_TEXT_PADDING`: the gap cut around an arrow's label.
const BOUND_TEXT_PADDING: f64 = 5.0;
/// `FRAME_STYLE`.
const FRAME_RADIUS: f64 = 8.0;
const FRAME_STROKE: &str = "#bbb";
const FRAME_STROKE_WIDTH: f64 = 2.0;
/// `DARK_THEME_FILTER`: SVG pictures in dark mode.
const DARK_THEME_FILTER: &str = "invert(93%) hue-rotate(180deg)";
const SVG_MIME: &str = "image/svg+xml";

/// How to export.
#[derive(Clone, Debug)]
pub struct SvgOptions {
    /// Excalidraw's dark mode: colours inverted and turned.
    pub dark: bool,
    /// Paint the drawing's canvas colour behind it.
    pub background: bool,
    /// Space around the drawing (px).
    pub padding: f64,
    /// `@font-face` rules to put in the image.
    pub font_faces: Option<String>,
    /// Start with `<?xml …?>` and a doctype, as a file does.
    pub standalone: bool,
    /// The viewer's clock: the dates on sticky notes.
    pub time: sticky::LocalTime,
    /// Measures a line of text in a CSS font: frames' names are cut to
    /// their width.
    pub measure: fn(&str, &str) -> f64,
    /// Put the drawing inside, as Excalidraw's "Embed scene" does: the
    /// image opens again as the drawing, in notekpr and in Excalidraw.
    pub embed_scene: bool,
    /// A frame exported alone (its id; Excalidraw's `exportingFrame`, see
    /// [`crate::export::Part`]): only what overlaps it, all of it cut to the
    /// frame with square corners, the image the frame's size (no padding),
    /// no frame outlined or named.
    pub frame: Option<String>,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            dark: false,
            background: true,
            padding: DEFAULT_PADDING,
            font_faces: None,
            standalone: false,
            time: sticky::LocalTime::default(),
            measure: crate::edit::guess_width,
            embed_scene: false,
            frame: None,
        }
    }
}

fn n(x: f64) -> String {
    js::number_to_string(x)
}

/// Text for XML: `&`, `<`, `>` (and quotes in attributes) escaped.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// A link fit to put in the image (`sanitizeUrl`): web and mail addresses
/// and relative ones; anything else (`javascript:`, `data:`, …) goes nowhere.
pub fn safe_link(link: &str) -> &str {
    let l = link.trim();
    let lower = l.to_ascii_lowercase();
    let scheme = lower
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| !s.contains(['/', '?', '#']));
    match scheme {
        None | Some("http" | "https" | "mailto") => l,
        Some(_) => "about:blank",
    }
}

/// `isRTL`: whether a text has letters written right to left.
fn is_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32,
            0x0591..=0x07FF | 0xFB1D..=0xFDFD | 0xFE70..=0xFEFC)
    })
}

/// A rough shape as `<path>`s at the export's precision.
fn rough_paths(d: &Drawable) -> String {
    let mut d = d.clone();
    d.options.fixed_decimal_place_digits = Some(DECIMALS);
    rough::svg::draw(&d)
}

/// The part of its picture an image shows (`crop`), in the picture's own
/// pixels.
#[derive(Clone, Copy)]
struct Crop {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    natural_width: f64,
    natural_height: f64,
}

impl Crop {
    fn of(e: &Element) -> Option<Self> {
        let c = e.get("crop")?.as_object()?;
        let get = |k: &str| c.get(k).and_then(|v| v.as_f64());
        Some(Self {
            x: get("x")?,
            y: get("y")?,
            width: get("width")?,
            height: get("height")?,
            natural_width: get("naturalWidth")?,
            natural_height: get("naturalHeight")?,
        })
    }
}

/// `hashString`: djb2 over the UTF-16 code units, as an unsigned 32-bit
/// number.
fn hash_string(s: &str) -> u32 {
    s.encode_utf16().fold(5381u32, |h, c| {
        h.wrapping_mul(33).wrapping_add(u32::from(c))
    })
}

/// An element's link, if it has one.
fn link_of(e: &Element) -> Option<&str> {
    e.link().filter(|l| !l.trim().is_empty())
}

struct Renderer<'a> {
    scene: &'a Scene,
    elements: HashMap<&'a str, &'a Element>,
    cfg: RenderConfig,
    out: String,
    /// The frames there: their children take their opacity…
    frames: HashMap<&'a str, &'a Element>,
    /// …and are cut to them (`frameRendering.clip`), and the frames are
    /// outlined (`frameRendering.outline`); neither for a frame alone.
    clip_and_outline: bool,
    time: sticky::LocalTime,
    /// The pictures' `<symbol>`s for `<defs>`, in the order made; each
    /// goes first in it, so they end up last first.
    symbols: Vec<String>,
    /// The ids of the `<symbol>`s there are, in `<defs>` or a link.
    symbol_ids: std::collections::HashSet<String>,
}

impl<'a> Renderer<'a> {
    /// The opacity an element is drawn with (its frame's too).
    fn opacity(&self, e: &Element) -> f64 {
        let frame = e
            .frame_id()
            .and_then(|f| self.frames.get(f))
            .map_or(100.0, |f| f.opacity());
        (frame * e.opacity()) / 10000.0
    }

    fn opacity_attrs(o: f64) -> String {
        if o != 1.0 {
            format!(r#" stroke-opacity="{0}" fill-opacity="{0}""#, n(o))
        } else {
            String::new()
        }
    }

    /// Wraps an element's markup in its frame's clipping and its link.
    fn add(&mut self, e: &Element, markup: String) {
        self.add_after(e, String::new(), markup);
    }

    /// As [`Self::add`], `before` (unclipped) ahead of the markup.
    fn add_after(&mut self, e: &Element, before: String, markup: String) {
        let clipped = match e
            .frame_id()
            .filter(|f| self.clip_and_outline && self.frames.contains_key(f))
        {
            Some(f) => format!(r#"{before}<g clip-path="url(#{})">{markup}</g>"#, esc(f)),
            None => format!("{before}{markup}"),
        };
        match link_of(e) {
            Some(link) => {
                let _ = write!(
                    self.out,
                    r#"<a href="{}">{clipped}</a>"#,
                    esc(safe_link(link))
                );
            }
            None => self.out.push_str(&clipped),
        }
    }

    fn element(&mut self, e: &Element, offset_x: f64, offset_y: f64) {
        let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(e);
        let cx = (x2 - x1) / 2.0 - (e.x() - x1);
        let cy = (y2 - y1) / 2.0 - (e.y() - y1);
        let degree = (180.0 * e.angle()) / std::f64::consts::PI;
        let transform = format!(
            "translate({} {}) rotate({} {} {})",
            n(offset_x),
            n(offset_y),
            n(degree),
            n(cx),
            n(cy)
        );
        let opacity = self.opacity(e);
        let dark = self.cfg.dark;
        match e.kind() {
            ElementType::Rectangle | ElementType::Diamond | ElementType::Ellipse => {
                if let ElementShape::Rough(shapes) = shape::element_shape(e, &self.cfg) {
                    let paths: String = shapes.iter().map(rough_paths).collect();
                    let markup = format!(
                        r#"<g{} stroke-linecap="round" transform="{transform}">{paths}</g>"#,
                        Self::opacity_attrs(opacity)
                    );
                    self.add(e, markup);
                }
            }
            ElementType::Iframe | ElementType::Embeddable => {
                // Only the placeholder: notekpr never loads what's embedded.
                if let ElementShape::Rough(shapes) = shape::element_shape(e, &self.cfg) {
                    let paths: String = shapes.iter().map(rough_paths).collect();
                    let o = e.opacity() / 100.0;
                    let markup = format!(
                        r#"<g{} stroke-linecap="round" transform="{transform}">{paths}</g>"#,
                        Self::opacity_attrs(o)
                    );
                    self.add(e, markup);
                }
            }
            ElementType::Line | ElementType::Arrow => {
                let ElementShape::Rough(shapes) = shape::element_shape(e, &self.cfg) else {
                    return;
                };
                let label = self.bound_text(e);
                let mut mask = String::from("<mask></mask>");
                let mut group = String::from(r#"<g stroke-linecap="round">"#);
                if let Some(t) = label {
                    // A hole in the line where its label is.
                    let w = e.width() + 100.0 + offset_x;
                    let h = e.height() + 100.0 + offset_y;
                    let mx = offset_x + t.x() - e.x() - BOUND_TEXT_PADDING;
                    let my = offset_y + t.y() - e.y() - BOUND_TEXT_PADDING;
                    mask = format!(
                        r##"<mask id="mask-{id}" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{h}"><rect x="0" y="0" fill="#fff" width="{w}" height="{h}"></rect><rect x="{mx}" y="{my}" fill="#000" width="{tw}" height="{th}" opacity="1"></rect></mask>"##,
                        id = esc(e.id()),
                        w = n(w),
                        h = n(h),
                        mx = n(mx),
                        my = n(my),
                        tw = n(t.width() + BOUND_TEXT_PADDING * 2.0),
                        th = n(t.height() + BOUND_TEXT_PADDING * 2.0),
                    );
                    group = format!(
                        r#"<g mask="url(#mask-{})" stroke-linecap="round">"#,
                        esc(e.id())
                    );
                }
                let filled_loop = e.kind() == ElementType::Line
                    && shape::is_path_a_loop(&e.points())
                    && e.background_color() != "transparent";
                for s in &shapes {
                    let _ = write!(
                        group,
                        r#"<g{} transform="{transform}"{}>{}</g>"#,
                        Self::opacity_attrs(opacity),
                        if filled_loop {
                            r#" fill-rule="evenodd""#
                        } else {
                            ""
                        },
                        rough_paths(s)
                    );
                }
                group.push_str("</g>");
                self.add(e, group);
                self.out.push_str(&mask);
            }
            ElementType::Freedraw => {
                let ElementShape::Freedraw { fill, outline } = shape::element_shape(e, &self.cfg)
                else {
                    return;
                };
                let mut inner = String::new();
                if let Some(fill) = fill {
                    inner.push_str(&rough_paths(&fill));
                }
                let _ = write!(
                    inner,
                    r#"<path fill="{}" d="{}"></path>"#,
                    esc(&color::themed(e.stroke_color(), dark)),
                    outline
                );
                let markup = format!(
                    r#"<g{} transform="{transform}" stroke="none">{inner}</g>"#,
                    Self::opacity_attrs(opacity)
                );
                self.add(e, markup);
            }
            ElementType::Text => {
                let text = e.text().replace("\r\n", "\n").replace('\r', "\n");
                let line_height = e.font_size() * e.line_height();
                let h_offset = match e.text_align() {
                    "center" => e.width() / 2.0,
                    "right" => e.width(),
                    _ => 0.0,
                };
                let v_offset = fonts::baseline_offset(e.font_family(), e.font_size(), line_height);
                let direction = if is_rtl(&text) { "rtl" } else { "ltr" };
                let anchor = match e.text_align() {
                    "center" => "middle",
                    "right" => "end",
                    _ if direction == "rtl" => "end",
                    _ => "start",
                };
                let family = esc(&fonts::css_family(e.font_family()));
                let fill = esc(&color::themed(e.stroke_color(), dark));
                let mut markup = format!(
                    r#"<g{} transform="{transform}">"#,
                    Self::opacity_attrs(opacity)
                );
                for (i, line) in text.split('\n').enumerate() {
                    let _ = write!(
                        markup,
                        r#"<text x="{}" y="{}" font-family="{family}" font-size="{}px" fill="{fill}" text-anchor="{anchor}" style="white-space: pre;" direction="{direction}" dominant-baseline="alphabetic">{}</text>"#,
                        n(h_offset),
                        n(i as f64 * line_height + v_offset),
                        n(e.font_size()),
                        esc(line)
                    );
                }
                markup.push_str("</g>");
                self.add(e, markup);
            }
            ElementType::Image => {
                let Some(file) = e.file_id().and_then(|id| self.scene.files.get(id)) else {
                    return;
                };
                let Some(data_url) = file.get("dataURL").and_then(|d| d.as_str()) else {
                    return;
                };
                let file_id = file
                    .get("id")
                    .and_then(|i| i.as_str())
                    .or(e.file_id())
                    .unwrap_or_default();
                let (w, h) = (js::round(e.width()), js::round(e.height()));
                // The picture is a `<symbol>` each image showing it uses;
                // a cropped one is the whole picture, scaled, shifted and
                // masked to the part shown.
                let crop = Crop::of(e);
                let (mut symbol, mut uncropped) = (format!("image-{file_id}"), None);
                if let Some(c) = crop {
                    let uw = e.width() / (c.width / c.natural_width);
                    let uh = e.height() / (c.height / c.natural_height);
                    let size = hash_string(&format!("{}x{}", n(uw), n(uh)));
                    symbol = format!("image-crop-{file_id}-{size}");
                    uncropped = Some((uw, uh));
                }
                let mut before = String::new();
                if self.symbol_ids.insert(symbol.clone()) {
                    let (sw, sh) =
                        uncropped.map_or(("100%".into(), "100%".into()), |(w, h)| (n(w), n(h)));
                    let markup = format!(
                        r#"<symbol id="{}"><image href="{}" preserveAspectRatio="none" width="{sw}" height="{sh}"></image></symbol>"#,
                        esc(&symbol),
                        esc(data_url),
                    );
                    // Excalidraw puts it first in what holds the image: the
                    // link, if it has one, else `<defs>`.
                    if link_of(e).is_some() {
                        before.push_str(&markup);
                    } else {
                        self.symbols.push(markup);
                    }
                }
                let (crop_x, crop_y) = match (crop, uncropped) {
                    (Some(c), Some((uw, uh))) => {
                        (c.x / (c.natural_width / uw), c.y / (c.natural_height / uh))
                    }
                    _ => (0.0, 0.0),
                };
                let (ax, ay) = (cx + crop_x, cy + crop_y);
                let scale = e.get("scale").and_then(|s| s.as_array()).map(|s| {
                    (
                        s.first().and_then(|v| v.as_f64()).unwrap_or(1.0),
                        s.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0),
                    )
                });
                let flip = match scale {
                    Some((sx, sy)) if sx != 1.0 || sy != 1.0 => format!(
                        r#" transform="translate({} {}) scale({} {}) translate({} {})""#,
                        n(ax),
                        n(ay),
                        n(sx),
                        n(sy),
                        n(-ax),
                        n(-ay),
                    ),
                    _ => String::new(),
                };
                let mut g = String::new();
                let svg_picture = file.get("mimeType").and_then(|m| m.as_str()) == Some(SVG_MIME);
                if dark && svg_picture {
                    let _ = write!(g, r#" filter="{DARK_THEME_FILTER}""#);
                }
                if crop.is_some() {
                    let id = esc(&format!("mask-image-crop-{}", e.id()));
                    let _ = write!(
                        before,
                        r##"<mask id="{id}" fill="#fff"><rect x="{}" y="{}" width="{}" height="{}"></rect></mask>"##,
                        n(crop_x),
                        n(crop_y),
                        n(w),
                        n(h),
                    );
                    let _ = write!(g, r#" mask="url(#{id})""#);
                }
                let _ = write!(
                    g,
                    r#" transform="translate({} {}) rotate({} {} {})""#,
                    n(offset_x - crop_x),
                    n(offset_y - crop_y),
                    n(degree),
                    n(ax),
                    n(ay),
                );
                if e.roundness().is_some() {
                    let r = shape::corner_radius(js::min(e.width(), e.height()), e);
                    let id = esc(&format!("image-clipPath-{}", e.id()));
                    let _ = write!(
                        before,
                        r#"<clipPath id="{id}" clipPathUnits="userSpaceOnUse"><rect x="{}" y="{}" width="{}" height="{}" rx="{r}" ry="{r}"></rect></clipPath>"#,
                        n(crop_x),
                        n(crop_y),
                        n(e.width()),
                        n(e.height()),
                        r = n(r),
                    );
                    let _ = write!(g, r#" clip-path="url(#{id})""#);
                }
                let markup = format!(
                    r##"<g{g}><use href="#{}" width="{}" height="{}" opacity="{}"{flip}></use></g>"##,
                    esc(&symbol),
                    n(w + crop_x),
                    n(h + crop_y),
                    n(opacity),
                );
                self.add_after(e, before, markup);
            }
            ElementType::Frame | ElementType::MagicFrame => {
                if !self.clip_and_outline {
                    return;
                }
                let markup = format!(
                    r#"<rect transform="{transform}" width="{}px" height="{}px" rx="{r}" ry="{r}" fill="none" stroke="{}" stroke-width="{}"></rect>"#,
                    n(e.width()),
                    n(e.height()),
                    esc(&color::themed(FRAME_STROKE, dark)),
                    n(FRAME_STROKE_WIDTH),
                    r = n(FRAME_RADIUS),
                );
                self.add(e, markup);
            }
            ElementType::StickyNote => {
                let (clip, note) = sticky::svg(e, &transform, opacity, dark, &self.time);
                self.add_after(e, clip, note);
            }
            // Newer kinds aren't drawn.
            _ => {}
        }
    }

    /// The label of a shape or arrow, if it has one there.
    fn bound_text(&self, e: &Element) -> Option<&'a Element> {
        e.bound_elements()
            .iter()
            .filter(|b| b.kind == "text")
            .find_map(|b| self.elements.get(b.id.as_str()).copied())
            .filter(|t| !t.is_deleted())
    }
}

/// The drawing as an SVG image.
pub fn export(scene: &Scene, opts: &SvgOptions) -> String {
    // A frame exported alone, if it is there.
    let alone = opts
        .frame
        .as_deref()
        .and_then(|id| scene.visible().find(|e| e.id() == id && frame::is_frame(e)));
    let visible: Vec<&Element> = match alone {
        Some(f) => frame::overlapping(scene, f),
        None => scene.visible().collect(),
    };
    let elements: HashMap<&str, &Element> = visible.iter().map(|e| (e.id(), *e)).collect();
    let frames: HashMap<&str, &Element> = visible
        .iter()
        .filter(|e| matches!(e.kind(), ElementType::Frame | ElementType::MagicFrame))
        .map(|e| (e.id(), *e))
        .collect();
    // Frames' names are texts above them (not for a frame alone, which
    // frames the image instead).
    let frame_list: Vec<&Element> = scene.visible().filter(|e| frame::is_frame(e)).collect();
    let names = match alone {
        Some(_) => HashMap::new(),
        None => frame::name_labels(&frame_list, opts.dark, &opts.measure),
    };
    let [min_x, min_y, max_x, max_y] = match alone {
        Some(f) => bounds::element_bounds(f),
        None => frame::export_bounds(scene, names.values()),
    };
    let (width, height) = ((min_x - max_x).abs(), (min_y - max_y).abs());
    let pad = if alone.is_some() { 0.0 } else { opts.padding };
    let (w, h) = (width + pad * 2.0, height + pad * 2.0);
    let (offset_x, offset_y) = (-(min_x - pad), -(min_y - pad));
    let background = scene.background().to_string();

    let mut out = String::new();
    if opts.standalone {
        out.push_str(r#"<?xml version="1.0" standalone="no"?>"#);
        out.push('\n');
        out.push_str(r#"<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">"#);
        out.push('\n');
    }
    let payload = if opts.embed_scene {
        embed::svg_payload(&embed::scene_text(scene))
    } else {
        String::new()
    };
    let _ = write!(
        out,
        r#"<svg version="1.1" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><!-- svg-source:excalidraw --><metadata>{payload}</metadata><defs>"#,
        w = n(w),
        h = n(h)
    );
    // The rest of `<defs>`: the pictures' symbols go ahead of it once the
    // elements are drawn.
    let mut defs = String::new();
    // A frame alone cuts with square corners (all the clip paths do then).
    let radius = match alone {
        Some(_) => String::new(),
        None => format!(r#" rx="{r}" ry="{r}""#, r = n(FRAME_RADIUS)),
    };
    for f in &frame_list {
        let [x1, y1, x2, y2, _, _] = bounds::absolute_coords(f);
        let cx = (x2 - x1) / 2.0 - (f.x() - x1);
        let cy = (y2 - y1) / 2.0 - (f.y() - y1);
        let _ = write!(
            defs,
            r#"<clipPath id="{}"><rect transform="translate({} {}) rotate({} {} {})" width="{}" height="{}"{radius}></rect></clipPath>"#,
            esc(f.id()),
            n(f.x() + offset_x),
            n(f.y() + offset_y),
            n(f.angle()),
            n(cx),
            n(cy),
            n(f.width()),
            n(f.height()),
        );
    }
    let _ = write!(
        defs,
        "<style class=\"style-fonts\">\n      {}</style></defs>",
        opts.font_faces.as_deref().unwrap_or_default()
    );
    let mut body = String::new();
    if opts.background && background != "transparent" {
        let _ = write!(
            body,
            r#"<rect x="0" y="0" width="{w}" height="{h}" fill="{}"></rect>"#,
            esc(&color::themed(&background, opts.dark)),
            w = n(w),
            h = n(h)
        );
    }
    // Everything drawn is cut to a frame exported alone.
    if let Some(f) = alone {
        let _ = write!(body, r#"<g clip-path="url(#{})">"#, esc(f.id()));
    }
    let mut r = Renderer {
        scene,
        elements,
        cfg: RenderConfig {
            dark: opts.dark,
            exporting: true,
            canvas_background: background.clone(),
        },
        out: body,
        frames,
        clip_and_outline: alone.is_none(),
        time: opts.time,
        symbols: Vec::new(),
        symbol_ids: Default::default(),
    };
    let iframe_like =
        |e: &Element| matches!(e.kind(), ElementType::Iframe | ElementType::Embeddable);
    for e in visible.iter().filter(|e| !iframe_like(e)) {
        // A label is drawn with what it labels.
        if e.kind() == ElementType::Text
            && e.container_id().is_some_and(|c| r.elements.contains_key(c))
        {
            continue;
        }
        if let Some(name) = names.get(e.id()) {
            r.element(name, name.x() + offset_x, name.y() + offset_y);
        }
        r.element(e, e.x() + offset_x, e.y() + offset_y);
        if let Some(t) = r.bound_text(e) {
            r.element(t, t.x() + offset_x, t.y() + offset_y);
        }
    }
    for e in visible.iter().filter(|e| iframe_like(e)) {
        r.element(e, e.x() + offset_x, e.y() + offset_y);
    }
    if alone.is_some() {
        r.out.push_str("</g>");
    }
    for symbol in r.symbols.iter().rev() {
        out.push_str(symbol);
    }
    out.push_str(&defs);
    out.push_str(&r.out);
    out.push_str("</svg>");
    out
}

/// Excalidraw's export of the test drawing (`tests/fixtures/example.svg`),
/// as notekpr draws it: the browser that made it computed `Math.atan2`
/// exactly, notekpr uses fdlibm's (`js::math`), which gives one pen
/// stroke's outline a point fewer at one sharp corner — inside the stroke's
/// fill, so the same pixels.
#[cfg(test)]
pub(crate) fn example_export() -> String {
    let svg = include_str!("../tests/fixtures/example.svg");
    let exact = "98.61,5.29 98.39,5.06 98.16,4.82 98.83,3.77 99.50,2.72";
    assert_eq!(svg.matches(exact).count(), 1);
    svg.replace(exact, "98.61,5.29 99.06,4.01 99.50,2.72")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::read_drawing;
    use crate::restore::Ids;

    const DRAWING: &str = include_str!("../tests/fixtures/example.excalidraw");

    /// What follows the fonts: the elements.
    fn body(svg: &str) -> &str {
        &svg[svg.find("</style></defs>").unwrap()..]
    }

    #[test]
    fn same_as_excalidraws_export() {
        let scene = read_drawing(DRAWING, &mut Ids::new(1)).unwrap().scene;
        let ours = export(
            &scene,
            &SvgOptions {
                dark: true,
                standalone: true,
                ..SvgOptions::default()
            },
        );
        let theirs = example_export();
        // The frame.
        let head = |s: &str| s[..s.find("<defs>").unwrap()].to_string();
        assert_eq!(head(&ours), head(&theirs));
        // Every element, group by group, path by path.
        let (a, b) = (body(&ours), body(&theirs));
        if a != b {
            let at = a
                .bytes()
                .zip(b.bytes())
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            let from = at.saturating_sub(300);
            panic!(
                "differs at byte {at}:\nours:  {}\ntheirs: {}",
                &a[from..(at + 200).min(a.len())],
                &b[from..(at + 200).min(b.len())]
            );
        }
    }

    #[test]
    fn links_only_to_safe_places() {
        assert_eq!(safe_link(" https://x.y/z "), "https://x.y/z");
        assert_eq!(safe_link("mailto:a@b.c"), "mailto:a@b.c");
        assert_eq!(safe_link("Notes/Plan.md#x"), "Notes/Plan.md#x");
        assert_eq!(safe_link("JavaScript:alert(1)"), "about:blank");
        assert_eq!(safe_link("data:text/html,x"), "about:blank");
    }

    #[test]
    fn light_and_without_background() {
        let scene = read_drawing(DRAWING, &mut Ids::new(1)).unwrap().scene;
        let light = export(&scene, &SvgOptions::default());
        assert!(light.starts_with("<svg"));
        assert!(light.contains(r##"fill="#ffffff""##));
        assert!(light.contains(r##"stroke="#1e1e1e""##));
        let bare = export(
            &scene,
            &SvgOptions {
                background: false,
                ..SvgOptions::default()
            },
        );
        assert!(!bare.contains(r##"fill="#ffffff""##));
        assert!(export(&Scene::default(), &SvgOptions::default()).ends_with("</svg>"));
    }

    #[test]
    fn the_drawing_inside() {
        let scene = read_drawing(DRAWING, &mut Ids::new(1)).unwrap().scene;
        let plain = export(&scene, &SvgOptions::default());
        assert!(plain.contains("<metadata></metadata>"));
        let svg = export(
            &scene,
            &SvgOptions {
                embed_scene: true,
                ..SvgOptions::default()
            },
        );
        // Only the metadata differs.
        let at = svg.find("<metadata><!-- payload-type:").unwrap();
        assert_eq!(svg[..at], plain[..at]);
        assert_eq!(body(&svg), body(&plain));
        let back = crate::file::read_drawing_bytes(svg.as_bytes(), &mut Ids::new(1)).unwrap();
        let shown: Vec<&Element> = scene.visible().collect();
        assert_eq!(back.scene.elements.iter().collect::<Vec<_>>(), shown);
    }

    #[test]
    fn a_frame_alone() {
        // A frame at 100,100 (200 × 100, half opaque), a box in it, an
        // ellipse across its edge, a box away from it, another frame over
        // it with a box of its own.
        let scene = crate::file::scene_from_json(
            &serde_json::json!({
                "type": "excalidraw",
                "elements": [
                    {"id": "inside", "type": "rectangle", "x": 120, "y": 120, "width": 40,
                     "height": 40, "frameId": "f", "opacity": 80, "strokeColor": "#111111"},
                    {"id": "f", "type": "frame", "x": 100, "y": 100, "width": 200, "height": 100,
                     "name": "Plan", "opacity": 50},
                    {"id": "across", "type": "ellipse", "x": 280, "y": 150, "width": 60,
                     "height": 30, "strokeColor": "#222222"},
                    {"id": "away", "type": "rectangle", "x": 500, "y": 500, "width": 20,
                     "height": 20, "strokeColor": "#333333"},
                    {"id": "g", "type": "frame", "x": 250, "y": 50, "width": 100, "height": 100},
                    {"id": "other", "type": "rectangle", "x": 260, "y": 120, "width": 30,
                     "height": 20, "frameId": "g", "strokeColor": "#444444"},
                ],
            }),
            &mut Ids::new(1),
        )
        .unwrap();
        let opts = SvgOptions {
            frame: Some("f".into()),
            ..SvgOptions::default()
        };
        let svg = export(&scene, &opts);
        // The frame's size, no padding.
        assert!(svg.starts_with(
            r#"<svg version="1.1" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100" width="200" height="100">"#
        ));
        // Every frame's clip path with square corners; the frame's own is
        // where it is in the image.
        assert!(svg.contains(r#"<clipPath id="f"><rect transform="translate(0 0) rotate(0 100 50)" width="200" height="100"></rect></clipPath>"#));
        assert!(svg.contains(r#"<clipPath id="g"><rect transform="translate(150 -50) rotate(0 50 50)" width="100" height="100"></rect></clipPath>"#));
        // The background, then everything in one group cut to the frame
        // (nothing cut to its frame on its own).
        let body = body(&svg);
        assert!(body.starts_with(concat!(
            r##"</style></defs><rect x="0" y="0" width="200" height="100" fill="#ffffff"></rect>"##,
            r#"<g clip-path="url(#f)"><g stroke-opacity="0.4" fill-opacity="0.4" stroke-linecap="round" transform="translate(20 20) rotate(0 20 20)">"#,
        )));
        assert!(body.ends_with("</g></svg>"));
        assert_eq!(body.matches("clip-path=").count(), 1);
        // What overlaps it is drawn; not what is away or in the other frame,
        // nor the frames' outlines or names.
        assert!(body.contains("#111111") && body.contains("#222222"));
        assert!(!body.contains("#333333") && !body.contains("#444444"));
        assert!(!body.contains(FRAME_STROKE) && !body.contains("Plan"));
        // Not there: the whole drawing.
        let missing = export(
            &scene,
            &SvgOptions {
                frame: Some("nothing".into()),
                ..SvgOptions::default()
            },
        );
        assert_eq!(missing, export(&scene, &SvgOptions::default()));
        // What is inside the image: what it was given.
        let part = crate::export::Part::selected(&scene, &["f".to_string()]);
        let svg = export(
            &part.scene,
            &SvgOptions {
                embed_scene: true,
                ..opts
            },
        );
        let back = crate::file::read_drawing_bytes(svg.as_bytes(), &mut Ids::new(1)).unwrap();
        let ids: Vec<&str> = back.scene.elements.iter().map(Element::id).collect();
        assert_eq!(ids, ["inside", "f", "across", "g"]);
    }

    #[test]
    fn pictures_cropped_flipped_and_shared() {
        let scene = crate::file::scene_from_json(
            &serde_json::json!({
                "type": "excalidraw",
                "elements": [
                    // The middle of a 400×200 picture, mirrored, round.
                    {"id": "a", "type": "image", "x": 10, "y": 20, "width": 100, "height": 50,
                     "fileId": "pic", "status": "saved", "scale": [-1, 1],
                     "roundness": {"type": 3},
                     "crop": {"x": 40, "y": 10, "width": 200, "height": 100,
                              "naturalWidth": 400, "naturalHeight": 200}},
                    // The same picture, whole.
                    {"id": "b", "type": "image", "x": 200, "y": 20, "width": 60, "height": 30,
                     "fileId": "pic", "status": "saved"},
                    // Another, with a link: its symbol goes in the link.
                    {"id": "c", "type": "image", "x": 300, "y": 20, "width": 10, "height": 10,
                     "fileId": "svg", "status": "saved", "link": "https://x.y"},
                ],
                "files": {
                    "pic": {"id": "pic", "mimeType": "image/png", "dataURL": "data:image/png;base64,AAAA"},
                    "svg": {"id": "svg", "mimeType": "image/svg+xml", "dataURL": "data:image/svg+xml;base64,BBBB"},
                },
            }),
            &mut Ids::new(1),
        )
        .unwrap();
        let svg = export(&scene, &SvgOptions::default());
        // The symbols made last come first.
        let defs = &svg[svg.find("<defs>").unwrap()..svg.find("<style").unwrap()];
        assert_eq!(
            defs,
            concat!(
                r#"<defs><symbol id="image-pic"><image href="data:image/png;base64,AAAA" preserveAspectRatio="none" width="100%" height="100%"></image></symbol>"#,
                // djb2("200x100"): the size of the whole picture.
                r#"<symbol id="image-crop-pic-2151568832"><image href="data:image/png;base64,AAAA" preserveAspectRatio="none" width="200" height="100"></image></symbol>"#,
            )
        );
        // The crop's offset in the picture as drawn: 40 px of 400 is 20 of
        // 200, 10 of 200 is 5 of 100.
        assert_eq!(
            body(&svg),
            concat!(
                r##"</style></defs><rect x="0" y="0" width="320" height="70" fill="#ffffff"></rect>"##,
                r##"<mask id="mask-image-crop-a" fill="#fff"><rect x="20" y="5" width="100" height="50"></rect></mask>"##,
                r#"<clipPath id="image-clipPath-a" clipPathUnits="userSpaceOnUse"><rect x="20" y="5" width="100" height="50" rx="12.5" ry="12.5"></rect></clipPath>"#,
                r#"<g mask="url(#mask-image-crop-a)" transform="translate(-10 5) rotate(0 70 30)" clip-path="url(#image-clipPath-a)">"#,
                r##"<use href="#image-crop-pic-2151568832" width="120" height="55" opacity="1" transform="translate(70 30) scale(-1 1) translate(-70 -30)"></use></g>"##,
                r##"<g transform="translate(200 10) rotate(0 30 15)"><use href="#image-pic" width="60" height="30" opacity="1"></use></g>"##,
                r#"<a href="https://x.y"><symbol id="image-svg"><image href="data:image/svg+xml;base64,BBBB" preserveAspectRatio="none" width="100%" height="100%"></image></symbol>"#,
                r##"<g transform="translate(300 10) rotate(0 5 5)"><use href="#image-svg" width="10" height="10" opacity="1"></use></g></a>"##,
                "</svg>",
            )
        );
        // In dark mode SVG pictures are inverted, as the canvas shows them.
        let dark = export(
            &scene,
            &SvgOptions {
                dark: true,
                ..SvgOptions::default()
            },
        );
        assert!(dark.contains(
            r#"<g filter="invert(93%) hue-rotate(180deg)" transform="translate(300 10)"#
        ));
        assert_eq!(dark.matches("filter=").count(), 1);
    }
}
