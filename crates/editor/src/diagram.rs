//! Diagrams in notes: a code block in Mermaid's language (```` ```mermaid ````,
//! as Obsidian and GitHub write them), shown as the diagram it describes.
//! The host draws it ([`crate::Host::diagram`]; the web client with the
//! `notekpr-diagram` module, loaded on first use); this module only says
//! which blocks are diagrams and how a drawing shows.

use crate::syntax;

/// Whether a code block in `lang` is a diagram.
pub fn is_diagram(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("mermaid")
}

/// A diagram as the host has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drawing {
    /// Drawn: its SVG.
    Svg(String),
    /// Why it can't be drawn.
    Failed(String),
    /// Being drawn (what draws diagrams is still loading).
    Loading,
}

/// What a diagram's preview shows: the drawing, or a line saying why
/// there is none (yet).
pub fn html(d: &Drawing, out: &mut String) {
    match d {
        Drawing::Svg(svg) => out.push_str(svg),
        Drawing::Failed(why) => {
            out.push_str("<div class=\"dgerr\">Can’t draw this diagram: ");
            syntax::escape(why, out);
            out.push_str("</div>");
        }
        Drawing::Loading => out.push_str("<div class=\"dgload\">Drawing the diagram…</div>"),
    }
}
