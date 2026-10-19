//! Reading view: Markdown → HTML that is safe by construction.
//!
//! * Raw HTML in notes is rendered as text, never as markup.
//! * Link targets are limited to http(s), mailto and in-vault references.
//! * External images are not loaded (privacy: no tracking pixels; the CSP
//!   blocks them anyway) and are shown as links instead.
//! * Every attribute we emit is escaped.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::editor::syntax;

pub struct Ctx<'a> {
    /// Resolve a link target (wiki or relative Markdown) to a note id.
    pub note: &'a dyn Fn(&str) -> Option<String>,
    /// Resolve an embed target to an attachment id.
    pub attachment: &'a dyn Fn(&str) -> Option<String>,
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    syntax::escape(s, &mut o);
    o
}

/// Whether a link may be opened outside the app: only web and e-mail links,
/// never `javascript:`, `data:`, `file:` or custom protocol handlers.
pub fn safe_external(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:")
}

fn text_with_tags(s: &str) -> String {
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
                out.push_str(&esc(&s[last..i]));
                let tag = &s[i + 1..i + 1 + len];
                out.push_str(&format!(
                    "<a class=\"tg\" data-tag=\"{}\">#{}</a>",
                    esc(tag),
                    esc(tag)
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
    out.push_str(&esc(&s[last..]));
    out
}

pub fn highlight_block(code: &str, lang: &str) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    let block = syntax::Block::Code {
        lang: lang.to_string(),
    };
    for (i, line) in code.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let st = syntax::style_line(line, &block, &|_| true);
        if line.is_empty() {
            continue;
        }
        let html = syntax::line_html(line, &st);
        out.push_str(&html);
    }
    out
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
    let mut code: Option<(String, String)> = None;
    let mut image: Option<(String, String, bool)> = None; // (target, alt, wiki)
    let mut heading_n = 0usize;
    let mut link_stack: Vec<bool> = Vec::new();

    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        if let Some((_, alt, _)) = &mut image {
            match ev {
                Event::End(TagEnd::Image) => {
                    let (target, alt, wiki) = image.take().unwrap();
                    out_events.push(Event::Html(CowStr::from(image_html(
                        &target, &alt, wiki, ctx,
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
                    let (lang, buf) = code.take().unwrap();
                    let body = highlight_block(buf.trim_end_matches('\n'), &lang);
                    let label = if lang.is_empty() {
                        String::new()
                    } else {
                        format!(" data-lang=\"{}\"", esc(&lang))
                    };
                    out_events.push(Event::Html(CowStr::from(format!(
                        "<pre class=\"code\"{label}><code>{body}</code></pre>"
                    ))));
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(Tag::MetadataBlock(_)) => in_meta = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
            _ if in_meta => {}
            Event::Html(h) | Event::InlineHtml(h) => out_events.push(Event::Text(h)),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
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
                let html = if matches!(link_type, LinkType::WikiLink { .. })
                    || coolpsonotes_core::markdown::is_internal_target(&dest)
                {
                    let target = coolpsonotes_core::markdown::percent_decode(&dest);
                    let name = target.split('#').next().unwrap_or(&target).to_string();
                    let heading = target
                        .split_once('#')
                        .map(|(_, h)| h.trim())
                        .filter(|h| !h.is_empty())
                        .map(|h| format!(" data-heading=\"{}\"", esc(h)))
                        .unwrap_or_default();
                    match (ctx.note)(&name) {
                        Some(id) => format!(
                            "<a class=\"wl\" href=\"#\" data-note=\"{}\"{heading}>",
                            esc(&id)
                        ),
                        None => format!(
                            "<a class=\"wl wlb\" href=\"#\" data-create=\"{}\" title=\"Create this note\">",
                            esc(&name)
                        ),
                    }
                } else if safe_external(&dest) {
                    format!(
                        "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">",
                        esc(dest.trim())
                    )
                } else {
                    "<a>".to_string()
                };
                link_stack.push(true);
                out_events.push(Event::Html(CowStr::from(html)));
            }
            Event::End(TagEnd::Link) => {
                if link_stack.pop().is_some() {
                    out_events.push(Event::Html(CowStr::from("</a>")));
                }
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
                if link_stack.is_empty() {
                    out_events.push(Event::Html(CowStr::from(text_with_tags(&t))));
                } else {
                    out_events.push(Event::Text(t));
                }
            }
            e => out_events.push(e),
        }
    }
    let mut out = String::with_capacity(src.len() * 2);
    pulldown_cmark::html::push_html(&mut out, out_events.into_iter());
    out
}

fn image_html(target: &str, alt: &str, wiki: bool, ctx: &Ctx) -> String {
    let target = coolpsonotes_core::markdown::percent_decode(target);
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
            "<img class=\"att\" data-att=\"{}\" alt=\"{}\">",
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
        to_html(
            s,
            &Ctx {
                note: &note,
                attachment: &att,
            },
        )
    }

    #[test]
    fn neutralises_html_and_bad_links() {
        let h = render(
            "<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>",
        );
        assert!(!h.contains("<script"));
        assert!(!h.contains("<img src=x"));
        assert!(!h.contains("javascript:"));
        assert!(h.contains("&lt;script&gt;"));
    }

    #[test]
    fn links_to_a_heading_keep_it() {
        let note = |s: &str| (s == "Plan").then(|| "n1".to_string());
        let attachment = |_: &str| None;
        let h = to_html(
            "see [[Plan#Next steps]]",
            &Ctx {
                note: &note,
                attachment: &attachment,
            },
        );
        assert!(
            h.contains("data-note=\"n1\" data-heading=\"Next steps\""),
            "{h}"
        );
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
}
