//! A Markdown editor on a `contenteditable` element.
//!
//! The model is the Markdown source string (mirroring the note's CRDT text).
//! Native input is intercepted via `beforeinput`, turned into byte-offset
//! splices, applied to the model, reported to the host (which applies them to
//! the CRDT) and re-rendered line by line. Anything the browser does on its
//! own (IME composition, unusual input types) is reconciled by diffing the DOM
//! text against the model afterwards, so the DOM can never silently diverge
//! from what gets synced.

pub mod syntax;
pub mod vim;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use coolpsonotes_core::textdiff::{self, Edit};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement, InputEvent, KeyboardEvent, MouseEvent, Node};

use crate::util::document;
use syntax::{Block, Target};

pub trait Host {
    /// Local edits, already applied to the editor's text.
    fn edit(&self, edits: &[Edit]);
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
    fn insert_files(&self, files: Vec<web_sys::File>, at: usize);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Bold,
    Italic,
    Strike,
    Highlight,
    Code,
    CodeBlock,
    Link,
    Heading(u8),
    Bullet,
    Numbered,
    Task,
    Quote,
    Rule,
    Table,
    Indent,
    Outdent,
    Undo,
    Redo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteCursor {
    pub name: String,
    pub color: String,
    pub anchor: usize,
    pub head: usize,
}

struct State {
    text: String,
    starts: Vec<usize>,
    keys: Vec<String>,
    readonly: bool,
    mode: Mode,
    composing: bool,
    pending_remote: Option<String>,
    sel: (usize, usize),
    active: Vec<usize>,
    cursors: Vec<RemoteCursor>,
    vim: Option<vim::Vim>,
}

pub struct Editor {
    root: HtmlElement,
    overlay: HtmlElement,
    host: Rc<dyn Host>,
    st: RefCell<State>,
    listeners: RefCell<Vec<(web_sys::EventTarget, &'static str, Closure<dyn FnMut(web_sys::Event)>)>>,
}

fn utf16_to_byte(s: &str, u: usize) -> usize {
    let mut acc = 0;
    for (i, c) in s.char_indices() {
        if acc >= u {
            return i;
        }
        acc += c.len_utf16();
    }
    s.len()
}

fn byte_to_utf16(s: &str, b: usize) -> usize {
    let b = b.min(s.len());
    let mut b2 = b;
    while !s.is_char_boundary(b2) {
        b2 -= 1;
    }
    s[..b2].chars().map(char::len_utf16).sum()
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

fn prev_char(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
}

fn next_char(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(s.len())
}

impl Editor {
    /// Build an editor inside `container` (which should be empty).
    pub fn new(container: &Element, text: String, host: Rc<dyn Host>, mode: Mode, readonly: bool, vim: bool) -> Rc<Editor> {
        let doc = document();
        let wrap: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
        wrap.set_class_name("ed-wrap");
        let root: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
        root.set_class_name("ed");
        let _ = root.set_attribute("spellcheck", "true");
        let _ = root.set_attribute("autocapitalize", "sentences");
        let _ = root.set_attribute("role", "textbox");
        let _ = root.set_attribute("aria-multiline", "true");
        let _ = root.set_attribute("aria-label", "Note text");
        let overlay: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
        overlay.set_class_name("ed-cursors");
        let _ = overlay.set_attribute("aria-hidden", "true");
        let _ = wrap.append_child(&root);
        let _ = wrap.append_child(&overlay);
        let _ = container.append_child(&wrap);
        let ed = Rc::new(Editor {
            root,
            overlay,
            host,
            st: RefCell::new(State {
                starts: line_starts(&text),
                text,
                keys: vec![],
                readonly,
                mode,
                composing: false,
                pending_remote: None,
                sel: (0, 0),
                active: vec![],
                cursors: vec![],
                vim: if vim { Some(vim::Vim::default()) } else { None },
            }),
            listeners: RefCell::new(vec![]),
        });
        ed.apply_mode_classes();
        ed.render(true);
        Editor::attach(&ed);
        ed
    }

    pub fn element(&self) -> &HtmlElement {
        &self.root
    }

    pub fn text(&self) -> String {
        self.st.borrow().text.clone()
    }

    fn apply_mode_classes(&self) {
        let st = self.st.borrow();
        let mut c = String::from("ed");
        c.push_str(if st.mode == Mode::Source { " source" } else { " live" });
        if st.readonly {
            c.push_str(" readonly");
        }
        if let Some(v) = &st.vim {
            c.push_str(if v.insert { " vim vim-insert" } else { " vim vim-normal" });
        }
        self.root.set_class_name(&c);
        let _ = self.root.set_attribute("contenteditable", if st.readonly { "false" } else { "true" });
    }

    pub fn set_mode(&self, mode: Mode) {
        self.st.borrow_mut().mode = mode;
        self.apply_mode_classes();
        self.render(true);
    }

    pub fn set_readonly(&self, ro: bool) {
        self.st.borrow_mut().readonly = ro;
        self.apply_mode_classes();
    }

    pub fn set_vim(&self, on: bool) {
        self.st.borrow_mut().vim = if on { Some(vim::Vim::default()) } else { None };
        self.apply_mode_classes();
    }

    pub fn focus(&self) {
        let _ = self.root.focus();
        let (a, h) = self.st.borrow().sel;
        self.set_selection(a, h);
    }

    pub fn has_focus(&self) -> bool {
        document().active_element().is_some_and(|e| e.is_same_node(Some(self.root.unchecked_ref())))
    }

    pub fn selection(&self) -> (usize, usize) {
        self.st.borrow().sel
    }

    // ------------------------------------------------------------ rendering

    /// Re-render every line (e.g. after link targets or images changed).
    pub fn refresh(&self) {
        let sel = self.read_selection();
        self.render(true);
        if let Some((a, h)) = sel
            && self.has_focus() {
                self.set_selection(a, h);
            }
    }

    fn render(&self, force: bool) {
        let (lines, blocks, keys, htmls) = {
            let st = self.st.borrow();
            let lines: Vec<String> = st.text.split('\n').map(str::to_string).collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let blocks = syntax::classify(&refs);
            let host = self.host.clone();
            let resolves = move |t: &str| host.link_exists(t);
            let mut keys = Vec::with_capacity(lines.len());
            let mut htmls = Vec::with_capacity(lines.len());
            for (i, line) in lines.iter().enumerate() {
                let styled = syntax::style_line(line, &blocks[i], &resolves);
                let mut html = syntax::line_html(line, &styled);
                let mut img = String::new();
                if let Some(t) = &styled.image
                    && let Some(url) = self.host.image_url(t) {
                        img = url;
                        html.push_str("<span class=\"w\" contenteditable=\"false\" data-w=\"1\"><img src=\"");
                        syntax::escape(&img, &mut html);
                        html.push_str("\" alt=\"\" draggable=\"false\"></span>");
                    }
                let lang = match &blocks[i] {
                    Block::Fence { lang } | Block::Code { lang } => lang.clone(),
                    _ => String::new(),
                };
                keys.push(format!("{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}", blocks[i].class(), line, img, lang, html.len()));
                htmls.push(html);
            }
            (lines, blocks, keys, htmls)
        };
        let old = std::mem::take(&mut self.st.borrow_mut().keys);
        let (mut p, mut s) = (0, 0);
        if !force {
            while p < old.len() && p < keys.len() && old[p] == keys[p] {
                p += 1;
            }
            while s < old.len() - p && s < keys.len() - p && old[old.len() - 1 - s] == keys[keys.len() - 1 - s] {
                s += 1;
            }
        }
        let old_mid = old.len() - p - s;
        let new_mid = keys.len() - p - s;
        let children = self.root.children();
        let common = old_mid.min(new_mid);
        for k in 0..common {
            if let Some(el) = children.item((p + k) as u32) {
                el.set_class_name(blocks[p + k].class());
                el.set_inner_html(&htmls[p + k]);
            }
        }
        let doc = document();
        if new_mid > old_mid {
            let before = children.item((p + old_mid) as u32);
            for k in common..new_mid {
                let el = doc.create_element("div").unwrap();
                el.set_class_name(blocks[p + k].class());
                el.set_inner_html(&htmls[p + k]);
                let _ = self.root.insert_before(&el, before.as_ref().map(|b| b.unchecked_ref::<Node>()));
            }
        } else if old_mid > new_mid {
            for k in (new_mid..old_mid).rev() {
                if let Some(el) = children.item((p + k) as u32) {
                    el.remove();
                }
            }
        }
        // Defensive: drop anything the browser may have inserted.
        while self.root.children().length() as usize > lines.len() {
            if let Some(last) = self.root.last_element_child() {
                last.remove();
            }
        }
        let children = self.root.children();
        for i in 0..children.length() {
            if let Some(el) = children.item(i)
                && el.get_attribute("data-l").as_deref() != Some(i.to_string().as_str()) {
                    let _ = el.set_attribute("data-l", &i.to_string());
                }
        }
        {
            let mut st = self.st.borrow_mut();
            st.keys = keys;
            st.starts = line_starts(&st.text);
            st.active.clear();
        }
        self.update_active();
        self.draw_cursors();
    }

    fn line_of(&self, offset: usize) -> usize {
        let st = self.st.borrow();
        match st.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    fn update_active(&self) {
        let (a, h) = self.st.borrow().sel;
        let (lo, hi) = (a.min(h), a.max(h));
        let (l0, l1) = (self.line_of(lo), self.line_of(hi));
        let focused = self.has_focus();
        let want: Vec<usize> = if focused { (l0..=l1).collect() } else { vec![] };
        let prev = std::mem::take(&mut self.st.borrow_mut().active);
        let children = self.root.children();
        for i in prev.iter().filter(|i| !want.contains(i)) {
            if let Some(el) = children.item(*i as u32) {
                let _ = el.class_list().remove_1("active");
            }
        }
        for i in &want {
            if let Some(el) = children.item(*i as u32) {
                let _ = el.class_list().add_1("active");
            }
        }
        self.st.borrow_mut().active = want;
    }

    // ------------------------------------------------------------ selection mapping

    fn line_element(&self, node: &Node) -> Option<(usize, Element)> {
        let mut cur = Some(node.clone());
        while let Some(n) = cur {
            let parent = n.parent_node();
            if parent.as_ref().is_some_and(|p| p == self.root.unchecked_ref::<Node>()) {
                let el: Element = n.dyn_into().ok()?;
                let idx = el.get_attribute("data-l")?.parse().ok()?;
                return Some((idx, el));
            }
            cur = parent;
        }
        None
    }

    fn dom_to_offset(&self, node: &Node, offset: u32) -> Option<usize> {
        let root_node: &Node = self.root.unchecked_ref();
        let st = self.st.borrow();
        if node == root_node {
            let n_lines = st.starts.len();
            let idx = offset as usize;
            return Some(if idx >= n_lines { st.text.len() } else { st.starts[idx] });
        }
        let (idx, el) = self.line_element(node)?;
        let start = *st.starts.get(idx)?;
        let end = st.starts.get(idx + 1).map(|e| e - 1).unwrap_or(st.text.len());
        let line = &st.text[start..end];
        let range = document().create_range().ok()?;
        range.set_start(&el, 0).ok()?;
        if range.set_end(node, offset).is_err() {
            return Some(end);
        }
        let prefix: String = range.to_string().into();
        Some(start + utf16_to_byte(line, prefix.encode_utf16().count()))
    }

    fn offset_to_dom(&self, offset: usize) -> Option<(Node, u32)> {
        let idx = self.line_of(offset);
        let st = self.st.borrow();
        let start = st.starts[idx];
        let end = st.starts.get(idx + 1).map(|e| e - 1).unwrap_or(st.text.len());
        let line = &st.text[start..end];
        let target = byte_to_utf16(line, offset.saturating_sub(start).min(line.len()));
        let el = self.root.children().item(idx as u32)?;
        let walker = document().create_tree_walker_with_what_to_show(&el, 4).ok()?; // SHOW_TEXT
        let mut acc = 0usize;
        let mut last: Option<(Node, u32)> = None;
        while let Ok(Some(n)) = walker.next_node() {
            if n.parent_element().is_some_and(|p| p.closest("[data-w]").ok().flatten().is_some()) {
                continue;
            }
            let len = n.node_value().map(|v| v.encode_utf16().count()).unwrap_or(0);
            if acc + len >= target && len > 0 {
                return Some((n, (target - acc) as u32));
            }
            acc += len;
            last = Some((n.clone(), len as u32));
        }
        Some(last.unwrap_or((el.unchecked_into(), 0)))
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

    pub fn set_selection(&self, anchor: usize, head: usize) {
        let len = self.st.borrow().text.len();
        let (anchor, head) = (anchor.min(len), head.min(len));
        self.st.borrow_mut().sel = (anchor, head);
        let (Some((an, ao)), Some((hn, ho))) = (self.offset_to_dom(anchor), self.offset_to_dom(head)) else { return };
        if let Ok(Some(sel)) = document().get_selection() {
            let _ = sel.set_base_and_extent(&an, ao, &hn, ho);
        }
        self.update_active();
        self.scroll_caret_into_view();
    }

    fn scroll_caret_into_view(&self) {
        let head = self.st.borrow().sel.1;
        let idx = self.line_of(head);
        if let Some(el) = self.root.children().item(idx as u32) {
            let r = el.get_bounding_client_rect();
            let vh = crate::util::window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0);
            if r.bottom() > vh - 24.0 || r.top() < 56.0 {
                let opts = web_sys::ScrollIntoViewOptions::new();
                opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                el.scroll_into_view_with_scroll_into_view_options(&opts);
            }
        }
    }

    fn sel_sorted(&self) -> (usize, usize) {
        let (a, h) = self.st.borrow().sel;
        (a.min(h), a.max(h))
    }

    // ------------------------------------------------------------ editing

    /// Replace `[from, to)` with `insert`, then put the caret at `caret`.
    fn replace(&self, from: usize, to: usize, insert: &str, caret: Option<(usize, usize)>) {
        if self.st.borrow().readonly {
            return;
        }
        let mut edits = Vec::with_capacity(2);
        {
            let mut st = self.st.borrow_mut();
            let to = to.min(st.text.len());
            let from = from.min(to);
            if to > from {
                edits.push(Edit::Delete { at: from, len: to - from });
            }
            if !insert.is_empty() {
                edits.push(Edit::Insert { at: from, text: insert.to_string() });
            }
            if edits.is_empty() {
                return;
            }
            let mut t = std::mem::take(&mut st.text);
            t.replace_range(from..to, insert);
            st.text = t;
        }
        self.host.edit(&edits);
        self.render(false);
        let (a, h) = caret.unwrap_or_else(|| {
            let p = from + insert.len();
            (p, p)
        });
        self.set_selection(a, h);
        self.host.selection_changed(a, h);
    }

    fn apply_edits_local(&self, edits: Vec<Edit>, caret: (usize, usize)) {
        if edits.is_empty() || self.st.borrow().readonly {
            return;
        }
        {
            let mut st = self.st.borrow_mut();
            st.text = textdiff::apply(&st.text, &edits);
        }
        self.host.edit(&edits);
        self.render(false);
        self.set_selection(caret.0, caret.1);
        self.host.selection_changed(caret.0, caret.1);
    }

    pub fn insert_text(&self, s: &str) {
        let (a, b) = self.sel_sorted();
        self.replace(a, b, s, None);
    }

    /// Replace the whole text with a new version produced elsewhere (remote
    /// edits, undo). The selection is mapped through the change.
    pub fn set_text(&self, new_text: String, caret_to_change: bool) {
        if self.st.borrow().composing {
            self.st.borrow_mut().pending_remote = Some(new_text);
            return;
        }
        let edits = textdiff::diff(&self.st.borrow().text, &new_text);
        if edits.is_empty() {
            return;
        }
        let focused = self.has_focus();
        let (mut a, mut h) = self.read_selection().unwrap_or(self.st.borrow().sel);
        let mut changed_at = None;
        for e in &edits {
            match e {
                Edit::Insert { at, text } => {
                    changed_at = Some(at + text.len());
                    if *at < a || (*at == a && !caret_to_change) {
                        a += text.len();
                    }
                    if *at < h || (*at == h && !caret_to_change) {
                        h += text.len();
                    }
                }
                Edit::Delete { at, len } => {
                    changed_at = Some(*at);
                    if a > *at {
                        a -= (*len).min(a - at);
                    }
                    if h > *at {
                        h -= (*len).min(h - at);
                    }
                }
            }
        }
        if caret_to_change
            && let Some(c) = changed_at {
                a = c;
                h = c;
            }
        // Shift remote cursors as well.
        {
            let mut st = self.st.borrow_mut();
            for c in &mut st.cursors {
                for e in &edits {
                    match e {
                        Edit::Insert { at, text } => {
                            if *at < c.head {
                                c.head += text.len();
                            }
                            if *at < c.anchor {
                                c.anchor += text.len();
                            }
                        }
                        Edit::Delete { at, len } => {
                            if c.head > *at {
                                c.head -= (*len).min(c.head - at);
                            }
                            if c.anchor > *at {
                                c.anchor -= (*len).min(c.anchor - at);
                            }
                        }
                    }
                }
            }
            st.text = new_text;
        }
        self.render(false);
        if focused {
            self.set_selection(a, h);
        } else {
            self.st.borrow_mut().sel = (a, h);
        }
    }

    fn current_line(&self, offset: usize) -> (usize, usize, usize) {
        let idx = self.line_of(offset);
        let st = self.st.borrow();
        let start = st.starts[idx];
        let end = st.starts.get(idx + 1).map(|e| e - 1).unwrap_or(st.text.len());
        (idx, start, end)
    }

    fn smart_newline(&self) {
        let (a, b) = self.sel_sorted();
        let (_, ls, le) = self.current_line(a);
        let text = self.st.borrow().text.clone();
        let line = &text[ls..le];
        let block = syntax::classify(&[line]).remove(0);
        let in_code = {
            let lines: Vec<&str> = text[..le].split('\n').collect();
            matches!(syntax::classify(&lines).last(), Some(Block::Code { .. }))
        };
        self.host.boundary();
        if in_code {
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            self.replace(a, b, &format!("\n{indent}"), None);
            return;
        }
        if let Some(k) = syntax::table_enter(&text, a) {
            self.apply_table_key(k);
            return;
        }
        if let Some((indent, mlen, ordered)) = syntax::list_marker(line) {
            let content_start = indent + mlen;
            let task = syntax::task_box(&line[content_start..]);
            let body_start = content_start + if task.is_some() { 4.min(line.len() - content_start) } else { 0 };
            if line[body_start..].trim().is_empty() && a >= ls + body_start {
                // Empty item: end the list (or outdent one level).
                if indent > 0 {
                    let cut = indent.min(2);
                    self.replace(ls, ls + cut, "", Some((a - cut, a - cut)));
                } else {
                    self.replace(ls, le, "", Some((ls, ls)));
                }
                return;
            }
            if a < ls + content_start {
                self.replace(a, b, "\n", None);
                return;
            }
            let marker = &line[indent..indent + mlen];
            let next = if ordered {
                let n: u64 = marker.trim_end_matches([' ', '.', ')']).parse().unwrap_or(1);
                let sep = if marker.contains(')') { ")" } else { "." };
                format!("{}{sep} ", n + 1)
            } else {
                marker.to_string()
            };
            let task_part = if task.is_some() { "[ ] " } else { "" };
            self.replace(a, b, &format!("\n{}{next}{task_part}", &line[..indent]), None);
            return;
        }
        if block == Block::Quote {
            let prefix: String = line.chars().take_while(|c| *c == '>' || *c == ' ').collect();
            if line[prefix.len()..].trim().is_empty() {
                self.replace(ls, le, "", Some((ls, ls)));
            } else {
                self.replace(a, b, &format!("\n{prefix}"), None);
            }
            return;
        }
        self.replace(a, b, "\n", None);
    }

    fn apply_table_key(&self, k: syntax::TableKey) {
        match k {
            syntax::TableKey::Move(o) => {
                self.set_selection(o, o);
                self.update_active();
            }
            syntax::TableKey::Edit { from, to, insert, caret } => self.replace(from, to, &insert, Some((caret, caret))),
        }
    }

    fn smart_backspace(&self) -> bool {
        let (a, b) = self.sel_sorted();
        if a != b {
            return false;
        }
        let (_, ls, _le) = self.current_line(a);
        let text = self.st.borrow().text.clone();
        let line_to_caret = &text[ls..a];
        if let Some((indent, mlen, _)) = syntax::list_marker(&text[ls..]) {
            let content_start = indent + mlen;
            let task = syntax::task_box(&text[ls + content_start..]).map(|_| 4).unwrap_or(0);
            if line_to_caret.len() == content_start + task {
                // Backspace right after a list marker removes the marker.
                self.host.boundary();
                self.replace(ls + indent, a, "", Some((ls + indent, ls + indent)));
                return true;
            }
        }
        if let Some(rest) = line_to_caret.strip_prefix('>')
            && rest.trim().is_empty() {
                self.replace(ls, a, "", Some((ls, ls)));
                return true;
            }
        false
    }

    fn toggle_wrap(&self, open: &str, close: &str) {
        let (a, b) = self.sel_sorted();
        let text = self.st.borrow().text.clone();
        let (ol, cl) = (open.len(), close.len());
        if a >= ol && b + cl <= text.len() && &text[a - ol..a] == open && &text[b..b + cl] == close {
            let edits = vec![Edit::Delete { at: b, len: cl }, Edit::Delete { at: a - ol, len: ol }];
            self.apply_edits_local(edits, (a - ol, b - ol));
            return;
        }
        if a == b {
            // Wrap the word under the caret, or insert an empty pair.
            let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
            let ws = text[..a].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map(|(i, _)| i).unwrap_or(a);
            let we = a + text[a..].chars().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();
            if ws < we {
                let edits =
                    vec![Edit::Insert { at: we, text: close.into() }, Edit::Insert { at: ws, text: open.into() }];
                self.apply_edits_local(edits, (a + ol, a + ol));
            } else {
                self.replace(a, a, &format!("{open}{close}"), Some((a + ol, a + ol)));
            }
            return;
        }
        let edits = vec![Edit::Insert { at: b, text: close.into() }, Edit::Insert { at: a, text: open.into() }];
        self.apply_edits_local(edits, (a + ol, b + ol));
    }

    /// Apply a transformation to every line touched by the selection.
    fn map_lines(&self, f: impl Fn(&str, usize) -> String) {
        let (a, b) = self.sel_sorted();
        let (l0, s0, _) = self.current_line(a);
        let (l1, _, e1) = self.current_line(b);
        let text = self.st.borrow().text.clone();
        let old = &text[s0..e1];
        let new: Vec<String> = old.split('\n').enumerate().map(|(i, l)| f(l, l0 + i)).collect();
        let new = new.join("\n");
        if new == old {
            return;
        }
        self.host.boundary();
        let delta = new.len() as isize - old.len() as isize;
        let caret = if l0 == l1 { ((b as isize + delta).max(s0 as isize)) as usize } else { s0 + new.len() };
        self.replace(s0, e1, &new, Some((caret, caret)));
    }

    fn strip_prefix(line: &str) -> (&str, &str) {
        if let Some((lvl, _)) = coolpsonotes_core::markdown::heading(line) {
            let cut = line.find('#').unwrap_or(0) + lvl as usize;
            let cut = cut + line[cut..].chars().take_while(|c| *c == ' ').count();
            return (&line[..0], &line[cut..]);
        }
        if let Some((indent, mlen, _)) = syntax::list_marker(line) {
            let mut cut = indent + mlen;
            if syntax::task_box(&line[cut..]).is_some() {
                cut = (cut + 4).min(line.len());
            }
            return (&line[..indent], &line[cut..]);
        }
        if let Some(r) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
            return ("", r);
        }
        ("", line)
    }

    pub fn command(&self, cmd: Command) {
        if self.st.borrow().readonly {
            return;
        }
        match cmd {
            Command::Bold => self.toggle_wrap("**", "**"),
            Command::Italic => self.toggle_wrap("*", "*"),
            Command::Strike => self.toggle_wrap("~~", "~~"),
            Command::Highlight => self.toggle_wrap("==", "=="),
            Command::Code => self.toggle_wrap("`", "`"),
            Command::Link => {
                let (a, b) = self.sel_sorted();
                let sel = self.st.borrow().text[a..b].to_string();
                if sel.starts_with("http://") || sel.starts_with("https://") {
                    self.replace(a, b, &format!("[]({sel})"), Some((a + 1, a + 1)));
                } else if sel.is_empty() {
                    self.replace(a, b, "[[]]", Some((a + 2, a + 2)));
                } else {
                    self.replace(a, b, &format!("[{sel}](https://)"), Some((a + sel.len() + 3, a + sel.len() + 11)));
                }
            }
            Command::CodeBlock => {
                let (a, b) = self.sel_sorted();
                let sel = self.st.borrow().text[a..b].to_string();
                let (_, ls, _) = self.current_line(a);
                let lead = if a > ls { "\n" } else { "" };
                let ins = format!("{lead}```\n{sel}\n```\n");
                let caret = a + lead.len() + 3;
                self.replace(a, b, &ins, Some((caret, caret)));
            }
            Command::Heading(n) => self.map_lines(|l, _| {
                let (_, body) = Editor::strip_prefix(l);
                let cur = coolpsonotes_core::markdown::heading(l).map(|h| h.0);
                if n == 0 || cur == Some(n) { body.to_string() } else { format!("{} {body}", "#".repeat(n as usize)) }
            }),
            Command::Bullet => self.map_lines(|l, _| {
                let (indent, body) = Editor::strip_prefix(l);
                if syntax::list_marker(l).is_some_and(|m| !m.2) && syntax::task_box(&l[syntax::list_marker(l).unwrap().0 + 2..]).is_none() {
                    format!("{indent}{body}")
                } else {
                    format!("{indent}- {body}")
                }
            }),
            Command::Numbered => {
                let (a, _) = self.sel_sorted();
                let first = self.line_of(a);
                self.map_lines(move |l, i| {
                    let (indent, body) = Editor::strip_prefix(l);
                    if syntax::list_marker(l).is_some_and(|m| m.2) {
                        format!("{indent}{body}")
                    } else {
                        format!("{indent}{}. {body}", i - first + 1)
                    }
                })
            }
            Command::Task => self.map_lines(|l, _| {
                let (indent, body) = Editor::strip_prefix(l);
                let is_task = syntax::list_marker(l).is_some_and(|(i, m, _)| syntax::task_box(&l[i + m..]).is_some());
                if is_task { format!("{indent}{body}") } else { format!("{indent}- [ ] {body}") }
            }),
            Command::Quote => self.map_lines(|l, _| {
                if let Some(r) = l.strip_prefix("> ").or_else(|| l.strip_prefix('>')) { r.to_string() } else { format!("> {l}") }
            }),
            Command::Indent => self.map_lines(|l, _| format!("  {l}")),
            Command::Outdent => self.map_lines(|l, _| {
                l.strip_prefix("  ").or_else(|| l.strip_prefix('\t')).or_else(|| l.strip_prefix(' ')).unwrap_or(l).to_string()
            }),
            Command::Rule => {
                let (a, b) = self.sel_sorted();
                self.replace(a, b, "\n\n---\n\n", None);
            }
            Command::Table => {
                let (a, b) = self.sel_sorted();
                let t = "\n| Column 1 | Column 2 |\n| --- | --- |\n|  |  |\n";
                self.replace(a, b, t, Some((a + 2, a + 10)));
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
        }
        let _ = self.root.focus();
    }

    fn undo(&self) {
        if let Some(t) = self.host.undo() {
            self.set_text(t, true);
        }
    }

    fn redo(&self) {
        if let Some(t) = self.host.redo() {
            self.set_text(t, true);
        }
    }

    fn toggle_task_at_line(&self, idx: usize) {
        let (start, line) = {
            let st = self.st.borrow();
            let Some(&s) = st.starts.get(idx) else { return };
            let e = st.starts.get(idx + 1).map(|e| e - 1).unwrap_or(st.text.len());
            (s, st.text[s..e].to_string())
        };
        if let Some((indent, mlen, _)) = syntax::list_marker(&line) {
            let at = start + indent + mlen + 1;
            match syntax::task_box(&line[indent + mlen..]) {
                Some(true) => {
                    let sel = self.st.borrow().sel;
                    self.host.boundary();
                    self.apply_edits_local(vec![Edit::Delete { at, len: 1 }, Edit::Insert { at, text: " ".into() }], sel)
                }
                Some(false) => {
                    let sel = self.st.borrow().sel;
                    self.host.boundary();
                    self.apply_edits_local(vec![Edit::Delete { at, len: 1 }, Edit::Insert { at, text: "x".into() }], sel)
                }
                None => {}
            }
        }
    }

    /// Re-read the DOM after the browser changed it on its own.
    fn sync_from_dom(&self) {
        let mut lines = Vec::new();
        let children = self.root.child_nodes();
        for i in 0..children.length() {
            let Some(n) = children.item(i) else { continue };
            let t = match n.dyn_ref::<Element>() {
                Some(el) => {
                    // Exclude widget text and treat nested block children as line breaks.
                    let clone = el.clone_node_with_deep(true).ok().and_then(|c| c.dyn_into::<Element>().ok());
                    if let Some(c) = &clone {
                        if let Ok(ws) = c.query_selector_all("[data-w]") {
                            for k in 0..ws.length() {
                                if let Some(w) = ws.item(k)
                                    && let Some(p) = w.parent_node() {
                                        let _ = p.remove_child(&w);
                                    }
                            }
                        }
                        if let Ok(divs) = c.query_selector_all("div, br + *") {
                            if divs.length() > 0 {
                                // Browser split a line into blocks: use innerText-like joining.
                                let mut parts = Vec::new();
                                let kids = c.child_nodes();
                                for j in 0..kids.length() {
                                    if let Some(k) = kids.item(j) {
                                        parts.push(k.text_content().unwrap_or_default());
                                    }
                                }
                                parts.join("\n")
                            } else {
                                c.text_content().unwrap_or_default()
                            }
                        } else {
                            c.text_content().unwrap_or_default()
                        }
                    } else {
                        el.text_content().unwrap_or_default()
                    }
                }
                None => n.text_content().unwrap_or_default(),
            };
            lines.push(t);
        }
        let dom_text = lines.join("\n").replace('\u{a0}', " ");
        let sel = self.read_selection();
        let old = self.st.borrow().text.clone();
        if dom_text == old {
            return;
        }
        let edits = textdiff::diff(&old, &dom_text);
        self.st.borrow_mut().text = dom_text;
        self.host.edit(&edits);
        let caret = sel.unwrap_or_else(|| {
            let p = match edits.last() {
                Some(Edit::Insert { at, text }) => at + text.len(),
                Some(Edit::Delete { at, .. }) => *at,
                None => 0,
            };
            (p, p)
        });
        self.render(true);
        self.set_selection(caret.0, caret.1);
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

    // ------------------------------------------------------------ remote cursors

    pub fn set_remote_cursors(&self, cursors: Vec<RemoteCursor>) {
        self.st.borrow_mut().cursors = cursors;
        self.draw_cursors();
    }

    fn draw_cursors(&self) {
        let cursors = self.st.borrow().cursors.clone();
        self.overlay.set_inner_html("");
        if cursors.is_empty() {
            return;
        }
        let base = self.overlay.get_bounding_client_rect();
        let doc = document();
        let len = self.st.borrow().text.len();
        for c in cursors {
            let Some((node, off)) = self.offset_to_dom(c.head.min(len)) else { continue };
            let Ok(range) = doc.create_range() else { continue };
            if range.set_start(&node, off).is_err() || range.set_end(&node, off).is_err() {
                continue;
            }
            let mut rect = range.get_bounding_client_rect();
            if rect.height() == 0.0 {
                if let Some(el) = node.dyn_ref::<Element>() {
                    rect = el.get_bounding_client_rect();
                } else if let Some(p) = node.parent_element() {
                    rect = p.get_bounding_client_rect();
                }
            }
            let el: HtmlElement = doc.create_element("div").unwrap().unchecked_into();
            el.set_class_name("rc");
            let style = el.style();
            let _ = style.set_property("left", &format!("{}px", rect.left() - base.left()));
            let _ = style.set_property("top", &format!("{}px", rect.top() - base.top()));
            let _ = style.set_property("height", &format!("{}px", rect.height().max(16.0)));
            let _ = style.set_property("--c", &c.color);
            let label: HtmlElement = doc.create_element("span").unwrap().unchecked_into();
            label.set_text_content(Some(&c.name));
            let _ = el.append_child(&label);
            let _ = self.overlay.append_child(&el);
        }
    }

    // ------------------------------------------------------------ events

    fn listen(this: &Rc<Editor>, target: &web_sys::EventTarget, name: &'static str, f: impl Fn(&Rc<Editor>, web_sys::Event) + 'static) {
        let weak: Weak<Editor> = Rc::downgrade(this);
        let cb = Closure::wrap(Box::new(move |e: web_sys::Event| {
            if let Some(ed) = weak.upgrade() {
                f(&ed, e);
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        let _ = target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
        this.listeners.borrow_mut().push((target.clone(), name, cb));
    }

    fn attach(this: &Rc<Editor>) {
        let root: web_sys::EventTarget = this.root.clone().unchecked_into();
        Editor::listen(this, &root, "beforeinput", |ed, e| ed.on_beforeinput(e.unchecked_into()));
        Editor::listen(this, &root, "input", |ed, e| {
            let ie: InputEvent = e.unchecked_into();
            if !ed.st.borrow().composing && !ie.is_composing() {
                ed.sync_from_dom();
            }
        });
        Editor::listen(this, &root, "compositionstart", |ed, _| ed.st.borrow_mut().composing = true);
        Editor::listen(this, &root, "compositionend", |ed, _| {
            ed.st.borrow_mut().composing = false;
            ed.sync_from_dom();
            if let Some(t) = ed.st.borrow_mut().pending_remote.take() {
                ed.set_text(t, false);
            }
        });
        Editor::listen(this, &root, "keydown", |ed, e| ed.on_keydown(e.unchecked_into()));
        Editor::listen(this, &root, "copy", |ed, e| ed.on_copy(e.unchecked_into(), false));
        Editor::listen(this, &root, "cut", |ed, e| ed.on_copy(e.unchecked_into(), true));
        Editor::listen(this, &root, "paste", |ed, e| ed.on_paste(e.unchecked_into()));
        Editor::listen(this, &root, "drop", |ed, e| ed.on_drop(e.unchecked_into()));
        Editor::listen(this, &root, "dragover", |_ed, e| e.prevent_default());
        Editor::listen(this, &root, "mousedown", |ed, e| ed.on_mousedown(e.unchecked_into()));
        Editor::listen(this, &root, "focus", |ed, _| ed.update_active());
        Editor::listen(this, &root, "blur", |ed, _| ed.update_active());
        let doc: web_sys::EventTarget = document().unchecked_into();
        Editor::listen(this, &doc, "selectionchange", |ed, _| {
            if ed.st.borrow().composing {
                return;
            }
            if let Some((a, h)) = ed.read_selection() {
                let changed = ed.st.borrow().sel != (a, h);
                ed.st.borrow_mut().sel = (a, h);
                ed.update_active();
                if changed {
                    ed.host.selection_changed(a, h);
                }
            }
        });
        let win: web_sys::EventTarget = crate::util::window().unchecked_into();
        Editor::listen(this, &win, "resize", |ed, _| ed.draw_cursors());
    }

    fn on_beforeinput(&self, ev: InputEvent) {
        let ro = self.st.borrow().readonly;
        if ro {
            ev.prevent_default();
            return;
        }
        if ev.is_composing() || self.st.borrow().composing {
            return;
        }
        if self.st.borrow().vim.as_ref().is_some_and(|v| !v.insert) {
            ev.prevent_default();
            return;
        }
        let it = ev.input_type();
        let sel = self.read_selection().map(|(a, h)| (a.min(h), a.max(h))).unwrap_or_else(|| self.sel_sorted());
        self.st.borrow_mut().sel = sel;
        match it.as_str() {
            "insertText" => {
                ev.prevent_default();
                let data = ev.data().unwrap_or_default();
                let (a, b) = sel;
                if data == " " || data.chars().any(|c| ".,;:!?".contains(c)) {
                    self.host.boundary();
                }
                self.replace(a, b, &data, None);
            }
            "insertReplacementText" => {
                ev.prevent_default();
                let data = ev
                    .data()
                    .or_else(|| ev.data_transfer().and_then(|d| d.get_data("text/plain").ok()))
                    .unwrap_or_default();
                let (a, b) = self.target_range(&ev).unwrap_or(sel);
                self.replace(a, b, &data, None);
            }
            "insertParagraph" | "insertLineBreak" => {
                ev.prevent_default();
                self.smart_newline();
            }
            "deleteContentBackward" => {
                ev.prevent_default();
                if self.smart_backspace() {
                    return;
                }
                let (a, b) = if sel.0 != sel.1 {
                    sel
                } else {
                    self.target_range(&ev).filter(|(x, y)| x != y).unwrap_or_else(|| {
                        let t = &self.st.borrow().text;
                        (prev_char(t, sel.0), sel.0)
                    })
                };
                self.replace(a, b, "", None);
            }
            "deleteContentForward" => {
                ev.prevent_default();
                let (a, b) = if sel.0 != sel.1 {
                    sel
                } else {
                    self.target_range(&ev).filter(|(x, y)| x != y).unwrap_or_else(|| {
                        let t = &self.st.borrow().text;
                        (sel.0, next_char(t, sel.0))
                    })
                };
                self.replace(a, b, "", None);
            }
            t if t.starts_with("delete") => {
                ev.prevent_default();
                let (a, b) = if sel.0 != sel.1 { sel } else { self.target_range(&ev).unwrap_or(sel) };
                if a != b {
                    self.host.boundary();
                    self.replace(a, b, "", None);
                }
            }
            "insertFromPaste" | "insertFromDrop" | "insertFromYank" | "insertFromPasteAsQuotation" => {
                ev.prevent_default();
                if let Some(t) = ev.data_transfer().and_then(|d| d.get_data("text/plain").ok()) {
                    let (a, b) = sel;
                    self.replace(a, b, &t.replace("\r\n", "\n"), None);
                }
            }
            "historyUndo" => {
                ev.prevent_default();
                self.undo();
            }
            "historyRedo" => {
                ev.prevent_default();
                self.redo();
            }
            "formatBold" => {
                ev.prevent_default();
                self.command(Command::Bold);
            }
            "formatItalic" => {
                ev.prevent_default();
                self.command(Command::Italic);
            }
            "formatUnderline" | "formatStrikeThrough" => {
                ev.prevent_default();
                self.command(Command::Strike);
            }
            _ => {}
        }
    }

    fn on_keydown(&self, ev: KeyboardEvent) {
        if ev.is_composing() {
            return;
        }
        if self.st.borrow().vim.is_some() && self.vim_key(&ev) {
            ev.prevent_default();
            return;
        }
        let m = ev.ctrl_key() || ev.meta_key();
        let key = ev.key();
        let shift = ev.shift_key();
        let cmd = match (m, shift, key.as_str()) {
            (true, false, "z") | (true, false, "Z") => Some(Command::Undo),
            (true, true, "z") | (true, true, "Z") | (true, false, "y") => Some(Command::Redo),
            (true, false, "b") => Some(Command::Bold),
            (true, false, "i") => Some(Command::Italic),
            (true, false, "k") => Some(Command::Link),
            (true, false, "`") => Some(Command::Code),
            (true, true, "X") | (true, true, "x") => Some(Command::Strike),
            (true, true, "H") | (true, true, "h") => Some(Command::Highlight),
            (true, true, "*") | (true, true, "8") => Some(Command::Bullet),
            (true, true, "&") | (true, true, "7") => Some(Command::Numbered),
            (true, true, "(") | (true, true, "9") => Some(Command::Task),
            (true, true, ">") | (true, true, ".") => Some(Command::Quote),
            (true, false, d) if d.len() == 1 && ("0123456".contains(d)) && ev.alt_key() => {
                Some(Command::Heading(d.parse().unwrap_or(0)))
            }
            (false, false, "Tab") | (false, true, "Tab") => {
                let (a, _) = self.sel_sorted();
                let in_table = syntax::table_tab(&self.st.borrow().text, a, shift);
                if let Some(k) = in_table {
                    ev.prevent_default();
                    self.apply_table_key(k);
                    return;
                }
                let (_, ls, le) = self.current_line(a);
                let line = self.st.borrow().text[ls..le].to_string();
                if syntax::list_marker(&line).is_some() || shift {
                    Some(if shift { Command::Outdent } else { Command::Indent })
                } else {
                    ev.prevent_default();
                    self.insert_text("\t");
                    return;
                }
            }
            (true, false, "Enter") => {
                ev.prevent_default();
                let (a, _) = self.sel_sorted();
                let idx = self.line_of(a);
                self.toggle_task_at_line(idx);
                return;
            }
            _ => None,
        };
        if let Some(c) = cmd {
            if self.st.borrow().readonly && !matches!(c, Command::Undo | Command::Redo) {
                return;
            }
            ev.prevent_default();
            self.command(c);
        }
    }

    fn on_copy(&self, ev: web_sys::ClipboardEvent, cut: bool) {
        let Some((a, h)) = self.read_selection() else { return };
        let (a, b) = (a.min(h), a.max(h));
        if a == b {
            return;
        }
        ev.prevent_default();
        let text = self.st.borrow().text[a..b].to_string();
        if let Some(dt) = ev.clipboard_data() {
            let _ = dt.set_data("text/plain", &text);
        }
        if cut && !self.st.borrow().readonly {
            self.host.boundary();
            self.replace(a, b, "", None);
        }
    }

    fn on_paste(&self, ev: web_sys::ClipboardEvent) {
        ev.prevent_default();
        if self.st.borrow().readonly {
            return;
        }
        let Some(dt) = ev.clipboard_data() else { return };
        let (a, b) = self.read_selection().map(|(a, h)| (a.min(h), a.max(h))).unwrap_or_else(|| self.sel_sorted());
        if let Some(files) = dt.files()
            && files.length() > 0 {
                let v: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
                self.host.insert_files(v, a);
                return;
            }
        if let Ok(t) = dt.get_data("text/plain") {
            self.host.boundary();
            self.replace(a, b, &t.replace("\r\n", "\n"), None);
            self.host.boundary();
        }
    }

    fn on_drop(&self, ev: web_sys::DragEvent) {
        ev.prevent_default();
        if self.st.borrow().readonly {
            return;
        }
        let Some(dt) = ev.data_transfer() else { return };
        let (a, b) = self.sel_sorted();
        if let Some(files) = dt.files()
            && files.length() > 0 {
                let v: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
                self.host.insert_files(v, a);
                return;
            }
        if let Ok(t) = dt.get_data("text/plain") {
            self.replace(a, b, &t.replace("\r\n", "\n"), None);
        }
    }

    fn on_mousedown(&self, ev: MouseEvent) {
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else { return };
        let line = target.closest(".ln").ok().flatten();
        let line_idx: Option<usize> = line.as_ref().and_then(|l| l.get_attribute("data-l")).and_then(|s| s.parse().ok());
        let line_active = line.as_ref().is_some_and(|l| l.class_list().contains("active"));
        if target.class_list().contains("tk") {
            ev.prevent_default();
            if let Some(i) = line_idx
                && !self.st.borrow().readonly {
                    self.toggle_task_at_line(i);
                }
            return;
        }
        if let Some(link_el) = target.closest("[data-link]").ok().flatten() {
            let follow = ev.ctrl_key() || ev.meta_key() || !line_active || self.st.borrow().readonly;
            if !follow {
                return;
            }
            let Some(i) = line_idx else { return };
            let Some(k) = link_el.get_attribute("data-link").and_then(|s| s.parse::<usize>().ok()) else { return };
            let text = self.st.borrow().text.clone();
            let lines: Vec<&str> = text.split('\n').collect();
            let blocks = syntax::classify(&lines);
            let host = self.host.clone();
            let styled = syntax::style_line(lines[i], &blocks[i], &move |t| host.link_exists(t));
            if let Some((_, _, t)) = styled.links.get(k) {
                ev.prevent_default();
                self.host.open_link(t, ev.ctrl_key() || ev.meta_key());
            }
        }
    }

    fn vim_key(&self, ev: &KeyboardEvent) -> bool {
        let text = self.st.borrow().text.clone();
        let sel = self.read_selection().unwrap_or(self.st.borrow().sel);
        let action = {
            let mut st = self.st.borrow_mut();
            let Some(v) = st.vim.as_mut() else { return false };
            v.key(ev, &text, sel)
        };
        let Some(action) = action else { return false };
        self.apply_mode_classes();
        match action {
            vim::Action::Consumed => {}
            vim::Action::Move(a, h) => self.set_selection(a, h),
            vim::Action::Edit { from, to, insert, caret } => {
                self.host.boundary();
                self.replace(from, to, &insert, Some((caret, caret)));
            }
            vim::Action::Undo => self.undo(),
            vim::Action::Redo => self.redo(),
            vim::Action::Yank(t) => crate::util::copy_to_clipboard(&t),
        }
        true
    }

    pub fn destroy(&self) {
        for (t, name, cb) in self.listeners.borrow_mut().drain(..) {
            let _ = t.remove_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
        }
        if let Some(p) = self.root.parent_element() {
            p.remove();
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        for (t, name, cb) in self.listeners.borrow_mut().drain(..) {
            let _ = t.remove_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
        }
    }
}

#[wasm_bindgen]
extern "C" {}
