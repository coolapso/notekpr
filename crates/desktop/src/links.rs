//! Where the window may go. It shows only the app's own files: anything
//! else opens in the default browser (web and mail addresses) or nowhere.

use url::Url;

/// The app's own pages: Tauri serves them at `tauri://localhost` (Linux,
/// macOS) or `http(s)://tauri.localhost` (Windows).
pub fn is_app(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost") && url.port().is_none(),
        _ => false,
    }
}

/// Addresses the default browser or mail program may open: web and mail
/// addresses, nothing that could start a program or open a file.
pub fn is_external(url: &Url) -> bool {
    match url.scheme() {
        "https" | "http" => url.host_str().is_some_and(|h| !h.is_empty()) && !is_app(url),
        "mailto" => true,
        _ => false,
    }
}

/// [`is_external`] for a string from the window.
pub fn external(url: &str) -> Option<Url> {
    Url::parse(url).ok().filter(is_external)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn app_pages() {
        assert!(is_app(&u("tauri://localhost/")));
        assert!(is_app(&u("tauri://localhost/v/abc?x=1")));
        assert!(is_app(&u("http://tauri.localhost/")));
        assert!(is_app(&u("https://tauri.localhost/settings")));
        assert!(!is_app(&u("tauri://evil/")));
        assert!(!is_app(&u("http://tauri.localhost:8080/")));
        assert!(!is_app(&u("https://notes.example.com/")));
        assert!(!is_app(&u("file:///etc/passwd")));
    }

    #[test]
    fn external_addresses() {
        assert!(external("https://example.com/a?b#c").is_some());
        assert!(external("http://example.com").is_some());
        assert!(external("mailto:someone@example.com").is_some());
        for bad in [
            "file:///home/me/.bashrc",
            "javascript:alert(1)",
            "data:text/html,hi",
            "smb://server/share",
            "ftp://example.com",
            "tauri://localhost/",
            "https://tauri.localhost/",
            "/relative",
            "not a url",
            "vscode://open?x",
        ] {
            assert!(external(bad).is_none(), "{bad}");
        }
    }
}
