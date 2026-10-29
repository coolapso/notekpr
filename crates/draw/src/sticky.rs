//! Sticky notes, as Excalidraw has them (`stickyNote.ts`, the `STICKY_NOTE_*`
//! constants, how `renderElement.ts` and `staticSvgScene.ts` draw them; MIT,
//! see `PORTED.md`): a paper with a soft shadow, its corners a little
//! wobbly (one corner lifted when a cartoonist draws it), the day it was
//! made in its footer, and a label whose font shrinks to fit the note
//! before the note grows.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::color;
use crate::edit::label;
use crate::edit::new::Measure;
use crate::edit::transform::{self, Frame, Handle, ResizeMode};
use crate::fonts;
use crate::geom::{self, Point};
use crate::js;
use crate::scene::{Element, ElementType, Scene, num};

/// A note's paper unless picked otherwise (`DEFAULT_STICKY_NOTE_BG`).
pub const BACKGROUND: &str = color::palette::STICKY_NOTE;
/// Its ink (text and date) unless picked otherwise.
pub const INK: &str = color::palette::BLACK;
/// The smallest font a label shrinks to before the note grows.
pub const MIN_FONT_SIZE: f64 = 16.0;
pub const MAX_FONT_SIZE: f64 = 512.0;
/// A label's font when its size is no number.
pub const FALLBACK_FONT_SIZE: f64 = 28.0;
/// The steps a label's font shrinks by.
pub const FONT_STEP: f64 = 2.0;
/// `MIN_FONT_SIZE` of any text.
const TEXT_MIN_FONT_SIZE: f64 = 1.0;
/// Around the label.
pub const PADDING: f64 = 16.0;
/// The date's row under the label (`STICKY_NOTE_FOOTER`): its height, font,
/// where its baseline is, and how wide the room must be for the year.
pub const FOOTER_HEIGHT: f64 = 20.0;
pub const FOOTER_FONT_SIZE: f64 = 12.0;
pub const FOOTER_FONT: &str = "Helvetica, Arial, sans-serif";
pub const FOOTER_BASELINE: f64 = 14.0;
pub const FOOTER_OPACITY: f64 = 1.0;
const FOOTER_YEAR_WIDTH: f64 = 80.0;
/// A note's height less its label's room: the padding and the footer.
pub const BODY_INSET_Y: f64 = PADDING * 2.0 + FOOTER_HEIGHT;
/// A note placed with a click.
pub const DEFAULT_SIZE: f64 = 250.0;
/// The smallest a note gets.
pub const MIN_SIZE: f64 = 75.0;
pub const SHADOW_OFFSET: f64 = 3.0;
pub const SHADOW_OPACITY: f64 = 0.16;
pub const EDGE_SHADOW_WIDTH: f64 = 0.5;
pub const EDGE_SHADOW_OPACITY: f64 = 0.08;
/// How far the corners wobble, by sloppiness.
const RENDER_ROUGHNESS: [f64; 3] = [0.0, 1.5, 8.0];
const CORNER_RADIUS_RATIO: f64 = 0.04;
const MAX_CORNER_RADIUS: f64 = 16.0;
/// Under this many screen pixels, drawing a note is a click
/// (`DRAGGING_THRESHOLD`).
const DRAGGING_THRESHOLD: f64 = 10.0;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn is_note(e: &Element) -> bool {
    e.kind() == ElementType::StickyNote
}

/// `seededRandom` (mulberry32): the same numbers for the same seed.
struct SeededRandom(u32);

impl SeededRandom {
    fn new(seed: f64) -> Self {
        // `seed >>> 0`.
        Self(js::to_int32(seed) as u32)
    }

    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

fn seed(e: &Element) -> f64 {
    e.f64("seed").unwrap_or(1.0)
}

/// One step of a note's outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    Move(Point),
    Line(Point),
    Quadratic { control: Point, to: Point },
}

impl PathCommand {
    fn end(&self) -> Point {
        match *self {
            PathCommand::Move(p) | PathCommand::Line(p) => p,
            PathCommand::Quadratic { to, .. } => to,
        }
    }
}

/// `getStickyNoteCornerRadius`.
pub fn corner_radius(e: &Element) -> f64 {
    if e.roundness().is_none() {
        return 0.0;
    }
    js::min(
        js::min(e.width(), e.height()) * CORNER_RADIUS_RATIO,
        MAX_CORNER_RADIUS,
    )
}

/// `getStickyNoteRenderPoints`: the corners, wobbled by the note's seed.
fn render_points(e: &Element, offset: f64, seed_offset: f64) -> [Point; 4] {
    let (w, h) = (e.width(), e.height());
    let roughness = js::max(0.0, js::min(2.0, js::round(e.roughness())));
    let amount = js::min(RENDER_ROUGHNESS[roughness as usize], js::min(w, h) * 0.012);
    let corners = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
    if amount == 0.0 || amount.is_nan() {
        return corners.map(|[x, y]| [offset + x, offset + y]);
    }
    let mut random = SeededRandom::new(seed(e) + seed_offset);
    let mut jitter = || (random.next() * 2.0 - 1.0) * amount;
    corners.map(|[x, y]| {
        let jx = jitter();
        let jy = jitter();
        [offset + x + jx, offset + y + jy]
    })
}

/// `pointAtDistance`: `distance` along from `from` towards `to`.
fn at_distance(from: Point, to: Point, distance: f64) -> Point {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let length = js::hypot(dx, dy);
    if length == 0.0 || length.is_nan() {
        return from;
    }
    let ratio = js::min(distance / length, 1.0);
    [from[0] + dx * ratio, from[1] + dy * ratio]
}

/// `getStickyNotePathCommands`: the outline of the paper, or of its shadow
/// (a little down and right), in the note's own coordinates.
pub fn path_commands(e: &Element, shadow: bool) -> Vec<PathCommand> {
    let points = if shadow {
        render_points(e, SHADOW_OFFSET, 1.0)
    } else {
        render_points(e, 0.0, 0.0)
    };
    let radius = corner_radius(e);
    // The seed keeps the same corner lifted through redraws and resizes.
    let lifted = (e.roughness() == 2.0)
        .then(|| (SeededRandom::new(seed(e)).next() * points.len() as f64).floor() as usize);
    if (radius == 0.0 || radius.is_nan()) && lifted.is_none() {
        let mut out = vec![PathCommand::Move(points[0])];
        out.extend(points[1..].iter().map(|p| PathCommand::Line(*p)));
        return out;
    }
    let n = points.len();
    let corners: Vec<Vec<PathCommand>> = (0..n)
        .map(|i| {
            let point = points[i];
            let prev = points[(i + n - 1) % n];
            let next = points[(i + 1) % n];
            let r = js::min(
                js::min(radius, geom::distance(prev, point) / 2.0),
                geom::distance(next, point) / 2.0,
            );
            if Some(i) == lifted {
                let size = js::min(e.width(), e.height());
                let reach = js::min(size * 0.18, 40.0);
                // The shadow follows the paper at half the bend.
                let lift = js::min(size * 0.02, 5.0) * if shadow { 0.5 } else { 1.0 };
                let tip = [
                    point[0] + if i == 1 || i == 2 { -lift } else { lift },
                    point[1] + if i >= 2 { -lift } else { lift },
                ];
                let start = at_distance(point, prev, reach);
                let end = at_distance(point, next, reach);
                return vec![
                    PathCommand::Line(start),
                    PathCommand::Quadratic {
                        control: at_distance(point, prev, reach / 2.0),
                        to: at_distance(tip, start, r),
                    },
                    PathCommand::Quadratic {
                        control: tip,
                        to: at_distance(tip, end, r),
                    },
                    PathCommand::Quadratic {
                        control: at_distance(point, next, reach / 2.0),
                        to: end,
                    },
                ];
            }
            vec![
                PathCommand::Line(at_distance(point, prev, r)),
                PathCommand::Quadratic {
                    control: point,
                    to: at_distance(point, next, r),
                },
            ]
        })
        .collect();
    let start = corners[0].last().map_or(points[0], PathCommand::end);
    let mut out = vec![PathCommand::Move(start)];
    for i in 1..=n {
        out.extend(corners[i % n].iter().copied());
    }
    out
}

/// The outline as SVG path data, as Excalidraw writes it.
pub fn path_data(commands: &[PathCommand]) -> String {
    let n = js::number_to_string;
    let mut parts: Vec<String> = commands
        .iter()
        .map(|c| match *c {
            PathCommand::Move([x, y]) => format!("M {} {}", n(x), n(y)),
            PathCommand::Line([x, y]) => format!("L {} {}", n(x), n(y)),
            PathCommand::Quadratic {
                control: [cx, cy],
                to: [x, y],
            } => format!("Q {} {} {} {}", n(cx), n(cy), n(x), n(y)),
        })
        .collect();
    parts.push("Z".into());
    parts.join(" ")
}

/// The viewer's clock, for the dates on notes: now (milliseconds since
/// 1970), and how many minutes their time zone is ahead of UTC at a given
/// moment (daylight saving time changes it).
#[derive(Clone, Copy, Debug)]
pub struct LocalTime {
    pub now: f64,
    pub utc_offset: fn(f64) -> f64,
}

impl Default for LocalTime {
    /// UTC, at no particular time (dates show their year).
    fn default() -> Self {
        Self {
            now: 0.0,
            utc_offset: |_| 0.0,
        }
    }
}

impl LocalTime {
    /// The calendar day of a moment, there: year, month (0–11), day.
    fn day(&self, ms: f64) -> (i64, usize, i64) {
        let days = ((ms + (self.utc_offset)(ms) * 60_000.0) / 86_400_000.0).floor() as i64;
        // Howard Hinnant's `civil_from_days`.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        (year, (month - 1) as usize, day)
    }
}

/// `getStickyNoteDateLabel`: "7 Sep", with the year when it isn't this one
/// (unless `short`); `None` for no date.
pub fn date_label(created: Option<f64>, short: bool, time: &LocalTime) -> Option<String> {
    // Beyond what a JavaScript `Date` holds.
    let created = created.filter(|c| c.is_finite() && c.abs() <= 8.64e15)?;
    let (year, month, day) = time.day(created);
    let label = format!("{day} {}", MONTHS[month]);
    if short || year == time.day(time.now).0 {
        Some(label)
    } else {
        Some(format!("{label} {year}"))
    }
}

/// The date in a note's footer: what, and where its right end's baseline
/// is in the note.
#[derive(Clone, Debug, PartialEq)]
pub struct Footer {
    pub text: String,
    pub x: f64,
    pub y: f64,
}

/// `getStickyNoteFooter`: none on a note smaller than any note is (one
/// being drawn), or one without a date.
pub fn footer(e: &Element, time: &LocalTime) -> Option<Footer> {
    let (w, h) = (e.width(), e.height());
    if w < MIN_SIZE || h < MIN_SIZE {
        return None;
    }
    let text = date_label(
        e.f64("created"),
        w - PADDING * 2.0 < FOOTER_YEAR_WIDTH,
        time,
    )?;
    Some(Footer {
        text,
        x: w - PADDING,
        y: h - FOOTER_BASELINE,
    })
}

/// Text for XML.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A note in an SVG image, as `staticSvgScene.ts` draws it: the clip path
/// its edge's shadow needs, then the note (moved and turned by
/// `transform`).
pub fn svg(
    e: &Element,
    transform: &str,
    opacity: f64,
    dark: bool,
    time: &LocalTime,
) -> (String, String) {
    let n = js::number_to_string;
    let paper = path_data(&path_commands(e, false));
    let clip_id = format!("sticky-note-clipPath-{}", esc(e.id()));
    let clip = format!(
        r##"<clipPath id="{clip_id}" clipPathUnits="userSpaceOnUse"><path d="{paper}" fill="#000" stroke="none"></path></clipPath>"##
    );
    let mut group = format!(r#"<g transform="{transform}""#);
    if opacity != 1.0 {
        group.push_str(&format!(r#" opacity="{}""#, n(opacity)));
    }
    group.push_str(&format!(
        r##"><path d="{}" fill="#000" fill-opacity="{}" stroke="none"></path><path d="{paper}" fill="{}" stroke="none"></path><path d="{paper}" fill="none" stroke="#000" stroke-opacity="{}" stroke-width="{}" clip-path="url(#{clip_id})"></path>"##,
        path_data(&path_commands(e, true)),
        n(SHADOW_OPACITY),
        esc(&color::themed(e.background_color(), dark)),
        n(EDGE_SHADOW_OPACITY),
        n(EDGE_SHADOW_WIDTH * 2.0),
    ));
    if let Some(f) = footer(e, time) {
        group.push_str(&format!(
            r#"<text x="{}" y="{}" font-family="{FOOTER_FONT}" font-size="{}px" text-anchor="end" direction="ltr" fill="{}" fill-opacity="{}">{}</text>"#,
            n(f.x),
            n(f.y),
            n(FOOTER_FONT_SIZE),
            esc(&color::themed(e.stroke_color(), dark)),
            n(FOOTER_OPACITY),
            esc(&f.text),
        ));
    }
    group.push_str("</g>");
    (clip, group)
}

/// `normalizeStickyNoteFontSize`: a label's font ceiling, a number in
/// range.
pub fn normalize_font_size(size: f64) -> f64 {
    if !size.is_finite() {
        return FALLBACK_FONT_SIZE;
    }
    js::min(MAX_FONT_SIZE, js::max(TEXT_MIN_FONT_SIZE, size))
}

/// The font size a label was given (its ceiling: `baseFontSize`), which
/// the size it is shown in shrinks from.
pub fn base_font_size(label: &Element) -> f64 {
    label
        .f64("baseFontSize")
        .unwrap_or_else(|| label.font_size())
}

/// `getStickyNoteMinSize`: the smallest note one line in `font_size` fits
/// in, with its padding and footer.
pub fn min_size(font_size: f64, font_family: u32) -> (f64, f64) {
    let line = (normalize_font_size(font_size) * fonts::get(font_family).line_height).ceil();
    (
        js::max(MIN_SIZE, line + PADDING * 2.0),
        js::max(MIN_SIZE, line + BODY_INSET_Y),
    )
}

/// The edge that stays put when a note's height changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    #[default]
    Top,
    Center,
    Bottom,
}

/// What a layout is asked for (`StickyNoteLayoutOpts`); `None` keeps what
/// the note and its label have.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Intent {
    /// The height the note has when its text needs no more.
    pub base_height: Option<f64>,
    /// The label's font ceiling.
    pub base_font_size: Option<f64>,
    pub anchor: Anchor,
}

/// `container.baseHeight || container.height`.
fn base_height(note: &Element) -> f64 {
    note.f64("baseHeight")
        .filter(|h| *h != 0.0)
        .unwrap_or_else(|| note.height())
}

/// `getPositionAfterHeightChange`: where a (turned) note goes when its
/// height becomes `height`, `anchor` staying put.
fn moved_for_height(note: &Element, height: f64, anchor: Anchor) -> Point {
    let delta = (note.height() - height) / 2.0;
    let (x, y) = (note.x(), note.y());
    match anchor {
        // Turning is about the centre: holding it is the same at any angle.
        Anchor::Center => [x, y + delta],
        Anchor::Bottom => [
            x - delta * js::sin(note.angle()),
            y + delta * (1.0 + js::cos(note.angle())),
        ],
        Anchor::Top => [
            x + delta * js::sin(note.angle()),
            y + delta * (1.0 - js::cos(note.angle())),
        ],
    }
}

/// `computeBoundTextPosition` in a note: a middle-aligned label centred in
/// the whole padded note while it clears the footer, pushed up against it
/// once it wouldn't; turned with the note about the note's centre.
pub fn label_position(note: &Element, label: &Element) -> Point {
    let (w, h) = (note.width(), note.height());
    let (tw, th) = (label.width(), label.height());
    let [cx, cy] = [note.x() + PADDING, note.y() + PADDING];
    let max_w = w - PADDING * 2.0;
    let max_h = js::max(0.0, h - BODY_INSET_Y);
    let y = match label.vertical_align() {
        "top" => cy,
        "bottom" => cy + (max_h - th),
        _ => cy + js::min((h - PADDING * 2.0 - th) / 2.0, max_h - th),
    };
    let x = match label.text_align() {
        "left" => cx,
        "right" => cx + (max_w - tw),
        _ => cx + (max_w / 2.0 - tw / 2.0),
    };
    let angle = note.angle();
    if angle == 0.0 {
        return [x, y];
    }
    let center = [note.x() + w / 2.0, note.y() + h / 2.0];
    let [rx, ry] = geom::rotate([x + tw / 2.0, y + th / 2.0], center, angle);
    [rx - tw / 2.0, ry - th / 2.0]
}

/// A wrapped text in one font size, measured.
#[derive(Clone, Debug)]
struct Fit {
    text: String,
    size: f64,
    width: f64,
    height: f64,
}

/// Wraps and measures `text` in `size` (as `label::layout` measures).
fn fit_text(
    text: &str,
    size: f64,
    family: u32,
    line_height: f64,
    max_width: f64,
    measure: Measure,
) -> Fit {
    let font = format!(
        "{}px {}",
        js::number_to_string(size),
        fonts::css_family(family)
    );
    let wrapped = label::wrap(text, &font, max_width, measure);
    let lines: Vec<&str> = wrapped.split('\n').collect();
    let width = lines.iter().map(|l| measure(&font, l)).fold(0.0, f64::max);
    Fit {
        height: lines.len() as f64 * size * line_height,
        text: wrapped,
        size,
        width,
    }
}

/// `fitStickyNoteFont`: the largest size on the grid `ceiling − k·step`
/// (and the smallest size) whose text fits; the smallest when none does.
/// Starts from the size it had (a keystroke takes a pass or two).
fn fit_font(
    fit: &dyn Fn(f64) -> Fit,
    base: f64,
    min: f64,
    max_width: f64,
    max_height: f64,
    warm_start: f64,
) -> Fit {
    let steps = js::max(0.0, ((base - min) / FONT_STEP).ceil()) as usize;
    let size_at = |i: usize| {
        if i >= steps {
            min
        } else {
            base - i as f64 * FONT_STEP
        }
    };
    let mut fits: HashMap<usize, Fit> = HashMap::new();
    let mut at = |i: usize| -> Fit { fits.entry(i).or_insert_with(|| fit(size_at(i))).clone() };
    let fits_in = |f: Fit| f.width <= max_width && f.height <= max_height;
    if steps == 0 {
        return at(0);
    }
    // The size it had, on the grid and in range.
    let warm = js::round((base - warm_start) / FONT_STEP).clamp(0.0, steps as f64) as usize;
    let (mut lo, mut hi);
    if fits_in(at(warm)) {
        if warm == 0 || !fits_in(at(warm - 1)) {
            return at(warm);
        }
        lo = 0;
        hi = warm - 1;
    } else {
        if warm == steps {
            return at(steps);
        }
        lo = warm + 1;
        hi = steps;
    }
    // The smallest step (largest font) that fits.
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if fits_in(at(mid)) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    at(lo)
}

/// `getStickyNoteLayout`: a note and its label laid out together — the
/// text wrapped to the note's width, its font fitted under its ceiling, the
/// note grown past its base height only when the text still overflows at
/// the smallest font, the label placed inside. Returns both, changed.
pub fn layout(
    note: &Element,
    label: Option<&Element>,
    intent: &Intent,
    measure: Measure,
) -> (Element, Option<Element>) {
    let base_width = js::max(note.width(), MIN_SIZE);
    let base_height = js::max(
        intent.base_height.unwrap_or_else(|| base_height(note)),
        MIN_SIZE,
    );
    let mut next = note.clone();
    let place = |next: &mut Element, height: f64| {
        let [x, y] = moved_for_height(note, height, intent.anchor);
        next.set("x", num(x));
        next.set("y", num(y));
        next.set("width", num(base_width));
        next.set("height", num(height));
        next.set("baseHeight", num(base_height));
    };
    let Some(label) = label else {
        // An empty note is its base height.
        place(&mut next, base_height);
        return (next, None);
    };
    let original = label.original_text().to_string();
    let base_font = normalize_font_size(
        intent
            .base_font_size
            .unwrap_or_else(|| base_font_size(label)),
    );
    let min_font = js::min(MIN_FONT_SIZE, base_font);
    let max_width = js::max(base_width - PADDING * 2.0, 1.0);
    let max_height = js::max(base_height - BODY_INSET_Y, 0.0);
    let (family, line_height) = (label.font_family(), label.line_height());
    let fit = |size: f64| fit_text(&original, size, family, line_height, max_width, measure);
    let blank = original.trim().is_empty();
    let fitted = if blank {
        fit_text("", base_font, family, line_height, max_width, measure)
    } else {
        fit_font(
            &fit,
            base_font,
            min_font,
            max_width,
            max_height,
            label.font_size(),
        )
    };
    let height = if blank {
        base_height
    } else {
        js::max(base_height, fitted.height + BODY_INSET_Y)
    };
    place(&mut next, height);
    let mut t = label.clone();
    t.set("text", json!(fitted.text));
    t.set("fontSize", num(fitted.size));
    t.set("baseFontSize", num(base_font));
    t.set("width", num(fitted.width));
    t.set("height", num(fitted.height));
    let [x, y] = label_position(&next, &t);
    t.set("x", num(x));
    t.set("y", num(y));
    t.set("angle", num(note.angle()));
    (next, Some(t))
}

/// A note's label laid out (`label::layout` for notes): the label, and the
/// note if it changed.
pub fn layout_label(
    note: &Element,
    label: &Element,
    measure: Measure,
) -> (Element, Option<Element>) {
    let (n, t) = layout(note, Some(label), &Intent::default(), measure);
    let t = t.expect("laid out with its label");
    (t, (n != *note).then_some(n))
}

/// The note's label, if it has one there.
pub fn label_of<'a>(scene: &'a Scene, note: &Element) -> Option<&'a Element> {
    note.bound_elements()
        .iter()
        .filter(|b| b.kind == "text")
        .find_map(|b| scene.element(&b.id))
        .filter(|t| !t.is_deleted())
}

/// Whether a text is a note's label.
pub fn is_label(scene: &Scene, t: &Element) -> bool {
    t.container_id()
        .and_then(|c| scene.element(c))
        .is_some_and(is_note)
}

/// A note drawn with the mouse, finished (pointer up): a click places one
/// of the default size centred on it; a drag keeps its size, at least the
/// smallest note a line in `font` fits in, square when drawn square, its
/// far edge where the drag started.
pub fn finish_drawn(note: &Element, origin: Point, zoom: f64, font: (f64, u32)) -> Element {
    let mut e = note.clone();
    let (w, h) = (note.width(), note.height());
    let (x, y, w, h) = if w * zoom < DRAGGING_THRESHOLD && h * zoom < DRAGGING_THRESHOLD {
        let s = DEFAULT_SIZE;
        (origin[0] - s / 2.0, origin[1] - s / 2.0, s, s)
    } else {
        let (min_w, min_h) = min_size(font.0, font.1);
        let square = w == h;
        let (mut nw, mut nh) = (js::max(w, min_w), js::max(h, min_h));
        if square {
            nw = js::max(nw, nh);
            nh = nw;
        }
        (
            if note.x() < origin[0] {
                origin[0] - nw
            } else {
                note.x()
            },
            if note.y() < origin[1] {
                origin[1] - nh
            } else {
                note.y()
            },
            nw,
            nh,
        )
    };
    e.set("x", num(x));
    e.set("y", num(y));
    e.set("width", num(w));
    e.set("height", num(h));
    e.set("baseHeight", num(h));
    e
}

/// Whether resizing `shapes` by `handle` keeps their proportions unless
/// Shift is held: a lone note's corners do (its label's font scales with
/// it).
pub fn proportional_by_default(shapes: &[Element], handle: Handle) -> bool {
    matches!(shapes, [one] if is_note(one))
        && matches!(handle, Handle::NE | Handle::NW | Handle::SE | Handle::SW)
}

/// `getStickyNoteResizeIntent`: what a resize asks of a note's layout,
/// from the note as the gesture began (so letting go of Shift mid-way
/// gives back the base height and font): a height-changing resize makes
/// the asked height the base, a proportional one scales the font ceiling
/// too, and the content's growth goes from the edge held still.
pub fn resize_intent(
    resized: &Element,
    original: &Element,
    original_label: Option<&Element>,
    handle: Handle,
    mode: ResizeMode,
) -> Intent {
    let ceiling = original_label.map(base_font_size);
    let changes_height = mode.keep_ratio
        || matches!(
            handle,
            Handle::N | Handle::S | Handle::NE | Handle::NW | Handle::SE | Handle::SW
        );
    let scale = if mode.keep_ratio && original.width() != 0.0 {
        resized.width() / original.width()
    } else {
        1.0
    };
    Intent {
        base_height: Some(if changes_height {
            resized.height()
        } else {
            base_height(original)
        }),
        base_font_size: ceiling.map(|c| c * scale),
        anchor: if mode.from_center {
            Anchor::Center
        } else if matches!(handle, Handle::N | Handle::NE | Handle::NW) {
            Anchor::Bottom
        } else if mode.keep_ratio && matches!(handle, Handle::E | Handle::W) {
            // Shift on a side holds the other side's middle.
            Anchor::Center
        } else {
            Anchor::Top
        },
    }
}

/// `getResizedOrigin`: where a box turned by `angle` goes when it becomes
/// `w` × `h` (signed: flipped past its other side) by `handle`, the side or
/// corner across from it held still.
fn resized_origin(original: &Element, w: f64, h: f64, handle: Handle, mode: ResizeMode) -> Point {
    let [x, y] = [original.x(), original.y()];
    let (pw, ph) = (original.width(), original.height());
    let (cos, sin) = (js::cos(original.angle()), js::sin(original.angle()));
    if mode.from_center {
        return [x - (w - pw) / 2.0, y - (h - ph) / 2.0];
    }
    use Handle::*;
    match (handle, mode.keep_ratio) {
        (N, true) => [
            x + (pw - w) / 2.0 + ((h - ph) / 2.0) * sin,
            y + ((ph - h) / 2.0) * (cos + 1.0),
        ],
        (E, true) => [
            x + ((pw - w) / 2.0) * (1.0 - cos),
            y + ((w - pw) / 2.0) * sin + (ph - h) / 2.0,
        ],
        (S, true) => [
            x + (pw - w) / 2.0 + ((ph - h) / 2.0) * sin,
            y + ((h - ph) / 2.0) * (cos - 1.0),
        ],
        (W, true) => [
            x + ((pw - w) / 2.0) * (cos + 1.0),
            y + ((pw - w) / 2.0) * sin + (ph - h) / 2.0,
        ],
        (E | SE | S, _) => [
            x + (pw - w) / 2.0 + ((w - pw) / 2.0) * cos + ((ph - h) / 2.0) * sin,
            y + (ph - h) / 2.0 + ((w - pw) / 2.0) * sin + ((h - ph) / 2.0) * cos,
        ],
        (N | NW | W, _) => [
            x + ((pw - w) / 2.0) * (cos + 1.0) + ((h - ph) / 2.0) * sin,
            y + ((ph - h) / 2.0) * (cos + 1.0) + ((pw - w) / 2.0) * sin,
        ],
        (NE, _) => [
            x + ((pw - w) / 2.0) * (1.0 - cos) + ((h - ph) / 2.0) * sin,
            y + ((ph - h) / 2.0) * (cos + 1.0) + ((w - pw) / 2.0) * sin,
        ],
        (SW, _) => [
            x + ((pw - w) / 2.0) * (cos + 1.0) + ((ph - h) / 2.0) * sin,
            y + (ph - h) / 2.0 + ((pw - w) / 2.0) * sin + ((h - ph) / 2.0) * cos,
        ],
        (Rotation, _) => [x, y],
    }
}

/// A lone note resized by `handle` (`resizeSingleElement` for notes): at
/// least one line of its label's font high and wide (proportions kept even
/// then), flipped past its other side, laid out again with the resize's
/// intent. `None` unless `originals` (the note with its label, as the
/// gesture began) is one note.
pub fn resize_alone(
    originals: &[Element],
    frame: &Frame,
    handle: Handle,
    target: Point,
    mode: ResizeMode,
    measure: Measure,
) -> Option<Vec<Element>> {
    let ids: Vec<&str> = originals.iter().map(Element::id).collect();
    let shapes: Vec<&Element> = originals
        .iter()
        .filter(|e| e.container_id().is_none_or(|c| !ids.contains(&c)))
        .collect();
    let [note] = shapes.as_slice() else {
        return None;
    };
    if !is_note(note) || note.width() == 0.0 || note.height() == 0.0 {
        return None;
    }
    let label = originals
        .iter()
        .find(|t| t.container_id() == Some(note.id()) && !t.is_deleted());
    let [x1, y1, x2, y2] = transform::resized_box(frame, handle, target, mode);
    let (mut w, mut h) = ((x2 - x1).abs(), (y2 - y1).abs());
    let (fx, fy) = (
        if x2 < x1 { -1.0 } else { 1.0 },
        if y2 < y1 { -1.0 } else { 1.0 },
    );
    let (min_w, min_h) = label.map_or((MIN_SIZE, MIN_SIZE), |t| {
        min_size(base_font_size(t), t.font_family())
    });
    w = js::max(w, min_w);
    h = js::max(h, min_h);
    if mode.keep_ratio {
        // One scale for both, even at the smallest.
        let scale = js::max(w / note.width(), h / note.height());
        w = note.width() * scale;
        h = note.height() * scale;
    }
    let (w, h) = (w * fx, h * fy);
    let [mut x, mut y] = resized_origin(note, w, h, handle, mode);
    if w < 0.0 {
        x += w;
    }
    if h < 0.0 {
        y += h;
    }
    let mut resized = (*note).clone();
    resized.set("x", num(x));
    resized.set("y", num(y));
    resized.set("width", num(w.abs()));
    resized.set("height", num(h.abs()));
    let intent = resize_intent(&resized, note, label, handle, mode);
    let (n, t) = layout(&resized, label, &intent, measure);
    Some(with_label(n, t))
}

/// Notes among several resized together (`resizeMultipleElements`): their
/// base heights and labels' ceilings follow the resize, and they are laid
/// out again.
pub fn after_resize(
    originals: &[Element],
    changed: &mut Vec<Element>,
    handle: Handle,
    mode: ResizeMode,
    measure: Measure,
) {
    let notes: Vec<Element> = changed.iter().filter(|e| is_note(e)).cloned().collect();
    for resized in notes {
        let Some(original) = originals.iter().find(|o| o.id() == resized.id()) else {
            continue;
        };
        let label = originals
            .iter()
            .find(|t| t.container_id() == Some(original.id()) && !t.is_deleted());
        let intent = resize_intent(&resized, original, label, handle, mode);
        let (n, t) = layout(&resized, label, &intent, measure);
        for e in with_label(n, t) {
            upsert(changed, e);
        }
    }
}

/// Puts `e` in `changed`, in place of the element it replaces.
pub fn upsert(changed: &mut Vec<Element>, e: Element) {
    match changed.iter_mut().find(|c| c.id() == e.id()) {
        Some(slot) => *slot = e,
        None => changed.push(e),
    }
}

/// A note and its label, as a list of changes.
fn with_label(note: Element, label: Option<Element>) -> Vec<Element> {
    std::iter::once(note).chain(label).collect()
}

/// A property set on a note or its label, kept as notes keep them
/// (`normalizeStickyNoteStyle`, `getColorUpdate`, `getBaseFontSizeUpdate`):
/// a note is never transparent and always filled; its label's ink is never
/// transparent; a label's font size is its ceiling. `e` as it was, `c` with
/// the property set.
pub fn keep_property(e: &Element, c: &mut Element, scene: &Scene, key: &str) {
    if is_note(c) {
        let fix = |v: &str, fallback: &str| {
            if color::is_transparent(v) || v.is_empty() {
                fallback.to_string()
            } else {
                v.to_string()
            }
        };
        let (ink, paper) = (
            fix(c.stroke_color(), INK),
            fix(c.background_color(), BACKGROUND),
        );
        c.set("strokeColor", json!(ink));
        c.set("backgroundColor", json!(paper));
        c.set("fillStyle", json!("solid"));
        return;
    }
    if !is_label(scene, c) {
        return;
    }
    match key {
        // Its note, picked with it, takes the default ink too
        // (`syncStickyNoteInk`).
        "strokeColor" if color::is_transparent(c.stroke_color()) => {
            c.set("strokeColor", json!(INK));
        }
        "fontSize" => {
            let size = normalize_font_size(c.font_size());
            c.set("baseFontSize", num(size));
            c.set("fontSize", num(e.font_size()));
        }
        _ => {}
    }
}

/// Which of the colours of what comes next a pick sets
/// (`resolveColorTarget`): notes have their own — with nothing selected,
/// the note tool's are picked; picked for notes and other elements
/// together, both.
pub fn pick_color(
    style: &mut crate::edit::Style,
    tool: crate::edit::Tool,
    selected: &[&Element],
    scene: &Scene,
    key: &str,
    value: &Value,
) {
    let v = value.as_str().unwrap_or_default().to_string();
    let notes = selected.iter().any(|e| is_note(e) || is_label(scene, e));
    let others = selected.iter().any(|e| !is_note(e) && !is_label(scene, e));
    let (notes, others) = if selected.is_empty() {
        let n = tool == crate::edit::Tool::StickyNote;
        (n, !n)
    } else {
        (notes, others)
    };
    let sticky = |fallback: &str| {
        if color::is_transparent(&v) || v.is_empty() {
            fallback.to_string()
        } else {
            v.clone()
        }
    };
    match key {
        "strokeColor" => {
            if notes {
                style.sticky_stroke_color = sticky(INK);
            }
            if others {
                style.stroke_color = v.clone();
            }
        }
        _ => {
            if notes {
                style.sticky_background_color = sticky(BACKGROUND);
            }
            if others {
                style.background_color = v.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests;
