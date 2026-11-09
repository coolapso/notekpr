// Part of notekpr-mhchem, a Rust port of mhchemParser 4.2.2 (Copyright
// 2015-2023 Martin Hensel, Apache License 2.0: see lib.rs and LICENSE.txt).
// Modified: translated to Rust.

//! mhchemTexify: what the parser outputs, and how it becomes TeX.

use std::borrow::Cow;

use crate::Error;
use crate::parser::{DType, Parser};

/// `hyphen`
pub(crate) const HYPHEN: &str = "\\text{-}";
/// `ellipsis`
pub(crate) const ELLIPSIS: &str = "\\ldots ";
/// `addition compound`
pub(crate) const ADDITION_COMPOUND: &str = "\\,{\\cdot}\\,";
/// `tinySkip`
pub(crate) const TINY_SKIP: &str = "\\mkern2mu";
/// `entitySkip`
pub(crate) const ENTITY_SKIP: &str = "~";
/// `electron dot`
pub(crate) const ELECTRON_DOT: &str = "\\mkern1mu \\bullet\\mkern1mu ";
/// `KV x`
pub(crate) const KV_X: &str = "{\\times}";
/// `prime`
pub(crate) const PRIME: &str = "\\prime ";
/// `commaDecimal`
pub(crate) const COMMA_DECIMAL: &str = "{,}";
/// `circa`
pub(crate) const CIRCA: &str = "{\\sim}";
/// `pu-space-1`
pub(crate) const PU_SPACE_1: &str = "~";
/// `pu-space-2`
pub(crate) const PU_SPACE_2: &str = "\\mkern3mu ";
/// `1000 separator`
pub(crate) const THOUSANDS: &str = "\\mkern2mu ";
/// `cdot`
pub(crate) const CDOT: &str = "\\cdot ";
/// `tight cdot`
pub(crate) const TIGHT_CDOT: &str = "\\mkern1mu{\\cdot}\\mkern1mu ";
/// `times`
pub(crate) const TIMES: &str = "\\times ";
/// ` / `
pub(crate) const SPACED_SLASH: &str = "\\,/\\,";

/// What the parser outputs: mhchemParser's `Parsed`. The types that are
/// always the same TeX (and strings) are [`P::Tex`].
#[derive(Debug)]
pub(crate) enum P {
    Tex(Cow<'static, str>),
    /// `chemfive`: a formula with its amount, scripts and charge.
    Chemfive(Box<Chemfive>),
    /// `rm`, `roman numeral`
    Rm(String),
    Text(String),
    StateOfAggregation(Vec<P>),
    StateOfAggregationSubscript(Vec<P>),
    Bond(String),
    /// `frac`: a small fraction of an amount.
    Frac(String, String),
    PuFrac(Vec<P>, Vec<P>),
    TexMath(String),
    /// `frac-ce`, `overset`, `underset`, `underbrace`
    Two(Two, Vec<P>, Vec<P>),
    Color(String, Vec<P>),
    Color0(String),
    Arrow(Box<Arrow>),
    Operator(String),
    /// `1st-level escape`: `&`, `\\`, `\hline`.
    Escape(String),
    /// `comma enumeration L`
    CommaL(String),
    /// `comma enumeration M`
    CommaM(String),
    /// `comma enumeration S`
    CommaS(String),
}

/// The parts of a formula (see [`crate::parser::Buffer`]).
#[derive(Debug)]
pub(crate) struct Chemfive {
    pub(crate) a: Vec<P>,
    pub(crate) b: Vec<P>,
    pub(crate) p: Vec<P>,
    pub(crate) o: Vec<P>,
    pub(crate) q: Vec<P>,
    pub(crate) d: Vec<P>,
    pub(crate) d_type: Option<DType>,
}

/// A reaction arrow with the texts above (`rd`) and below (`rq`) it.
#[derive(Debug)]
pub(crate) struct Arrow {
    pub(crate) r: String,
    pub(crate) rd: Vec<P>,
    pub(crate) rq: Vec<P>,
}

/// The commands of two formulas.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Two {
    Frac,
    Overset,
    Underset,
    Underbrace,
}

impl Parser {
    /// mhchemTexify's `go`: the TeX of `items`, in braces if `outer_braces`
    /// (unless empty or escaping to the surrounding TeX).
    pub(crate) fn texify(&mut self, items: &[P], outer_braces: bool) -> Result<String, Error> {
        let mut res = String::new();
        let mut escapes = false;
        for item in items {
            match item {
                P::Tex(s) => res.push_str(s),
                _ => {
                    let tex = self.texify_item(item)?;
                    res.push_str(&tex);
                    escapes |= matches!(item, P::Escape(_));
                }
            }
        }
        if outer_braces && !escapes && !res.is_empty() {
            res = ["{", &res, "}"].concat();
        }
        self.work(res.len())?;
        Ok(res)
    }

    fn inner(&mut self, items: &[P]) -> Result<String, Error> {
        self.texify(items, false)
    }

    /// mhchemTexify's `_go2`: the TeX of one item.
    fn texify_item(&mut self, item: &P) -> Result<String, Error> {
        let res = match item {
            P::Tex(s) => s.to_string(),
            P::Chemfive(five) => self.chemfive(five)?,
            P::Rm(s) => ["\\mathrm{", s, "}"].concat(),
            P::Text(s) => {
                if s.contains(['^', '_']) {
                    // Simple math like e_2 in text: upright, kept as math.
                    let s = s.replace(' ', "~").replace('-', "\\text{-}");
                    ["\\mathrm{", &s, "}"].concat()
                } else {
                    ["\\text{", s, "}"].concat()
                }
            }
            P::StateOfAggregation(p) => ["\\mskip2mu ", &self.inner(p)?].concat(),
            P::StateOfAggregationSubscript(p) => ["\\mskip1mu ", &self.inner(p)?].concat(),
            P::Bond(kind) => bond(kind)
                .ok_or_else(|| {
                    Error::new(
                        crate::ErrorKind::UnknownBond,
                        format!("mhchem Error. Unknown bond type ({kind})"),
                    )
                })?
                .to_owned(),
            P::Frac(top, bottom) => small_frac(top, bottom),
            P::PuFrac(top, bottom) => {
                let top = self.inner(top)?;
                let bottom = self.inner(bottom)?;
                small_frac(&top, &bottom)
            }
            P::TexMath(s) => [s, " "].concat(),
            P::Two(command, x, y) => {
                let x = self.inner(x)?;
                let y = self.inner(y)?;
                let (open, middle) = match command {
                    Two::Frac => ("\\frac{", "}{"),
                    Two::Overset => ("\\overset{", "}{"),
                    Two::Underset => ("\\underset{", "}{"),
                    Two::Underbrace => ("\\underbrace{", "}_{"),
                };
                [open, &x, middle, &y, "}"].concat()
            }
            P::Color(color, p) => ["{\\color{", color, "}{", &self.inner(p)?, "}}"].concat(),
            P::Color0(color) => ["\\color{", color, "}"].concat(),
            P::Arrow(arrow) => self.arrow(arrow)?,
            P::Operator(kind) => operator(kind).ok_or_else(Error::internal)?.to_owned(),
            P::Escape(s) => [s, " "].concat(),
            P::CommaL(s) => ["{", s, "}\\mkern6mu "].concat(),
            P::CommaM(s) => ["{", s, "}\\mkern3mu "].concat(),
            P::CommaS(s) => ["{", s, "}\\mkern1mu "].concat(),
        };
        self.work(res.len())?;
        Ok(res)
    }

    fn chemfive(&mut self, five: &Chemfive) -> Result<String, Error> {
        let a = self.inner(&five.a)?;
        let b = self.inner(&five.b)?;
        let p = self.inner(&five.p)?;
        let o = self.inner(&five.o)?;
        let q = self.inner(&five.q)?;
        let d = self.inner(&five.d)?;
        let signed = |s: String| {
            if s.starts_with(['+', '-']) {
                ["{", &s, "}"].concat()
            } else {
                s
            }
        };
        let mut res = String::new();
        if !a.is_empty() {
            res.push_str(&signed(a));
            res.push_str("\\,");
        }
        if !b.is_empty() || !p.is_empty() {
            for part in [
                "{\\vphantom{A}}^{\\hphantom{",
                &b,
                "}}_{\\hphantom{",
                &p,
                "}}\\mkern-1.5mu{\\vphantom{A}}^{\\smash[t]{\\vphantom{2}}\\llap{",
                &b,
                "}}_{\\vphantom{2}\\llap{\\smash[t]{",
                &p,
                "}}}",
            ] {
                res.push_str(part);
            }
        }
        if !o.is_empty() {
            res.push_str(&signed(o));
        }
        let charge = |res: &mut String, d: &str| {
            res.push_str("{\\vphantom{A}}^{");
            res.push_str(d);
            res.push('}');
        };
        let index = |res: &mut String, q: &str| {
            res.push_str("{\\vphantom{A}}_{\\smash[t]{");
            res.push_str(q);
            res.push_str("}}");
        };
        match five.d_type {
            Some(DType::Kv) => {
                if !d.is_empty() || !q.is_empty() {
                    res.push_str("{\\vphantom{A}}");
                }
                if !d.is_empty() {
                    res.push_str("^{");
                    res.push_str(&d);
                    res.push('}');
                }
                if !q.is_empty() {
                    res.push_str("_{\\smash[t]{");
                    res.push_str(&q);
                    res.push_str("}}");
                }
            }
            Some(DType::Oxidation) => {
                if !d.is_empty() {
                    charge(&mut res, &d);
                }
                if !q.is_empty() {
                    index(&mut res, &q);
                }
            }
            None => {
                if !q.is_empty() {
                    index(&mut res, &q);
                }
                if !d.is_empty() {
                    charge(&mut res, &d);
                }
            }
        }
        Ok(res)
    }

    fn arrow(&mut self, arrow: &Arrow) -> Result<String, Error> {
        let rd = self.inner(&arrow.rd)?;
        let rq = self.inner(&arrow.rq)?;
        let name = arrow_name(&arrow.r).ok_or_else(Error::internal)?;
        if rd.is_empty() && rq.is_empty() {
            return Ok([" {}\\mathrel{\\long", name, "}{} "].concat());
        }
        if matches!(arrow.r.as_str(), "<=>" | "<=>>" | "<<=>" | "<-->") {
            // Arrows that don't stretch in MathJax: long ones with text set over and under.
            let mut tex = ["\\long", name].concat();
            if !rd.is_empty() {
                tex = ["\\overset{", &rd, "}{", &tex, "}"].concat();
            }
            if !rq.is_empty() {
                let lower = if arrow.r == "<-->" { "2mu" } else { "6mu" };
                tex = ["\\underset{\\lower", lower, "{", &rq, "}}{", &tex, "}"].concat();
            }
            Ok([" {}\\mathrel{", &tex, "}{} "].concat())
        } else {
            let mut tex = ["\\x", name].concat();
            if !rq.is_empty() {
                tex = [&tex, "[{", &rq, "}]"].concat();
            }
            Ok([" {}\\mathrel{", &tex, "{", &rd, "}}{} "].concat())
        }
    }
}

/// `\mathchoice` of a fraction that stays small in display style.
fn small_frac(top: &str, bottom: &str) -> String {
    let c = ["\\frac{", top, "}{", bottom, "}"].concat();
    [
        "\\mathchoice{\\textstyle",
        &c,
        "}{",
        &c,
        "}{",
        &c,
        "}{",
        &c,
        "}",
    ]
    .concat()
}

fn arrow_name(r: &str) -> Option<&'static str> {
    Some(match r {
        "->" | "\u{2192}" | "\u{27f6}" => "rightarrow",
        "<-" => "leftarrow",
        "<->" => "leftrightarrow",
        "<-->" => "leftrightarrows",
        "<=>" | "\u{21cc}" => "rightleftharpoons",
        "<=>>" => "Rightleftharpoons",
        "<<=>" => "Leftrightharpoons",
        _ => return None,
    })
}

fn bond(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "-" | "1" => "{-}",
        "=" | "2" => "{=}",
        "#" | "3" => "{\\equiv}",
        "~" => "{\\tripledash}",
        "~-" => "{\\rlap{\\lower.1em{-}}\\raise.1em{\\tripledash}}",
        "~=" | "~--" => "{\\rlap{\\lower.2em{-}}\\rlap{\\raise.2em{\\tripledash}}-}",
        "-~-" => "{\\rlap{\\lower.2em{-}}\\rlap{\\raise.2em{-}}\\tripledash}",
        "..." => "{{\\cdot}{\\cdot}{\\cdot}}",
        "...." => "{{\\cdot}{\\cdot}{\\cdot}{\\cdot}}",
        "->" => "{\\rightarrow}",
        "<-" => "{\\leftarrow}",
        "<" => "{<}",
        ">" => "{>}",
        _ => return None,
    })
}

fn operator(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "+" => " {}+{} ",
        "-" => " {}-{} ",
        "=" => " {}={} ",
        "<" => " {}<{} ",
        ">" => " {}>{} ",
        "<<" => " {}\\ll{} ",
        ">>" => " {}\\gg{} ",
        "\\pm" => " {}\\pm{} ",
        "\\approx" | "$\\approx$" => " {}\\approx{} ",
        "v" | "(v)" => " \\downarrow{} ",
        "^" | "(^)" => " \\uparrow{} ",
        _ => return None,
    })
}
