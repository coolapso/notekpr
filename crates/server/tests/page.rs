//! The web client's files (`--web-dir`), and its page with whole addresses
//! for link previews (`page.rs`).

use crate::common::*;

const INDEX: &str = r#"<!doctype html>
<meta property="og:image" content="/social.png">
<meta property="og:image:width" content="1200">
<link rel="icon" href="/favicon.svg">
"#;

#[tokio::test]
async fn link_previews_get_the_public_url() {
    let web = tempfile::tempdir().unwrap();
    std::fs::write(web.path().join("index.html"), INDEX).unwrap();
    std::fs::write(web.path().join("social.png"), b"picture").unwrap();
    std::fs::create_dir(web.path().join("pkg")).unwrap();
    std::fs::write(web.path().join("pkg/app.js"), b"app").unwrap();
    let dir = web.path().to_path_buf();
    let srv = start_with(move |c| {
        c.web_dir = Some(dir);
        c.public_url = "https://notes.example.com/".into();
    })
    .await;
    let root = srv.base.trim_end_matches("/api/v1");
    let http = reqwest::Client::new();
    let get = |path: &str| http.get(format!("{root}{path}")).send();

    // The page, wherever the client goes, at its own name too.
    for path in ["/", "/index.html", "/v/abc/def", "/pkg", "/pkg/"] {
        let res = get(path).await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(
            res.headers()["content-type"],
            "text/html; charset=utf-8",
            "{path}"
        );
        assert_eq!(res.headers()["cache-control"], "no-cache", "{path}");
        let html = res.text().await.unwrap();
        assert!(
            html.contains(r#"content="https://notes.example.com/social.png""#),
            "{path}: {html}"
        );
        assert!(html.contains(r#"content="1200""#), "{path}");
        assert!(html.contains(r#"href="/favicon.svg""#), "{path}");
    }

    // The client's files as they are.
    let res = get("/social.png").await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "image/png");
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"picture");
    assert_eq!(
        get("/pkg/app.js").await.unwrap().text().await.unwrap(),
        "app"
    );

    // Only reading.
    let res = http.post(format!("{root}/")).send().await.unwrap();
    assert_eq!(res.status(), 405);
}
