//! The emoji offered as icons: every one, by group ([`table`], made by
//! `scripts/emoji.py` from Unicode's data), found by their names and
//! keywords.

use std::sync::OnceLock;

mod table;

/// One emoji, with its name and the words it is found by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emoji {
    pub emoji: &'static str,
    pub name: &'static str,
    /// Its name's words, lowercase.
    name_words: Vec<String>,
    /// The name without spaces or punctuation (`thumbs up` → `thumbsup`).
    compact: String,
    /// Its keywords' words, lowercase.
    words: Vec<&'static str>,
}

/// A group of emoji, as Unicode orders them.
pub struct Group {
    pub name: &'static str,
    pub emoji: Vec<Emoji>,
}

/// Every group, in order.
pub fn groups() -> &'static [Group] {
    static GROUPS: OnceLock<Vec<Group>> = OnceLock::new();
    GROUPS.get_or_init(|| {
        table::GROUPS
            .iter()
            .map(|(name, lines)| Group {
                name,
                emoji: lines.lines().filter_map(parse).collect(),
            })
            .collect()
    })
}

fn split(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| c.is_whitespace() || matches!(c, ':' | ',' | '-' | '(' | ')' | '_'))
        .filter(|w| !w.is_empty())
}

fn parse(line: &'static str) -> Option<Emoji> {
    let mut f = line.splitn(3, '|');
    let (emoji, name) = (f.next()?, f.next()?);
    let lower = name.to_lowercase();
    Some(Emoji {
        emoji,
        name,
        name_words: split(&lower).map(str::to_string).collect(),
        compact: lower.chars().filter(|c| c.is_alphanumeric()).collect(),
        words: f
            .next()
            .unwrap_or("")
            .split(' ')
            .filter(|w| !w.is_empty())
            .collect(),
    })
}

/// How well an emoji matches a search, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    /// Its name starts with what was typed (spaces aside: `thumbsup`).
    NameStart,
    /// Every word typed starts a word of its name.
    Name,
    /// … or of its keywords.
    Keyword,
}

impl Emoji {
    fn matches(&self, query: &[String], compact: &str) -> Option<Match> {
        if !compact.is_empty() && self.compact.starts_with(compact) {
            return Some(Match::NameStart);
        }
        let in_name = |q: &String| self.name_words.iter().any(|w| w.starts_with(q.as_str()));
        if query.iter().all(in_name) {
            return Some(Match::Name);
        }
        query
            .iter()
            .all(|q| in_name(q) || self.words.iter().any(|w| w.starts_with(q.as_str())))
            .then_some(Match::Keyword)
    }
}

/// The emoji found by `query`: those whose name or keywords have words
/// starting with each word of it, the best matches first (then in Unicode's
/// order). Shortcodes work too (`:thumbs_up:`).
pub fn search(query: &str) -> Vec<&'static Emoji> {
    let lower = query.trim().to_lowercase();
    let words: Vec<String> = split(&lower).map(str::to_string).collect();
    if words.is_empty() {
        return vec![];
    }
    // `thumbsup` for `thumbs up`, but not `1` for `+1`.
    let plain = lower
        .chars()
        .all(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, ':' | '_' | '-'));
    let compact: String = match plain {
        true => lower.chars().filter(|c| c.is_alphanumeric()).collect(),
        false => String::new(),
    };
    let mut found: Vec<(Match, &'static Emoji)> = groups()
        .iter()
        .flat_map(|g| &g.emoji)
        .filter_map(|e| Some((e.matches(&words, &compact)?, e)))
        .collect();
    // Stable: Unicode's order within each kind of match.
    found.sort_by_key(|(m, _)| *m);
    found.into_iter().map(|(_, e)| e).collect()
}

/// The name of an emoji the table has (ignoring variation selectors).
pub fn name_of(emoji: &str) -> Option<&'static str> {
    let bare = |s: &'_ str| s.chars().filter(|c| *c != '\u{fe0f}').collect::<Vec<_>>();
    let want = bare(emoji);
    groups()
        .iter()
        .flat_map(|g| &g.emoji)
        .find(|e| {
            e.emoji
                .chars()
                .filter(|c| *c != '\u{fe0f}')
                .eq(want.iter().copied())
        })
        .map(|e| e.name)
}

/// Buttons for `emoji` (with their names, if known), as HTML: grids of
/// hundreds are made this way at once instead of node by node. A click is
/// handled by their container (`data-e` holds the emoji).
pub fn buttons_html<'a>(emoji: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let mut out = String::new();
    for (e, name) in emoji {
        let e = esc(e);
        out.push_str("<button type=\"button\" class=\"emoji-btn\" data-e=\"");
        out.push_str(&e);
        out.push_str("\" aria-label=\"");
        out.push_str(&e);
        if let Some(n) = name {
            out.push_str("\" title=\"");
            out.push_str(&esc(n));
        }
        out.push_str("\">");
        out.push_str(&e);
        out.push_str("</button>");
    }
    out
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    crate::editor::syntax::escape(s, &mut o);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(q: &str) -> Vec<&'static str> {
        search(q).into_iter().map(|e| e.emoji).collect()
    }

    #[test]
    fn every_group_is_there_without_skin_tones() {
        let names: Vec<&str> = groups().iter().map(|g| g.name).collect();
        assert_eq!(names[0], "Smileys & Emotion");
        assert_eq!(names.last(), Some(&"Flags"));
        let all: Vec<&Emoji> = groups().iter().flat_map(|g| &g.emoji).collect();
        assert!(all.len() > 1800, "{}", all.len());
        for e in &all {
            assert!(!e.emoji.is_empty() && !e.name.is_empty(), "{e:?}");
            assert!(
                !e.emoji
                    .chars()
                    .any(|c| ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)),
                "no skin tones: {}",
                e.name
            );
            // Each fits as an icon.
            assert_eq!(
                notekpr_core::doc::clean_icon(e.emoji).unwrap().as_deref(),
                Some(e.emoji)
            );
        }
        assert_eq!(name_of("❤"), Some("red heart"), "without its selector too");
    }

    #[test]
    fn found_by_name_keyword_or_shortcode() {
        // The name first, then keywords.
        let dog = found("dog");
        assert_eq!(dog[0], "🐶", "{dog:?}");
        assert!(dog.contains(&"🐕"));
        // Several words: each starts a word.
        assert_eq!(found("thumbs up")[0], "👍");
        assert_eq!(found("thumbsup")[0], "👍", "without the space");
        assert_eq!(found(":thumbs_up:")[0], "👍", "as a shortcode");
        assert!(found("tada").contains(&"🎉"), "a keyword");
        assert_eq!(found("+1")[0], "👍", "punctuation counts");
        assert!(found("pet").contains(&"🐶"));
        assert!(found("PORTUGAL").contains(&"🇵🇹"), "any case");
        assert!(found("seedl").contains(&"🌱"), "the start of a word");
        assert!(found("eedling").is_empty(), "not the middle of one");
        assert!(found("   ").is_empty());
        assert!(found("zzzz").is_empty());
    }

    #[test]
    fn buttons_are_escaped() {
        let html = buttons_html([("🇹🇹", Some("flag: Trinidad & Tobago")), ("\"<x>", None)]);
        assert!(
            html.contains("title=\"flag: Trinidad &amp; Tobago\""),
            "{html}"
        );
        assert!(html.contains("data-e=\"&quot;&lt;x&gt;\""), "{html}");
        assert!(!html.contains("<x>"));
    }
}
