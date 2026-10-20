//! Wrapping a line of a note into rows that fit a width, keeping track of
//! where each character went (so cursors land in the right place).

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Characters `start..end` of the line.
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Break `line` at spaces (or inside a word too long for a row) so every row
/// is at most `max` wide by `measure`.
pub fn wrap(line: &str, max: f64, measure: impl Fn(&str) -> f64) -> Vec<Row> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![Row {
            start: 0,
            end: 0,
            text: String::new(),
        }];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        // The longest prefix that fits, at least one character.
        let mut end = start + 1;
        let mut last_space = None;
        while end < chars.len() {
            let candidate: String = chars[start..=end].iter().collect();
            if measure(&candidate) > max {
                break;
            }
            if chars[end] == ' ' {
                last_space = Some(end);
            }
            end += 1;
        }
        if end < chars.len()
            && let Some(sp) = last_space
        {
            end = sp + 1;
        }
        rows.push(Row {
            start,
            end,
            text: chars[start..end].iter().collect(),
        });
        start = end;
    }
    rows
}

/// The row and the column within it of character `col` (a cursor after
/// the last character sits at the end of the last row).
pub fn locate(rows: &[Row], col: usize) -> (usize, usize) {
    for (i, r) in rows.iter().enumerate() {
        if col < r.end || i == rows.len() - 1 {
            return (i, col.saturating_sub(r.start).min(r.end - r.start));
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width(s: &str) -> f64 {
        s.chars().count() as f64
    }

    #[test]
    fn breaks_at_spaces_and_inside_long_words() {
        let rows = wrap("the quick brown fox", 10.0, width);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["the quick ", "brown fox"]);
        let rows = wrap("abcdefghijkl", 5.0, width);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["abcde", "fghij", "kl"]);
        assert_eq!(wrap("", 5.0, width).len(), 1);
    }

    #[test]
    fn cursors_find_their_row() {
        let rows = wrap("the quick brown fox", 10.0, width);
        assert_eq!(locate(&rows, 0), (0, 0));
        assert_eq!(locate(&rows, 12), (1, 2));
        // After the last character.
        assert_eq!(locate(&rows, 19), (1, 9));
    }
}
