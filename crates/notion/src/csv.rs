//! The CSV files Notion exports beside each database (RFC 4180, UTF-8 with
//! a byte order mark): every row, every property, as text.

/// The rows of `text`, the header first.
pub fn parse(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quotes_commas_and_lines() {
        let rows = parse(
            "\u{feff}Name,Tags\r\n\"@May 3, 2022 \",\"Daily, Work\"\r\n\"A \"\"b\"\"\",\"two\nlines\"\n",
        );
        assert_eq!(
            rows,
            vec![
                vec!["Name", "Tags"],
                vec!["@May 3, 2022 ", "Daily, Work"],
                vec!["A \"b\"", "two\nlines"],
            ]
        );
        assert_eq!(parse("a,b"), vec![vec!["a", "b"]]);
        assert!(parse("").is_empty());
    }
}
