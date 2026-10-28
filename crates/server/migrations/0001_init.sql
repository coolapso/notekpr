-- notekpr server schema: the starting point of the data freeze.
-- The server stores identities, authorization state, public keys and
-- ciphertext. It never stores plaintext note content, paths or vault names.
--
-- Never edit this file or any later one once applied: every schema change is
-- a new numbered file that upgrades the database in place (AGENTS.md, rule 8).

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
    display_name  TEXT NOT NULL,
    email         TEXT,
    owner_id      TEXT REFERENCES users(id) ON DELETE CASCADE,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER,
    -- What this person may own, set by an administrator: NULL = the
    -- instance's default, -1 = unlimited, otherwise at most that many.
    vault_allowance INTEGER,
    agent_allowance INTEGER,
    quick_note_allowance INTEGER,
    -- What their quick notes may take, in MiB (NULL: the instance's
    -- `quick_notes_storage_mb`, -1: unlimited).
    quick_notes_storage_allowance INTEGER
);
CREATE INDEX users_owner ON users(owner_id);
-- The few people whose quick notes have their own storage limit, so
-- counting the full ones (`notekpr_vaults_full`) doesn't read everyone.
CREATE INDEX users_quick_notes_storage ON users(quick_notes_storage_allowance)
    WHERE quick_notes_storage_allowance IS NOT NULL;

-- Settings administrators change at runtime (limits, read-only mode): one
-- row, JSON (`InstanceSettings`; missing fields take their defaults).
CREATE TABLE instance_settings (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    settings   TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Scripts and services administering the instance (a billing system, say).
-- Not accounts: no keys, sessions or memberships, only the admin API.
CREATE TABLE admin_clients (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    -- Comma-separated `AdminScope`s.
    scopes     TEXT NOT NULL,
    -- Kept by `notekpr-server admin-client ensure` (the server's configuration).
    managed    INTEGER NOT NULL DEFAULT 0,
    -- The account or admin client that made it; NULL from the command line.
    created_by TEXT,
    created_at INTEGER NOT NULL
);

-- Their bearer tokens: only hashes, and the last characters to tell them apart.
CREATE TABLE admin_tokens (
    id           TEXT PRIMARY KEY,
    client_id    TEXT NOT NULL REFERENCES admin_clients(id) ON DELETE CASCADE,
    token_hash   BLOB NOT NULL UNIQUE,
    hint         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX admin_tokens_client ON admin_tokens(client_id);

-- OIDC identities, keyed by issuer + subject (never by email).
CREATE TABLE identities (
    issuer     TEXT NOT NULL,
    subject    TEXT NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (issuer, subject)
);
CREATE INDEX identities_user ON identities(user_id);

CREATE TABLE account_keys (
    user_id           TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    key_version       INTEGER NOT NULL,
    enc_public        BLOB NOT NULL,
    sign_public       BLOB NOT NULL,
    signature         BLOB NOT NULL,
    recovery_envelope BLOB,
    -- The version of the last signed recovery update (R3, [S-L8]): only
    -- higher ones are accepted, so an older update can't be replayed.
    -- 0: none yet.
    recovery_version  INTEGER NOT NULL DEFAULT 0,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

CREATE TABLE devices (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    enc_public       BLOB NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('pending', 'active', 'revoked')),
    account_envelope BLOB,
    created_at       INTEGER NOT NULL,
    approved_at      INTEGER,
    last_seen_at     INTEGER,
    revoked_at       INTEGER
);
CREATE INDEX devices_user ON devices(user_id);

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY,
    token_hash   BLOB NOT NULL UNIQUE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id    TEXT REFERENCES devices(id) ON DELETE SET NULL,
    kind         TEXT NOT NULL,
    client_name  TEXT,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE INDEX sessions_device ON sessions(device_id);

-- One-time codes handed to the browser after the OIDC callback.
CREATE TABLE login_codes (
    code_hash  BLOB PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

CREATE TABLE oidc_states (
    state         TEXT PRIMARY KEY,
    pkce_verifier TEXT NOT NULL,
    nonce         TEXT NOT NULL,
    return_to     TEXT NOT NULL,
    expires_at    INTEGER NOT NULL,
    -- Binds the login to the browser that started it (login CSRF
    -- protection): SHA-256 of a random value kept in an HttpOnly cookie on
    -- that browser.
    browser_hash  BLOB
);

-- RFC 8628 device authorization grants (CLI / desktop login).
CREATE TABLE device_grants (
    device_code_hash BLOB PRIMARY KEY,
    user_code        TEXT NOT NULL UNIQUE,
    client_name      TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'denied', 'consumed')),
    user_id          TEXT REFERENCES users(id) ON DELETE CASCADE,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    last_poll_at     INTEGER,
    -- The address the code was requested from, shown when approving it;
    -- forgotten once it is decided or expired.
    requested_from   TEXT
);

-- OAuth 2.0 client credentials for agent identities.
CREATE TABLE agent_credentials (
    client_id    TEXT PRIMARY KEY,
    agent_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    secret_hash  BLOB NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);
CREATE INDEX agent_credentials_agent ON agent_credentials(agent_id);

CREATE TABLE vaults (
    id             TEXT PRIMARY KEY,
    owner_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    encrypted_meta BLOB NOT NULL,
    key_epoch      INTEGER NOT NULL DEFAULT 1,
    head_seq       INTEGER NOT NULL DEFAULT 0,
    archived       INTEGER NOT NULL DEFAULT 0,
    -- Viewers (and people notes are shared with read-only) may comment.
    viewers_comment INTEGER NOT NULL DEFAULT 1,
    -- What the vault is for: 'quick_notes' is its owner's quick notes
    -- (never shared, a limited number of notes), at most one per person.
    kind           TEXT NOT NULL DEFAULT 'regular' CHECK (kind IN ('regular', 'quick_notes')),
    -- Someone who lost access still knows the current key (they left, or
    -- their agent was deleted): the owner's apps rotate it. Cleared by that.
    rotation_pending INTEGER NOT NULL DEFAULT 0,
    -- Bytes of encrypted content the vault holds: update payloads, snapshot
    -- and attachment sizes, without what was deleted for good (kept for the
    -- undo period, but no longer the vault's). Kept by the triggers at the
    -- end of this file, whichever query adds or removes content.
    storage_bytes  INTEGER NOT NULL DEFAULT 0,
    -- Its own storage limit in MiB, set by an administrator: NULL = the
    -- instance's default (`storage_per_vault_mb`), -1 = unlimited. Never set
    -- on quick notes: their limit is the owner's
    -- (`users.quick_notes_storage_allowance`).
    storage_allowance INTEGER,
    -- Authors sign their updates and snapshots (R3). Once the vault's
    -- membership log requires it (`require_signed`), unsigned content after
    -- this sequence number is refused.
    signed_from    INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX vaults_owner ON vaults(owner_id);
CREATE INDEX vaults_storage ON vaults(storage_bytes);
-- The few vaults with their own limit, so counting the full ones
-- (`notekpr_vaults_full`) doesn't read every vault.
CREATE INDEX vaults_own_storage ON vaults(storage_allowance, storage_bytes)
    WHERE storage_allowance IS NOT NULL;
CREATE UNIQUE INDEX vaults_quick_notes ON vaults(owner_id) WHERE kind = 'quick_notes';

CREATE TABLE vault_members (
    vault_id TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role     TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    added_by TEXT,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, user_id)
);
CREATE INDEX vault_members_user ON vault_members(user_id);

-- Vault keys encrypted (HPKE) to each member's account key, per epoch.
CREATE TABLE vault_keys (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    epoch      INTEGER NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    envelope   BLOB NOT NULL,
    created_by TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, epoch, user_id)
);

-- Epoch N's key encrypts epoch N-1's key, so members can read older content.
CREATE TABLE vault_key_chain (
    vault_id TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    epoch    INTEGER NOT NULL,
    link     BLOB NOT NULL,
    PRIMARY KEY (vault_id, epoch)
);

-- The vault membership log (R3): owner-signed, hash-chained entries. The
-- server checks that each entry continues the log and matches the change it
-- comes with; clients verify signatures and rules themselves.
CREATE TABLE member_log (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    entry      TEXT NOT NULL,   -- JSON `LogEntry`
    hash       BLOB NOT NULL,   -- the entry's hash: the next entry's `prev`
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, seq)
);

-- Deleting an object for good (purge) hides it at once and keeps its data
-- for the undo period: `erased_at` is set once the data is gone for good.
-- An undo clears `purged_at`/`purged_seq` and sets `restored_seq`.
CREATE TABLE objects (
    vault_id     TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    id           TEXT NOT NULL,
    created_by   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    purged_at    INTEGER,
    purged_seq   INTEGER,
    restored_seq INTEGER,
    erased_at    INTEGER,
    -- Every note has its own document key (R2). The current epoch of a
    -- note's key once it was shared (NULL = only vault members ever had it)
    -- and its title encrypted with that key.
    key_epoch    INTEGER,
    encrypted_meta BLOB,
    -- People a note is shared with get content from here on: the envelope
    -- epoch (4 bytes, big-endian, as in the ciphertext header) of the key
    -- created when sharing started. NULL = never shared.
    share_from   BLOB,
    -- As for vaults, for the note's own key: someone it was shared with
    -- lost access without it being rotated.
    rotation_pending INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (vault_id, id)
);
CREATE INDEX objects_purged ON objects(vault_id, purged_seq);
CREATE INDEX objects_restored ON objects(vault_id, restored_seq) WHERE restored_seq IS NOT NULL;
CREATE INDEX objects_to_erase ON objects(purged_at) WHERE purged_at IS NOT NULL AND erased_at IS NULL;

-- Individually shared notes: who has access, under the note's document key.
CREATE TABLE object_shares (
    vault_id  TEXT NOT NULL,
    object_id TEXT NOT NULL,
    user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role      TEXT NOT NULL CHECK (role IN ('editor', 'viewer')),
    added_by  TEXT,
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (vault_id, object_id, user_id),
    FOREIGN KEY (vault_id, object_id) REFERENCES objects(vault_id, id) ON DELETE CASCADE
);
CREATE INDEX object_shares_user ON object_shares(user_id);

-- Document key envelopes. user_id = '' is the copy wrapped under the vault
-- key for vault members; other rows are HPKE envelopes for recipients.
CREATE TABLE object_keys (
    vault_id   TEXT NOT NULL,
    object_id  TEXT NOT NULL,
    epoch      INTEGER NOT NULL,
    user_id    TEXT NOT NULL,
    envelope   BLOB NOT NULL,
    created_by TEXT,
    created_at INTEGER NOT NULL,
    -- The vault owner's signature on a key given to someone the note is
    -- shared with (R3): recipients use only keys the owner signed for them.
    signature  BLOB,
    PRIMARY KEY (vault_id, object_id, epoch, user_id),
    FOREIGN KEY (vault_id, object_id) REFERENCES objects(vault_id, id) ON DELETE CASCADE
);

CREATE TABLE object_key_chain (
    vault_id  TEXT NOT NULL,
    object_id TEXT NOT NULL,
    epoch     INTEGER NOT NULL,
    link      BLOB NOT NULL,
    PRIMARY KEY (vault_id, object_id, epoch),
    FOREIGN KEY (vault_id, object_id) REFERENCES objects(vault_id, id) ON DELETE CASCADE
);

CREATE TABLE updates (
    vault_id         TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    seq              INTEGER NOT NULL,
    object_id        TEXT NOT NULL,
    client_update_id TEXT NOT NULL,
    author_id        TEXT NOT NULL,
    device_id        TEXT,
    payload          BLOB NOT NULL,
    -- The author's signature (R3; see vaults.signed_from).
    signature        BLOB,
    created_at       INTEGER NOT NULL,
    PRIMARY KEY (vault_id, seq)
);
CREATE UNIQUE INDEX updates_client_id ON updates(vault_id, client_update_id);
CREATE INDEX updates_object ON updates(vault_id, object_id, seq);

CREATE TABLE snapshots (
    id         TEXT PRIMARY KEY,
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    object_id  TEXT NOT NULL,
    covers_seq INTEGER NOT NULL,
    author_id  TEXT NOT NULL,
    payload    BLOB NOT NULL,
    size       INTEGER NOT NULL,
    signature  BLOB,
    -- A snapshot that carries changes the log doesn't have (an update too
    -- large to push on its own) is sent to everyone with every catch-up
    -- from before `created_head`, the vault's head when it was stored.
    carries      INTEGER NOT NULL DEFAULT 0,
    created_head INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX snapshots_object ON snapshots(vault_id, object_id, covers_seq);
CREATE INDEX snapshots_carried ON snapshots(vault_id, created_head) WHERE carries = 1;

-- Encrypted attachment bytes live in the database so a single consistent
-- file (VACUUM INTO) is a complete backup.
CREATE TABLE attachments (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    size       INTEGER NOT NULL,
    author_id  TEXT NOT NULL,
    data       BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    -- Deleted for good, kept for the undo period (as objects).
    purged_at  INTEGER,
    PRIMARY KEY (vault_id, id)
);
CREATE INDEX attachments_purged ON attachments(purged_at) WHERE purged_at IS NOT NULL;

-- Per-user private data, encrypted with the account key.
CREATE TABLE user_prefs (
    user_id    TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    ciphertext BLOB NOT NULL,
    version    INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    at       INTEGER NOT NULL,
    actor_id TEXT,
    user_id  TEXT,
    vault_id TEXT,
    action   TEXT NOT NULL,
    target   TEXT,
    details  TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_vault ON audit_log(vault_id, id);
CREATE INDEX audit_user ON audit_log(user_id, id);

-- vaults.storage_bytes follows every change of a vault's content. An
-- object's log and history stop counting when it is deleted for good
-- (objects.purged_at set), count again if that is undone, and are erased
-- later without counting twice; attachments likewise.
CREATE TRIGGER updates_stored AFTER INSERT ON updates
WHEN NOT EXISTS (SELECT 1 FROM objects WHERE vault_id = NEW.vault_id AND id = NEW.object_id AND purged_at IS NOT NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes + length(NEW.payload) WHERE id = NEW.vault_id;
END;

CREATE TRIGGER updates_removed AFTER DELETE ON updates
WHEN NOT EXISTS (SELECT 1 FROM objects WHERE vault_id = OLD.vault_id AND id = OLD.object_id AND purged_at IS NOT NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes - length(OLD.payload) WHERE id = OLD.vault_id;
END;

CREATE TRIGGER snapshots_stored AFTER INSERT ON snapshots
WHEN NOT EXISTS (SELECT 1 FROM objects WHERE vault_id = NEW.vault_id AND id = NEW.object_id AND purged_at IS NOT NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes + NEW.size WHERE id = NEW.vault_id;
END;

CREATE TRIGGER snapshots_removed AFTER DELETE ON snapshots
WHEN NOT EXISTS (SELECT 1 FROM objects WHERE vault_id = OLD.vault_id AND id = OLD.object_id AND purged_at IS NOT NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes - OLD.size WHERE id = OLD.vault_id;
END;

CREATE TRIGGER objects_purged AFTER UPDATE OF purged_at ON objects
WHEN (OLD.purged_at IS NULL) <> (NEW.purged_at IS NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes
        + (CASE WHEN NEW.purged_at IS NULL THEN 1 ELSE -1 END) * (
            COALESCE((SELECT SUM(length(payload)) FROM updates WHERE vault_id = NEW.vault_id AND object_id = NEW.id), 0)
          + COALESCE((SELECT SUM(size) FROM snapshots WHERE vault_id = NEW.vault_id AND object_id = NEW.id), 0))
    WHERE id = NEW.vault_id;
END;

CREATE TRIGGER attachments_stored AFTER INSERT ON attachments
WHEN NEW.purged_at IS NULL
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes + NEW.size WHERE id = NEW.vault_id;
END;

CREATE TRIGGER attachments_removed AFTER DELETE ON attachments
WHEN OLD.purged_at IS NULL
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes - OLD.size WHERE id = OLD.vault_id;
END;

CREATE TRIGGER attachments_purged AFTER UPDATE OF purged_at ON attachments
WHEN (OLD.purged_at IS NULL) <> (NEW.purged_at IS NULL)
BEGIN
    UPDATE vaults SET storage_bytes = storage_bytes
        + (CASE WHEN NEW.purged_at IS NULL THEN NEW.size ELSE -NEW.size END)
    WHERE id = NEW.vault_id;
END;
