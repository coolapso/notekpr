//! Math in notes: LaTeX between dollar signs, shown as MathML that the
//! browser lays out itself (MathML Core), with chemistry in it — mhchem's
//! `\ce{…}` and `\pu{…}` ([`notekpr_mhchem`]).
//!
//! What counts as math follows pulldown-cmark (the reading view's parser),
//! so that both views agree:
//!
//! * `$…$` is inline math: the `$` that opens it is followed by a
//!   non-space, the one that closes it follows one, so `$5 and $10` stays
//!   text. `$$…$$` is display math. `\$` is a dollar sign. Braces inside
//!   must balance; a `$` inside braces doesn't close.
//! * A block of display math spans lines: a line starting with `$$`, the
//!   formula, a line with the closing `$$` ([`crate::syntax::Block::Math`]).
//!
//! [`html`] converts with math-core (LaTeX → MathML Core). What it writes is
//! escaped by construction (`\text{<b>}` is text, colors are checked, there
//! are no links); what it can't convert shows as its source, marked as an
//! error. Notes are other people's text: a formula nested deeper than
//! [`MAX_DEPTH`] is refused before converting, because the converter
//! recurses and a stack overflow would end the app.

use std::ops::Range;

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};

use crate::syntax;

/// How deeply a formula may nest: groups, `\left…\right` and environments.
/// Real formulas stay under 20. math-core overflows a 1 MiB stack (the
/// browser's) at about 380 levels of `\frac{…}`.
pub const MAX_DEPTH: usize = 50;

/// How many commands and scripts may follow each other with nothing else
/// between them (each may take the next as its argument: `\sqrt\sqrt…`).
pub const MAX_RUN: usize = 100;

/// A formula in a line: where its delimiters are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The opening `$` or `$$`.
    pub open: usize,
    /// Where the closing `$` or `$$` starts.
    pub close: usize,
    pub display: bool,
}

impl Span {
    /// The length of each delimiter.
    pub fn delim(&self) -> usize {
        if self.display { 2 } else { 1 }
    }

    /// The formula's bytes, between the delimiters.
    pub fn content(&self) -> Range<usize> {
        self.open + self.delim()..self.close
    }

    /// Where the closing delimiter ends.
    pub fn end(&self) -> usize {
        self.close + self.delim()
    }
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The formula whose opening `$` is byte `open` of `b`, if there is one
/// (pulldown-cmark's rules, within one line).
pub fn span_at(b: &[u8], open: usize) -> Option<Span> {
    if b.get(open) != Some(&b'$') {
        return None;
    }
    let display = b.get(open + 1) == Some(&b'$');
    if !display && b.get(open + 1).is_none_or(|&c| space(c)) {
        return None;
    }
    let start = open + if display { 2 } else { 1 };
    let mut depth = 0usize;
    let mut j = start;
    while j < b.len() {
        match b[j] {
            b'\\' if b.get(j + 1).is_some_and(u8::is_ascii_punctuation) => {
                j += 2;
                continue;
            }
            b'{' => depth += 1,
            // An unbalanced brace: nothing after it closes this one.
            b'}' => depth = depth.checked_sub(1)?,
            b'$' if depth == 0 => {
                if display {
                    // `$$` closes; a single `$` can't be in display math.
                    return (b.get(j + 1) == Some(&b'$')).then_some(Span {
                        open,
                        close: j,
                        display,
                    });
                }
                // Inline math can't be empty, and a `$` after a space
                // can't close it (nor can any later one).
                return (j > start && !space(b[j - 1])).then_some(Span {
                    open,
                    close: j,
                    display,
                });
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Whether `line` opens a block of display math: it starts with `$$` (after
/// up to three spaces) that doesn't close on the line.
pub fn opens_block(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    line.len() - t.len() < 4 && t.starts_with("$$") && closing(&t[2..]).is_none()
}

/// Where the `$$` that closes a block of display math is in `line`.
pub fn closing(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut j = 0;
    while j + 1 < b.len() {
        if b[j] == b'\\' && b[j + 1].is_ascii_punctuation() {
            j += 2;
        } else if b[j] == b'$' && b[j + 1] == b'$' {
            return Some(j);
        } else {
            j += 1;
        }
    }
    None
}

/// The formula of a block of display math, from its lines: what is between
/// the `$$` that opens it (on the first line) and the one that closes it
/// (on the last), or `None` if it doesn't close.
pub fn block_formula(lines: &[&str]) -> Option<String> {
    let (first, rest) = lines.split_first()?;
    let t = first.trim_start_matches(' ');
    let mut out = t.strip_prefix("$$")?.to_string();
    for (k, line) in rest.iter().enumerate() {
        out.push('\n');
        match closing(line) {
            Some(at) => {
                out.push_str(&line[..at]);
                return Some(out);
            }
            None if k + 1 == rest.len() => return None,
            None => out.push_str(line),
        }
    }
    None
}

/// How deeply `tex` nests (groups, `\left`, `\begin`), and the longest run
/// of commands and scripts in a row.
fn nesting(tex: &str) -> (usize, usize) {
    let b = tex.as_bytes();
    let (mut depth, mut run) = (0usize, 0usize);
    let (mut max_depth, mut max_run) = (0, 0);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == i + 1 {
                    // A command of one character (`\{`, `\,`, `\é`).
                    j += tex[j..].chars().next().map_or(0, char::len_utf8);
                }
                match &tex[i + 1..j] {
                    "left" | "begin" => depth += 1,
                    "right" | "end" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                run += 1;
                i = j;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                run = 0;
                i += 1;
            }
            b'^' | b'_' => {
                run += 1;
                i += 1;
            }
            c if space(c) => i += 1,
            _ => {
                run = 0;
                i += tex[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
        max_depth = max_depth.max(depth);
        max_run = max_run.max(run);
    }
    (max_depth, max_run)
}

thread_local! {
    static CONVERTER: LatexToMathML = LatexToMathML::new(
        MathCoreConfig {
            // An unknown command shows in red, and the rest of the formula
            // as usual.
            ignore_unknown_commands: true,
            ..Default::default()
        },
        vec![],
    )
    .unwrap_or_default();
}

/// `tex` as MathML (`<math>`, `display="block"` for display math), or why
/// it can't be shown.
pub fn mathml(tex: &str, display: bool) -> Result<String, String> {
    let chem;
    let tex = if tex.contains("\\ce") || tex.contains("\\pu") {
        chem = chemistry(tex)?;
        &chem
    } else {
        tex
    };
    let (depth, run) = nesting(tex);
    if depth > MAX_DEPTH || run > MAX_RUN {
        return Err("This formula nests too deeply to show.".into());
    }
    let how = if display {
        MathDisplay::Block
    } else {
        MathDisplay::Inline
    };
    CONVERTER
        .with(|c| c.convert_with_local_state(tex, how))
        .map(|r| r.mathml)
        .map_err(|e| e.to_string())
}

/// A formula as HTML: its MathML, or its source (with its delimiters) marked
/// as an error that says what is wrong.
pub fn html(tex: &str, display: bool, out: &mut String) {
    match mathml(tex, display) {
        Ok(m) => out.push_str(&m),
        Err(why) => {
            out.push_str("<span class=\"merr\" title=\"");
            syntax::escape(&why, out);
            out.push_str("\">");
            let d = if display { "$$" } else { "$" };
            out.push_str(d);
            syntax::escape(tex, out);
            out.push_str(d);
            out.push_str("</span>");
        }
    }
}

/// How many times `\ce` may be expanded in one formula: mhchem leaves the
/// `\ce` of a formula's parts (`\underset{…}{\ce{…}}`) to be expanded again.
const CHEM_ROUNDS: usize = 4;

/// `tex` with its `\ce{…}` and `\pu{…}` turned into TeX math-core renders.
fn chemistry(tex: &str) -> Result<String, String> {
    let mut tex = tex.to_string();
    for _ in 0..CHEM_ROUNDS {
        if !tex.contains("\\ce") && !tex.contains("\\pu") {
            break;
        }
        tex = notekpr_mhchem::expand(&tex).map_err(|e| e.to_string())?;
    }
    Ok(for_math_core(&tex))
}

/// The group starting at byte `at` of `s` (a `{`): where it ends (after its
/// `}`), or `None` if it doesn't close.
fn group_end(s: &str, at: usize) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(at) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut j = at;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// mhchem's bonds math-core can't draw (they overlap dashes with `\rlap`
/// and `\raise`), as the closest it can: a dashed bond as `┄`, two bonds
/// stacked without a line between them.
const BONDS: &[(&str, &str)] = &[
    (
        "{\\rlap{\\lower.1em{-}}\\raise.1em{\\tripledash}}",
        "{\\genfrac{}{}{0pt}{}{\\text{┄}}{-}}",
    ),
    (
        "{\\rlap{\\lower.2em{-}}\\rlap{\\raise.2em{\\tripledash}}-}",
        "{\\genfrac{}{}{0pt}{}{\\text{┄}}{=}}",
    ),
    (
        "{\\rlap{\\lower.2em{-}}\\rlap{\\raise.2em{-}}\\tripledash}",
        "{\\genfrac{}{}{0pt}{}{-}{\\text{┄}}}",
    ),
    ("{\\tripledash}", "{\\text{┄}}"),
];

/// Arrows math-core doesn't know, as the closest it does: equilibria as
/// `⇌` (mhchem's long forms only stretch better), `<-->` as `⇆`.
const ARROWS: &[(&str, &str)] = &[
    ("\\longrightleftharpoons", "\\rightleftharpoons"),
    ("\\longRightleftharpoons", "\\rightleftharpoons"),
    ("\\longLeftrightharpoons", "\\rightleftharpoons"),
    ("\\longleftrightarrows", "\\leftrightarrows"),
];

/// Wrappers that only move or hide what they hold, which MathML Core can't
/// do: dropped, their content kept as a group.
const WRAPPERS: &[&str] = &[
    "\\smash[t]",
    "\\smash[b]",
    "\\smash",
    "\\llap",
    "\\rlap",
    "\\mathllap",
    "\\mathrlap",
];

/// mhchem's TeX (written for MathJax) in what math-core renders.
///
/// * An isotope's numbers are aligned to the right with an invisible copy
///   and `\llap`: the copy goes (they align left).
/// * Bonds and arrows math-core can't draw become the closest it can
///   ([`BONDS`], [`ARROWS`]); an equilibrium arrow with text above or below
///   stretches under it (`\xrightleftharpoons[below]{above}`).
/// * [`WRAPPERS`], `\raise` and `\lower` give way to their content.
fn for_math_core(tex: &str) -> String {
    let mut s = tex.to_string();
    for (from, to) in BONDS {
        s = s.replace(from, to);
    }
    s = drop_isotope_copies(&s);
    s = stretch_equilibria(&s);
    for (from, to) in ARROWS {
        s = s.replace(from, to);
    }
    unwrap(&s)
}

/// Without the invisible copy of an isotope's numbers:
/// `^{\hphantom{B}}_{\hphantom{P}}\mkern-1.5mu{\vphantom{A}}`.
fn drop_isotope_copies(s: &str) -> String {
    const SUP: &str = "^{\\hphantom";
    const SUB: &str = "}_{\\hphantom";
    const TAIL: &str = "}\\mkern-1.5mu{\\vphantom{A}}";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(SUP) {
        let after_sup = at + SUP.len();
        let copy = group_end(rest, after_sup)
            .and_then(|e| rest[e..].starts_with(SUB).then_some(e + SUB.len()))
            .and_then(|e| group_end(rest, e))
            .and_then(|e| rest[e..].starts_with(TAIL).then_some(e + TAIL.len()));
        match copy {
            Some(end) => {
                out.push_str(&rest[..at]);
                rest = &rest[end..];
            }
            None => {
                out.push_str(&rest[..after_sup]);
                rest = &rest[after_sup..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `\underset{\lower6mu{B}}{\overset{A}{\longrightleftharpoons}}` (either
/// part may be missing) as `\xrightleftharpoons[B]{A}`, for each long
/// equilibrium arrow.
fn stretch_equilibria(s: &str) -> String {
    const LONG: &[&str] = &[
        "\\longrightleftharpoons",
        "\\longRightleftharpoons",
        "\\longLeftrightharpoons",
    ];
    // (below, above, the source they came from)
    let parse = |s: &str| -> Option<(Option<String>, Option<String>, usize)> {
        let mut below = None;
        let mut i = 0;
        if let Some(r) = s.strip_prefix("\\underset{\\lower6mu") {
            let a = s.len() - r.len();
            let e = group_end(s, a)?;
            below = Some(s[a + 1..e - 1].to_string());
            // The `}` of the first argument, then the second.
            if s.as_bytes().get(e) != Some(&b'}') || s.as_bytes().get(e + 1) != Some(&b'{') {
                return None;
            }
            i = e + 2;
        }
        let mut above = None;
        let mut j = i;
        if s[j..].starts_with("\\overset") {
            let a = j + "\\overset".len();
            let e = group_end(s, a)?;
            above = Some(s[a + 1..e - 1].to_string());
            if s.as_bytes().get(e) != Some(&b'{') {
                return None;
            }
            j = e + 1;
        }
        let arrow = LONG.iter().find(|l| s[j..].starts_with(**l))?;
        let mut end = j + arrow.len();
        let closes = usize::from(above.is_some()) + usize::from(below.is_some());
        for _ in 0..closes {
            if s.as_bytes().get(end) != Some(&b'}') {
                return None;
            }
            end += 1;
        }
        (above.is_some() || below.is_some()).then_some((below, above, end))
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if (rest.starts_with("\\underset") || rest.starts_with("\\overset"))
            && let Some((below, above, len)) = parse(rest)
        {
            out.push_str("\\xrightleftharpoons");
            if let Some(b) = below {
                out.push('[');
                out.push_str(&b);
                out.push(']');
            }
            out.push('{');
            out.push_str(above.as_deref().unwrap_or(""));
            out.push('}');
            i += len;
            continue;
        }
        let c = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..c]);
        i += c;
    }
    out
}

/// [`WRAPPERS`], `\raise<length>` and `\lower<length>` dropped, their
/// argument kept as a group.
fn unwrap(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with('\\') {
            let mut skip = WRAPPERS
                .iter()
                .find(|w| rest.starts_with(**w) && rest[w.len()..].starts_with('{'))
                .map(|w| w.len());
            for cmd in ["\\raise", "\\lower"] {
                if skip.is_none()
                    && let Some(r) = rest.strip_prefix(cmd)
                {
                    // A length: a number and its unit (`.1em`, `6mu`).
                    let num = r
                        .bytes()
                        .take_while(|b| b.is_ascii_digit() || *b == b'.')
                        .count();
                    let unit = r[num..].bytes().take_while(u8::is_ascii_lowercase).count();
                    let len = num + unit;
                    if num > 0 && unit > 0 && r[len..].starts_with('{') {
                        skip = Some(cmd.len() + len);
                    }
                }
            }
            if let Some(n) = skip {
                i += n;
                continue;
            }
        }
        let c = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..c]);
        i += c;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(line: &str) -> Vec<(&str, bool)> {
        let b = line.as_bytes();
        let mut out = vec![];
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if let Some(s) = span_at(b, i) {
                out.push((&line[s.content()], s.display));
                i = s.end();
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn dollars_that_are_math() {
        assert_eq!(spans("$x^2$"), [("x^2", false)]);
        assert_eq!(spans("a $x$ and $$y$$ b"), [("x", false), ("y", true)]);
        // Prices stay text.
        assert_eq!(spans("costs $5 and $10"), []);
        assert_eq!(spans("$5, $10 or $20"), []);
        // A space after the opening or before the closing `$`.
        assert_eq!(spans("$ x$"), []);
        assert_eq!(spans("$x $"), []);
        // Escaped dollars, and dollars inside braces.
        assert_eq!(spans(r"\$x$ $a\$b$"), [(r"a\$b", false)]);
        assert_eq!(spans(r"$\text{a $b$ c}$"), [(r"\text{a $b$ c}", false)]);
        // An unbalanced brace ends what can close.
        assert_eq!(spans("$}$2+2$"), [("2+2", false)]);
        // `$$` that doesn't close opens inline math with its second `$`.
        assert_eq!(spans("$$x$"), [("x", false)]);
        assert_eq!(spans("$$ x $$"), [(" x ", true)]);
        assert_eq!(spans("$$"), []);
        assert_eq!(spans("$a$$b$"), [("a", false), ("b", false)]);
    }

    #[test]
    fn blocks_of_display_math() {
        assert!(opens_block("$$"));
        assert!(opens_block("  $$ x = 1"));
        assert!(!opens_block("$$x$$"));
        assert!(!opens_block("    $$"));
        assert!(!opens_block("a $$"));
        assert_eq!(closing("x $$ after"), Some(2));
        assert_eq!(closing(r"\$$"), None);
        assert_eq!(
            block_formula(&["$$", "x^2", "+ 1$$"]).as_deref(),
            Some("\nx^2\n+ 1")
        );
        assert_eq!(block_formula(&["$$ a", "$$"]).as_deref(), Some(" a\n"));
        assert_eq!(block_formula(&["$$", "x"]), None);
    }

    #[test]
    fn formulas_as_mathml() {
        let m = mathml(r"\frac{a}{b}", false).unwrap();
        assert!(m.starts_with("<math") && m.contains("<mfrac>"), "{m}");
        assert!(mathml("x", true).unwrap().contains("display=\"block\""));
        // Text is text.
        let m = mathml(r"\text{<img src=x onerror=alert(1)>}", false).unwrap();
        assert!(!m.contains("<img"), "{m}");
        // An unknown command shows; a broken formula doesn't convert.
        assert!(mathml(r"\nosuchthing + 1", false).is_ok());
        assert!(mathml(r"\frac{a", false).is_err());
        // Balanced braces (so it is math), but a missing argument.
        assert!(mathml(r"\frac{1}", false).is_err());
        let mut out = String::new();
        html(r"\frac{<a", true, &mut out);
        assert!(out.starts_with("<span class=\"merr\" title=\""), "{out}");
        assert!(out.ends_with(">$$\\frac{&lt;a$$</span>"), "{out}");
    }

    #[test]
    fn formulas_too_deep_for_the_stack_are_refused() {
        for deep in [
            "{".repeat(60) + &"}".repeat(60),
            r"\frac{".repeat(1000) + &"}{x}".repeat(1000),
            r"\sqrt".repeat(200) + "x",
            r"\left(".repeat(60) + &r"\right)".repeat(60),
            "x^{".repeat(10_000) + &"}".repeat(10_000),
        ] {
            assert!(mathml(&deep, false).is_err());
        }
        // Long, but flat.
        let long = r"\alpha + ".repeat(2_000) + "1";
        assert!(mathml(&long, false).is_ok());
        let ok = r"\frac{".repeat(MAX_DEPTH / 2) + &"}{x}".repeat(MAX_DEPTH / 2);
        assert!(mathml(&ok, false).is_ok());
    }

    #[test]
    fn chemistry() {
        for ce in [
            r"\ce{H2O}",
            r"\ce{CO2 + C -> 2 CO}",
            r"\ce{Hg^2+ ->[I-] HgI2 ->[I-] [Hg^{II}I4]^2-}",
            r"\ce{SO4^2- + Ba^2+ -> BaSO4 v}",
            r"\ce{A <=> B}",
            r"\ce{A <=>>[x][y] B}",
            r"\ce{A <<=>[above] B}",
            r"\ce{A <--> B}",
            r"\ce{^{227}_{90}Th+}",
            r"\ce{^{14}C}",
            r"\ce{A-B=C#D}",
            r"\ce{A~B~-C~=D~--E-~-F}",
            r"\ce{KCr(SO4)2*12H2O}",
            r"\ce{Zn^2+ <=>[+ 2OH-][+ 2H+] $\underset{\text{amphoteres Hydroxid}}{\ce{Zn(OH)2 v}}$}",
            r"K = \frac{[\ce{Hg^2+}][\ce{Hg}]}{[\ce{Hg2^2+}]}",
            r"\pu{123 kJ/mol}",
            r"\pu{1.2e3 kJ}",
            r"m_{\ce{H2O}} = \pu{1.2kg}",
        ] {
            let m = mathml(ce, false);
            assert!(m.is_ok(), "{ce}: {m:?}");
            let m = m.unwrap();
            assert!(!m.contains("unknown") && !m.contains("ce{"), "{ce}: {m}");
        }
        // Whatever mhchem makes of a broken formula, it neither panics nor
        // writes TeX that is left unconverted.
        for broken in [r"\ce{A ->[}", r"\ce{", r"\ce{^}", r"\pu{}", r"\ce{$}"] {
            if let Ok(m) = mathml(broken, false) {
                assert!(!m.contains("\\ce"), "{broken}: {m}");
            }
        }
    }

    #[test]
    fn mhchem_for_math_core() {
        assert_eq!(
            for_math_core(r"{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"),
            r"{\mathrm{H}{\vphantom{A}}_{{2}}\mathrm{O}}"
        );
        assert_eq!(
            for_math_core(
                r"{\vphantom{A}}^{\hphantom{227}}_{\hphantom{90}}\mkern-1.5mu{\vphantom{A}}^{\smash[t]{\vphantom{2}}\llap{227}}_{\vphantom{2}\llap{\smash[t]{90}}}\mathrm{Th}"
            ),
            r"{\vphantom{A}}^{{\vphantom{2}}{227}}_{\vphantom{2}{{90}}}\mathrm{Th}"
        );
        assert_eq!(
            for_math_core(
                r"{}\mathrel{\underset{\lower6mu{y}}{\overset{x}{\longrightleftharpoons}}}{}"
            ),
            r"{}\mathrel{\xrightleftharpoons[y]{x}}{}"
        );
        assert_eq!(
            for_math_core(r"\mathrel{\overset{x}{\longLeftrightharpoons}}"),
            r"\mathrel{\xrightleftharpoons{x}}"
        );
        assert_eq!(
            for_math_core(r"\mathrel{\underset{\lower2mu{y}}{\longleftrightarrows}}"),
            r"\mathrel{\underset{{y}}{\leftrightarrows}}"
        );
        assert_eq!(
            for_math_core(r"{}\mathrel{\longrightleftharpoons}{}"),
            r"{}\mathrel{\rightleftharpoons}{}"
        );
        // Not mhchem's: left alone.
        assert_eq!(
            for_math_core(r"\raise x \smashed{y}"),
            r"\raise x \smashed{y}"
        );
    }
}
