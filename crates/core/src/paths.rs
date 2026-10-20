//! Vault-relative paths. Always `/`-separated, never absolute, never containing
//! `.` or `..` segments. Notes end in `.md`.

pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_SEGMENT_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path is too long")]
    TooLong,
    #[error("'{0}' is not allowed as a path segment")]
    BadSegment(String),
    #[error("paths may not contain control characters")]
    ControlChar,
}

/// Normalise user input into a canonical vault path.
pub fn normalize(input: &str) -> Result<String, PathError> {
    if input.chars().any(|c| c.is_control()) {
        return Err(PathError::ControlChar);
    }
    let replaced = nfc(input).replace('\\', "/");
    let mut segs = Vec::new();
    for seg in replaced.split('/') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(PathError::BadSegment(seg.to_string()));
        }
        if seg.len() > MAX_SEGMENT_BYTES {
            return Err(PathError::TooLong);
        }
        segs.push(seg);
    }
    if segs.is_empty() {
        return Err(PathError::Empty);
    }
    let out = segs.join("/");
    if out.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }
    Ok(out)
}

/// One way to write each name: accented letters composed (Unicode NFC).
/// macOS hands out file names with accents as separate marks (NFD), and zip
/// files made there keep them; "Café" must be the same name either way.
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// Normalise a note path, appending `.md` if missing.
pub fn normalize_note(input: &str) -> Result<String, PathError> {
    let p = normalize(input)?;
    if is_note(&p) {
        Ok(p)
    } else {
        Ok(format!("{p}.md"))
    }
}

pub fn is_note(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".md")
}

/// Final path segment.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Display title: file name without `.md`.
pub fn title(path: &str) -> &str {
    let name = file_name(path);
    if is_note(name) {
        &name[..name.len() - 3]
    } else {
        name
    }
}

/// Parent folder ("" for the vault root).
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

pub fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// Whether `path` lies inside `folder` (at any depth).
pub fn is_within(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || (path.len() > folder.len()
            && path.starts_with(folder)
            && path.as_bytes()[folder.len()] == b'/')
}

/// Pick a path not in `taken` by appending " 2", " 3", ... before the extension.
pub fn unique(path: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(path) {
        return path.to_string();
    }
    let (stem, ext) = match file_name(path).rfind('.') {
        Some(i) if i > 0 => {
            let cut = path.len() - (file_name(path).len() - i);
            (&path[..cut], &path[cut..])
        }
        _ => (path, ""),
    };
    (2..)
        .map(|n| format!("{stem} {n}{ext}"))
        .find(|p| !taken(p))
        .expect("infinite iterator")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_are_written_one_way() {
        // "Café" with the accent as a separate mark (as macOS lists it).
        let decomposed = "Notes/Cafe\u{301}.md";
        assert_eq!(normalize(decomposed).unwrap(), "Notes/Caf\u{e9}.md");
        assert_eq!(normalize_note("Cafe\u{301}").unwrap(), "Caf\u{e9}.md");
        assert_eq!(normalize("Caf\u{e9}.md").unwrap(), "Caf\u{e9}.md");
        assert_eq!(nfc("Cafe\u{301}"), nfc("Caf\u{e9}"));
    }

    #[test]
    fn normalization() {
        assert_eq!(normalize(" a//b\\c.md ").unwrap(), "a/b/c.md");
        assert_eq!(normalize("/x/").unwrap(), "x");
        assert!(normalize("a/../b").is_err());
        assert!(normalize("").is_err());
        assert!(normalize("a\nb").is_err());
        assert_eq!(normalize_note("Ideas/New").unwrap(), "Ideas/New.md");
        assert_eq!(title("Ideas/New.md"), "New");
        assert_eq!(parent("Ideas/New.md"), "Ideas");
        assert_eq!(parent("New.md"), "");
        assert!(is_within("a/b/c.md", "a"));
        assert!(!is_within("ab/c.md", "a"));
        assert_eq!(
            unique("a/n.md", |p| p == "a/n.md" || p == "a/n 2.md"),
            "a/n 3.md"
        );
        assert_eq!(unique("folder", |p| p == "folder"), "folder 2");
    }
}
