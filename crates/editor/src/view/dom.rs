//! Stateless DOM helpers of the view: rendering a line, reading text back
//! from the DOM, and mapping between DOM positions and offsets in a line.
//! Nothing here touches the editor's state.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Node};

use crate::callout;
use crate::code;
use crate::html;
use crate::syntax::{self, Block};
use crate::text::{byte_to_utf16, utf16_to_byte};

pub fn document() -> Document {
    web_sys::window()
        .and_then(|w| w.document())
        .expect("no document")
}

/// A line to render: copied out of the state so rendering (which asks the
/// host about links and images) runs without the state borrowed.
pub struct LineJob {
    pub idx: usize,
    pub text: String,
    pub block: Block,
    /// On the last line of an HTML block: the block's source, for its
    /// preview ([`crate::state::EditorState::html_source`]).
    pub html: Option<String>,
}

pub struct Rendered {
    pub idx: usize,
    pub class: Cow<'static, str>,
    pub html: String,
    pub hash: u64,
    /// The rendering depends on the host (link targets, image URLs), so
    /// `refresh` must look at it again.
    pub dynamic: bool,
    /// The content of the line's HTML preview widget ([`fill_widget`]).
    pub widget: Option<String>,
}

/// The HTML of one line: its source text with syntax spans, plus a widget
/// (`contenteditable=false`, `data-w`, after the text): an image preview for
/// an embed line, the header (title, language, copy button) of a code block
/// on its opening fence, the type of a callout without a title on its first
/// line (as its title), the indentation guides of a line of code (no text),
/// the rendered preview of an HTML block on its last line (an empty element
/// here, filled by [`fill_widget`]).
pub fn render_line(
    job: LineJob,
    link_exists: &dyn Fn(&str) -> bool,
    image_url: &dyn Fn(&str) -> Option<String>,
) -> Rendered {
    let styled = syntax::style_line(&job.text, &job.block, link_exists);
    let mut html = syntax::line_html(&job.text, &styled);
    if let Some(t) = &styled.image
        && let Some(url) = image_url(t)
    {
        html.push_str("<span class=\"w\" contenteditable=\"false\" data-w=\"1\"><img src=\"");
        syntax::escape(&url, &mut html);
        html.push_str("\" alt=\"\" draggable=\"false\"></span>");
    }
    if let Block::Code { guides, .. } = &job.block {
        // Inside the part that scrolls, so the guides move with the text.
        let guides = code::guides_html(*guides, true);
        if styled.outer.is_some() && html.ends_with("</span>") {
            html.insert_str(html.len() - "</span>".len(), &guides);
        } else {
            html.push_str(&guides);
        }
    }
    if let Block::Callout(c) = &job.block
        && c.title
        && let (_, at) = syntax::quote_prefix(&job.text)
        && let Some(h) = callout::header(&job.text[at..])
        && h.title.is_empty()
    {
        html.push_str("<span class=\"w cot-d\" contenteditable=\"false\" data-w=\"1\">");
        syntax::escape(&callout::default_title(h.name), &mut html);
        html.push_str("</span>");
    }
    if let Block::Fence { info, open: true } = &job.block {
        html.push_str("<span class=\"w code-head\" contenteditable=\"false\" data-w=\"1\">");
        html.push_str(&code::head_html(info, false));
        html.push_str("</span>");
    }
    let widget = job
        .html
        .as_deref()
        .map(|src| html_preview(src, image_url))
        .filter(|w| !w.trim().is_empty());
    if widget.is_some() {
        html.push_str("<span class=\"w hw\" contenteditable=\"false\" data-w=\"1\"></span>");
    }
    let class = job.block.class();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    class.hash(&mut h);
    html.hash(&mut h);
    widget.hash(&mut h);
    Rendered {
        idx: job.idx,
        class,
        dynamic: !styled.links.is_empty() || styled.image.is_some() || widget.is_some(),
        hash: h.finish() | 1,
        html,
        widget,
    }
}

/// The preview of an HTML block: what [`html::sanitize`] keeps of it, with
/// links that open through the host when clicked (`data-href`), images
/// from the vault's attachments, and external images not loaded.
fn html_preview(src: &str, image_url: &dyn Fn(&str) -> Option<String>) -> String {
    let link = |href: &str, attrs: &str| {
        let mut o = String::from("<a class=\"a\" data-href=\"");
        syntax::escape(href, &mut o);
        o.push('"');
        o.push_str(attrs);
        o.push('>');
        o
    };
    let image = |src: &str, alt: &str, attrs: &str| {
        let mut o = String::new();
        if !notekpr_core::markdown::is_internal_target(src) {
            o.push_str("<a class=\"extimg\" data-href=\"");
            syntax::escape(src, &mut o);
            o.push_str("\" title=\"External images are not loaded automatically\">🖼 ");
            syntax::escape(
                if alt.is_empty() {
                    "external image"
                } else {
                    alt
                },
                &mut o,
            );
            o.push_str("</a>");
            return o;
        }
        o.push_str("<img");
        if let Some(url) = image_url(&notekpr_core::markdown::percent_decode(src)) {
            o.push_str(" src=\"");
            syntax::escape(&url, &mut o);
            o.push('"');
        }
        o.push_str(" alt=\"");
        syntax::escape(alt, &mut o);
        o.push('"');
        o.push_str(attrs);
        o.push_str(" draggable=\"false\">");
        o
    };
    html::sanitize(
        src,
        &html::Urls {
            link: &link,
            image: &image,
        },
    )
}

/// Put a line's HTML preview into its widget. The preview is parsed on its
/// own, with the widget as its context, so no tag in it (a stray `</span>`,
/// say) can reach the line or the lines after it.
pub fn fill_widget(line: &Element, r: &Rendered) {
    if let Some(w) = &r.widget
        && let Ok(Some(el)) = line.query_selector(":scope > .hw")
    {
        el.set_inner_html(w);
    }
}

pub fn class_with(class: &str, active: bool) -> String {
    if active {
        format!("{class} active")
    } else {
        class.to_string()
    }
}

/// Text of a line element as the source it shows: widget text left out,
/// blocks the browser nested into it (by editing natively) read as line breaks.
pub fn line_text(el: &Element) -> String {
    let Some(c) = el
        .clone_node_with_deep(true)
        .ok()
        .and_then(|c| c.dyn_into::<Element>().ok())
    else {
        return el.text_content().unwrap_or_default();
    };
    if let Ok(ws) = c.query_selector_all("[data-w]") {
        for k in 0..ws.length() {
            if let Some(w) = ws.item(k)
                && let Some(p) = w.parent_node()
            {
                let _ = p.remove_child(&w);
            }
        }
    }
    let nested = c
        .query_selector_all("div, br + *")
        .map(|l| l.length() > 0)
        .unwrap_or(false);
    if !nested {
        return c.text_content().unwrap_or_default();
    }
    let kids = c.child_nodes();
    (0..kids.length())
        .filter_map(|j| kids.item(j))
        .map(|k| k.text_content().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text of the whole editor element, one line per child node.
pub fn full_text(root: &Element) -> String {
    let kids = root.child_nodes();
    (0..kids.length())
        .filter_map(|i| kids.item(i))
        .map(|n| match n.dyn_ref::<Element>() {
            Some(el) => line_text(el),
            None => n.text_content().unwrap_or_default(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The child of `root` that contains `node` (or is it).
pub fn root_child(root: &Node, node: &Node) -> Option<Node> {
    let mut cur = node.clone();
    loop {
        let parent = cur.parent_node()?;
        if parent.is_same_node(Some(root)) {
            return Some(cur);
        }
        cur = parent;
    }
}

/// UTF-16 length of the text from the start of `line` to `(node, offset)`.
pub fn prefix_utf16(line: &Element, node: &Node, offset: u32) -> Option<usize> {
    let range = document().create_range().ok()?;
    range.set_start(line, 0).ok()?;
    if range.set_end(node, offset).is_err() {
        return None;
    }
    let prefix: String = range.to_string().into();
    Some(prefix.encode_utf16().count())
}

/// Byte offset in `text` (the line's source) of `(node, offset)` inside `line`.
pub fn offset_in_line(line: &Element, text: &str, node: &Node, offset: u32) -> usize {
    match prefix_utf16(line, node, offset) {
        Some(u) => utf16_to_byte(text, u),
        None => text.len(),
    }
}

/// The DOM position of byte `offset` of `text` (the source of `line`):
/// inside a text node, skipping widget text.
pub fn offset_to_dom(line: &HtmlElement, text: &str, offset: usize) -> (Node, u32) {
    let target = byte_to_utf16(text, offset.min(text.len()));
    let fallback = || (line.clone().unchecked_into::<Node>(), 0);
    let Ok(walker) = document().create_tree_walker_with_what_to_show(line, 4) else {
        return fallback(); // SHOW_TEXT
    };
    let mut acc = 0usize;
    let mut last: Option<(Node, u32)> = None;
    while let Ok(Some(n)) = walker.next_node() {
        if n.parent_element()
            .is_some_and(|p| p.closest("[data-w]").ok().flatten().is_some())
        {
            continue;
        }
        let len = n
            .node_value()
            .map(|v| v.encode_utf16().count())
            .unwrap_or(0);
        if acc + len >= target && len > 0 {
            return (n, (target - acc) as u32);
        }
        acc += len;
        last = Some((n.clone(), len as u32));
    }
    last.unwrap_or_else(fallback)
}

/// Where a drop at viewport coordinates `(x, y)` lands in the DOM.
pub fn caret_from_point(x: f64, y: f64) -> Option<(Node, u32)> {
    let doc = document();
    let has = |name: &str| {
        js_sys::Reflect::get(&doc, &name.into())
            .map(|f| f.is_function())
            .unwrap_or(false)
    };
    if has("caretPositionFromPoint") {
        let p = doc.caret_position_from_point(x as f32, y as f32)?;
        return Some((p.offset_node()?, p.offset()));
    }
    if has("caretRangeFromPoint") {
        // WebKit's older API.
        let f: js_sys::Function = js_sys::Reflect::get(&doc, &"caretRangeFromPoint".into())
            .ok()?
            .dyn_into()
            .ok()?;
        let r: web_sys::Range = f.call2(&doc, &x.into(), &y.into()).ok()?.dyn_into().ok()?;
        return Some((r.start_container().ok()?, r.start_offset().ok()?));
    }
    None
}
