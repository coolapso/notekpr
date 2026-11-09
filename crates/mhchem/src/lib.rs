// notekpr-mhchem: a Rust port of mhchemParser 4.2.2
// https://github.com/mhchem/mhchemParser
//
// Copyright 2015-2023 Martin Hensel
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//     or in file LICENSE.txt
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Modified for notekpr: translated from TypeScript to Rust (hand-written
// matchers instead of regular expressions), with limits on nesting and work
// for untrusted input, and `expand` in place of the "tex" mode.

//! mhchem's `\ce{…}` (chemical equations) and `\pu{…}` (physical units)
//! as plain TeX, for math renderers that don't know them.
//!
//! * [`to_tex`] translates what is inside one `\ce{…}` or `\pu{…}`, the
//!   way `mhchemParser.toTex(input, "ce" | "pu")` does.
//! * [`expand`] replaces every `\ce{…}` and `\pu{…}` in a TeX string by its
//!   translation in braces and leaves the rest as it is.
//!
//! The input is anyone's note text, so work is bounded: nesting deeper
//! than [`MAX_DEPTH`] or more than [`MAX_WORK`] steps of work give an
//! [`Error`], never a panic, an endless loop or an overflowed stack.
//!
//! A port of mhchemParser 4.2.2 by Martin Hensel (Apache License 2.0, see
//! the notice at the top of this file and LICENSE.txt). It gives the same
//! TeX for the same input, with these differences:
//!
//! * A character outside the Basic Multilingual Plane (an emoji, say) is
//!   one character here and two UTF-16 code units in JavaScript, which
//!   could split it in two.
//! * An unknown `\bond{…}` is reported as such ([`ErrorKind::UnknownBond`]);
//!   mhchemParser means to but throws its generic "bug T" instead.
//! * [`expand`] leaves everything outside `\ce`/`\pu` byte for byte, where
//!   the "tex" mode also turns line breaks into spaces, dashes into `-` and
//!   `…` into `...`, and fails on a carriage return or a Unicode line
//!   separator. Within the limits, it also expands the
//!   `\ce`/`\pu` that a translation passes through unchanged (inside
//!   `$…$` in `\ce`), as MathJax does when it reads the TeX back.

mod machines;
mod parser;
mod patterns;
mod texify;

use std::fmt;

use machines::Machine;
use parser::Parser;

/// How deeply `\ce`, `\pu`, braces, arrows' texts and the like may nest
/// (counted in runs of the parser's state machines, a few per level).
pub const MAX_DEPTH: u32 = 64;

/// The most work one call may do: patterns tried, characters scanned and
/// bytes of TeX written. Ordinary formulas take a few dozen per character.
pub const MAX_WORK: u64 = 1_000_000;

/// Which mhchem command the input is the argument of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `\ce{…}`: chemical equations and formulas.
    Ce,
    /// `\pu{…}`: physical units.
    Pu,
}

/// Translates the argument of a `\ce{…}` or `\pu{…}` into TeX.
///
/// The result is in braces unless it is empty or the input escapes to the
/// surrounding TeX (`&`, `\\`, `\hline`). It can still contain `\ce{…}` or
/// `\pu{…}` that mhchem passes through inside `$…$`; [`expand`] translates
/// those too.
///
/// Besides LaTeX and amsmath, the TeX uses what MathJax's and KaTeX's
/// mhchem extensions define: `\tripledash`, `\longrightleftharpoons`,
/// `\longRightleftharpoons`, `\longLeftrightharpoons`, `\longleftrightarrows`,
/// `\xrightleftharpoons`, and `\raise`/`\lower` with a dimension and an
/// argument in braces (`\lower2mu{…}`).
///
/// ```
/// use notekpr_mhchem::{to_tex, Kind};
/// assert_eq!(
///     to_tex("H2O", Kind::Ce).unwrap(),
///     r"{\mathrm{H}{\vphantom{A}}_{\smash[t]{2}}\mathrm{O}}"
/// );
/// assert_eq!(to_tex("123 kJ", Kind::Pu).unwrap(), r"{123~\mathrm{kJ}}");
/// ```
pub fn to_tex(input: &str, kind: Kind) -> Result<String, Error> {
    let machine = match kind {
        Kind::Ce => Machine::Ce,
        Kind::Pu => Machine::Pu,
    };
    let mut parser = Parser::new();
    let parsed = parser.go(input, machine)?;
    parser.texify(&parsed, true)
}

/// Replaces every `\ce{…}` and `\pu{…}` (with balanced braces) in a TeX
/// string by its translation in braces, leaving everything else as it is.
///
/// ```
/// use notekpr_mhchem::expand;
/// assert_eq!(expand(r"\pu{1.2kg} \ce{H+}").unwrap(), r"{1.2~\mathrm{kg}} {\mathrm{H}{\vphantom{A}}^{+}}");
/// assert_eq!(expand(r"x^2 \ce{").unwrap(), r"x^2 \ce{");
/// ```
pub fn expand(tex: &str) -> Result<String, Error> {
    Parser::new().expand(tex)
}

/// Why a translation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A `}` without its `{` where mhchem looked for the end of a group.
    UnbalancedBrace,
    /// `\bond{…}` with a bond mhchem doesn't know.
    UnknownBond,
    /// Input none of mhchem's rules accepts (mhchem's "bug U").
    Unexpected,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More work than [`MAX_WORK`].
    TooComplex,
    /// A case mhchem itself reports as a bug.
    Internal,
}

/// A translation that failed, with a short message to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn unbalanced_brace() -> Error {
        Error::new(
            ErrorKind::UnbalancedBrace,
            "Extra close brace or missing open brace",
        )
    }

    pub(crate) fn unexpected() -> Error {
        Error::new(ErrorKind::Unexpected, "mhchem bug U. Please report.")
    }

    pub(crate) fn internal() -> Error {
        Error::new(ErrorKind::Internal, "mhchem bug T. Please report.")
    }

    pub(crate) fn too_deep() -> Error {
        Error::new(ErrorKind::TooDeep, "Nested too deeply")
    }

    pub(crate) fn too_complex() -> Error {
        Error::new(ErrorKind::TooComplex, "Too long or too complex")
    }

    /// Why it failed.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message, as mhchem words it where mhchem has the same error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
