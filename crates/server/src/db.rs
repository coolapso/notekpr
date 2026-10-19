use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};

pub type Db = SqlitePool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(path: &Path) -> anyhow::Result<Db> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating data directory {}", dir.display()))?;
    }
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(15));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .min_connections(1)
        .connect_with(opts)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;
    Ok(pool)
}

/// Run migrations, taking an automatic backup first if an existing database
/// is about to be upgraded.
pub async fn migrate(db: &Db, path: &Path) -> anyhow::Result<()> {
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
        .fetch_all(db)
        .await
        .unwrap_or_default();
    let pending = MIGRATOR.iter().filter(|m| !applied.contains(&m.version)).count();
    if pending > 0 && !applied.is_empty() {
        let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("pre-migration-{}.db", coolpsonotes_core::now_ms()));
        backup_to(db, &out).await?;
        tracing::info!(backup = %out.display(), pending, "backed up database before migration");
    }
    MIGRATOR.run(db).await.context("running database migrations")?;
    Ok(())
}

/// Consistent online copy of the whole database.
pub async fn backup_to(db: &Db, out: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!out.exists(), "{} already exists", out.display());
    let p = out.to_str().context("backup path must be valid UTF-8")?.replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{p}'"))).execute(db).await?;
    Ok(())
}

/// Verify a backup and install it as the database at `target`. The current
/// database (if any) is moved aside and its path returned. Stop the server first.
pub async fn restore_from(target: &Path, from: &Path) -> anyhow::Result<Option<std::path::PathBuf>> {
    anyhow::ensure!(from.exists(), "{} does not exist", from.display());
    {
        let pool = connect(from).await.context("opening backup")?;
        let check: String = sqlx::query_scalar("PRAGMA integrity_check").fetch_one(&pool).await?;
        anyhow::ensure!(check == "ok", "backup failed integrity check: {check}");
        let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .context("file is not a coolpsonotes database backup")?;
        anyhow::ensure!(versions > 0, "file is not a coolpsonotes database backup");
        pool.close().await;
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut aside = None;
    if target.exists() {
        let a = target.with_extension(format!("db.pre-restore-{}", now()));
        std::fs::rename(target, &a)?;
        for ext in ["db-wal", "db-shm"] {
            let side = target.with_extension(ext);
            if side.exists() {
                std::fs::rename(&side, format!("{}-{}", a.display(), ext))?;
            }
        }
        aside = Some(a);
    }
    std::fs::copy(from, target)?;
    // Bring the restored database up to this binary's schema.
    let pool = connect(target).await?;
    migrate(&pool, target).await?;
    pool.close().await;
    Ok(aside)
}

pub fn now() -> i64 {
    coolpsonotes_core::now_ms()
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

fn random<const N: usize>() -> [u8; N] {
    coolpsonotes_core::crypto::random_bytes::<N>()
}

/// New opaque bearer secret. Returns `(secret, sha256(secret))`; only the hash is stored.
pub fn new_secret(prefix: &str) -> (String, Vec<u8>) {
    let s = format!("{prefix}{}", data_encoding::BASE64URL_NOPAD.encode(&random::<32>()));
    let h = sha256(s.as_bytes());
    (s, h)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// RFC 8628 §6.1: short, unambiguous, consonants only.
pub fn new_user_code() -> String {
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    let bytes = random::<8>();
    let chars: String = bytes.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

/// Identifiers chosen by clients (vault, object, device, attachment ids).
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub async fn audit(
    db: &Db,
    actor_id: Option<&str>,
    user_id: Option<&str>,
    vault_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    details: serde_json::Value,
) {
    let r = sqlx::query(
        "INSERT INTO audit_log (at, actor_id, user_id, vault_id, action, target, details) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now())
    .bind(actor_id)
    .bind(user_id)
    .bind(vault_id)
    .bind(action)
    .bind(target)
    .bind(details.to_string())
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, action, "failed to write audit log");
    }
}
