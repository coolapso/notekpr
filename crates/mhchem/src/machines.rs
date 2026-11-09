// Part of notekpr-mhchem, a Rust port of mhchemParser 4.2.2 (Copyright
// 2015-2023 Martin Hensel, Apache License 2.0: see lib.rs and LICENSE.txt).
// Modified: translated to Rust.

//! mhchemParser's state machines: for each, the rules in the order of its
//! transition table. A rule applies in its states (or in all, `Any`); the
//! first of its patterns that matches runs its actions.

use crate::parser::Act as A;
use crate::patterns::Pat as P;
use crate::texify::{ADDITION_COMPOUND, ELECTRON_DOT, ELLIPSIS, HYPHEN, KV_X, PRIME, TINY_SKIP};

/// A machine's state, named after mhchemParser's (`'0'` is `S0`, `'qD'`
/// is `QBigD`, `'!f'` is `NotF`, `'/'` is `Slash`, `'*'` is `Any`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum St {
    Any,
    S0,
    S1,
    S2,
    S3,
    A,
    As,
    B,
    P,
    Bp,
    O,
    Q,
    D,
    BigD,
    Qd,
    QBigD,
    Dq,
    R,
    Rt,
    Rd,
    Rdt,
    Rdq,
    C0,
    F,
    NotF,
    Slash,
}

use St as S;

/// One entry of a transition table.
pub(crate) struct Rule {
    pub(crate) pats: &'static [P],
    pub(crate) states: &'static [S],
    pub(crate) acts: &'static [A],
    pub(crate) next: Option<S>,
    /// Match again at the same place (the input isn't shortened).
    pub(crate) revisit: bool,
    /// Go on with the next rules after this one.
    pub(crate) cont: bool,
}

impl Rule {
    pub(crate) fn applies(&self, state: S) -> bool {
        self.states.iter().any(|s| *s == state || *s == S::Any)
    }
}

const fn t(pats: &'static [P], states: &'static [S], acts: &'static [A], next: Option<S>) -> Rule {
    Rule {
        pats,
        states,
        acts,
        next,
        revisit: false,
        cont: false,
    }
}

const fn revisit(rule: Rule) -> Rule {
    Rule {
        revisit: true,
        ..rule
    }
}

/// The state machines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Machine {
    Ce,
    A,
    O,
    Text,
    Pq,
    Bd,
    Oxidation,
    TexMath,
    TexMathTight,
    NineNine,
    Pu,
    Pu2,
    Pu99,
}

impl Machine {
    pub(crate) fn rules(self) -> &'static [Rule] {
        match self {
            Machine::Ce => CE,
            Machine::A => AMOUNT,
            Machine::O => OBJECT,
            Machine::Text => TEXT,
            Machine::Pq => PQ,
            Machine::Bd => BD,
            Machine::Oxidation => OXIDATION,
            Machine::TexMath => TEX_MATH,
            Machine::TexMathTight => TEX_MATH_TIGHT,
            Machine::NineNine => NINE_NINE,
            Machine::Pu => PU,
            Machine::Pu2 => PU2,
            Machine::Pu99 => PU99,
        }
    }
}

const ANY: &[S] = &[S::Any];
const SUP: &[P] = &[P::SupA, P::SupCmd2, P::SupCmd1, P::SupCmd, P::Prime];
const SUB: &[P] = &[
    P::SubBraces,
    P::SubDollars,
    P::Sub9,
    P::SubCmd2,
    P::SubCmd1,
    P::SubCmd,
];
const CMD: &[P] = &[P::Cmd2, P::Cmd1, P::Cmd];
const SPACING_CMD: &[P] = &[P::Spacing, P::Cmd2, P::Cmd1, P::Cmd];
const MINUS: A = A::Bond(Some("-"));

/// `ce`: the main parser of `\ce`.
static CE: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::CeOutput(0)], None),
    Rule {
        cont: true,
        ..revisit(t(
            &[P::Else],
            &[S::S0, S::S1, S::S2],
            &[A::BeginsWithBond(false)],
            None,
        ))
    },
    t(&[P::OxidationEnd], &[S::S0], &[A::OxidationOutput], None),
    t(&[P::Cmt], &[S::R], &[A::RdtEq], Some(S::Rt)),
    t(&[P::Cmt], &[S::Rd], &[A::RqtEq], Some(S::Rdt)),
    t(
        &[P::ArrowUpDown],
        &[S::S0, S::S1, S::S2, S::As],
        &[A::Sb(false), A::CeOutput(0), A::Operator(None)],
        Some(S::S1),
    ),
    t(
        &[P::Upright],
        &[S::S0, S::S1, S::S2],
        &[A::OEq, A::CeOutput(0)],
        Some(S::S1),
    ),
    t(
        &[P::Orbital],
        &[S::S0, S::S1, S::S2, S::S3],
        &[A::OEq],
        Some(S::O),
    ),
    t(
        &[P::Arrow],
        &[S::S0, S::S1, S::S2, S::S3],
        &[A::REq],
        Some(S::R),
    ),
    t(
        &[P::Arrow],
        &[S::A, S::As],
        &[A::CeOutput(0), A::REq],
        Some(S::R),
    ),
    t(&[P::Arrow], ANY, &[A::CeOutput(0), A::REq], Some(S::R)),
    t(&[P::Plus], &[S::O], &[A::DEqKv], Some(S::D)),
    t(&[P::Plus], &[S::D, S::BigD], &[A::DEq], Some(S::D)),
    t(&[P::Plus], &[S::Q], &[A::DEq], Some(S::Qd)),
    t(&[P::Plus], &[S::Qd, S::QBigD], &[A::DEq], Some(S::Qd)),
    t(&[P::Plus], &[S::Dq], &[A::CeOutput(0), A::DEq], Some(S::D)),
    t(
        &[P::Plus],
        &[S::S3],
        &[A::Sb(false), A::CeOutput(0), A::Operator(None)],
        Some(S::S0),
    ),
    t(&[P::Amount], &[S::S0, S::S2], &[A::AEq], Some(S::A)),
    t(
        &[P::PmOperator],
        &[S::S0, S::S1, S::S2, S::A, S::As],
        &[A::Sb(false), A::CeOutput(0), A::Operator(Some("\\pm"))],
        Some(S::S0),
    ),
    t(
        &[P::Operator],
        &[S::S0, S::S1, S::S2, S::A, S::As],
        &[A::Sb(false), A::CeOutput(0), A::Operator(None)],
        Some(S::S0),
    ),
    t(
        &[P::MinusEnd],
        &[S::O, S::Q],
        &[A::ChargeOrBond, A::CeOutput(0)],
        Some(S::Qd),
    ),
    t(&[P::MinusEnd], &[S::D], &[A::DEq], Some(S::D)),
    t(
        &[P::MinusEnd],
        &[S::BigD],
        &[A::CeOutput(0), MINUS],
        Some(S::S3),
    ),
    t(&[P::MinusEnd], &[S::Q], &[A::DEq], Some(S::Qd)),
    t(&[P::MinusEnd], &[S::Qd], &[A::DEq], Some(S::Qd)),
    t(
        &[P::MinusEnd],
        &[S::QBigD, S::Dq],
        &[A::CeOutput(0), MINUS],
        Some(S::S3),
    ),
    t(
        &[P::MinusDigit],
        &[S::S3, S::O],
        &[A::CeOutput(0), A::Insert(HYPHEN)],
        Some(S::S3),
    ),
    t(
        &[P::MinusOrbital],
        &[S::O],
        &[A::CeOutput(0), A::Insert(HYPHEN)],
        Some(S::S2),
    ),
    t(
        &[P::MinusOrbital],
        &[S::D],
        &[A::CeOutput(0), A::Insert(HYPHEN)],
        Some(S::S2),
    ),
    t(
        &[P::Minus],
        &[S::S0, S::S1, S::S2],
        &[A::CeOutput(1), A::BeginsWithBond(true), MINUS],
        Some(S::S3),
    ),
    t(&[P::Minus], &[S::S3], &[MINUS], None),
    t(
        &[P::Minus],
        &[S::A],
        &[A::CeOutput(0), A::Insert(HYPHEN)],
        Some(S::S2),
    ),
    t(&[P::Minus], &[S::As], &[A::CeOutput(2), MINUS], Some(S::S3)),
    t(&[P::Minus], &[S::B], &[A::BEq], None),
    t(&[P::Minus], &[S::O], &[A::MinusAfterOD(false)], Some(S::S2)),
    t(&[P::Minus], &[S::Q], &[A::MinusAfterOD(false)], Some(S::S2)),
    t(
        &[P::Minus],
        &[S::D, S::Qd, S::Dq],
        &[A::MinusAfterOD(true)],
        Some(S::S2),
    ),
    t(
        &[P::Minus],
        &[S::BigD, S::QBigD, S::P],
        &[A::CeOutput(0), MINUS],
        Some(S::S3),
    ),
    t(&[P::Amount], &[S::S1, S::S3], &[A::AEq], Some(S::A)),
    t(
        &[P::Letters],
        &[
            S::S0,
            S::S1,
            S::S2,
            S::S3,
            S::A,
            S::As,
            S::B,
            S::P,
            S::Bp,
            S::O,
        ],
        &[A::OEq],
        Some(S::O),
    ),
    t(
        &[P::Letters],
        &[S::Q, S::Dq],
        &[A::CeOutput(0), A::OEq],
        Some(S::O),
    ),
    t(
        &[P::Letters],
        &[S::D, S::BigD, S::Qd, S::QBigD],
        &[A::OAfterD],
        Some(S::O),
    ),
    t(&[P::Digits], &[S::O], &[A::QEq], Some(S::Q)),
    t(&[P::Digits], &[S::D, S::BigD], &[A::QEq], Some(S::Dq)),
    t(&[P::Digits], &[S::Q], &[A::CeOutput(0), A::OEq], Some(S::O)),
    t(&[P::Digits], &[S::A], &[A::OEq], Some(S::O)),
    t(&[P::SpaceA], &[S::B, S::P, S::Bp], &[], None),
    t(&[P::Space], &[S::A], &[], Some(S::As)),
    t(&[P::Space], &[S::S0], &[A::Sb(false)], None),
    t(&[P::Space], &[S::S1, S::S2], &[A::Sb(true)], None),
    t(
        &[P::Space],
        &[S::R, S::Rt, S::Rd, S::Rdt, S::Rdq],
        &[A::CeOutput(0)],
        Some(S::S0),
    ),
    t(
        &[P::Space],
        ANY,
        &[A::CeOutput(0), A::Sb(true)],
        Some(S::S1),
    ),
    t(
        &[P::Escape],
        &[S::S1, S::S2],
        &[A::CeOutput(0), A::Escape],
        None,
    ),
    t(&[P::Escape], ANY, &[A::CeOutput(0), A::Escape], Some(S::S0)),
    t(&[P::Brackets], &[S::R, S::Rt], &[A::RdEq], Some(S::Rd)),
    t(&[P::Brackets], &[S::Rd, S::Rdt], &[A::RqEq], Some(S::Rdq)),
    t(
        &[P::Ellipsis],
        &[S::O, S::D, S::BigD, S::Dq, S::Qd, S::QBigD],
        &[A::CeOutput(0), A::Bond(Some("..."))],
        Some(S::S3),
    ),
    t(
        &[P::Ellipsis],
        ANY,
        &[A::CeOutput(1), A::Insert(ELLIPSIS)],
        Some(S::S1),
    ),
    t(
        &[P::AdditionDot],
        ANY,
        &[A::CeOutput(0), A::Insert(ADDITION_COMPOUND)],
        Some(S::S1),
    ),
    t(
        &[P::StateOfAggregation],
        ANY,
        &[A::CeOutput(0), A::CeStateOfAggregation],
        Some(S::S1),
    ),
    t(
        &[P::Open],
        &[S::A, S::As, S::O],
        &[A::OEq, A::CeOutput(0), A::ParenInc],
        Some(S::S2),
    ),
    t(
        &[P::Open],
        &[S::S0, S::S1, S::S2, S::S3],
        &[A::OEq, A::CeOutput(0), A::ParenInc],
        Some(S::S2),
    ),
    t(
        &[P::Open],
        ANY,
        &[A::CeOutput(0), A::OEq, A::CeOutput(0), A::ParenInc],
        Some(S::S2),
    ),
    t(
        &[P::Close],
        &[S::S0, S::S1, S::S2, S::S3, S::B, S::P, S::Bp, S::O],
        &[A::OEq, A::ParenDec],
        Some(S::O),
    ),
    t(
        &[P::Close],
        &[S::A, S::As, S::D, S::BigD, S::Q, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::OEq, A::ParenDec],
        Some(S::O),
    ),
    t(
        &[P::CommaSpace],
        ANY,
        &[A::CeOutput(0), A::CeComma],
        Some(S::S0),
    ),
    t(&[P::SupSubAlone], ANY, &[], None),
    t(
        &[P::SupBraces, P::SupDollars],
        &[S::S0, S::S1, S::S2, S::As],
        &[A::BEq],
        Some(S::B),
    ),
    t(
        &[P::SupBraces, P::SupDollars],
        &[S::P],
        &[A::BEq],
        Some(S::Bp),
    ),
    t(
        &[P::SupBraces, P::SupDollars],
        &[S::S3, S::O],
        &[A::DEqKv],
        Some(S::BigD),
    ),
    t(
        &[P::SupBraces, P::SupDollars],
        &[S::Q],
        &[A::DEq],
        Some(S::QBigD),
    ),
    t(
        &[P::SupBraces, P::SupDollars],
        &[S::D, S::BigD, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::DEq],
        Some(S::BigD),
    ),
    t(SUP, &[S::S0, S::S1, S::S2, S::As], &[A::BEq], Some(S::B)),
    t(SUP, &[S::P], &[A::BEq], Some(S::Bp)),
    t(SUP, &[S::S3, S::O], &[A::DEqKv], Some(S::D)),
    t(SUP, &[S::Q], &[A::DEq], Some(S::Qd)),
    t(SUP, &[S::D, S::Qd, S::BigD, S::QBigD], &[A::DEq], None),
    t(SUP, &[S::Dq], &[A::CeOutput(0), A::DEq], Some(S::D)),
    t(
        &[P::SubStateOfAggregation],
        &[S::D, S::BigD, S::Q, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::QEq],
        Some(S::Q),
    ),
    t(SUB, &[S::S0, S::S1, S::S2, S::As], &[A::PEq], Some(S::P)),
    t(SUB, &[S::B], &[A::PEq], Some(S::Bp)),
    t(SUB, &[S::S3, S::O], &[A::QEq], Some(S::Q)),
    t(SUB, &[S::D, S::BigD], &[A::QEq], Some(S::Dq)),
    t(
        SUB,
        &[S::Q, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::QEq],
        Some(S::Q),
    ),
    t(
        &[P::BondChar],
        &[
            S::S0,
            S::S1,
            S::S2,
            S::S3,
            S::A,
            S::As,
            S::O,
            S::Q,
            S::D,
            S::BigD,
            S::Qd,
            S::QBigD,
            S::Dq,
        ],
        &[A::CeOutput(2), A::Bond(None)],
        Some(S::S3),
    ),
    t(
        &[P::Hash],
        &[S::S0, S::S1, S::S2, S::S3, S::A, S::As, S::O],
        &[A::CeOutput(2), A::Bond(Some("#"))],
        Some(S::S3),
    ),
    t(
        &[P::EmptyBracesSup],
        ANY,
        &[A::CeOutput(1), A::Insert(TINY_SKIP)],
        Some(S::S1),
    ),
    t(&[P::EmptyBraces], ANY, &[A::CeOutput(1)], Some(S::S1)),
    t(
        &[P::Braces],
        &[S::S0, S::S1, S::S2, S::S3, S::A, S::As, S::B, S::P, S::Bp],
        &[A::OEq],
        Some(S::O),
    ),
    t(
        &[P::Braces],
        &[S::O, S::D, S::BigD, S::Q, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::OEq],
        Some(S::O),
    ),
    t(&[P::Dollars], &[S::A], &[A::AEq], None),
    t(
        &[P::Dollars],
        &[S::S0, S::S1, S::S2, S::S3, S::As, S::B, S::P, S::Bp, S::O],
        &[A::OEq],
        Some(S::O),
    ),
    t(&[P::Dollars], &[S::As, S::O], &[A::OEq], None),
    t(
        &[P::Dollars],
        &[S::Q, S::D, S::BigD, S::Qd, S::QBigD, S::Dq],
        &[A::CeOutput(0), A::OEq],
        Some(S::O),
    ),
    t(
        &[P::BondCmd],
        ANY,
        &[A::CeOutput(2), A::Bond(None)],
        Some(S::S3),
    ),
    t(
        &[P::Frac],
        ANY,
        &[A::CeOutput(1), A::FracOutput],
        Some(S::S3),
    ),
    t(
        &[P::Overset],
        ANY,
        &[A::CeOutput(2), A::OversetOutput],
        Some(S::S3),
    ),
    t(
        &[P::Underset],
        ANY,
        &[A::CeOutput(2), A::UndersetOutput],
        Some(S::S3),
    ),
    t(
        &[P::Underbrace],
        ANY,
        &[A::CeOutput(2), A::UnderbraceOutput],
        Some(S::S3),
    ),
    t(
        &[P::Color2],
        ANY,
        &[A::CeOutput(2), A::ColorOutput(Machine::Ce)],
        Some(S::S3),
    ),
    t(&[P::Color1], ANY, &[A::CeOutput(2), A::Color0Output], None),
    t(&[P::CeCmd], ANY, &[A::CeOutput(2), A::Ce], Some(S::S3)),
    t(&[P::Spacing], ANY, &[A::CeOutput(1), A::Copy], Some(S::S1)),
    t(
        &[P::PuCmd],
        ANY,
        &[A::CeOutput(0), A::Write("{"), A::Pu, A::Write("}")],
        Some(S::S3),
    ),
    t(
        CMD,
        &[
            S::S0,
            S::S1,
            S::S2,
            S::S3,
            S::A,
            S::As,
            S::B,
            S::P,
            S::Bp,
            S::O,
            S::C0,
        ],
        &[A::OEq, A::CeOutput(0)],
        Some(S::S3),
    ),
    t(
        CMD,
        ANY,
        &[A::CeOutput(0), A::OEq, A::CeOutput(0)],
        Some(S::S3),
    ),
    t(&[P::Others], ANY, &[A::CeOutput(1), A::Copy], Some(S::S3)),
    revisit(t(&[P::Else], &[S::A], &[A::AToO], Some(S::O))),
    revisit(t(
        &[P::Else],
        &[S::As],
        &[A::CeOutput(0), A::Sb(true)],
        Some(S::S1),
    )),
    revisit(t(
        &[P::Else],
        &[S::R, S::Rt, S::Rd, S::Rdt, S::Rdq],
        &[A::CeOutput(0)],
        Some(S::S0),
    )),
    t(&[P::Else], ANY, &[A::CeOutput(0), A::Copy], Some(S::S3)),
];

/// `a`: an amount.
static AMOUNT: &[Rule] = &[
    t(&[P::Empty], ANY, &[], None),
    t(&[P::HalfEnd], &[S::S0], &[A::Half], None),
    revisit(t(&[P::Else], &[S::S0], &[], Some(S::S1))),
    t(&[P::DollarsInner], ANY, &[A::TexMathTight], Some(S::S1)),
    t(
        &[P::Comma],
        ANY,
        &[A::Insert(crate::texify::COMMA_DECIMAL)],
        None,
    ),
    t(&[P::Else], ANY, &[A::Copy], None),
];

/// `o`: an object, the formula itself.
static OBJECT: &[Rule] = &[
    t(&[P::Empty], ANY, &[], None),
    t(&[P::HalfEnd], &[S::S0], &[A::Half], None),
    revisit(t(&[P::Else], &[S::S0], &[], Some(S::S1))),
    t(&[P::Letters], ANY, &[A::Rm], None),
    t(&[P::Ca], ANY, &[A::Insert(crate::texify::CIRCA)], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(CMD, ANY, &[A::Copy], None),
    t(&[P::DollarsInner], ANY, &[A::TexMath], None),
    t(
        &[P::BracesInner],
        ANY,
        &[A::Write("{"), A::Text, A::Write("}")],
        None,
    ),
    t(&[P::Else], ANY, &[A::Copy], None),
];

/// `text`: text in braces.
static TEXT: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::TextOutput], None),
    t(&[P::Braces], ANY, &[A::TextEq], None),
    t(&[P::DollarsInner], ANY, &[A::TexMath], None),
    t(&[P::Greek], ANY, &[A::TextOutput, A::Rm], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::TextOutput, A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(SPACING_CMD, ANY, &[A::TextOutput, A::Copy], None),
    t(&[P::Else], ANY, &[A::TextEq], None),
];

/// `pq`: a subscript (`p` before the formula, `q` after it).
static PQ: &[Rule] = &[
    t(&[P::Empty], ANY, &[], None),
    t(
        &[P::StateOfAggregation],
        ANY,
        &[A::PqStateOfAggregation],
        None,
    ),
    revisit(t(&[P::IEnd], &[S::S0], &[], Some(S::NotF))),
    t(&[P::KvLetters], &[S::S0], &[A::Rm], Some(S::S0)),
    revisit(t(&[P::FormulaEnd], &[S::S0], &[], Some(S::F))),
    t(&[P::HalfEnd], &[S::S0], &[A::Half], None),
    revisit(t(&[P::Else], &[S::S0], &[], Some(S::NotF))),
    t(&[P::DollarsInner], ANY, &[A::TexMath], None),
    t(&[P::BracesInner], ANY, &[A::Text], None),
    t(&[P::LowerLetter], &[S::F], &[A::TexMath], None),
    t(&[P::Letters], ANY, &[A::Rm], None),
    t(&[P::Number], ANY, &[A::NineNine], None),
    t(&[P::Comma], ANY, &[A::CommaS], None),
    t(&[P::Color2], ANY, &[A::ColorOutput(Machine::Pq)], None),
    t(&[P::Color1], ANY, &[A::Color0Output], None),
    t(&[P::CeCmd], ANY, &[A::Ce], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(SPACING_CMD, ANY, &[A::Copy], None),
    t(&[P::Else], ANY, &[A::Copy], None),
];

/// `bd`: a superscript (`b` before the formula, `d` after it).
static BD: &[Rule] = &[
    t(&[P::Empty], ANY, &[], None),
    revisit(t(&[P::XEnd], &[S::S0], &[], Some(S::NotF))),
    revisit(t(&[P::FormulaEnd], &[S::S0], &[], Some(S::F))),
    revisit(t(&[P::Else], &[S::S0], &[], Some(S::NotF))),
    t(&[P::NumberNoMissing0], ANY, &[A::NineNine], None),
    t(&[P::Dot], ANY, &[A::Insert(ELECTRON_DOT)], None),
    t(&[P::LowerLetter], &[S::F], &[A::TexMath], None),
    t(&[P::X], ANY, &[A::Insert(KV_X)], None),
    t(&[P::Letters], ANY, &[A::Rm], None),
    t(&[P::Prime], ANY, &[A::Insert(PRIME)], None),
    t(&[P::DollarsInner], ANY, &[A::TexMath], None),
    t(&[P::BracesInner], ANY, &[A::Text], None),
    t(&[P::Color2], ANY, &[A::ColorOutput(Machine::Bd)], None),
    t(&[P::Color1], ANY, &[A::Color0Output], None),
    t(&[P::CeCmd], ANY, &[A::Ce], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(SPACING_CMD, ANY, &[A::Copy], None),
    t(&[P::Else], ANY, &[A::Copy], None),
];

/// `oxidation`: an oxidation state in roman numerals.
static OXIDATION: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::RomanNumeral], None),
    t(&[P::PmOperator], ANY, &[A::OEqStr("\\pm")], None),
    t(&[P::Else], ANY, &[A::OEq], None),
];

/// `tex-math`: TeX passed through, but for `\ce` and `\pu`.
static TEX_MATH: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::TexMathOutput], None),
    t(&[P::CeCmd], ANY, &[A::TexMathOutput, A::Ce], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::TexMathOutput, A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(
        &[P::Braces, P::Spacing, P::Cmd2, P::Cmd1, P::Cmd],
        ANY,
        &[A::OEq],
        None,
    ),
    t(&[P::Else], ANY, &[A::OEq], None),
];

/// `tex-math tight`: the same with `+` and `-` braced, for amounts.
static TEX_MATH_TIGHT: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::TexMathOutput], None),
    t(&[P::CeCmd], ANY, &[A::TexMathOutput, A::Ce], None),
    t(
        &[P::PuCmd],
        ANY,
        &[A::TexMathOutput, A::Write("{"), A::Pu, A::Write("}")],
        None,
    ),
    t(
        &[P::Braces, P::Spacing, P::Cmd2, P::Cmd1, P::Cmd],
        ANY,
        &[A::OEq],
        None,
    ),
    t(&[P::Minus, P::Plus], ANY, &[A::TightOperator], None),
    t(&[P::Else], ANY, &[A::OEq], None),
];

/// `9,9`: a number with a decimal comma.
static NINE_NINE: &[Rule] = &[
    t(&[P::Empty], ANY, &[], None),
    t(
        &[P::Comma],
        ANY,
        &[A::Insert(crate::texify::COMMA_DECIMAL)],
        None,
    ),
    t(&[P::Else], ANY, &[A::Copy], None),
];

/// `pu`: the main parser of `\pu`.
static PU: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::PuOutput], None),
    t(
        &[P::SpaceEnd],
        ANY,
        &[A::PuOutput, A::Insert(crate::texify::PU_SPACE_1)],
        None,
    ),
    t(&[P::Open, P::Close], &[S::S0, S::A], &[A::Copy], None),
    t(&[P::PowNumber], &[S::S0], &[A::NumberPow], Some(S::A)),
    t(&[P::ENumber], &[S::S0], &[A::ENumber], Some(S::A)),
    t(&[P::Space], &[S::S0, S::A], &[], None),
    t(
        &[P::PmOperator],
        &[S::S0, S::A],
        &[A::Operator(Some("\\pm"))],
        Some(S::S0),
    ),
    t(&[P::Operator], &[S::S0, S::A], &[A::Copy], Some(S::S0)),
    t(&[P::DoubleSlash], &[S::D], &[A::OEq], Some(S::Slash)),
    t(&[P::Slash], &[S::D], &[A::OEq], Some(S::Slash)),
    t(&[P::Braces, P::Else], &[S::S0, S::D], &[A::DEq], Some(S::D)),
    t(
        &[P::Braces, P::Else],
        &[S::A],
        &[A::Insert(crate::texify::PU_SPACE_1), A::DEq],
        Some(S::D),
    ),
    t(
        &[P::Braces, P::Else],
        &[S::Slash, S::Q],
        &[A::QEq],
        Some(S::Q),
    ),
];

/// `pu-2`: units without a fraction.
static PU2: &[Rule] = &[
    t(&[P::Empty], ANY, &[A::Pu2Output], None),
    t(
        &[P::Star],
        ANY,
        &[A::Pu2Output, A::Insert(crate::texify::TIGHT_CDOT)],
        Some(S::S0),
    ),
    t(&[P::Cmd], ANY, &[A::RmEq], None),
    t(
        &[P::Space],
        ANY,
        &[A::Pu2Output, A::Insert(crate::texify::PU_SPACE_2)],
        Some(S::S0),
    ),
    t(&[P::SupBraces, P::SupInt], &[S::S1], &[A::PowEq], None),
    t(&[P::Number], &[S::S0], &[A::RmEq], Some(S::S0)),
    t(&[P::Number], &[S::S1], &[A::PowEq], Some(S::S0)),
    t(&[P::Braces, P::Else], ANY, &[A::RmEq], Some(S::S1)),
];

/// `pu-9,9`: a number with thin spaces between groups of three digits.
static PU99: &[Rule] = &[
    t(&[P::Empty], &[S::S0], &[A::Output0], None),
    t(&[P::Empty], &[S::O], &[A::OutputO], None),
    t(
        &[P::Comma],
        &[S::S0],
        &[A::Output0, A::Insert(crate::texify::COMMA_DECIMAL)],
        Some(S::O),
    ),
    t(&[P::Dot], &[S::S0], &[A::Output0, A::Copy], Some(S::O)),
    t(&[P::Else], ANY, &[A::TextEq], None),
];
