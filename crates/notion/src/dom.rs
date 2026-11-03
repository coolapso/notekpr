//! A page of Notion's export as a tree of elements. Notion writes its HTML
//! well formed, so html5ever's tokenizer and a stack of open elements are
//! enough: no tree construction rules. Styles and scripts are left out.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, StartTag, TagToken, Token, TokenSink, TokenSinkResult,
    Tokenizer, TokenizerOpts,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|c| c == class)
    }

    /// The element children.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first element `f` accepts, this one or one inside it (depth
    /// first).
    pub fn find(&self, f: &dyn Fn(&Element) -> bool) -> Option<&Element> {
        if f(self) {
            return Some(self);
        }
        self.elements().find_map(|e| e.find(f))
    }

    /// Every element `f` accepts, this one and those inside it, in document
    /// order (also those inside one it accepted).
    pub fn find_all<'a>(&'a self, f: &dyn Fn(&Element) -> bool, out: &mut Vec<&'a Element>) {
        if f(self) {
            out.push(self);
        }
        for e in self.elements() {
            e.find_all(f, out);
        }
    }

    /// Its text, as it reads.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.text_into(&mut out);
        out
    }

    fn text_into(&self, out: &mut String) {
        for n in &self.children {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) if e.name == "br" => out.push('\n'),
                Node::Element(e) => e.text_into(out),
            }
        }
    }
}

/// Elements that never have content or an end tag.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// `html` as a tree, under an element named `#root`.
pub fn parse(html: &str) -> Element {
    let tok = Tokenizer::new(
        Sink {
            stack: RefCell::new(vec![Element {
                name: "#root".into(),
                ..Element::default()
            }]),
            skip: RefCell::new(None),
        },
        TokenizerOpts::default(),
    );
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&input);
    tok.end();
    let mut stack = tok.sink.stack.into_inner();
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().unwrap_or_default()
}

struct Sink {
    stack: RefCell<Vec<Element>>,
    /// The element whose content is being passed by (a style or script).
    skip: RefCell<Option<String>>,
}

/// The innermost open element goes into its parent.
fn close(stack: &mut Vec<Element>) {
    if let Some(e) = stack.pop()
        && let Some(parent) = stack.last_mut()
    {
        parent.children.push(Node::Element(e));
    }
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut skip = self.skip.borrow_mut();
        if let Some(name) = skip.as_deref() {
            if let TagToken(t) = &token
                && t.kind == EndTag
                && *t.name == *name
            {
                *skip = None;
            }
            return TokenSinkResult::Continue;
        }
        let mut stack = self.stack.borrow_mut();
        match token {
            CharacterTokens(text) => {
                let Some(top) = stack.last_mut() else {
                    return TokenSinkResult::Continue;
                };
                if let Some(Node::Text(t)) = top.children.last_mut() {
                    t.push_str(&text);
                } else {
                    top.children.push(Node::Text(text.to_string()));
                }
            }
            TagToken(tag) => {
                let name = tag.name.to_string();
                if tag.kind == StartTag {
                    let raw = match name.as_str() {
                        "script" => Some(RawKind::ScriptData),
                        "style" | "noscript" | "iframe" => Some(RawKind::Rawtext),
                        "title" | "textarea" => Some(RawKind::Rcdata),
                        _ => None,
                    };
                    if let Some(raw) = raw {
                        *skip = Some(name);
                        return TokenSinkResult::RawData(raw);
                    }
                    let void = tag.self_closing || VOID.contains(&name.as_str());
                    let e = Element {
                        name,
                        attrs: tag
                            .attrs
                            .iter()
                            .map(|a| (a.name.local.to_string(), a.value.to_string()))
                            .collect(),
                        children: Vec::new(),
                    };
                    if void {
                        if let Some(top) = stack.last_mut() {
                            top.children.push(Node::Element(e));
                        }
                    } else {
                        stack.push(e);
                    }
                } else if let Some(i) = stack.iter().rposition(|e| e.name == name)
                    && i > 0
                {
                    while stack.len() > i {
                        close(&mut stack);
                    }
                }
            }
            // Comments, doctypes, NUL characters, parse errors.
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_tree_and_drops_styles() {
        let root = parse(
            "<html><head><style>p > a { x: '<b>' }</style></head><body>\
             <p id=\"a\" class=\"x y\">One &amp; <strong>two</strong><br/>three</p>\
             <img src=\"i.png\"><script>if (a < b) {}</script><p>four</p></body></html>",
        );
        let p = root.find(&|e| e.attr("id") == Some("a")).unwrap();
        assert!(p.has_class("y"));
        assert_eq!(p.text(), "One & two\nthree");
        let mut ps = Vec::new();
        root.find_all(&|e| e.name == "p", &mut ps);
        assert_eq!(ps.len(), 2);
        assert!(root.find(&|e| e.name == "img").is_some());
        assert!(root.find(&|e| e.name == "style" || e.name == "b").is_none());
        assert!(!root.text().contains("if (a"));
    }

    #[test]
    fn stray_end_tags_are_ignored_and_open_ones_closed() {
        let root = parse("<div><p>a</span>b</div><ul><li>c");
        let div = root.find(&|e| e.name == "div").unwrap();
        assert_eq!(div.text(), "ab");
        assert_eq!(root.find(&|e| e.name == "li").unwrap().text(), "c");
    }
}
