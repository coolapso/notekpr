//! The editor state and transactions (pure Rust: no DOM, no host).
//!
//! Every change to the editor goes through [`EditorState::apply`] with a
//! [`Transaction`]: changes to the text, the selection afterwards, where it
//! came from and what the host must hear about it. `apply` returns an
//! [`Applied`] that says exactly which lines were replaced and which must be
//! drawn again; the view turns that into DOM work after the state is no
//! longer borrowed.

use std::cell::Cell;
use std::ops::Range;
use std::rc::{Rc, Weak};

use crate::change::{Assoc, ChangeSet};
use crate::decorations::Decorations;
use crate::syntax::{self, Block};
use crate::text::{LineSplice, Text};
use crate::vim::Vim;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Source,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteCursor {
    pub name: String,
    pub color: String,
    pub anchor: usize,
    pub head: usize,
}

/// A highlighted range of the text (e.g. what a comment is about), drawn
/// behind it. It follows edits; text typed right before or after it isn't
/// added to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Byte offsets.
    pub start: usize,
    pub end: usize,
    /// CSS classes of its highlight.
    pub class: String,
    /// The host's name for it.
    pub id: String,
}

/// Byte offsets; `anchor` stays put while `head` moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn new(anchor: usize, head: usize) -> Selection {
        Selection { anchor, head }
    }

    pub fn cursor(at: usize) -> Selection {
        Selection::new(at, at)
    }

    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn map(&self, c: &ChangeSet, assoc: Assoc) -> Selection {
        Selection::new(c.map_pos(self.anchor, assoc), c.map_pos(self.head, assoc))
    }

    pub fn pair(&self) -> (usize, usize) {
        (self.anchor, self.head)
    }
}

/// Where a transaction comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Typing, deleting, pasting.
    Input,
    /// Toolbar, shortcuts, vim, task boxes.
    Command,
    /// An edit the browser made in the DOM itself (IME, spellcheck, …).
    Native,
    /// The document changed elsewhere (another device, a rename, …).
    Remote,
    /// Undo / redo, already applied by the host.
    History,
}

/// Undo grouping hooks: start a new undo group before and/or after the change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Boundary {
    pub before: bool,
    pub after: bool,
}

/// What the host is told about a transaction's changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The changes, made on the editor's text (local edits).
    Changes,
    /// Nothing: the host already has them (remote edits, undo).
    Nothing,
    /// Other changes on another base: a native edit moved over remote text
    /// that arrived while it was being made (see [`merge_native`]).
    Rebased { base: String, changes: ChangeSet },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub changes: ChangeSet,
    /// The selection afterwards; `None` maps the current one through the changes.
    pub selection: Option<Selection>,
    pub origin: Origin,
    pub report: Report,
    pub boundary: Boundary,
    /// Bring the caret into view afterwards.
    pub scroll: bool,
}

impl Transaction {
    pub fn new(changes: ChangeSet, origin: Origin) -> Transaction {
        let report = match origin {
            Origin::Input | Origin::Command | Origin::Native => Report::Changes,
            Origin::Remote | Origin::History => Report::Nothing,
        };
        Transaction {
            changes,
            selection: None,
            origin,
            report,
            boundary: Boundary::default(),
            scroll: false,
        }
    }

    /// Only move the selection.
    pub fn select(doc_len: usize, sel: Selection) -> Transaction {
        Transaction::new(ChangeSet::empty(doc_len), Origin::Command).with_selection(sel)
    }

    pub fn with_selection(mut self, sel: Selection) -> Transaction {
        self.selection = Some(sel);
        self
    }

    pub fn with_boundary(mut self, before: bool, after: bool) -> Transaction {
        self.boundary = Boundary { before, after };
        self
    }

    pub fn scrolling(mut self) -> Transaction {
        self.scroll = true;
        self
    }

    /// This transaction followed by `next` (whose changes apply to this
    /// one's result), as one.
    pub fn then(self, next: Transaction) -> Transaction {
        let selection = next
            .selection
            .or_else(|| self.selection.map(|s| s.map(&next.changes, Assoc::After)));
        Transaction {
            changes: self.changes.compose(&next.changes),
            selection,
            origin: self.origin,
            report: self.report,
            boundary: Boundary {
                before: self.boundary.before,
                after: next.boundary.after,
            },
            scroll: self.scroll || next.scroll,
        }
    }

    pub fn is_local(&self) -> bool {
        self.report != Report::Nothing
    }
}

/// What a transaction did.
#[derive(Clone, Debug)]
pub struct Applied {
    pub changes: ChangeSet,
    /// Replaced lines, in document order.
    pub splices: Vec<LineSplice>,
    /// Lines of the new document to draw again (spliced lines included).
    pub redraw: Vec<Range<usize>>,
    pub selection: Selection,
    pub origin: Origin,
    pub report: Report,
    pub boundary: Boundary,
    pub scroll: bool,
}

/// A position that follows the changes made after it was taken (where an
/// uploaded file's link goes once the upload finished).
#[derive(Clone, Debug)]
pub struct Anchor {
    pos: Rc<Cell<usize>>,
    owner: usize,
}

impl Anchor {
    pub fn pos(&self) -> usize {
        self.pos.get()
    }

    pub(crate) fn owner(&self) -> usize {
        self.owner
    }
}

pub struct EditorState {
    doc: Text,
    deco: Decorations,
    sel: Selection,
    pub readonly: bool,
    pub mode: Mode,
    pub vim: Option<Vim>,
    cursors: Vec<RemoteCursor>,
    marks: Vec<Mark>,
    anchors: Vec<Weak<Cell<usize>>>,
    id: usize,
}

impl EditorState {
    pub fn new(text: String) -> EditorState {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        let doc = Text::new(text);
        EditorState {
            deco: Decorations::new(&doc),
            doc,
            sel: Selection::default(),
            readonly: false,
            mode: Mode::Live,
            vim: None,
            cursors: vec![],
            marks: vec![],
            anchors: vec![],
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn doc(&self) -> &Text {
        &self.doc
    }

    pub fn text(&self) -> &str {
        self.doc.as_str()
    }

    pub fn blocks(&self) -> &[Block] {
        self.deco.blocks()
    }

    pub fn block(&self, line: usize) -> &Block {
        self.deco.block(line)
    }

    /// The code block `line` belongs to (a fence or a line of code): the
    /// lines of its opening fence and of its closing one (`None` while it
    /// isn't closed).
    pub fn code_block(&self, line: usize) -> Option<(usize, Option<usize>)> {
        let opening = |i| matches!(self.block(i), Block::Fence { open: true, .. });
        let open = match self.block(line) {
            Block::Fence { open: true, .. } => line,
            Block::Fence { .. } | Block::Code { .. } => {
                let mut i = line.checked_sub(1)?;
                while matches!(self.block(i), Block::Code { .. }) {
                    i = i.checked_sub(1)?;
                }
                opening(i).then_some(i)?
            }
            _ => return None,
        };
        let n = self.doc.line_count();
        let mut j = open + 1;
        while j < n && matches!(self.block(j), Block::Code { .. }) {
            j += 1;
        }
        let close = j < n && matches!(self.block(j), Block::Fence { open: false, .. });
        Some((open, close.then_some(j)))
    }

    /// The lines of the HTML block `line` is in.
    pub fn html_block(&self, line: usize) -> Option<Range<usize>> {
        let inner = |i| matches!(self.block(i), Block::Html { last: false, .. });
        if !matches!(self.block(line), Block::Html { .. }) {
            return None;
        }
        let mut a = line;
        while a > 0 && inner(a - 1) {
            a -= 1;
        }
        let mut b = line;
        while inner(b) && b + 1 < self.doc.line_count() {
            b += 1;
        }
        Some(a..b + 1)
    }

    /// The source of the HTML block whose last line is `line`: what its
    /// preview shows.
    pub fn html_source(&self, line: usize) -> Option<String> {
        if !matches!(self.block(line), Block::Html { last: true, .. }) {
            return None;
        }
        let lines = self.html_block(line)?;
        Some(
            lines
                .map(|i| self.doc.line(i))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// The code of the block whose opening fence is on `line`.
    pub fn code_text(&self, line: usize) -> String {
        let Some((open, close)) = self.code_block(line) else {
            return String::new();
        };
        let end = close.unwrap_or(self.doc.line_count());
        (open + 1..end)
            .map(|i| self.doc.line(i))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The document as the Markdown helpers take it.
    pub fn syntax_doc(&self) -> syntax::Doc<'_> {
        syntax::Doc {
            text: self.doc.as_str(),
            starts: self.doc.starts(),
            blocks: self.deco.blocks(),
        }
    }

    pub fn selection(&self) -> Selection {
        self.sel
    }

    /// Set the selection (clamped onto character boundaries).
    pub fn set_selection(&mut self, sel: Selection) {
        self.sel = Selection::new(self.doc.floor(sel.anchor), self.doc.floor(sel.head));
    }

    pub fn cursors(&self) -> &[RemoteCursor] {
        &self.cursors
    }

    pub fn set_cursors(&mut self, cursors: Vec<RemoteCursor>) {
        self.cursors = cursors;
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// Set the highlighted ranges (clamped onto the text; empty ones are
    /// dropped).
    pub fn set_marks(&mut self, marks: Vec<Mark>) {
        self.marks = marks
            .into_iter()
            .map(|m| Mark {
                start: self.doc.floor(m.start),
                end: self.doc.floor(m.end),
                ..m
            })
            .filter(|m| m.start < m.end)
            .collect();
    }

    /// Ids of the marks around `pos` (from their start up to their end).
    pub fn marks_at(&self, pos: usize) -> Vec<String> {
        self.marks
            .iter()
            .filter(|m| m.start <= pos && pos <= m.end)
            .map(|m| m.id.clone())
            .collect()
    }

    /// A key for vim mode (`None`: vim is off, or the key is not vim's).
    pub fn vim_key(
        &mut self,
        key: &str,
        mods: crate::vim::Mods,
        sel: (usize, usize),
    ) -> Option<crate::vim::Action> {
        let EditorState { doc, vim, .. } = self;
        vim.as_mut()?.key(key, mods, doc.as_str(), sel)
    }

    /// Track `at` across later changes.
    pub fn anchor(&mut self, at: usize) -> Anchor {
        let pos = Rc::new(Cell::new(self.doc.floor(at)));
        self.anchors.retain(|w| w.strong_count() > 0);
        self.anchors.push(Rc::downgrade(&pos));
        Anchor {
            pos,
            owner: self.id,
        }
    }

    /// Where `a` is now, if it was taken in this state.
    pub fn anchor_pos(&self, a: &Anchor) -> Option<usize> {
        (a.owner() == self.id).then(|| self.doc.floor(a.pos()))
    }

    pub fn apply(&mut self, tr: Transaction) -> Applied {
        let Transaction {
            mut changes,
            selection,
            origin,
            report,
            boundary,
            scroll,
        } = tr;
        if changes.base_len() != self.doc.len() {
            // A change for another text (a bug): drop it rather than corrupt ours.
            debug_assert!(false, "change for a text of another length");
            changes = ChangeSet::empty(self.doc.len());
        }
        let sel = selection.unwrap_or_else(|| self.sel.map(&changes, Assoc::After));
        let splices = self.doc.apply(&changes);
        let mut redraw = self.deco.update(&self.doc, &splices);
        // The last line of an HTML block shows the whole block's preview:
        // redraw it when a line of the block changed, or a line above it
        // (which may have left the block).
        let mut lasts: Vec<Range<usize>> = vec![];
        for r in &redraw {
            let mut i = r.start;
            while i <= r.end && i < self.doc.line_count() {
                match self.html_block(i) {
                    Some(b) => {
                        lasts.push(b.end - 1..b.end);
                        i = b.end;
                    }
                    None => i += 1,
                }
            }
        }
        if !lasts.is_empty() {
            redraw.extend(lasts);
            redraw = crate::decorations::merge_ranges(redraw);
        }
        if !changes.is_empty() {
            for c in &mut self.cursors {
                c.anchor = changes.map_pos(c.anchor, Assoc::Before);
                c.head = changes.map_pos(c.head, Assoc::Before);
            }
            for m in &mut self.marks {
                m.start = changes.map_pos(m.start, Assoc::After);
                m.end = changes.map_pos(m.end, Assoc::Before);
            }
            self.marks.retain(|m| m.start < m.end);
            self.anchors.retain(|w| match w.upgrade() {
                Some(p) => {
                    p.set(changes.map_pos(p.get(), Assoc::Before));
                    true
                }
                None => false,
            });
        }
        self.set_selection(sel);
        Applied {
            changes,
            splices,
            redraw,
            selection: self.sel,
            origin,
            report,
            boundary,
            scroll,
        }
    }
}

/// A change the browser made natively on `base` (the editor's text), merged
/// with `remote`, a newer text of the document that arrived meanwhile (an
/// IME composition must not be disturbed, so remote text waits for its end).
///
/// Returns the transaction that takes the editor from `base` to the merged
/// text and tells the host the native change as made on `remote`, and the
/// change from what the DOM shows (`base` + native) to the merged text (to
/// map the DOM's caret).
pub fn merge_native(
    base: &str,
    native: ChangeSet,
    remote: Option<String>,
) -> (Transaction, ChangeSet) {
    match remote {
        Some(r) if r != base => {
            let theirs = ChangeSet::diff(base, &r);
            // Where both inserted at the same place, their text goes first.
            let (theirs_after, mine) = theirs.transform(&native);
            let mut tr = Transaction::new(native.compose(&theirs_after), Origin::Native);
            tr.report = Report::Rebased {
                base: r,
                changes: mine,
            };
            (tr, theirs_after)
        }
        _ => {
            let after = ChangeSet::empty(native.target_len());
            (Transaction::new(native, Origin::Native), after)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_blocks_and_their_previews() {
        let mut st = EditorState::new("<!-- a\nb\n<!-- x -->\n\n<div>\n<b>y</b>\n</div>".into());
        assert_eq!(st.html_block(1), Some(0..3));
        assert_eq!(st.html_source(1), None);
        assert_eq!(st.html_source(2).as_deref(), Some("<!-- a\nb\n<!-- x -->"));
        assert_eq!(st.html_block(3), None);
        assert_eq!(
            st.html_source(6).as_deref(),
            Some("<div>\n<b>y</b>\n</div>")
        );
        // An edit in a block redraws its last line, which shows the preview.
        let at = st.text().find("y<").unwrap();
        let applied = st.apply(Transaction::new(
            ChangeSet::replace(st.doc.len(), at, at, "z"),
            Origin::Input,
        ));
        assert!(
            applied.redraw.iter().any(|r| r.contains(&6)),
            "{:?}",
            applied.redraw
        );
        // So does one that takes lines out of a block above an unchanged
        // last line (found by the property tests).
        let applied = st.apply(Transaction::new(
            ChangeSet::replace(st.doc.len(), 0, 5, ""),
            Origin::Input,
        ));
        assert_eq!(st.html_source(2).as_deref(), Some("<!-- x -->"));
        assert!(
            applied.redraw.iter().any(|r| r.contains(&2)),
            "{:?}",
            applied.redraw
        );
    }

    #[test]
    fn finds_code_blocks_and_their_code() {
        let st = EditorState::new("a\n```sh title=x.sh\nls\npwd\n```\nb\n```\nopen".into());
        assert_eq!(st.code_block(0), None);
        for line in 1..=4 {
            assert_eq!(st.code_block(line), Some((1, Some(4))), "line {line}");
        }
        assert_eq!(st.code_block(5), None);
        assert_eq!(st.code_block(7), Some((6, None)));
        assert_eq!(st.code_text(1), "ls\npwd");
        assert_eq!(st.code_text(6), "open");
        let empty = EditorState::new("```\n```".into());
        assert_eq!(empty.code_block(1), Some((0, Some(1))));
        assert_eq!(empty.code_text(0), "");
    }

    #[test]
    fn transactions_update_selection_cursors_and_anchors() {
        let mut st = EditorState::new("hello world".into());
        st.set_selection(Selection::cursor(6));
        st.set_cursors(vec![RemoteCursor {
            name: "b".into(),
            color: "red".into(),
            anchor: 6,
            head: 6,
        }]);
        let a = st.anchor(6);
        let applied = st.apply(Transaction::new(
            ChangeSet::replace(11, 6, 6, "big "),
            Origin::Remote,
        ));
        // The local caret moves past text inserted at it; others stay before it.
        assert_eq!(applied.selection, Selection::cursor(10));
        assert_eq!(st.cursors()[0].head, 6);
        assert_eq!(st.anchor_pos(&a), Some(6));
        assert_eq!(st.text(), "hello big world");
        assert_eq!(applied.report, Report::Nothing);
        // An anchor of another editor is not valid here.
        let other = EditorState::new(String::new()).anchor(0);
        assert_eq!(st.anchor_pos(&other), None);
    }

    #[test]
    fn marks_follow_edits_without_growing() {
        let mut st = EditorState::new("buy milk today".into());
        st.set_marks(vec![
            Mark {
                start: 4,
                end: 8,
                class: "c".into(),
                id: "m".into(),
            },
            Mark {
                start: 2,
                end: 2,
                class: "c".into(),
                id: "empty".into(),
            },
        ]);
        assert_eq!(st.marks().len(), 1, "empty ones are dropped");
        let edit = |st: &mut EditorState, at: usize, del: usize, ins: &str| {
            let len = st.doc().len();
            st.apply(Transaction::new(
                ChangeSet::replace(len, at, at + del, ins),
                Origin::Input,
            ));
        };
        // Typed right before and right after: not part of it; inside: is.
        edit(&mut st, 4, 0, "(");
        edit(&mut st, 9, 0, ")");
        edit(&mut st, 6, 0, "i");
        assert_eq!(st.text(), "buy (miilk) today");
        let m = &st.marks()[0];
        assert_eq!((m.start, m.end), (5, 10));
        assert_eq!(st.marks_at(7), ["m"]);
        assert!(st.marks_at(12).is_empty());
        // Gone with its text.
        edit(&mut st, 5, 5, "");
        assert!(st.marks().is_empty());
    }

    #[test]
    fn composed_transactions() {
        let t = Transaction::new(ChangeSet::replace(3, 3, 3, "d"), Origin::Input)
            .with_boundary(true, false)
            .then(
                Transaction::new(ChangeSet::replace(4, 0, 0, ">"), Origin::Input)
                    .with_boundary(false, true),
            );
        let mut st = EditorState::new("abc".into());
        st.set_selection(Selection::cursor(3));
        let applied = st.apply(t);
        assert_eq!(st.text(), ">abcd");
        assert_eq!(
            applied.boundary,
            Boundary {
                before: true,
                after: true
            }
        );
        assert_eq!(applied.selection, Selection::cursor(5));
    }

    #[test]
    fn native_edits_merge_with_waiting_remote_text() {
        let base = "Shopping: milk\neggs";
        let native = ChangeSet::diff(base, "かなShopping: milk\neggs");
        let remote = "Shopping: milk\neggs remote".to_string();
        let (tr, after) = merge_native(base, native.clone(), Some(remote.clone()));
        let merged = tr.changes.apply(base).unwrap();
        assert_eq!(merged, "かなShopping: milk\neggs remote");
        let Report::Rebased { base: b, changes } = &tr.report else {
            panic!("not rebased")
        };
        assert_eq!(b, &remote);
        assert_eq!(changes.apply(&remote).unwrap(), merged);
        // From what the DOM shows to the merged text.
        let dom = native.apply(base).unwrap();
        assert_eq!(after.apply(&dom).unwrap(), merged);
        // Without remote text (or an unchanged one) it is the native change.
        assert_eq!(
            merge_native(base, native.clone(), None).0.report,
            Report::Changes
        );
        assert_eq!(
            merge_native(base, native, Some(base.into())).0.report,
            Report::Changes
        );
    }
}
