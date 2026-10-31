//! Attachments outside the database (`--file-store`, `blobs.rs`): in a
//! folder here (the default); S3-compatible storage in `s3.rs`.

mod common;

use std::path::{Path, PathBuf};

use axum::body::Bytes;
use common::*;
use notekpr_server::config::Config;
use notekpr_server::error::AppError;
use notekpr_server::state::AppState;
use reqwest::Method;

const DAY: i64 = 24 * 3600 * 1000;

/// Files in the store's folder (under every vault).
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(vaults) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    vaults
        .flatten()
        .flat_map(|v| std::fs::read_dir(v.path()).into_iter().flatten().flatten())
        .map(|f| f.path())
        .collect()
}

/// `(file_key, bytes left in the database)` of an attachment's row.
async fn row(state: &AppState, id: &str) -> Option<(Option<String>, i64)> {
    sqlx::query_as("SELECT file_key, length(data) FROM attachments WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .unwrap()
}

/// A server on `data` (which outlives it), as `common::start_with` makes.
async fn serve(data: &Path, f: impl FnOnce(&mut Config)) -> anyhow::Result<(String, AppState)> {
    let mut cfg = Config::for_tests(data.to_path_buf());
    f(&mut cfg);
    let (app, state) = notekpr_server::build(cfg, None).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Ok((format!("http://{addr}/api/v1"), state))
}

#[tokio::test]
async fn attachments_are_files_in_the_store_and_rows_in_the_database() {
    let srv = start().await;
    let files = srv.dir.path().join("files");
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Home").await;
    assert_eq!(upload(&alice, &v, "a1", b"a picture").await, 201);

    let (key, in_db) = row(&srv.state, "a1").await.unwrap();
    let key = key.expect("a file in the store");
    assert!(
        key.starts_with(&format!("{v}/a1.")),
        "named by vault and attachment: {key}"
    );
    assert_eq!(in_db, 0, "no bytes left in the database");
    assert_eq!(files_in(&files), vec![files.join(&key)]);
    let size = std::fs::metadata(files.join(&key)).unwrap().len();
    let (plain, len) = download(&alice, &v, "a1").await;
    assert_eq!(plain, b"a picture");
    assert_eq!(len, Some(size), "streamed with its size");
    // Instance storage counts it.
    assert_eq!(
        notekpr_server::repo::attachments::file_bytes(&srv.state.db)
            .await
            .unwrap(),
        size
    );

    // A retry is answered without a second file; another attachment with
    // the same id is refused, and leaves none either.
    assert_eq!(upload(&alice, &v, "a1", b"a picture").await, 200);
    assert_eq!(
        upload(&alice, &v, "a1", b"something else entirely").await,
        409
    );
    assert_eq!(files_in(&files).len(), 1);
    assert_eq!(download(&alice, &v, "a1").await.0, b"a picture");
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_deletions")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(pending, 0, "every upload was claimed or deleted");
}

#[tokio::test]
async fn files_are_deleted_a_while_after_their_rows() {
    let srv = start_with(|c| c.purge_undo_days = 0).await;
    let files = srv.dir.path().join("files");
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Home").await;
    let w = alice.create_vault("Old").await;
    for (vault, id) in [(&v, "a1"), (&v, "a2"), (&w, "a3")] {
        assert_eq!(upload(&alice, vault, id, id.as_bytes()).await, 201);
    }
    let path = |id: &'static str| {
        let state = srv.state.clone();
        let files = files.clone();
        async move { files.join(row(&state, id).await.unwrap().0.unwrap()) }
    };
    let (a1, a3) = (path("a1").await, path("a3").await);

    // Deleted for good (no undo period here) and a whole vault deleted:
    // the rows go at once, the files after the grace period.
    for p in [
        format!("/vaults/{v}/attachments/a1"),
        format!("/vaults/{w}"),
    ] {
        assert_eq!(alice.api.status::<()>(Method::DELETE, &p, None).await, 204);
    }
    assert!(row(&srv.state, "a1").await.is_none() && row(&srv.state, "a3").await.is_none());
    let blobs = &srv.state.blobs;
    let now = notekpr_server::db::now();
    assert_eq!(blobs.delete_due(now + DAY).await.unwrap(), 0);
    assert!(
        a1.exists() && a3.exists(),
        "kept for a backup of the database"
    );
    assert_eq!(blobs.delete_due(now + 15 * DAY).await.unwrap(), 2);
    assert!(!a1.exists() && !a3.exists());
    assert_eq!(files_in(&files).len(), 1);
    assert_eq!(download(&alice, &v, "a2").await.0, b"a2");
    assert_eq!(
        blobs.delete_due(now + 15 * DAY).await.unwrap(),
        0,
        "done once"
    );
}

#[tokio::test]
async fn uploads_never_claimed_are_deleted() {
    let srv = start().await;
    let files = srv.dir.path().join("files");
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Home").await;
    let blobs = &srv.state.blobs;

    // A file written, then nothing (a crash before its row).
    let lost = blobs
        .stage(&v, "x1", Bytes::from_static(b"lost"))
        .await
        .unwrap();
    assert_eq!(files_in(&files).len(), 1);
    let now = notekpr_server::db::now();
    assert_eq!(
        blobs.delete_due(now).await.unwrap(),
        0,
        "it may still be claimed"
    );
    assert_eq!(blobs.delete_due(now + 2 * 3600 * 1000).await.unwrap(), 1);
    assert!(files_in(&files).is_empty());
    // Too late to claim it now.
    assert!(matches!(
        blobs.commit(&lost, &alice.id).await,
        Err(AppError::Unavailable(_))
    ));
    assert!(row(&srv.state, "x1").await.is_none());
}

#[tokio::test]
async fn attachments_in_the_database_move_to_the_file_store() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let files = data.join("files");

    // In the database, as before.
    let (base, state) = serve(data, |c| c.file_store = Some("database".into()))
        .await
        .unwrap();
    let mut alice = new_user(&base, "alice").await;
    let v = alice.create_vault("Home").await;
    assert_eq!(upload(&alice, &v, "a1", b"one").await, 201);
    assert_eq!(upload(&alice, &v, "a2", b"two").await, 201);
    assert_eq!(upload(&alice, &v, "a2", b"two").await, 200);
    assert_eq!(row(&state, "a1").await.unwrap().0, None);
    assert_eq!(download(&alice, &v, "a1").await.0, b"one");
    // One deleted for good, still undoable: it moves too.
    let a2 = format!("/vaults/{v}/attachments/a2");
    assert_eq!(alice.api.status::<()>(Method::DELETE, &a2, None).await, 204);
    assert!(files_in(&files).is_empty());

    // With a folder (the default), they move there by themselves.
    let (base, state) = serve(data, |_| {}).await.unwrap();
    alice.api.base = base;
    for _ in 0..100 {
        let left = notekpr_server::repo::attachments::count_in_database(&state.db)
            .await
            .unwrap();
        if left == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    for id in ["a1", "a2"] {
        let (key, in_db) = row(&state, id).await.unwrap();
        assert!(files.join(key.unwrap()).exists());
        assert_eq!(in_db, 0);
    }
    assert_eq!(files_in(&files).len(), 2);
    assert_eq!(download(&alice, &v, "a1").await.0, b"one");
    let restore = format!("{a2}/restore");
    assert_eq!(
        alice.api.status::<()>(Method::POST, &restore, None).await,
        204
    );
    assert_eq!(download(&alice, &v, "a2").await.0, b"two");

    // Back to the database store: refused, saying why.
    let refused = serve(data, |c| c.file_store = Some("database".into()))
        .await
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("kept in a file store"),
        "{refused}"
    );
}
