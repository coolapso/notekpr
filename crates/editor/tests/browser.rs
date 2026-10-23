//! Browser tests of the editor's DOM view (`wasm-bindgen-test`, headless
//! Chromium): `task test:editor`, or
//!
//! ```sh
//! CARGO_TARGET_DIR=target-web CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test -p notekpr-editor --target wasm32-unknown-unknown --test browser
//! ```
//!
//! Events are dispatched the way the browser sends them (`beforeinput`,
//! composition events, DOM changes made "natively" by the test), against a
//! mock host that keeps its own copy of the text like the note's CRDT does.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use notekpr_core::textdiff::{self, Edit};
use notekpr_editor::syntax::Target;
use notekpr_editor::{Editor, Host, Mark, Mode, RemoteCursor};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
use web_sys::{Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default)]
struct MockHost {
    text: RefCell<String>,
    /// `(base, edits)` of every accepted edit.
    edits: RefCell<Vec<(String, Vec<Edit>)>>,
    history: RefCell<Vec<String>>,
    future: RefCell<Vec<String>>,
    /// Refuse edits that insert more than this many bytes.
    limit: Cell<usize>,
    boundaries: Cell<usize>,
    files_at: RefCell<Vec<usize>>,
    opened: RefCell<Vec<Target>>,
}

impl MockHost {
    fn new(text: &str) -> Rc<MockHost> {
        let h = MockHost::default();
        *h.text.borrow_mut() = text.to_string();
        h.limit.set(usize::MAX);
        Rc::new(h)
    }
}

impl Host for MockHost {
    fn edit(&self, base: &str, edits: &[Edit]) -> bool {
        let inserted: usize = edits
            .iter()
            .map(|e| match e {
                Edit::Insert { text, .. } => text.len(),
                _ => 0,
            })
            .sum();
        // Like the note host: the edits must be made on the current text.
        if inserted > self.limit.get() || *self.text.borrow() != base {
            return false;
        }
        assert!(notekpr_editor::edits_fit(base, edits));
        let new = textdiff::apply(base, edits);
        self.history.borrow_mut().push(base.to_string());
        self.future.borrow_mut().clear();
        *self.text.borrow_mut() = new;
        self.edits
            .borrow_mut()
            .push((base.to_string(), edits.to_vec()));
        true
    }
    fn text(&self) -> Option<String> {
        Some(self.text.borrow().clone())
    }
    fn undo(&self) -> Option<String> {
        let prev = self.history.borrow_mut().pop()?;
        let cur = std::mem::replace(&mut *self.text.borrow_mut(), prev.clone());
        self.future.borrow_mut().push(cur);
        Some(prev)
    }
    fn redo(&self) -> Option<String> {
        let next = self.future.borrow_mut().pop()?;
        let cur = std::mem::replace(&mut *self.text.borrow_mut(), next.clone());
        self.history.borrow_mut().push(cur);
        Some(next)
    }
    fn boundary(&self) {
        self.boundaries.set(self.boundaries.get() + 1);
    }
    fn link_exists(&self, target: &str) -> bool {
        target == "Exists"
    }
    fn open_link(&self, target: &Target, _new_tab: bool) {
        self.opened.borrow_mut().push(target.clone());
    }
    fn image_url(&self, target: &str) -> Option<String> {
        (target == "pic.png").then(|| {
            "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"
                .to_string()
        })
    }
    fn selection_changed(&self, _anchor: usize, _head: usize) {}
    fn insert_files(&self, _files: Vec<web_sys::File>, at: usize) {
        self.files_at.borrow_mut().push(at);
    }
}

fn document() -> web_sys::Document {
    web_sys::window().unwrap().document().unwrap()
}

struct Fixture {
    ed: Rc<Editor>,
    host: Rc<MockHost>,
    container: Element,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.ed.destroy();
        self.container.remove();
    }
}

fn setup(text: &str) -> Fixture {
    setup_with(text, false)
}

/// The app's layout of the remote cursors over the text (style.css), for
/// tests that look at where their names go.
fn cursor_style(f: &Fixture) {
    let style = document().create_element("style").unwrap();
    style.set_text_content(Some(
        ".ed-wrap { position: relative } .ed-cursors { position: absolute; inset: 0 } \
         .rc, .rc span { position: absolute } .rc span { bottom: 100%; white-space: nowrap }",
    ));
    f.container.append_child(&style).unwrap();
}

fn setup_with(text: &str, readonly: bool) -> Fixture {
    let container = document().create_element("div").unwrap();
    let _ = container.set_attribute("style", "width: 700px; font: 16px/1.5 sans-serif");
    document().body().unwrap().append_child(&container).unwrap();
    let host = MockHost::new(text);
    let ed = Editor::new(
        &container,
        text.to_string(),
        host.clone(),
        Mode::Live,
        readonly,
        false,
    );
    ed.focus();
    Fixture {
        ed,
        host,
        container,
    }
}

fn root(f: &Fixture) -> HtmlElement {
    f.ed.element().clone()
}

/// What the e2e tests read: every line's text without widgets.
fn dom_text(f: &Fixture) -> String {
    let kids = root(f).children();
    (0..kids.length())
        .map(|i| {
            let c: Element = kids
                .item(i)
                .unwrap()
                .clone_node_with_deep(true)
                .unwrap()
                .unchecked_into();
            let ws = c.query_selector_all("[data-w]").unwrap();
            for k in 0..ws.length() {
                let w = ws.item(k).unwrap();
                w.parent_node().unwrap().remove_child(&w).unwrap();
            }
            c.text_content().unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The editor, the DOM and the host agree on `want`.
fn assert_text(f: &Fixture, want: &str) {
    assert_eq!(dom_text(f), want, "DOM text");
    assert_eq!(f.ed.text(), want, "editor text");
    assert_eq!(*f.host.text.borrow(), want, "host text");
    let kids = root(f).child_nodes();
    for i in 0..kids.length() {
        let n = kids.item(i).unwrap();
        let el: Option<&Element> = n.dyn_ref();
        assert!(
            el.is_some_and(|e| e.class_list().contains("ln")),
            "child {i} of the editor is a line"
        );
    }
}

fn lines(f: &Fixture) -> Vec<Element> {
    let kids = root(f).children();
    (0..kids.length()).map(|i| kids.item(i).unwrap()).collect()
}

fn input(f: &Fixture, kind: &str, data: Option<&str>) -> web_sys::InputEvent {
    let init = web_sys::InputEventInit::new();
    init.set_input_type(kind);
    init.set_data(data);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::InputEvent::new_with_event_init_dict("beforeinput", &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
    ev
}

fn native_input(f: &Fixture, kind: &str, composing: bool) {
    let init = web_sys::InputEventInit::new();
    init.set_input_type(kind);
    init.set_bubbles(true);
    init.set_is_composing(composing);
    let ev = web_sys::InputEvent::new_with_event_init_dict("input", &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
}

fn type_str(f: &Fixture, s: &str) {
    for c in s.chars() {
        let ev = input(f, "insertText", Some(&c.to_string()));
        assert!(ev.default_prevented());
    }
}

fn enter(f: &Fixture) {
    input(f, "insertParagraph", None);
}

fn key(f: &Fixture, k: &str, ctrl: bool, shift: bool) {
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(k);
    init.set_ctrl_key(ctrl);
    init.set_shift_key(shift);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
}

fn composition(f: &Fixture, kind: &str, data: &str) {
    let init = web_sys::CompositionEventInit::new();
    init.set_data(data);
    init.set_bubbles(true);
    let ev = web_sys::CompositionEvent::new_with_event_init_dict(kind, &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
}

/// The first text node of line `i`.
fn text_node(f: &Fixture, i: usize) -> web_sys::Text {
    let w = document()
        .create_tree_walker_with_what_to_show(&lines(f)[i], 4)
        .unwrap();
    w.next_node().unwrap().unwrap().unchecked_into()
}

fn caret_at(f: &Fixture, offset: usize) {
    f.ed.set_selection(offset, offset);
}

async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    wasm_bindgen_futures::JsFuture::from(p).await.unwrap();
}

async fn frame() {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .request_animation_frame(&resolve)
            .unwrap();
    });
    wasm_bindgen_futures::JsFuture::from(p).await.unwrap();
}

// ---------------------------------------------------------------- tests

#[wasm_bindgen_test]
fn typing_renders_only_the_line_it_changes() {
    let f = setup("# Title\nfirst\nsecond **bold**\nthird");
    let before = lines(&f);
    caret_at(&f, "# Title\nfirst".len());
    type_str(&f, " 日本 x");
    assert_text(&f, "# Title\nfirst 日本 x\nsecond **bold**\nthird");
    let after = lines(&f);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert!(a.is_same_node(Some(b)), "line elements are kept");
    }
    // Syntax markers of the caret's line show, the others' don't.
    assert!(after[1].class_list().contains("active"));
    assert!(!after[2].class_list().contains("active"));
    assert!(after[2].query_selector(".b").unwrap().is_some());
    // A space starts a new undo group.
    assert!(f.host.boundaries.get() >= 2);
}

#[wasm_bindgen_test]
fn enter_continues_and_ends_lists() {
    let f = setup("");
    type_str(&f, "- milk");
    enter(&f);
    type_str(&f, "eggs");
    enter(&f);
    enter(&f);
    type_str(&f, "Thanks!");
    // A blank line ends the list: text right under it would continue the item.
    assert_text(&f, "- milk\n- eggs\n\nThanks!");
    assert!(lines(&f)[0].class_list().contains("li"));
    assert!(!lines(&f)[3].class_list().contains("lazy"));
    // Ordered lists and tasks continue too; backspace after a marker removes it.
    let f = setup("1. one");
    caret_at(&f, 6);
    enter(&f);
    type_str(&f, "two");
    enter(&f);
    assert_text(&f, "1. one\n2. two\n3. ");
    input(&f, "deleteContentBackward", None);
    assert_text(&f, "1. one\n2. two\n");
}

#[wasm_bindgen_test]
fn tables_are_built_with_enter_and_tab() {
    let f = setup("");
    type_str(&f, "| Item | Cost |");
    enter(&f);
    type_str(&f, "| :--- | ---: |");
    enter(&f);
    type_str(&f, "Bread");
    key(&f, "Tab", false, false);
    type_str(&f, "3");
    key(&f, "Tab", false, false);
    type_str(&f, "Milk");
    key(&f, "Tab", false, false);
    type_str(&f, "2");
    assert_text(
        &f,
        "| Item | Cost |\n| :--- | ---: |\n| Bread | 3 |\n| Milk | 2 |",
    );
    let ls = lines(&f);
    assert!(ls.iter().all(|l| l.class_list().contains("table")));
    assert!(ls[0].class_list().contains("th") && ls[1].class_list().contains("tdl"));
    assert_eq!(ls[2].query_selector_all(".td").unwrap().length(), 2);
    // Shift+Tab goes back to the start of the previous cell; Enter on an
    // empty row ends the table.
    key(&f, "Tab", false, true);
    type_str(&f, "!");
    enter(&f);
    enter(&f);
    type_str(&f, "Total: 5");
    assert_text(
        &f,
        "| Item | Cost |\n| :--- | ---: |\n| Bread | 3 |\n| !Milk | 2 |\n\nTotal: 5",
    );
}

#[wasm_bindgen_test]
async fn a_composition_merges_with_text_that_arrived_meanwhile() {
    let f = setup("Shopping: milk\neggs");
    caret_at(&f, 0);
    let composing = text_node(&f, 0);
    composition(&f, "compositionstart", "");
    // Remote text waits; refreshes too: the composition's DOM stays the browser's.
    let remote = "Shopping: milk\neggs remote".to_string();
    *f.host.text.borrow_mut() = remote.clone();
    f.ed.set_text(remote.clone(), false);
    f.ed.refresh();
    assert!(!dom_text(&f).contains("remote"));
    composing.set_data(&format!("かな{}", composing.data()));
    native_input(&f, "insertCompositionText", true);
    assert!(text_node(&f, 0).is_same_node(Some(&composing)));
    composition(&f, "compositionend", "かな");
    sleep(20).await;
    assert_text(&f, "かなShopping: milk\neggs remote");
    // The host got the composition as an edit of the text it had.
    let edits = f.host.edits.borrow();
    let (base, e) = edits.last().unwrap();
    assert_eq!(base, &remote);
    assert_eq!(textdiff::apply(base, e), "かなShopping: milk\neggs remote");
}

#[wasm_bindgen_test]
async fn a_cancelled_composition_applies_the_waiting_text() {
    let f = setup("one\ntwo");
    caret_at(&f, 3);
    composition(&f, "compositionstart", "");
    *f.host.text.borrow_mut() = "one\ntwo again".into();
    f.ed.set_text("one\ntwo again".into(), false);
    composition(&f, "compositionend", "");
    sleep(20).await;
    assert_text(&f, "one\ntwo again");
}

/// Safari ends a composition before it replaces the composed text:
/// `compositionend`, then `deleteCompositionText` and `insertFromComposition`
/// (not marked as composing) [W-L3].
#[wasm_bindgen_test]
async fn safari_composition_events_are_left_to_the_browser() {
    let f = setup("tea\ncake");
    caret_at(&f, 3);
    let t = text_node(&f, 0);
    composition(&f, "compositionstart", "");
    t.set_data("teaちゃ");
    native_input(&f, "insertCompositionText", true);
    composition(&f, "compositionend", "茶");
    let ev = input(&f, "deleteCompositionText", None);
    assert!(
        !ev.default_prevented(),
        "deleteCompositionText is the browser's"
    );
    t.set_data("tea");
    native_input(&f, "deleteCompositionText", false);
    let ev = input(&f, "insertFromComposition", Some("茶"));
    assert!(
        !ev.default_prevented(),
        "insertFromComposition is the browser's"
    );
    t.set_data("tea茶");
    native_input(&f, "insertFromComposition", false);
    sleep(20).await;
    assert_text(&f, "tea茶\ncake");
    // The key that ended the composition (keyCode 229) is not a shortcut.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Enter");
    init.set_key_code(229);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    root(&f).dispatch_event(&ev).unwrap();
    assert!(!ev.default_prevented());
    assert_text(&f, "tea茶\ncake");
}

#[wasm_bindgen_test]
async fn no_full_rebuild_at_the_end_of_a_composition() {
    let text: String = (0..50).map(|i| format!("line {i}\n")).collect();
    let f = setup(&text);
    let before = lines(&f);
    caret_at(&f, "line 0\nline 1\nline".len());
    composition(&f, "compositionstart", "");
    let t = text_node(&f, 2);
    t.set_data("line 2あ");
    composition(&f, "compositionend", "あ");
    sleep(20).await;
    assert!(dom_text(&f).starts_with("line 0\nline 1\nline 2あ\nline 3"));
    let after = lines(&f);
    let kept = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| a.is_same_node(Some(b)))
        .count();
    assert_eq!(kept, before.len(), "every line element is kept");
}

/// Every offset maps to a DOM position and back: typing there inserts there.
#[wasm_bindgen_test]
fn selection_maps_between_the_dom_and_offsets() {
    let text = "# Wel**come** 👋\n\n- [ ] task [[Exists]] and [[Nope|alias]]\n![[pic.png]]\n| a | b |\n| - | - |\n| 日 | x |\n```rust title=\"a b.rs\" hl=1\nlet x = \"é\";\n    let y = 1;\n\n\tz\n```\nend `c` ==hl==";
    let f = setup(text);
    assert!(
        root(&f).query_selector(".w img").unwrap().is_some(),
        "image widget"
    );
    // Indentation guides, also on the blank line between indented ones.
    assert_eq!(root(&f).query_selector_all(".igs").unwrap().length(), 3);
    let offsets: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    for o in offsets {
        caret_at(&f, o);
        assert_eq!(f.ed.selection(), (o, o));
        type_str(&f, "X");
        let mut want = text.to_string();
        want.insert(o, 'X');
        assert_eq!(dom_text(&f), want, "typing at {o}");
        input(&f, "deleteContentBackward", None);
        assert_eq!(dom_text(&f), text, "deleting at {o}");
    }
    assert_text(&f, text);
}

#[wasm_bindgen_test]
fn undo_and_redo() {
    let f = setup("abc");
    caret_at(&f, 3);
    type_str(&f, "de");
    assert_text(&f, "abcde");
    key(&f, "z", true, false);
    assert_text(&f, "abcd");
    key(&f, "z", true, false);
    assert_text(&f, "abc");
    key(&f, "z", true, true);
    assert_text(&f, "abcd");
    // The caret is where the change was.
    assert_eq!(f.ed.selection(), (4, 4));
    type_str(&f, "!");
    assert_text(&f, "abcd!");
}

#[wasm_bindgen_test]
fn remote_edits_while_the_caret_is_elsewhere() {
    let text: String = (0..10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let f = setup(&text);
    let at = text.find("line 8").unwrap() + 3;
    caret_at(&f, at);
    let before = lines(&f);
    // Someone inserts a line near the top and edits another one.
    let remote = text
        .replace("line 1\n", "line 1\nnew line\n")
        .replace("line 5", "line five");
    *f.host.text.borrow_mut() = remote.clone();
    f.ed.set_text(remote.clone(), false);
    assert_eq!(dom_text(&f), remote);
    let after = lines(&f);
    assert_eq!(after.len(), before.len() + 1);
    // Only the touched lines got new elements.
    assert!(after[9].is_same_node(Some(&before[8])));
    assert!(after[0].is_same_node(Some(&before[0])));
    // The caret moved with its text: typing lands in "line 8".
    type_str(&f, "X");
    assert_text(&f, &remote.replace("line 8", "linXe 8"));
}

#[wasm_bindgen_test]
fn stray_nodes_after_native_edits_are_removed() {
    let f = setup("one\ntwo");
    let r = root(&f);
    // The browser wrapped text in its own markup and added a block of its own.
    let b = document().create_element("b").unwrap();
    b.set_text_content(Some("t"));
    let first = lines(&f)[1].clone();
    let t = text_node(&f, 1);
    t.set_data("wo");
    first.insert_before(&b, Some(&t)).unwrap();
    let div = document().create_element("div").unwrap();
    div.set_inner_html("three<br>");
    r.append_child(&div).unwrap();
    native_input(&f, "insertText", false);
    assert_text(&f, "one\ntwo\nthree");
    assert!(
        r.query_selector("b").unwrap().is_none(),
        "no browser markup left"
    );
    // A text node straight in the editor element becomes a line of its own.
    r.append_child(&document().create_text_node("four"))
        .unwrap();
    native_input(&f, "insertText", false);
    assert_text(&f, "one\ntwo\nthree\nfour");
}

#[wasm_bindgen_test]
fn refused_edits_restore_the_host_text() {
    let f = setup("short");
    f.host.limit.set(10);
    caret_at(&f, 5);
    let ev = {
        let dt = web_sys::DataTransfer::new().unwrap();
        dt.set_data("text/plain", &"x".repeat(100)).unwrap();
        let init = web_sys::ClipboardEventInit::new();
        init.set_clipboard_data(Some(&dt));
        init.set_bubbles(true);
        init.set_cancelable(true);
        web_sys::ClipboardEvent::new_with_event_init_dict("paste", &init).unwrap()
    };
    root(&f).dispatch_event(&ev).unwrap();
    assert_text(&f, "short");
    type_str(&f, "!");
    assert_text(&f, "short!");
}

#[wasm_bindgen_test]
fn commands_and_shortcuts() {
    let f = setup("word and more\nsecond");
    f.ed.set_selection(0, 4);
    key(&f, "b", true, false);
    assert_text(&f, "**word** and more\nsecond");
    f.ed.command(notekpr_editor::Command::Heading(2));
    assert_text(&f, "## **word** and more\nsecond");
    f.ed.set_selection(21, 27);
    f.ed.command(notekpr_editor::Command::Task);
    assert_text(&f, "## **word** and more\n- [ ] second");
    key(&f, "Enter", true, false);
    assert_text(&f, "## **word** and more\n- [x] second");
    // Source mode only changes classes.
    let before = lines(&f);
    f.ed.set_mode(Mode::Source);
    assert!(root(&f).class_list().contains("source"));
    assert!(lines(&f)[0].is_same_node(Some(&before[0])));
    // Ctrl+Shift+`: a code block, the caret on its fence for the language.
    let f = setup("x");
    caret_at(&f, 1);
    key(&f, "~", true, true);
    assert_text(&f, "x\n```\n\n```\n");
    assert_eq!(f.ed.selection(), (5, 5));
    // Enter after a fence nothing closes closes it.
    let f = setup("```py");
    caret_at(&f, 5);
    enter(&f);
    assert_text(&f, "```py\n\n```");
    assert_eq!(f.ed.selection(), (6, 6));
}

/// A keydown with the key it types and the physical key it is.
fn key_at(f: &Fixture, k: &str, code: &str, ctrl: bool, meta: bool, alt: bool) -> bool {
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(k);
    init.set_code(code);
    init.set_ctrl_key(ctrl);
    init.set_meta_key(meta);
    init.set_alt_key(alt);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
    ev.default_prevented()
}

#[wasm_bindgen_test]
fn heading_shortcuts_on_macs_and_elsewhere() {
    let f = setup("title\nmore");
    caret_at(&f, 0);
    // A Mac: ⌘+Option+2 types "™".
    assert!(key_at(&f, "™", "Digit2", false, true, true));
    assert_text(&f, "## title\nmore");
    // Elsewhere: Ctrl+Alt+1.
    assert!(key_at(&f, "1", "Digit1", true, false, true));
    assert_text(&f, "# title\nmore");
    // AltGr+0 (Ctrl+Alt on Windows) types "}" on some layouts: no heading.
    assert!(!key_at(&f, "}", "Digit0", true, false, true));
    assert_text(&f, "# title\nmore");
}

#[wasm_bindgen_test]
fn vim_keys() {
    let f = setup("one two\nthree");
    f.ed.set_vim(true);
    caret_at(&f, 0);
    // Normal mode: typing is not text.
    assert!(input(&f, "insertText", Some("z")).default_prevented());
    assert_text(&f, "one two\nthree");
    for k in ["w", "x"] {
        key(&f, k, false, false);
    }
    assert_text(&f, "one wo\nthree");
    key(&f, "d", false, false);
    key(&f, "d", false, false);
    assert_text(&f, "three");
    key(&f, "u", false, false);
    assert_text(&f, "one wo\nthree");
    key(&f, "i", false, false);
    assert!(root(&f).class_list().contains("vim-insert"));
    type_str(&f, "!");
    key(&f, "Escape", false, false);
    assert!(root(&f).class_list().contains("vim-normal"));
    assert_eq!(f.ed.text().matches('!').count(), 1);
}

#[wasm_bindgen_test]
fn readonly_ignores_input() {
    let f = setup_with("read me", true);
    let ev = input(&f, "insertText", Some("x"));
    assert!(ev.default_prevented());
    key(&f, "b", true, false);
    assert_text(&f, "read me");
    assert_eq!(
        root(&f).get_attribute("contenteditable").as_deref(),
        Some("false")
    );
}

#[wasm_bindgen_test]
fn task_boxes_and_links() {
    let f = setup("- [ ] buy [ ] boxes\nsee [[Exists]]");
    let tk = lines(&f)[0].query_selector(".tk").unwrap().unwrap();
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    tk.dispatch_event(&ev).unwrap();
    assert_text(&f, "- [x] buy [ ] boxes\nsee [[Exists]]");
    // A link on a line without the caret opens on a plain click.
    caret_at(&f, 0);
    let link = lines(&f)[1].query_selector("[data-link]").unwrap().unwrap();
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    link.dispatch_event(&ev).unwrap();
    assert_eq!(*f.host.opened.borrow(), vec![Target::Wiki("Exists".into())]);
}

/// A code block's header is a widget: the text stays the source, its copy
/// button copies without editing, and the caret in the block shows both fences.
#[wasm_bindgen_test]
fn code_block_headers() {
    let text = "intro\n```sh title=deploy.sh hl=2\nls\npwd\n```\nafter";
    let f = setup(text);
    let ls = lines(&f);
    let head = ls[1].query_selector(".code-head[data-w]").unwrap().unwrap();
    assert_eq!(
        head.get_attribute("contenteditable").as_deref(),
        Some("false")
    );
    let title = head.query_selector(".code-title").unwrap().unwrap();
    assert_eq!(title.text_content().as_deref(), Some("deploy.sh"));
    assert!(ls[1].class_list().contains("head"));
    assert!(ls[3].class_list().contains("chl"), "hl=2");
    assert!(!ls[2].class_list().contains("chl"));
    assert!(
        ls[2].query_selector(".igs").unwrap().is_none(),
        "no indentation"
    );
    let g = setup("```py vl=false\nif x:\n    y()\n```");
    assert!(
        root(&g).query_selector(".igs").unwrap().is_none(),
        "vl=false"
    );
    assert_text(&f, text);

    // The caret in the code shows both fences; elsewhere neither.
    caret_at(&f, "intro\n```sh title=deploy.sh hl=2\nl".len());
    let ls = lines(&f);
    assert!(ls[1].class_list().contains("active") && ls[4].class_list().contains("active"));
    caret_at(&f, text.len());
    let ls = lines(&f);
    assert!(!ls[1].class_list().contains("active") && !ls[4].class_list().contains("active"));

    let copy = ls[1].query_selector(".code-copy").unwrap().unwrap();
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    copy.dispatch_event(&ev).unwrap();
    assert!(ev.default_prevented(), "the caret stays where it was");
    assert!(copy.class_list().contains("copied"));
    assert_text(&f, text);
    assert_eq!(f.ed.selection(), (text.len(), text.len()));

    // Editing the options updates the header.
    caret_at(&f, "intro\n```sh title=deploy".len());
    type_str(&f, "2");
    let title = lines(&f)[1].query_selector(".code-title").unwrap().unwrap();
    assert_eq!(title.text_content().as_deref(), Some("deploy2.sh"));
}

/// While the mouse button is down, lines don't start showing their
/// Markdown (that moves their text under a drag); on release they do.
#[wasm_bindgen_test]
fn markers_show_when_the_mouse_is_released() {
    let f = setup("- one\n- two\nthree");
    caret_at(&f, "- one\n- two\nthr".len());
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_button(0);
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    lines(&f)[1].dispatch_event(&ev).unwrap();
    // Where the press puts the caret.
    caret_at(&f, "- one\n- t".len());
    assert!(
        !lines(&f)[1].class_list().contains("active"),
        "not while pressed"
    );
    assert!(lines(&f)[2].class_list().contains("active"));
    let up = web_sys::Event::new("pointerup").unwrap();
    document().dispatch_event(&up).unwrap();
    assert!(lines(&f)[1].class_list().contains("active"), "on release");
    assert!(!lines(&f)[2].class_list().contains("active"));
}

fn point_of(f: &Fixture, line: usize, utf16: u32) -> (i32, i32) {
    let t = text_node(f, line);
    let r = document().create_range().unwrap();
    r.set_start(&t, utf16).unwrap();
    r.set_end(&t, utf16).unwrap();
    let rect = r.get_bounding_client_rect();
    (
        (rect.left() + 1.0) as i32,
        (rect.top() + rect.height() / 2.0) as i32,
    )
}

fn drop_at(f: &Fixture, (x, y): (i32, i32), dt: &web_sys::DataTransfer) {
    let init = web_sys::DragEventInit::new();
    init.set_client_x(x);
    init.set_client_y(y);
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_data_transfer(Some(dt));
    let ev = web_sys::DragEvent::new_with_event_init_dict("drop", &init).unwrap();
    root(f).dispatch_event(&ev).unwrap();
}

/// Dropped text and files land at the drop point, not at the caret [W-L7].
#[wasm_bindgen_test]
fn drops_land_where_they_are_dropped() {
    let f = setup("hello world\nsecond line");
    caret_at(&f, 0);
    let dt = web_sys::DataTransfer::new().unwrap();
    dt.set_data("text/plain", "XY").unwrap();
    drop_at(&f, point_of(&f, 1, 3), &dt);
    assert_text(&f, "hello world\nsecXYond line");
    let dt = web_sys::DataTransfer::new().unwrap();
    let parts = js_sys::Array::of1(&"data".into());
    let file = web_sys::File::new_with_str_sequence(&parts, "a.txt").unwrap();
    dt.items().add_with_file(&file).unwrap();
    drop_at(&f, point_of(&f, 0, 5), &dt);
    assert_eq!(*f.host.files_at.borrow(), vec![5]);
    // Their links go to that point even if the text changed meanwhile.
    let anchor = f.ed.anchor(5);
    caret_at(&f, 0);
    type_str(&f, ">> ");
    f.ed.insert_at(&anchor, "![[a.txt]]");
    assert_text(&f, ">> hello![[a.txt]] world\nsecXYond line");
}

#[wasm_bindgen_test]
async fn remote_cursors_are_drawn() {
    let f = setup("one\ntwo");
    cursor_style(&f);
    f.ed.set_remote_cursors(vec![RemoteCursor {
        name: "Bob".into(),
        color: "#c00".into(),
        anchor: 5,
        head: 5,
    }]);
    frame().await;
    frame().await;
    let rc = f.container.query_selector_all(".rc").unwrap();
    assert_eq!(rc.length(), 1);
    let below = |f: &Fixture| {
        let rc = f.container.query_selector(".rc").unwrap().unwrap();
        rc.class_list().contains("below")
    };
    assert!(!below(&f), "the name is above the cursor");
    // On the first line the name goes under it: the text's box clips what
    // is above it.
    f.ed.set_remote_cursors(vec![RemoteCursor {
        name: "Bob".into(),
        color: "#c00".into(),
        anchor: 1,
        head: 1,
    }]);
    frame().await;
    frame().await;
    assert!(below(&f), "the name is under a cursor on the first line");
    f.ed.set_remote_cursors(vec![RemoteCursor {
        name: "Bob".into(),
        color: "#c00".into(),
        anchor: 5,
        head: 5,
    }]);
    // They move with remote text inserted before them.
    *f.host.text.borrow_mut() = "zero\none\ntwo".into();
    f.ed.set_text("zero\none\ntwo".into(), false);
    frame().await;
    frame().await;
    assert_eq!(f.container.query_selector_all(".rc").unwrap().length(), 1);
}

#[wasm_bindgen_test]
async fn a_remote_cursors_name_stays_within_the_text() {
    let long = "w".repeat(300);
    let f = setup(&format!("one\n{long}"));
    cursor_style(&f);
    let at = |head: usize| RemoteCursor {
        name: "Bob".into(),
        color: "#c00".into(),
        anchor: head,
        head,
    };
    let end = |f: &Fixture| {
        let rc = f.container.query_selector(".rc").unwrap().unwrap();
        rc.class_list().contains("end")
    };
    // At the end of the widest line the name goes left of the cursor:
    // sticking out, it would make the text scroll sideways.
    f.ed.set_remote_cursors(vec![at(4 + long.len())]);
    frame().await;
    frame().await;
    assert!(
        end(&f),
        "the name is left of a cursor at the end of the text"
    );
    f.ed.set_remote_cursors(vec![at(4)]);
    frame().await;
    frame().await;
    assert!(!end(&f), "and right of it elsewhere");
}

#[wasm_bindgen_test]
async fn marks_are_drawn_behind_the_text_and_follow_it() {
    let f = setup("one two\nthree");
    f.ed.set_marks(vec![Mark {
        start: 4,
        end: 13,
        class: "c".into(),
        id: "m1".into(),
    }]);
    frame().await;
    frame().await;
    // Across two lines: a highlight per line, behind the text, and the
    // text itself untouched.
    let hl = f.container.query_selector_all(".ed-marks .hl.c").unwrap();
    assert_eq!(hl.length(), 2);
    assert_text(&f, "one two\nthree");
    assert_eq!(f.ed.marks_at(6), ["m1"]);
    assert!(f.ed.marks_at(1).is_empty());
    // Typed before it: it moves along.
    caret_at(&f, 0);
    type_str(&f, ">");
    frame().await;
    assert_eq!(f.ed.mark_range("m1"), Some((5, 14)));
    // Its text deleted: gone.
    f.ed.set_text(">one".into(), false);
    *f.host.text.borrow_mut() = ">one".into();
    frame().await;
    frame().await;
    assert_eq!(f.ed.mark_range("m1"), None);
    assert_eq!(
        f.container
            .query_selector_all(".ed-marks .hl")
            .unwrap()
            .length(),
        0
    );
}

/// Nothing keeps the editor (or its host) alive once it is destroyed [W-L5].
#[wasm_bindgen_test]
async fn destroying_releases_listeners_and_timers() {
    let f = setup("text");
    composition(&f, "compositionstart", "");
    composition(&f, "compositionend", "");
    f.ed.set_remote_cursors(vec![RemoteCursor {
        name: "Bob".into(),
        color: "#c00".into(),
        anchor: 1,
        head: 1,
    }]);
    let r = root(&f);
    let container = f.container.clone();
    let host = f.host.clone();
    let weak = Rc::downgrade(&f.ed);
    f.ed.destroy();
    assert!(f.ed.is_destroyed());
    assert!(!container.contains(Some(&r)));
    drop(f);
    // No listener, observer or timer keeps the editor or its host alive.
    assert!(weak.upgrade().is_none());
    assert_eq!(Rc::strong_count(&host), 1);
    // Events on the detached element and pending timers do nothing.
    let init = web_sys::InputEventInit::new();
    init.set_input_type("insertText");
    init.set_data(Some("x"));
    init.set_cancelable(true);
    let ev = web_sys::InputEvent::new_with_event_init_dict("beforeinput", &init).unwrap();
    r.dispatch_event(&ev).unwrap();
    assert!(!ev.default_prevented());
    sleep(20).await;
    frame().await;
    assert_eq!(*host.text.borrow(), "text");
}

/// Typing latency on a large note (printed; the bound only catches
/// pathological regressions).
#[wasm_bindgen_test]
fn typing_on_a_large_note() {
    let block = [
        "## Section",
        "Some text with **bold**, *italic*, `code` and a [[Link]] plus #tag.",
        "- item one",
        "- [ ] task two",
        "> a quote line",
        "| a | b |",
        "| --- | --- |",
        "| 1 | x |",
        "plain paragraph line with some more words to make it longer than usual",
        "",
    ];
    let text: String = (0..5000)
        .map(|i| block[i % block.len()])
        .collect::<Vec<_>>()
        .join("\n");
    let f = setup(&text);
    let perf = web_sys::window().unwrap().performance().unwrap();
    let mid = text.len() / 2;
    let at = mid + text[mid..].find('\n').unwrap();
    caret_at(&f, at);
    let mut times = vec![];
    for _ in 0..40 {
        let t0 = perf.now();
        type_str(&f, "x");
        let _ = root(&f).offset_height();
        times.push(perf.now() - t0);
    }
    let mut enter_times = vec![];
    for _ in 0..10 {
        let t0 = perf.now();
        enter(&f);
        let _ = root(&f).offset_height();
        enter_times.push(perf.now() - t0);
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (t, e) = (median(&mut times), median(&mut enter_times));
    console_log!("5000 lines: type a character {t:.2} ms, Enter {e:.2} ms (median)");
    assert_eq!(dom_text(&f).len(), text.len() + 40 + 10);
    assert!(t < 250.0 && e < 250.0, "typing took {t} ms, Enter {e} ms");
}

/// An HTML block shows a preview on its last line: a widget, so the text
/// stays the source. Nothing in it reaches outside it, it follows edits, the
/// whole block is active while the caret is in it (CSS shows the source
/// then, the preview otherwise), its links open through the host, and a
/// click anywhere else in it puts the caret in the block.
#[wasm_bindgen_test]
fn html_block_previews() {
    let text = "intro\n<div align=\"center\">\n<b>bold</b> <a href=\"Exists\">x</a> <img src=\"pic.png\" width=\"20\" onerror=\"x()\">\n</div>\n\nafter <kbd>K</kbd>";
    let f = setup(text);
    let ls = lines(&f);
    assert_eq!(ls.len(), 6);
    assert!(ls[1].query_selector(".hw").unwrap().is_none());
    assert!(ls[2].query_selector(".hw").unwrap().is_none());
    let w = ls[3].query_selector(".hw[data-w]").unwrap().unwrap();
    assert_eq!(w.get_attribute("contenteditable").as_deref(), Some("false"));
    assert!(w.query_selector("div[align=center] > b").unwrap().is_some());
    let img = w.query_selector("img").unwrap().unwrap();
    assert!(
        img.get_attribute("src")
            .unwrap()
            .starts_with("data:image/gif")
    );
    assert_eq!(img.get_attribute("width").as_deref(), Some("20"));
    assert!(!img.has_attribute("onerror"));
    assert!(
        ls[5].query_selector(".ht").unwrap().is_some(),
        "inline tags"
    );
    assert_text(&f, text);

    // A link in the preview opens; a click elsewhere in it goes into the block.
    caret_at(&f, text.len());
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    let a = w.query_selector("[data-href]").unwrap().unwrap();
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    a.dispatch_event(&ev).unwrap();
    assert!(ev.default_prevented());
    assert_eq!(*f.host.opened.borrow(), vec![Target::Wiki("Exists".into())]);
    assert_eq!(f.ed.selection(), (text.len(), text.len()));
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousedown", &init).unwrap();
    img.dispatch_event(&ev).unwrap();
    assert!(ev.default_prevented());
    assert_eq!(f.ed.selection(), (6, 6), "the start of the block");
    let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict("click", &init).unwrap();
    img.dispatch_event(&ev).unwrap();
    assert!(ev.default_prevented(), "no default action in the preview");

    // The caret anywhere in the block makes all of it active.
    caret_at(&f, "intro\n<div".len());
    let ls = lines(&f);
    assert!((1..4).all(|i| ls[i].class_list().contains("active")));
    assert!(!ls[0].class_list().contains("active") && !ls[4].class_list().contains("active"));
    // Typing in it updates the preview.
    caret_at(&f, "intro\n<div align=\"center\">\n<b>bold".len());
    type_str(&f, "er");
    let w = lines(&f)[3].query_selector(".hw").unwrap().unwrap();
    assert_eq!(
        w.query_selector("b")
            .unwrap()
            .unwrap()
            .text_content()
            .as_deref(),
        Some("bolder")
    );

    // Whatever the HTML closes or opens, it stays in its widget.
    let hostile = "<div>\n</span></div></div><div class=\"ln\">x</div><p>y</td></table><table><tr><td>z\n</div>\n\nend";
    let g = setup(hostile);
    assert_eq!(lines(&g).len(), 5);
    let w = lines(&g)[2].query_selector(".hw").unwrap().unwrap();
    assert!(w.text_content().unwrap().contains('z'));
    assert!(w.query_selector(".ln").unwrap().is_none());
    assert_text(&g, hostile);
}
