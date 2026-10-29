use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

pub type Db = SqlitePool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(path: &Path) -> anyhow::Result<Db> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating data directory {}", dir.display()))?;
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
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
            .fetch_all(db)
            .await
            .unwrap_or_default();
    let pending = MIGRATOR
        .iter()
        .filter(|m| !applied.contains(&m.version))
        .count();
    if pending > 0 && !applied.is_empty() {
        let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("pre-migration-{}.db", notekpr_core::now_ms()));
        backup_to(db, &out).await?;
        tracing::info!(backup = %out.display(), pending, "backed up database before migration");
    }
    MIGRATOR
        .run(db)
        .await
        .context("running database migrations")?;
    Ok(())
}

/// Consistent online copy of the whole database.
pub async fn backup_to(db: &Db, out: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!out.exists(), "{} already exists", out.display());
    let p = out
        .to_str()
        .context("backup path must be valid UTF-8")?
        .replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{p}'")))
        .execute(db)
        .await?;
    Ok(())
}

/// Verify a backup and install it as the database at `target`. The current
/// database (if any) is moved aside and its path returned. Stop the server first.
pub async fn restore_from(
    target: &Path,
    from: &Path,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    anyhow::ensure!(from.exists(), "{} does not exist", from.display());
    {
        let pool = connect(from).await.context("opening backup")?;
        let check: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await?;
        anyhow::ensure!(check == "ok", "backup failed integrity check: {check}");
        let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .context("file is not a notekpr database backup")?;
        anyhow::ensure!(versions > 0, "file is not a notekpr database backup");
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
    notekpr_core::now_ms()
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

fn random<const N: usize>() -> [u8; N] {
    notekpr_core::crypto::random_bytes::<N>()
}

/// New opaque bearer secret. Returns `(secret, sha256(secret))`; only the hash is stored.
pub fn new_secret(prefix: &str) -> (String, Vec<u8>) {
    let s = format!(
        "{prefix}{}",
        data_encoding::BASE64URL_NOPAD.encode(&random::<32>())
    );
    let h = sha256(s.as_bytes());
    (s, h)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// RFC 8628 §6.1: short, unambiguous, consonants only; every character
/// equally likely (lookups are rate-limited and codes expire).
pub fn new_user_code() -> String {
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    // Accept only bytes below the largest multiple of the alphabet size.
    let limit = 256 - 256 % ALPHABET.len();
    let mut chars = String::with_capacity(9);
    while chars.len() < 8 {
        for b in random::<16>() {
            if (b as usize) < limit && chars.len() < 8 {
                chars.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    format!("{}-{}", &chars[..4], &chars[4..])
}

/// Identifiers chosen by clients (vault, object, device, attachment ids).
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Best-effort audit entry (see [`crate::repo::audit::record`]).
pub use crate::repo::audit::record as audit;

/// Readiness probe: can the database answer a query?
pub async fn ping(db: &Db) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(db)
        .await
        .is_ok()
}

#[cfg(test)]
mod user_code_tests {
    #[test]
    fn user_codes_use_the_whole_alphabet_evenly_and_keep_their_shape() {
        let mut counts = std::collections::HashMap::new();
        for _ in 0..2000 {
            let c = super::new_user_code();
            assert_eq!(c.len(), 9);
            assert_eq!(&c[4..5], "-");
            for ch in c.chars().filter(|c| *c != '-') {
                assert!("BCDFGHJKLMNPQRSTVWXZ".contains(ch));
                *counts.entry(ch).or_insert(0usize) += 1;
            }
        }
        // 16000 draws over 20 letters: about 800 each.
        assert_eq!(counts.len(), 20);
        assert!(
            counts.values().all(|n| (600..1000).contains(n)),
            "{counts:?}"
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// Every table that refers to `vaults`, directly or through `objects`.
    const TABLES: [&str; 11] = [
        "vault_members",
        "vault_keys",
        "vault_key_chain",
        "member_log",
        "objects",
        "object_shares",
        "object_keys",
        "object_key_chain",
        "updates",
        "snapshots",
        "attachments",
    ];

    async fn count(db: &Db, table: &str, vault: &str) -> i64 {
        let q = format!("SELECT COUNT(*) FROM {table} WHERE vault_id = ?");
        sqlx::query_scalar(sqlx::AssertSqlSafe(q))
            .bind(vault)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn storage_bytes(db: &Db, vault: &str) -> i64 {
        sqlx::query_scalar("SELECT storage_bytes FROM vaults WHERE id = ?")
            .bind(vault)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn run(db: &Db, q: &'static str) -> Result<(), sqlx::Error> {
        sqlx::query(q).execute(db).await.map(|_| ())
    }

    #[derive(sqlx::FromRow, Debug, PartialEq)]
    struct VaultRow {
        owner_id: String,
        encrypted_meta: Vec<u8>,
        key_epoch: i64,
        head_seq: i64,
        archived: bool,
        viewers_comment: bool,
        kind: String,
        rotation_pending: bool,
        storage_bytes: i64,
        storage_allowance: Option<i64>,
        signed_from: Option<i64>,
        created_at: i64,
        updated_at: i64,
    }

    async fn vault_rows(db: &Db) -> Vec<VaultRow> {
        sqlx::query_as(
            "SELECT owner_id, encrypted_meta, key_epoch, head_seq, archived, viewers_comment, kind,
                    rotation_pending, storage_bytes, storage_allowance, signed_from, created_at,
                    updated_at
             FROM vaults ORDER BY id",
        )
        .fetch_all(db)
        .await
        .unwrap()
    }

    /// A database made at the start of the data freeze, with rows in every
    /// table that refers to a vault, keeps them all through the drawings
    /// migration; the triggers keeping each vault's size and the cascades
    /// deleting a vault still work.
    #[tokio::test]
    async fn the_drawings_migration_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        let db = connect(&path).await.unwrap();
        MIGRATOR.run_to(1, &db).await.unwrap();
        for q in [
            "INSERT INTO users (id, kind, display_name, created_at, quick_note_allowance)
             VALUES ('alice', 'human', 'Alice', 1, 5), ('bob', 'human', 'Bob', 1, NULL),
                    ('carol', 'human', 'Carol', 1, NULL)",
            "INSERT INTO vaults (id, owner_id, encrypted_meta, key_epoch, head_seq, archived,
                                 viewers_comment, kind, rotation_pending, storage_allowance,
                                 signed_from, created_at, updated_at)
             VALUES ('v', 'alice', x'01', 2, 3, 1, 0, 'regular', 1, 7, 2, 10, 11),
                    ('q', 'alice', x'02', 1, 1, 0, 1, 'quick_notes', 0, NULL, NULL, 12, 13)",
            "INSERT INTO vault_members (vault_id, user_id, role, added_by, added_at)
             VALUES ('v', 'alice', 'owner', 'alice', 1), ('v', 'bob', 'editor', 'alice', 2),
                    ('q', 'alice', 'owner', 'alice', 1)",
            "INSERT INTO vault_keys (vault_id, epoch, user_id, envelope, created_at)
             VALUES ('v', 1, 'alice', x'aa', 1), ('v', 2, 'alice', x'ab', 2),
                    ('v', 2, 'bob', x'ac', 2), ('q', 1, 'alice', x'ad', 1)",
            "INSERT INTO vault_key_chain (vault_id, epoch, link) VALUES ('v', 2, x'bb')",
            "INSERT INTO member_log (vault_id, seq, entry, hash, created_at)
             VALUES ('v', 0, '{}', x'cc', 1), ('v', 1, '{}', x'cd', 2)",
            "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at, key_epoch, share_from)
             VALUES ('v', 'index', 'alice', 1, 1, NULL, NULL),
                    ('v', 'n', 'alice', 1, 1, 2147483649, x'80000001'),
                    ('v', 'c_n', 'bob', 1, 1, NULL, NULL), ('q', 'm', 'alice', 1, 1, NULL, NULL)",
            "INSERT INTO object_shares (vault_id, object_id, user_id, role, added_at)
             VALUES ('v', 'n', 'carol', 'viewer', 3)",
            "INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_at)
             VALUES ('v', 'n', 2147483649, '', x'dd', 3), ('v', 'n', 2147483649, 'carol', x'de', 3)",
            "INSERT INTO object_key_chain (vault_id, object_id, epoch, link)
             VALUES ('v', 'n', 2147483650, x'ee')",
            "INSERT INTO updates (vault_id, seq, object_id, client_update_id, author_id, payload, created_at)
             VALUES ('v', 1, 'index', 'u1', 'alice', x'0102', 1),
                    ('v', 2, 'n', 'u2', 'alice', x'010203', 2),
                    ('v', 3, 'c_n', 'u3', 'bob', x'01', 3), ('q', 1, 'm', 'u4', 'alice', x'0102', 4)",
            "INSERT INTO snapshots (id, vault_id, object_id, covers_seq, author_id, payload, size, created_at)
             VALUES ('s', 'v', 'n', 2, 'alice', x'0405', 2, 5)",
            "INSERT INTO attachments (vault_id, id, size, author_id, data, created_at)
             VALUES ('v', 'a', 4, 'alice', x'05060708', 6)",
        ] {
            run(&db, q).await.unwrap();
        }
        let mut before = Vec::new();
        for t in TABLES {
            let n = count(&db, t, "v").await;
            assert!(n > 0, "no rows in {t}");
            before.push(n);
        }
        // 2 + 3 + 1 bytes of updates, a 2-byte snapshot, a 4-byte file.
        assert_eq!(storage_bytes(&db, "v").await, 12);
        let vaults_before = vault_rows(&db).await;

        migrate(&db, &path).await.unwrap();

        // Everything is still there, as it was.
        for (t, n) in TABLES.iter().zip(before) {
            assert_eq!(count(&db, t, "v").await, n, "{t}");
        }
        assert_eq!(vault_rows(&db).await, vaults_before);
        let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM objects")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(kinds, ["note"], "what was there is notes");
        // The old database was backed up first.
        assert!(dir.path().join("backups").read_dir().unwrap().count() > 0);
        // Foreign keys are still enforced.
        let on: bool = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(on);
        let dangling = run(
            &db,
            "INSERT INTO vault_members (vault_id, user_id, role, added_at)
             VALUES ('nowhere', 'bob', 'editor', 1)",
        )
        .await;
        assert!(dangling.is_err(), "a member of no vault");

        // The triggers still keep the vault's size.
        run(
            &db,
            "INSERT INTO updates (vault_id, seq, object_id, client_update_id, author_id, payload, created_at)
             VALUES ('v', 4, 'n', 'u5', 'alice', x'0102030405', 7)",
        )
        .await
        .unwrap();
        assert_eq!(storage_bytes(&db, "v").await, 17);
        run(
            &db,
            "UPDATE attachments SET purged_at = 8 WHERE vault_id = 'v' AND id = 'a'",
        )
        .await
        .unwrap();
        assert_eq!(storage_bytes(&db, "v").await, 13);
        run(
            &db,
            "UPDATE objects SET purged_at = 9 WHERE vault_id = 'v' AND id = 'n'",
        )
        .await
        .unwrap();
        // The note's 3 + 5 bytes of updates and its 2-byte snapshot are out.
        assert_eq!(storage_bytes(&db, "v").await, 3);

        // Every vault takes the instance's drawings limit until an
        // administrator gives it its own.
        let own: Vec<Option<i64>> = sqlx::query_scalar("SELECT drawing_allowance FROM vaults")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(own, [None, None]);
        // Objects are notes or drawings, nothing else.
        assert!(
            run(
                &db,
                "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at, kind)
                 VALUES ('v', 'b', 'alice', 1, 1, 'board')"
            )
            .await
            .is_err()
        );

        // Deleting a vault still takes everything in it.
        run(&db, "DELETE FROM vaults WHERE id = 'v'").await.unwrap();
        for t in TABLES {
            assert_eq!(count(&db, t, "v").await, 0, "{t}");
        }
        assert_eq!(count(&db, "objects", "q").await, 1, "other vaults stay");
        let check: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(check, "ok");
    }
}
