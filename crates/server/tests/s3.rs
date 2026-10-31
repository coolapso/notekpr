//! Attachments in S3-compatible storage (`--file-store s3://…`). `task
//! test:s3` (scripts/test-s3.sh) runs this against a local Garage in
//! Docker, with the usual `AWS_*` variables set, as for a server.

mod common;

use common::*;
use futures::TryStreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};
use reqwest::Method;

const DAY: i64 = 24 * 3600 * 1000;

async fn count(store: &dyn ObjectStore, prefix: &str) -> usize {
    let files: Vec<_> = store
        .list(Some(&Path::from(prefix)))
        .try_collect()
        .await
        .unwrap();
    files.len()
}

#[tokio::test]
#[ignore = "needs an S3-compatible server: task test:s3"]
async fn attachments_live_in_s3_compatible_storage() {
    let bucket = std::env::var("NOTEKPR_TEST_S3_BUCKET").expect("run by scripts/test-s3.sh");
    let bucket_store = AmazonS3Builder::from_env()
        .with_bucket_name(&bucket)
        .build()
        .unwrap();
    // Two servers share the bucket, each under a prefix of its own.
    let a = start_with(|c| {
        c.file_store = Some(format!("s3://{bucket}/a"));
        c.purge_undo_days = 0;
    })
    .await;
    let b = start_with(|c| c.file_store = Some(format!("s3://{bucket}/b/"))).await;
    let mut alice = new_user(&a.base, "alice").await;
    let mut bob = new_user(&b.base, "bob").await;
    let v = alice.create_vault("Home").await;
    let w = bob.create_vault("Home").await;
    assert_eq!(upload(&alice, &v, "a1", b"one").await, 201);
    assert_eq!(upload(&alice, &v, "a2", b"two").await, 201);
    assert_eq!(upload(&bob, &w, "b1", b"bob's").await, 201);
    let (plain, len) = download(&alice, &v, "a1").await;
    assert_eq!(plain, b"one");
    assert!(len.is_some(), "streamed with its size");
    assert_eq!(upload(&alice, &v, "a1", b"one").await, 200);
    assert_eq!(upload(&alice, &v, "a1", b"another one").await, 409);
    assert_eq!(count(&bucket_store, "a").await, 2);
    assert_eq!(count(&bucket_store, "b").await, 1);

    // Deleted for good: gone from the bucket after the grace period, and
    // only that file.
    let a1 = format!("/vaults/{v}/attachments/a1");
    assert_eq!(alice.api.status::<()>(Method::DELETE, &a1, None).await, 204);
    let later = notekpr_server::db::now() + 15 * DAY;
    assert_eq!(a.state.blobs.delete_due(later).await.unwrap(), 1);
    assert_eq!(b.state.blobs.delete_due(later).await.unwrap(), 0);
    assert_eq!(count(&bucket_store, "a").await, 1);
    assert_eq!(count(&bucket_store, "b").await, 1);
    assert_eq!(download(&alice, &v, "a2").await.0, b"two");
    assert_eq!(download(&bob, &w, "b1").await.0, b"bob's");

    // The bucket itself is checked when a server starts: an empty prefix
    // holds none of a server's files.
    let key: String = sqlx::query_scalar("SELECT file_key FROM attachments")
        .fetch_one(&b.state.db)
        .await
        .unwrap();
    bucket_store
        .delete(&Path::from(format!("b/{key}")))
        .await
        .unwrap();
    let mut cfg = notekpr_server::config::Config::for_tests(b.dir.path().to_path_buf());
    cfg.file_store = Some(format!("s3://{bucket}/b"));
    let refused = notekpr_server::build(cfg, None).await.err().unwrap();
    assert!(refused.to_string().contains("holds none"), "{refused}");
}
