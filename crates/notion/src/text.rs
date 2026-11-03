//! Text as it goes into notes: names from titles, Markdown escaping, front
//! matter values, and the IDs and dates in Notion's links.

use notekpr_core::dates::Date;
use notekpr_core::paths;

/// Longest name made from a title, in bytes (paths allow 255 a segment;
/// this leaves room for ` 2.md` and keeps paths short).
pub const MAX_NAME: usize = 120;

/// A file or folder name for a page titled `title`: what file systems,
/// wiki links or tags can't hold (`/ \ : * ? " < > | # ^ [ ]`) left out,
/// spaces made single, no dots or spaces at either end, at most
/// [`MAX_NAME`] bytes; `Untitled` when nothing is left.
pub fn file_name(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        let c = if c.is_control() || "/\\:*?\"<>|#^[]".contains(c) {
            ' '
        } else {
            c
        };
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    let mut name = paths::nfc(out.trim_matches(|c: char| c == '.' || c.is_whitespace()));
    if name.len() > MAX_NAME {
        let mut cut = MAX_NAME;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
        name = name
            .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
            .to_string();
    }
    if name.is_empty() {
        "Untitled".into()
    } else {
        name
    }
}

/// A multi-select option as a tag: what a tag can't hold (spaces
/// included) becomes `-`. `None` when nothing is left.
pub fn tag(option: &str) -> Option<String> {
    let mut out = String::new();
    for c in option.trim().chars() {
        if c.is_alphanumeric() || c == '_' || c == '/' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    // `work / unplanned`: a nested tag, not `work-/-unplanned`.
    let out = out.replace("-/", "/").replace("/-", "/");
    let out = out.trim_matches(['-', '/']);
    (!out.is_empty()).then(|| out.to_string())
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The day in a date as Notion writes it in English, `November 13, 2023`
/// or `Nov 13, 2023`, with or without a mention's `@`.
pub fn english_date(s: &str) -> Option<Date> {
    let s = s.trim().trim_start_matches('@').trim();
    let mut words = s.split_whitespace();
    let month = words.next()?.to_lowercase();
    let day = words.next()?.strip_suffix(',')?.parse().ok()?;
    let year = words.next()?.parse().ok()?;
    if words.next().is_some() || month.len() < 3 {
        return None;
    }
    let m = MONTHS.iter().position(|full| full.starts_with(&month))? as u8 + 1;
    Date::new(year, m, day)
}

/// An export's `datetime` (`2023-11-10`, `2023-11-10T20:52`,
/// `2023-11-10T20:52:00.000+01:00`) to the minute: `2023-11-10T20:52`.
pub fn iso(datetime: &str) -> String {
    let datetime = datetime.trim();
    match datetime.split_once('T') {
        Some((day, time)) => format!("{day}T{}", time.get(..5).unwrap_or(time)),
        None => datetime.to_string(),
    }
}

/// An export's `datetime` as notekpr writes a date in a note:
/// `@2023-11-10`, or `@2023-11-10 20:52` with its time.
pub fn date_mention(datetime: &str) -> String {
    format!("@{}", iso(datetime).replacen('T', " ", 1))
}

/// `s` with its `%XX` escapes decoded (as UTF-8).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The Notion ID (32 hex digits, lower case) at the end of `s`: a UUID,
/// a file name without its extension (`Diary 9db7…`), or a link to a page
/// (`https://app.notion.com/p/Week-24-75e0…?pvs=21`).
pub fn notion_id(s: &str) -> Option<String> {
    let s = s
        .split(['?', '#'])
        .next()
        .unwrap_or(s)
        .trim_end_matches('/');
    let last = s.rsplit('/').next().unwrap_or(s);
    let compact: Vec<char> = last.chars().filter(|c| *c != '-').collect();
    let tail = compact.get(compact.len().checked_sub(32)?..)?;
    tail.iter()
        .all(char::is_ascii_hexdigit)
        .then(|| tail.iter().collect::<String>().to_ascii_lowercase())
}

/// Text written into Markdown so it reads as it did: what would start
/// formatting, a link, HTML or math gets a backslash. A `#` stays: a
/// hashtag in Notion becomes a tag.
pub fn escape(text: &str, out: &mut String) {
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let needs = match c {
            '\\' | '*' | '`' | '[' | ']' | '$' => true,
            // Inside a word (`snake_case`) it can't emphasize.
            '_' => !(word(prev) && word(next)),
            '~' => prev == Some('~') || next == Some('~'),
            '=' => prev == Some('=') || next == Some('='),
            '<' => next.is_some_and(|n| n.is_alphabetic() || "/!?".contains(n)),
            '&' => {
                let rest: String = chars[i + 1..].iter().take(32).collect();
                let name = rest.split(';').next().unwrap_or("");
                rest.contains(';')
                    && !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
            }
            _ => false,
        };
        if needs {
            out.push('\\');
        }
        out.push(c);
    }
}

/// `line` (Markdown) with a backslash before what would make it a heading,
/// quote, list item, rule or table at the start of a line.
pub fn escape_line_start(line: &str) -> String {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let numbered = digits > 0
        && line[digits..].starts_with(['.', ')'])
        && line[digits + 1..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
    if numbered {
        format!("{}\\{}", &line[..digits], &line[digits..])
    } else if line.starts_with(['#', '>', '-', '+', '=', '|']) {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

/// A run of backticks longer than any in `code`, and `code` between two of
/// them: an inline code span.
pub fn code_span(code: &str) -> String {
    let longest = longest_run(code, '`');
    let fence = "`".repeat(longest + 1);
    let pad = if code.starts_with('`') || code.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{code}{pad}{fence}")
}

/// The longest run of `c` in `s`.
pub fn longest_run(s: &str, c: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for ch in s.chars() {
        run = if ch == c { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A string as a YAML value: as it is where that reads back the same
/// string, double-quoted otherwise.
pub fn yaml(s: &str) -> String {
    if plain_yaml(s) {
        s.to_string()
    } else {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

fn plain_yaml(s: &str) -> bool {
    let lower = s.to_lowercase();
    !s.is_empty()
        && s == s.trim()
        && !s.starts_with(|c| "-?:,[]{}#&*!|>'\"%@`".contains(c))
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !s.chars().any(char::is_control)
        && !matches!(
            lower.as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "~"
        )
        && s.parse::<f64>().is_err()
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Whether `s` is a number as YAML writes one.
pub fn is_number(s: &str) -> bool {
    !s.is_empty()
        && s.parse::<f64>().is_ok()
        && s.chars().all(|c| c.is_ascii_digit() || ".-+eE".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_titles() {
        assert_eq!(
            file_name("Toobox: Read multiple flags"),
            "Toobox Read multiple flags"
        );
        assert_eq!(
            file_name("Toolbox: Add don / total count"),
            "Toolbox Add don total count"
        );
        assert_eq!(file_name("  [WIP] C# notes  "), "WIP C notes");
        assert_eq!(file_name("...hidden"), "hidden");
        assert_eq!(
            file_name("Infrastructure-vaas TF → TG"),
            "Infrastructure-vaas TF → TG"
        );
        assert_eq!(file_name(" ? "), "Untitled");
        let long = file_name(&"é".repeat(200));
        assert!(long.len() <= MAX_NAME && long.chars().all(|c| c == 'é'));
        assert_eq!(file_name("Cafe\u{301}"), "Café");
    }

    #[test]
    fn tags_from_options() {
        assert_eq!(tag("Daily OPS").as_deref(), Some("Daily-OPS"));
        assert_eq!(tag("work / unplanned").as_deref(), Some("work/unplanned"));
        assert_eq!(tag("Done ✅").as_deref(), Some("Done"));
        assert_eq!(tag(" - "), None);
    }

    #[test]
    fn dates() {
        assert_eq!(english_date("@November 13, 2023 "), Date::new(2023, 11, 13));
        assert_eq!(english_date("Sep 3, 2024"), Date::new(2024, 9, 3));
        assert_eq!(english_date("Week 24"), None);
        assert_eq!(english_date("Ma 3, 2024"), None);
        assert_eq!(iso("2023-11-10T20:52:00.000+01:00"), "2023-11-10T20:52");
        assert_eq!(date_mention("2023-11-10T20:52"), "@2023-11-10 20:52");
        assert_eq!(date_mention("2023-11-13"), "@2023-11-13");
    }

    #[test]
    fn ids_in_names_and_links() {
        let id = "75e0bc87959f46e5bd415e9afa6a1316";
        assert_eq!(
            notion_id("https://app.notion.com/p/Week-24-75e0bc87959f46e5bd415e9afa6a1316?pvs=21")
                .as_deref(),
            Some(id)
        );
        assert_eq!(
            notion_id("75e0bc87-959f-46e5-bd41-5e9afa6a1316").as_deref(),
            Some(id)
        );
        assert_eq!(
            notion_id("Week 24 75E0BC87959F46E5BD415E9AFA6A1316").as_deref(),
            Some(id)
        );
        assert_eq!(notion_id("Week 24"), None);
        assert_eq!(
            percent_decode("Diary/@May%2010,%202023%20x.html"),
            "Diary/@May 10, 2023 x.html"
        );
        assert_eq!(percent_decode("caf%C3%A9 100%"), "café 100%");
    }

    #[test]
    fn escaping() {
        let e = |s: &str| {
            let mut o = String::new();
            escape(s, &mut o);
            o
        };
        assert_eq!(e("snake_case and _this_"), "snake_case and \\_this\\_");
        assert_eq!(e("5 * 3 [x] `y` $4"), "5 \\* 3 \\[x\\] \\`y\\` \\$4");
        assert_eq!(e("~~a~~ ~b =c=="), "\\~\\~a\\~\\~ ~b =c\\=\\=");
        assert_eq!(e("a < b <tag> &amp; & co;"), "a < b \\<tag> \\&amp; & co;");
        assert_eq!(e("#hoje C#"), "#hoje C#");
        assert_eq!(escape_line_start("# not a heading"), "\\# not a heading");
        assert_eq!(escape_line_start("1. not a list"), "1\\. not a list");
        assert_eq!(escape_line_start("2024. A year"), "2024\\. A year");
        assert_eq!(escape_line_start("1.5 kg"), "1.5 kg");
        assert_eq!(code_span("a `b` c"), "``a `b` c``");
        assert_eq!(code_span("`x"), "`` `x ``");
    }

    #[test]
    fn yaml_values() {
        assert_eq!(yaml("Daily"), "Daily");
        assert_eq!(yaml("2023-11-10T20:52"), "\"2023-11-10T20:52\"");
        assert_eq!(yaml("[[Week 24]]"), "\"[[Week 24]]\"");
        assert_eq!(yaml("a: b"), "\"a: b\"");
        assert_eq!(yaml("yes"), "\"yes\"");
        assert_eq!(yaml("46"), "\"46\"");
        assert_eq!(yaml("say \"hi\"\n"), "\"say \\\"hi\\\"\\n\"");
        assert_eq!(yaml("🖥️ Weekly Planning"), "🖥️ Weekly Planning");
        assert!(is_number("46") && is_number("-1.5") && !is_number("inf") && !is_number("1,5"));
    }
}
