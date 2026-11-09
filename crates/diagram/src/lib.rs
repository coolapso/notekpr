//! Mermaid diagrams as SVG, for the web client.
//!
//! [merman](https://github.com/Latias94/merman) is a Rust implementation
//! of Mermaid. It is large (≈3.8 MB of WebAssembly, compressed), so this
//! crate is a WebAssembly module of its own: `scripts/build-web.sh` builds
//! it next to the app, and the app loads it the first time a note shows a
//! diagram (`crates/web/src/diagrams.rs`). docs/development.md ("Diagrams")
//! says why merman, and what replacing it would take.
//!
//! Notes are other people's text, so a diagram is untrusted:
//!
//! * Mermaid's strict security level, which no diagram can change: labels
//!   are sanitized, `click` runs nothing.
//! * merman's interactive resource policy bounds the work and output.
//! * Links (`click A href "…"`) open web and mail addresses in a new tab;
//!   any other link is dropped ([`links`]).
//! * The app's Content-Security-Policy runs no inline script and loads no
//!   image from elsewhere, whatever the SVG says.
//!
//! Text is measured with the browser's canvas, in the app's own font, so
//! labels fit their boxes.

use merman::svg::{
    DeterministicTextMeasurer, MeasurementProfileId, TextMeasurementPolicy, TextMeasurementProfile,
    TextMeasurementProfileIdentity, TextStyle,
};
use merman::{
    MermaidEngine, OperationControl, RenderOutput, RenderRequest, Renderer,
    SourcePresentationConfig, SvgEnvironment, SvgRequest,
};

/// How a diagram is drawn.
pub struct Options<'a> {
    /// Mermaid's dark theme (else its default one), unless the diagram
    /// picks one itself.
    pub dark: bool,
    /// The CSS font family of the text (what it is measured in).
    pub font: &'a str,
    /// The SVG's id, which its styles are scoped to: unique per diagram on
    /// a page.
    pub id: &'a str,
}

/// Settings no diagram can change with `%%{init: …}%%` (Mermaid's own list).
const SECURE: &[&str] = &[
    "secure",
    "securityLevel",
    "startOnLoad",
    "maxTextSize",
    "suppressErrorRendering",
    "maxEdges",
];

/// `src` (a diagram in Mermaid's language) as SVG, its text measured with
/// `width(text, style)` in CSS pixels; or why it can't be drawn.
pub fn render(
    src: &str,
    opts: &Options,
    width: impl Fn(&str, &TextStyle) -> f64 + Send + Sync + 'static,
) -> Result<String, String> {
    let site = SourcePresentationConfig::from_value(serde_json::json!({
        "theme": if opts.dark { "dark" } else { "default" },
        "fontFamily": opts.font,
        "securityLevel": "strict",
        "secure": SECURE,
    }));
    let renderer =
        Renderer::new().with_presentation_engine(MermaidEngine::new().with_site_config(site));
    let identity = TextMeasurementProfileIdentity::new(
        MeasurementProfileId::new("notekpr.canvas").map_err(|e| e.to_string())?,
        format!("1:{}", opts.font),
    )
    .map_err(|e| e.to_string())?;
    let measurer = DeterministicTextMeasurer::default().with_width_callback(width);
    let policy = TextMeasurementPolicy::uniform(TextMeasurementProfile::new(identity, measurer));
    let mut request = SvgRequest {
        environment: SvgEnvironment::deterministic().with_text_measurement_policy(policy),
        ..Default::default()
    };
    request.options.diagram_id = Some(opts.id.to_string());
    let out = renderer
        .render(RenderRequest::svg(src, OperationControl::new(), request))
        .map_err(|e| e.to_string())?;
    match out {
        RenderOutput::Svg(Some(svg)) => Ok(links(svg.svg())),
        _ => Err("This is not a diagram Mermaid knows.".into()),
    }
}

/// Whether `url` may be opened from a diagram: a web or mail address.
fn safe_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:")
}

/// `svg` with each link (`<a>`) opening its web or mail address in a new
/// tab, and losing any other target.
fn links(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find("<a") {
        let after = &rest[at + 2..];
        // `<a` followed by a space or the end of the tag: an `a` element
        // (not `<animate`, say).
        if !after.starts_with([' ', '>', '/', '\n', '\t']) {
            out.push_str(&rest[..at + 2]);
            rest = after;
            continue;
        }
        let Some(end) = after.find('>') else { break };
        out.push_str(&rest[..at]);
        out.push_str(&link_tag(&after[..end]));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The attributes of an `<a` tag rewritten: its target (`href`,
/// `xlink:href`) kept if safe and opened in a new tab, `target` and `rel`
/// ours.
fn link_tag(attrs: &str) -> String {
    let mut kept = String::new();
    let mut href = None;
    for (name, value) in attributes(attrs) {
        match name {
            "href" | "xlink:href" => href = href.or(Some(value)),
            "target" | "rel" => {}
            _ if name.starts_with("on") => {}
            _ => {
                kept.push(' ');
                kept.push_str(name);
                kept.push_str("=\"");
                kept.push_str(value);
                kept.push('"');
            }
        }
    }
    let mut tag = String::from("<a");
    tag.push_str(&kept);
    if let Some(h) = href.filter(|h| safe_url(&unescape(h))) {
        tag.push_str(" href=\"");
        tag.push_str(h);
        tag.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\"");
    }
    if attrs.trim_end().ends_with('/') {
        tag.push_str(" /");
    }
    tag
}

/// The attributes in a tag's text, as written (values still escaped).
/// merman writes them as `name="value"`; anything else ends the list.
fn attributes(s: &str) -> Vec<(&str, &str)> {
    let mut out = vec![];
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find("=\"") else { break };
        let name = &rest[..eq];
        if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '/') {
            break;
        }
        let value_from = eq + 2;
        let Some(close) = rest[value_from..].find('"') else {
            break;
        };
        out.push((name, &rest[value_from..value_from + close]));
        rest = &rest[value_from + close + 1..];
    }
    out
}

/// An attribute value as the browser reads it (the entities a scheme could
/// hide behind).
fn unescape(v: &str) -> String {
    v.replace("&amp;", "&")
        .replace("&#58;", ":")
        .replace("&#x3a;", ":")
        .replace("&#x3A;", ":")
        .replace("&colon;", ":")
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;

    use wasm_bindgen::prelude::*;
    use web_sys::CanvasRenderingContext2d;

    thread_local! {
        /// A canvas to measure text with, and the font it is set to.
        static CANVAS: RefCell<Option<(CanvasRenderingContext2d, String)>> =
            const { RefCell::new(None) };
    }

    /// The width of `text` in `font` (CSS), measured by the browser.
    fn measure(text: &str, font: &str) -> f64 {
        CANVAS.with(|c| {
            let mut c = c.borrow_mut();
            if c.is_none() {
                let ctx = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.create_element("canvas").ok())
                    .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                    .and_then(|canvas| canvas.get_context("2d").ok().flatten())
                    .and_then(|ctx| ctx.dyn_into::<CanvasRenderingContext2d>().ok());
                *c = ctx.map(|ctx| (ctx, String::new()));
            }
            let Some((ctx, set)) = c.as_mut() else {
                // No canvas: about half an em per character.
                return text.chars().count() as f64 * 8.0;
            };
            if set != font {
                ctx.set_font(font);
                *set = font.to_string();
            }
            ctx.measure_text(text)
                .map(|m| m.width())
                .ok()
                .filter(|w| w.is_finite() && *w >= 0.0)
                .unwrap_or(0.0)
        })
    }

    /// `src` as SVG (see [`crate::render`]); throws a string saying why it
    /// can't be drawn.
    #[wasm_bindgen]
    pub fn render(src: &str, dark: bool, font: &str, id: &str) -> Result<String, JsValue> {
        let family = font.to_string();
        let opts = crate::Options { dark, font, id };
        crate::render(src, &opts, move |text, style| {
            let css = format!(
                "{} {} {}px {}",
                style.font_style.as_deref().unwrap_or("normal"),
                style.font_weight.as_deref().unwrap_or("normal"),
                style.font_size,
                style.font_family.as_deref().unwrap_or(&family),
            );
            measure(text, &css)
        })
        .map_err(|e| JsValue::from_str(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(src: &str) -> Result<String, String> {
        let opts = Options {
            dark: false,
            font: "sans-serif",
            id: "d1",
        };
        render(src, &opts, |t, s| {
            t.chars().count() as f64 * s.font_size * 0.55
        })
    }

    #[test]
    fn draws_diagrams() {
        for src in [
            "flowchart LR\n  A[Write] --> B{Shared?}\n  B -->|Yes| C[Encrypt]",
            "sequenceDiagram\n  Alice->>Bob: Hi\n  Bob-->>Alice: Hello",
            "classDiagram\n  Vault --> Note",
            "stateDiagram-v2\n  [*] --> Draft\n  Draft --> [*]",
            "pie title Notes\n  \"Work\" : 42\n  \"Home\" : 17",
            "gantt\n  dateFormat YYYY-MM-DD\n  section A\n  Task :a1, 2026-11-01, 5d",
            "erDiagram\n  VAULT ||--o{ NOTE : holds",
            "mindmap\n  root((notekpr))\n    Notes\n    Sync",
            "architecture-beta\n  service db(database)[Database]\n  service api(server)[API]\n  db:L -- R:api",
        ] {
            let svg = draw(src);
            assert!(
                svg.as_ref().is_ok_and(|s| s.starts_with("<svg")),
                "{src}: {svg:?}"
            );
            assert!(svg.unwrap().contains("id=\"d1\""), "{src}");
        }
        assert!(draw("flowchart LR\n  A -->").is_err());
        assert!(draw("not a diagram").is_err());
        let dark = render(
            "flowchart LR\n  A --> B",
            &Options {
                dark: true,
                font: "sans-serif",
                id: "d2",
            },
            |t, _| t.len() as f64 * 8.0,
        )
        .unwrap();
        assert_ne!(
            dark,
            draw("flowchart LR\n  A --> B").unwrap().replace("d1", "d2")
        );
    }

    #[test]
    fn diagrams_are_untrusted() {
        let svg = draw(
            "%%{init: {\"securityLevel\": \"loose\"}}%%\nflowchart LR\n  \
             A[\"<img src=x onerror=alert(1)><script>alert(2)</script>\"] --> \
             B[\"<a href='https://e.example' onclick='alert(3)'>x</a>\"]\n  \
             click A href \"javascript:alert(4)\"\n  click B href \"https://ok.example\"",
        )
        .unwrap();
        for bad in ["<script", "onerror", "onclick", "javascript:"] {
            assert!(!svg.contains(bad), "{bad} in {svg}");
        }
        // Links to web addresses stay, as in a note, opening a new tab.
        for to in ["https://ok.example/", "https://e.example"] {
            assert!(
                svg.contains(&format!(
                    "href=\"{to}\" target=\"_blank\" rel=\"noopener noreferrer nofollow\""
                )),
                "{to} in {svg}"
            );
        }
        // A diagram can't loosen the security level (antiscript would
        // keep the frame and the style).
        for level in ["antiscript", "loose", "sandbox"] {
            let svg = draw(&format!(
                "%%{{init: {{\"securityLevel\": \"{level}\"}}}}%%\nflowchart LR\n  \
                 A[\"<iframe src='https://e.example'></iframe><style>*{{color:red}}</style>\"] --> B"
            ))
            .unwrap();
            assert!(
                !svg.contains("<iframe") && !svg.contains("color:red"),
                "{level}: {svg}"
            );
        }
    }

    /// Every rule of a diagram's style sheet is about that diagram (its
    /// `#id`), whatever its styles say. (What they do inside it, the page
    /// contains: `contain: paint` in style.css.)
    #[test]
    fn styles_stay_in_the_diagram() {
        for src in [
            "flowchart LR\n  A --> B\n  classDef c fill:#f00} body {display:none\n  class B c",
            "flowchart LR\n  A --> B\n  style A fill:red} * {display:none",
            "flowchart LR\n  A --> B\n  linkStyle 0 stroke:red} html{display:none",
            "%%{init: {\"themeVariables\": {\"primaryColor\": \"red} body {display:none} .x{color:red\"}}}%%\nflowchart LR\n  A --> B",
            "%%{init: {\"fontFamily\": \"x} body {display:none} .x{color:red\"}}%%\nflowchart LR\n  A --> B",
            "%%{init: {\"themeCSS\": \"body {display:none}\"}}%%\nflowchart LR\n  A --> B",
        ] {
            let Ok(svg) = draw(src) else { continue };
            let style = svg
                .split("<style>")
                .nth(1)
                .and_then(|s| s.split("</style>").next())
                .unwrap_or("");
            for rule in style.split('}') {
                let selectors = rule.split('{').next().unwrap_or("").trim();
                let scoped = selectors.split(',').all(|p| {
                    let p = p.trim();
                    p.is_empty()
                        || p.starts_with("#d1")
                        || p.starts_with("@keyframes")
                        || p == "from"
                        || p == "to"
                        || p.ends_with('%')
                });
                assert!(scoped, "{selectors:?} in {src}");
            }
        }
    }

    #[test]
    fn links_open_safe_addresses_in_a_new_tab() {
        assert_eq!(
            links(
                "<g><a transform=\"t\" xlink:href=\"https://x.example\" target=\"_self\"><g/></a>"
            ),
            "<g><a transform=\"t\" href=\"https://x.example\" target=\"_blank\" rel=\"noopener noreferrer nofollow\"><g/></a>"
        );
        assert_eq!(
            links("<a href=\"java&#x3A;script:x\" onclick=\"y\">"),
            "<a>"
        );
        assert_eq!(links("<a xlink:href=\"#local\">"), "<a>");
        assert_eq!(
            links("<animate attributeName=\"x\"/>"),
            "<animate attributeName=\"x\"/>"
        );
        assert_eq!(links("<a/>"), "<a />");
    }
}
