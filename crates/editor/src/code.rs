//! Code blocks: what an opening fence says, and syntax highlighting.
//!
//! A fence's info string is a language followed by options, e.g.
//! ```` ```bash title=deploy.sh hl=2,4-6 ln=false ````: `title` names the
//! file, `hl` highlights lines, `ln=false` hides the line numbers,
//! `vl=false` the indentation guides (vertical lines) and `wrap=false` /
//! `wrap=true` decide whether long lines wrap. Options
//! are `key=value` or `key:value` (values may be quoted, `title="a b.sh"`);
//! unknown ones are ignored, so notes written for other editors keep working.
//!
//! Highlighting works a line at a time, like the rest of the editor: a line is
//! highlighted from the [`Carry`] the line above left (an open block comment
//! or a multi-line string) and returns the carry for the line below. The
//! editor keeps it in its line state, so typing `/*` re-highlights the lines
//! below until the carry is what it was before.
//!
//! The highlighter is hand-written and small on purpose: a table of words per
//! language and a few rules (comments, strings, numbers, calls, keys), plus
//! dedicated scanners where a language works differently (shells,
//! PowerShell, HTML, CSS, diffs, Makefiles).

use crate::syntax::{
    COMMENT, CONST, DEL, FUNC, INS, KW, META, NUM, OP, PROP, STR, TYPE, VAR, escape,
};

/// The options of an opening fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FenceInfo {
    /// The language (the first word), as written.
    pub lang: String,
    /// `title=…`: usually a file name, shown in the block's header.
    pub title: Option<String>,
    /// `hl=…`: highlighted lines (1-based, inclusive ranges).
    pub hl: Vec<(u32, u32)>,
    /// Line numbers, unless `ln=false`.
    pub numbers: bool,
    /// Indentation guides, unless `vl=false`.
    pub guides: bool,
    /// `wrap=true` / `wrap=false`: whether long lines wrap (`None`: as the
    /// reader's setting says).
    pub wrap: Option<bool>,
}

impl Default for FenceInfo {
    fn default() -> Self {
        FenceInfo {
            lang: String::new(),
            title: None,
            hl: Vec::new(),
            numbers: true,
            guides: true,
            wrap: None,
        }
    }
}

/// The words of an info string: `(key, value)`, the key `None` for a bare word.
fn info_words(info: &str) -> Vec<(Option<&str>, &str)> {
    let b = info.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
            i += 1;
        }
        let key = (i > start && i < n && matches!(b[i], b'=' | b':')).then(|| &info[start..i]);
        if key.is_some() {
            i += 1;
        } else {
            i = start;
        }
        let vs = i;
        let value = if i < n && matches!(b[i], b'"' | b'\'') {
            let q = b[i];
            let end = info[i + 1..].find(q as char).map(|k| i + 1 + k);
            match end {
                Some(e) => {
                    i = e + 1;
                    &info[vs + 1..e]
                }
                None => {
                    i = n;
                    &info[vs + 1..]
                }
            }
        } else {
            while i < n && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            &info[vs..i]
        };
        out.push((key, value));
    }
    out
}

/// `false`, `off`, `no`, `0` or `none`.
fn off(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "false" | "off" | "no" | "0" | "none"
    )
}

fn parse_lines(v: &str) -> Vec<(u32, u32)> {
    v.trim_matches(|c| c == '{' || c == '}')
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            match part.split_once('-') {
                Some((a, b)) => {
                    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                    (a <= b).then_some((a, b))
                }
                None => part.parse().ok().map(|a| (a, a)),
            }
        })
        .collect()
}

impl FenceInfo {
    pub fn parse(info: &str) -> FenceInfo {
        let mut out = FenceInfo::default();
        for (k, (key, value)) in info_words(info).into_iter().enumerate() {
            match key.map(str::to_ascii_lowercase).as_deref() {
                None if k == 0 && !matches!(value, "wrap" | "nowrap" | "unwrap") => {
                    out.lang = value.to_string()
                }
                // Obsidian's bare words.
                None if value == "wrap" => out.wrap = Some(true),
                None if matches!(value, "nowrap" | "unwrap") => out.wrap = Some(false),
                Some("title") if !value.trim().is_empty() => {
                    out.title = Some(value.trim().to_string())
                }
                Some("hl" | "highlight") => out.hl.extend(parse_lines(value)),
                Some("ln" | "linenos" | "numbers") => out.numbers = !off(value),
                Some("vl" | "guides") => out.guides = !off(value),
                Some("wrap") => out.wrap = Some(!off(value)),
                _ => {}
            }
        }
        out
    }

    /// Whether line `n` (1-based) of the block is highlighted.
    pub fn highlighted(&self, n: u32) -> bool {
        self.hl.iter().any(|&(a, b)| (a..=b).contains(&n))
    }

    /// Whether the block shows a header (a title or a language).
    pub fn has_head(&self) -> bool {
        self.title.is_some() || !self.lang.is_empty()
    }
}

const COPY_ICON: &str = r#"<svg class="ic i-copy" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>"#;
const CHECK_ICON: &str = r#"<svg class="ic i-ok" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5L20 7"/></svg>"#;

/// The inside of a block's header: its title, its language and a copy
/// button (`.code-copy`). The editor and the reading view wrap it.
pub fn head_html(info: &FenceInfo, focusable: bool) -> String {
    let mut h = String::new();
    if let Some(t) = &info.title {
        h.push_str("<span class=\"code-title\">");
        escape(t, &mut h);
        h.push_str("</span>");
    }
    if !info.lang.is_empty() {
        h.push_str("<span class=\"code-lang\">");
        escape(&info.lang, &mut h);
        h.push_str("</span>");
    }
    h.push_str(
        "<button class=\"code-copy\" type=\"button\" title=\"Copy code\" aria-label=\"Copy code\"",
    );
    if !focusable {
        h.push_str(" tabindex=\"-1\"");
    }
    h.push('>');
    h.push_str(COPY_ICON);
    h.push_str(CHECK_ICON);
    h.push_str("</button>");
    h
}

// ---------------------------------------------------------------- highlighting

/// What a line of code leaves open for the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Carry {
    #[default]
    None,
    /// Inside the language's block comment number `k`.
    Comment(u8),
    /// Inside its string kind number `k`.
    Str(u8),
    /// Inside an HTML tag (attributes on several lines).
    Tag,
    /// After a shell line ending in `\`: the command goes on.
    Cont,
}

struct Quote {
    open: &'static str,
    close: &'static str,
    /// Escape character (0: none).
    escape: u8,
    /// May span lines.
    multi: bool,
}

const fn q(open: &'static str, close: &'static str, escape: u8, multi: bool) -> Quote {
    Quote {
        open,
        close,
        escape,
        multi,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Generic,
    Rust,
    C,
    Shell,
    Console,
    PowerShell,
    Html,
    Css,
    Diff,
    Dockerfile,
    Make,
}

struct Lang {
    kind: Kind,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    builtins: &'static [&'static str],
    constants: &'static [&'static str],
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    quotes: &'static [Quote],
    /// Words match whatever their case.
    nocase: bool,
    /// Comments start only at the start of a word (`$#`, `a#b`,
    /// `http://` and `src/*.rs` aren't comments).
    hash_word: bool,
    /// `name(` is a call.
    calls: bool,
    /// `ALL_CAPS` is a constant, `Capitalized` a type.
    caps: bool,
    /// Operator characters are coloured.
    ops: bool,
    /// `key:` (or `key =`) at the start of a line is a key.
    key_sep: u8,
    /// A string followed by `:` is a key (JSON, YAML).
    string_keys: bool,
    /// `[section]` lines (TOML, INI).
    sections: bool,
    /// `@name` is an annotation or decorator.
    at_meta: bool,
    /// `$name` is a variable.
    dollar: bool,
}

const BASE: Lang = Lang {
    kind: Kind::Generic,
    keywords: &[],
    types: &[],
    builtins: &[],
    constants: &[],
    line: &[],
    block: &[],
    quotes: &[],
    nocase: false,
    hash_word: false,
    calls: true,
    caps: false,
    ops: true,
    key_sep: 0,
    string_keys: false,
    sections: false,
    at_meta: false,
    dollar: false,
};

const SLASH: &[&str] = &["//"];
const HASH: &[&str] = &["#"];
const C_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
const C_QUOTES: &[Quote] = &[q("\"", "\"", b'\\', false), q("'", "'", b'\\', false)];

const PLAIN: Lang = Lang {
    kind: Kind::Plain,
    calls: false,
    ops: false,
    ..BASE
};

const GENERIC: Lang = Lang {
    constants: &["true", "false", "null", "nil", "None", "True", "False"],
    line: &["//", "#"],
    block: C_BLOCK,
    quotes: &[q("\"", "\"", b'\\', false)],
    hash_word: true,
    ops: false,
    ..BASE
};

const RUST: Lang = Lang {
    kind: Kind::Rust,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while", "yield",
    ],
    types: &[
        "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
        "i64", "i128", "isize", "f32", "f64",
    ],
    constants: &["true", "false"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("r#\"", "\"#", 0, true),
        q("r\"", "\"", 0, true),
        q("\"", "\"", b'\\', true),
    ],
    caps: true,
    ..BASE
};

const C: Lang = Lang {
    kind: Kind::C,
    keywords: &[
        "break",
        "case",
        "catch",
        "class",
        "const",
        "constexpr",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "explicit",
        "extern",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "operator",
        "private",
        "protected",
        "public",
        "register",
        "return",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "typedef",
        "typename",
        "union",
        "using",
        "virtual",
        "volatile",
        "while",
        "override",
        "static_cast",
        "dynamic_cast",
        "reinterpret_cast",
        "const_cast",
    ],
    types: &[
        "auto", "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned",
        "void", "size_t", "ssize_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t",
        "uint16_t", "uint32_t", "uint64_t", "wchar_t", "string", "vector",
    ],
    constants: &["true", "false", "NULL", "nullptr"],
    line: SLASH,
    block: C_BLOCK,
    quotes: C_QUOTES,
    caps: true,
    ..BASE
};

const CSHARP: Lang = Lang {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "break",
        "case",
        "catch",
        "checked",
        "class",
        "const",
        "continue",
        "default",
        "delegate",
        "do",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "finally",
        "fixed",
        "for",
        "foreach",
        "get",
        "goto",
        "if",
        "implicit",
        "in",
        "init",
        "interface",
        "internal",
        "is",
        "lock",
        "namespace",
        "new",
        "operator",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "ref",
        "return",
        "sealed",
        "set",
        "sizeof",
        "stackalloc",
        "static",
        "struct",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "unchecked",
        "unsafe",
        "using",
        "var",
        "virtual",
        "volatile",
        "when",
        "where",
        "while",
        "yield",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "object",
        "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
    ],
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("@\"", "\"", 0, true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    ..BASE
};

const JAVA: Lang = Lang {
    keywords: &[
        "abstract",
        "assert",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "interface",
        "native",
        "new",
        "package",
        "permits",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "sealed",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "var",
        "volatile",
        "while",
        "yield",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short", "void",
    ],
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const KOTLIN: Lang = Lang {
    keywords: &[
        "abstract",
        "annotation",
        "as",
        "break",
        "by",
        "catch",
        "class",
        "companion",
        "const",
        "constructor",
        "continue",
        "data",
        "do",
        "else",
        "enum",
        "external",
        "final",
        "finally",
        "for",
        "fun",
        "get",
        "if",
        "import",
        "in",
        "infix",
        "init",
        "inline",
        "inner",
        "interface",
        "internal",
        "is",
        "lateinit",
        "object",
        "open",
        "operator",
        "out",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "reified",
        "return",
        "sealed",
        "set",
        "super",
        "suspend",
        "this",
        "throw",
        "try",
        "typealias",
        "val",
        "var",
        "vararg",
        "when",
        "where",
        "while",
    ],
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("\"\"\"", "\"\"\"", 0, true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const SWIFT: Lang = Lang {
    keywords: &[
        "actor",
        "as",
        "associatedtype",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "continue",
        "default",
        "defer",
        "deinit",
        "do",
        "else",
        "enum",
        "extension",
        "fallthrough",
        "fileprivate",
        "for",
        "func",
        "guard",
        "if",
        "import",
        "in",
        "init",
        "inout",
        "internal",
        "is",
        "let",
        "open",
        "operator",
        "private",
        "protocol",
        "public",
        "repeat",
        "rethrows",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "subscript",
        "super",
        "switch",
        "throw",
        "throws",
        "try",
        "typealias",
        "var",
        "where",
        "while",
    ],
    constants: &["true", "false", "nil"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("\"", "\"", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const GO: Lang = Lang {
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    types: &[
        "any",
        "bool",
        "byte",
        "complex64",
        "complex128",
        "error",
        "float32",
        "float64",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "rune",
        "string",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uintptr",
    ],
    builtins: &[
        "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make",
        "max", "min", "new", "panic", "print", "println", "real", "recover",
    ],
    constants: &["true", "false", "nil", "iota"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("`", "`", 0, true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    ..BASE
};

const JS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const JS: Lang = Lang {
    keywords: JS_KEYWORDS,
    types: &[
        "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
    ],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("`", "`", b'\\', true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
        "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    builtins: &[
        "abs",
        "all",
        "any",
        "dict",
        "enumerate",
        "filter",
        "float",
        "getattr",
        "hasattr",
        "input",
        "int",
        "isinstance",
        "iter",
        "len",
        "list",
        "map",
        "max",
        "min",
        "next",
        "open",
        "print",
        "range",
        "repr",
        "set",
        "setattr",
        "sorted",
        "str",
        "sum",
        "super",
        "tuple",
        "type",
        "zip",
    ],
    constants: &["True", "False", "None"],
    line: HASH,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("'''", "'''", b'\\', true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const RUBY: Lang = Lang {
    keywords: &[
        "alias", "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end",
        "ensure", "for", "if", "in", "module", "next", "not", "or", "redo", "rescue", "retry",
        "return", "self", "super", "then", "undef", "unless", "until", "when", "while", "yield",
    ],
    builtins: &[
        "puts",
        "print",
        "require",
        "require_relative",
        "attr_accessor",
        "attr_reader",
    ],
    constants: &["true", "false", "nil"],
    line: HASH,
    quotes: &[q("\"", "\"", b'\\', false), q("'", "'", b'\\', false)],
    hash_word: true,
    caps: true,
    at_meta: false,
    ..BASE
};

const PHP: Lang = Lang {
    keywords: &[
        "abstract",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "echo",
        "else",
        "elseif",
        "extends",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "function",
        "if",
        "implements",
        "include",
        "interface",
        "match",
        "namespace",
        "new",
        "private",
        "protected",
        "public",
        "require",
        "return",
        "static",
        "switch",
        "throw",
        "trait",
        "try",
        "use",
        "while",
        "yield",
    ],
    constants: &["true", "false", "null", "TRUE", "FALSE", "NULL"],
    line: &["//", "#"],
    block: C_BLOCK,
    quotes: C_QUOTES,
    dollar: true,
    ..BASE
};

const LUA: Lang = Lang {
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    constants: &["true", "false", "nil"],
    line: &["--"],
    block: &[("--[[", "]]")],
    quotes: &[
        q("[[", "]]", 0, true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    ..BASE
};

const SQL: Lang = Lang {
    keywords: &[
        "add",
        "all",
        "alter",
        "and",
        "as",
        "asc",
        "begin",
        "between",
        "by",
        "case",
        "check",
        "column",
        "commit",
        "constraint",
        "create",
        "cross",
        "database",
        "default",
        "delete",
        "desc",
        "distinct",
        "drop",
        "else",
        "end",
        "exists",
        "foreign",
        "from",
        "full",
        "grant",
        "group",
        "having",
        "if",
        "in",
        "index",
        "inner",
        "insert",
        "into",
        "is",
        "join",
        "key",
        "left",
        "like",
        "limit",
        "not",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "primary",
        "references",
        "returning",
        "revoke",
        "right",
        "rollback",
        "select",
        "set",
        "table",
        "then",
        "transaction",
        "union",
        "unique",
        "update",
        "values",
        "view",
        "when",
        "where",
        "with",
    ],
    types: &[
        "bigint",
        "blob",
        "boolean",
        "char",
        "date",
        "decimal",
        "float",
        "int",
        "integer",
        "json",
        "jsonb",
        "numeric",
        "real",
        "serial",
        "smallint",
        "text",
        "timestamp",
        "timestamptz",
        "uuid",
        "varchar",
    ],
    builtins: &[
        "avg", "cast", "coalesce", "count", "length", "lower", "max", "min", "now", "sum", "upper",
    ],
    constants: &["null", "true", "false"],
    line: &["--"],
    block: C_BLOCK,
    quotes: &[q("'", "'", 0, false), q("\"", "\"", 0, false)],
    nocase: true,
    ..BASE
};

const HASKELL: Lang = Lang {
    keywords: &[
        "case", "class", "data", "deriving", "do", "else", "if", "import", "in", "infix", "infixl",
        "infixr", "instance", "let", "module", "newtype", "of", "then", "type", "where",
    ],
    constants: &["True", "False"],
    line: &["--"],
    block: &[("{-", "-}")],
    quotes: &[q("\"", "\"", b'\\', false)],
    caps: true,
    ..BASE
};

const R: Lang = Lang {
    keywords: &[
        "break", "else", "for", "function", "if", "in", "next", "repeat", "return", "while",
    ],
    constants: &["TRUE", "FALSE", "NULL", "NA", "Inf", "NaN"],
    line: HASH,
    quotes: C_QUOTES,
    ..BASE
};

const SCALA: Lang = Lang {
    keywords: &[
        "abstract",
        "case",
        "catch",
        "class",
        "def",
        "do",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "for",
        "given",
        "if",
        "implicit",
        "import",
        "lazy",
        "match",
        "new",
        "object",
        "override",
        "package",
        "private",
        "protected",
        "return",
        "sealed",
        "super",
        "then",
        "this",
        "throw",
        "trait",
        "try",
        "type",
        "using",
        "val",
        "var",
        "while",
        "with",
        "yield",
    ],
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[q("\"\"\"", "\"\"\"", 0, true), q("\"", "\"", b'\\', false)],
    caps: true,
    at_meta: true,
    ..BASE
};

const DART: Lang = Lang {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "extension",
        "factory",
        "final",
        "finally",
        "for",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "is",
        "late",
        "library",
        "mixin",
        "new",
        "on",
        "part",
        "required",
        "return",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("'''", "'''", b'\\', true),
        q("\"", "\"", b'\\', false),
        q("'", "'", b'\\', false),
    ],
    caps: true,
    at_meta: true,
    ..BASE
};

const ZIG: Lang = Lang {
    keywords: &[
        "break",
        "comptime",
        "const",
        "continue",
        "defer",
        "else",
        "enum",
        "errdefer",
        "error",
        "export",
        "extern",
        "fn",
        "for",
        "if",
        "inline",
        "orelse",
        "pub",
        "return",
        "struct",
        "switch",
        "test",
        "try",
        "union",
        "unreachable",
        "var",
        "while",
    ],
    constants: &["true", "false", "null", "undefined"],
    line: SLASH,
    quotes: C_QUOTES,
    ..BASE
};

const ELIXIR: Lang = Lang {
    keywords: &[
        "after",
        "alias",
        "case",
        "catch",
        "cond",
        "def",
        "defmodule",
        "defp",
        "do",
        "else",
        "end",
        "fn",
        "for",
        "if",
        "import",
        "quote",
        "receive",
        "require",
        "rescue",
        "try",
        "unless",
        "use",
        "when",
        "with",
    ],
    constants: &["true", "false", "nil"],
    line: HASH,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("\"", "\"", b'\\', false),
    ],
    hash_word: true,
    caps: true,
    ..BASE
};

const NIX: Lang = Lang {
    keywords: &[
        "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
    ],
    builtins: &["import", "builtins", "map", "toString"],
    constants: &["true", "false", "null"],
    line: HASH,
    block: C_BLOCK,
    quotes: &[q("''", "''", 0, true), q("\"", "\"", b'\\', false)],
    hash_word: true,
    calls: false,
    ..BASE
};

const HCL: Lang = Lang {
    keywords: &[
        "data",
        "for",
        "if",
        "in",
        "locals",
        "module",
        "output",
        "provider",
        "resource",
        "terraform",
        "variable",
    ],
    constants: &["true", "false", "null"],
    line: &["#", "//"],
    block: C_BLOCK,
    quotes: &[q("\"", "\"", b'\\', false)],
    hash_word: true,
    key_sep: b'=',
    ..BASE
};

const YAML: Lang = Lang {
    constants: &["true", "false", "null", "yes", "no", "on", "off", "~"],
    line: HASH,
    quotes: &[q("\"", "\"", b'\\', false), q("'", "'", 0, false)],
    nocase: true,
    hash_word: true,
    calls: false,
    ops: false,
    key_sep: b':',
    string_keys: true,
    ..BASE
};

const TOML: Lang = Lang {
    constants: &["true", "false", "inf", "nan"],
    line: HASH,
    quotes: &[
        q("\"\"\"", "\"\"\"", b'\\', true),
        q("'''", "'''", 0, true),
        q("\"", "\"", b'\\', false),
        q("'", "'", 0, false),
    ],
    calls: false,
    ops: false,
    key_sep: b'=',
    sections: true,
    ..BASE
};

const JSON: Lang = Lang {
    constants: &["true", "false", "null"],
    line: SLASH,
    block: C_BLOCK,
    quotes: &[q("\"", "\"", b'\\', false)],
    calls: false,
    ops: false,
    string_keys: true,
    ..BASE
};

const INI: Lang = Lang {
    constants: &["true", "false", "yes", "no", "on", "off"],
    line: &[";", "#"],
    quotes: &[q("\"", "\"", b'\\', false)],
    nocase: true,
    hash_word: true,
    calls: false,
    ops: false,
    key_sep: b'=',
    sections: true,
    ..BASE
};

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "select", "time", "return", "break", "continue", "local", "export",
    "readonly", "declare", "typeset", "unset",
];

const SHELL: Lang = Lang {
    kind: Kind::Shell,
    keywords: SHELL_KEYWORDS,
    line: HASH,
    quotes: &[q("\"", "\"", b'\\', true), q("'", "'", 0, true)],
    hash_word: true,
    ..BASE
};

const CONSOLE: Lang = Lang {
    kind: Kind::Console,
    ..SHELL
};

const DOCKERFILE: Lang = Lang {
    kind: Kind::Dockerfile,
    keywords: &[
        "ADD",
        "ARG",
        "AS",
        "CMD",
        "COPY",
        "ENTRYPOINT",
        "ENV",
        "EXPOSE",
        "FROM",
        "HEALTHCHECK",
        "LABEL",
        "MAINTAINER",
        "ONBUILD",
        "RUN",
        "SHELL",
        "STOPSIGNAL",
        "USER",
        "VOLUME",
        "WORKDIR",
    ],
    nocase: true,
    ..SHELL
};

const MAKE: Lang = Lang {
    kind: Kind::Make,
    keywords: &[
        "define", "else", "endef", "endif", "export", "ifdef", "ifeq", "ifndef", "ifneq",
        "include", "override", "unexport", "vpath",
    ],
    ..SHELL
};

const POWERSHELL: Lang = Lang {
    kind: Kind::PowerShell,
    keywords: &[
        "begin",
        "break",
        "catch",
        "class",
        "continue",
        "data",
        "do",
        "dynamicparam",
        "else",
        "elseif",
        "end",
        "enum",
        "exit",
        "filter",
        "finally",
        "for",
        "foreach",
        "function",
        "if",
        "in",
        "param",
        "process",
        "return",
        "switch",
        "throw",
        "trap",
        "try",
        "until",
        "using",
        "while",
    ],
    line: HASH,
    block: &[("<#", "#>")],
    quotes: &[
        q("@\"", "\"@", 0, true),
        q("@'", "'@", 0, true),
        q("\"", "\"", b'`', true),
        q("'", "'", 0, true),
    ],
    nocase: true,
    hash_word: true,
    ..BASE
};

/// PowerShell's `-word` operators (anything else after a `-` is a parameter).
const PS_OPERATORS: &[&str] = &[
    "and",
    "as",
    "band",
    "bnot",
    "bor",
    "bxor",
    "ccontains",
    "ceq",
    "cge",
    "cgt",
    "cle",
    "clike",
    "clt",
    "cmatch",
    "cne",
    "cnotlike",
    "cnotmatch",
    "contains",
    "creplace",
    "csplit",
    "eq",
    "f",
    "ge",
    "gt",
    "icontains",
    "ieq",
    "ige",
    "igt",
    "ile",
    "ilike",
    "ilt",
    "imatch",
    "in",
    "ine",
    "inotlike",
    "inotmatch",
    "ireplace",
    "is",
    "isnot",
    "isplit",
    "join",
    "le",
    "like",
    "lt",
    "match",
    "ne",
    "not",
    "notcontains",
    "notin",
    "notlike",
    "notmatch",
    "or",
    "replace",
    "shl",
    "shr",
    "split",
    "xor",
];

const HTML: Lang = Lang {
    kind: Kind::Html,
    block: &[("<!--", "-->")],
    quotes: &[q("\"", "\"", 0, false), q("'", "'", 0, false)],
    calls: false,
    ..BASE
};

const CSS: Lang = Lang {
    kind: Kind::Css,
    keywords: &["!important"],
    line: SLASH,
    block: C_BLOCK,
    quotes: C_QUOTES,
    ops: false,
    ..BASE
};

const DIFF: Lang = Lang {
    kind: Kind::Diff,
    ..PLAIN
};

/// Language names (lower case) and their definitions.
const LANGS: &[(&[&str], &Lang)] = &[
    (&["rust", "rs"], &RUST),
    (
        &[
            "c", "h", "cpp", "c++", "cc", "cxx", "hpp", "hh", "objc", "arduino",
        ],
        &C,
    ),
    (&["cs", "csharp", "c#"], &CSHARP),
    (&["java", "groovy"], &JAVA),
    (&["kotlin", "kt", "kts"], &KOTLIN),
    (&["swift"], &SWIFT),
    (&["go", "golang"], &GO),
    (
        &[
            "js",
            "javascript",
            "mjs",
            "cjs",
            "jsx",
            "ts",
            "typescript",
            "tsx",
            "mts",
            "cts",
        ],
        &JS,
    ),
    (&["py", "python", "python3", "py3", "gyp"], &PYTHON),
    (&["rb", "ruby"], &RUBY),
    (&["php"], &PHP),
    (&["lua"], &LUA),
    (
        &[
            "sql",
            "mysql",
            "postgres",
            "postgresql",
            "psql",
            "pgsql",
            "plsql",
            "sqlite",
            "tsql",
        ],
        &SQL,
    ),
    (&["haskell", "hs"], &HASKELL),
    (&["r"], &R),
    (&["scala", "sc"], &SCALA),
    (&["dart"], &DART),
    (&["zig"], &ZIG),
    (&["elixir", "ex", "exs"], &ELIXIR),
    (&["nix"], &NIX),
    (&["hcl", "terraform", "tf", "tfvars"], &HCL),
    (&["yaml", "yml"], &YAML),
    (&["toml"], &TOML),
    (&["json", "jsonc", "json5", "jsonl", "geojson"], &JSON),
    (
        &[
            "ini",
            "cfg",
            "conf",
            "dosini",
            "properties",
            "editorconfig",
            "env",
            "dotenv",
        ],
        &INI,
    ),
    (
        &["sh", "bash", "shell", "zsh", "ksh", "fish", "ash", "dash"],
        &SHELL,
    ),
    (
        &[
            "console",
            "shellsession",
            "shell-session",
            "terminal",
            "sh-session",
        ],
        &CONSOLE,
    ),
    (
        &["powershell", "ps1", "ps", "pwsh", "psm1", "psd1", "posh"],
        &POWERSHELL,
    ),
    (&["dockerfile", "docker", "containerfile"], &DOCKERFILE),
    (&["make", "makefile", "mk", "mak"], &MAKE),
    (
        &[
            "html", "htm", "xhtml", "xml", "svg", "vue", "svelte", "plist", "xsl",
        ],
        &HTML,
    ),
    (&["css", "scss", "sass", "less"], &CSS),
    (&["diff", "patch"], &DIFF),
    (
        &[
            "",
            "text",
            "txt",
            "plain",
            "plaintext",
            "output",
            "log",
            "md",
            "markdown",
            "csv",
            "tsv",
            "none",
        ],
        &PLAIN,
    ),
];

fn lang(name: &str) -> &'static Lang {
    LANGS
        .iter()
        .find(|(names, _)| names.iter().any(|n| n.eq_ignore_ascii_case(name)))
        .map(|(_, l)| *l)
        .unwrap_or(&GENERIC)
}

/// Highlight `line` (a line of code in `lang`) into `flags` (one per byte),
/// continuing from what the line above left open; returns what this line
/// leaves open.
pub fn highlight(line: &str, lang_name: &str, carry: Carry, flags: &mut [u32]) -> Carry {
    Scan::new(line, lang(lang_name), Some(flags)).run(carry)
}

/// What `line` leaves open for the next line, without styling it.
pub fn carry_after(line: &str, lang_name: &str, carry: Carry) -> Carry {
    let l = lang(lang_name);
    if l.kind == Kind::Plain {
        return Carry::None;
    }
    Scan::new(line, l, None).run(carry)
}

// ---------------------------------------------------------------- indentation guides

/// Most guides a line gets.
const MAX_GUIDES: u32 = 16;

/// Columns of a line's leading whitespace (a tab goes to the next multiple
/// of 4, the editor's tab size).
fn indent_width(line: &str) -> u32 {
    let mut w = 0;
    for b in line.bytes() {
        match b {
            b' ' => w += 1,
            b'\t' => w = (w / 4 + 1) * 4,
            _ => break,
        }
    }
    w
}

/// Indentation guides (a vertical line per indentation level), worked out a
/// line at a time like the highlighting: the block's indentation step — its
/// smallest indentation so far, 2 to 8 columns — and the guides of the last
/// line with text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Indent {
    unit: u8,
    prev: u8,
}

impl Indent {
    fn unit_with(self, width: u32) -> u32 {
        match (self.unit as u32, width) {
            (u, 0) => u,
            (0, w) => w.clamp(2, 8),
            (u, w) => u.min(w.max(2)),
        }
    }

    /// The guides of `line` (followed by `next`, if the block goes on) —
    /// `(how many, the step in columns)` — and the state for the next line.
    /// A blank line continues the guides around it, as many as the lines
    /// before and after it share.
    pub fn line(self, line: &str, next: Option<&str>) -> ((u8, u8), Indent) {
        let guides = |width: u32, unit: u32| {
            if unit == 0 {
                0
            } else {
                width.div_ceil(unit).min(MAX_GUIDES)
            }
        };
        if line.trim().is_empty() {
            let g = match next.filter(|n| !n.trim().is_empty()) {
                Some(n) => {
                    let w = indent_width(n);
                    (self.prev as u32).min(guides(w, self.unit_with(w)))
                }
                None => self.prev as u32,
            };
            let unit = if g == 0 { 0 } else { self.unit };
            return ((g as u8, unit), self);
        }
        let width = indent_width(line);
        let unit = self.unit_with(width);
        let g = guides(width, unit);
        let next = Indent {
            unit: unit as u8,
            prev: g as u8,
        };
        ((g as u8, if g == 0 { 0 } else { unit as u8 }), next)
    }
}

/// The guides of a line as an empty element (`span.igs`, positioned with
/// CSS); `widget`: as an editor widget.
pub fn guides_html((n, unit): (u8, u8), widget: bool) -> String {
    if n == 0 {
        return String::new();
    }
    if widget {
        format!(
            "<span class=\"w igs ig{n} iu{unit}\" contenteditable=\"false\" data-w=\"1\"></span>"
        )
    } else {
        format!("<span class=\"igs ig{n} iu{unit}\"></span>")
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

const OPS: &[u8] = b"=+-*/%<>!&|^~?";

struct Scan<'a> {
    line: &'a str,
    b: &'a [u8],
    n: usize,
    i: usize,
    l: &'static Lang,
    out: Option<&'a mut [u32]>,
}

/// What a step of the scanner did.
enum Step {
    /// Moved on.
    Next,
    /// The line ends inside something that goes on below.
    End(Carry),
}

impl<'a> Scan<'a> {
    fn new(line: &'a str, l: &'static Lang, out: Option<&'a mut [u32]>) -> Scan<'a> {
        Scan {
            line,
            b: line.as_bytes(),
            n: line.len(),
            i: 0,
            l,
            out,
        }
    }

    fn mark(&mut self, a: usize, b: usize, f: u32) {
        if let Some(o) = self.out.as_deref_mut() {
            let n = o.len();
            for x in &mut o[a.min(n)..b.min(n)] {
                *x |= f;
            }
        }
    }

    fn rest(&self) -> &'a str {
        &self.line[self.i..]
    }

    fn at(&self, k: usize) -> u8 {
        self.b.get(k).copied().unwrap_or(0)
    }

    fn prev_word(&self) -> bool {
        self.i > 0 && is_word(self.b[self.i - 1])
    }

    fn at_word_start(&self) -> bool {
        self.i == 0 || self.b[self.i - 1].is_ascii_whitespace()
    }

    /// Advance past one character.
    fn bump(&mut self) {
        self.i += self.line[self.i..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }

    fn word_end(&self, from: usize) -> usize {
        let mut j = from;
        while j < self.n && is_word(self.b[j]) {
            j += 1;
        }
        j
    }

    fn is(&self, list: &[&str], w: &str) -> bool {
        if self.l.nocase {
            list.iter().any(|k| k.eq_ignore_ascii_case(w))
        } else {
            list.contains(&w)
        }
    }

    fn run(mut self, carry: Carry) -> Carry {
        if let Step::End(c) = self.resume(carry) {
            return c;
        }
        match self.l.kind {
            Kind::Plain => Carry::None,
            Kind::Diff => {
                self.diff();
                Carry::None
            }
            Kind::Html => self.html(carry == Carry::Tag),
            Kind::Shell => self.shell(carry == Carry::Cont),
            Kind::Console => self.console(carry),
            Kind::Dockerfile => self.dockerfile(carry == Carry::Cont),
            Kind::Make => self.make(carry == Carry::Cont),
            _ => self.code(),
        }
    }

    /// Finish a comment or string carried over from the line above.
    fn resume(&mut self, carry: Carry) -> Step {
        match carry {
            Carry::Comment(k) => {
                let close = self.l.block.get(k as usize).map(|c| c.1).unwrap_or("");
                match self.line.find(close).filter(|_| !close.is_empty()) {
                    Some(j) => {
                        self.mark(0, j + close.len(), COMMENT);
                        self.i = j + close.len();
                        Step::Next
                    }
                    None => {
                        self.mark(0, self.n, COMMENT);
                        Step::End(carry)
                    }
                }
            }
            Carry::Str(k) => {
                let Some(qt) = self.l.quotes.get(k as usize) else {
                    return Step::Next;
                };
                match self.string_end(0, qt) {
                    Some(j) => {
                        self.mark(0, j, STR);
                        self.i = j;
                        Step::Next
                    }
                    None => {
                        self.mark(0, self.n, STR);
                        Step::End(carry)
                    }
                }
            }
            _ => Step::Next,
        }
    }

    /// Where a string that has its content start at `from` ends (after the
    /// closing quote), if it ends on this line.
    fn string_end(&self, from: usize, qt: &Quote) -> Option<usize> {
        let mut j = from;
        while j < self.n {
            if qt.escape != 0 && self.b[j] == qt.escape {
                j += 1;
                if j < self.n {
                    j += self.line[j..]
                        .chars()
                        .next()
                        .map(char::len_utf8)
                        .unwrap_or(1);
                }
                continue;
            }
            if self.line[j..].starts_with(qt.close) {
                return Some(j + qt.close.len());
            }
            j += self.line[j..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
        }
        None
    }

    /// A comment or string starting here: styles it and moves past it.
    fn comment_or_string(&mut self) -> Option<Step> {
        let rest = self.rest();
        let comment_ok = !self.l.hash_word || self.at_word_start();
        for (k, (open, close)) in self.l.block.iter().enumerate() {
            if comment_ok && rest.starts_with(open) {
                let from = self.i + open.len();
                return Some(match self.line[from..].find(close) {
                    Some(j) => {
                        let end = from + j + close.len();
                        self.mark(self.i, end, COMMENT);
                        self.i = end;
                        Step::Next
                    }
                    None => {
                        self.mark(self.i, self.n, COMMENT);
                        Step::End(Carry::Comment(k as u8))
                    }
                });
            }
        }
        for c in self.l.line {
            if comment_ok && rest.starts_with(c) {
                self.mark(self.i, self.n, COMMENT);
                return Some(Step::End(Carry::None));
            }
        }
        for (k, qt) in self.l.quotes.iter().enumerate() {
            if !rest.starts_with(qt.open) {
                continue;
            }
            // `r"…"` and the like: the prefix must start a word.
            if qt.open.as_bytes()[0].is_ascii_alphabetic() && self.prev_word() {
                continue;
            }
            let start = self.i;
            return Some(match self.string_end(start + qt.open.len(), qt) {
                Some(end) => {
                    let key = self.l.string_keys && self.followed_by_colon(end);
                    self.mark(start, end, if key { PROP } else { STR });
                    self.i = end;
                    Step::Next
                }
                None => {
                    self.mark(start, self.n, STR);
                    Step::End(if qt.multi {
                        Carry::Str(k as u8)
                    } else {
                        Carry::None
                    })
                }
            });
        }
        None
    }

    fn followed_by_colon(&self, from: usize) -> bool {
        let t = self.line[from..].trim_start();
        t.starts_with(':') && !t.starts_with("::")
    }

    /// A number starting here (not inside a word).
    fn number(&mut self) -> bool {
        let c = self.at(self.i);
        if !c.is_ascii_digit() || self.prev_word() {
            return false;
        }
        let mut j = self.i;
        while j < self.n
            && (is_word(self.b[j]) || (self.b[j] == b'.' && self.at(j + 1).is_ascii_digit()))
        {
            j += 1;
        }
        self.mark(self.i, j, NUM);
        self.i = j;
        true
    }

    /// `key:` / `key =` at the start of the line (after indentation and a
    /// YAML list dash).
    fn line_key(&mut self) {
        let sep = self.l.key_sep;
        let mut j = self.i;
        while j < self.n && self.b[j].is_ascii_whitespace() {
            j += 1;
        }
        if sep == b':' && self.at(j) == b'-' && matches!(self.at(j + 1), b' ' | 0) {
            self.mark(j, j + 1, OP);
            j += 1;
            while j < self.n && self.b[j].is_ascii_whitespace() {
                j += 1;
            }
        }
        let start = j;
        if matches!(self.at(j), b'#' | b'"' | b'\'' | b'[' | b'{' | b';' | 0) {
            self.i = start;
            return;
        }
        while j < self.n && self.b[j] != sep && !matches!(self.b[j], b'#' | b'"' | b'\'' | b'{') {
            j += 1;
        }
        let ok = self.at(j) == sep
            && (sep != b':' || matches!(self.at(j + 1), b' ' | b'\t' | 0))
            && !(sep == b'=' && self.at(j + 1) == b'=');
        if ok {
            let key_end = start + self.line[start..j].trim_end().len();
            if key_end > start {
                self.mark(start, key_end, PROP);
                self.mark(j, j + 1, OP);
                self.i = j + 1;
                return;
            }
        }
        self.i = start;
    }

    /// `[section]` / `[[array]]` lines.
    fn section(&mut self) -> bool {
        let t = self.rest().trim_start();
        if !t.starts_with('[') {
            return false;
        }
        let start = self.n - t.len();
        let end = match t.rfind(']') {
            Some(k) => start + k + 1,
            None => return false,
        };
        self.mark(start, end, TYPE);
        self.i = end;
        true
    }

    /// The common scanner: C-like languages, scripting languages, data.
    fn code(&mut self) -> Carry {
        if self.l.sections && self.section() {
            // The rest of the line may hold a comment.
        } else if self.l.key_sep != 0 {
            self.line_key();
        }
        if self.l.kind == Kind::C {
            self.preprocessor();
        }
        while self.i < self.n {
            if let Some(step) = self.comment_or_string() {
                match step {
                    Step::Next => continue,
                    Step::End(c) => return c,
                }
            }
            let c = self.b[self.i];
            if self.number() {
                continue;
            }
            if is_word(c) {
                self.word();
                continue;
            }
            let kind = self.l.kind;
            match kind {
                Kind::Rust if self.rust_special() => continue,
                Kind::PowerShell if self.powershell_special() => continue,
                Kind::Css if self.css_special() => continue,
                _ => {}
            }
            if c == b'@' && self.l.at_meta && is_word(self.at(self.i + 1)) {
                let e = self.word_end(self.i + 1);
                self.mark(self.i, e, META);
                self.i = e;
                continue;
            }
            if c == b'$' && self.l.dollar && is_word(self.at(self.i + 1)) {
                let e = self.word_end(self.i + 1);
                self.mark(self.i, e, VAR);
                self.i = e;
                continue;
            }
            if self.l.ops && OPS.contains(&c) {
                self.mark(self.i, self.i + 1, OP);
            }
            self.bump();
        }
        Carry::None
    }

    /// A word: keyword, constant, type, builtin, call…
    fn word(&mut self) {
        let start = self.i;
        let mut end = self.word_end(start);
        if self.l.kind == Kind::Css {
            // Property names and values have dashes.
            while end < self.n && (self.b[end] == b'-' || is_word(self.b[end])) {
                end += 1;
            }
        }
        if self.l.kind == Kind::PowerShell
            && self.at(end) == b'-'
            && self.at(end + 1).is_ascii_alphabetic()
        {
            // A cmdlet: `Get-ChildItem`.
            let mut e = end;
            while e < self.n && (self.b[e] == b'-' || is_word(self.b[e])) {
                e += 1;
            }
            self.mark(start, e, FUNC);
            self.i = e;
            return;
        }
        let w = &self.line[start..end];
        let l = self.l;
        let next = self.at(end);
        let f = if self.is(l.keywords, w) {
            KW
        } else if self.is(l.constants, w) {
            CONST
        } else if self.is(l.types, w) {
            TYPE
        } else if self.is(l.builtins, w) {
            FUNC
        } else if l.kind == Kind::Rust && next == b'!' && self.at(end + 1) != b'=' {
            // A macro call, `!` included.
            self.mark(start, end + 1, FUNC);
            self.i = end + 1;
            return;
        } else if l.kind == Kind::Css && self.css_property(end) {
            PROP
        } else if l.calls && next == b'(' {
            FUNC
        } else if l.caps
            && w.len() > 1
            && w.bytes().all(|c| !c.is_ascii_lowercase())
            && w.bytes().any(|c| c.is_ascii_uppercase())
        {
            CONST
        } else if l.caps && w.as_bytes()[0].is_ascii_uppercase() {
            TYPE
        } else {
            0
        };
        if f != 0 {
            self.mark(start, end, f);
        }
        self.i = end;
    }

    /// C's `#include <x.h>` and other directives.
    fn preprocessor(&mut self) {
        let t = self.rest().trim_start();
        if !t.starts_with('#') {
            return;
        }
        let start = self.n - t.len();
        let end = self.word_end(start + 1);
        self.mark(start, end, META);
        self.i = end;
        let rest = self.rest().trim_start();
        if rest.starts_with('<')
            && let Some(k) = rest.find('>')
        {
            let s = self.n - rest.len();
            self.mark(s, s + k + 1, STR);
            self.i = s + k + 1;
        }
    }

    /// Rust: char literals and lifetimes, attributes.
    fn rust_special(&mut self) -> bool {
        let c = self.b[self.i];
        if c == b'\'' {
            // Bytes after the opening quote, the closing one included.
            let rest = &self.line[self.i + 1..];
            let mut chars = rest.chars();
            let len = match chars.next() {
                Some('\\') => chars.next().and_then(|e| {
                    let from = 1 + e.len_utf8();
                    rest[from..].find('\'').map(|k| from + k + 1)
                }),
                Some(ch) => rest[ch.len_utf8()..]
                    .starts_with('\'')
                    .then(|| ch.len_utf8() + 1),
                None => None,
            };
            if let Some(len) = len {
                self.mark(self.i, self.i + 1 + len, STR);
                self.i += 1 + len;
            } else {
                // A lifetime or label.
                let e = self.word_end(self.i + 1);
                self.mark(self.i, e, META);
                self.i = e.max(self.i + 1);
            }
            return true;
        }
        if c == b'#' && matches!(self.at(self.i + 1), b'[' | b'!') {
            let mut depth = 0i32;
            let mut j = self.i;
            while j < self.n {
                match self.b[j] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            self.mark(self.i, j, META);
            self.i = j;
            return true;
        }
        false
    }

    /// PowerShell: `$variables`, `-operators` and `-Parameters`, `[types]`.
    fn powershell_special(&mut self) -> bool {
        let c = self.b[self.i];
        if c == b'$' {
            let start = self.i;
            let end = if self.at(start + 1) == b'{' {
                self.line[start..]
                    .find('}')
                    .map(|k| start + k + 1)
                    .unwrap_or(self.n)
            } else {
                let mut e = self.word_end(start + 1);
                // `$env:PATH`, `$script:count`
                if self.at(e) == b':' && is_word(self.at(e + 1)) {
                    e = self.word_end(e + 1);
                }
                e
            };
            if end == start + 1 {
                // `$(…)`
                self.mark(start, start + 1, OP);
                self.i = start + 1;
                return true;
            }
            let name = &self.line[start + 1..end];
            let f = if ["true", "false", "null"]
                .iter()
                .any(|k| k.eq_ignore_ascii_case(name))
            {
                CONST
            } else {
                VAR
            };
            self.mark(start, end, f);
            self.i = end;
            return true;
        }
        if c == b'-' && !self.prev_word() && self.at(self.i + 1).is_ascii_alphabetic() {
            let end = self.word_end(self.i + 1);
            let w = &self.line[self.i + 1..end];
            let f = if PS_OPERATORS.iter().any(|k| k.eq_ignore_ascii_case(w)) {
                OP
            } else {
                PROP
            };
            self.mark(self.i, end, f);
            self.i = end;
            return true;
        }
        if c == b'[' {
            let rest = &self.line[self.i + 1..];
            if let Some(k) = rest.find(']') {
                let inner = &rest[..k];
                if !inner.is_empty()
                    && inner.as_bytes()[0].is_ascii_alphabetic()
                    && inner.bytes().all(|b| is_word(b) || b == b'.' || b == b'[')
                {
                    self.mark(self.i + 1, self.i + 1 + k, TYPE);
                    self.i += 1 + k;
                    return true;
                }
            }
        }
        if c == b'@' && matches!(self.at(self.i + 1), b'(' | b'{') {
            self.mark(self.i, self.i + 1, OP);
            self.i += 1;
            return true;
        }
        false
    }

    /// CSS: at-rules, `#hex` colours, `.class` / `#id` selectors, variables.
    fn css_special(&mut self) -> bool {
        let c = self.b[self.i];
        let next = self.at(self.i + 1);
        if c == b'@' && is_word(next) {
            let e = self.word_end(self.i + 1);
            self.mark(self.i, e, KW);
            self.i = e;
            return true;
        }
        if (c == b'$' || (c == b'-' && next == b'-')) && (is_word(next) || next == b'-') {
            let mut e = self.i + 1;
            while e < self.n && (self.b[e] == b'-' || is_word(self.b[e])) {
                e += 1;
            }
            self.mark(self.i, e, VAR);
            self.i = e;
            return true;
        }
        if (c == b'#' || c == b'.') && is_word(next) {
            let mut e = self.i + 1;
            while e < self.n && (self.b[e] == b'-' || is_word(self.b[e])) {
                e += 1;
            }
            let selector = self.css_selector_line();
            if c == b'#' && !selector {
                self.mark(self.i, e, NUM);
            } else if selector {
                self.mark(self.i, e, TYPE);
            }
            self.i = e;
            return true;
        }
        if c == b'!'
            && self.rest()[1..]
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("important")
        {
            let k = self
                .rest()
                .to_ascii_lowercase()
                .find("important")
                .unwrap_or(0);
            self.mark(self.i, self.i + k + "important".len(), KW);
            self.i += k + "important".len();
            return true;
        }
        false
    }

    /// The line is (part of) a selector: it opens a block or continues a list.
    fn css_selector_line(&self) -> bool {
        let t = self.line.trim_end();
        (t.ends_with('{') || t.ends_with(',')) && !self.line[..self.i].contains('{')
    }

    /// A word that ends at `end` is a property name: followed by `:`, first
    /// on its line or after `{` / `;`, on a line that is not a selector.
    fn css_property(&self, end: usize) -> bool {
        let before = self.line[..self.i].trim_end();
        let first = before.is_empty() || before.ends_with('{') || before.ends_with(';');
        first
            && self.line[end..].trim_start().starts_with(':')
            && !(self.line.trim_end().ends_with('{') && !before.ends_with('{'))
    }

    fn diff(&mut self) {
        let l = self.line;
        let f = if l.starts_with("+++")
            || l.starts_with("---")
            || l.starts_with("diff ")
            || l.starts_with("index ")
        {
            META
        } else if l.starts_with("@@") {
            KW
        } else if l.starts_with('+') || l.starts_with('>') {
            INS
        } else if l.starts_with('-') || l.starts_with('<') {
            DEL
        } else {
            0
        };
        if f != 0 {
            self.mark(0, self.n, f);
        }
    }

    /// HTML and XML: tags, attributes, entities, comments.
    fn html(&mut self, mut in_tag: bool) -> Carry {
        while self.i < self.n {
            let c = self.b[self.i];
            if in_tag {
                if c == b'>' || (c == b'/' && self.at(self.i + 1) == b'>') {
                    in_tag = false;
                    self.i += if c == b'/' { 2 } else { 1 };
                    continue;
                }
                if let Some(step) = self.comment_or_string() {
                    match step {
                        Step::Next => continue,
                        Step::End(_) => return Carry::Tag,
                    }
                }
                if is_word(c) {
                    let mut e = self.i;
                    while e < self.n
                        && (is_word(self.b[e]) || matches!(self.b[e], b'-' | b':' | b'.' | b'@'))
                    {
                        e += 1;
                    }
                    self.mark(self.i, e, PROP);
                    self.i = e;
                    continue;
                }
                if c == b'=' {
                    self.mark(self.i, self.i + 1, OP);
                }
                self.bump();
                continue;
            }
            if self.rest().starts_with("<!--") {
                match self.comment_or_string() {
                    Some(Step::End(c)) => return c,
                    _ => continue,
                }
            }
            if c == b'<' {
                let mut j = self.i + 1;
                if matches!(self.at(j), b'/' | b'!' | b'?') {
                    j += 1;
                }
                let mut e = j;
                while e < self.n && (is_word(self.b[e]) || matches!(self.b[e], b'-' | b':' | b'.'))
                {
                    e += 1;
                }
                if e > j {
                    let meta = matches!(self.at(self.i + 1), b'!' | b'?');
                    self.mark(j, e, if meta { META } else { KW });
                    self.i = e;
                    in_tag = true;
                    continue;
                }
            }
            if c == b'&'
                && let Some(k) = self.rest().find(';')
                && k > 1
                && k < 12
                && self.rest()[1..k]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'#')
            {
                self.mark(self.i, self.i + k + 1, CONST);
                self.i += k + 1;
                continue;
            }
            self.bump();
        }
        if in_tag { Carry::Tag } else { Carry::None }
    }

    /// A shell variable at `$` here.
    fn shell_var(&mut self) -> bool {
        let start = self.i;
        let next = self.at(start + 1);
        let end = if next == b'{' {
            self.line[start..]
                .find('}')
                .map(|k| start + k + 1)
                .unwrap_or(self.n)
        } else if next.is_ascii_alphabetic() || next == b'_' {
            self.word_end(start + 1)
        } else if next.is_ascii_digit()
            || matches!(next, b'@' | b'#' | b'?' | b'$' | b'!' | b'*' | b'-')
        {
            start + 2
        } else {
            return false;
        };
        self.mark(start, end, VAR);
        self.i = end;
        true
    }

    /// Shells: keywords, commands (the first word of each command),
    /// variables, operators. `cont`: this line continues a command.
    fn shell(&mut self, cont: bool) -> Carry {
        let mut cmd = !cont;
        self.shell_from(&mut cmd)
    }

    fn shell_from(&mut self, cmd: &mut bool) -> Carry {
        while self.i < self.n {
            let c = self.b[self.i];
            if c.is_ascii_whitespace() {
                self.i += 1;
                continue;
            }
            if c == b'\\' && self.i + 1 == self.n {
                return Carry::Cont;
            }
            if let Some(step) = self.comment_or_string() {
                match step {
                    Step::Next => {
                        *cmd = false;
                        continue;
                    }
                    Step::End(c) => return c,
                }
            }
            if c == b'$' {
                if self.at(self.i + 1) == b'(' {
                    let two = self.at(self.i + 2) == b'(';
                    let len = if two { 3 } else { 2 };
                    self.mark(self.i, self.i + len, OP);
                    self.i += len;
                    *cmd = !two;
                    continue;
                }
                if self.shell_var() {
                    *cmd = false;
                    continue;
                }
            }
            if matches!(c, b'|' | b'&' | b';' | b'(' | b'`' | b'{') {
                let mut e = self.i + 1;
                while e < self.n && matches!(self.b[e], b'|' | b'&' | b';') {
                    e += 1;
                }
                self.mark(self.i, e, OP);
                self.i = e;
                *cmd = true;
                continue;
            }
            if matches!(c, b'<' | b'>' | b'=' | b'!' | b')' | b'}') {
                self.mark(self.i, self.i + 1, OP);
                self.i += 1;
                continue;
            }
            // A word: up to a space or a shell metacharacter.
            let start = self.i;
            let mut e = start;
            while e < self.n
                && !self.b[e].is_ascii_whitespace()
                && !matches!(
                    self.b[e],
                    b'|' | b'&'
                        | b';'
                        | b'('
                        | b')'
                        | b'<'
                        | b'>'
                        | b'`'
                        | b'"'
                        | b'\''
                        | b'$'
                        | b'='
                )
            {
                e += 1;
            }
            if e == start {
                self.bump();
                continue;
            }
            let w = &self.line[start..e];
            if self.at(e) == b'=' && *cmd {
                // `name=value`: an assignment; a command may follow.
                self.mark(e, e + 1, OP);
                self.i = e + 1;
                continue;
            }
            if SHELL_KEYWORDS.contains(&w) {
                self.mark(start, e, KW);
                *cmd = matches!(
                    w,
                    "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "time" | "!"
                );
            } else if *cmd {
                self.mark(start, e, FUNC);
                // These run the command that follows them.
                *cmd = matches!(
                    w,
                    "sudo" | "exec" | "env" | "nohup" | "command" | "builtin" | "xargs" | "doas"
                );
            } else if matches!(w, "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge") {
                self.mark(start, e, OP);
            } else if w.as_bytes()[0].is_ascii_digit()
                && w.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            {
                self.mark(start, e, NUM);
            }
            self.i = e;
        }
        Carry::None
    }

    /// A shell session: lines with a prompt (`$ `, `# `, `% `) are commands,
    /// the others output.
    fn console(&mut self, carry: Carry) -> Carry {
        if carry != Carry::None {
            return self.shell(carry == Carry::Cont);
        }
        let t = self.line.trim_start();
        let lead = self.n - t.len();
        for p in ["$ ", "# ", "% "] {
            if t.starts_with(p) || t == p.trim_end() {
                self.mark(lead, lead + 1, META);
                self.i = lead + 1;
                return self.shell(false);
            }
        }
        Carry::None
    }

    /// Dockerfiles: an instruction, then shell-like arguments.
    fn dockerfile(&mut self, cont: bool) -> Carry {
        let mut cmd = cont;
        if !cont {
            let t = self.line.trim_start();
            if t.starts_with('#') {
                self.mark(self.n - t.len(), self.n, COMMENT);
                return Carry::None;
            }
            let start = self.n - t.len();
            let e = self.word_end(start);
            let w = &self.line[start..e];
            if self.is(self.l.keywords, w) {
                self.mark(start, e, KW);
                cmd = w.eq_ignore_ascii_case("RUN");
                self.i = e;
                if w.eq_ignore_ascii_case("FROM")
                    && let Some(k) = self.line.to_ascii_lowercase().find(" as ")
                {
                    self.mark(k + 1, k + 3, KW);
                }
            }
        }
        self.shell_from(&mut cmd)
    }

    /// Makefiles: targets, variables, directives; recipes are shell.
    fn make(&mut self, cont: bool) -> Carry {
        if cont || self.line.starts_with('\t') {
            return self.shell(cont);
        }
        let t = self.line.trim_start();
        let start = self.n - t.len();
        if t.starts_with('#') {
            self.mark(start, self.n, COMMENT);
            return Carry::None;
        }
        let e = self.word_end(start);
        if self.is(self.l.keywords, &self.line[start..e]) {
            self.mark(start, e, KW);
            self.i = e;
        } else if let Some(k) = t.find([':', '=']) {
            let name_end = start + t[..k].trim_end().len();
            let assign = t[k..].starts_with('=')
                || t[k..].starts_with(":=")
                || t[..k].ends_with(['?', '+', '!']);
            if name_end > start && !t[..k].contains(['$', ' ']) || assign {
                let name_end = if assign {
                    start + t[..k].trim_end_matches(['?', '+', '!']).trim_end().len()
                } else {
                    name_end
                };
                self.mark(start, name_end, if assign { PROP } else { FUNC });
                self.i = name_end;
            }
        }
        // The rest: variables and comments.
        while self.i < self.n {
            let c = self.b[self.i];
            if c == b'#' {
                self.mark(self.i, self.n, COMMENT);
                break;
            }
            if c == b'$' {
                let next = self.at(self.i + 1);
                if next == b'(' || next == b'{' {
                    let close = if next == b'(' { ')' } else { '}' };
                    let end = self.line[self.i..]
                        .find(close)
                        .map(|k| self.i + k + 1)
                        .unwrap_or(self.n);
                    self.mark(self.i, end, VAR);
                    self.i = end;
                    continue;
                }
                if next != 0 {
                    // `$@`, `$<`, `$x`: one character.
                    let e = self.i
                        + 1
                        + self.line[self.i + 1..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                    self.mark(self.i, e, VAR);
                    self.i = e;
                    continue;
                }
            }
            self.bump();
        }
        if self.line.ends_with('\\') {
            Carry::Cont
        } else {
            Carry::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::classes;

    /// `(text, classes)` runs of a highlighted line, unstyled text left out.
    fn runs(line: &str, lang: &str, carry: Carry) -> (Vec<(String, String)>, Carry) {
        let mut flags = vec![0; line.len()];
        let after = highlight(line, lang, carry, &mut flags);
        assert_eq!(after, carry_after(line, lang, carry), "carry_after agrees");
        let mut out: Vec<(String, String)> = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let mut j = i + line[i..].chars().next().unwrap().len_utf8();
            while j < line.len() && flags[j] == flags[i] {
                j += 1;
            }
            if flags[i] != 0 {
                out.push((line[i..j].to_string(), classes(flags[i])));
            }
            i = j;
        }
        (out, after)
    }

    fn tokens(line: &str, lang: &str) -> Vec<(String, String)> {
        runs(line, lang, Carry::None).0
    }

    fn has(t: &[(String, String)], text: &str, class: &str) -> bool {
        t.iter().any(|(a, b)| a == text && b == class)
    }

    #[test]
    fn parses_fence_options() {
        let i = FenceInfo::parse("bash title=search_keys_web01.sh");
        assert_eq!(i.lang, "bash");
        assert_eq!(i.title.as_deref(), Some("search_keys_web01.sh"));
        assert!(i.numbers);
        assert!(i.hl.is_empty());

        let i = FenceInfo::parse("powershell title=exec.ps1 hl=15");
        assert_eq!(i.lang, "powershell");
        assert!(i.highlighted(15) && !i.highlighted(14));

        let i = FenceInfo::parse(r#"python title:"my script.py" hl:1-3,7 ln:false"#);
        assert_eq!(i.title.as_deref(), Some("my script.py"));
        assert_eq!(i.hl, vec![(1, 3), (7, 7)]);
        assert!(!i.numbers);
        assert!(i.guides);
        assert!(!FenceInfo::parse("yaml vl=false").guides);
        assert!(!FenceInfo::parse("yaml guides:off").guides);
        assert!(FenceInfo::parse("yaml vl=true").guides);
        assert_eq!(FenceInfo::parse("yaml").wrap, None);
        assert_eq!(FenceInfo::parse("yaml wrap=false").wrap, Some(false));
        assert_eq!(FenceInfo::parse("yaml wrap:true").wrap, Some(true));
        assert_eq!(FenceInfo::parse("yaml unwrap").wrap, Some(false));
        assert_eq!(FenceInfo::parse("yaml wrap").wrap, Some(true));
        assert_eq!(FenceInfo::parse("yaml wrap").lang, "yaml");
        let bare = FenceInfo::parse("nowrap title=x");
        assert_eq!((bare.lang.as_str(), bare.wrap), ("", Some(false)));
        assert!(i.highlighted(2) && i.highlighted(7) && !i.highlighted(4));

        let i = FenceInfo::parse("c++ unknown=1 hl={2,4} ln=true");
        assert_eq!(i.lang, "c++");
        assert_eq!(i.hl, vec![(2, 2), (4, 4)]);
        assert!(i.numbers);

        // Options without a language; nonsense is ignored.
        let i = FenceInfo::parse("title=notes.txt hl=x,5-3,9");
        assert_eq!(i.lang, "");
        assert_eq!(i.title.as_deref(), Some("notes.txt"));
        assert_eq!(i.hl, vec![(9, 9)]);
        assert_eq!(FenceInfo::parse(""), FenceInfo::default());
        assert_eq!(
            FenceInfo::parse("title=\"unclosed").title.as_deref(),
            Some("unclosed")
        );
    }

    #[test]
    fn the_header_escapes_its_text() {
        let i = FenceInfo::parse(r#"html title="<b>&x</b>""#);
        let h = head_html(&i, true);
        assert!(h.contains("&lt;b&gt;&amp;x&lt;/b&gt;"));
        assert!(!h.contains("<b>"));
        assert!(h.contains("code-copy"));
        assert!(!h.contains("tabindex"));
        assert!(head_html(&i, false).contains("tabindex=\"-1\""));
    }

    #[test]
    fn rust() {
        let t = tokens("let s: &'a str = \"hi\"; // done", "rust");
        assert!(has(&t, "let", "kw"));
        assert!(has(&t, "'a", "me"));
        assert!(has(&t, "str", "ty"));
        assert!(has(&t, "\"hi\"", "st"));
        assert!(has(&t, "// done", "cm"));
        let t = tokens("println!(\"{}\", MAX + Some(1)); let c = 'x';", "rs");
        assert!(has(&t, "println!", "fn"));
        assert!(has(&t, "MAX", "co"));
        assert!(has(&t, "Some", "fn"));
        assert!(has(&t, "1", "nu"));
        assert!(has(&t, "'x'", "st"));
        let t = tokens("#[derive(Debug)] struct A;", "rust");
        assert!(has(&t, "#[derive(Debug)]", "me"));
        assert!(has(&t, "A", "ty"));
    }

    #[test]
    fn block_comments_and_strings_span_lines() {
        let (t, c) = runs("int x = 1; /* start", "c", Carry::None);
        assert!(has(&t, "/* start", "cm"));
        assert_eq!(c, Carry::Comment(0));
        let (t, c) = runs("still a comment", "c", c);
        assert_eq!(t, vec![("still a comment".into(), "cm".into())]);
        let (t, c) = runs("end */ return 0;", "c", c);
        assert!(has(&t, "end */", "cm"));
        assert!(has(&t, "return", "kw"));
        assert_eq!(c, Carry::None);

        let (_, c) = runs("doc = \"\"\"Hello", "python", Carry::None);
        assert!(matches!(c, Carry::Str(_)));
        let (t, c) = runs("def not_code(): \"\"\" + x", "python", c);
        assert!(has(&t, "def not_code(): \"\"\"", "st"));
        assert_eq!(c, Carry::None);

        // A single-line string doesn't run into the next line.
        assert_eq!(runs("x = \"open", "python", Carry::None).1, Carry::None);
        // Plain text carries nothing.
        assert_eq!(carry_after("/* x", "text", Carry::None), Carry::None);
    }

    #[test]
    fn shell() {
        let t = tokens("for k in \"id_rsa\" \"id_ed25519\"; do", "bash");
        assert!(has(&t, "for", "kw"));
        assert!(has(&t, "in", "kw"));
        assert!(has(&t, "\"id_rsa\"", "st"));
        assert!(has(&t, "do", "kw"));
        let t = tokens(
            "    response=$(curl -s -w \"%{http_code}\" \"$pt\" -o /dev/null)",
            "bash",
        );
        assert!(has(&t, "=$(", "op"));
        assert!(has(&t, "curl", "fn"));
        assert!(!t.iter().any(|(a, _)| a == "-s"));
        let t = tokens(
            "if [ \"$response\" -ne 404 ]; then echo OK | tee log # note",
            "sh",
        );
        assert!(has(&t, "-ne", "op"));
        assert!(has(&t, "404", "nu"));
        assert!(has(&t, "then", "kw"));
        assert!(has(&t, "echo", "fn"));
        assert!(has(&t, "tee", "fn"));
        assert!(has(&t, "# note", "cm"));
        let t = tokens("echo $HOME ${USER} $1 a#b", "zsh");
        assert!(has(&t, "$HOME", "va"));
        assert!(has(&t, "${USER}", "va"));
        assert!(has(&t, "$1", "va"));
        assert!(
            !t.iter().any(|(a, _)| a.contains('#')),
            "a#b is not a comment"
        );
        // A continued line is not a new command.
        let (_, c) = runs("sudo apt-get install \\", "bash", Carry::None);
        assert_eq!(c, Carry::Cont);
        let (t, _) = runs("  curl git", "bash", c);
        assert!(t.is_empty(), "{t:?}");
        // Strings span lines.
        assert!(matches!(
            runs("echo 'one", "bash", Carry::None).1,
            Carry::Str(_)
        ));
    }

    #[test]
    fn console_highlights_commands_not_output() {
        let t = tokens("$ ls -la /tmp", "console");
        assert!(has(&t, "$", "me"));
        assert!(has(&t, "ls", "fn"));
        assert!(tokens("total 0 for if", "console").is_empty());
    }

    #[test]
    fn powershell() {
        let t = tokens("$enableSSL = $false", "powershell");
        assert!(has(&t, "$enableSSL", "va"));
        assert!(has(&t, "=", "op"));
        assert!(has(&t, "$false", "co"));
        let t = tokens("If ($String -Match $IPregex) {$Matches.Address}", "ps1");
        assert!(has(&t, "If", "kw"));
        assert!(has(&t, "-Match", "op"));
        assert!(has(&t, "$Matches", "va"));
        let t = tokens(
            "Clear-DnsClientCache; Get-ChildItem -Path $env:TEMP",
            "pwsh",
        );
        assert!(has(&t, "Clear-DnsClientCache", "fn"));
        assert!(has(&t, "Get-ChildItem", "fn"));
        assert!(has(&t, "-Path", "pr"));
        assert!(has(&t, "$env:TEMP", "va"));
        let t = tokens(
            "Function ExtractValidIPAddress([string]$String){ #$password = 1",
            "powershell",
        );
        assert!(has(&t, "Function", "kw"));
        assert!(has(&t, "ExtractValidIPAddress", "fn"));
        assert!(has(&t, "string", "ty"));
        assert!(has(&t, "#$password = 1", "cm"));
        let t = tokens("$port = 110", "powershell");
        assert!(has(&t, "110", "nu"));
        let (_, c) = runs("<# a block", "powershell", Carry::None);
        assert_eq!(c, Carry::Comment(0));
    }

    #[test]
    fn data_languages() {
        let t = tokens("  - name: \"x\" # c", "yaml");
        assert!(has(&t, "-", "op"));
        assert!(has(&t, "name", "pr"));
        assert!(has(&t, "\"x\"", "st"));
        assert!(has(&t, "# c", "cm"));
        assert!(has(&tokens("enabled: Yes", "yml"), "Yes", "co"));
        let t = tokens("{\"a\": [1, true, null], \"b\": \"c\"}", "json");
        assert!(has(&t, "\"a\"", "pr"));
        assert!(has(&t, "\"b\"", "pr"));
        assert!(has(&t, "\"c\"", "st"));
        assert!(has(&t, "true", "co"));
        assert!(has(&t, "1", "nu"));
        let t = tokens("[server.http] # x", "toml");
        assert!(has(&t, "[server.http]", "ty"));
        let t = tokens("port = 8080", "toml");
        assert!(has(&t, "port", "pr"));
        assert!(has(&t, "8080", "nu"));
        let t = tokens("; comment", "ini");
        assert!(has(&t, "; comment", "cm"));
    }

    #[test]
    fn web_languages() {
        let t = tokens(
            "<a href=\"/x\" class='y'>Tom &amp; Jerry</a> <!-- c -->",
            "html",
        );
        assert!(has(&t, "a", "kw"));
        assert!(has(&t, "href", "pr"));
        assert!(has(&t, "\"/x\"", "st"));
        assert!(has(&t, "&amp;", "co"));
        assert!(has(&t, "<!-- c -->", "cm"));
        let (_, c) = runs("<img src=\"a.png\"", "html", Carry::None);
        assert_eq!(c, Carry::Tag);
        let (t, c) = runs("     alt=\"x\">text", "html", c);
        assert!(has(&t, "alt", "pr"));
        assert_eq!(c, Carry::None);

        let t = tokens(".btn, #main {", "css");
        assert!(has(&t, ".btn", "ty"));
        assert!(has(&t, "#main", "ty"));
        let t = tokens("  color: #fff !important; margin: 0 4px;", "css");
        assert!(has(&t, "color", "pr"));
        assert!(has(&t, "#fff", "nu"));
        assert!(has(&t, "!important", "kw"));
        assert!(has(&t, "margin", "pr"));
        assert!(has(&t, "4px", "nu"));
        assert!(has(
            &tokens("@media (max-width: 600px) {", "css"),
            "@media",
            "kw"
        ));

        let t = tokens("const x = await fetch(`/api/${id}`);", "ts");
        assert!(has(&t, "const", "kw"));
        assert!(has(&t, "fetch", "fn"));
        assert!(has(&t, "`/api/${id}`", "st"));
    }

    #[test]
    fn other_languages() {
        let t = tokens("FROM rust:1.94 AS build", "dockerfile");
        assert!(has(&t, "FROM", "kw"));
        assert!(has(&t, "AS", "kw"));
        let t = tokens("RUN cargo build --release && strip app", "Dockerfile");
        assert!(has(&t, "RUN", "kw"));
        assert!(has(&t, "cargo", "fn"));
        assert!(has(&t, "strip", "fn"));
        let t = tokens("build: deps", "make");
        assert!(has(&t, "build", "fn"));
        let t = tokens("CC := gcc $(FLAGS)", "makefile");
        assert!(has(&t, "CC", "pr"));
        assert!(has(&t, "$(FLAGS)", "va"));
        assert!(has(&tokens("\tgcc -o app main.c", "make"), "gcc", "fn"));
        assert!(has(&tokens("+added", "diff"), "+added", "ins"));
        assert!(has(&tokens("-removed", "diff"), "-removed", "del"));
        assert!(has(&tokens("@@ -1 +1 @@", "patch"), "@@ -1 +1 @@", "kw"));
        let t = tokens("SELECT id FROM users WHERE n = 'x' -- c", "sql");
        assert!(has(&t, "SELECT", "kw"));
        assert!(has(&t, "'x'", "st"));
        assert!(has(&t, "-- c", "cm"));
        assert!(has(&tokens("@dataclass", "python"), "@dataclass", "me"));
        assert!(has(&tokens("#include <stdio.h>", "c"), "<stdio.h>", "st"));
        assert!(has(&tokens("#include <stdio.h>", "c"), "#include", "me"));
    }

    #[test]
    fn plain_and_unknown_languages() {
        assert!(tokens("don't \"quote\" // 42", "").is_empty());
        assert!(tokens("don't \"quote\" // 42", "text").is_empty());
        // Unknown: strings, numbers, comments only at a word's start.
        let t = tokens("url = \"https://x\" 42 // c", "unknownlang");
        assert!(has(&t, "\"https://x\"", "st"));
        assert!(has(&t, "42", "nu"));
        assert!(has(&t, "// c", "cm"));
        assert!(tokens("don't stop", "unknownlang").is_empty());
        // Names match whatever their case.
        assert!(has(&tokens("fn x()", "Rust"), "fn", "kw"));
    }

    /// The guides of every line of `code`.
    fn guides(code: &str) -> Vec<(u8, u8)> {
        let lines: Vec<&str> = code.split('\n').collect();
        let mut st = Indent::default();
        (0..lines.len())
            .map(|i| {
                let (g, next) = st.line(lines[i], lines.get(i + 1).copied());
                st = next;
                g
            })
            .collect()
    }

    #[test]
    fn indentation_guides() {
        // The script from the screenshots: four spaces a level.
        let bash = "#!/bin/bash\n\nfor u in a b; do\n    echo $u\n    for k in x y; do\n        if true; then\n            echo OK\n        fi\n    done\ndone";
        let g: Vec<u8> = guides(bash).iter().map(|g| g.0).collect();
        assert_eq!(g, vec![0, 0, 0, 1, 1, 2, 3, 2, 1, 0]);
        assert!(
            guides(bash)
                .iter()
                .all(|&(n, u)| n == 0 && u == 0 || u == 4)
        );
        // Two spaces a level (YAML); a tab is a level of four.
        assert_eq!(guides("a:\n  b:\n    c: 1"), vec![(0, 0), (1, 2), (2, 2)]);
        assert_eq!(
            guides("f {\n\tg {\n\t\tx\n\t}\n}"),
            vec![(0, 0), (1, 4), (2, 4), (1, 4), (0, 0)]
        );
        // A blank line keeps the guides the lines around it share.
        assert_eq!(
            guides("def a():\n    x = 1\n\n    y = 2\n\ndef b():"),
            vec![(0, 0), (1, 4), (1, 4), (1, 4), (0, 0), (0, 0)]
        );
        // A deeper first indentation is corrected by a shallower one later.
        assert_eq!(guides("a\n        b\n    c"), vec![(0, 0), (1, 8), (1, 4)]);
        // One space is not a level of its own.
        assert_eq!(guides("a\n b\n   c"), vec![(0, 0), (1, 2), (2, 2)]);
        assert_eq!(guides_html((0, 0), true), "");
        assert_eq!(
            guides_html((2, 4), false),
            "<span class=\"igs ig2 iu4\"></span>"
        );
        assert!(guides_html((2, 4), true).contains("data-w"));
    }

    #[test]
    fn every_byte_is_kept_and_nothing_panics() {
        let samples = [
            "",
            "'",
            "\"",
            "\\",
            "$",
            "${",
            "$(",
            "#[",
            "'\\",
            "<",
            "<!--",
            "&",
            "-",
            "@",
            "r#\"",
            "日本語 'é' \"€\" $é",
            "`",
            "/*",
            "\t\tgcc",
            "[",
            "]",
            "!",
            "!imp",
        ];
        for (names, _) in LANGS {
            for name in names.iter().take(1) {
                for s in samples {
                    for carry in [
                        Carry::None,
                        Carry::Comment(0),
                        Carry::Str(0),
                        Carry::Str(9),
                        Carry::Tag,
                        Carry::Cont,
                    ] {
                        let mut f = vec![0; s.len()];
                        highlight(s, name, carry, &mut f);
                        carry_after(s, name, carry);
                    }
                }
            }
        }
    }
}
