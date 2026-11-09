// Part of notekpr-mhchem, a Rust port of mhchemParser 4.2.2 (Copyright
// 2015-2023 Martin Hensel, Apache License 2.0: see lib.rs and LICENSE.txt).
// Modified: translated to Rust.

//! The patterns the state machines try at the start of their input:
//! hand-written matchers for mhchemParser's regular expressions and its
//! `findObserveGroups`.
//!
//! They follow JavaScript's rules: `\s` is JavaScript's whitespace, `.`
//! stops at line ends, and where a regular expression would backtrack to a
//! different match, so do these.

use crate::Error;
use crate::parser::Parser;

/// mhchemParser's patterns, by the names its tables use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pat {
    /// `empty`
    Empty,
    /// `else`, `else2`
    Else,
    /// `space`
    Space,
    /// `space A`
    SpaceA,
    /// `space$`
    SpaceEnd,
    /// `a-z`
    LowerLetter,
    /// `x`
    X,
    /// `x$`
    XEnd,
    /// `i$`
    IEnd,
    /// `letters`
    Letters,
    /// `\greek`
    Greek,
    /// `digits`
    Digits,
    /// `-9.,9`
    Number,
    /// `-9.,9 no missing 0`
    NumberNoMissing0,
    /// `(-)(9.,9)(e)(99)`
    ENumber,
    /// `(-)(9)^(-9)`
    PowNumber,
    /// `state of aggregation $`
    StateOfAggregation,
    /// `_{(state of aggregation)}$`
    SubStateOfAggregation,
    /// `{[(`
    Open,
    /// `)]}`
    Close,
    /// `, `
    CommaSpace,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `. __* `
    AdditionDot,
    /// `...`
    Ellipsis,
    /// `^{(...)}`
    SupBraces,
    /// `^($...$)`
    SupDollars,
    /// `^a`
    SupA,
    /// `^\x{}{}`
    SupCmd2,
    /// `^\x{}`
    SupCmd1,
    /// `^\x`
    SupCmd,
    /// `^(-1)`
    SupInt,
    /// `'`
    Prime,
    /// `_{(...)}`
    SubBraces,
    /// `_($...$)`
    SubDollars,
    /// `_9`
    Sub9,
    /// `_\x{}{}`
    SubCmd2,
    /// `_\x{}`
    SubCmd1,
    /// `_\x`
    SubCmd,
    /// `^_`
    SupSubAlone,
    /// `{}^`
    EmptyBracesSup,
    /// `{}`
    EmptyBraces,
    /// `{...}`
    Braces,
    /// `{(...)}`
    BracesInner,
    /// `$...$`
    Dollars,
    /// `${(...)}$__$(...)$`
    DollarsInner,
    /// `=<>`
    BondChar,
    /// `#`
    Hash,
    /// `+`
    Plus,
    /// `-$`
    MinusEnd,
    /// `-9`
    MinusDigit,
    /// `- orbital overlap`
    MinusOrbital,
    /// `-`
    Minus,
    /// `pm-operator`
    PmOperator,
    /// `operator`
    Operator,
    /// `arrowUpDown`
    ArrowUpDown,
    /// `\bond{(...)}`
    BondCmd,
    /// `->`
    Arrow,
    /// `CMT`
    Cmt,
    /// `[(...)]`
    Brackets,
    /// `1st-level escape`
    Escape,
    /// `\,`
    Spacing,
    /// `\x{}{}`
    Cmd2,
    /// `\x{}`
    Cmd1,
    /// `\ca`
    Ca,
    /// `\x`
    Cmd,
    /// `orbital`
    Orbital,
    /// `others`
    Others,
    /// `\frac{(...)}`
    Frac,
    /// `\overset{(...)}`
    Overset,
    /// `\underset{(...)}`
    Underset,
    /// `\underbrace{(...)}`
    Underbrace,
    /// `\color{(...)}`
    Color1,
    /// `\color{(...)}{(...)}`
    Color2,
    /// `\ce{(...)}`
    CeCmd,
    /// `\pu{(...)}`
    PuCmd,
    /// `oxidation$`
    OxidationEnd,
    /// `1/2$`
    HalfEnd,
    /// `amount`, `amount2`
    Amount,
    /// `(KV letters),`
    KvLetters,
    /// `formula$`
    FormulaEnd,
    /// `uprightEntities`
    Upright,
    /// `/`
    Slash,
    /// `//`
    DoubleSlash,
    /// `*`
    Star,
}

/// What a pattern matched: mhchemParser's `match_`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mv<'a> {
    /// A string (the whole match, or its one group).
    S(&'a str),
    /// Two groups, as `\frac{…}{…}`.
    Two(&'a str, &'a str),
    /// The numbered groups of a number pattern (unmatched ones `None`).
    Groups([Option<&'a str>; 6]),
}

impl<'a> Mv<'a> {
    /// The matched string ("" for the other shapes, which no action that
    /// wants a string is given).
    pub(crate) fn s(self) -> &'a str {
        match self {
            Mv::S(s) => s,
            _ => "",
        }
    }

    pub(crate) fn two(self) -> (&'a str, &'a str) {
        match self {
            Mv::Two(a, b) => (a, b),
            _ => ("", ""),
        }
    }

    pub(crate) fn groups(self) -> [Option<&'a str>; 6] {
        match self {
            Mv::Groups(g) => g,
            _ => [None; 6],
        }
    }
}

/// A match and the input after it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Matched<'a> {
    pub(crate) m: Mv<'a>,
    pub(crate) rest: &'a str,
}

/// The first `n` bytes matched, the whole match is the value.
fn whole(s: &str, n: usize) -> Matched<'_> {
    Matched {
        m: Mv::S(&s[..n]),
        rest: &s[n..],
    }
}

/// The first `n` bytes matched, the group `from..to` is the value.
fn group(s: &str, from: usize, to: usize, n: usize) -> Matched<'_> {
    Matched {
        m: Mv::S(&s[from..to]),
        rest: &s[n..],
    }
}

/// JavaScript's `\s`.
pub(crate) fn is_ws(c: char) -> bool {
    // Tab, line feed, vertical tab, form feed, carriage return; then the
    // Unicode spaces and line separators JavaScript counts.
    let spaces = matches!(
        c,
        '\u{a0}' | '\u{1680}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
    );
    matches!(c, '\t'..='\r' | ' ' | '\u{2000}'..='\u{200a}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
        || spaces
}

/// The characters JavaScript's `.` doesn't match.
fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn first(s: &str) -> Option<char> {
    s.chars().next()
}

fn starts_with_digit(s: &str) -> bool {
    first(s).is_some_and(|c| c.is_ascii_digit())
}

fn starts_with_sign(s: &str) -> bool {
    s.starts_with(['+', '-'])
}

/// Bytes of whitespace at the start of `s`.
fn ws_len(s: &str) -> usize {
    s.len() - s.trim_start_matches(is_ws).len()
}

/// Bytes of ASCII digits at the start of `s`.
fn digits_len(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_digit).count()
}

/// Bytes of ASCII letters at the start of `s`.
fn letters_len(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_alphabetic).count()
}

/// Bytes of lowercase ASCII letters at the start of `s`.
fn lower_len(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_lowercase).count()
}

/// Bytes of lowercase ASCII letters at the start of `s`, counting no
/// further than `max` (enough to tell whether there are more).
fn lower_len_upto(s: &str, max: usize) -> usize {
    s.bytes()
        .take(max)
        .take_while(u8::is_ascii_lowercase)
        .count()
}

/// `(?![a-zA-Z])`
fn no_letter_follows(s: &str) -> bool {
    !first(s).is_some_and(|c| c.is_ascii_alphabetic())
}

const LOWER_GREEK: [&str; 24] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi", "chi",
    "psi", "omega",
];
const UPPER_GREEK: [&str; 11] = [
    "Gamma", "Delta", "Theta", "Lambda", "Xi", "Pi", "Sigma", "Upsilon", "Phi", "Psi", "Omega",
];

/// `\` and a Greek letter's name (none is a prefix of another, so the
/// order of the alternatives doesn't matter): its length.
fn greek_name(s: &str, upper_too: bool) -> Option<usize> {
    let rest = s.strip_prefix('\\')?;
    let names = LOWER_GREEK
        .iter()
        .chain(if upper_too { &UPPER_GREEK[..] } else { &[] });
    names
        .into_iter()
        .find(|n| rest.starts_with(**n))
        .map(|n| 1 + n.len())
}

/// `(?:\s+|\{\}|(?![a-zA-Z]))` after a Greek letter's name.
fn greek_end(s: &str) -> Option<usize> {
    let ws = ws_len(s);
    if ws > 0 {
        Some(ws)
    } else if s.starts_with("{}") {
        Some(2)
    } else {
        no_letter_follows(s).then_some(0)
    }
}

/// `\greek`: `\alpha`, `\Omega`, … with what may end it.
fn greek(s: &str) -> Option<usize> {
    let n = greek_name(s, true)?;
    Some(n + greek_end(&s[n..])?)
}

fn is_greek_lower(c: char) -> bool {
    ('\u{3b1}'..='\u{3c9}').contains(&c)
}

fn is_greek_upper(c: char) -> bool {
    ('\u{391}'..='\u{3a9}').contains(&c)
}

/// A number without a sign: `[0-9]+(?:[,.][0-9]+)?` or `[0-9]*(?:\.[0-9]+)`.
fn unsigned_number(s: &str) -> Option<usize> {
    let d = digits_len(s);
    if d > 0 {
        let rest = &s[d..];
        if rest.starts_with([',', '.']) {
            let d2 = digits_len(&rest[1..]);
            if d2 > 0 {
                return Some(d + 1 + d2);
            }
        }
        Some(d)
    } else if let Some(fraction) = s.strip_prefix('.') {
        let d2 = digits_len(fraction);
        (d2 > 0).then_some(1 + d2)
    } else {
        None
    }
}

/// `(\+\-|\+\/\-|\+|\-|\\pm\s?)`
fn number_sign(s: &str) -> Option<usize> {
    for p in ["+-", "+/-", "+", "-"] {
        if s.starts_with(p) {
            return Some(p.len());
        }
    }
    let rest = s.strip_prefix("\\pm")?;
    Some(3 + first(rest).filter(|c| is_ws(*c)).map_or(0, char::len_utf8))
}

/// An exponent: `[+\-]?[0-9]+` or `\{[+\-]?[0-9]+\}`.
fn exponent(s: &str) -> Option<usize> {
    let signed = |s: &str| {
        let sign = usize::from(starts_with_sign(s));
        let d = digits_len(&s[sign..]);
        (d > 0).then_some(sign + d)
    };
    if let Some(n) = signed(s) {
        return Some(n);
    }
    let inner = s.strip_prefix('{')?;
    let n = signed(inner)?;
    inner[n..].starts_with('}').then_some(n + 2)
}

/// `(?=$|[\s,;\)\]\}])`
fn phrase_ends(s: &str) -> bool {
    match first(s) {
        None => true,
        Some(c) => is_ws(c) || matches!(c, ',' | ';' | ')' | ']' | '}'),
    }
}

/// `one lowercase latin letter $`: `^(?:([a-z])(?:$|[^a-zA-Z]))$`
pub(crate) fn one_latin_letter(s: &str) -> bool {
    let mut cs = s.chars();
    match (cs.next(), cs.next(), cs.next()) {
        (Some(a), None, _) => a.is_ascii_lowercase(),
        (Some(a), Some(b), None) => a.is_ascii_lowercase() && !b.is_ascii_alphabetic(),
        _ => false,
    }
}

/// `$one lowercase latin letter$ $`: `^\$(?:([a-z])(?:$|[^a-zA-Z]))\$$`
/// (only `$`, a letter, a character that isn't one, and `$` can match).
pub(crate) fn dollar_latin_letter(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(
        (cs.next(), cs.next(), cs.next(), cs.next(), cs.next()),
        (Some('$'), Some(a), Some(b), Some('$'), None) if a.is_ascii_lowercase() && !b.is_ascii_alphabetic()
    )
}

/// `one lowercase greek letter $`:
/// `^(?:\$?[α-ω]\$?|\$?\\(?:alpha|…|omega)\s*\$?)(?:\s+|\{\}|(?![a-zA-Z]))$`
pub(crate) fn one_greek_letter(s: &str) -> bool {
    // After an optional `$`, the rest must be whitespace (or nothing) or `{}`.
    let ends = |r: &str| {
        let r = r.strip_prefix('$').unwrap_or(r);
        r.chars().all(is_ws) || r == "{}"
    };
    let t = s.strip_prefix('$').unwrap_or(s);
    if let Some(c) = first(t)
        && is_greek_lower(c)
        && ends(&t[c.len_utf8()..])
    {
        return true;
    }
    if let Some(n) = greek_name(t, false) {
        let r = t[n..].trim_start_matches(is_ws);
        if r.starts_with('$') {
            return ends(r);
        }
        return r.is_empty() || r == "{}";
    }
    false
}

/// `d-oxidation$` (with `sign_optional`) and `oxidation$`:
/// `^(?:[+-]?[IVX]+|(?:\\pm|\$\\pm\$|\+-|\+\/-)\s*0)$`
pub(crate) fn oxidation(s: &str, sign_optional: bool) -> bool {
    let numeral = if starts_with_sign(s) {
        Some(&s[1..])
    } else if sign_optional {
        Some(s)
    } else {
        None
    };
    if numeral.is_some_and(|n| !n.is_empty() && n.bytes().all(|c| matches!(c, b'I' | b'V' | b'X')))
    {
        return true;
    }
    ["\\pm", "$\\pm$", "+-", "+/-"].iter().any(|p| {
        s.strip_prefix(p)
            .is_some_and(|r| r.trim_start_matches(is_ws) == "0")
    })
}

/// `1/2$`: `^[+\-]?(?:[0-9]+|\$[a-z]\$|[a-z])\/[0-9]+(?:\$[a-z]\$|[a-z])?$`
fn half(s: &str) -> bool {
    let s = if starts_with_sign(s) { &s[1..] } else { s };
    let Some(rest) = numerator(s).map(|n| &s[n..]) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('/') else {
        return false;
    };
    let d = digits_len(rest);
    if d == 0 {
        return false;
    }
    let rest = &rest[d..];
    rest.is_empty() || variable(rest) == Some(rest.len())
}

/// `[0-9]+|\$[a-z]\$|[a-z]`, the top of a fraction like `1/2` or `n/2`.
fn numerator(s: &str) -> Option<usize> {
    let d = digits_len(s);
    if d > 0 { Some(d) } else { variable(s) }
}

/// `\$[a-z]\$|[a-z]`
fn variable(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() >= 3 && b[0] == b'$' && b[1].is_ascii_lowercase() && b[2] == b'$' {
        Some(3)
    } else if b.first().is_some_and(u8::is_ascii_lowercase) {
        Some(1)
    } else {
        None
    }
}

/// The variable amounts `amount` accepts in `$…$` (the text between):
/// `\(?[+\-]?(?:[0-9]*[a-z]?[+\-])?[0-9]*[a-z](?:[+\-][0-9]*[a-z]?)?\)?`
fn amount_variable(s: &str) -> bool {
    let s = s.strip_prefix('(').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s).as_bytes();
    let n = s.len();
    let sign = |i: usize| i < n && matches!(s[i], b'+' | b'-');
    let lower = |i: usize| i < n && s[i].is_ascii_lowercase();
    let digits = |mut i: usize| {
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    for lead in [true, false] {
        if lead && !sign(0) {
            continue;
        }
        let i = usize::from(lead);
        for prefix in [true, false] {
            let mut j = i;
            if prefix {
                j = digits(j);
                if lower(j) && sign(j + 1) {
                    j += 2;
                } else if sign(j) {
                    j += 1;
                } else {
                    continue;
                }
            }
            let k = digits(j);
            if !lower(k) {
                continue;
            }
            let k = k + 1;
            if k == n {
                return true;
            }
            if sign(k) {
                let mut t = digits(k + 1);
                if lower(t) {
                    t += 1;
                }
                if t == n {
                    return true;
                }
            }
        }
    }
    false
}

/// `[spdfgh]` and the like after a number: `orbital`.
/// `^(?:[0-9]{1,2}[spdfgh]|[0-9]{0,2}sp)(?=$|[^a-zA-Z])`
fn orbital(s: &str) -> Option<usize> {
    let d = s.bytes().take(3).take_while(u8::is_ascii_digit).count();
    if d > 2 {
        return None;
    }
    let rest = &s[d..];
    if d > 0 && rest.starts_with(['s', 'p', 'd', 'f', 'g', 'h']) && no_letter_follows(&rest[1..]) {
        return Some(d + 1);
    }
    (rest.starts_with("sp") && no_letter_follows(&rest[2..])).then_some(d + 2)
}

/// `formula$`: a formula of lowercase letters, digits and `+-,.() `.
fn formula(s: &str) -> bool {
    // `^\([a-z]+\)$` is a state of aggregation, not a formula.
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')'))
        && !inner.is_empty()
        && inner.bytes().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    // `^(?:[a-z]|(?:[P]+[a-z])+[P]*|(?:[a-z][P]+)+[a-z]?)$` with P = `[0-9 +\-,.()]`
    // is: only those characters, no two letters in a row, at least one letter.
    let mut letters = 0;
    let mut previous_letter = false;
    for c in s.bytes() {
        if c.is_ascii_lowercase() {
            if previous_letter {
                return false;
            }
            letters += 1;
            previous_letter = true;
        } else if c.is_ascii_digit() || b" +-,.()".contains(&c) {
            previous_letter = false;
        } else {
            return false;
        }
    }
    letters > 0
}

/// What `findObserveGroups` looks for at a place: a literal or one of the
/// few regular expressions it is given.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Pt {
    L(&'static str),
    /// `^\\[a-zA-Z]+\{`
    CmdBrace,
    /// `^(?=\{)`
    BeforeBrace,
    /// `^\([a-z]{1,3}(?=[\),])`
    StateOpen,
}

fn pt(s: &str, p: Pt) -> Option<usize> {
    match p {
        Pt::L(l) => s.starts_with(l).then_some(l.len()),
        Pt::CmdBrace => {
            let rest = s.strip_prefix('\\')?;
            let l = letters_len(rest);
            (l > 0 && rest[l..].starts_with('{')).then_some(l + 2)
        }
        Pt::BeforeBrace => s.starts_with('{').then_some(0),
        Pt::StateOpen => {
            let rest = s.strip_prefix('(')?;
            let l = lower_len_upto(rest, 4);
            ((1..=3).contains(&l) && rest[l..].starts_with([')', ','])).then_some(1 + l)
        }
    }
}

impl Parser {
    /// mhchemParser's `findObserveGroups` for one group: after `beg_excl`,
    /// from `beg_incl` to the first `end` outside braces. Returns the group
    /// (with `beg_incl`, and with `end` if `end_incl`) and the rest.
    pub(crate) fn find_group<'a>(
        &mut self,
        s: &'a str,
        beg_excl: Pt,
        beg_incl: Pt,
        end: Pt,
        end_incl: bool,
    ) -> Result<Option<(&'a str, &'a str)>, Error> {
        let Some(l1) = pt(s, beg_excl) else {
            return Ok(None);
        };
        let s = &s[l1..];
        let Some(l2) = pt(s, beg_incl) else {
            return Ok(None);
        };
        let mut braces = 0usize;
        let mut i = l2;
        while let Some(c) = first(&s[i..]) {
            self.work(1)?;
            if braces == 0
                && let Some(l) = pt(&s[i..], end)
            {
                let to = if end_incl { i + l } else { i };
                return Ok(Some((&s[..to], &s[i + l..])));
            }
            match c {
                '{' => braces += 1,
                '}' if braces == 0 => return Err(Error::unbalanced_brace()),
                '}' => braces -= 1,
                _ => {}
            }
            i += c.len_utf8();
        }
        Ok(None)
    }

    /// A group between literals, as a match.
    fn lit_group<'a>(
        &mut self,
        s: &'a str,
        beg_excl: &'static str,
        beg_incl: &'static str,
        end: &'static str,
        end_incl: bool,
    ) -> Result<Option<Matched<'a>>, Error> {
        Ok(self
            .find_group(s, Pt::L(beg_excl), Pt::L(beg_incl), Pt::L(end), end_incl)?
            .map(|(m, rest)| Matched { m: Mv::S(m), rest }))
    }

    /// `\cmd{…}{…}` after `prefix`, as one string.
    fn cmd2<'a>(&mut self, s: &'a str, prefix: &'static str) -> Result<Option<Matched<'a>>, Error> {
        let Some((_, rest)) = self.find_group(s, Pt::L(prefix), Pt::CmdBrace, Pt::L("}"), true)?
        else {
            return Ok(None);
        };
        let Some((_, rest)) = self.find_group(rest, Pt::L(""), Pt::L("{"), Pt::L("}"), true)?
        else {
            return Ok(None);
        };
        Ok(Some(Matched {
            m: Mv::S(&s[prefix.len()..s.len() - rest.len()]),
            rest,
        }))
    }

    /// `\cmd{…}` after `prefix`.
    fn cmd1<'a>(&mut self, s: &'a str, prefix: &'static str) -> Result<Option<Matched<'a>>, Error> {
        Ok(self
            .find_group(s, Pt::L(prefix), Pt::CmdBrace, Pt::L("}"), true)?
            .map(|(m, rest)| Matched { m: Mv::S(m), rest }))
    }

    /// `\cmd{…}{…}` with a known command: the two arguments.
    fn pair<'a>(
        &mut self,
        s: &'a str,
        open: &'static str,
        close: &'static str,
    ) -> Result<Option<Matched<'a>>, Error> {
        let Some((m1, rest)) = self.find_group(s, Pt::L(open), Pt::L(""), Pt::L(close), false)?
        else {
            return Ok(None);
        };
        Ok(self
            .find_group(rest, Pt::L("{"), Pt::L(""), Pt::L("}"), false)?
            .map(|(m2, rest)| Matched {
                m: Mv::Two(m1, m2),
                rest,
            }))
    }

    /// Tries a pattern at the start of `s`.
    pub(crate) fn pattern<'a>(
        &mut self,
        pat: Pat,
        s: &'a str,
    ) -> Result<Option<Matched<'a>>, Error> {
        use Pat as P;
        self.work(1)?;
        let found = match pat {
            P::Empty => s.is_empty().then(|| whole(s, 0)),
            P::Else => first(s)
                .filter(|c| !is_line_end(*c))
                .map(|c| whole(s, c.len_utf8())),
            P::Space => first(s)
                .filter(|c| is_ws(*c))
                .map(|c| whole(s, c.len_utf8())),
            P::SpaceA => first(s)
                .filter(|c| is_ws(*c))
                .map(char::len_utf8)
                .filter(|n| matches!(first(&s[*n..]), Some('A'..='Z' | '\\' | '$')))
                .map(|n| whole(s, n)),
            P::SpaceEnd => first(s)
                .filter(|c| is_ws(*c) && c.len_utf8() == s.len())
                .map(|_| whole(s, s.len())),
            P::LowerLetter => first(s)
                .filter(char::is_ascii_lowercase)
                .map(|_| whole(s, 1)),
            P::X => s.starts_with('x').then(|| whole(s, 1)),
            P::XEnd => (s == "x").then(|| whole(s, 1)),
            P::IEnd => (s == "i").then(|| whole(s, 1)),
            P::Letters => self.letters(s).map(|n| whole(s, n)),
            P::Greek => greek(s).map(|n| whole(s, n)),
            P::Digits => {
                let n = digits_len(s);
                self.work(n)?;
                (n > 0).then(|| whole(s, n))
            }
            P::Number | P::NumberNoMissing0 => {
                let sign = usize::from(starts_with_sign(s));
                let rest = &s[sign..];
                let n = if pat == P::Number || starts_with_digit(rest) {
                    unsigned_number(rest)
                } else {
                    None
                };
                self.work(n.unwrap_or(0))?;
                n.map(|n| whole(s, sign + n))
            }
            P::ENumber => {
                let (found, scanned) = e_number(s);
                self.work(scanned)?;
                found
            }
            P::PowNumber => pow_number(s),
            P::StateOfAggregation => self.state_of_aggregation(s)?,
            P::SubStateOfAggregation => {
                let rest = s.strip_prefix("_{(").unwrap_or("");
                let l = lower_len_upto(rest, 4);
                ((1..=3).contains(&l) && rest[l..].starts_with(")}"))
                    .then(|| group(s, 2, 4 + l, 5 + l))
            }
            P::Open => ["\\{", "[", "("]
                .iter()
                .find(|p| s.starts_with(**p))
                .map(|p| whole(s, p.len())),
            P::Close => [")", "]", "\\}"]
                .iter()
                .find(|p| s.starts_with(**p))
                .map(|p| whole(s, p.len())),
            P::CommaSpace => s
                .starts_with([',', ';'])
                .then(|| whole(s, 1 + ws_len(&s[1..]))),
            P::Comma => s.starts_with([',', ';']).then(|| whole(s, 1)),
            P::Dot => s.starts_with('.').then(|| whole(s, 1)),
            P::AdditionDot => first(s)
                .filter(|c| matches!(c, '.' | '\u{22c5}' | '\u{b7}' | '\u{2022}' | '*'))
                .map(char::len_utf8)
                .map(|n| group(s, 0, n, n + ws_len(&s[n..]))),
            P::Ellipsis => (s.starts_with("...") && !s[3..].starts_with('.')).then(|| whole(s, 3)),
            P::SupBraces => self.lit_group(s, "^{", "", "}", false)?,
            P::SupDollars => self.lit_group(s, "^", "$", "$", true)?,
            P::SupA => s.strip_prefix('^').and_then(|rest| {
                let d = digits_len(rest);
                if d > 0 {
                    Some(group(s, 1, 1 + d, 1 + d))
                } else {
                    first(rest)
                        .filter(|c| !matches!(c, '\\' | '_'))
                        .map(|c| group(s, 1, 1 + c.len_utf8(), 1 + c.len_utf8()))
                }
            }),
            P::SupCmd2 => self.cmd2(s, "^")?,
            P::SupCmd1 => self.cmd1(s, "^")?,
            P::SupCmd => script_cmd(s, '^'),
            P::SupInt => s.strip_prefix('^').and_then(|rest| {
                let minus = usize::from(rest.starts_with('-'));
                let d = digits_len(&rest[minus..]);
                (d > 0).then(|| group(s, 1, 1 + minus + d, 1 + minus + d))
            }),
            P::Prime => s.starts_with('\'').then(|| whole(s, 1)),
            P::SubBraces => self.lit_group(s, "_{", "", "}", false)?,
            P::SubDollars => self.lit_group(s, "_", "$", "$", true)?,
            P::Sub9 => s.strip_prefix('_').and_then(|rest| {
                let sign = usize::from(starts_with_sign(rest));
                let d = digits_len(&rest[sign..]);
                if d > 0 {
                    Some(group(s, 1, 1 + sign + d, 1 + sign + d))
                } else {
                    first(rest)
                        .filter(|c| *c != '\\')
                        .map(|c| group(s, 1, 1 + c.len_utf8(), 1 + c.len_utf8()))
                }
            }),
            P::SubCmd2 => self.cmd2(s, "_")?,
            P::SubCmd1 => self.cmd1(s, "_")?,
            P::SubCmd => script_cmd(s, '_'),
            P::SupSubAlone => (s.starts_with("^_") || s.starts_with("_^") || s == "^" || s == "_")
                .then(|| whole(s, 1)),
            P::EmptyBracesSup => s.starts_with("{}^").then(|| whole(s, 2)),
            P::EmptyBraces => s.starts_with("{}").then(|| whole(s, 2)),
            P::Braces => self.lit_group(s, "", "{", "}", true)?,
            P::BracesInner => self.lit_group(s, "{", "", "}", false)?,
            P::Dollars => self.lit_group(s, "", "$", "$", true)?,
            P::DollarsInner => match self.lit_group(s, "${", "", "}$", false)? {
                Some(m) => Some(m),
                None => self.lit_group(s, "$", "", "$", false)?,
            },
            P::BondChar => s.starts_with(['=', '<', '>']).then(|| whole(s, 1)),
            P::Hash => first(s)
                .filter(|c| matches!(c, '#' | '\u{2261}'))
                .map(|c| whole(s, c.len_utf8())),
            P::Plus => s.starts_with('+').then(|| whole(s, 1)),
            P::MinusEnd => s.strip_prefix('-').and_then(|rest| {
                let ends = match first(rest) {
                    None => true,
                    Some(c) if is_ws(c) || matches!(c, '_' | '}' | ',' | ';' | ']' | '/') => true,
                    Some('(') => {
                        let l = lower_len(&rest[1..]);
                        l > 0 && rest[1 + l..].starts_with(')')
                    }
                    _ => false,
                };
                ends.then(|| whole(s, 1))
            }),
            P::MinusDigit => {
                (s.starts_with('-') && starts_with_digit(&s[1..])).then(|| whole(s, 1))
            }
            P::MinusOrbital => s.strip_prefix('-').and_then(|rest| {
                let one = rest.starts_with(['s', 'p', 'd']) && phrase_ends(&rest[1..]);
                let sp = rest.starts_with("sp") && phrase_ends(&rest[2..]);
                (one || sp).then(|| whole(s, 1))
            }),
            P::Minus => s.starts_with('-').then(|| whole(s, 1)),
            P::PmOperator => ["\\pm", "$\\pm$", "+-", "+/-"]
                .iter()
                .find(|p| s.starts_with(**p))
                .map(|p| whole(s, p.len())),
            P::Operator => operator(s).map(|n| whole(s, n)),
            P::ArrowUpDown => ["v", "(v)", "^", "(^)"]
                .iter()
                .find(|p| s.starts_with(**p) && phrase_ends(&s[p.len()..]))
                .map(|p| whole(s, p.len())),
            P::BondCmd => self.lit_group(s, "\\bond{", "", "}", false)?,
            P::Arrow => [
                "<->", "<-->", "->", "<-", "<=>>", "<<=>", "<=>", "\u{2192}", "\u{27f6}",
                "\u{21cc}",
            ]
            .iter()
            .find(|p| s.starts_with(**p))
            .map(|p| whole(s, p.len())),
            P::Cmt => {
                (s.starts_with(['C', 'M', 'T']) && s[1..].starts_with('[')).then(|| whole(s, 1))
            }
            P::Brackets => self.lit_group(s, "[", "", "]", false)?,
            P::Escape => ["&", "\\\\", "\\hline"]
                .iter()
                .find(|p| s.starts_with(**p))
                .map(|p| group(s, 0, p.len(), p.len() + ws_len(&s[p.len()..]))),
            P::Spacing => s
                .strip_prefix('\\')
                .filter(|r| r.starts_with([',', ' ', ';', ':']))
                .map(|_| whole(s, 2)),
            P::Cmd2 => self.cmd2(s, "")?,
            P::Cmd1 => self.cmd1(s, "")?,
            P::Ca => s.strip_prefix("\\ca").and_then(|rest| {
                let ws = ws_len(rest);
                (ws > 0 || no_letter_follows(rest)).then(|| whole(s, 3 + ws))
            }),
            P::Cmd => s.strip_prefix('\\').and_then(|rest| {
                let l = letters_len(rest);
                if l > 0 {
                    Some(whole(s, 1 + l + ws_len(&rest[l..])))
                } else {
                    rest.starts_with(['_', '&', '{', '}', '%'])
                        .then(|| whole(s, 2))
                }
            }),
            P::Orbital => orbital(s).map(|n| whole(s, n)),
            P::Others => s.starts_with(['/', '~', '|']).then(|| whole(s, 1)),
            P::Frac => self.pair(s, "\\frac{", "}")?,
            P::Overset => self.pair(s, "\\overset{", "}")?,
            P::Underset => self.pair(s, "\\underset{", "}")?,
            P::Underbrace => self.pair(s, "\\underbrace{", "}_")?,
            P::Color1 => self.lit_group(s, "\\color{", "", "}", false)?,
            P::Color2 => match self.pair(s, "\\color{", "}")? {
                Some(m) => Some(m),
                None => match self.find_group(
                    s,
                    Pt::L("\\color"),
                    Pt::L("\\"),
                    Pt::BeforeBrace,
                    false,
                )? {
                    Some((m1, rest)) => self
                        .find_group(rest, Pt::L("{"), Pt::L(""), Pt::L("}"), false)?
                        .map(|(m2, rest)| Matched {
                            m: Mv::Two(m1, m2),
                            rest,
                        }),
                    None => None,
                },
            },
            P::CeCmd => self.lit_group(s, "\\ce{", "", "}", false)?,
            P::PuCmd => self.lit_group(s, "\\pu{", "", "}", false)?,
            // These two look at no more than the number at the start of `s`.
            P::OxidationEnd => oxidation(s, false).then(|| whole(s, s.len())),
            P::HalfEnd => half(s).then(|| whole(s, s.len())),
            P::Amount => self.amount(s)?,
            P::KvLetters => kv_letters(s).map(|n| whole(s, n)),
            P::FormulaEnd => {
                self.work(s.len())?;
                formula(s).then(|| whole(s, s.len()))
            }
            P::Upright => ["pH", "pOH", "pC", "pK", "iPr", "iBu"]
                .iter()
                .find(|p| s.starts_with(**p))
                .filter(|p| no_letter_follows(&s[p.len()..]))
                .map(|p| whole(s, p.len())),
            P::Slash | P::DoubleSlash | P::Star => {
                let ws = ws_len(s);
                self.work(ws)?;
                let rest = &s[ws..];
                let core = match pat {
                    P::Slash => rest.starts_with('/').then_some(1),
                    P::DoubleSlash => rest.starts_with("//").then_some(2),
                    _ => rest.starts_with(['*', '.']).then_some(1),
                };
                core.map(|n| {
                    let end = ws + n + ws_len(&rest[n..]);
                    if pat == P::Star {
                        whole(s, end)
                    } else {
                        group(s, ws, ws + n, end)
                    }
                })
            }
        };
        Ok(found)
    }

    /// `letters`: Latin and Greek letters, `?`, `@` and `\alpha`-style names.
    fn letters(&mut self, s: &str) -> Option<usize> {
        let mut i = 0;
        while let Some(c) = first(&s[i..]) {
            if c.is_ascii_alphabetic()
                || is_greek_lower(c)
                || is_greek_upper(c)
                || c == '?'
                || c == '@'
            {
                i += c.len_utf8();
            } else if let Some(n) = greek(&s[i..]) {
                i += n;
            } else {
                break;
            }
        }
        self.steps += i as u64;
        (i > 0).then_some(i)
    }

    /// `state of aggregation $`: `(aq)`, `(aq, sat)` at the end of a
    /// phrase, or a crystal system like `($c$)`, `(\ca$c$)`.
    fn state_of_aggregation<'a>(&mut self, s: &'a str) -> Result<Option<Matched<'a>>, Error> {
        if let Some((m, rest)) = self.find_group(s, Pt::L(""), Pt::StateOpen, Pt::L(")"), true)?
            && phrase_ends(rest)
        {
            return Ok(Some(Matched { m: Mv::S(m), rest }));
        }
        // `^(?:\((?:\\ca\s?)?\$[amothc]\$\))`
        let Some(rest) = s.strip_prefix('(') else {
            return Ok(None);
        };
        let mut i = 1;
        if let Some(r) = rest.strip_prefix("\\ca") {
            let ws = first(r).filter(|c| is_ws(*c)).map_or(0, char::len_utf8);
            if r[ws..].starts_with('$') {
                i += 3 + ws;
            }
        }
        let b = s.as_bytes();
        let ok = b.len() >= i + 4
            && b[i] == b'$'
            && b"amothc".contains(&b[i + 1])
            && b[i + 2] == b'$'
            && b[i + 3] == b')';
        Ok(ok.then(|| whole(s, i + 4)))
    }

    /// `amount`: a stoichiometric number (`2`, `0.5`, `1/2`, `n`, `+`) or
    /// one in dollars (`$2n-1$`).
    fn amount<'a>(&mut self, s: &'a str) -> Result<Option<Matched<'a>>, Error> {
        if let Some(n) = amount_number(s) {
            return Ok(Some(whole(s, n)));
        }
        if let Some((m, rest)) = self.find_group(s, Pt::L(""), Pt::L("$"), Pt::L("$"), true)? {
            let inner = &m[1..m.len() - 1];
            if inner == "+" || inner == "-" || amount_variable(inner) {
                return Ok(Some(Matched { m: Mv::S(m), rest }));
            }
        }
        Ok(None)
    }
}

/// `^\x` / `_\x`: the script mark, a command (the group) and whitespace.
fn script_cmd(s: &str, mark: char) -> Option<Matched<'_>> {
    let rest = s.strip_prefix(mark)?.strip_prefix('\\')?;
    let l = letters_len(rest);
    if l == 0 {
        return None;
    }
    Some(group(s, 1, 2 + l, 2 + l + ws_len(&rest[l..])))
}

/// `operator`: `+`, or `-`, `=`, `<`, `>`, `<<`, `>>`, `\approx`,
/// `$\approx$` followed by whitespace, the end or a (negative) number.
fn operator(s: &str) -> Option<usize> {
    if s.starts_with('+') {
        return Some(1);
    }
    let follows = |r: &str| {
        r.is_empty()
            || first(r).is_some_and(is_ws)
            || starts_with_digit(r)
            || (r.starts_with('-') && starts_with_digit(&r[1..]))
    };
    let single = s.starts_with(['-', '=', '<', '>']).then_some(1);
    single
        .into_iter()
        .chain(
            ["<<", ">>", "\\approx", "$\\approx$"]
                .iter()
                .filter(|p| s.starts_with(**p))
                .map(|p| p.len()),
        )
        .find(|n| follows(&s[*n..]))
}

/// `(KV letters),`: an element symbol or `i` before a comma.
fn kv_letters(s: &str) -> Option<usize> {
    if first(s).is_some_and(|c| c.is_ascii_uppercase()) {
        let l = lower_len_upto(&s[1..], 3);
        if l <= 2 && s[1 + l..].starts_with(',') {
            return Some(1 + l);
        }
    }
    s.starts_with("i,").then_some(1)
}

/// The number part of `amount`:
/// `(?:(?:N)(?:[a-z](?=\s*[A-Z]))?)|[+\-]?[a-z](?=\s*[A-Z])|\+(?!\s)`
fn amount_number(s: &str) -> Option<usize> {
    // `[a-z](?=\s*[A-Z])`
    let letter_before_formula = |r: &str| {
        r.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && first(r[1..].trim_start_matches(is_ws)).is_some_and(|c| c.is_ascii_uppercase())
    };
    if let Some(n) = amount_value(s) {
        return Some(if letter_before_formula(&s[n..]) {
            n + 1
        } else {
            n
        });
    }
    let sign = usize::from(starts_with_sign(s));
    if letter_before_formula(&s[sign..]) {
        return Some(sign + 1);
    }
    (s.starts_with('+') && !first(&s[1..]).is_some_and(is_ws)).then_some(1)
}

/// The numbers `amount` knows, in its order: `(1/2)`, `1/2` (or `n/2`,
/// `$n$/2`), `0.5`, `.5`, `2`, each but the first with a sign.
fn amount_value(s: &str) -> Option<usize> {
    if let Some(inner) = s.strip_prefix('(') {
        let sign = usize::from(starts_with_sign(inner));
        let d1 = digits_len(&inner[sign..]);
        let rest = &inner[sign + d1..];
        if d1 > 0 && rest.starts_with('/') {
            let d2 = digits_len(&rest[1..]);
            if d2 > 0 && rest[1 + d2..].starts_with(')') {
                return Some(1 + sign + d1 + 1 + d2 + 1);
            }
        }
    }
    let sign = usize::from(starts_with_sign(s));
    let r = &s[sign..];
    if let Some(n) = numerator(r)
        && r[n..].starts_with('/')
    {
        let d = digits_len(&r[n + 1..]);
        if d > 0 {
            return Some(sign + n + 1 + d);
        }
    }
    let d = digits_len(r);
    if d > 0 && r[d..].starts_with([',', '.']) {
        let d2 = digits_len(&r[d + 1..]);
        if d2 > 0 {
            return Some(sign + d + 1 + d2);
        }
    }
    if d == 0 && r.starts_with('.') {
        let d2 = digits_len(&r[1..]);
        if d2 > 0 {
            return Some(sign + 1 + d2);
        }
    }
    (d > 0).then_some(sign + d)
}

/// `(-)(9.,9)(e)(99)`: a number with sign, uncertainty and exponent, as
/// six groups: sign, number, `(uncertainty)`, `e`/`E`, `*`/`x`/`\times`/`×`
/// and the exponent. Also how much whitespace it looked at past the match
/// (for `× 10^`, which may not be there).
fn e_number(s: &str) -> (Option<Matched<'_>>, usize) {
    let mut g: [Option<&str>; 6] = [None; 6];
    let mut scanned = 0;
    let mut i = 0;
    if let Some(n) = number_sign(s) {
        g[0] = Some(&s[..n]);
        i = n;
    }
    if let Some(n) = unsigned_number(&s[i..]) {
        g[1] = Some(&s[i..i + n]);
        i += n;
    }
    if let Some(inner) = s[i..].strip_prefix('(')
        && let Some(n) = unsigned_number(inner)
        && inner[n..].starts_with(')')
    {
        g[2] = Some(&s[i..i + n + 2]);
        i += n + 2;
    }
    let rest = &s[i..];
    if rest.starts_with(['e', 'E']) {
        if let Some(n) = exponent(&rest[1..]) {
            g[3] = Some(&rest[..1]);
            g[5] = Some(&rest[1..1 + n]);
            i += 1 + n;
        }
    } else {
        let ws = ws_len(rest);
        scanned = ws;
        let op = ["*", "x", "\\times", "\u{d7}"]
            .iter()
            .find(|p| rest[ws..].starts_with(**p));
        if let Some(op) = op {
            let after = ws + op.len();
            let ws2 = ws_len(&rest[after..]);
            scanned += ws2;
            if let Some(power) = rest[after + ws2..].strip_prefix("10^")
                && let Some(n) = exponent(power)
            {
                let start = after + ws2 + 3;
                g[4] = Some(&rest[ws..after]);
                g[5] = Some(&rest[start..start + n]);
                i += start + n;
            }
        }
    }
    (
        (i > 0).then(|| Matched {
            m: Mv::Groups(g),
            rest: &s[i..],
        }),
        scanned,
    )
}

/// `(-)(9)^(-9)`: a power of a number, as three groups: sign, number
/// (maybe empty) and exponent.
fn pow_number(s: &str) -> Option<Matched<'_>> {
    let mut g: [Option<&str>; 6] = [None; 6];
    let mut i = 0;
    if let Some(n) = number_sign(s) {
        g[0] = Some(&s[..n]);
        i = n;
    }
    let r = &s[i..];
    let n = if starts_with_digit(r) {
        unsigned_number(r).unwrap_or(0)
    } else {
        unsigned_number(r)
            .filter(|_| r.starts_with('.'))
            .unwrap_or(0)
    };
    g[1] = Some(&r[..n]);
    i += n;
    let power = s[i..].strip_prefix('^')?;
    let e = exponent(power)?;
    g[2] = Some(&power[..e]);
    Some(Matched {
        m: Mv::Groups(g),
        rest: &power[e..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_variables() {
        for ok in ["n", "2n", "2n-1", "+-a", "(n+1)", "x+y", "-", "a-", "2a+3b"] {
            assert!(ok == "-" || amount_variable(ok), "{ok}");
        }
        for bad in ["", "2", "nn", "n+1+", "()", "a b"] {
            assert!(!amount_variable(bad), "{bad}");
        }
    }

    #[test]
    fn formulas() {
        assert!(formula("x"));
        assert!(formula("1-2x"));
        assert!(formula("a1b"));
        assert!(!formula("ab"));
        assert!(!formula("12"));
        assert!(!formula("(aq)"));
        assert!(formula("(a1)"));
    }

    #[test]
    fn greek_letters() {
        assert!(one_greek_letter("\\alpha"));
        assert!(one_greek_letter("$\\alpha$"));
        assert!(one_greek_letter("\\alpha {}"));
        assert!(!one_greek_letter("\\alpha$ {}"));
        assert!(one_greek_letter("α"));
        assert!(!one_greek_letter("\\Gamma"));
        assert!(!one_greek_letter("\\alphax"));
    }

    #[test]
    fn numbers() {
        let g = e_number("1.2e3 kJ").0.unwrap();
        assert_eq!(
            g.m.groups(),
            [None, Some("1.2"), None, Some("e"), None, Some("3")]
        );
        assert_eq!(g.rest, " kJ");
        let g = e_number("1,2 x 10^{-3}").0.unwrap();
        assert_eq!(
            g.m.groups(),
            [None, Some("1,2"), None, None, Some("x"), Some("{-3}")]
        );
        assert!(e_number("kg").0.is_none());
        let g = pow_number("^2").unwrap();
        assert_eq!(g.m.groups()[..3], [None, Some(""), Some("2")]);
        assert!(pow_number("1.^2").is_none());
    }
}
