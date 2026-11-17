//! The web client's page, its `index.html`: what the server answers for
//! every address that isn't one of the client's files (the client finds
//! its way from the address).
//!
//! The page tells link previews (chat apps, social networks) what to show,
//! with Open Graph meta tags, and those want whole addresses (`og:image`),
//! which only the server knows: its public URL. So each `content="/…"` in
//! the page gets the public URL in front here. The file is read for each
//! request, as a file server does, so a client built again shows at once.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// The page, as [`Page::respond`] serves it.
#[derive(Clone)]
pub struct Page {
    file: Arc<PathBuf>,
    /// The public URL, without a `/` at the end.
    origin: Arc<str>,
}

impl Page {
    /// `file` is the client's `index.html`; `public_url` is the server's
    /// ([`crate::config::Config::public_url`]).
    pub fn new(file: PathBuf, public_url: &str) -> Self {
        Page {
            file: Arc::new(file),
            origin: public_url.trim_end_matches('/').into(),
        }
    }

    pub async fn respond(&self) -> Response {
        match tokio::fs::read_to_string(&*self.file).await {
            Ok(html) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                absolute(&html, &self.origin),
            )
                .into_response(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                StatusCode::NOT_FOUND.into_response()
            }
            Err(e) => {
                tracing::error!(error = %e, "couldn't read the web client's page");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// `html` with `origin` in front of each `content="/…"` attribute's address.
pub fn absolute(html: &str, origin: &str) -> String {
    html.replace(
        r#"content="/"#,
        &format!(r#"content="{}/"#, escape_attribute(origin)),
    )
}

/// `s` as it may stand in a quoted HTML attribute. (The public URL is the
/// operator's, but a stray `"` mustn't break the page.)
fn escape_attribute(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_addresses_become_whole() {
        let html = r#"<meta property="og:image" content="/social.png">
<meta property="og:image:width" content="1200">
<link rel="icon" href="/favicon.svg">"#;
        assert_eq!(
            absolute(html, "https://notes.example.com"),
            r#"<meta property="og:image" content="https://notes.example.com/social.png">
<meta property="og:image:width" content="1200">
<link rel="icon" href="/favicon.svg">"#
        );
    }

    #[test]
    fn the_public_url_cannot_break_the_page() {
        assert_eq!(
            absolute(r#"<meta content="/a">"#, r#"https://x"><script>"#),
            r#"<meta content="https://x&quot;&gt;&lt;script&gt;/a">"#
        );
    }

    #[test]
    fn a_trailing_slash_is_not_doubled() {
        let page = Page::new(PathBuf::from("index.html"), "https://notes.example.com/");
        assert_eq!(&*page.origin, "https://notes.example.com");
    }
}
