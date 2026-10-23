//! Reading view: Markdown → HTML that is safe by construction.
//!
//! * Raw HTML in notes keeps only what [`html::sanitize`] allows: formatting
//!   elements with a few checked attributes; no scripts, styles, frames,
//!   forms, event handlers, `style`, `class` or `id` (a text or background
//!   color becomes the app's class for a palette color).
//! * Link targets are limited to http(s), mailto and in-vault references,
//!   in Markdown and HTML alike.
//! * External images are not loaded (privacy: no tracking pixels; the CSP
//!   blocks them anyway) and are shown as links instead.
//! * Every attribute we emit is escaped.
//!
//! Callouts (`> [!tip] Title`, [`crate::editor::callout`]) are block quotes
//! rendered as a box with an icon and the title, a `details` element when
//! they fold.
//!
//! Comment threads (docs/comments.md) highlight the text they are about
//! (`span.cmt[data-thread]`), and text can say where it is in the source
//! (`data-o`, a byte offset) so a selection in the reading view can be
//! commented on: every piece of text that is the source character for
//! character sits directly in an element with its offset.

use pulldown_cmark::{
    Alignment, CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd,
};

use crate::editor::code::{self, Carry, FenceInfo, Indent};
use crate::editor::{callout, html, syntax};

pub struct Ctx<'a> {
    /// Resolve a link target (wiki or relative Markdown) to a note id.
    pub note: &'a dyn Fn(&str) -> Option<String>,
    /// Resolve an embed target to an attachment id.
    pub attachment: &'a dyn Fn(&str) -> Option<String>,
    /// Comment threads to highlight.
    pub comments: &'a [Highlight],
    /// Say where text is in the source (`data-o`).
    pub offsets: bool,
}

impl<'a> Ctx<'a> {
    /// Links resolved with `note` and `attachment`; no highlights, no offsets.
    pub fn new(
        note: &'a dyn Fn(&str) -> Option<String>,
        attachment: &'a dyn Fn(&str) -> Option<String>,
    ) -> Self {
        Ctx {
            note,
            attachment,
            comments: &[],
            offsets: false,
        }
    }

    fn marks_text(&self) -> bool {
        self.offsets || !self.comments.is_empty()
    }
}

/// A comment thread's highlight: the bytes of the source its text is, and
/// its id; `open` while its thread is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub id: String,
    pub open: bool,
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    syntax::escape(s, &mut o);
    o
}

/// Where a piece of rendered text is in the source.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// From this byte offset on, character for character.
    At(usize),
    /// Somewhere in these bytes, not character for character (escapes,
    /// entities).
    Within(usize, usize),
}

impl Place {
    /// A text event's text, from the source bytes `range`: they are the same,
    /// or the text is their end (a callout title without its type).
    fn text(src: &str, range: &std::ops::Range<usize>, text: &str) -> Place {
        match src.get(range.clone()) {
            Some(s) if s.ends_with(text) => Place::At(range.end - text.len()),
            _ => Place::Within(range.start, range.end),
        }
    }

    /// Inline code's text, inside its backticks in `range`.
    fn code(src: &str, range: &std::ops::Range<usize>, text: &str) -> Place {
        match src.get(range.clone()).and_then(|s| s.find(text)) {
            Some(i) if !text.is_empty() => Place::At(range.start + i),
            _ => Place::Within(range.start, range.end),
        }
    }

    /// The place `by` bytes further into the text.
    fn after(self, by: usize) -> Place {
        match self {
            Place::At(a) => Place::At(a + by),
            w => w,
        }
    }
}

fn highlighted(h: &Highlight, at: Option<usize>, inner: &str) -> String {
    format!(
        "<span class=\"cmt{}\" data-thread=\"{}\"{}>{inner}</span>",
        if h.open { " cmt-open" } else { "" },
        esc(&h.id),
        at.map(|a| format!(" data-o=\"{a}\"")).unwrap_or_default()
    )
}

/// `s` (at `place` in the source) as HTML: escaped, in the highlights of the
/// threads it is part of (the innermost, where they overlap), and with its
/// place in the source when asked for.
fn esc_at(s: &str, place: Place, ctx: &Ctx) -> String {
    if s.is_empty() || !ctx.marks_text() {
        return esc(s);
    }
    let at = match place {
        Place::At(at) => at,
        Place::Within(a, b) => {
            return match ctx.comments.iter().rfind(|h| h.start < b && a < h.end) {
                Some(h) => highlighted(h, None, &esc(s)),
                None => esc(s),
            };
        }
    };
    let end = at + s.len();
    let mut cuts = vec![at, end];
    for h in ctx.comments {
        for p in [h.start, h.end] {
            if p > at && p < end && s.is_char_boundary(p - at) {
                cuts.push(p);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = String::with_capacity(s.len() + 32);
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let piece = esc(&s[a - at..b - at]);
        let offset = ctx.offsets.then_some(a);
        match ctx
            .comments
            .iter()
            .filter(|h| h.start <= a && b <= h.end)
            .max_by_key(|h| h.start)
        {
            Some(h) => out.push_str(&highlighted(h, offset, &piece)),
            None if ctx.offsets => out.push_str(&format!("<span data-o=\"{a}\">{piece}</span>")),
            None => out.push_str(&piece),
        }
    }
    out
}

/// Whether a link may be opened outside the app: only web and e-mail links,
/// never `javascript:`, `data:`, `file:` or custom protocol handlers.
pub fn safe_external(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:")
}

fn text_with_tags(s: &str, place: Place, ctx: &Ctx) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut last = 0;
    let mut k = 0;
    while k < chars.len() {
        let (i, c) = chars[k];
        let prev_ok = k == 0 || chars[k - 1].1.is_whitespace() || chars[k - 1].1 == '(';
        if c == '#' && prev_ok {
            let len: usize = s[i + 1..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                .map(char::len_utf8)
                .sum();
            if len > 0 && !s[i + 1..i + 1 + len].chars().all(|c| c.is_ascii_digit()) {
                out.push_str(&esc_at(&s[last..i], place.after(last), ctx));
                let tag = &s[i + 1..i + 1 + len];
                out.push_str(&format!(
                    "<a class=\"tg\" data-tag=\"{}\">{}</a>",
                    esc(tag),
                    esc_at(&s[i..i + 1 + len], place.after(i), ctx)
                ));
                last = i + 1 + len;
                while k < chars.len() && chars[k].0 < last {
                    k += 1;
                }
                continue;
            }
        }
        k += 1;
    }
    out.push_str(&esc_at(&s[last..], place.after(last), ctx));
    out
}

/// A code block: its header (title, language, copy button) and its lines,
/// one `span.cl` each (`chl` when highlighted, with its indentation guides
/// as an empty `span.igs`, its indentation and text in `span.ind` and
/// `span.lc`), numbered with CSS. The text of the `code` element is exactly
/// the code.
pub fn code_block(code: &str, info: &FenceInfo) -> String {
    let mut out = String::with_capacity(code.len() * 2 + 512);
    out.push_str("<div class=\"codeblock");
    if !info.numbers {
        out.push_str(" nonum");
    }
    if !info.has_head() {
        out.push_str(" bare");
    }
    match info.wrap {
        Some(true) => out.push_str(" wrap"),
        Some(false) => out.push_str(" nowrap"),
        None => {}
    }
    out.push_str("\"><div class=\"code-head\">");
    out.push_str(&code::head_html(info, true));
    out.push_str("</div><pre class=\"code\"><code>");
    let lines: Vec<&str> = code.split('\n').collect();
    let mut carry = Carry::None;
    let mut indent = Indent::default();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(if info.highlighted(i as u32 + 1) {
            "<span class=\"cl chl\">"
        } else {
            "<span class=\"cl\">"
        });
        let (guides, next) = indent.line(line, lines.get(i + 1).copied());
        indent = next;
        if info.guides {
            out.push_str(&code::guides_html(guides, false));
        }
        let mut flags = vec![0; line.len()];
        carry = code::highlight(line, &info.lang, carry, &mut flags);
        if !line.is_empty() {
            let st = syntax::Styled {
                flags,
                cells: syntax::code_cells(line),
                ..Default::default()
            };
            out.push_str(&syntax::line_html(line, &st));
        }
        // Lines are laid out as blocks; the line break is there for the text
        // (copying) only.
        if i + 1 < lines.len() {
            out.push_str("<span class=\"nl\">\n</span>");
        }
        out.push_str("</span>");
    }
    out.push_str("</code></pre></div>");
    out
}

/// A `==` in text: where its placeholder event is, its block, and whether it
/// may open (text follows) or close (text precedes) a highlight — the
/// editor's rule.
struct Mark {
    at: usize,
    block: usize,
    opens: bool,
    closes: bool,
}

fn inline_tag(t: &Tag) -> bool {
    matches!(
        t,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn inline_end(t: &TagEnd) -> bool {
    matches!(
        t,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

/// Text (at `place` in the source) as HTML, each `==` as a placeholder for
/// [`pair_marks`].
fn push_text<'a>(
    t: &str,
    place: Place,
    ctx: &Ctx,
    block: usize,
    out: &mut Vec<Event<'a>>,
    marks: &mut Vec<Mark>,
) {
    let mut rest = t;
    let mut done = 0;
    while let Some(i) = rest.find("==") {
        let (before, after) = (&rest[..i], &rest[i + 2..]);
        if !before.is_empty() {
            out.push(Event::Html(CowStr::from(text_with_tags(
                before,
                place.after(done),
                ctx,
            ))));
        }
        marks.push(Mark {
            at: out.len(),
            block,
            opens: after.chars().next().is_none_or(|c| !c.is_whitespace()),
            closes: before
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_whitespace()),
        });
        out.push(Event::Html(CowStr::from("==")));
        rest = after;
        done += i + 2;
    }
    if !rest.is_empty() {
        out.push(Event::Html(CowStr::from(text_with_tags(
            rest,
            place.after(done),
            ctx,
        ))));
    }
}

/// Pair `==` marks within each block into `<mark>…</mark>`; the rest stay
/// as they were typed.
fn pair_marks(marks: &[Mark], out: &mut [Event]) {
    let mut open: Option<&Mark> = None;
    for m in marks {
        if open.is_some_and(|o| o.block != m.block) {
            open = None;
        }
        match open {
            Some(o) if m.closes => {
                out[o.at] = Event::Html(CowStr::from("<mark>"));
                out[m.at] = Event::Html(CowStr::from("</mark>"));
                open = None;
            }
            _ if m.opens => open = Some(m),
            _ => {}
        }
    }
}

/// An inline element opened in a callout's title and still open where the
/// title's line ends (`**bold` there, `text**` on the next line): it is
/// closed there and opened again in the callout's text, so tags nest.
enum Reopen<'a> {
    Tag(Tag<'a>),
    /// A link, with its opening tag.
    Link(String),
}

/// The title of the callout being read: the start of its paragraph's first
/// line, up to that line's end. Its text starts at `from`; before is the
/// callout's type.
struct Title<'a> {
    from: usize,
    end: usize,
    /// What ends the title and starts the callout's text.
    close: &'static str,
    open: Vec<Reopen<'a>>,
}

/// The tags of a callout of `kind`, folding (open or not) or not: what
/// starts it and its title, what ends the title, what ends the callout.
fn callout_tags(kind: callout::Kind, fold: Option<bool>) -> (String, &'static str, &'static str) {
    let class = kind.class();
    let (open, close_title, close) = match fold {
        None => (
            format!("<div class=\"callout {class}\"><div class=\"co-title\">"),
            "</span></div><div class=\"co-body\">",
            "</div></div>",
        ),
        Some(open) => (
            format!(
                "<details class=\"callout {class}\"{}><summary class=\"co-title\">",
                if open { " open" } else { "" }
            ),
            "</span></summary><div class=\"co-body\">",
            "</div></details>",
        ),
    };
    (
        open + "<span class=\"co-icon\"></span><span class=\"co-name\">",
        close_title,
        close,
    )
}

pub fn to_html(src: &str, ctx: &Ctx) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_WIKILINKS);
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    let line_of = |off: usize| {
        src[..off.min(src.len())]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    };

    let mut out_events: Vec<Event> = Vec::new();
    let mut in_meta = false;
    let mut code: Option<(FenceInfo, String)> = None;
    let mut image: Option<(String, String, bool)> = None; // (target, alt, wiki)
    let mut heading_n = 0usize;
    let mut link_stack: Vec<bool> = Vec::new();
    // `==highlight==`: each `==` in text, paired up per block at the end.
    let mut marks: Vec<Mark> = Vec::new();
    let mut block = 0usize;
    // The current table's column alignments, and the next cell's column.
    let mut table_align: Vec<Alignment> = Vec::new();
    let mut column = 0usize;
    // The HTML block being read.
    let mut html_block: Option<String> = None;
    // The open block quotes: a callout's closing tags, or none.
    let mut quotes: Vec<Option<&'static str>> = Vec::new();
    let mut title: Option<Title> = None;
    let html_link = |dest: &str, attrs: &str| link_open(dest, false, attrs, ctx);
    let html_image = |src: &str, alt: &str, attrs: &str| image_html(src, alt, false, attrs, ctx);
    let urls = html::Urls {
        link: &html_link,
        image: &html_image,
    };

    let mut events = Parser::new_ext(src, opts).into_offset_iter().peekable();
    while let Some((ev, range)) = events.next() {
        if let Event::Start(t) = &ev
            && !inline_tag(t)
        {
            block += 1;
        }
        if let Event::End(t) = &ev
            && !inline_end(t)
        {
            block += 1;
        }
        if let Some(buf) = &mut html_block {
            match ev {
                Event::Html(h) => buf.push_str(&h),
                Event::End(TagEnd::HtmlBlock) => {
                    let src = html_block.take().unwrap();
                    out_events.push(Event::Html(CowStr::from(html::sanitize(&src, &urls))));
                }
                _ => {}
            }
            continue;
        }
        if let Some((_, alt, _)) = &mut image {
            match ev {
                Event::End(TagEnd::Image) => {
                    let (target, alt, wiki) = image.take().unwrap();
                    out_events.push(Event::Html(CowStr::from(image_html(
                        &target, &alt, wiki, "", ctx,
                    ))));
                }
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                _ => {}
            }
            continue;
        }
        if let Some((_, buf)) = &mut code {
            match ev {
                Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let (info, buf) = code.take().unwrap();
                    let html = code_block(buf.trim_end_matches('\n'), &info);
                    out_events.push(Event::Html(CowStr::from(html)));
                }
                _ => {}
            }
            continue;
        }
        // A callout's title ends with its line: what is still open in it is
        // closed, and opened again in the text after it.
        if let Some(t) = &title
            && (matches!(
                ev,
                Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph)
            ) || range.start >= t.end)
        {
            let t = title.take().unwrap();
            for r in t.open.iter().rev() {
                out_events.push(match r {
                    Reopen::Tag(tag) => Event::End(tag.to_end()),
                    Reopen::Link(_) => Event::Html(CowStr::from("</a>")),
                });
            }
            out_events.push(Event::Html(CowStr::from(t.close)));
            // `==` pairs only within the title, or the text.
            block += 1;
            if matches!(ev, Event::End(TagEnd::Paragraph)) {
                continue;
            }
            out_events.push(Event::Start(Tag::Paragraph));
            out_events.extend(t.open.into_iter().map(|r| match r {
                Reopen::Tag(tag) => Event::Start(tag),
                Reopen::Link(open) => Event::Html(CowStr::from(open)),
            }));
            if matches!(ev, Event::SoftBreak | Event::HardBreak) {
                continue;
            }
        }
        let ev = match (&mut title, ev) {
            // The type before the title.
            (Some(t), Event::Text(text)) if range.start < t.from => {
                if range.end <= t.from {
                    continue;
                }
                let cut = text.len().saturating_sub(range.end - t.from);
                Event::Text(CowStr::from(text.get(cut..).unwrap_or("").to_string()))
            }
            (Some(t), ev) => {
                match &ev {
                    // Links are followed below; images are read whole.
                    Event::Start(tag)
                        if inline_tag(tag)
                            && !matches!(tag, Tag::Link { .. } | Tag::Image { .. }) =>
                    {
                        t.open.push(Reopen::Tag(tag.clone()))
                    }
                    Event::End(end) if inline_end(end) && *end != TagEnd::Link => {
                        t.open.pop();
                    }
                    _ => {}
                }
                ev
            }
            (None, ev) => ev,
        };
        match ev {
            // A quote whose first line is `[!type]` is a callout.
            Event::Start(Tag::BlockQuote(kind)) => {
                let header = match events.peek() {
                    Some((Event::Start(Tag::Paragraph), p))
                        if !src[range.start..p.start].contains('\n') =>
                    {
                        let end = src[p.start..]
                            .find('\n')
                            .map_or(src.len(), |i| p.start + i)
                            .min(p.end);
                        callout::header(&src[p.start..end]).map(|h| (h, p.start, end))
                    }
                    _ => None,
                };
                let Some((h, start, end)) = header else {
                    quotes.push(None);
                    out_events.push(Event::Start(Tag::BlockQuote(kind)));
                    continue;
                };
                // Its first paragraph: the title, then the callout's text.
                events.next();
                block += 1;
                let (open, close_title, close) = callout_tags(h.kind, h.fold);
                out_events.push(Event::Html(CowStr::from(open)));
                if h.title.is_empty() {
                    let name = callout::default_title(h.name);
                    out_events.push(Event::Html(CowStr::from(esc(&name))));
                }
                quotes.push(Some(close));
                title = Some(Title {
                    from: start + h.title_at,
                    end,
                    close: close_title,
                    open: Vec::new(),
                });
            }
            Event::End(TagEnd::BlockQuote(kind)) => match quotes.pop().flatten() {
                Some(close) => out_events.push(Event::Html(CowStr::from(close))),
                None => out_events.push(Event::End(TagEnd::BlockQuote(kind))),
            },
            Event::Start(Tag::MetadataBlock(_)) => in_meta = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
            _ if in_meta => {}
            Event::Start(Tag::HtmlBlock) => html_block = Some(String::new()),
            Event::Html(h) | Event::InlineHtml(h) => {
                out_events.push(Event::InlineHtml(CowStr::from(html::sanitize(&h, &urls))))
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(l) => FenceInfo::parse(&l),
                    CodeBlockKind::Indented => FenceInfo::default(),
                };
                code = Some((info, String::new()));
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                ..
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                image = Some((dest_url.to_string(), String::new(), wiki));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let mut dest = dest_url.to_string();
                // `<a@b.c>` comes without its scheme; it is never a note.
                if link_type == LinkType::Email && !dest.starts_with("mailto:") {
                    dest = format!("mailto:{dest}");
                }
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                link_stack.push(true);
                let open = link_open(&dest, wiki, "", ctx);
                if let Some(t) = &mut title {
                    t.open.push(Reopen::Link(open.clone()));
                }
                out_events.push(Event::Html(CowStr::from(open)));
            }
            Event::End(TagEnd::Link) => {
                if let Some(t) = &mut title {
                    t.open.pop();
                }
                if link_stack.pop().is_some() {
                    out_events.push(Event::Html(CowStr::from("</a>")));
                }
            }
            Event::Start(Tag::Table(align)) => {
                table_align = align.clone();
                out_events.push(Event::Start(Tag::Table(align)));
            }
            e @ (Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow)) => {
                column = 0;
                out_events.push(e);
            }
            // A cell's content is in a block of its own, which CSS sizes (so
            // that a column keeps a readable width on a phone) and places by
            // the column's alignment, like the editor's cells.
            Event::Start(Tag::TableCell) => {
                let tc = match table_align.get(column) {
                    Some(Alignment::Center) => "<div class=\"tc ac\">",
                    Some(Alignment::Right) => "<div class=\"tc ar\">",
                    _ => "<div class=\"tc\">",
                };
                column += 1;
                out_events.push(Event::Start(Tag::TableCell));
                out_events.push(Event::Html(CowStr::from(tc)));
            }
            Event::End(TagEnd::TableCell) => {
                out_events.push(Event::Html(CowStr::from("</div>")));
                out_events.push(Event::End(TagEnd::TableCell));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = level as usize;
                out_events.push(Event::Html(CowStr::from(format!(
                    "<h{lvl} data-h=\"{heading_n}\">"
                ))));
                heading_n += 1;
            }
            Event::End(TagEnd::Heading(level)) => {
                out_events.push(Event::Html(CowStr::from(format!("</h{}>", level as usize))));
            }
            Event::TaskListMarker(checked) => {
                let line = line_of(range.start);
                let c = if checked { " checked" } else { "" };
                out_events.push(Event::Html(CowStr::from(format!(
                    "<input type=\"checkbox\" class=\"task\" data-task=\"{line}\"{c}> "
                ))));
            }
            Event::Text(t) => {
                let place = Place::text(src, &range, &t);
                if link_stack.is_empty() {
                    push_text(&t, place, ctx, block, &mut out_events, &mut marks);
                } else if ctx.marks_text() {
                    out_events.push(Event::Html(CowStr::from(esc_at(&t, place, ctx))));
                } else {
                    out_events.push(Event::Text(t));
                }
            }
            Event::Code(t) if ctx.marks_text() => {
                let place = Place::code(src, &range, &t);
                out_events.push(Event::Html(CowStr::from(format!(
                    "<code>{}</code>",
                    esc_at(&t, place, ctx)
                ))));
            }
            e => out_events.push(e),
        }
    }
    pair_marks(&marks, &mut out_events);
    let mut out = String::with_capacity(src.len() * 2);
    pulldown_cmark::html::push_html(&mut out, out_events.into_iter());
    out
}

/// The opening tag of a link to `dest` (a wiki link's target if `wiki`):
/// a note, a note to create, a web or e-mail link, or no link at all.
/// `attrs`: more attributes (escaped, each after a space).
fn link_open(dest: &str, wiki: bool, attrs: &str, ctx: &Ctx) -> String {
    if wiki || notekpr_core::markdown::is_internal_target(dest) {
        let target = notekpr_core::markdown::percent_decode(dest);
        let name = target.split('#').next().unwrap_or(&target).to_string();
        let heading = target
            .split_once('#')
            .map(|(_, h)| h.trim())
            .filter(|h| !h.is_empty())
            .map(|h| format!(" data-heading=\"{}\"", esc(h)))
            .unwrap_or_default();
        match (ctx.note)(&name) {
            Some(id) => format!(
                "<a class=\"wl\" href=\"#\" data-note=\"{}\"{heading}{attrs}>",
                esc(&id)
            ),
            None => format!(
                "<a class=\"wl wlb\" href=\"#\" data-create=\"{}\" title=\"Create this note\">",
                esc(&name)
            ),
        }
    } else if safe_external(dest) {
        format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer nofollow\"{attrs}>",
            esc(dest.trim())
        )
    } else {
        format!("<a{attrs}>")
    }
}

/// An embedded image: an attachment, a note (as a link), an external image
/// (as a link: not loaded) or a missing one. `attrs`: more attributes of an
/// attachment's `img` (escaped, each after a space).
fn image_html(target: &str, alt: &str, wiki: bool, attrs: &str, ctx: &Ctx) -> String {
    let target = notekpr_core::markdown::percent_decode(target);
    if !wiki && safe_external(&target) {
        let label = if alt.is_empty() {
            "external image"
        } else {
            alt
        };
        return format!(
            "<a class=\"extimg\" href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer nofollow\" title=\"External images are not loaded automatically\">🖼 {}</a>",
            esc(&target),
            esc(label)
        );
    }
    if let Some(id) = (ctx.attachment)(&target) {
        return format!(
            "<img class=\"att\" data-att=\"{}\" alt=\"{}\"{attrs}>",
            esc(&id),
            esc(alt)
        );
    }
    if let Some(id) = (ctx.note)(&target) {
        return format!(
            "<a class=\"wl embed\" href=\"#\" data-note=\"{}\">{}</a>",
            esc(&id),
            esc(&target)
        );
    }
    format!(
        "<span class=\"missing\">{}</span>",
        esc(&format!("missing: {target}"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(s: &str) -> String {
        let note = |t: &str| (t == "Known").then(|| "id1".to_string());
        let att = |t: &str| (t == "pic.png").then(|| "a1".to_string());
        to_html(s, &Ctx::new(&note, &att))
    }

    #[test]
    fn neutralises_html_and_bad_links() {
        let h = render(
            "<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>\n\n<iframe src=\"https://example.org\"></iframe>",
        );
        assert!(!h.contains("<script") && !h.contains("alert"), "{h}");
        assert!(!h.contains("onerror") && !h.contains("<img src"), "{h}");
        assert!(!h.contains("javascript:"), "{h}");
        assert!(!h.contains("iframe"), "{h}");
        let h = render("<div style=\"position:fixed\" class=\"wl\" data-note=\"id1\" id=x>a</div>");
        assert!(h.contains("<div>a</div>"), "{h}");
    }

    #[test]
    fn renders_allowed_html() {
        let h = render("Press <kbd>Ctrl</kbd>+<kbd>K</kbd>, H<sub>2</sub>O <!-- secret -->");
        assert!(
            h.contains("Press <kbd>Ctrl</kbd>+<kbd>K</kbd>, H<sub>2</sub>O"),
            "{h}"
        );
        assert!(!h.contains("secret"), "{h}");
        // HTML around Markdown, the way GitHub reads it.
        let h = render("<details>\n<summary>More</summary>\n\nSome **bold** text\n\n</details>");
        assert!(h.contains("<details>\n<summary>More</summary>"), "{h}");
        assert!(h.contains("<p>Some <strong>bold</strong> text</p>"), "{h}");
        assert!(h.contains("</details>"), "{h}");
        let h =
            render("<table>\n<tr><td colspan=\"2\" align=\"right\">a &amp; b</td></tr>\n</table>");
        assert!(
            h.contains("<td colspan=\"2\" align=\"right\">a &amp; b</td>"),
            "{h}"
        );
        // Code is code.
        assert!(render("`<b>x</b>`").contains("<code>&lt;b&gt;x&lt;/b&gt;</code>"));
    }

    #[test]
    fn html_links_and_images_follow_the_markdown_rules() {
        let h = render(
            "<a href=\"Known\">k</a> <a href=\"https://e.org\" title=\"t\">e</a> <a href=\"&#106;avascript:x\">j</a>",
        );
        assert!(
            h.contains("<a class=\"wl\" href=\"#\" data-note=\"id1\">k</a>"),
            "{h}"
        );
        assert!(
            h.contains("<a href=\"https://e.org\" target=\"_blank\" rel=\"noopener noreferrer nofollow\" title=\"t\">e</a>"),
            "{h}"
        );
        assert!(h.contains("<a>j</a>"), "{h}");
        let h = render(
            "<img src=\"pic.png\" width=\"200\" alt=\"p\"> <img src=\"https://e.org/x.png\">",
        );
        assert!(
            h.contains("<img class=\"att\" data-att=\"a1\" alt=\"p\" width=\"200\">"),
            "{h}"
        );
        assert!(
            h.contains("class=\"extimg\" href=\"https://e.org/x.png\""),
            "{h}"
        );
        assert!(!h.contains("<img src"), "{h}");
    }

    #[test]
    fn highlights_like_the_editor() {
        let html = render;
        assert!(html("a ==b c== d").contains("a <mark>b c</mark> d"));
        assert!(html("==**bold**==").contains("<mark><strong>bold</strong></mark>"));
        // Unpaired, or with spaces on the wrong side: as typed.
        assert!(html("a == b").contains("a == b"));
        assert!(html("x == y ==z").contains("x == y ==z"));
        // Not across paragraphs, not in code, not in link targets.
        let two = html("==one\n\ntwo==");
        assert!(!two.contains("<mark>"), "{two}");
        assert!(html("`==x==`").contains("<code>==x==</code>"));
        let link = html("[t](https://example.org/?a==b) ==hi==");
        assert!(
            link.contains("href=\"https://example.org/?a==b\""),
            "{link}"
        );
        assert!(link.contains("<mark>hi</mark>"), "{link}");
    }

    #[test]
    fn links_to_a_heading_keep_it() {
        let note = |s: &str| (s == "Plan").then(|| "n1".to_string());
        let attachment = |_: &str| None;
        let h = to_html("see [[Plan#Next steps]]", &Ctx::new(&note, &attachment));
        assert!(
            h.contains("data-note=\"n1\" data-heading=\"Next steps\""),
            "{h}"
        );
    }

    #[test]
    fn table_cells_hold_their_text_in_a_block_with_the_columns_alignment() {
        let h = render("| a | b | c |\n| --- | :-: | --: |\n| x **y** | 2 | 3 |\n| z |\n");
        assert!(h.contains("<th><div class=\"tc\">a</div></th>"), "{h}");
        assert!(h.contains("<div class=\"tc ac\">b</div>"), "{h}");
        assert!(h.contains("<div class=\"tc ar\">c</div>"), "{h}");
        assert!(
            h.contains("<td><div class=\"tc\">x <strong>y</strong></div></td>"),
            "{h}"
        );
        // Every row starts at the first column again.
        assert!(h.contains("<div class=\"tc ac\">2</div>"), "{h}");
        assert!(h.contains("<td><div class=\"tc\">z</div></td>"), "{h}");
        // (A short row is filled up with empty cells.)
        assert_eq!(h.matches("<div class=\"tc").count(), 9, "{h}");
    }

    #[test]
    fn email_autolinks_are_mail_links_not_notes() {
        let h = render("write to <ana@example.org>");
        assert!(h.contains("href=\"mailto:ana@example.org\""), "{h}");
        assert!(!h.contains("data-create"), "{h}");
    }

    #[test]
    fn only_web_and_mail_links_are_external() {
        for ok in ["https://x.org", " HTTP://x.org/a?b", "mailto:a@b.c"] {
            assert!(safe_external(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            " JavaScript:alert(1)",
            "data:text/html,<script>",
            "file:///etc/passwd",
            "vbscript:x",
            "ms-settings:",
            "steam://run/1",
            "//evil.example",
            "",
        ] {
            assert!(!safe_external(bad), "{bad}");
        }
    }

    #[test]
    fn wiki_tasks_tags_code() {
        let h = render(
            "- [ ] todo #home\n\n[[Known]] [[Missing]]\n\n![[pic.png]]\n\n```rust\nfn x() {}\n```\n",
        );
        assert!(h.contains("data-task=\"0\""));
        assert!(h.contains("data-tag=\"home\""));
        assert!(h.contains("data-note=\"id1\""));
        assert!(h.contains("data-create=\"Missing\""));
        assert!(h.contains("data-att=\"a1\""));
        assert!(h.contains("class=\"kw\""));
    }

    /// The text of a block's `code` element (tags removed, entities decoded).
    fn code_text(h: &str) -> String {
        let a = h.find("<code>").unwrap() + 6;
        let b = h[a..].find("</code>").unwrap() + a;
        let mut t = String::new();
        let mut tag = false;
        for c in h[a..b].chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => t.push(c),
                _ => {}
            }
        }
        t.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    #[test]
    fn code_blocks_with_options() {
        let src = "```bash title=search_keys.sh hl=2\n#!/bin/bash\nfor u in a b; do\n\n  echo \"$u\"\ndone\n```\n";
        let h = render(src);
        assert!(
            h.contains("<span class=\"code-title\">search_keys.sh</span>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"code-lang\">bash</span>"));
        assert!(h.contains("class=\"code-copy\""));
        assert_eq!(h.matches("class=\"cl chl\"").count(), 1);
        assert_eq!(h.matches("<span class=\"cl").count(), 5);
        assert!(!h.contains("nonum"));
        // Copying takes the text of the code element: exactly the code.
        assert_eq!(
            code_text(&h),
            "#!/bin/bash\nfor u in a b; do\n\n  echo \"$u\"\ndone"
        );

        let h = render("```python ln=false\nx = 1\n```\n");
        assert!(h.contains("codeblock nonum"));
        assert!(!h.contains("code-title"));
        let h = render("```\nplain <b>\n```\n\n    indented\n");
        assert_eq!(h.matches("codeblock bare").count(), 2);
        assert!(h.contains("plain &lt;b&gt;"));
        // The title is text, never markup.
        let h = render("```html title=\"<img src=x onerror=alert(1)>\"\n<p>\n```\n");
        assert!(!h.contains("<img"), "{h}");
        assert!(h.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn indentation_guides_in_the_reading_view() {
        let h = render("```yaml\na:\n  b:\n\n    c: 1\nd: 2\n```\n");
        assert_eq!(h.matches("class=\"igs ig1 iu2\"").count(), 2, "{h}");
        assert_eq!(h.matches("class=\"igs ig2 iu2\"").count(), 1, "{h}");
        assert!(!h.contains("data-w"));
        assert_eq!(code_text(&h), "a:\n  b:\n\n    c: 1\nd: 2");
        let h = render("```yaml vl=false\na:\n  b: 1\n```\n");
        assert!(!h.contains("igs"), "{h}");
        // Indentation and text in their own parts (wrapped rows start under the text).
        assert!(
            h.contains("<span class=\"ind\">  </span><span class=\"lc\">"),
            "{h}"
        );
        assert!(!h.contains("nowrap"));
        assert!(render("```yaml wrap=false\na\n```\n").contains("codeblock nowrap"));
        assert!(render("```yaml wrap\na\n```\n").contains("codeblock wrap"));
    }

    #[test]
    fn callouts() {
        // GitHub's alerts: the type's name is the title.
        let h = render("> [!NOTE]\n> Useful *information*.\n");
        assert_eq!(
            h,
            "<div class=\"callout callout-note\"><div class=\"co-title\"><span class=\"co-icon\"></span>\
             <span class=\"co-name\">Note</span></div><div class=\"co-body\">\n<p>Useful <em>information</em>.</p>\n\
             </div></div>"
        );
        // Obsidian's: any case, a title with Markdown, text without markers.
        let h = render("> [!Warning] Mind **the** [[Known|gap]]\nlazy text\n>\n> - [ ] item\n");
        assert!(
            h.starts_with("<div class=\"callout callout-warning\">"),
            "{h}"
        );
        assert!(
            h.contains("<span class=\"co-name\">Mind <strong>the</strong> <a class=\"wl\" href=\"#\" data-note=\"id1\">gap</a></span></div>"),
            "{h}"
        );
        assert!(
            h.contains("<div class=\"co-body\">\n<p>lazy text</p>"),
            "{h}"
        );
        assert!(h.contains("data-task=\"3\""), "{h}");
        assert!(!h.contains("[!") && !h.contains("blockquote"), "{h}");
        // Folding: open or closed at first; aliases and unknown types.
        let h = render("> [!faq]- Why?\n> Because.\n\n> [!tldr]+\n\n> [!recipe] Soup\n");
        assert!(
            h.contains("<details class=\"callout callout-question\"><summary class=\"co-title\"><span class=\"co-icon\"></span><span class=\"co-name\">Why?</span></summary>"),
            "{h}"
        );
        assert!(
            h.contains("<details class=\"callout callout-abstract\" open>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"co-name\">Tldr</span></summary><div class=\"co-body\"></div></details>"), "{h}");
        assert!(h.contains("callout callout-note\"><div class=\"co-title\"><span class=\"co-icon\"></span><span class=\"co-name\">Soup</span>"), "{h}");
        // Nested.
        let h = render("> [!info]\n> > [!bug] Inner\n> > x\n>\n> after\n");
        assert!(
            h.contains("<div class=\"co-body\"><div class=\"callout callout-bug\">"),
            "{h}"
        );
        assert!(
            h.contains(
                "Inner</span></div><div class=\"co-body\">\n<p>x</p>\n</div></div>\n<p>after</p>"
            ),
            "{h}"
        );
        // Elements across the title's line end are closed and opened again.
        let h = render("> [!tip] **a ==b\n> c== d** [x\n> y](https://e.org)\n");
        assert!(h.contains("<strong>a ==b</strong></span></div><div class=\"co-body\">\n<p><strong>c== d</strong>"), "{h}");
        let h = render("> [!tip] [x\n> y](https://e.org)\n");
        assert_eq!(h.matches("<a ").count(), 2, "{h}");
        assert_eq!(h.matches("</a>").count(), 2, "{h}");
        // Not callouts: not on the quote's first line, not a type, a link.
        for not in [
            "> a\n> [!note]",
            ">\n> [!note]",
            "> [!no te]",
            "> [!note](https://e.org)",
            "> \\[!note]",
        ] {
            let h = render(not);
            assert!(
                h.contains("<blockquote>") && !h.contains("callout"),
                "{not:?}: {h}"
            );
        }
        // The title is text: a type can't inject markup.
        let h = render("> [!x\"onclick=1]\n> [!<b>] t\n");
        assert!(!h.contains("callout") && !h.contains("onclick=1\""), "{h}");
    }

    fn render_with(s: &str, comments: &[Highlight], offsets: bool) -> String {
        let note = |t: &str| (t == "Known").then(|| "id1".to_string());
        let att = |_: &str| None;
        to_html(
            s,
            &Ctx {
                comments,
                offsets,
                ..Ctx::new(&note, &att)
            },
        )
    }

    fn hl(start: usize, end: usize, id: &str) -> Highlight {
        Highlight {
            start,
            end,
            id: id.into(),
            open: false,
        }
    }

    /// The text of every `data-o` element with its offset, as a browser
    /// would read it (entities decoded).
    fn offsets(h: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for part in h.split("data-o=\"").skip(1) {
            let (n, rest) = part.split_once('"').unwrap();
            let text = rest[1..].split('<').next().unwrap();
            let text = text
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&amp;", "&");
            out.push((n.parse().unwrap(), text));
        }
        out
    }

    #[test]
    fn comment_threads_are_highlighted_in_the_reading_view() {
        let src = "Buy **milk** and bread, #home `x`\n\n# Title";
        // "milk** and bread" (6..22, across the bold) and "Title" (37..42).
        let h = render_with(src, &[hl(6, 22, "t1"), hl(37, 42, "t2")], false);
        assert!(
            h.contains("<strong><span class=\"cmt\" data-thread=\"t1\">milk</span></strong><span class=\"cmt\" data-thread=\"t1\"> and bread</span>, "),
            "{h}"
        );
        assert!(
            h.contains("<h1 data-h=\"0\"><span class=\"cmt\" data-thread=\"t2\">Title</span></h1>"),
            "{h}"
        );
        // Open, nested and across a tag.
        let h = render_with(
            "ab #tag cd",
            &[
                Highlight {
                    open: true,
                    ..hl(0, 9, "o")
                },
                hl(1, 2, "i"),
            ],
            false,
        );
        assert!(h.contains("<span class=\"cmt cmt-open\" data-thread=\"o\">a</span><span class=\"cmt\" data-thread=\"i\">b</span>"), "{h}");
        assert!(
            h.contains(
                "data-tag=\"tag\"><span class=\"cmt cmt-open\" data-thread=\"o\">#tag</span></a>"
            ),
            "{h}"
        );
        // Without highlights and offsets, nothing changes.
        assert_eq!(render_with(src, &[], false), render(src));
        // Ids are escaped.
        let h = render_with("abc", &[hl(0, 1, "\"><b>")], false);
        assert!(!h.contains("<b>"), "{h}");
    }

    #[test]
    fn text_says_where_it_is_in_the_source() {
        let src = "A *b* [c](https://e.org) `d` ==e== \\* &amp; [[Known]]\n\n> [!note] T\n> u";
        let h = render_with(src, &[], true);
        for (at, text) in offsets(&h) {
            assert_eq!(&src[at..at + text.len()], text, "{h}");
        }
        let found: Vec<String> = offsets(&h).into_iter().map(|(_, t)| t).collect();
        for t in ["A ", "b", "c", "d", "e", "T", "u"] {
            assert!(found.iter().any(|f| f == t), "{t}: {found:?}");
        }
        // The escaped star, the character after its backslash.
        assert!(found.iter().any(|f| f.starts_with('*')), "{found:?}");
        // An entity isn't the source character for character: no offset.
        assert!(!found.iter().any(|f| f.contains('&')), "{found:?}");
        // Highlighted text has one too.
        let h = render_with("one two", &[hl(4, 7, "t")], true);
        assert!(h.contains("<span data-o=\"0\">one </span><span class=\"cmt\" data-thread=\"t\" data-o=\"4\">two</span>"), "{h}");
    }

    #[test]
    fn comments_span_lines_in_the_reading_view() {
        let h = render("```c\n/* one\ntwo */ int x;\n```\n");
        assert!(h.contains("<span class=\"cm\">two */</span>"), "{h}");
        assert!(h.contains("<span class=\"ty\">int</span>"), "{h}");
    }
}
