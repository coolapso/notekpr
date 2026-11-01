//! `@today`, `@tomorrow` and `@yesterday` become dates when the CLI or the
//! gateway writes a note (the Markdown folder leaves them: tests/mirror.rs).

mod common;

use common::*;
use notekpr_cli::{manage, note_shares, ops};
use notekpr_core::protocol::Role;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn day_words_become_dates_when_written() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let h = hub(&alice).await;
    let today = ops::local_today();
    let (t, tm, y) = (today, today.add_days(1), today.add_days(-1));

    ops::put_document(&h, "Home", "Plan.md", "Call @today, `@today` stays\n", None)
        .await
        .unwrap();
    assert_eq!(
        note(&h, "Home", "Plan").await.unwrap(),
        format!("Call @{t}, `@today` stays\n")
    );
    let out = ops::append_document(&h, "Home", "Plan.md", "Then @Tomorrow.\n")
        .await
        .unwrap();
    assert_eq!(out.bytes, format!("Then @{tm}.\n").len());
    ops::create_document(&h, "Home", "Log.md", "Since @yesterday, see me@today.com\n")
        .await
        .unwrap();
    assert_eq!(
        note(&h, "Home", "Log").await.unwrap(),
        format!("Since @{y}, see me@today.com\n")
    );

    // Notes shared with you too.
    let v = h.resolve("Home").await.unwrap();
    note_shares::share_note(&alice, &v, "Plan", "bob", Role::Editor, false)
        .await
        .unwrap();
    note_shares::write_shared(&bob, "Plan", "Bob: @today", true)
        .await
        .unwrap();
    let (_, text) = note_shares::read_shared(&bob, "Plan").await.unwrap();
    assert!(text.ends_with(&format!("Bob: @{t}")), "{text}");
}
