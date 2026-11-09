// Part of notekpr-mhchem, a Rust port of mhchemParser 4.2.2 (Copyright
// 2015-2023 Martin Hensel, Apache License 2.0: see lib.rs and LICENSE.txt).
// Modified: translated to Rust.

//! The parser: runs a state machine over its input, collecting what the
//! actions output ([`P`]), which [`crate::texify`] turns into TeX.

use std::borrow::Cow;
use std::mem::take;

use crate::machines::{Machine, St};
use crate::patterns::{self, Mv, Pat, Pt};
use crate::texify::{self, P};
use crate::{Error, MAX_DEPTH, MAX_WORK};

/// The actions of the transition tables. Most are mhchemParser's generic
/// ones; the others belong to one machine (named after it, where several
/// machines have their own version of one).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Act {
    /// `a=`
    AEq,
    /// `b=`
    BEq,
    /// `p=`
    PEq,
    /// `o=`
    OEq,
    /// `o=+p1`
    OEqStr(&'static str),
    /// `q=`
    QEq,
    /// `d=`
    DEq,
    /// `rm=`
    RmEq,
    /// `text=`
    TextEq,
    /// `insert`: a fixed piece of TeX.
    Insert(&'static str),
    /// `insert+p1` with `1st-level escape`
    Escape,
    /// `insert+p1` with `comma enumeration S`
    CommaS,
    /// `copy`
    Copy,
    /// `write`
    Write(&'static str),
    /// `rm`
    Rm,
    /// `text`
    Text,
    /// `tex-math`
    TexMath,
    /// `tex-math tight`
    TexMathTight,
    /// `bond`, with the kind or the match
    Bond(Option<&'static str>),
    /// `color0-output`
    Color0Output,
    /// `color-output`, its text parsed by the given machine
    ColorOutput(Machine),
    /// `ce`
    Ce,
    /// `pu`
    Pu,
    /// `1/2`
    Half,
    /// `9,9`
    NineNine,
    /// ce: `o after d`
    OAfterD,
    /// ce: `d= kv`
    DEqKv,
    /// ce: `charge or bond`
    ChargeOrBond,
    /// ce: `- after o/d`, whether after d
    MinusAfterOD(bool),
    /// ce: `a to o`
    AToO,
    /// ce: `sb=true`, `sb=false`
    Sb(bool),
    /// ce: `beginsWithBond=true`, `beginsWithBond=false`
    BeginsWithBond(bool),
    /// ce: `parenthesisLevel++`
    ParenInc,
    /// ce: `parenthesisLevel--`
    ParenDec,
    /// ce: `state of aggregation`
    CeStateOfAggregation,
    /// ce: `comma`
    CeComma,
    /// ce: `output`, with what follows: 0 (nothing), 1 or 2
    CeOutput(u8),
    /// ce: `oxidation-output`
    OxidationOutput,
    /// ce: `frac-output`
    FracOutput,
    /// ce: `overset-output`
    OversetOutput,
    /// ce: `underset-output`
    UndersetOutput,
    /// ce: `underbrace-output`
    UnderbraceOutput,
    /// ce: `r=`
    REq,
    /// ce: `rdt=`
    RdtEq,
    /// ce: `rd=`
    RdEq,
    /// ce: `rqt=`
    RqtEq,
    /// ce: `rq=`
    RqEq,
    /// ce and pu: `operator`, with the kind or the match
    Operator(Option<&'static str>),
    /// text: `output`
    TextOutput,
    /// pq: `state of aggregation`
    PqStateOfAggregation,
    /// oxidation: `roman-numeral`
    RomanNumeral,
    /// tex-math (tight): `output`
    TexMathOutput,
    /// tex-math tight: `tight operator`
    TightOperator,
    /// pu: `enumber`
    ENumber,
    /// pu: `number^`
    NumberPow,
    /// pu: `output`
    PuOutput,
    /// pu-2: `^(-1)`
    PowEq,
    /// pu-2: `output`
    Pu2Output,
    /// pu-9,9: `output-0`
    Output0,
    /// pu-9,9: `output-o`
    OutputO,
}

/// How a superscript after a formula is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DType {
    /// Kröger–Vink notation (or a charge).
    Kv,
    /// An oxidation state.
    Oxidation,
}

/// A machine's buffer. The parts of a formula: amount `a`, superscript
/// `b` and subscript `p` before the object `o`, subscript `q` and
/// superscript `d` after it; `r…` an arrow and its texts. An empty string
/// is mhchemParser's `undefined`.
#[derive(Default)]
pub(crate) struct Buffer {
    a: String,
    b: String,
    p: String,
    o: String,
    q: String,
    d: String,
    d_type: Option<DType>,
    rm: String,
    text: String,
    r: String,
    rd: String,
    rq: String,
    rdt: String,
    rqt: String,
    /// A space was read before what comes next.
    sb: bool,
    paren_level: i64,
    begins_with_bond: bool,
}

impl Buffer {
    /// Empties the buffer but for the parenthesis level and whether the
    /// formula began with a bond.
    fn clear(&mut self) {
        let paren_level = self.paren_level;
        let begins_with_bond = self.begins_with_bond;
        *self = Buffer {
            paren_level,
            begins_with_bond,
            ..Buffer::default()
        };
    }
}

/// The parser's state across machines: how deep and how much work so far.
pub(crate) struct Parser {
    pub(crate) steps: u64,
    depth: u32,
}

/// mhchemParser's first step on every input: line breaks to spaces, the
/// Unicode dashes to `-`, `…` to `...`.
fn normalize(s: &str) -> Cow<'_, str> {
    let special = |c: char| {
        matches!(
            c,
            '\n' | '\u{2212}' | '\u{2013}' | '\u{2014}' | '\u{2010}' | '\u{2026}'
        )
    };
    if !s.contains(special) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push(' '),
            '\u{2212}' | '\u{2013}' | '\u{2014}' | '\u{2010}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

impl Parser {
    pub(crate) fn new() -> Parser {
        Parser { steps: 0, depth: 0 }
    }

    /// Counts `n` steps of work; an error past [`MAX_WORK`].
    pub(crate) fn work(&mut self, n: usize) -> Result<(), Error> {
        self.steps = self.steps.saturating_add(n as u64);
        if self.steps > MAX_WORK {
            Err(Error::too_complex())
        } else {
            Ok(())
        }
    }

    /// mhchemParser's `go`: parses `input` with a machine.
    pub(crate) fn go(&mut self, input: &str, machine: Machine) -> Result<Vec<P>, Error> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        if self.depth >= MAX_DEPTH {
            return Err(Error::too_deep());
        }
        self.depth += 1;
        let result = self.run(input, machine);
        self.depth -= 1;
        result
    }

    fn run(&mut self, input: &str, machine: Machine) -> Result<Vec<P>, Error> {
        let text = normalize(input);
        self.work(text.len())?;
        let mut input: &str = &text;
        let mut state = St::S0;
        let mut buffer = Buffer::default();
        let mut output = Vec::new();
        // Input that matches no rule (or only ones that revisit) for ten
        // rounds is an error, as in mhchemParser.
        let mut last_len = usize::MAX;
        let mut watchdog = 10;
        loop {
            if input.len() != last_len {
                watchdog = 10;
                last_len = input.len();
            } else {
                watchdog -= 1;
            }
            let current = state;
            'rules: for rule in machine.rules() {
                if !rule.applies(current) {
                    continue;
                }
                for &pat in rule.pats {
                    let Some(found) = self.pattern(pat, input)? else {
                        continue;
                    };
                    for &act in rule.acts {
                        let out = self.act(act, &mut buffer, found.m)?;
                        output.extend(out);
                    }
                    if let Some(next) = rule.next {
                        state = next;
                    }
                    if input.is_empty() {
                        return Ok(output);
                    }
                    if !rule.revisit {
                        input = found.rest;
                    }
                    if !rule.cont {
                        break 'rules;
                    }
                }
            }
            if watchdog <= 0 {
                return Err(Error::unexpected());
            }
        }
    }

    /// Runs an action; returns what it outputs.
    fn act(&mut self, act: Act, b: &mut Buffer, m: Mv<'_>) -> Result<Vec<P>, Error> {
        let s = m.s();
        let none = Vec::new();
        Ok(match act {
            Act::AEq => {
                b.a.push_str(s);
                none
            }
            Act::BEq => {
                b.b.push_str(s);
                none
            }
            Act::PEq => {
                b.p.push_str(s);
                none
            }
            Act::OEq => {
                b.o.push_str(s);
                none
            }
            Act::OEqStr(x) => {
                b.o.push_str(x);
                none
            }
            Act::QEq => {
                b.q.push_str(s);
                none
            }
            Act::DEq => {
                b.d.push_str(s);
                none
            }
            Act::RmEq => {
                b.rm.push_str(s);
                none
            }
            Act::TextEq => {
                b.text.push_str(s);
                none
            }
            Act::Insert(tex) => vec![P::Tex(Cow::Borrowed(tex))],
            Act::Escape => vec![P::Escape(s.to_owned())],
            Act::CommaS => vec![P::CommaS(s.to_owned())],
            Act::Copy if s.is_empty() => none,
            Act::Copy => vec![P::Tex(Cow::Owned(s.to_owned()))],
            Act::Write(x) => vec![P::Tex(Cow::Borrowed(x))],
            Act::Rm => vec![P::Rm(s.to_owned())],
            Act::Text => self.go(s, Machine::Text)?,
            Act::TexMath => self.go(s, Machine::TexMath)?,
            Act::TexMathTight => self.go(s, Machine::TexMathTight)?,
            Act::Bond(kind) => vec![P::Bond(kind.unwrap_or(s).to_owned())],
            Act::Color0Output => vec![P::Color0(s.to_owned())],
            Act::ColorOutput(machine) => {
                let (color, text) = m.two();
                vec![P::Color(color.to_owned(), self.go(text, machine)?)]
            }
            Act::Ce => self.go(s, Machine::Ce)?,
            Act::Pu => self.go(s, Machine::Pu)?,
            Act::Half => half(s),
            Act::NineNine => self.go(s, Machine::NineNine)?,
            Act::OAfterD => {
                let mut ret;
                if is_positive_integer(&b.d) {
                    let d = take(&mut b.d);
                    ret = self.ce_output(b, 0)?;
                    ret.push(P::Tex(Cow::Borrowed(texify::TINY_SKIP)));
                    b.b = d;
                } else {
                    ret = self.ce_output(b, 0)?;
                }
                b.o.push_str(s);
                ret
            }
            Act::DEqKv => {
                b.d = s.to_owned();
                b.d_type = Some(DType::Kv);
                none
            }
            Act::ChargeOrBond => {
                if b.begins_with_bond {
                    let mut ret = self.ce_output(b, 0)?;
                    ret.push(P::Bond("-".to_owned()));
                    ret
                } else {
                    b.d = s.to_owned();
                    none
                }
            }
            Act::MinusAfterOD(after_d) => self.minus_after_o_or_d(b, s, after_d)?,
            Act::AToO => {
                b.o = take(&mut b.a);
                none
            }
            Act::Sb(sb) => {
                b.sb = sb;
                none
            }
            Act::BeginsWithBond(v) => {
                b.begins_with_bond = v;
                none
            }
            Act::ParenInc => {
                b.paren_level += 1;
                none
            }
            Act::ParenDec => {
                b.paren_level -= 1;
                none
            }
            Act::CeStateOfAggregation => vec![P::StateOfAggregation(self.go(s, Machine::O)?)],
            Act::CeComma => {
                let a = s.trim_end_matches(patterns::is_ws);
                if a.len() != s.len() && b.paren_level == 0 {
                    vec![P::CommaL(a.to_owned())]
                } else {
                    vec![P::CommaM(a.to_owned())]
                }
            }
            Act::CeOutput(entity_follows) => self.ce_output(b, entity_follows)?,
            Act::OxidationOutput => {
                let mut ret = vec![P::Tex(Cow::Borrowed("{"))];
                ret.extend(self.go(s, Machine::Oxidation)?);
                ret.push(P::Tex(Cow::Borrowed("}")));
                ret
            }
            Act::FracOutput | Act::OversetOutput | Act::UndersetOutput | Act::UnderbraceOutput => {
                let (x, y) = m.two();
                let x = self.go(x, Machine::Ce)?;
                let y = self.go(y, Machine::Ce)?;
                let command = match act {
                    Act::FracOutput => texify::Two::Frac,
                    Act::OversetOutput => texify::Two::Overset,
                    Act::UndersetOutput => texify::Two::Underset,
                    _ => texify::Two::Underbrace,
                };
                vec![P::Two(command, x, y)]
            }
            Act::REq => {
                b.r = s.to_owned();
                none
            }
            Act::RdtEq => {
                b.rdt = s.to_owned();
                none
            }
            Act::RdEq => {
                b.rd = s.to_owned();
                none
            }
            Act::RqtEq => {
                b.rqt = s.to_owned();
                none
            }
            Act::RqEq => {
                b.rq = s.to_owned();
                none
            }
            Act::Operator(kind) => vec![P::Operator(kind.unwrap_or(s).to_owned())],
            Act::TextOutput => {
                if b.text.is_empty() {
                    none
                } else {
                    let text = take(&mut b.text);
                    b.clear();
                    vec![P::Text(text)]
                }
            }
            Act::PqStateOfAggregation => {
                vec![P::StateOfAggregationSubscript(self.go(s, Machine::O)?)]
            }
            Act::RomanNumeral => vec![P::Rm(b.o.clone())],
            Act::TexMathOutput => {
                if b.o.is_empty() {
                    none
                } else {
                    let o = take(&mut b.o);
                    b.clear();
                    vec![P::TexMath(o)]
                }
            }
            Act::TightOperator => {
                b.o.push('{');
                b.o.push_str(s);
                b.o.push('}');
                none
            }
            Act::ENumber => self.e_number(m.groups())?,
            Act::NumberPow => {
                let g = m.groups();
                let mut ret = number_sign(g[0]);
                ret.extend(self.go(g[1].unwrap_or(""), Machine::Pu99)?);
                ret.push(P::Tex(Cow::Owned(["^{", g[2].unwrap_or(""), "}"].concat())));
                ret
            }
            Act::PuOutput => self.pu_output(b)?,
            Act::PowEq => {
                b.rm.push_str("^{");
                b.rm.push_str(s);
                b.rm.push('}');
                none
            }
            Act::Pu2Output => self.pu2_output(b)?,
            Act::Output0 => thousands(take(&mut b.text), true),
            Act::OutputO => thousands(take(&mut b.text), false),
        })
    }

    /// ce's `output`: the formula (or the arrow) in the buffer, which is
    /// emptied. `entity_follows`: 0 if nothing need follow (an empty
    /// buffer outputs nothing), 1 if an entity follows, 2 if one that can
    /// have an amount (so the amount stays one).
    fn ce_output(&mut self, b: &mut Buffer, entity_follows: u8) -> Result<Vec<P>, Error> {
        let mut ret = Vec::new();
        if b.r.is_empty() {
            let all_empty = [&b.a, &b.b, &b.p, &b.o, &b.q, &b.d]
                .iter()
                .all(|s| s.is_empty());
            if !all_empty || entity_follows != 0 {
                if b.sb {
                    ret.push(P::Tex(Cow::Borrowed(texify::ENTITY_SKIP)));
                }
                if b.o.is_empty()
                    && b.q.is_empty()
                    && b.d.is_empty()
                    && b.b.is_empty()
                    && b.p.is_empty()
                    && entity_follows != 2
                {
                    b.o = take(&mut b.a);
                } else if b.o.is_empty()
                    && b.q.is_empty()
                    && b.d.is_empty()
                    && (!b.b.is_empty() || !b.p.is_empty())
                {
                    b.o = take(&mut b.a);
                    b.d = take(&mut b.b);
                    b.q = take(&mut b.p);
                } else if !b.o.is_empty()
                    && b.d_type == Some(DType::Kv)
                    && patterns::oxidation(&b.d, true)
                {
                    b.d_type = Some(DType::Oxidation);
                } else if !b.o.is_empty() && b.d_type == Some(DType::Kv) && b.q.is_empty() {
                    b.d_type = None;
                }
                let d_machine = if b.d_type == Some(DType::Oxidation) {
                    Machine::Oxidation
                } else {
                    Machine::Bd
                };
                let five = texify::Chemfive {
                    a: self.go(&b.a, Machine::A)?,
                    b: self.go(&b.b, Machine::Bd)?,
                    p: self.go(&b.p, Machine::Pq)?,
                    o: self.go(&b.o, Machine::O)?,
                    q: self.go(&b.q, Machine::Pq)?,
                    d: self.go(&b.d, d_machine)?,
                    d_type: b.d_type,
                };
                ret.push(P::Chemfive(Box::new(five)));
            }
        } else {
            let rd = self.arrow_text(&b.rd, &b.rdt)?;
            let rq = self.arrow_text(&b.rq, &b.rqt)?;
            ret.push(P::Arrow(Box::new(texify::Arrow {
                r: take(&mut b.r),
                rd,
                rq,
            })));
        }
        b.clear();
        Ok(ret)
    }

    /// The text above or below an arrow: a formula, or with `M[…]` math,
    /// with `T[…]` text.
    fn arrow_text(&mut self, text: &str, kind: &str) -> Result<Vec<P>, Error> {
        match kind {
            "M" => self.go(text, Machine::TexMath),
            "T" => Ok(vec![P::Text(text.to_owned())]),
            _ => self.go(text, Machine::Ce),
        }
    }

    /// ce's `- after o/d`: a hyphen (`n-Pr`), a charge or a bond.
    fn minus_after_o_or_d(
        &mut self,
        b: &mut Buffer,
        m: &str,
        after_d: bool,
    ) -> Result<Vec<P>, Error> {
        self.work(b.o.len())?;
        let orbital = self
            .pattern(Pat::Orbital, &b.o)?
            .map(|found| found.rest.is_empty());
        let greek = patterns::one_greek_letter(&b.o);
        let latin = patterns::one_latin_letter(&b.o);
        let dollar_latin = patterns::dollar_latin_letter(&b.o);
        let hyphen_follows = m == "-" && (orbital == Some(true) || greek || latin || dollar_latin);
        let rest_empty = [&b.a, &b.b, &b.p, &b.d, &b.q].iter().all(|s| s.is_empty());
        if hyphen_follows && rest_empty && orbital.is_none() && latin {
            b.o = ["$", &b.o, "$"].concat();
        }
        let mut ret;
        if hyphen_follows {
            ret = self.ce_output(b, 0)?;
            ret.push(P::Tex(Cow::Borrowed(texify::HYPHEN)));
        } else if after_d && !b.d.is_empty() && b.d.bytes().all(|c| c.is_ascii_digit()) {
            b.d.push_str(m);
            ret = self.ce_output(b, 0)?;
        } else {
            ret = self.ce_output(b, 0)?;
            ret.push(P::Bond("-".to_owned()));
        }
        Ok(ret)
    }

    /// pu's `enumber`: a number with sign, uncertainty and exponent.
    fn e_number(&mut self, g: [Option<&str>; 6]) -> Result<Vec<P>, Error> {
        let present = |i: usize| g[i].filter(|s| !s.is_empty());
        let mut ret = number_sign(g[0]);
        if let Some(number) = present(1) {
            ret.extend(self.go(number, Machine::Pu99)?);
            if let Some(uncertainty) = present(2) {
                if uncertainty.contains([',', '.']) {
                    ret.extend(self.go(uncertainty, Machine::Pu99)?);
                } else {
                    ret.push(P::Tex(Cow::Owned(uncertainty.to_owned())));
                }
            }
            if present(3).is_some() || present(4).is_some() {
                let dot = g[3] == Some("e") || g[4] == Some("*");
                ret.push(P::Tex(Cow::Borrowed(if dot {
                    texify::CDOT
                } else {
                    texify::TIMES
                })));
            }
        }
        if let Some(exponent) = present(5) {
            ret.push(P::Tex(Cow::Owned(["10^{", exponent, "}"].concat())));
        }
        Ok(ret)
    }

    /// pu's `output`: the units in the buffer, as a fraction if there is
    /// a `/` or `//`.
    fn pu_output(&mut self, b: &mut Buffer) -> Result<Vec<P>, Error> {
        if let Some(inner) = self.whole_braces(&b.d)? {
            b.d = inner;
        }
        if let Some(inner) = self.whole_braces(&b.q)? {
            b.q = inner;
        }
        if !b.d.is_empty() {
            b.d = degrees(&b.d);
        }
        let ret = if !b.q.is_empty() {
            b.q = degrees(&b.q);
            let d = self.go(&b.d, Machine::Pu)?;
            let q = self.go(&b.q, Machine::Pu)?;
            if b.o == "//" {
                vec![P::PuFrac(d, q)]
            } else {
                let slash = if d.len() > 1 || q.len() > 1 {
                    texify::SPACED_SLASH
                } else {
                    "/"
                };
                let mut ret = d;
                ret.push(P::Tex(Cow::Borrowed(slash)));
                ret.extend(q);
                ret
            }
        } else {
            self.go(&b.d, Machine::Pu2)?
        };
        b.clear();
        Ok(ret)
    }

    /// pu-2's `output`: a unit (`{…}` is parsed as units again).
    fn pu2_output(&mut self, b: &mut Buffer) -> Result<Vec<P>, Error> {
        let mut ret = Vec::new();
        if !b.rm.is_empty() {
            ret = match self.whole_braces(&b.rm)? {
                Some(inner) => self.go(&inner, Machine::Pu)?,
                None => vec![P::Rm(b.rm.clone())],
            };
        }
        b.clear();
        Ok(ret)
    }

    /// What is inside braces that are all of `s`.
    fn whole_braces(&mut self, s: &str) -> Result<Option<String>, Error> {
        Ok(
            match self.find_group(s, Pt::L("{"), Pt::L(""), Pt::L("}"), false)? {
                Some((inner, "")) => Some(inner.to_owned()),
                _ => None,
            },
        )
    }

    /// Replaces every `\ce{…}` and `\pu{…}` in `tex` by its translation in
    /// braces, and those that translation passes through in turn.
    pub(crate) fn expand(&mut self, tex: &str) -> Result<String, Error> {
        let mut out = String::with_capacity(tex.len());
        let mut rest = tex;
        while let Some(c) = rest.chars().next() {
            if c == '\\' {
                let mut found = None;
                for (command, machine) in [("\\ce{", Machine::Ce), ("\\pu{", Machine::Pu)] {
                    if let Some((inner, after)) =
                        self.find_group(rest, Pt::L(command), Pt::L(""), Pt::L("}"), false)?
                    {
                        found = Some((inner, after, machine));
                        break;
                    }
                }
                if let Some((inner, after, machine)) = found {
                    let parsed = self.go(inner, machine)?;
                    let translated = self.texify(&parsed, false)?;
                    let translated = if translated.contains("\\ce{") || translated.contains("\\pu{")
                    {
                        self.expand_nested(&translated)?
                    } else {
                        translated
                    };
                    out.push('{');
                    out.push_str(&translated);
                    out.push('}');
                    rest = after;
                    continue;
                }
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        Ok(out)
    }

    fn expand_nested(&mut self, tex: &str) -> Result<String, Error> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::too_deep());
        }
        self.depth += 1;
        let result = self.expand(tex);
        self.depth -= 1;
        result
    }
}

/// A number's sign as pu writes it (`+-` and `+/-` as `\pm`).
fn number_sign(sign: Option<&str>) -> Vec<P> {
    match sign {
        Some("+-" | "+/-") => vec![P::Tex(Cow::Borrowed("\\pm "))],
        Some(s) if !s.is_empty() => vec![P::Tex(Cow::Owned(s.to_owned()))],
        _ => Vec::new(),
    }
}

/// `^[1-9][0-9]*$`
fn is_positive_integer(s: &str) -> bool {
    s.as_bytes()
        .first()
        .is_some_and(|c| (b'1'..=b'9').contains(c))
        && s.bytes().all(|c| c.is_ascii_digit())
}

/// The `1/2` action: a fraction like `1/2`, `n/2` or `-1/2n` as a small
/// fraction (and what follows it as math).
fn half(s: &str) -> Vec<P> {
    let mut ret = Vec::new();
    let mut s = s;
    if s.starts_with(['+', '-']) {
        ret.push(P::Tex(Cow::Owned(s[..1].to_owned())));
        s = &s[1..];
    }
    // `^([0-9]+|\$[a-z]\$|[a-z])\/([0-9]+)(\$[a-z]\$|[a-z])?$` (already checked)
    let (top, rest) = s.split_once('/').unwrap_or((s, ""));
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (bottom, after) = rest.split_at(digits);
    ret.push(P::Frac(top.replace('$', ""), bottom.to_owned()));
    if !after.is_empty() {
        ret.push(P::TexMath(after.replace('$', "")));
    }
    ret
}

/// pu-9,9's outputs: a number's digits with thin spaces between groups of
/// three, counted from the decimal point: from the right before it
/// (`before_point`), from the left after it. Four digits stay together.
fn thousands(text: String, before_point: bool) -> Vec<P> {
    let len = text.len();
    if len <= 4 || !text.is_ascii() {
        return vec![P::Tex(Cow::Owned(text))];
    }
    let sep = || P::Tex(Cow::Borrowed(texify::THOUSANDS));
    let piece = |from: usize, to: usize| P::Tex(Cow::Owned(text[from..to.min(len)].to_owned()));
    let mut ret = Vec::new();
    if before_point {
        let head = match len % 3 {
            0 => 3,
            n => n,
        };
        ret.push(piece(0, head));
        let mut i = head;
        while i < len {
            ret.push(sep());
            ret.push(piece(i, i + 3));
            i += 3;
        }
    } else {
        let mut i = 0;
        while i < len - 3 {
            ret.push(piece(i, i + 3));
            ret.push(sep());
            i += 3;
        }
        ret.push(piece(i, len));
    }
    ret
}

/// pu writes degrees Celsius and Fahrenheit (`°C`, `^oC`, `^{o}C`) as
/// `{}^{\circ}C`.
fn degrees(s: &str) -> String {
    let mut s = s.to_owned();
    for unit in ['C', 'F'] {
        let mut out = String::with_capacity(s.len());
        let mut rest = s.as_str();
        while let Some(c) = rest.chars().next() {
            let found = ["\u{b0}", "^o", "^{o}"]
                .iter()
                .find(|p| rest.strip_prefix(**p).is_some_and(|r| r.starts_with(unit)));
            if let Some(p) = found {
                out.push_str("{}^{\\circ}");
                out.push(unit);
                rest = &rest[p.len() + 1..];
            } else {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        s = out;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(items: &[P]) -> Vec<String> {
        items
            .iter()
            .map(|p| match p {
                P::Tex(s) => s.to_string(),
                _ => "?".to_owned(),
            })
            .collect()
    }

    #[test]
    fn thousands_groups() {
        assert_eq!(tex(&thousands("1234".into(), true)), ["1234"]);
        assert_eq!(
            tex(&thousands("12345".into(), true)),
            ["12", texify::THOUSANDS, "345"]
        );
        assert_eq!(
            tex(&thousands("123456".into(), true)),
            ["123", texify::THOUSANDS, "456"]
        );
        assert_eq!(
            tex(&thousands("00001".into(), false)),
            ["000", texify::THOUSANDS, "01"]
        );
        assert_eq!(
            tex(&thousands("011111".into(), false)),
            ["011", texify::THOUSANDS, "111"]
        );
    }

    #[test]
    fn degrees_both_units() {
        assert_eq!(degrees("°C"), "{}^{\\circ}C");
        assert_eq!(degrees("^oF/^{o}C"), "{}^{\\circ}F/{}^{\\circ}C");
        assert_eq!(degrees("^oK"), "^oK");
    }
}
