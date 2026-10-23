//! Dates in notes: `@2026-10-23`. Typing `@today`, `@tomorrow` or
//! `@yesterday` puts that day's date in its place — in the web editor as you
//! type, with the CLI and the gateway as they write — so the note keeps
//! saying which day it meant and stays plain Markdown. Apps show a date as
//! Today, Tomorrow or Yesterday while it is one of those, and as the date
//! otherwise ([`relative`]).

use std::fmt;

/// A calendar day (no time, no time zone: the day as the writer's device
/// saw it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

fn leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    /// The day, if there is such a day (years 1 to 9999).
    pub fn new(year: i32, month: u8, day: u8) -> Option<Date> {
        ((1..=9999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(year, month))
        .then_some(Date { year, month, day })
    }

    /// `YYYY-MM-DD`, exactly.
    pub fn parse(s: &str) -> Option<Date> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let num = |r: std::ops::Range<usize>| -> Option<u32> {
            let part = &s[r];
            part.bytes()
                .all(|c| c.is_ascii_digit())
                .then(|| part.parse().ok())
                .flatten()
        };
        Date::new(num(0..4)? as i32, num(5..7)? as u8, num(8..10)? as u8)
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm).
    pub fn days(self) -> i64 {
        let (m, d) = (i64::from(self.month), i64::from(self.day));
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The day `days` days after 1970-01-01.
    pub fn from_days(days: i64) -> Date {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Date { year, month, day }
    }

    pub fn add_days(self, n: i64) -> Date {
        Date::from_days(self.days() + n)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// The words that stand for a day, and how many days from today.
pub const KEYWORDS: [(&str, i64); 3] = [("today", 0), ("tomorrow", 1), ("yesterday", -1)];

/// Whether an `@` after `prev` may start a date: at the start, after a space
/// or an opening bracket, quote or emphasis — not inside a word, an e-mail
/// address or a URL.
pub fn starts_date(prev: Option<char>) -> bool {
    prev.is_none_or(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '(' | '[' | '{' | '"' | '\'' | '*' | '_' | '~' | '>' | '“' | '‘' | '«'
            )
    })
}

/// Whether a word may end before `next`.
fn ends_word(next: Option<char>) -> bool {
    next.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'))
}

/// A date written right after an `@`, at the start of `rest`: its length
/// and the day.
pub fn date_after_at(rest: &str) -> Option<(usize, Date)> {
    let d = Date::parse(rest.get(..10)?)?;
    ends_word(rest[10..].chars().next()).then_some((10, d))
}

/// `today`, `tomorrow` or `yesterday` (in any case) right after an `@`, at
/// the start of `rest`: its length and how many days from today.
pub fn keyword_after_at(rest: &str) -> Option<(usize, i64)> {
    KEYWORDS.iter().find_map(|&(w, off)| {
        let head = rest.get(..w.len())?;
        (head.eq_ignore_ascii_case(w) && ends_word(rest[w.len()..].chars().next()))
            .then_some((w.len(), off))
    })
}

/// The words `typed` after an `@` may still become (in any case): each
/// with how many days from today, in the order today, tomorrow, yesterday.
pub fn keywords_starting(typed: &str) -> Vec<(&'static str, i64)> {
    KEYWORDS
        .iter()
        .filter(|(w, _)| {
            w.len() >= typed.len()
                && w.as_bytes()[..typed.len()].eq_ignore_ascii_case(typed.as_bytes())
        })
        .copied()
        .collect()
}

/// How `date` reads on `today`: "Today", "Tomorrow" or "Yesterday", or
/// `None` on any other day (then it is shown as the date).
pub fn relative(date: Date, today: Date) -> Option<&'static str> {
    match date.days() - today.days() {
        0 => Some("Today"),
        1 => Some("Tomorrow"),
        -1 => Some("Yesterday"),
        _ => None,
    }
}

/// `text` with every `@today`, `@tomorrow` and `@yesterday` of its prose
/// replaced by that day's date, from `today`; not in code, HTML tags or the
/// frontmatter. `None` when it has none.
pub fn expand_keywords(text: &str, today: Date) -> Option<String> {
    use crate::markdown::{frontmatter_len, is_fence, mask_html, mask_inline_code};
    if !text.contains('@') {
        return None;
    }
    let mut found: Vec<(usize, usize, Date)> = Vec::new();
    let body = frontmatter_len(text).unwrap_or(0);
    let mut fence: Option<&str> = None;
    let mut offset = body;
    for raw in text[body..].split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let at = offset;
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
        if line.starts_with("    ") || line.starts_with('\t') || !line.contains('@') {
            continue;
        }
        let masked = mask_html(&mask_inline_code(line));
        let mut prev = None;
        for (i, c) in masked.char_indices() {
            if c == '@'
                && starts_date(prev)
                && let Some((len, off)) = keyword_after_at(&masked[i + 1..])
            {
                found.push((at + i, at + i + 1 + len, today.add_days(off)));
            }
            prev = Some(c);
        }
    }
    if found.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (a, b, d) in found {
        out.push_str(&text[last..a]);
        out.push('@');
        out.push_str(&d.to_string());
        last = b;
    }
    out.push_str(&text[last..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s).unwrap()
    }

    #[test]
    fn days_and_dates() {
        assert_eq!(d("1970-01-01").days(), 0);
        assert_eq!(d("2026-10-23").days(), 20_749);
        for s in [
            "2000-02-29",
            "1969-12-31",
            "2024-03-01",
            "0001-01-01",
            "9999-12-31",
        ] {
            assert_eq!(Date::from_days(d(s).days()), d(s), "{s}");
        }
        assert_eq!(d("2026-12-31").add_days(1), d("2027-01-01"));
        assert_eq!(d("2024-03-01").add_days(-1), d("2024-02-29"));
        assert_eq!(d("2026-10-23").to_string(), "2026-10-23");
        for bad in [
            "2026-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-1-01",
            "26-10-23",
            "2026-10-2x",
            "+026-10-23",
        ] {
            assert_eq!(Date::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn written_dates_and_words() {
        assert_eq!(
            date_after_at("2026-10-23, then"),
            Some((10, d("2026-10-23")))
        );
        assert_eq!(date_after_at("2026-10-23x"), None);
        assert_eq!(date_after_at("2026-10-23-1"), None);
        assert_eq!(keyword_after_at("Today!"), Some((5, 0)));
        assert_eq!(keyword_after_at("tomorrow"), Some((8, 1)));
        assert_eq!(keyword_after_at("yesterday."), Some((9, -1)));
        assert_eq!(keyword_after_at("todays"), None);
        assert_eq!(keyword_after_at("to"), None);
        assert!(starts_date(None) && starts_date(Some(' ')) && starts_date(Some('(')));
        assert!(!starts_date(Some('a')) && !starts_date(Some('/')) && !starts_date(Some('.')));
        let words = |t: &str| {
            keywords_starting(t)
                .iter()
                .map(|(w, _)| *w)
                .collect::<Vec<_>>()
        };
        assert_eq!(words(""), ["today", "tomorrow", "yesterday"]);
        assert_eq!(words("TO"), ["today", "tomorrow"]);
        assert_eq!(words("tom"), ["tomorrow"]);
        assert!(words("tomorrows").is_empty() && words("x").is_empty() && words("é").is_empty());
    }

    #[test]
    fn days_read_relative_to_today() {
        let today = d("2026-10-23");
        assert_eq!(relative(d("2026-10-23"), today), Some("Today"));
        assert_eq!(relative(d("2026-10-24"), today), Some("Tomorrow"));
        assert_eq!(relative(d("2026-10-22"), today), Some("Yesterday"));
        assert_eq!(relative(d("2026-10-25"), today), None);
        // Written as @tomorrow on the 23rd: Today on the 24th, Yesterday on
        // the 25th, the date after.
        let written = today.add_days(1);
        assert_eq!(relative(written, d("2026-10-24")), Some("Today"));
        assert_eq!(relative(written, d("2026-10-25")), Some("Yesterday"));
        assert_eq!(relative(written, d("2026-10-26")), None);
    }

    #[test]
    fn words_become_dates_in_prose_only() {
        let today = d("2026-10-23");
        let text = "---\ndue: @today\n---\nCall @Today, (@tomorrow) and *@yesterday*.\n\
                    `@today` in code, me@today.com, https://x.org/@today, @todays\n\
                    ```\n@today\n```\n    @today indented\n<a title=\"@today\">@tomorrow</a>\n";
        assert_eq!(
            expand_keywords(text, today).unwrap(),
            "---\ndue: @today\n---\nCall @2026-10-23, (@2026-10-24) and *@2026-10-22*.\n\
             `@today` in code, me@today.com, https://x.org/@today, @todays\n\
             ```\n@today\n```\n    @today indented\n<a title=\"@today\">@2026-10-24</a>\n"
        );
        assert_eq!(expand_keywords("nothing to do @2026-10-23", today), None);
        assert_eq!(
            expand_keywords("é @today ü", today).unwrap(),
            "é @2026-10-23 ü"
        );
    }
}
