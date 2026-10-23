//! Raw HTML in notes: what of it is shown ([`sanitize`]), and where HTML
//! blocks start and end in the source ([`BlockKind`]).
//!
//! Notes come from other people (shared vaults, notes shared with you), and
//! the page that shows them holds the decrypted keys, so HTML in a note is
//! untrusted. [`sanitize`] keeps an allow list of formatting elements
//! (headings, tables, lists, `details`, `kbd`, `sub`/`sup`, …) with a few
//! attributes whose values are checked (`align`, `colspan`, `width`, …).
//! Everything else goes: scripts and styles with their content, frames,
//! forms, SVG and MathML, comments, event handlers, and the `style`, `class`
//! and `id` attributes (nothing drawn over the app, no DOM clobbering,
//! nothing posing as the app's own markup). The one thing read from `style`
//! is the text or background color of a `span`, `font` or `mark`, which is
//! written as the app's class for the palette color nearest to it
//! ([`color`]). Links and images follow the rules for Markdown ones, which
//! the caller applies ([`Urls`]).
//!
//! The input is tokenized by html5ever as the HTML standard says (so
//! character references and odd quoting decode the way a browser decodes
//! them), and the output is written from the tokens: allowed tags with
//! their allowed attributes, and text, all escaped. Nothing is copied
//! through as it was, and no allowed element changes how a browser
//! tokenizes what follows it (no raw text, RCDATA or foreign content), so
//! a browser sees the tags written here and no others.
//!
//! The output isn't balanced: unclosed and stray tags are left to the
//! browser, so that HTML around Markdown (`<details>`, Markdown, then
//! `</details>`) works in the reading view. Insert it into an element of its
//! own (`innerHTML`), where a stray end tag can't close anything outside it.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, StartTag, Tag, TagToken, Token, TokenSink,
    TokenSinkResult, Tokenizer, TokenizerOpts,
};

use crate::color::{self, Colors};
use crate::syntax::escape;

/// How links and images are written: the caller resolves their targets
/// (notes, attachments) and decides which external ones may open.
pub struct Urls<'a> {
    /// The opening tag of a link to `href` (decoded). `attrs` are the
    /// element's other attributes, escaped, each after a space.
    pub link: &'a dyn Fn(&str, &str) -> String,
    /// An image: its `src` and `alt` (decoded; `alt` may be empty) and its
    /// other attributes as for links.
    pub image: &'a dyn Fn(&str, &str, &str) -> String,
}

/// Keep only the harmless part of `html` (see the module docs).
pub fn sanitize(html: &str, urls: &Urls) -> String {
    let tok = Tokenizer::new(
        Sink {
            urls,
            out: RefCell::new(String::with_capacity(html.len())),
            skip: RefCell::new(None),
        },
        TokenizerOpts::default(),
    );
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.out.into_inner()
}

/// Whether [`sanitize`] keeps element `name` and, if given, its attribute
/// `attr`.
pub fn is_allowed(name: &str, attr: Option<&str>) -> bool {
    element(name).is_some_and(|a| attr.is_none_or(|k| GLOBAL.contains(&k) || a.contains(&k)))
}

/// Elements that are kept, and the attributes each may have besides
/// [`GLOBAL`] ones.
fn element(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "a" => &["href"],
        "img" => &["src", "alt", "width", "height", "align"],
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "caption" => &["align"],
        "table" => &["align", "width"],
        "thead" | "tbody" | "tfoot" | "tr" => &["align", "valign"],
        "td" | "th" => &["align", "valign", "colspan", "rowspan", "width"],
        "ol" => &["start", "reversed", "type"],
        "li" => &["value"],
        "details" => &["open"],
        // Only the class the sanitizer writes for their colors.
        "span" | "font" | "mark" => &["class"],
        "abbr" | "b" | "blockquote" | "br" | "cite" | "code" | "dd" | "del" | "dfn" | "dl"
        | "dt" | "em" | "figcaption" | "figure" | "hr" | "i" | "ins" | "kbd" | "pre" | "q"
        | "rp" | "rt" | "ruby" | "s" | "samp" | "small" | "strike" | "strong" | "sub"
        | "summary" | "sup" | "u" | "ul" | "var" | "wbr" => &[],
        _ => return None,
    })
}

/// Attributes any kept element may have.
const GLOBAL: &[&str] = &["title", "lang", "dir"];

/// Elements without content: never closed.
fn void(name: &str) -> bool {
    matches!(name, "br" | "hr" | "img" | "wbr")
}

/// Elements dropped together with their content, and how a browser
/// tokenizes that content (so that it ends where a browser ends it).
fn dropped_with_content(name: &str) -> Option<TokenSinkResult<()>> {
    Some(match name {
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
            TokenSinkResult::RawData(RawKind::Rawtext)
        }
        "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rcdata),
        "plaintext" => TokenSinkResult::Plaintext,
        _ => return None,
    })
}

/// The value to write for attribute `name` (`Some("")` for a boolean one),
/// if `value` is acceptable for it.
fn attr_value<'v>(name: &str, value: &'v str) -> Option<&'v str> {
    let v = value.trim();
    let digits = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())
    };
    let one_of = |names: &[&str]| names.iter().any(|n| v.eq_ignore_ascii_case(n));
    let ok = match name {
        "title" | "alt" => return Some(value),
        "open" | "reversed" => return Some(""),
        "align" => one_of(&["left", "center", "right", "justify"]),
        "valign" => one_of(&["top", "middle", "bottom", "baseline"]),
        "dir" => one_of(&["ltr", "rtl", "auto"]),
        "width" | "height" => digits(v.strip_suffix('%').unwrap_or(v), 5),
        "colspan" | "rowspan" => digits(v, 4),
        "start" | "value" => digits(v.strip_prefix('-').unwrap_or(v), 9),
        "type" => matches!(v, "1" | "a" | "A" | "i" | "I"),
        "lang" => {
            !v.is_empty()
                && v.len() <= 35
                && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }
        _ => false,
    };
    ok.then_some(v)
}

struct Sink<'a> {
    urls: &'a Urls<'a>,
    out: RefCell<String>,
    /// The element whose content is being dropped.
    skip: RefCell<Option<String>>,
}

impl TokenSink for Sink<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut skip = self.skip.borrow_mut();
        if let Some(name) = skip.as_deref() {
            // Its content is tokenized as text: the only tag that can come
            // is the end tag that closes it.
            if let TagToken(t) = &token
                && t.kind == EndTag
                && *t.name == *name
            {
                *skip = None;
            }
            return TokenSinkResult::Continue;
        }
        match token {
            CharacterTokens(text) => escape(&text, &mut self.out.borrow_mut()),
            TagToken(tag) => {
                if tag.kind == StartTag
                    && let Some(raw) = dropped_with_content(&tag.name)
                {
                    *skip = Some(tag.name.to_string());
                    return raw;
                }
                self.tag(&tag);
            }
            // Comments, doctypes, NUL characters, parse errors.
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

impl Sink<'_> {
    fn tag(&self, tag: &Tag) {
        let name: &str = &tag.name;
        let Some(allowed) = element(name) else {
            return;
        };
        let mut out = self.out.borrow_mut();
        if tag.kind == EndTag {
            if !void(name) {
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
            return;
        }
        let mut attrs = String::new();
        let (mut href, mut src, mut alt) = (None, None, "");
        // The tokenizer keeps the first of duplicate attributes, as browsers do.
        for a in &tag.attrs {
            let key: &str = &a.name.local;
            if !(GLOBAL.contains(&key) || allowed.contains(&key)) {
                continue;
            }
            match key {
                "href" => href = Some(&*a.value),
                "src" => src = Some(&*a.value),
                "alt" => alt = &a.value,
                _ => {
                    if let Some(v) = attr_value(key, &a.value) {
                        attrs.push(' ');
                        attrs.push_str(key);
                        if !matches!(key, "open" | "reversed") {
                            attrs.push_str("=\"");
                            escape(v, &mut attrs);
                            attrs.push('"');
                        }
                    }
                }
            }
        }
        let colors = color::of_element(name, tag.attrs.iter().map(|a| (&*a.name.local, &*a.value)));
        if !colors.is_empty() {
            attrs.push_str(" class=\"");
            attrs.push_str(&colors.classes());
            attrs.push('"');
        }
        match name {
            "a" => match href {
                Some(h) => out.push_str(&(self.urls.link)(h, &attrs)),
                None => {
                    out.push_str("<a");
                    out.push_str(&attrs);
                    out.push('>');
                }
            },
            "img" => {
                if let Some(s) = src {
                    out.push_str(&(self.urls.image)(s, alt, &attrs));
                }
            }
            _ => {
                out.push('<');
                out.push_str(name);
                out.push_str(&attrs);
                out.push('>');
            }
        }
    }
}

// ------------------------------------------------------------ HTML blocks

/// The kinds of HTML block (CommonMark §4.6), which differ in how they end.
/// Markdown inside an HTML block is not Markdown: the block is HTML up to
/// its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// `<pre`, `<script`, `<style` or `<textarea`: up to the line with the
    /// closing tag.
    Raw,
    /// `<!--`: up to the line with `-->`.
    Comment,
    /// `<?`: up to `?>`.
    Instruction,
    /// `<!` and a letter: up to `>`.
    Declaration,
    /// `<![CDATA[`: up to `]]>`.
    Cdata,
    /// A block-level tag (`<div`, `</table`, `<details`, …): up to a blank
    /// line.
    Block,
    /// Any other complete tag alone on its line (`<img …>`, `<span>`): up to
    /// a blank line. It can't interrupt a paragraph.
    Tag,
}

const RAW: [&str; 4] = ["pre", "script", "style", "textarea"];

const BLOCK_NAMES: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

fn starts_with_ci(s: &[u8], p: &str) -> bool {
    s.len() >= p.len() && s[..p.len()].eq_ignore_ascii_case(p.as_bytes())
}

/// The ASCII tag name at the start of `s` (a letter, then letters, digits
/// and `-`), as its length.
fn tag_name(s: &[u8]) -> usize {
    if !s.first().is_some_and(u8::is_ascii_alphabetic) {
        return 0;
    }
    1 + s[1..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
        .count()
}

impl BlockKind {
    /// The kind of HTML block `line` starts, if it starts one.
    pub fn start(line: &str) -> Option<BlockKind> {
        let indent = line.bytes().take_while(|b| *b == b' ').count();
        if indent > 3 {
            return None;
        }
        let s = &line.as_bytes()[indent..];
        if s.first() != Some(&b'<') {
            return None;
        }
        let rest = &s[1..];
        let name_ends = |n: usize, slash_ok: bool| match rest.get(n) {
            None | Some(b' ' | b'\t' | b'>') => true,
            Some(b'/') => slash_ok && rest.get(n + 1) == Some(&b'>'),
            _ => false,
        };
        if RAW
            .iter()
            .any(|r| starts_with_ci(rest, r) && name_ends(r.len(), false))
        {
            return Some(BlockKind::Raw);
        }
        if rest.starts_with(b"!--") {
            return Some(BlockKind::Comment);
        }
        if rest.starts_with(b"?") {
            return Some(BlockKind::Instruction);
        }
        if rest.starts_with(b"![CDATA[") {
            return Some(BlockKind::Cdata);
        }
        if rest.first() == Some(&b'!') && rest.get(1).is_some_and(u8::is_ascii_alphabetic) {
            return Some(BlockKind::Declaration);
        }
        let closing = usize::from(rest.first() == Some(&b'/'));
        let n = tag_name(&rest[closing..]);
        if n == 0 {
            return None;
        }
        let name = &rest[closing..closing + n];
        let is = |names: &[&str]| {
            names
                .iter()
                .any(|b| name.eq_ignore_ascii_case(b.as_bytes()))
        };
        if is(BLOCK_NAMES) && name_ends(closing + n, true) {
            return Some(BlockKind::Block);
        }
        let len = tag_len(s)?;
        (!is(&RAW) && s[len..].iter().all(|b| matches!(b, b' ' | b'\t'))).then_some(BlockKind::Tag)
    }

    /// Whether the block may start right after a line of a paragraph.
    pub fn interrupts_paragraph(self) -> bool {
        self != BlockKind::Tag
    }

    /// Whether the block ends at the first blank line after it (rather than
    /// at a line with its end marker).
    pub fn ends_at_blank(self) -> bool {
        matches!(self, BlockKind::Block | BlockKind::Tag)
    }

    /// Whether `line` (the first one of the block or a later one) is the
    /// block's last because it holds its end marker.
    pub fn ends_on(self, line: &str) -> bool {
        match self {
            BlockKind::Raw => {
                let l = line.to_ascii_lowercase();
                RAW.iter().any(|r| l.contains(&format!("</{r}>")))
            }
            BlockKind::Comment => line.contains("-->"),
            BlockKind::Instruction => line.contains("?>"),
            BlockKind::Declaration => line.contains('>'),
            BlockKind::Cdata => line.contains("]]>"),
            BlockKind::Block | BlockKind::Tag => false,
        }
    }
}

/// The length of the raw HTML at the start of `s` as CommonMark sees it in
/// a line (§6.6): an open tag, a closing tag or a comment (`<!-- … -->`).
pub use notekpr_core::markdown::html_tag_len as tag_len;

/// One tag in a line of Markdown, as a browser reads it.
#[derive(Debug, PartialEq, Eq)]
pub struct InlineTag {
    /// Lowercase.
    pub name: String,
    pub closing: bool,
    /// An `a`'s link target, decoded.
    pub href: Option<String>,
    /// The colors it sets.
    pub colors: Colors,
}

/// Read the tag `tag` (one that [`tag_len`] found).
pub fn inline_tag(tag: &str) -> Option<InlineTag> {
    struct First(RefCell<Option<Tag>>);
    impl TokenSink for First {
        type Handle = ();
        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            if let TagToken(t) = token {
                self.0.borrow_mut().get_or_insert(t);
            }
            TokenSinkResult::Continue
        }
    }
    let tok = Tokenizer::new(First(RefCell::new(None)), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(tag));
    let _ = tok.feed(&input);
    tok.end();
    let t = tok.sink.0.into_inner()?;
    Some(InlineTag {
        name: t.name.to_string(),
        closing: t.kind == EndTag,
        href: t
            .attrs
            .iter()
            .find(|a| &*a.name.local == "href")
            .map(|a| a.value.to_string()),
        colors: match t.kind {
            EndTag => Colors::default(),
            StartTag => {
                color::of_element(&t.name, t.attrs.iter().map(|a| (&*a.name.local, &*a.value)))
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(html: &str) -> String {
        let link = |href: &str, attrs: &str| {
            let mut o = String::from("<a href=\"");
            escape(href, &mut o);
            o.push('"');
            o.push_str(attrs);
            o.push('>');
            o
        };
        let image = |src: &str, alt: &str, attrs: &str| {
            let mut o = String::from("<img src=\"");
            escape(src, &mut o);
            o.push_str("\" alt=\"");
            escape(alt, &mut o);
            o.push('"');
            o.push_str(attrs);
            o.push('>');
            o
        };
        sanitize(
            html,
            &Urls {
                link: &link,
                image: &image,
            },
        )
    }

    #[test]
    fn keeps_formatting() {
        assert_eq!(
            plain("<b>bold</b> <kbd>Ctrl</kbd>+<kbd>K</kbd> H<sub>2</sub>O x<sup>2</sup>"),
            "<b>bold</b> <kbd>Ctrl</kbd>+<kbd>K</kbd> H<sub>2</sub>O x<sup>2</sup>"
        );
        assert_eq!(
            plain("<details open><summary>More</summary>\n<p align=\"center\">x</p></details>"),
            "<details open><summary>More</summary>\n<p align=\"center\">x</p></details>"
        );
        assert_eq!(
            plain("<table><tr><td colspan=2 align=RIGHT>a</td></tr></table>"),
            "<table><tr><td colspan=\"2\" align=\"RIGHT\">a</td></tr></table>"
        );
        assert_eq!(plain("a<br/>b<br>c</br>"), "a<br>b<br>c");
        assert_eq!(
            plain("<ol start=3 reversed type=i><li value=7>x"),
            "<ol start=\"3\" reversed type=\"i\"><li value=\"7\">x"
        );
    }

    #[test]
    fn drops_scripts_styles_frames_and_forms() {
        assert_eq!(plain("a<script>alert(1)</script>b"), "ab");
        assert_eq!(
            plain("a<script>if (x</script>) alert(1)</script>b"),
            "a) alert(1)b"
        );
        assert_eq!(plain("<style>body{display:none}</style>x"), "x");
        assert_eq!(plain("<iframe src=https://example.org></iframe>x"), "x");
        assert_eq!(plain("<textarea><b>x</b></textarea>y"), "y");
        assert_eq!(plain("<title></title><script>alert(1)</script>"), "");
        assert_eq!(
            plain("<noscript><img src=x onerror=alert(1)></noscript>ok"),
            "ok"
        );
        assert_eq!(plain("<xmp><script>alert(1)</script></xmp>ok"), "ok");
        assert_eq!(plain("x<plaintext><b>everything after"), "x");
        assert_eq!(
            plain("<form action=/api><input name=pw><button>Go</button></form>"),
            "Go"
        );
        assert_eq!(
            plain("<object data=x><embed src=y>fallback</object>"),
            "fallback"
        );
        assert_eq!(
            plain("<svg><style>x</style><script>alert(1)</script><text>t</text></svg>"),
            "t"
        );
        assert_eq!(plain("<math><mi>x</mi></math>"), "x");
        assert_eq!(
            plain("<!-- hidden -->shown<!DOCTYPE html><![CDATA[x]]>"),
            "shown"
        );
        assert_eq!(plain("a\0b"), "ab");
    }

    #[test]
    fn drops_dangerous_attributes() {
        assert_eq!(
            plain("<b onclick=alert(1) onmouseover='x'>x</b>"),
            "<b>x</b>"
        );
        assert_eq!(
            plain("<div style=\"position:fixed;inset:0\" class=wl id=app data-note=1>x</div>"),
            "<div>x</div>"
        );
        assert_eq!(
            plain("<p align=\"center;x\" title='a\"b'>x</p>"),
            "<p title=\"a&quot;b\">x</p>"
        );
        assert_eq!(
            plain("<img src=a.png width=300 height=\"50%\" onerror=alert(1)>"),
            "<img src=\"a.png\" alt=\"\" width=\"300\" height=\"50%\">"
        );
        assert_eq!(
            plain("<img src=a.png width=\"100px\" alt=hi>"),
            "<img src=\"a.png\" alt=\"hi\">"
        );
        assert_eq!(plain("<img alt=no-src>"), "");
        assert_eq!(plain("<td colspan=99999 rowspan=-1>"), "<td>");
        assert_eq!(
            plain("<span lang=\"en\" dir=rtl xml:lang=x>"),
            "<span lang=\"en\" dir=\"rtl\">"
        );
        // Only the first of duplicate attributes counts, as in a browser.
        assert_eq!(plain("<details open open>"), "<details open>");
    }

    #[test]
    fn keeps_colors_as_palette_classes() {
        assert_eq!(
            plain("<span style=\"color: #e44045\">red</span>"),
            "<span class=\"fg-red\">red</span>"
        );
        assert_eq!(
            plain(
                "<mark style=\"background: #FFB86CA6;\">x</mark> <font color=\"blue\" face=x>y</font>"
            ),
            "<mark class=\"bg-orange\">x</mark> <font class=\"fg-blue\">y</font>"
        );
        // Nothing else of the style or the class: only palette classes come out.
        assert_eq!(
            plain(
                "<span class=\"wl fg-green\" style=\"position:fixed;color:navy;background:url(x)\" title=t>"
            ),
            "<span title=\"t\" class=\"fg-blue\">"
        );
        assert_eq!(
            plain("<span style=\"color: black\" class=x>x</span>"),
            "<span>x</span>"
        );
        assert_eq!(
            plain("<span style=\"color: &quot;red&quot;\">x</span>"),
            "<span>x</span>"
        );
        assert_eq!(plain("<div style=\"color: red\">x</div>"), "<div>x</div>");
        // What comes out reads back the same.
        let out = plain("<span style=\"color: red; background-color: hsl(120 50% 50%)\">x</span>");
        assert_eq!(out, "<span class=\"fg-red bg-green\">x</span>");
        assert_eq!(plain(&out), out);
    }

    #[test]
    fn links_and_images_go_to_the_caller_decoded() {
        // Character references are decoded the way a browser decodes them,
        // so the caller checks the real target.
        assert_eq!(
            plain("<a href=\"&#106;avascript&colon;alert(1)\" title=t>x</a>"),
            "<a href=\"javascript:alert(1)\" title=\"t\">x</a>"
        );
        assert_eq!(plain("<a name=x>x</a>"), "<a>x</a>");
        assert_eq!(
            plain("<img src='a b.png' alt='&lt;x&gt;'>"),
            "<img src=\"a b.png\" alt=\"&lt;x&gt;\">"
        );
    }

    #[test]
    fn text_is_escaped_and_entities_kept() {
        assert_eq!(plain("a < b && c > d"), "a &lt; b &amp;&amp; c &gt; d");
        // A carriage return stays one (found by fuzzing).
        assert_eq!(plain("'&#13)"), "&#39;&#13;)");
        assert_eq!(plain(&plain("'&#13)")), plain("'&#13)"));
        assert_eq!(
            plain("&lt;script&gt; &copy; &nbsp;"),
            "&lt;script&gt; \u{a9} \u{a0}"
        );
        assert_eq!(
            plain("<b title=\"<script>\">"),
            "<b title=\"&lt;script&gt;\">"
        );
        assert_eq!(plain("<unknown>x</unknown> <custom-el>y"), "x y");
        assert_eq!(plain("<b"), "");
        assert_eq!(plain("</div></span>x"), "</div></span>x");
    }

    #[test]
    fn block_kinds() {
        use BlockKind::*;
        let k = BlockKind::start;
        assert_eq!(k("<pre>"), Some(Raw));
        assert_eq!(k("<SCRIPT type=x>"), Some(Raw));
        assert_eq!(k("<prefix>"), Some(Tag));
        assert_eq!(k("<!-- note -->"), Some(Comment));
        assert_eq!(k("<?php"), Some(Instruction));
        assert_eq!(k("<!DOCTYPE html>"), Some(Declaration));
        assert_eq!(k("<![CDATA["), Some(Cdata));
        assert_eq!(k("<div>"), Some(Block));
        assert_eq!(k("   <details open>"), Some(Block));
        assert_eq!(k("</TABLE>"), Some(Block));
        assert_eq!(k("<div"), Some(Block));
        assert_eq!(k("<hr/>"), Some(Block));
        assert_eq!(k("<div>text after"), Some(Block));
        assert_eq!(k("<divx>"), Some(Tag));
        assert_eq!(k("<img src=\"a.png\" width=300>"), Some(Tag));
        assert_eq!(k("<span>  "), Some(Tag));
        assert_eq!(k("</span>"), Some(Tag));
        assert_eq!(k("<span>text</span>"), None);
        assert_eq!(k("<img src=\"a.png"), None);
        assert_eq!(k("    <div>"), None);
        assert_eq!(k("<https://example.org>"), None);
        assert_eq!(k("<a@b.c>"), None);
        assert_eq!(k("text <div>"), None);
        assert!(Block.interrupts_paragraph() && !Tag.interrupts_paragraph());
        assert!(Comment.ends_on("x -->") && !Comment.ends_on("x"));
        assert!(Raw.ends_on("</Pre>") && Raw.ends_on("a </style> b") && !Raw.ends_on("</p>"));
        assert!(Declaration.ends_on("<!DOCTYPE html>"));
        assert!(!Block.ends_on("</div>") && Block.ends_at_blank() && !Raw.ends_at_blank());
    }

    #[test]
    fn inline_tags() {
        let t = |s: &str| inline_tag(s).unwrap();
        assert_eq!(
            t("<A HREF='Note%20x' title=t>"),
            InlineTag {
                name: "a".into(),
                closing: false,
                href: Some("Note%20x".into()),
                colors: Colors::default(),
            }
        );
        assert_eq!(
            t("<font color=red style='background: #ffd00080'>").colors,
            Colors {
                text: Some(color::Color::Red),
                background: Some(color::Color::Yellow)
            }
        );
        // An end tag's attributes don't count.
        assert!(t("</span style='color: red'>").colors.is_empty());
        assert_eq!(t("<a href=\"&#106;s:x\">").href.as_deref(), Some("js:x"));
        assert!(t("</Kbd >").closing && t("</Kbd >").name == "kbd");
        assert!(inline_tag("<!-- c -->").is_none());
    }
}
