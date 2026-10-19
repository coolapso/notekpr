//! Lightweight Markdown analysis for knowledge navigation: internal links
//! (wiki-style `[[...]]` and relative Markdown links), tags, headings and
//! frontmatter. Runs on trusted clients only, since the server never sees plaintext.

use crate::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Wiki,
    Markdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    /// Target as written, without heading/alias (e.g. `Folder/Note`).
    pub target: String,
    pub heading: Option<String>,
    pub alias: Option<String>,
    /// `![[...]]` / `![](...)`.
    pub embed: bool,
    /// Byte range of the whole link in the source.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub offset: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteInfo {
    pub links: Vec<Link>,
    pub tags: Vec<String>,
    pub headings: Vec<Heading>,
    /// Raw frontmatter body (between the `---` fences), if present.
    pub frontmatter: Option<String>,
}

fn is_fence(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Byte length of the leading frontmatter block (including fences), if any.
pub fn frontmatter_len(text: &str) -> Option<usize> {
    let first = text.lines().next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let mut off = first.len() + 1;
    for line in text[off.min(text.len())..].split_inclusive('\n') {
        if line.trim_end() == "---" || line.trim_end() == "..." {
            return Some((off + line.len()).min(text.len()));
        }
        off += line.len();
    }
    None
}

pub fn analyze(text: &str) -> NoteInfo {
    let mut info = NoteInfo::default();
    let mut body_start = 0;
    if let Some(len) = frontmatter_len(text) {
        let first_nl = text.find('\n').map(|i| i + 1).unwrap_or(len);
        let inner = &text[first_nl..len];
        let inner = inner.trim_end_matches(['\n', '\r']);
        let inner = inner
            .strip_suffix("---")
            .or_else(|| inner.strip_suffix("..."))
            .unwrap_or(inner);
        info.frontmatter = Some(inner.to_string());
        info.tags.extend(frontmatter_tags(inner));
        body_start = len;
    }
    let mut fence: Option<&str> = None;
    let mut offset = body_start;
    for raw in text[body_start..].split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let line_off = offset;
        offset += raw.len();
        if let Some(f) = fence {
            if line.trim_start().starts_with(f) {
                fence = None;
            }
            continue;
        }
        if let Some(f) = is_fence(line) {
            fence = Some(f);
            continue;
        }
        if line.starts_with("    ") || line.starts_with('\t') {
            // Indented code block (approximation: only outside lists matters rarely).
            continue;
        }
        if let Some(h) = heading(line) {
            info.headings.push(Heading {
                level: h.0,
                text: h.1.to_string(),
                offset: line_off,
            });
        }
        let masked = mask_inline_code(line);
        scan_links(&masked, line, line_off, &mut info.links);
        scan_tags(&masked, line, &mut info.tags);
    }
    let mut seen = std::collections::HashSet::new();
    info.tags.retain(|t| seen.insert(t.to_lowercase()));
    info
}

/// `(level, text)` for ATX headings.
pub fn heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let hashes = t.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    Some((hashes as u8, rest.trim().trim_end_matches('#').trim_end()))
}

/// Replace inline code spans with spaces so links/tags inside them are ignored,
/// keeping byte offsets intact.
fn mask_inline_code(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = line.as_bytes().to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let ticks = b[i..].iter().take_while(|c| **c == b'`').count();
            let open_end = i + ticks;
            // find closing run of equal length
            let mut j = open_end;
            let mut closed = None;
            while j < b.len() {
                if b[j] == b'`' {
                    let run = b[j..].iter().take_while(|c| **c == b'`').count();
                    if run == ticks {
                        closed = Some(j + run);
                        break;
                    }
                    j += run;
                } else {
                    j += 1;
                }
            }
            if let Some(end) = closed {
                // Blank out the whole span (any byte), preserving offsets.
                out[i..end].fill(b' ');
                i = end;
                continue;
            }
            i = open_end;
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

fn scan_links(masked: &str, orig: &str, line_off: usize, out: &mut Vec<Link>) {
    let b = masked.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'['
            && b.get(i + 1) == Some(&b'[')
            && let Some(close) = masked[i + 2..].find("]]")
        {
            let inner = &orig[i + 2..i + 2 + close];
            if !inner.is_empty() && !inner.contains('\n') && !inner.contains('[') {
                let embed = i > 0 && b[i - 1] == b'!';
                let (target_part, alias) = match inner.split_once('|') {
                    Some((t, a)) => (t, Some(a.trim().to_string())),
                    None => (inner, None),
                };
                let (target, heading) = match target_part.split_once('#') {
                    Some((t, h)) => (t.trim().to_string(), Some(h.trim().to_string())),
                    None => (target_part.trim().to_string(), None),
                };
                let start = if embed { i - 1 } else { i };
                out.push(Link {
                    kind: LinkKind::Wiki,
                    target,
                    heading,
                    alias,
                    embed,
                    start: line_off + start,
                    end: line_off + i + 2 + close + 2,
                });
                i = i + 2 + close + 2;
                continue;
            }
        }
        if b[i] == b']' && b.get(i + 1) == Some(&b'(') {
            // Find matching '[' backwards on this line.
            if let Some(open) = masked[..i].rfind('[')
                && let Some(close_rel) = masked[i + 2..].find(')')
            {
                let raw_target = orig[i + 2..i + 2 + close_rel].trim();
                let raw_target = raw_target
                    .split_once(' ')
                    .map(|(t, _)| t)
                    .unwrap_or(raw_target);
                let raw_target = raw_target.trim_start_matches('<').trim_end_matches('>');
                if is_internal_target(raw_target) {
                    let embed = open > 0 && b[open - 1] == b'!';
                    let decoded = percent_decode(raw_target);
                    let (target, heading) = match decoded.split_once('#') {
                        Some((t, h)) => (t.to_string(), Some(h.to_string())),
                        None => (decoded, None),
                    };
                    if !target.is_empty() {
                        out.push(Link {
                            kind: LinkKind::Markdown,
                            target,
                            heading,
                            alias: Some(orig[open + 1..i].to_string()),
                            embed,
                            start: line_off + if embed { open - 1 } else { open },
                            end: line_off + i + 2 + close_rel + 1,
                        });
                    }
                }
                i = i + 2 + close_rel + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// Whether a Markdown link target refers to another file in the vault.
pub fn is_internal_target(t: &str) -> bool {
    if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
        return false;
    }
    // Any URI scheme ("https:", "mailto:", "javascript:") is external.
    if let Some(colon) = t.find(':') {
        let scheme = &t[..colon];
        if !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            return false;
        }
    }
    true
}

pub fn percent_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        (c as char).to_digit(16).map(|d| d as u8)
    }
    // Works on bytes: `%` may be followed by anything, including the middle
    // of a multibyte character.
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                b.get(i + 1).and_then(|c| hex(*c)),
                b.get(i + 2).and_then(|c| hex(*c)),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '/'
}

fn scan_tags(masked: &str, orig: &str, out: &mut Vec<String>) {
    if heading(masked).is_some() {
        // Heading markers aren't tags, but tags inside heading text are.
    }
    let chars: Vec<(usize, char)> = masked.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if c != '#' {
            continue;
        }
        let prev_ok = k == 0 || {
            let p = chars[k - 1].1;
            p.is_whitespace() || p == '(' || p == ','
        };
        if !prev_ok {
            continue;
        }
        let rest = &orig[i + 1..];
        let tag: String = rest.chars().take_while(|c| is_tag_char(*c)).collect();
        let tag = tag.trim_end_matches('/').to_string();
        if !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) {
            out.push(tag);
        }
    }
}

fn frontmatter_tags(fm: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut in_list = false;
    for line in fm.lines() {
        let t = line.trim();
        if in_list {
            if let Some(item) = t.strip_prefix("- ") {
                tags.push(clean_tag(item));
                continue;
            }
            if t.is_empty() {
                continue;
            }
            in_list = false;
        }
        let Some(rest) = t.strip_prefix("tags:").or_else(|| t.strip_prefix("tag:")) else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            in_list = true;
        } else {
            let rest = rest.trim_start_matches('[').trim_end_matches(']');
            tags.extend(
                rest.split([',', ' '])
                    .filter(|s| !s.trim().is_empty())
                    .map(clean_tag),
            );
        }
    }
    tags.retain(|t| !t.is_empty());
    tags
}

fn clean_tag(s: &str) -> String {
    s.trim()
        .trim_matches(['"', '\''])
        .trim_start_matches('#')
        .to_string()
}

/// Normalised key for link resolution: lower-case, without `.md`.
fn link_key(s: &str) -> String {
    let s = s.trim().trim_start_matches("./").trim_start_matches('/');
    let s = if paths::is_note(s) {
        &s[..s.len() - 3]
    } else {
        s
    };
    s.to_lowercase()
}

/// Resolve a link from the note at `from_path` to one of `candidates`
/// (`(id, path)` pairs). Wiki links resolve like Obsidian: exact path first,
/// then by file name with the shortest path winning. Markdown links are
/// resolved relative to the linking note's folder first.
pub fn resolve<'a>(
    link: &Link,
    from_path: &str,
    candidates: &'a [(String, String)],
) -> Option<&'a str> {
    if link.target.is_empty() {
        // `[[#Heading]]` refers to the current note.
        return candidates
            .iter()
            .find(|(_, p)| p == from_path)
            .map(|(id, _)| id.as_str());
    }
    if link.kind == LinkKind::Markdown {
        let folder = paths::parent(from_path);
        let joined = join_relative(folder, &link.target);
        if let Some(joined) = joined {
            let key = link_key(&joined);
            if let Some((id, _)) = candidates.iter().find(|(_, p)| link_key(p) == key) {
                return Some(id);
            }
        }
    }
    let key = link_key(&link.target);
    if let Some((id, _)) = candidates.iter().find(|(_, p)| link_key(p) == key) {
        return Some(id);
    }
    let mut best: Option<&(String, String)> = None;
    for c in candidates {
        let k = link_key(&c.1);
        if k.ends_with(&format!("/{key}")) {
            best = match best {
                Some(b) if b.1.len() < c.1.len() || (b.1.len() == c.1.len() && b.1 <= c.1) => {
                    Some(b)
                }
                _ => Some(c),
            };
        }
    }
    best.map(|(id, _)| id.as_str())
}

fn join_relative(folder: &str, target: &str) -> Option<String> {
    let mut segs: Vec<&str> = if folder.is_empty() {
        vec![]
    } else {
        folder.split('/').collect()
    };
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            s => segs.push(s),
        }
    }
    Some(segs.join("/"))
}

/// Rewrite wiki links pointing at a renamed note. Returns `None` if nothing changed.
/// `resolves_to_renamed` decides, per link, whether it targeted the renamed note.
pub fn rewrite_links(
    text: &str,
    new_target: &str,
    mut resolves_to_renamed: impl FnMut(&Link) -> bool,
) -> Option<String> {
    let info = analyze(text);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut changed = false;
    for l in info.links.iter() {
        if !resolves_to_renamed(l) {
            continue;
        }
        let replacement = match l.kind {
            LinkKind::Wiki => {
                let mut s = String::new();
                if l.embed {
                    s.push('!');
                }
                s.push_str("[[");
                s.push_str(new_target);
                if let Some(h) = &l.heading {
                    s.push('#');
                    s.push_str(h);
                }
                if let Some(a) = &l.alias {
                    s.push('|');
                    s.push_str(a);
                }
                s.push_str("]]");
                s
            }
            LinkKind::Markdown => continue,
        };
        out.push_str(&text[last..l.start]);
        out.push_str(&replacement);
        last = l.end;
        changed = true;
    }
    if !changed {
        return None;
    }
    out.push_str(&text[last..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_any_input() {
        assert_eq!(
            percent_decode("Daily%20notes/2026%2D10.md"),
            "Daily notes/2026-10.md"
        );
        // `%` before multibyte characters or at the end must not panic.
        for s in ["notes/50%増.md", "a%a€b", "%", "%2", "100%", "%€€", "%%41"] {
            let _ = percent_decode(s);
        }
        assert_eq!(percent_decode("50%増"), "50%増");
        assert_eq!(percent_decode("%%41"), "%A");
        // Invalid UTF-8 after decoding falls back to the input.
        assert_eq!(percent_decode("%FF"), "%FF");
    }

    #[test]
    fn extracts_links_tags_headings() {
        let src = "---\ntags: [project, \"home\"]\n---\n# Title #notatag\nSee [[Other Note|alias]] and [[Folder/Deep#Sec]].\n![[image.png]] [doc](../Docs/My%20Doc.md) [web](https://x.y)\nA #tag and #nested/tag but not#this or #123.\n`[[code]] #code`\n```\n[[fenced]] #fenced\n```\n";
        let info = analyze(src);
        let targets: Vec<_> = info.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(
            targets,
            vec![
                "Other Note",
                "Folder/Deep",
                "image.png",
                "../Docs/My Doc.md"
            ]
        );
        assert_eq!(info.links[0].alias.as_deref(), Some("alias"));
        assert_eq!(info.links[1].heading.as_deref(), Some("Sec"));
        assert!(info.links[2].embed);
        assert_eq!(
            info.tags,
            vec!["project", "home", "notatag", "tag", "nested/tag"]
        );
        assert_eq!(info.headings[0].text, "Title #notatag");
        assert_eq!(
            &src[info.links[0].start..info.links[0].end],
            "[[Other Note|alias]]"
        );
    }

    #[test]
    fn resolution() {
        let c = vec![
            ("a".to_string(), "Note.md".to_string()),
            ("b".to_string(), "Deep/Folder/Note.md".to_string()),
            ("c".to_string(), "Folder/Other.md".to_string()),
            ("d".to_string(), "Docs/My Doc.md".to_string()),
        ];
        let wl = |t: &str| Link {
            kind: LinkKind::Wiki,
            target: t.into(),
            heading: None,
            alias: None,
            embed: false,
            start: 0,
            end: 0,
        };
        assert_eq!(resolve(&wl("note"), "x.md", &c), Some("a"));
        assert_eq!(resolve(&wl("Folder/Note"), "x.md", &c), Some("b"));
        assert_eq!(resolve(&wl("Other"), "x.md", &c), Some("c"));
        assert_eq!(resolve(&wl("Missing"), "x.md", &c), None);
        let ml = Link {
            kind: LinkKind::Markdown,
            target: "../Docs/My Doc.md".into(),
            ..wl("")
        };
        assert_eq!(resolve(&ml, "Folder/Other.md", &c), Some("d"));
    }

    #[test]
    fn rewrite() {
        let t = "a [[Old]] b [[old#H|x]] c [[Other]]";
        let r = rewrite_links(t, "New", |l| l.target.eq_ignore_ascii_case("old")).unwrap();
        assert_eq!(r, "a [[New]] b [[New#H|x]] c [[Other]]");
    }
}
