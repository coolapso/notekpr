//! Client-side full-text search. Plaintext only exists on trusted clients, so
//! indexing and matching happen there; the server is never involved.
//!
//! Query syntax: free terms (all must match), `"quoted phrases"`, and filters
//! `tag:x`, `folder:x` (alias `path:`), `vault:x`, `title:x`.

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<String>,
    pub tags: Vec<String>,
    pub folder: Option<String>,
    pub vault: Option<String>,
    pub title: Option<String>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.tags.is_empty()
            && self.folder.is_none()
            && self.title.is_none()
    }
}

fn fold(s: &str) -> Vec<char> {
    // One-to-one char folding keeps char indices aligned with the original.
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn fold_str(s: &str) -> String {
    fold(s).into_iter().collect()
}

pub fn parse_query(q: &str) -> Query {
    let mut out = Query::default();
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => {
                if quoted {
                    if !cur.is_empty() {
                        tokens.push(std::mem::take(&mut cur));
                    }
                } else if !cur.is_empty() && !cur.ends_with(':') {
                    tokens.push(std::mem::take(&mut cur));
                }
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    for t in tokens {
        if let Some(v) = t
            .strip_prefix("tag:")
            .or_else(|| t.strip_prefix('#').filter(|v| !v.is_empty()))
        {
            out.tags.push(fold_str(v.trim_start_matches('#')));
        } else if let Some(v) = t
            .strip_prefix("folder:")
            .or_else(|| t.strip_prefix("path:"))
        {
            out.folder = Some(fold_str(v.trim_matches('/')));
        } else if let Some(v) = t.strip_prefix("vault:") {
            out.vault = Some(fold_str(v));
        } else if let Some(v) = t.strip_prefix("title:") {
            out.title = Some(fold_str(v));
        } else {
            out.terms.push(fold_str(&t));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub score: f32,
    /// Excerpt around the first content match.
    pub snippet: String,
    /// Char ranges within `snippet` to highlight.
    pub highlights: Vec<(usize, usize)>,
}

fn find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn count(hay: &[char], needle: &[char], cap: usize) -> usize {
    let mut n = 0;
    let mut i = 0;
    while n < cap {
        match find(hay, needle, i) {
            Some(p) => {
                n += 1;
                i = p + needle.len();
            }
            None => break,
        }
    }
    n
}

/// Score one note against a query. `tags` are the note's tags.
pub fn score(q: &Query, path: &str, text: &str, tags: &[String]) -> Option<Hit> {
    let path_f = fold_str(path);
    if let Some(folder) = &q.folder
        && !(path_f.starts_with(&format!("{folder}/")) || path_f.starts_with(folder.as_str()))
    {
        return None;
    }
    let title = crate::paths::title(path);
    let title_f = fold(title);
    if let Some(t) = &q.title {
        find(&title_f, &fold(t), 0)?;
    }
    let note_tags: Vec<String> = tags.iter().map(|t| fold_str(t)).collect();
    for want in &q.tags {
        if !note_tags
            .iter()
            .any(|t| t == want || t.starts_with(&format!("{want}/")))
        {
            return None;
        }
    }
    let text_f = fold(text);
    let path_c: Vec<char> = path_f.chars().collect();
    let mut score = 1.0f32;
    let mut first_match: Option<(usize, usize)> = None;
    for term in &q.terms {
        let tc: Vec<char> = term.chars().collect();
        let in_title = count(&title_f, &tc, 5);
        let in_path = count(&path_c, &tc, 5);
        let in_text = count(&text_f, &tc, 50);
        if in_title + in_path + in_text == 0 {
            return None;
        }
        score += in_title as f32 * 10.0 + in_path as f32 * 3.0 + (in_text as f32).sqrt() * 2.0;
        if find(&title_f, &tc, 0) == Some(0) {
            score += 5.0;
        }
        if first_match.is_none()
            && let Some(p) = find(&text_f, &tc, 0)
        {
            first_match = Some((p, tc.len()));
        }
    }
    let orig: Vec<char> = text.chars().collect();
    let (snippet, highlights) = match first_match {
        Some((p, _)) => {
            let start = p.saturating_sub(60);
            let end = (p + 140).min(orig.len());
            let mut s: String = orig[start..end].iter().collect();
            s = s.replace('\n', " ");
            let snippet_f = fold(&s);
            let mut hl = Vec::new();
            for term in &q.terms {
                let tc: Vec<char> = term.chars().collect();
                let mut i = 0;
                while let Some(m) = find(&snippet_f, &tc, i) {
                    hl.push((m, m + tc.len()));
                    i = m + tc.len();
                }
            }
            hl.sort();
            let prefix = if start > 0 { "…" } else { "" };
            let shift = prefix.chars().count();
            let suffix = if end < orig.len() { "…" } else { "" };
            (
                format!("{prefix}{s}{suffix}"),
                hl.into_iter()
                    .map(|(a, b)| (a + shift, b + shift))
                    .collect(),
            )
        }
        None => {
            let body = crate::markdown::frontmatter_len(text)
                .map(|n| &text[n..])
                .unwrap_or(text);
            let s: String = body
                .chars()
                .take(140)
                .collect::<String>()
                .replace('\n', " ");
            (s, vec![])
        }
    };
    Some(Hit {
        score,
        snippet,
        highlights,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let q = parse_query(r#"rust "sync engine" tag:Work folder:Projects/ #idea vault:home"#);
        assert_eq!(q.terms, vec!["rust", "sync engine"]);
        assert_eq!(q.tags, vec!["work", "idea"]);
        assert_eq!(q.folder.as_deref(), Some("projects"));
        assert_eq!(q.vault.as_deref(), Some("home"));
    }

    #[test]
    fn scoring_and_filters() {
        let q = parse_query("sync");
        let a = score(&q, "Sync design.md", "about sync", &[]).unwrap();
        let b = score(&q, "Other.md", "talks about sync here", &[]).unwrap();
        assert!(a.score > b.score);
        assert!(score(&q, "x.md", "nothing", &[]).is_none());
        let q = parse_query("tag:work");
        assert!(score(&q, "x.md", "", &["Work/Meetings".into()]).is_some());
        assert!(score(&q, "x.md", "", &["home".into()]).is_none());
        let q = parse_query("folder:projects engine");
        assert!(score(&q, "Projects/a.md", "the engine", &[]).is_some());
        assert!(score(&q, "Notes/a.md", "the engine", &[]).is_none());
        let hit = score(&parse_query("ENGINE"), "a.md", "The Engine room", &[]).unwrap();
        assert_eq!(hit.highlights, vec![(4, 10)]);
    }
}
