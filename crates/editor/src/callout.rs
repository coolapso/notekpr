//! Callouts: block quotes whose first line names a type in brackets — the
//! alerts of GitHub (`> [!NOTE]`) and the callouts of Obsidian, which add a
//! title after the type (`> [!warning] Mind the gap`) and `+` or `-` for a
//! callout that folds (open or closed at first). Types are case-insensitive.
//! Obsidian's aliases (`summary`, `faq`, `error`, …) look like the type they
//! stand for, and a type we don't know looks like a note, as in Obsidian.
//! GitHub's `important` and `caution` (aliases in Obsidian) keep GitHub's
//! looks, which are their own.
//!
//! The note stays plain Markdown: elsewhere a callout is at worst a quote.
//! The editor classifies callout lines in [`crate::syntax`]; the reading
//! view renders them in the web crate.

/// How a callout looks: its color and icon, set by CSS for [`Kind::class`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Note,
    Abstract,
    Info,
    Todo,
    Tip,
    Important,
    Success,
    Question,
    Warning,
    Caution,
    Failure,
    Danger,
    Bug,
    Example,
    Quote,
}

impl Kind {
    /// Every look, in the order the toolbar offers them.
    pub const ALL: [Kind; 15] = [
        Kind::Note,
        Kind::Abstract,
        Kind::Info,
        Kind::Todo,
        Kind::Tip,
        Kind::Important,
        Kind::Success,
        Kind::Question,
        Kind::Warning,
        Kind::Caution,
        Kind::Failure,
        Kind::Danger,
        Kind::Bug,
        Kind::Example,
        Kind::Quote,
    ];

    /// The look of the type `name`, in any case.
    pub fn of(name: &str) -> Kind {
        match name.to_lowercase().as_str() {
            "abstract" | "summary" | "tldr" => Kind::Abstract,
            "info" => Kind::Info,
            "todo" => Kind::Todo,
            "tip" | "hint" => Kind::Tip,
            "important" => Kind::Important,
            "success" | "check" | "done" => Kind::Success,
            "question" | "help" | "faq" => Kind::Question,
            "warning" | "attention" => Kind::Warning,
            "caution" => Kind::Caution,
            "failure" | "fail" | "missing" => Kind::Failure,
            "danger" | "error" => Kind::Danger,
            "bug" => Kind::Bug,
            "example" => Kind::Example,
            "quote" | "cite" => Kind::Quote,
            _ => Kind::Note,
        }
    }

    /// The type the toolbar writes for it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Abstract => "abstract",
            Kind::Info => "info",
            Kind::Todo => "todo",
            Kind::Tip => "tip",
            Kind::Important => "important",
            Kind::Success => "success",
            Kind::Question => "question",
            Kind::Warning => "warning",
            Kind::Caution => "caution",
            Kind::Failure => "failure",
            Kind::Danger => "danger",
            Kind::Bug => "bug",
            Kind::Example => "example",
            Kind::Quote => "quote",
        }
    }

    /// The CSS class that gives a callout (or a toolbar icon) this look.
    pub fn class(self) -> &'static str {
        match self {
            Kind::Note => "callout-note",
            Kind::Abstract => "callout-abstract",
            Kind::Info => "callout-info",
            Kind::Todo => "callout-todo",
            Kind::Tip => "callout-tip",
            Kind::Important => "callout-important",
            Kind::Success => "callout-success",
            Kind::Question => "callout-question",
            Kind::Warning => "callout-warning",
            Kind::Caution => "callout-caution",
            Kind::Failure => "callout-failure",
            Kind::Danger => "callout-danger",
            Kind::Bug => "callout-bug",
            Kind::Example => "callout-example",
            Kind::Quote => "callout-quote",
        }
    }

    /// The CSS class of a line of a callout nested in one of this kind (for
    /// the outer one's color around it).
    pub fn outer_class(self) -> &'static str {
        match self {
            Kind::Note => "in-note",
            Kind::Abstract => "in-abstract",
            Kind::Info => "in-info",
            Kind::Todo => "in-todo",
            Kind::Tip => "in-tip",
            Kind::Important => "in-important",
            Kind::Success => "in-success",
            Kind::Question => "in-question",
            Kind::Warning => "in-warning",
            Kind::Caution => "in-caution",
            Kind::Failure => "in-failure",
            Kind::Danger => "in-danger",
            Kind::Bug => "in-bug",
            Kind::Example => "in-example",
            Kind::Quote => "in-quote",
        }
    }
}

/// The first line of a callout (its text after the quote markers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header<'a> {
    /// The type as written.
    pub name: &'a str,
    pub kind: Kind,
    /// `Some(open)` for a callout that folds: `+` open at first, `-` closed.
    pub fold: Option<bool>,
    /// Where the title starts (at the end of the text without one), and the
    /// title.
    pub title_at: usize,
    pub title: &'a str,
}

/// `text`, a line's text after its quote markers, as the first line of a
/// callout: `[!type]`, maybe `+` or `-`, maybe a title after a space. Types
/// are letters, digits, `-` and `_`. `[!x](url)` is a link and `[!x]: url`
/// a link definition, so a title must come after white space.
pub fn header(text: &str) -> Option<Header<'_>> {
    let rest = text.strip_prefix("[!")?;
    let name = &rest[..rest.find(']')?];
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut at = 2 + name.len() + 1;
    let fold = match text[at..].chars().next() {
        Some('+') => Some(true),
        Some('-') => Some(false),
        _ => None,
    };
    if fold.is_some() {
        at += 1;
    }
    let after = &text[at..];
    if after.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let title = after.trim();
    let title_at = if title.is_empty() {
        text.len()
    } else {
        at + (after.len() - after.trim_start().len())
    };
    Some(Header {
        name,
        kind: Kind::of(name),
        fold,
        title_at,
        title,
    })
}

/// The title of a callout without one: its type with a capital first
/// letter (`[!TIP]` → "Tip"), as GitHub and Obsidian show it.
pub fn default_title(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut c = lower.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        let h = header("[!NOTE]").unwrap();
        assert_eq!(
            (h.name, h.kind, h.fold, h.title),
            ("NOTE", Kind::Note, None, "")
        );
        assert_eq!(h.title_at, 7);
        let h = header("[!faq]-  Why **not**?  ").unwrap();
        assert_eq!(
            (h.kind, h.fold, h.title),
            (Kind::Question, Some(false), "Why **not**?")
        );
        assert_eq!(&"[!faq]-  Why **not**?  "[h.title_at..], "Why **not**?  ");
        let h = header("[!Tip]+").unwrap();
        assert_eq!((h.kind, h.fold), (Kind::Tip, Some(true)));
        // Any type: unknown ones look like a note.
        assert_eq!(header("[!recipe] Soup").unwrap().kind, Kind::Note);
        assert_eq!(header("[!my-type_2]").unwrap().name, "my-type_2");
        for not in [
            "[!note](https://e.org)",
            "[!note]: https://e.org",
            "[!note][ref]",
            "[!note]title",
            "[!] x",
            "[!a b] x",
            "[!note",
            "[note] x",
            " [!note]",
            "text [!note]",
            "[!note]++",
        ] {
            assert_eq!(header(not), None, "{not:?}");
        }
    }

    #[test]
    fn looks_and_titles() {
        // Case-insensitive, Obsidian's aliases, GitHub's own two.
        assert_eq!(Kind::of("WARNING"), Kind::Warning);
        assert_eq!(Kind::of("tldr"), Kind::Abstract);
        assert_eq!(Kind::of("Error"), Kind::Danger);
        assert_eq!(Kind::of("CAUTION"), Kind::Caution);
        assert_eq!(Kind::of("Important"), Kind::Important);
        for k in Kind::ALL {
            assert_eq!(Kind::of(k.name()), k);
        }
        assert_eq!(default_title("TIP"), "Tip");
        assert_eq!(default_title("tldr"), "Tldr");
        assert_eq!(default_title("élan"), "Élan");
    }
}
