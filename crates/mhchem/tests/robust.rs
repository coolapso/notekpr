//! The input is anyone's note text: whatever it is, a translation returns
//! (TeX or an error) without panicking, hanging or overflowing the stack.

use std::time::{Duration, Instant};

use notekpr_mhchem::{ErrorKind, Kind, expand, to_tex};
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

/// Translates `input` every way there is; how long the slowest took.
fn all_ways(input: &str) -> Duration {
    let mut slowest = Duration::ZERO;
    for run in [
        &|s: &str| to_tex(s, Kind::Ce) as Result<String, _>,
        &|s: &str| to_tex(s, Kind::Pu),
        &|s: &str| expand(s),
    ] as [&dyn Fn(&str) -> Result<String, notekpr_mhchem::Error>; 3]
    {
        let start = Instant::now();
        let _ = run(input);
        slowest = slowest.max(start.elapsed());
    }
    slowest
}

/// Pieces of mhchem's syntax (and things that aren't), so that random
/// strings get far into the parser.
const TOKENS: &[&str] = &[
    "H",
    "O",
    "Cl",
    "Fe",
    "x",
    "n",
    "e",
    "s",
    "p",
    "sp",
    "aq",
    "i",
    "v",
    "I",
    "V",
    "X",
    "C",
    "M",
    "T",
    "0",
    "1",
    "2",
    "12",
    "1.5",
    "0,5",
    ",",
    ".",
    ";",
    ":",
    "...",
    " ",
    "  ",
    "\t",
    "\n",
    "\r",
    "+",
    "-",
    "=",
    "#",
    "<",
    ">",
    "<<",
    "*",
    "'",
    "/",
    "//",
    "~",
    "|",
    "&",
    "%",
    "@",
    "?",
    "!",
    "\"",
    "`",
    "^",
    "_",
    "{",
    "}",
    "{}",
    "(",
    ")",
    "[",
    "]",
    "$",
    "\\",
    "\\\\",
    "->",
    "<-",
    "<->",
    "<-->",
    "<=>",
    "<=>>",
    "<<=>",
    "→",
    "⇌",
    "⟶",
    "\\ce{",
    "\\pu{",
    "\\frac{",
    "\\overset{",
    "\\underset{",
    "\\underbrace{",
    "}_",
    "\\color{",
    "\\color",
    "\\bond{",
    "\\alpha",
    "\\mu",
    "\\Delta",
    "\\eta",
    "\\ca",
    "\\pm",
    "\\approx",
    "\\hline",
    "\\,",
    "\\;",
    "\\x",
    "\\text{",
    "α",
    "Ω",
    "°C",
    "^oF",
    "×",
    "−",
    "–",
    "…",
    "·",
    "•",
    "≡",
    "\u{2028}",
    "\u{a0}",
    "😀",
    "e\u{301}",
    "\0",
    "(aq)",
    "(s)",
    "($c$)",
    "^{2+}",
    "_{2}",
    "^2-",
    "$n$",
    "$\\pm$",
    "$2n-1$",
    "1/2",
    "(1/2)",
    "n/2",
    "kJ",
    "mol",
    "e3",
    "E-3",
    "10^",
    " x 10^",
    "(32)",
    "pH",
    "iPr",
    "M[",
    "T[",
    "^{.-}",
    "^x",
    "_{Li,x}",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn token_soup(tokens in prop::collection::vec(prop::sample::select(TOKENS), 0..48)) {
        let input = tokens.concat();
        all_ways(&input);
        all_ways(&format!("x \\ce{{{input}}} y \\pu{{{input}}}"));
    }

    #[test]
    fn printable_ascii(input in "[ -~]{0,80}") {
        all_ways(&input);
    }

    #[test]
    fn any_text(input in any::<String>()) {
        all_ways(&input);
    }
}

fn nest(open: &str, inner: &str, close: &str, n: usize) -> String {
    [open.repeat(n), inner.to_owned(), close.repeat(n)].concat()
}

/// Inputs made to hurt: very long, deeply nested, unbalanced, exploding.
fn hostile() -> Vec<(String, String)> {
    let mut inputs = Vec::new();
    let mut add = |name: &str, input: String| inputs.push((name.to_owned(), input));
    for unit in [
        "{", "}", "\\ce{", "\\pu{", "$", "^{", "_{", "(", "[", "->[", "\\", "\\frac{", "\\color{",
        "{}", "1",
    ] {
        add(&format!("{unit} x 100k"), unit.repeat(100_000));
    }
    for c in (b'!'..=b'~').filter(u8::is_ascii_punctuation) {
        let c = char::from(c);
        add(&format!("{c} x 1k"), c.to_string().repeat(1_000));
        add(&format!("{c}a x 500"), format!("{c}a").repeat(500));
    }
    add("nested \\ce", nest("\\ce{", "H2O", "}", 10_000));
    add("nested braces", nest("{", "x", "}", 100_000));
    add("nested $\\ce", nest("$\\ce{", "x", "}$", 5_000));
    add("nested \\frac", nest("\\frac{", "x", "}{y}", 5_000));
    add("nested \\color", nest("\\color{red}{", "x", "}", 5_000));
    add("nested arrows", nest("A ->[", "B", "] C", 5_000));
    add("nested scripts", nest("^{\\ce{", "H", "}}X", 5_000));
    // Each level of these writes the level below four times.
    add(
        "nested \\pu fractions",
        format!("{}//b", nest("{", "a", "//b}", 30)),
    );
    add("nested \\pu slashes", "1 kJ/mol ".repeat(5_000));
    add("long equation", "2H2 + O2 -> 2H2O ".repeat(100_000));
    add(
        "long space before /",
        format!("1 a{}x", " ".repeat(100_000)),
    );
    add(
        "long space before a formula",
        format!("a{}B", " ".repeat(100_000)),
    );
    add("unclosed dollars", "$a ".repeat(50_000));
    add("emoji", "😀".repeat(50_000));
    add("line separators", "\u{2028}".repeat(20_000));
    add("carriage returns", "{a\rb}".repeat(10_000));
    add("greek", "α\\alpha".repeat(20_000));
    add("combining marks", "e\u{301}".repeat(20_000));
    inputs
}

#[test]
fn hostile_inputs_end_quickly() {
    for (name, input) in hostile() {
        let slowest = all_ways(&input);
        // Generous, for slow machines and debug builds; the limits make it milliseconds.
        assert!(slowest < Duration::from_secs(5), "{name}: {slowest:?}");
    }
}

#[test]
fn limits_give_errors() {
    let kind = |input: &str, k: Kind| to_tex(input, k).map(|_| ()).map_err(|e| e.kind());
    assert_eq!(
        kind(&nest("\\ce{", "H2O", "}", 200), Kind::Ce),
        Err(ErrorKind::TooDeep)
    );
    assert_eq!(
        kind(&"2H2 + O2 -> 2H2O ".repeat(100_000), Kind::Ce),
        Err(ErrorKind::TooComplex)
    );
    assert_eq!(
        kind(&format!("{}//b", nest("{", "a", "//b}", 30)), Kind::Pu),
        Err(ErrorKind::TooComplex)
    );
    assert_eq!(
        expand(&nest("\\ce{$\\ce{", "x", "}$}", 100)).map_err(|e| e.kind()),
        Err(ErrorKind::TooDeep)
    );
    // Ordinary nesting and length are far inside the limits.
    assert!(to_tex(&nest("\\ce{", "H2O", "}", 20), Kind::Ce).is_ok());
    assert!(to_tex(&"2H2 + O2 -> 2H2O ".repeat(500), Kind::Ce).is_ok());
}

/// Deep but allowed nesting needs little stack: fine on a 1 MiB stack (as
/// in WebAssembly) even in a debug build.
#[test]
fn deepest_nesting_fits_a_small_stack() {
    let inputs = [
        nest("\\ce{", "H2O", "}", 60),
        nest("$\\ce{", "x", "}$", 20),
        nest("\\frac{", "x", "}{y}", 60),
        nest("{$\\ce{", "x", "}$}", 15),
    ];
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            for input in &inputs {
                assert!(to_tex(input, Kind::Ce).is_ok(), "{input}");
                let _ = to_tex(&[input.as_str(), "0"].concat(), Kind::Ce);
                let _ = expand(&["\\ce{", input, "}"].concat());
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
