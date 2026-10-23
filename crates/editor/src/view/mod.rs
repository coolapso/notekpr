//! The DOM view: a `contenteditable` element showing an [`EditorState`].
//!
//! The element contains one `div.ln` per line of the source and nothing
//! else; the text of each is exactly the line's Markdown (syntax characters
//! are styled, and hidden with CSS, never removed). Anything that isn't
//! source text is `contenteditable="false"` with `data-w`.
//!
//! Native input is intercepted in `beforeinput` and turned into a
//! [`Transaction`]. A transaction re-renders only the lines it replaced and
//! the lines whose Markdown block changed — never the whole note. What the
//! browser changes on its own (IME composition, spellcheck, unusual input
//! types) is picked up by a `MutationObserver` and reconciled line by line:
//! the touched lines are read back, the difference becomes a transaction, and
//! the lines are rendered again so no stray markup stays behind.
//!
//! # Borrows and re-entrancy
//!
//! DOM calls can fire events synchronously (focusing, changing
//! `contenteditable`), and host calls can call back into the editor (a host
//! may push new text from inside `edit`). The view is built so that neither
//! can find it half-way through an update:
//!
//! 1. **The state is only borrowed inside [`Editor::model`]**, whose closure
//!    is `'static`: it cannot capture `&self`, so it cannot reach the DOM or
//!    the host. It computes what to do (a transaction, the lines to render)
//!    and returns owned data. DOM work and host calls happen afterwards.
//! 2. **One operation at a time.** Every entry point runs as an operation
//!    ([`Editor::op`], [`Editor::run_now`]). A public method called while
//!    one is in progress (re-entrantly, from a host callback) is queued and
//!    runs right after it; an event fired synchronously by our own DOM calls
//!    (focus, blur, selection changes) is deferred to its end. So the DOM
//!    bookkeeping (`view`) is never touched by two operations at once.
//! 3. **No DOM changes during an IME composition.** Remote text, refreshes
//!    and redraws wait until the composition has ended; the composed text is
//!    then merged with whatever arrived meanwhile (see
//!    [`crate::state::merge_native`]).

mod dom;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;
use std::rc::{Rc, Weak};

use notekpr_core::textdiff::Edit;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, EventTarget, HtmlElement, InputEvent, KeyboardEvent, MouseEvent, MutationObserver,
    MutationRecord, Node,
};

use crate::change::{Assoc, ChangeSet};
use crate::commands::{self, Command};
use crate::state::{
    Anchor, Applied, EditorState, Mark, Mode, Origin, RemoteCursor, Report, Selection, Transaction,
    merge_native,
};
use crate::syntax::{self, Target};
use crate::text::{LineSplice, utf16_to_byte};
use crate::vim;
use dom::{LineJob, Rendered, document};

/// What the editor needs from the application. The editor never calls these
/// while its state is borrowed, and a host may call back into the editor
/// from them (such calls run once the current operation is done).
pub trait Host {
    /// Local edits made on `base` (the editor's text before them), already
    /// applied to the editor's text. Returns `false` if the editor must reload
    /// the document's text: the edits were refused (e.g. too large to sync),
    /// or the document no longer matched `base`.
    fn edit(&self, base: &str, edits: &[Edit]) -> bool;
    /// The document's current text.
    fn text(&self) -> Option<String>;
    fn undo(&self) -> Option<String>;
    fn redo(&self) -> Option<String>;
    /// Start a new undo group.
    fn boundary(&self);
    fn link_exists(&self, target: &str) -> bool;
    fn open_link(&self, target: &Target, new_tab: bool);
    /// Blob URL for an embedded image, if already loaded (the host loads it
    /// otherwise and calls [`Editor::refresh`]).
    fn image_url(&self, target: &str) -> Option<String>;
    fn selection_changed(&self, anchor: usize, head: usize);
    /// Files pasted or dropped at offset `at` (see [`Editor::anchor`] to keep
    /// track of that position while they upload).
    fn insert_files(&self, files: Vec<web_sys::File>, at: usize);
}

struct LineView {
    el: HtmlElement,
    /// Hash of what was rendered into `el` (0: must be rendered).
    hash: u64,
    dynamic: bool,
    /// A line of code: the `--code-w` it was given (its block's widest
    /// line). An opening fence: how far its block is scrolled sideways.
    code_w: u32,
    sx: i32,
}

#[derive(Default)]
struct View {
    lines: Vec<LineView>,
    /// Elements that have the `active` class (the lines of the selection
    /// while focused, and the fences of a code block it is in: their syntax
    /// markers show).
    active: Vec<HtmlElement>,
    active_lines: Vec<Range<usize>>,
    /// Range of an internal drag (text dragged within the editor).
    drag: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    /// Between `compositionstart` and `compositionend`.
    Active,
    /// After `compositionend`, until the browser's last composition events
    /// are through (Safari sends `deleteCompositionText` and
    /// `insertFromComposition` after `compositionend`).
    Ending,
}

#[derive(Default)]
struct Ime {
    phase: Phase,
    /// The newest document text that arrived during the composition.
    pending: Option<String>,
    /// A refresh was asked for during the composition.
    refresh: bool,
}

const DEFER_SELECTION: u8 = 1;
const DEFER_ACTIVE: u8 = 2;

/// The height of a remote cursor's name (`.rc span`: 11px text, line
/// height 1.5).
const NAME_HEIGHT: f64 = 17.0;

/// Target, event, capture phase, callback.
type Listener = (
    EventTarget,
    &'static str,
    bool,
    Closure<dyn FnMut(web_sys::Event)>,
);
type Queued = Box<dyn FnOnce(&Editor)>;
type Observer = (MutationObserver, Closure<dyn FnMut(js_sys::Array)>);
type Resize = (web_sys::ResizeObserver, Closure<dyn FnMut(js_sys::Array)>);

pub struct Editor {
    wrap: HtmlElement,
    root: HtmlElement,
    overlay: HtmlElement,
    /// Behind the text: the highlights of [`Mark`]s.
    marks: HtmlElement,
    host: Rc<dyn Host>,
    /// The document model. Borrowed only inside [`Editor::model`].
    state: RefCell<EditorState>,
    /// DOM bookkeeping. Borrowed only by the operation in progress.
    view: RefCell<View>,
    ime: RefCell<Ime>,
    /// An operation is running.
    busy: Cell<bool>,
    /// [`Editor::model`] is running (checked in debug builds).
    in_model: Cell<bool>,
    /// Calls made while an operation was running.
    queue: RefCell<VecDeque<Queued>>,
    /// Events that arrived while an operation was running.
    deferred: Cell<u8>,
    /// Mutation records seen by the observer callback but not handled yet.
    records: RefCell<Vec<MutationRecord>>,
    observer: RefCell<Option<Observer>>,
    /// Redraws the overlays when the text's size changes (wrapping,
    /// images loading).
    resize: RefCell<Option<Resize>>,
    listeners: RefCell<Vec<Listener>>,
    /// Pending animation frame (cursor drawing, scrolling) and its callback.
    frame: Cell<Option<i32>>,
    frame_cb: RefCell<Option<Closure<dyn FnMut()>>>,
    frame_scroll: Cell<bool>,
    frame_cursors: Cell<bool>,
    /// Timer that finishes a composition, and its callback.
    ime_timer: Cell<Option<i32>>,
    ime_cb: RefCell<Option<Closure<dyn FnMut()>>>,
    destroyed: Cell<bool>,
    /// The primary mouse button is down in the editor: which lines show
    /// their Markdown waits until it is released. Showing a line's markers
    /// moves its text, which would leave a drag that started there
    /// selecting nothing.
    mouse_down: Cell<bool>,
}

fn is_composition_input(t: &str) -> bool {
    matches!(
        t,
        "insertCompositionText"
            | "deleteCompositionText"
            | "insertFromComposition"
            | "deleteByComposition"
    )
}

/// Input types the editor carries out itself (or refuses) in `beforeinput`.
fn handled_input(t: &str) -> bool {
    matches!(
        t,
        "insertText"
            | "insertReplacementText"
            | "insertParagraph"
            | "insertLineBreak"
            | "insertFromPaste"
            | "insertFromDrop"
            | "insertFromYank"
            | "insertFromPasteAsQuotation"
            | "historyUndo"
            | "historyRedo"
            | "insertHorizontalRule"
            | "insertLink"
    ) || t.starts_with("delete")
        || t.starts_with("format")
        || (t.starts_with("insert") && t.ends_with("List"))
}

/// Ctrl+Alt+0…6 (⌘+Option on macs): a heading level. On macs Option
/// changes the key ("¡", "™", …), so the physical key counts there;
/// elsewhere the key must be the digit itself, so AltGr+0 ("}" on some
/// layouts) still types.
fn heading_level(ev: &KeyboardEvent) -> Option<u8> {
    if !ev.alt_key() || ev.shift_key() {
        return None;
    }
    let level = |s: &str| {
        (s.len() == 1 && "0123456".contains(s))
            .then(|| s.parse().ok())
            .flatten()
    };
    if ev.meta_key() && !ev.ctrl_key() {
        let code = ev.code();
        return level(&ev.key()).or_else(|| code.strip_prefix("Digit").and_then(level));
    }
    if ev.ctrl_key() && !ev.meta_key() && !ev.get_modifier_state("AltGraph") {
        return level(&ev.key());
    }
    None
}
impl Editor {
    /// Build an editor inside `container` (which should be empty).
    pub fn new(
        container: &Element,
        text: String,
        host: Rc<dyn Host>,
        mode: Mode,
        readonly: bool,
        vim: bool,
    ) -> Rc<Editor> {
        let doc = document();
        let el = |class: &str| -> HtmlElement {
            let e: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
            e.set_class_name(class);
            e
        };
        let (wrap, root, overlay) = (el("ed-wrap"), el("ed"), el("ed-cursors"));
        let marks = el("ed-marks");
        let _ = marks.set_attribute("aria-hidden", "true");
        let _ = root.set_attribute("spellcheck", "true");
        let _ = root.set_attribute("autocapitalize", "sentences");
        let _ = root.set_attribute("role", "textbox");
        let _ = root.set_attribute("aria-multiline", "true");
        let _ = root.set_attribute("aria-label", "Note text");
        let _ = overlay.set_attribute("aria-hidden", "true");
        let _ = wrap.append_child(&marks);
        let _ = wrap.append_child(&root);
        let _ = wrap.append_child(&overlay);
        let _ = container.append_child(&wrap);
        let mut st = EditorState::new(text);
        st.mode = mode;
        st.readonly = readonly;
        st.vim = vim.then(vim::Vim::default);
        let ed = Rc::new(Editor {
            wrap,
            root,
            overlay,
            marks,
            host,
            state: RefCell::new(st),
            view: RefCell::new(View::default()),
            ime: RefCell::new(Ime::default()),
            busy: Cell::new(false),
            in_model: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
            deferred: Cell::new(0),
            records: RefCell::new(vec![]),
            observer: RefCell::new(None),
            resize: RefCell::new(None),
            listeners: RefCell::new(vec![]),
            frame: Cell::new(None),
            frame_cb: RefCell::new(None),
            frame_scroll: Cell::new(false),
            frame_cursors: Cell::new(false),
            ime_timer: Cell::new(None),
            ime_cb: RefCell::new(None),
            destroyed: Cell::new(false),
            mouse_down: Cell::new(false),
        });
        ed.op(|ed| {
            ed.apply_mode_classes();
            ed.render_all();
        });
        Editor::attach(&ed);
        ed
    }

    // ------------------------------------------------------------ discipline

    /// Run `f` with the state borrowed. `f` is `'static`, so it can't reach
    /// the DOM or the host through `self`; it returns what the caller then
    /// does with them.
    fn model<R: 'static>(&self, f: impl FnOnce(&mut EditorState) -> R + 'static) -> R {
        debug_assert!(!self.in_model.get(), "editor state borrowed re-entrantly");
        self.in_model.set(true);
        let r = f(&mut self.state.borrow_mut());
        self.in_model.set(false);
        r
    }

    /// Run `f` as an operation now, or queue it if one is in progress.
    fn op(&self, f: impl FnOnce(&Editor) + 'static) {
        if self.destroyed.get() {
            return;
        }
        if self.busy.get() {
            self.queue.borrow_mut().push_back(Box::new(f));
            return;
        }
        self.busy.set(true);
        f(self);
        self.finish_op();
    }

    /// Run `f` as an operation now, if none is in progress (event handlers:
    /// they must act synchronously, e.g. to prevent the default). Returns
    /// whether it ran.
    fn run_now(&self, f: impl FnOnce(&Editor)) -> bool {
        if self.destroyed.get() || self.busy.get() {
            return false;
        }
        self.busy.set(true);
        f(self);
        self.finish_op();
        true
    }

    fn finish_op(&self) {
        loop {
            if self.destroyed.get() {
                self.queue.borrow_mut().clear();
                break;
            }
            let d = self.deferred.replace(0);
            if d & DEFER_SELECTION != 0 {
                self.selection_from_dom();
            } else if d & DEFER_ACTIVE != 0 {
                self.sync_active();
            }
            let next = self.queue.borrow_mut().pop_front();
            match next {
                Some(f) => f(self),
                None if self.deferred.get() == 0 => break,
                None => {}
            }
        }
        self.busy.set(false);
    }

    fn defer(&self, what: u8) {
        self.deferred.set(self.deferred.get() | what);
    }

    // ------------------------------------------------------------ public API

    pub fn element(&self) -> &HtmlElement {
        &self.root
    }

    pub fn text(&self) -> String {
        self.state.borrow().text().to_string()
    }

    pub fn selection(&self) -> (usize, usize) {
        self.state.borrow().selection().pair()
    }

    pub fn has_focus(&self) -> bool {
        document()
            .active_element()
            .is_some_and(|e| e.is_same_node(Some(self.root.unchecked_ref())))
    }

    pub fn set_mode(&self, mode: Mode) {
        self.op(move |ed| {
            ed.model(move |st| st.mode = mode);
            ed.apply_mode_classes();
            ed.request_frame(false, true);
        });
    }

    pub fn set_readonly(&self, ro: bool) {
        self.op(move |ed| {
            ed.model(move |st| st.readonly = ro);
            ed.apply_mode_classes();
        });
    }

    pub fn set_vim(&self, on: bool) {
        self.op(move |ed| {
            // Called on every preference change: keep the current vim state.
            let changed = ed.model(move |st| {
                if st.vim.is_some() == on {
                    return false;
                }
                st.vim = on.then(vim::Vim::default);
                true
            });
            if changed {
                ed.apply_mode_classes();
            }
        });
    }

    pub fn focus(&self) {
        self.op(|ed| {
            let _ = ed.root.focus();
            let sel = ed.model(|st| st.selection());
            ed.place_dom_selection(sel, true);
            ed.sync_active();
        });
    }

    /// Select `[anchor, head)` and scroll the caret into view.
    pub fn set_selection(&self, anchor: usize, head: usize) {
        self.op(move |ed| {
            let sel = ed.model(move |st| {
                st.set_selection(Selection::new(anchor, head));
                st.selection()
            });
            ed.place_dom_selection(sel, true);
            ed.sync_active();
        });
    }

    /// Render the lines whose look depends on the host again (link targets
    /// or images changed).
    pub fn refresh(&self) {
        self.op(|ed| {
            if ed.ime.borrow().phase != Phase::Idle {
                ed.ime.borrow_mut().refresh = true;
                return;
            }
            ed.refresh_now();
        });
    }

    /// Replace the selection with `s` (e.g. links to uploaded files).
    /// The page's styles for code blocks changed (long lines wrap or not):
    /// lay the blocks out again.
    pub fn code_styles_changed(&self) {
        self.op(|ed| {
            let n = ed.view.borrow().lines.len();
            ed.code_layout((0..n).collect());
        });
    }

    pub fn insert_text(&self, s: &str) {
        let s = s.to_string();
        self.op(move |ed| {
            ed.settle();
            if let Some(tr) = ed.model(move |st| commands::insert_text(st, &s)) {
                ed.dispatch(tr);
            }
        });
    }

    /// Track offset `at` across later changes (where to put the links of
    /// files that are still uploading). Safe to call at any time.
    pub fn anchor(&self, at: usize) -> Anchor {
        self.model(move |st| st.anchor(at))
    }

    /// Insert `s` at an anchor taken from this editor (at the selection if it
    /// came from another one). The selection is not moved there.
    pub fn insert_at(&self, anchor: &Anchor, s: &str) {
        let (a, s) = (anchor.clone(), s.to_string());
        self.op(move |ed| {
            ed.settle();
            let tr = ed.model(move |st| {
                let at = st.anchor_pos(&a).unwrap_or(st.selection().from());
                let mut tr = commands::replace(st, at, at, &s, None)?;
                tr.selection = None;
                tr.scroll = false;
                Some(tr)
            });
            if let Some(tr) = tr {
                ed.dispatch(tr);
            }
        });
    }

    /// Replace the whole text with a new version produced elsewhere (remote
    /// edits, undo). The selection is mapped through the change; with
    /// `caret_to_change` (undo/redo) it moves to the change instead.
    pub fn set_text(&self, new_text: String, caret_to_change: bool) {
        self.op(move |ed| ed.set_text_now(new_text, caret_to_change));
    }

    pub fn command(&self, cmd: Command) {
        self.op(move |ed| ed.command_now(cmd));
    }

    pub fn set_remote_cursors(&self, cursors: Vec<RemoteCursor>) {
        self.op(move |ed| {
            ed.model(move |st| st.set_cursors(cursors));
            ed.request_frame(false, true);
        });
    }

    /// Highlight these ranges (replacing earlier ones). They follow edits
    /// until set again.
    pub fn set_marks(&self, marks: Vec<Mark>) {
        self.op(move |ed| {
            ed.model(move |st| st.set_marks(marks));
            ed.request_frame(false, true);
        });
    }

    /// Ids of the highlighted ranges around offset `pos`.
    pub fn marks_at(&self, pos: usize) -> Vec<String> {
        self.model(move |st| st.marks_at(pos))
    }

    /// Where a highlighted range is now (byte offsets), if it still is.
    pub fn mark_range(&self, id: &str) -> Option<(usize, usize)> {
        let id = id.to_string();
        self.model(move |st| {
            st.marks()
                .iter()
                .find(|m| m.id == id)
                .map(|m| (m.start, m.end))
        })
    }

    /// Remove the editor from the page and stop listening.
    pub fn destroy(&self) {
        self.op(|ed| {
            ed.teardown();
            ed.wrap.remove();
        });
    }

    // ------------------------------------------------------------ rendering

    fn apply_mode_classes(&self) {
        let (c, editable) = self.model(|st| {
            let mut c = String::from("ed");
            c.push_str(if st.mode == Mode::Source {
                " source"
            } else {
                " live"
            });
            if st.readonly {
                c.push_str(" readonly");
            }
            if let Some(v) = &st.vim {
                c.push_str(if v.insert {
                    " vim vim-insert"
                } else {
                    " vim vim-normal"
                });
            }
            (c, if st.readonly { "false" } else { "true" })
        });
        // Changing a focused element can fire focus/blur synchronously; they
        // are deferred until this operation is done.
        self.root.set_class_name(&c);
        if self.root.get_attribute("contenteditable").as_deref() != Some(editable) {
            let _ = self.root.set_attribute("contenteditable", editable);
        }
    }

    fn render_jobs(&self, jobs: Vec<LineJob>) -> Vec<Rendered> {
        let host = &self.host;
        let links = |t: &str| host.link_exists(t);
        let images = |t: &str| host.image_url(t);
        jobs.into_iter()
            .map(|j| dom::render_line(j, &links, &images))
            .collect()
    }

    /// Build every line (a new editor).
    fn render_all(&self) {
        let jobs = self.model(|st| {
            (0..st.doc().line_count())
                .map(|i| LineJob {
                    idx: i,
                    text: st.doc().line(i).to_string(),
                    block: st.block(i).clone(),
                    html: st.html_source(i),
                })
                .collect::<Vec<_>>()
        });
        let rendered = self.render_jobs(jobs);
        let mut html = String::new();
        for r in &rendered {
            html.push_str("<div class=\"");
            html.push_str(&r.class);
            html.push_str("\">");
            html.push_str(&r.html);
            html.push_str("</div>");
        }
        self.root.set_inner_html(&html);
        let kids = self.root.children();
        let mut view = self.view.borrow_mut();
        view.lines = rendered
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let el = kids.item(i as u32)?;
                dom::fill_widget(&el, r);
                Some(LineView {
                    el: el.unchecked_into(),
                    hash: r.hash,
                    dynamic: r.dynamic,
                    code_w: 0,
                    sx: 0,
                })
            })
            .collect();
        view.active.clear();
        view.active_lines.clear();
        let n = view.lines.len();
        drop(view);
        self.take_own_records();
        self.code_layout((0..n).collect());
    }

    /// Bring the DOM in line with an applied transaction: replace the
    /// spliced line elements and render the given lines (sorted by index).
    fn patch(&self, splices: &[LineSplice], rendered: Vec<Rendered>, active: &[Range<usize>]) {
        let active = |i: usize| active.iter().any(|r| r.contains(&i));
        let touched: Vec<usize> = rendered
            .iter()
            .map(|r| r.idx)
            .chain(splices.iter().flat_map(|sp| sp.new_range()))
            .collect();
        let doc = document();
        let mut view = self.view.borrow_mut();
        let find = |idx: usize| rendered.binary_search_by_key(&idx, |r| r.idx).ok();
        for sp in splices.iter().rev() {
            let keep = sp.old.min(sp.new);
            let old_end = (sp.old_start + sp.old).min(view.lines.len());
            let from = (sp.old_start + keep).min(old_end);
            for lv in &view.lines[from..old_end] {
                lv.el.remove();
            }
            let mut new_views = Vec::with_capacity(sp.new - keep);
            if sp.new > keep {
                // Parse the new lines in one go.
                let mut html = String::new();
                let range = sp.new_start + keep..sp.new_start + sp.new;
                for idx in range.clone() {
                    let r = find(idx).map(|k| &rendered[k]);
                    html.push_str("<div class=\"");
                    html.push_str(&dom::class_with(
                        r.map(|r| r.class.as_ref()).unwrap_or("ln"),
                        active(idx),
                    ));
                    html.push_str("\">");
                    html.push_str(r.map(|r| r.html.as_str()).unwrap_or("<br>"));
                    html.push_str("</div>");
                }
                let tpl: web_sys::HtmlTemplateElement =
                    doc.create_element("template").unwrap().unchecked_into();
                tpl.set_inner_html(&html);
                let frag = tpl.content();
                let kids = frag.children();
                for (k, idx) in range.enumerate() {
                    if let Some(el) = kids.item(k as u32) {
                        let r = find(idx).map(|k| &rendered[k]);
                        if let Some(r) = r {
                            dom::fill_widget(&el, r);
                        }
                        new_views.push(LineView {
                            el: el.unchecked_into(),
                            hash: r.map(|r| r.hash).unwrap_or(0),
                            dynamic: r.is_some_and(|r| r.dynamic),
                            code_w: 0,
                            sx: 0,
                        });
                    }
                }
                let before: Option<Node> = view
                    .lines
                    .get(old_end)
                    .map(|lv| lv.el.clone().unchecked_into());
                let _ = self.root.insert_before(&frag, before.as_ref());
            }
            view.lines.splice(from..old_end, new_views);
        }
        for r in rendered {
            let Some(lv) = view.lines.get_mut(r.idx) else {
                continue;
            };
            if lv.hash != r.hash {
                lv.el
                    .set_class_name(&dom::class_with(&r.class, active(r.idx)));
                lv.el.set_inner_html(&r.html);
                dom::fill_widget(&lv.el, &r);
                lv.hash = r.hash;
            }
            lv.dynamic = r.dynamic;
        }
        drop(view);
        self.take_own_records();
        self.code_layout(touched);
    }

    /// Long lines of code that don't wrap scroll sideways, the lines of a
    /// block together. Give the lines of the blocks the `touched` lines are
    /// in the width of their block's longest line (`--code-w`, measured: so
    /// that all of them can scroll as far) and the block's scroll position
    /// (new lines start at the left). Blocks that wrap are left alone.
    fn code_layout(&self, mut touched: Vec<usize>) {
        touched.sort_unstable();
        touched.dedup();
        let blocks = self.model(move |st| {
            let n = st.doc().line_count();
            let mut out = Vec::new();
            let mut done = 0;
            for i in touched {
                if i < done {
                    continue;
                }
                let Some((open, close)) = st.code_block(i) else {
                    continue;
                };
                let end = close.unwrap_or(n);
                out.push((open, end));
                done = end + 1;
            }
            out
        });
        let win = web_sys::window();
        let mut view = self.view.borrow_mut();
        for (open, end) in blocks {
            let lines = view.lines.get(open + 1..end).unwrap_or_default();
            let parts: Vec<Option<Element>> = lines.iter().map(|lv| code_part(&lv.el)).collect();
            // Whether the block scrolls (doesn't wrap) is up to CSS.
            let scrolls = parts.iter().flatten().next().is_some_and(|lx| {
                win.as_ref()
                    .and_then(|w| w.get_computed_style(lx).ok().flatten())
                    .and_then(|s| s.get_property_value("overflow-x").ok())
                    .is_some_and(|o| o != "visible")
            });
            if !scrolls {
                continue;
            }
            // The text's own width: indentation and text don't stretch.
            let width = parts
                .iter()
                .flatten()
                .map(|lx| {
                    let kids = lx.children();
                    (0..kids.length())
                        .filter_map(|k| kids.item(k))
                        .filter(|k| !k.has_attribute("data-w"))
                        .map(|k| k.unchecked_into::<HtmlElement>().offset_width())
                        .sum::<i32>()
                })
                .max()
                .unwrap_or(0)
                .max(0) as u32;
            let sx = view.lines.get(open).map_or(0, |l| l.sx);
            for lv in view.lines.get_mut(open + 1..end).into_iter().flatten() {
                if lv.code_w != width {
                    let _ = lv
                        .el
                        .style()
                        .set_property("--code-w", &format!("{width}px"));
                    lv.code_w = width;
                }
                set_code_scroll(&lv.el, sx);
            }
        }
    }

    /// A line of code scrolled sideways: scroll the rest of its block with it.
    fn on_code_scroll(&self, ev: &web_sys::Event) {
        let Some(lx) = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .filter(|e| e.class_list().contains("lx"))
        else {
            return;
        };
        let root: &Node = self.root.unchecked_ref();
        let Some(idx) = dom::root_child(root, lx.unchecked_ref()).and_then(|l| self.line_index(&l))
        else {
            return;
        };
        let Some((open, end)) = self.model(move |st| {
            let n = st.doc().line_count();
            st.code_block(idx).map(|(o, c)| (o, c.unwrap_or(n)))
        }) else {
            return;
        };
        let x = lx.scroll_left();
        let mut view = self.view.borrow_mut();
        if let Some(lv) = view.lines.get_mut(open) {
            lv.sx = x;
        }
        for (i, lv) in view.lines.iter().enumerate().take(end).skip(open + 1) {
            if i != idx {
                set_code_scroll(&lv.el, x);
            }
        }
    }

    /// Put every line element back as the editor's only children, in order
    /// (after the browser added or removed nodes of its own).
    fn repair_structure(&self) {
        let arr = js_sys::Array::new();
        for lv in &self.view.borrow().lines {
            arr.push(&lv.el);
        }
        self.root.replace_children_with_node(&arr);
        self.take_own_records();
    }

    fn refresh_now(&self) {
        self.settle();
        let dynamic: Vec<usize> = {
            let view = self.view.borrow();
            (0..view.lines.len())
                .filter(|&i| view.lines[i].dynamic)
                .collect()
        };
        if dynamic.is_empty() {
            return;
        }
        let (jobs, sel, sel_lines) = self.model(move |st| {
            let jobs = dynamic
                .into_iter()
                .filter(|&i| i < st.doc().line_count())
                .map(|i| LineJob {
                    idx: i,
                    text: st.doc().line(i).to_string(),
                    block: st.block(i).clone(),
                    html: st.html_source(i),
                })
                .collect::<Vec<_>>();
            let s = st.selection();
            (
                jobs,
                s,
                st.doc().line_of(s.from())..st.doc().line_of(s.to()) + 1,
            )
        });
        let focused = self.has_focus();
        let rendered = self.render_jobs(jobs);
        let touches_caret = focused
            && rendered.iter().any(|r| {
                sel_lines.contains(&r.idx)
                    && self.view.borrow().lines.get(r.idx).map(|l| l.hash) != Some(r.hash)
            });
        let active = self.view.borrow().active_lines.clone();
        self.patch(&[], rendered, &active);
        if touches_caret {
            // Not the user's doing (an image loaded, links changed): don't scroll.
            self.place_dom_selection(sel, false);
        }
        self.sync_active();
        self.request_frame(false, true);
    }

    // ------------------------------------------------------------ transactions

    /// Apply a transaction: state, then the host, then the DOM.
    fn dispatch(&self, tr: Transaction) {
        self.dispatch_with(tr, vec![], false);
    }

    /// `force`: lines (of the old document) the browser changed natively,
    /// rendered again whatever the transaction does to them. `structural`:
    /// the browser added or removed nodes of the editor element itself.
    fn dispatch_with(&self, tr: Transaction, force: Vec<usize>, structural: bool) {
        struct Out {
            base: Option<String>,
            applied: Applied,
            jobs: Vec<LineJob>,
            active: Vec<Range<usize>>,
            caret_redrawn: bool,
        }
        // What the browser changed must be rendered again even where the text
        // is the same: the DOM there is the browser's, not ours [W-L2].
        {
            let mut view = self.view.borrow_mut();
            for &l in &force {
                if let Some(lv) = view.lines.get_mut(l) {
                    lv.hash = 0;
                }
            }
        }
        let out = self.model(move |st| {
            if tr.is_local() && st.readonly && !tr.changes.is_empty() {
                return None;
            }
            let base = (tr.report == Report::Changes && !tr.changes.is_empty())
                .then(|| st.text().to_string());
            // Where the forced lines start, in the new text.
            let forced: Vec<usize> = force
                .into_iter()
                .filter(|&l| l < st.doc().line_count())
                .map(|l| tr.changes.map_pos(st.doc().line_start(l), Assoc::Before))
                .collect();
            let applied = st.apply(tr);
            let doc = st.doc();
            let mut redraw = applied.redraw.clone();
            redraw.extend(forced.into_iter().map(|at| {
                let l = doc.line_of(at);
                l..l + 1
            }));
            let redraw = crate::decorations::merge_ranges(redraw);
            let s = applied.selection;
            let sel_lines = doc.line_of(s.from())..doc.line_of(s.to()) + 1;
            let caret_redrawn = redraw
                .iter()
                .any(|r| r.start < sel_lines.end && sel_lines.start < r.end);
            let jobs = redraw
                .iter()
                .flat_map(|r| r.clone())
                .filter(|&i| i < doc.line_count())
                .map(|i| LineJob {
                    idx: i,
                    text: doc.line(i).to_string(),
                    block: st.block(i).clone(),
                    html: st.html_source(i),
                })
                .collect();
            Some(Out {
                base,
                applied,
                jobs,
                active: active_lines(st),
                caret_redrawn,
            })
        });
        let Some(Out {
            base,
            applied,
            jobs,
            active,
            caret_redrawn,
        }) = out
        else {
            return;
        };
        // The host.
        let accepted = match (&applied.report, base) {
            (Report::Changes, Some(base)) => {
                if applied.boundary.before {
                    self.host.boundary();
                }
                let ok = self.host.edit(&base, &applied.changes.to_edits());
                if applied.boundary.after {
                    self.host.boundary();
                }
                ok
            }
            (Report::Rebased { base, changes }, _) => {
                changes.is_empty() || self.host.edit(base, &changes.to_edits())
            }
            _ => true,
        };
        // The DOM.
        let focused = self.has_focus();
        let active = if !focused {
            vec![]
        } else if self.mouse_down.get() {
            self.view.borrow().active_lines.clone()
        } else {
            active
        };
        let rendered = self.render_jobs(jobs);
        self.patch(&applied.splices, rendered, &active);
        if structural {
            self.repair_structure();
        }
        if !accepted {
            self.resync(applied.selection);
            return;
        }
        let remote = applied.origin == Origin::Remote;
        // A remote edit leaves the DOM selection alone unless the caret's line
        // was rendered again (and never scrolls: the page must not move under
        // the reader); everything else shows the new selection.
        if focused && (!remote || caret_redrawn) {
            self.place_dom_selection(applied.selection, applied.scroll);
        }
        self.sync_active();
        // (Also when the last mark just went: its highlight is still drawn.)
        if !self.model(|st| st.cursors().is_empty() && st.marks().is_empty())
            || self.marks.child_element_count() > 0
        {
            self.request_frame(false, true);
        }
        if !remote && applied.origin != Origin::History {
            let (a, h) = applied.selection.pair();
            self.host.selection_changed(a, h);
        }
    }

    /// Replace our text with the host's, after it refused local edits.
    /// `caret` (in our current text) is mapped through the change.
    fn resync(&self, caret: Selection) {
        let Some(t) = self.host.text() else { return };
        self.ime.borrow_mut().pending = None;
        let tr = self.model(move |st| {
            let c = ChangeSet::diff(st.text(), &t);
            let sel = caret.map(&c, Assoc::After);
            Transaction::new(c, Origin::Remote).with_selection(sel)
        });
        let sel = tr.selection.unwrap_or_default();
        self.dispatch(tr);
        if self.has_focus() {
            self.place_dom_selection(sel, true);
        }
        self.host.selection_changed(sel.anchor, sel.head);
    }

    fn set_text_now(&self, new_text: String, caret_to_change: bool) {
        if self.ime.borrow().phase != Phase::Idle {
            // Remote text waits for the end of a composition.
            self.ime.borrow_mut().pending = Some(new_text);
            return;
        }
        // Native edits not handled yet are merged with the new text.
        if self.has_damage() {
            self.flush_native(Some(new_text));
            return;
        }
        let dom_sel = if self.has_focus() {
            self.read_selection()
        } else {
            None
        };
        let tr = self.model(move |st| {
            if let Some((a, h)) = dom_sel {
                st.set_selection(Selection::new(a, h));
            }
            let c = ChangeSet::diff(st.text(), &new_text);
            if c.is_empty() {
                return None;
            }
            if caret_to_change {
                let end = c
                    .replacements()
                    .last()
                    .map(|r| c.map_pos(r.to, Assoc::After))
                    .unwrap_or(0);
                Some(
                    Transaction::new(c, Origin::History)
                        .with_selection(Selection::cursor(end))
                        .scrolling(),
                )
            } else {
                Some(Transaction::new(c, Origin::Remote))
            }
        });
        if let Some(tr) = tr {
            self.dispatch(tr);
        }
    }

    /// Before acting on the user's behalf: finish an ending composition and
    /// take in native edits.
    fn settle(&self) {
        let phase = self.ime.borrow().phase;
        if phase != Phase::Idle {
            self.finish_composition();
        } else if self.has_damage() {
            self.flush_native(None);
        }
    }

    fn command_now(&self, cmd: Command) {
        if self.model(|st| st.readonly) {
            return;
        }
        self.settle();
        match cmd {
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            c => {
                if let Some(tr) = self.model(move |st| commands::command(st, c)) {
                    self.dispatch(tr);
                }
            }
        }
        let _ = self.root.focus();
    }

    fn undo(&self) {
        if let Some(t) = self.host.undo() {
            self.set_text_now(t, true);
        }
    }

    fn redo(&self) {
        if let Some(t) = self.host.redo() {
            self.set_text_now(t, true);
        }
    }

    // ------------------------------------------------------------ native edits

    fn take_own_records(&self) {
        if let Some((o, _)) = self.observer.borrow().as_ref() {
            let _ = o.take_records();
        }
    }

    fn has_damage(&self) -> bool {
        if !self.records.borrow().is_empty() {
            return true;
        }
        let more = match self.observer.borrow().as_ref() {
            Some((o, _)) => o.take_records(),
            None => return false,
        };
        let mut recs = self.records.borrow_mut();
        for r in more.iter() {
            recs.push(r.unchecked_into());
        }
        !recs.is_empty()
    }

    /// Index of a line element (a child of the editor) — `None` for anything
    /// the browser put there itself.
    fn line_index(&self, node: &Node) -> Option<usize> {
        let view = self.view.borrow();
        let (mut lo, mut hi) = (0, view.lines.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            let m: &Node = view.lines[mid].el.unchecked_ref();
            if m.is_same_node(Some(node)) {
                return Some(mid);
            }
            let pos = m.compare_document_position(node);
            if pos & Node::DOCUMENT_POSITION_FOLLOWING != 0 {
                lo = mid + 1;
            } else if pos & Node::DOCUMENT_POSITION_PRECEDING != 0 {
                hi = mid;
            } else {
                return None;
            }
        }
        None
    }

    /// Take in what the browser changed in the DOM on its own, merged with
    /// `remote` (newer document text that waited for it), and render the
    /// touched lines again so the DOM is ours again.
    fn flush_native(&self, remote: Option<String>) {
        self.has_damage();
        let records = std::mem::take(&mut *self.records.borrow_mut());
        let root: &Node = self.root.unchecked_ref();
        let mut touched = BTreeSet::new();
        let mut structural = false;
        for r in &records {
            let t = r.target();
            match t {
                Some(t) if t.is_same_node(Some(root)) => structural = true,
                Some(t) => match dom::root_child(root, &t).and_then(|c| self.line_index(&c)) {
                    Some(i) => {
                        touched.insert(i);
                    }
                    None => structural = true,
                },
                None => {}
            }
        }
        if touched.is_empty() && !structural {
            if let Some(t) = remote {
                self.set_text_now(t, false);
            }
            return;
        }
        let touched: Vec<usize> = touched.into_iter().collect();
        // The DOM's text where it changed.
        let mut caret: Option<(usize, usize)> = None;
        let mut dom_lines: Vec<(usize, String)> = vec![];
        let mut full: Option<String> = None;
        if structural {
            full = Some(dom::full_text(&self.root));
        } else {
            let view = self.view.borrow();
            for &i in &touched {
                dom_lines.push((i, dom::line_text(&view.lines[i].el)));
            }
            drop(view);
            caret = self.dom_caret_line();
        }
        let tr = self.model(move |st| {
            let base = st.text().to_string();
            let doc = st.doc();
            // Browsers put no-break spaces into text they edit natively; make
            // inserted ones spaces, but leave the note's own alone.
            let plain = |s: &str| s.replace('\u{a0}', " ");
            let mut reps: Vec<(usize, usize, String)> = vec![];
            let mut new_lines: Vec<(usize, String)> = vec![];
            match full {
                Some(dom_text) => {
                    let c = ChangeSet::diff(&base, &dom_text);
                    for r in c.replacements() {
                        let ins = plain(r.insert);
                        if base.get(r.from..r.to) != Some(ins.as_str()) {
                            reps.push((r.from, r.to, ins));
                        }
                    }
                }
                None => {
                    for (i, dom_line) in dom_lines {
                        let (ls, line) = (doc.line_start(i), doc.line(i));
                        let c = ChangeSet::diff(line, &dom_line);
                        let mut new_line = String::with_capacity(dom_line.len());
                        let mut pos = 0;
                        for r in c.replacements() {
                            let ins = plain(r.insert);
                            new_line.push_str(&line[pos..r.from]);
                            new_line.push_str(&ins);
                            pos = r.to;
                            if line.get(r.from..r.to) != Some(ins.as_str()) {
                                reps.push((ls + r.from, ls + r.to, ins));
                            }
                        }
                        new_line.push_str(&line[pos..]);
                        new_lines.push((i, new_line));
                    }
                }
            }
            let native = ChangeSet::from_replacements(base.len(), &reps);
            // Where the caret is, in the text with the native edit applied.
            let caret_native = caret.map(|(k, u)| {
                let start = native.map_pos(doc.line_start(k), Assoc::Before);
                let line = new_lines
                    .iter()
                    .find(|(i, _)| *i == k)
                    .map(|(_, l)| l.as_str())
                    .unwrap_or_else(|| doc.line(k));
                start + utf16_to_byte(line, u)
            });
            let end_of_change = native
                .replacements()
                .last()
                .map(|r| native.map_pos(r.to, Assoc::After));
            let (mut tr, after) = merge_native(&base, native, remote);
            let caret = caret_native
                .or(end_of_change)
                .map(|p| after.map_pos(p, Assoc::After));
            tr.selection = caret.map(Selection::cursor);
            if tr.changes.is_empty() {
                tr.selection = tr.selection.or(Some(st.selection()));
            }
            tr
        });
        self.dispatch_with(tr, touched, structural);
    }

    /// The DOM caret as (line index, UTF-16 offset in the line element).
    fn dom_caret_line(&self) -> Option<(usize, usize)> {
        let sel = document().get_selection().ok()??;
        let node = sel.focus_node()?;
        if !self.root.contains(Some(&node)) {
            return None;
        }
        let root: &Node = self.root.unchecked_ref();
        let child = dom::root_child(root, &node)?;
        let i = self.line_index(&child)?;
        let el: &Element = child.unchecked_ref();
        Some((i, dom::prefix_utf16(el, &node, sel.focus_offset())?))
    }

    // ------------------------------------------------------------ composition

    fn composition_start(&self) {
        let ending = self.ime.borrow().phase == Phase::Ending;
        if !self.busy.get() {
            self.run_now(|ed| {
                if ending {
                    ed.finish_composition();
                } else if ed.has_damage() {
                    ed.flush_native(None);
                }
            });
        }
        self.ime.borrow_mut().phase = Phase::Active;
    }

    fn composition_end(&self) {
        self.ime.borrow_mut().phase = Phase::Ending;
        if self.ime_timer.get().is_none()
            && let Some(cb) = self.ime_cb.borrow().as_ref()
        {
            let id = web_sys::window()
                .and_then(|w| {
                    w.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        0,
                    )
                    .ok()
                })
                .unwrap_or(0);
            self.ime_timer.set(Some(id));
        }
    }

    /// The composition is over: take in what it typed, merged with the
    /// text that arrived meanwhile, and catch up on what waited for it.
    fn finish_composition(&self) {
        let (pending, refresh) = {
            let mut ime = self.ime.borrow_mut();
            if ime.phase == Phase::Idle {
                return;
            }
            ime.phase = Phase::Idle;
            (ime.pending.take(), std::mem::take(&mut ime.refresh))
        };
        if let Some(id) = self.ime_timer.take()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(id);
        }
        self.flush_native(pending);
        if refresh {
            self.refresh_now();
        }
    }

    // ------------------------------------------------------------ selection

    fn dom_to_offset(&self, node: &Node, offset: u32) -> Option<usize> {
        let root: &Node = self.root.unchecked_ref();
        let (line, child) = if node.is_same_node(Some(root)) {
            match self.root.child_nodes().item(offset) {
                Some(c) => (self.line_index(&c)?, None),
                None => return Some(self.model(|st| st.text().len())),
            }
        } else {
            let c = dom::root_child(root, node)?;
            (self.line_index(&c)?, Some(c))
        };
        let (start, text) =
            self.model(move |st| (st.doc().line_start(line), st.doc().line(line).to_string()));
        Some(match child {
            None => start,
            Some(c) => start + dom::offset_in_line(c.unchecked_ref(), &text, node, offset),
        })
    }

    fn read_selection(&self) -> Option<(usize, usize)> {
        let sel = document().get_selection().ok()??;
        let an = sel.anchor_node()?;
        let fo = sel.focus_node()?;
        if !self.root.contains(Some(&an)) || !self.root.contains(Some(&fo)) {
            return None;
        }
        let a = self.dom_to_offset(&an, sel.anchor_offset())?;
        let h = self.dom_to_offset(&fo, sel.focus_offset())?;
        Some((a, h))
    }

    /// Show `sel` as the DOM selection.
    fn place_dom_selection(&self, sel: Selection, scroll: bool) {
        let ends = self.model(move |st| {
            let d = st.doc();
            let end = |p: usize| {
                let l = d.line_of(p);
                (l, d.line(l).to_string(), p - d.line_start(l))
            };
            (end(sel.anchor), end(sel.head))
        });
        let (an, hn) = {
            let view = self.view.borrow();
            let pos = |(l, text, off): &(usize, String, usize)| {
                view.lines
                    .get(*l)
                    .map(|lv| dom::offset_to_dom(&lv.el, text, *off))
            };
            (pos(&ends.0), pos(&ends.1))
        };
        if let (Some((an, ao)), Some((hn, ho))) = (an, hn)
            && let Ok(Some(s)) = document().get_selection()
        {
            let _ = s.set_base_and_extent(&an, ao, &hn, ho);
        }
        if scroll {
            self.request_frame(true, false);
        }
    }

    /// Mark the lines of the selection `active` (their syntax shows) while
    /// the editor has the focus, and the fences of a code block it is in.
    /// Not while the mouse button is down ([`Editor::mouse_released`]).
    fn sync_active(&self) {
        if self.mouse_down.get() {
            return;
        }
        // Markers shown or hidden on these lines move the highlights.
        if !self.model(|st| st.marks().is_empty()) {
            self.request_frame(false, true);
        }
        let lines = self.model(|st| active_lines(st));
        let want = if self.has_focus() { lines } else { vec![] };
        let mut view = self.view.borrow_mut();
        let n = view.lines.len();
        let want: Vec<Range<usize>> = want
            .into_iter()
            .map(|r| r.start.min(n)..r.end.min(n))
            .collect();
        let new: Vec<HtmlElement> = want
            .iter()
            .flat_map(|r| view.lines[r.clone()].iter().map(|l| l.el.clone()))
            .collect();
        let small = view.active.len() * new.len() <= 4096;
        for el in &view.active {
            if !(small && new.iter().any(|n| n.is_same_node(Some(el)))) {
                let _ = el.class_list().remove_1("active");
            }
        }
        for el in &new {
            let _ = el.class_list().add_1("active");
        }
        view.active = new;
        view.active_lines = want;
    }

    /// The mouse button is up again: the lines of the selection show their
    /// Markdown now.
    fn mouse_released(&self) {
        if self.mouse_down.replace(false) && !self.run_now(|ed| ed.sync_active()) {
            self.defer(DEFER_ACTIVE);
        }
    }

    fn selection_from_dom(&self) {
        if self.ime.borrow().phase != Phase::Idle {
            return;
        }
        let Some((a, h)) = self.read_selection() else {
            self.sync_active();
            return;
        };
        let changed = self.model(move |st| {
            let c = st.selection().pair() != (a, h);
            st.set_selection(Selection::new(a, h));
            c
        });
        self.sync_active();
        if changed {
            self.follow_caret_sideways();
            self.host.selection_changed(a, h);
        }
    }

    // ------------------------------------------------------------ measuring

    fn request_frame(&self, scroll: bool, cursors: bool) {
        if scroll {
            self.frame_scroll.set(true);
        }
        if cursors {
            self.frame_cursors.set(true);
        }
        if self.frame.get().is_some() {
            return;
        }
        if let Some(cb) = self.frame_cb.borrow().as_ref()
            && let Some(w) = web_sys::window()
            && let Ok(id) = w.request_animation_frame(cb.as_ref().unchecked_ref())
        {
            self.frame.set(Some(id));
        }
    }

    fn on_frame(&self) {
        self.frame.set(None);
        if self.frame_scroll.replace(false) {
            self.scroll_caret_into_view();
        }
        if self.frame_cursors.replace(false) {
            self.draw_cursors();
            self.draw_marks();
        }
    }

    fn scroll_caret_into_view(&self) {
        let line = self.model(|st| st.doc().line_of(st.selection().head));
        let Some(el) = self.view.borrow().lines.get(line).map(|l| l.el.clone()) else {
            return;
        };
        let r = el.get_bounding_client_rect();
        let vh = web_sys::window()
            .and_then(|w| w.inner_height().ok())
            .and_then(|v| v.as_f64())
            .unwrap_or(800.0);
        if r.bottom() > vh - 24.0 || r.top() < 56.0 {
            // Up or down only: a table's row is as wide as the table, and
            // bringing all of it into view would undo the text's sideways
            // scroll, which follows the caret below.
            let x = self.wrap.scroll_left();
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
            el.scroll_into_view_with_scroll_into_view_options(&opts);
            self.wrap.set_scroll_left(x);
        }
        self.follow_caret_sideways();
    }

    /// Scroll sideways so that the caret shows: a long line of code that
    /// doesn't wrap (its block follows, [`Editor::on_code_scroll`]), and the
    /// editor's text when a table makes it wider than the editor. The
    /// browser only does that for caret moves of its own.
    fn follow_caret_sideways(&self) {
        let Some(sel) = document().get_selection().ok().flatten() else {
            return;
        };
        let Some(node) = sel.focus_node() else {
            return;
        };
        if !self.root.contains(Some(&node)) {
            return;
        }
        let Ok(range) = document().create_range() else {
            return;
        };
        if range.set_start(&node, sel.focus_offset()).is_err() {
            return;
        }
        range.collapse_with_to_start(true);
        let el = match node.dyn_ref::<Element>() {
            Some(e) => Some(e.clone()),
            None => node.parent_element(),
        };
        let lx = el.and_then(|e| e.closest(".lx").ok().flatten());
        // The line first: scrolling the text then measures where it put the caret.
        for scroller in lx.iter().chain([self.wrap.unchecked_ref::<Element>()]) {
            if scroller.scroll_width() <= scroller.client_width() {
                continue;
            }
            let caret = range.get_bounding_client_rect();
            if caret.height() == 0.0 {
                return;
            }
            let r = scroller.get_bounding_client_rect();
            let margin = 32.0;
            let x = scroller.scroll_left() as f64;
            let to = if caret.right() > r.right() - margin {
                x + caret.right() - (r.right() - margin)
            } else if caret.left() < r.left() + margin {
                (x - (r.left() + margin - caret.left())).max(0.0)
            } else {
                continue;
            };
            scroller.set_scroll_left(to.round() as i32);
        }
    }

    /// The client rectangles of the text from `start` to `end` (byte
    /// offsets), line by line.
    fn text_rects(&self, start: usize, end: usize) -> Vec<web_sys::DomRect> {
        let lines = self.model(move |st| {
            let d = st.doc();
            let (first, last) = (d.line_of(start), d.line_of(end));
            (first..=last)
                .map(|l| {
                    let ls = d.line_start(l);
                    let text = d.line(l).to_string();
                    let from = start.saturating_sub(ls).min(text.len());
                    let to = end.saturating_sub(ls).min(text.len());
                    (l, text, from, to)
                })
                .filter(|(_, _, from, to)| from < to)
                .collect::<Vec<_>>()
        });
        let doc = document();
        let mut out = Vec::new();
        for (line, text, from, to) in lines {
            let Some(lv) = self.view.borrow().lines.get(line).map(|l| l.el.clone()) else {
                continue;
            };
            let (sn, so) = dom::offset_to_dom(&lv, &text, from);
            let (en, eo) = dom::offset_to_dom(&lv, &text, to);
            let Ok(range) = doc.create_range() else {
                continue;
            };
            if range.set_start(&sn, so).is_err() || range.set_end(&en, eo).is_err() {
                continue;
            }
            if let Some(rects) = range.get_client_rects() {
                for i in 0..rects.length() {
                    if let Some(r) = rects.get(i)
                        && r.width() > 0.0
                    {
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    fn draw_marks(&self) {
        let marks = self.model(|st| st.marks().to_vec());
        self.marks.set_inner_html("");
        if marks.is_empty() {
            return;
        }
        let base = self.marks.get_bounding_client_rect();
        let doc = document();
        for m in marks {
            for r in self.text_rects(m.start, m.end) {
                let el: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
                el.set_class_name(&format!("hl {}", m.class));
                let _ = el.set_attribute("data-mark", &m.id);
                let style = el.style();
                let _ = style.set_property("left", &format!("{}px", r.left() - base.left()));
                let _ = style.set_property("top", &format!("{}px", r.top() - base.top()));
                let _ = style.set_property("width", &format!("{}px", r.width()));
                let _ = style.set_property("height", &format!("{}px", r.height()));
                let _ = self.marks.append_child(&el);
            }
        }
    }

    fn draw_cursors(&self) {
        let cursors = self.model(|st| {
            let d = st.doc();
            st.cursors()
                .iter()
                .map(|c| {
                    let p = d.floor(c.head);
                    let l = d.line_of(p);
                    (
                        c.name.clone(),
                        c.color.clone(),
                        l,
                        d.line(l).to_string(),
                        p - d.line_start(l),
                    )
                })
                .collect::<Vec<_>>()
        });
        self.overlay.set_inner_html("");
        if cursors.is_empty() {
            return;
        }
        let base = self.overlay.get_bounding_client_rect();
        let doc = document();
        let mut drawn = Vec::with_capacity(cursors.len());
        for (name, color, line, text, off) in cursors {
            let Some(lv) = self.view.borrow().lines.get(line).map(|l| l.el.clone()) else {
                continue;
            };
            let (node, o) = dom::offset_to_dom(&lv, &text, off);
            let Ok(range) = doc.create_range() else {
                continue;
            };
            if range.set_start(&node, o).is_err() || range.set_end(&node, o).is_err() {
                continue;
            }
            let mut rect = range.get_bounding_client_rect();
            if rect.height() == 0.0 {
                rect = match node.dyn_ref::<Element>() {
                    Some(el) => el.get_bounding_client_rect(),
                    None => match node.parent_element() {
                        Some(p) => p.get_bounding_client_rect(),
                        None => continue,
                    },
                };
            }
            let el: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
            let top = rect.top() - base.top();
            // The name goes above the cursor, or under it where there is no
            // room above (the text's box clips: it scrolls sideways).
            el.set_class_name(if top < NAME_HEIGHT { "rc below" } else { "rc" });
            let left = rect.left() - base.left();
            let style = el.style();
            let _ = style.set_property("left", &format!("{left}px"));
            let _ = style.set_property("top", &format!("{top}px"));
            let _ = style.set_property("height", &format!("{}px", rect.height().max(16.0)));
            let _ = style.set_property("--c", &color);
            let label: HtmlElement = doc.create_element("span").unwrap().unchecked_into();
            label.set_text_content(Some(&name));
            let _ = el.append_child(&label);
            let _ = self.overlay.append_child(&el);
            drawn.push((el, label, left));
        }
        // A name that would stick out past the text goes left of its cursor
        // (sticking out, it would make the text scroll sideways). All are
        // measured first, so that's one layout.
        let end = self.root.scroll_width() as f64;
        let past: Vec<bool> = drawn
            .iter()
            .map(|(_, label, left)| left + label.offset_width() as f64 > end)
            .collect();
        for ((el, ..), past) in drawn.iter().zip(past) {
            if past {
                let _ = el.class_list().add_1("end");
            }
        }
    }

    // ------------------------------------------------------------ events

    fn listen(
        this: &Rc<Editor>,
        target: &EventTarget,
        name: &'static str,
        f: impl Fn(&Editor, web_sys::Event) + 'static,
    ) {
        Editor::listen_with(this, target, name, false, f);
    }

    /// [`Editor::listen`], in the capture phase if `capture` (for events
    /// that don't bubble, like `scroll`).
    fn listen_with(
        this: &Rc<Editor>,
        target: &EventTarget,
        name: &'static str,
        capture: bool,
        f: impl Fn(&Editor, web_sys::Event) + 'static,
    ) {
        let weak: Weak<Editor> = Rc::downgrade(this);
        let cb = Closure::wrap(Box::new(move |e: web_sys::Event| {
            if let Some(ed) = weak.upgrade() {
                f(&ed, e);
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        let _ = target.add_event_listener_with_callback_and_bool(
            name,
            cb.as_ref().unchecked_ref(),
            capture,
        );
        this.listeners
            .borrow_mut()
            .push((target.clone(), name, capture, cb));
    }

    fn attach(this: &Rc<Editor>) {
        let root: EventTarget = this.root.clone().unchecked_into();
        Editor::listen(this, &root, "beforeinput", |ed, e| {
            ed.on_beforeinput(e.unchecked_into())
        });
        Editor::listen(this, &root, "input", |ed, e| {
            let ie: InputEvent = e.unchecked_into();
            if !ie.is_composing() && ed.ime.borrow().phase == Phase::Idle {
                ed.run_now(|ed| ed.flush_native(None));
            }
        });
        Editor::listen(this, &root, "compositionstart", |ed, _| {
            ed.composition_start()
        });
        Editor::listen(this, &root, "compositionend", |ed, _| ed.composition_end());
        Editor::listen(this, &root, "keydown", |ed, e| {
            let ev: KeyboardEvent = e.unchecked_into();
            // 229: a key the IME handles (Safari sends the key that ends a
            // composition after `compositionend`, not marked as composing).
            if ev.is_composing() || ev.key_code() == 229 {
                return;
            }
            ed.run_now(|ed| ed.on_keydown(&ev));
        });
        Editor::listen(this, &root, "copy", |ed, e| {
            ed.run_now(|ed| ed.on_copy(e.unchecked_ref(), false));
        });
        Editor::listen(this, &root, "cut", |ed, e| {
            ed.run_now(|ed| ed.on_copy(e.unchecked_ref(), true));
        });
        Editor::listen(this, &root, "paste", |ed, e| {
            ed.run_now(|ed| ed.on_paste(e.unchecked_ref()));
        });
        Editor::listen(this, &root, "dragstart", |ed, _| {
            ed.run_now(|ed| {
                let sel = ed.read_selection().map(|(a, h)| (a.min(h), a.max(h)));
                ed.view.borrow_mut().drag = sel.filter(|(a, b)| a != b);
            });
        });
        Editor::listen(this, &root, "dragend", |ed, _| {
            ed.run_now(|ed| ed.view.borrow_mut().drag = None);
        });
        Editor::listen(this, &root, "drop", |ed, e| {
            ed.run_now(|ed| ed.on_drop(e.unchecked_ref()));
        });
        Editor::listen(this, &root, "dragover", |_ed, e| e.prevent_default());
        Editor::listen_with(this, &root, "scroll", true, |ed, e| {
            ed.run_now(|ed| ed.on_code_scroll(&e));
        });
        Editor::listen(this, &root, "mousedown", |ed, e| {
            ed.run_now(|ed| ed.on_mousedown(e.unchecked_ref()));
        });
        // A click in an HTML block's preview was handled on mousedown; its
        // own default (a `summary` opening its `details`) would only change
        // a preview that is about to hide.
        Editor::listen(this, &root, "click", |_ed, e| {
            let in_preview = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest(".hw").ok().flatten())
                .is_some();
            if in_preview {
                e.prevent_default();
            }
        });
        for name in ["focus", "blur"] {
            Editor::listen(this, &root, name, |ed, _| {
                if !ed.run_now(|ed| ed.sync_active()) {
                    ed.defer(DEFER_ACTIVE);
                }
            });
        }
        let doc: EventTarget = document().unchecked_into();
        for name in ["pointerup", "pointercancel"] {
            Editor::listen(this, &doc, name, |ed, _| ed.mouse_released());
        }
        Editor::listen(this, &doc, "selectionchange", |ed, _| {
            if ed.ime.borrow().phase != Phase::Idle {
                return;
            }
            if !ed.run_now(|ed| ed.selection_from_dom()) {
                ed.defer(DEFER_SELECTION);
            }
        });
        if let Some(win) = web_sys::window() {
            let win: EventTarget = win.unchecked_into();
            Editor::listen(this, &win, "resize", |ed, _| {
                ed.op(|ed| ed.request_frame(false, true))
            });
            // A button released outside the window: no pointerup here.
            Editor::listen(this, &win, "blur", |ed, _| ed.mouse_released());
        }
        // Native DOM changes.
        let weak = Rc::downgrade(this);
        let cb = Closure::wrap(Box::new(move |records: js_sys::Array| {
            let Some(ed) = weak.upgrade() else { return };
            {
                let mut recs = ed.records.borrow_mut();
                for r in records.iter() {
                    recs.push(r.unchecked_into());
                }
            }
            if ed.ime.borrow().phase == Phase::Idle {
                ed.run_now(|ed| ed.flush_native(None));
            }
        }) as Box<dyn FnMut(js_sys::Array)>);
        if let Ok(obs) = MutationObserver::new(cb.as_ref().unchecked_ref()) {
            let opts = web_sys::MutationObserverInit::new();
            opts.set_child_list(true);
            opts.set_subtree(true);
            opts.set_character_data(true);
            let _ = obs.observe_with_options(&this.root, &opts);
            *this.observer.borrow_mut() = Some((obs, cb));
        }
        let weak = Rc::downgrade(this);
        let cb = Closure::wrap(Box::new(move |_: js_sys::Array| {
            if let Some(ed) = weak.upgrade() {
                ed.op(|ed| ed.request_frame(false, true));
            }
        }) as Box<dyn FnMut(js_sys::Array)>);
        if let Ok(obs) = web_sys::ResizeObserver::new(cb.as_ref().unchecked_ref()) {
            obs.observe(&this.root);
            *this.resize.borrow_mut() = Some((obs, cb));
        }
        let weak = Rc::downgrade(this);
        *this.frame_cb.borrow_mut() = Some(Closure::wrap(Box::new(move || {
            if let Some(ed) = weak.upgrade() {
                ed.op(|ed| ed.on_frame());
            }
        }) as Box<dyn FnMut()>));
        let weak = Rc::downgrade(this);
        *this.ime_cb.borrow_mut() = Some(Closure::wrap(Box::new(move || {
            if let Some(ed) = weak.upgrade() {
                ed.ime_timer.set(None);
                ed.op(|ed| ed.finish_composition());
            }
        }) as Box<dyn FnMut()>));
    }

    fn on_beforeinput(&self, ev: InputEvent) {
        // Read without an operation: the default must be prevented even in
        // the (impossible) case that one is running.
        let (ro, vim_normal) = self
            .state
            .try_borrow()
            .map(|s| (s.readonly, s.vim.as_ref().is_some_and(|v| !v.insert)))
            .unwrap_or((true, false));
        if ro {
            ev.prevent_default();
            return;
        }
        let it = ev.input_type();
        let phase = self.ime.borrow().phase;
        // Compositions are the browser's (and the IME's): their text is read
        // back from the DOM once they are over.
        if ev.is_composing() || is_composition_input(&it) || phase == Phase::Active {
            return;
        }
        // Right after `compositionend`, input we don't handle ourselves still
        // belongs to the composition (Safari's `insertFromComposition`, which
        // browsers that don't know it report without a type).
        if phase == Phase::Ending && !handled_input(&it) {
            return;
        }
        if vim_normal {
            ev.prevent_default();
            return;
        }
        self.run_now(|ed| ed.before_input(&ev, &it));
    }

    fn target_range(&self, ev: &InputEvent) -> Option<(usize, usize)> {
        let ranges = ev.get_target_ranges();
        let r = ranges.get(0);
        if r.is_undefined() || r.is_null() {
            return None;
        }
        let r: web_sys::StaticRange = r.dyn_into().ok()?;
        let a = self.dom_to_offset(&r.start_container(), r.start_offset())?;
        let b = self.dom_to_offset(&r.end_container(), r.end_offset())?;
        Some((a.min(b), a.max(b)))
    }

    fn before_input(&self, ev: &InputEvent, it: &str) {
        self.settle();
        let dom_sel = self.read_selection().map(|(a, h)| (a.min(h), a.max(h)));
        let (a, b) = self.model(move |st| {
            if let Some((a, b)) = dom_sel {
                st.set_selection(Selection::new(a, b));
            }
            let s = st.selection();
            (s.from(), s.to())
        });
        let data = ev.data().unwrap_or_default();
        let tr = match it {
            "insertText" => {
                ev.prevent_default();
                self.model(move |st| commands::type_text(st, a, b, &data))
            }
            "insertReplacementText" => {
                ev.prevent_default();
                let data = ev
                    .data()
                    .or_else(|| {
                        ev.data_transfer()
                            .and_then(|d| d.get_data("text/plain").ok())
                    })
                    .unwrap_or_default();
                let (x, y) = self.target_range(ev).unwrap_or((a, b));
                self.model(move |st| commands::replace(st, x, y, &data, None))
            }
            "insertParagraph" | "insertLineBreak" => {
                ev.prevent_default();
                self.model(|st| commands::newline(st))
            }
            "deleteContentBackward" => {
                ev.prevent_default();
                let t = self.target_range(ev);
                self.model(move |st| commands::delete_backward(st, t))
            }
            "deleteContentForward" => {
                ev.prevent_default();
                let t = self.target_range(ev);
                self.model(move |st| commands::delete_forward(st, t))
            }
            t if t.starts_with("delete") => {
                ev.prevent_default();
                let t = self.target_range(ev);
                self.model(move |st| commands::delete_range(st, t))
            }
            "insertFromPaste"
            | "insertFromDrop"
            | "insertFromYank"
            | "insertFromPasteAsQuotation" => {
                ev.prevent_default();
                let t = ev
                    .data_transfer()
                    .and_then(|d| d.get_data("text/plain").ok());
                t.and_then(|t| {
                    self.model(move |st| {
                        commands::replace(st, a, b, &t.replace("\r\n", "\n"), None)
                    })
                })
            }
            "historyUndo" => {
                ev.prevent_default();
                self.undo();
                None
            }
            "historyRedo" => {
                ev.prevent_default();
                self.redo();
                None
            }
            "formatBold" | "formatItalic" | "formatUnderline" | "formatStrikeThrough" => {
                ev.prevent_default();
                let c = match it {
                    "formatBold" => Command::Bold,
                    "formatItalic" => Command::Italic,
                    _ => Command::Strike,
                };
                self.command_now(c);
                None
            }
            // Rich-text input would put markup into the DOM that isn't the source.
            t if t.starts_with("format")
                || (t.starts_with("insert") && t.ends_with("List"))
                || t == "insertHorizontalRule"
                || t == "insertLink" =>
            {
                ev.prevent_default();
                None
            }
            // Anything else the browser does itself; it is read back from the DOM.
            _ => None,
        };
        if let Some(tr) = tr {
            self.dispatch(tr);
        }
    }

    fn on_keydown(&self, ev: &KeyboardEvent) {
        self.settle();
        if self.model(|st| st.vim.is_some()) && self.vim_key(ev) {
            ev.prevent_default();
            return;
        }
        let readonly = self.model(|st| st.readonly);
        let m = ev.ctrl_key() || ev.meta_key();
        let key = ev.key();
        let shift = ev.shift_key();
        let heading = heading_level(ev);
        let cmd = match (m, shift, key.as_str()) {
            _ if heading.is_some() => heading.map(Command::Heading),
            (true, false, "z") | (true, false, "Z") => Some(Command::Undo),
            (true, true, "z") | (true, true, "Z") | (true, false, "y") => Some(Command::Redo),
            (true, false, "b") => Some(Command::Bold),
            (true, false, "i") => Some(Command::Italic),
            (true, false, "k") => Some(Command::Link),
            (true, false, "`") => Some(Command::Code),
            (true, true, _) if key == "~" || ev.code() == "Backquote" => Some(Command::CodeBlock),
            (true, true, "X") | (true, true, "x") => Some(Command::Strike),
            (true, true, "H") | (true, true, "h") => Some(Command::Highlight),
            (true, true, "*") | (true, true, "8") => Some(Command::Bullet),
            (true, true, "&") | (true, true, "7") => Some(Command::Numbered),
            (true, true, "(") | (true, true, "9") => Some(Command::Task),
            (true, true, ">") | (true, true, ".") => Some(Command::Quote),
            (false, _, "Tab") if !readonly => {
                ev.prevent_default();
                let dom_sel = self.read_selection();
                if let Some(tr) = self.model(move |st| {
                    if let Some((a, h)) = dom_sel {
                        st.set_selection(Selection::new(a, h));
                    }
                    commands::tab(st, shift)
                }) {
                    self.dispatch(tr);
                }
                return;
            }
            (true, false, "Enter") => {
                ev.prevent_default();
                if let Some(tr) = self.model(|st| {
                    let line = st.doc().line_of(st.selection().from());
                    commands::toggle_task(st, line)
                }) {
                    self.dispatch(tr);
                }
                return;
            }
            _ => None,
        };
        if let Some(c) = cmd {
            if readonly {
                return;
            }
            ev.prevent_default();
            self.command_now(c);
        }
    }

    fn vim_key(&self, ev: &KeyboardEvent) -> bool {
        let key = ev.key();
        let mods = vim::Mods {
            ctrl: ev.ctrl_key(),
            meta: ev.meta_key(),
            alt: ev.alt_key(),
        };
        let dom_sel = self.read_selection();
        let action = self.model(move |st| {
            let sel = dom_sel.unwrap_or(st.selection().pair());
            st.vim_key(&key, mods, sel)
        });
        let Some(action) = action else { return false };
        self.apply_mode_classes();
        match action {
            vim::Action::Consumed => {}
            vim::Action::Move(a, h) => {
                let sel = self.model(move |st| {
                    st.set_selection(Selection::new(a, h));
                    st.selection()
                });
                self.place_dom_selection(sel, true);
                self.sync_active();
                self.host.selection_changed(sel.anchor, sel.head);
            }
            vim::Action::Edit {
                from,
                to,
                insert,
                caret,
            } => {
                if let Some(tr) = self.model(move |st| {
                    commands::replace(st, from, to, &insert, Some((caret, caret))).map(|mut t| {
                        t.origin = Origin::Command;
                        t.with_boundary(true, false)
                    })
                }) {
                    self.dispatch(tr);
                }
            }
            vim::Action::Undo => self.undo(),
            vim::Action::Redo => self.redo(),
            vim::Action::Yank(t) => {
                if let Some(w) = web_sys::window() {
                    let _ = w.navigator().clipboard().write_text(&t);
                }
            }
        }
        true
    }

    fn on_copy(&self, ev: &web_sys::ClipboardEvent, cut: bool) {
        self.settle();
        let Some((a, h)) = self.read_selection() else {
            return;
        };
        let (a, b) = (a.min(h), a.max(h));
        if a == b {
            return;
        }
        ev.prevent_default();
        let text = self.model(move |st| st.doc().slice(a, b).to_string());
        if let Some(dt) = ev.clipboard_data() {
            let _ = dt.set_data("text/plain", &text);
        }
        if cut
            && let Some(tr) = self.model(move |st| {
                commands::replace(st, a, b, "", None).map(|t| t.with_boundary(true, false))
            })
        {
            self.dispatch(tr);
        }
    }

    fn on_paste(&self, ev: &web_sys::ClipboardEvent) {
        ev.prevent_default();
        if self.model(|st| st.readonly) {
            return;
        }
        self.settle();
        let Some(dt) = ev.clipboard_data() else {
            return;
        };
        let dom_sel = self.read_selection();
        let (a, b) = self.model(move |st| {
            if let Some((a, h)) = dom_sel {
                st.set_selection(Selection::new(a.min(h), a.max(h)));
            }
            (st.selection().from(), st.selection().to())
        });
        if let Some(files) = dt.files()
            && files.length() > 0
        {
            let v: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
            self.host.insert_files(v, a);
            return;
        }
        if let Ok(t) = dt.get_data("text/plain")
            && let Some(tr) = self.model(move |st| commands::paste(st, a, b, &t))
        {
            self.dispatch(tr);
        }
    }

    /// Drops land where they are dropped [W-L7]: text dragged within the
    /// editor moves there, other text and files are inserted there.
    fn on_drop(&self, ev: &web_sys::DragEvent) {
        ev.prevent_default();
        let drag = self.view.borrow_mut().drag.take();
        if self.model(|st| st.readonly) {
            return;
        }
        self.settle();
        let Some(dt) = ev.data_transfer() else { return };
        let at = dom::caret_from_point(ev.client_x() as f64, ev.client_y() as f64)
            .and_then(|(n, o)| self.dom_to_offset(&n, o))
            .unwrap_or_else(|| self.model(|st| st.selection().from()));
        if let Some(files) = dt.files()
            && files.length() > 0
        {
            let v: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
            self.host.insert_files(v, at);
            return;
        }
        let Ok(text) = dt.get_data("text/plain") else {
            return;
        };
        let tr = self.model(move |st| {
            let text = text.replace("\r\n", "\n");
            let len = st.text().len();
            let at = st.doc().floor(at);
            match drag {
                // Dropped onto itself: nothing to do.
                Some((a, b)) if at >= a && at <= b => None,
                Some((a, b)) if st.doc().slice(a, b) == text => {
                    let cut = ChangeSet::replace(len, a, b, "");
                    let to = cut.map_pos(at, Assoc::Before);
                    let ins = ChangeSet::replace(len - (b - a), to, to, &text);
                    Some(
                        Transaction::new(cut.compose(&ins), Origin::Input)
                            .with_selection(Selection::new(to, to + text.len()))
                            .with_boundary(true, true),
                    )
                }
                _ => commands::paste(st, at, at, &text),
            }
        });
        if let Some(tr) = tr {
            let _ = self.root.focus();
            self.dispatch(tr);
        }
    }

    fn on_mousedown(&self, ev: &MouseEvent) {
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        let root: &Node = self.root.unchecked_ref();
        let line = dom::root_child(root, target.unchecked_ref());
        let Some(idx) = line.as_ref().and_then(|l| self.line_index(l)) else {
            return;
        };
        let line_active = line
            .as_ref()
            .and_then(|l| l.dyn_ref::<Element>())
            .is_some_and(|l| l.class_list().contains("active"));
        let readonly = self.model(|st| st.readonly);
        // The preview of an HTML block (which stands in for its source away
        // from the caret): a link opens, anywhere else puts the caret at the
        // start of the block, which shows its source.
        if let Some(preview) = target.closest(".hw").ok().flatten() {
            ev.prevent_default();
            if ev.button() != 0 {
                return;
            }
            let href = target
                .closest("[data-href]")
                .ok()
                .flatten()
                .filter(|a| preview.contains(Some(a)))
                .and_then(|a| a.get_attribute("data-href"));
            if let Some(href) = href {
                let href = href.trim();
                let target = if notekpr_core::markdown::is_internal_target(href) {
                    Target::Wiki(notekpr_core::markdown::percent_decode(href))
                } else {
                    Target::Url(href.to_string())
                };
                self.host.open_link(&target, ev.ctrl_key() || ev.meta_key());
            } else if let Some(tr) = self.model(move |st| {
                let at = st.doc().line_start(st.html_block(idx)?.start);
                Some(Transaction::select(st.doc().len(), Selection::new(at, at)))
            }) {
                let _ = self.root.focus();
                self.dispatch(tr);
            }
            return;
        }
        if ev.button() == 0 {
            self.mouse_down.set(true);
        }
        if let Some(button) = target.closest(".code-copy").ok().flatten() {
            ev.prevent_default();
            self.settle();
            let text = self.model(move |st| st.code_text(idx));
            if let Some(w) = web_sys::window() {
                let _ = w.navigator().clipboard().write_text(&text);
            }
            flash_copied(&button);
            return;
        }
        if target.class_list().contains("tk") {
            ev.prevent_default();
            if !readonly {
                self.settle();
                if let Some(tr) = self.model(move |st| commands::toggle_task(st, idx)) {
                    self.dispatch(tr);
                }
            }
            return;
        }
        if let Some(link_el) = target.closest("[data-link]").ok().flatten() {
            let follow = ev.ctrl_key() || ev.meta_key() || !line_active || readonly;
            if !follow {
                return;
            }
            let Some(k) = link_el
                .get_attribute("data-link")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            let target = self.model(move |st| {
                let styled = syntax::style_line(st.doc().line(idx), st.block(idx), &|_| true);
                styled.links.get(k).map(|(_, _, t)| t.clone())
            });
            if let Some(t) = target {
                ev.prevent_default();
                self.host.open_link(&t, ev.ctrl_key() || ev.meta_key());
            }
        }
    }

    // ------------------------------------------------------------ teardown

    /// Stop everything that could call back into the editor [W-L5]:
    /// listeners, the mutation observer, the frame and composition timers.
    fn teardown(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        for (t, name, capture, cb) in self.listeners.borrow_mut().drain(..) {
            let _ = t.remove_event_listener_with_callback_and_bool(
                name,
                cb.as_ref().unchecked_ref(),
                capture,
            );
        }
        if let Some((o, _cb)) = self.observer.borrow_mut().take() {
            o.disconnect();
        }
        if let Some((o, _cb)) = self.resize.borrow_mut().take() {
            o.disconnect();
        }
        if let Some(w) = web_sys::window() {
            if let Some(id) = self.frame.take() {
                let _ = w.cancel_animation_frame(id);
            }
            if let Some(id) = self.ime_timer.take() {
                w.clear_timeout_with_handle(id);
            }
        }
        self.frame_cb.borrow_mut().take();
        self.ime_cb.borrow_mut().take();
        self.records.borrow_mut().clear();
    }

    /// Whether the editor was destroyed (for tests).
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.get()
    }
}

/// The lines whose syntax shows while the editor has the focus: the
/// selection's, and the fences of the code blocks it starts or ends in (so
/// that the options of a block can be seen and edited from inside it).
fn active_lines(st: &EditorState) -> Vec<Range<usize>> {
    let doc = st.doc();
    let s = st.selection();
    let (a, b) = (doc.line_of(s.from()), doc.line_of(s.to()));
    let mut out = Vec::with_capacity(3);
    out.push(a..b + 1);
    for line in [a, b] {
        if let Some((open, close)) = st.code_block(line) {
            out.push(open..open + 1);
            out.extend(close.map(|c| c..c + 1));
        }
        // An HTML block shows its source, without the preview, while the
        // selection is in it.
        out.extend(st.html_block(line));
    }
    crate::decorations::merge_ranges(out)
}

/// The part of a line of code that scrolls sideways (`.lx`; blank lines
/// have none).
fn code_part(line: &HtmlElement) -> Option<Element> {
    line.first_element_child()
        .filter(|e| e.class_list().contains("lx"))
}

/// Scroll a line of code sideways to `x`: its scrolling part, or the guides
/// of a blank line (which has none).
fn set_code_scroll(line: &HtmlElement, x: i32) {
    match code_part(line) {
        Some(lx) => {
            if lx.scroll_left() != x {
                lx.set_scroll_left(x);
            }
        }
        _ => {
            let guides = line
                .query_selector(":scope > .igs")
                .ok()
                .flatten()
                .and_then(|g| g.dyn_into::<HtmlElement>().ok());
            if let Some(g) = guides {
                let s = g.style();
                let _ = if x == 0 {
                    s.remove_property("transform").map(|_| ())
                } else {
                    s.set_property("transform", &format!("translateX({}px)", -x))
                };
            }
        }
    }
}

/// Show on a copy button that it copied (its icon turns into a check).
pub fn flash_copied(button: &Element) {
    let _ = button.class_list().add_1("copied");
    let b = button.clone();
    let done = Closure::once_into_js(move || {
        let _ = b.class_list().remove_1("copied");
    });
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(done.unchecked_ref(), 1500);
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.teardown();
    }
}
