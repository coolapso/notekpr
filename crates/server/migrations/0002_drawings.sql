-- no-transaction
-- Drawings (docs/protocol.md §5.5): the server learns which objects are
-- drawings — never their names or content — to count them per account, and
-- each person may have a Drawings section, a vault of its own kind.
--
-- `vaults.kind` gains a value, and SQLite can't change a CHECK in place: the
-- table is copied following SQLite's procedure for schema changes
-- (https://sqlite.org/lang_altertable.html#otheralter). That needs foreign
-- keys off, which only works outside a transaction: sqlx runs this file as
-- it is (the first line), and it starts and commits its own.

PRAGMA foreign_keys = OFF;

BEGIN;

-- What an object is (`ObjectKind`): set when its row is made, never changed.
ALTER TABLE objects ADD COLUMN kind TEXT NOT NULL DEFAULT 'note' CHECK (kind IN ('note', 'drawing'));
-- A drawing moved here from another vault of the same owner, at their
-- limit: the original it replaces, deleted for good once this copy's first
-- update is stored (then cleared).
ALTER TABLE objects ADD COLUMN replaces_vault TEXT;
ALTER TABLE objects ADD COLUMN replaces_object TEXT;
-- Counting someone's drawings reads only those, vault by vault.
CREATE INDEX objects_drawings ON objects(vault_id) WHERE kind = 'drawing' AND purged_at IS NULL;
-- The few copies waiting to replace an original.
CREATE INDEX objects_replacing ON objects(replaces_vault, replaces_object) WHERE replaces_vault IS NOT NULL;

-- How many drawings this person may have, and what their Drawings section
-- may take in MiB: NULL = the instance's default, -1 = unlimited.
ALTER TABLE users ADD COLUMN drawing_allowance INTEGER;
ALTER TABLE users ADD COLUMN drawings_storage_allowance INTEGER;
CREATE INDEX users_drawings_storage ON users(drawings_storage_allowance)
    WHERE drawings_storage_allowance IS NOT NULL;

-- vaults.kind: 'drawings' is its owner's Drawings section (never shared,
-- drawings and files only), at most one per person. The rest as in
-- 0001_init.sql.
CREATE TABLE vaults_new (
    id             TEXT PRIMARY KEY,
    owner_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    encrypted_meta BLOB NOT NULL,
    key_epoch      INTEGER NOT NULL DEFAULT 1,
    head_seq       INTEGER NOT NULL DEFAULT 0,
    archived       INTEGER NOT NULL DEFAULT 0,
    viewers_comment INTEGER NOT NULL DEFAULT 1,
    kind           TEXT NOT NULL DEFAULT 'regular' CHECK (kind IN ('regular', 'quick_notes', 'drawings')),
    rotation_pending INTEGER NOT NULL DEFAULT 0,
    storage_bytes  INTEGER NOT NULL DEFAULT 0,
    -- Never set on quick notes and Drawings: their limits are the owner's.
    storage_allowance INTEGER,
    signed_from    INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
INSERT INTO vaults_new (id, owner_id, encrypted_meta, key_epoch, head_seq, archived,
                        viewers_comment, kind, rotation_pending, storage_bytes,
                        storage_allowance, signed_from, created_at, updated_at)
    SELECT id, owner_id, encrypted_meta, key_epoch, head_seq, archived,
           viewers_comment, kind, rotation_pending, storage_bytes,
           storage_allowance, signed_from, created_at, updated_at
    FROM vaults;
DROP TABLE vaults;
-- The triggers of other tables update `vaults` by name: without this, the
-- rename would check them while no table has that name, and fail.
PRAGMA legacy_alter_table = ON;
ALTER TABLE vaults_new RENAME TO vaults;
PRAGMA legacy_alter_table = OFF;
CREATE INDEX vaults_owner ON vaults(owner_id);
CREATE INDEX vaults_storage ON vaults(storage_bytes);
CREATE INDEX vaults_own_storage ON vaults(storage_allowance, storage_bytes)
    WHERE storage_allowance IS NOT NULL;
CREATE UNIQUE INDEX vaults_quick_notes ON vaults(owner_id) WHERE kind = 'quick_notes';
CREATE UNIQUE INDEX vaults_drawings ON vaults(owner_id) WHERE kind = 'drawings';

-- Every row of the tables referring to `vaults` still has its vault: any
-- that doesn't fails the CHECK, and with it the whole migration.
CREATE TEMP TABLE migration_check (dangling INTEGER CHECK (dangling = 0));
INSERT INTO migration_check SELECT COUNT(*) FROM pragma_foreign_key_check;
DROP TABLE migration_check;

COMMIT;

PRAGMA foreign_keys = ON;
