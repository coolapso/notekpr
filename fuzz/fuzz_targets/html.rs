//! HTML in notes, whatever the bytes: a browser reads only allowed tags with
//! allowed attributes, and text, from what the sanitizer keeps (no comments,
//! doctypes or raw text), and sanitizing that again changes nothing. Telling
//! HTML blocks and tags apart in a line never panics.
#![no_main]
use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use libfuzzer_sys::fuzz_target;
use notekpr_editor::html;

struct Collect(RefCell<Vec<Token>>);

impl TokenSink for Collect {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        self.0.borrow_mut().push(token);
        TokenSinkResult::Continue
    }
}

fn tokens(s: &str) -> Vec<Token> {
    let tok = Tokenizer::new(Collect(RefCell::default()), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(s));
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.0.into_inner()
}

fn sanitize(s: &str) -> String {
    let q = |s: &str| {
        let mut o = String::new();
        notekpr_editor::syntax::escape(s, &mut o);
        o
    };
    let link = |href: &str, attrs: &str| format!("<a href=\"{}\"{attrs}>", q(href));
    let image = |src: &str, alt: &str, attrs: &str| {
        format!("<img src=\"{}\" alt=\"{}\"{attrs}>", q(src), q(alt))
    };
    html::sanitize(s, &html::Urls { link: &link, image: &image })
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let out = sanitize(text);
    for t in tokens(&out) {
        match t {
            Token::TagToken(tag) => {
                assert!(html::is_allowed(&tag.name, None), "{out:?}");
                for a in &tag.attrs {
                    assert!(html::is_allowed(&tag.name, Some(&a.name.local)), "{out:?}");
                }
            }
            Token::CommentToken(_) | Token::DoctypeToken(_) | Token::NullCharacterToken => {
                panic!("{out:?}")
            }
            _ => {}
        }
    }
    assert_eq!(sanitize(&out), out);
    for line in text.lines() {
        let _ = html::BlockKind::start(line);
        for i in 0..line.len() {
            if let Some(n) = html::tag_len(&line.as_bytes()[i..]) {
                assert!(i + n <= line.len());
            }
        }
    }
});
