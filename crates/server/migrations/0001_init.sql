-- notekpr server schema v1.
-- The server stores identities, authorization state, public keys and
-- ciphertext. It never stores plaintext note content, paths or vault names.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
    display_name  TEXT NOT NULL,
    email         TEXT,
    owner_id      TEXT REFERENCES users(id) ON DELETE CASCADE,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER
);
CREATE INDEX users_owner ON users(owner_id);

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
    expires_at    INTEGER NOT NULL
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
    last_poll_at     INTEGER
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
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE vault_members (
    vault_id TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role     TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
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

CREATE TABLE objects (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    created_by TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    purged_at  INTEGER,
    purged_seq INTEGER,
    PRIMARY KEY (vault_id, id)
);
CREATE INDEX objects_purged ON objects(vault_id, purged_seq);

CREATE TABLE updates (
    vault_id         TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    seq              INTEGER NOT NULL,
    object_id        TEXT NOT NULL,
    client_update_id TEXT NOT NULL,
    author_id        TEXT NOT NULL,
    device_id        TEXT,
    payload          BLOB NOT NULL,
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
    created_at INTEGER NOT NULL
);
CREATE INDEX snapshots_object ON snapshots(vault_id, object_id, covers_seq);

-- Encrypted attachment bytes live in the database so a single consistent
-- file (VACUUM INTO) is a complete backup.
CREATE TABLE attachments (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    size       INTEGER NOT NULL,
    author_id  TEXT NOT NULL,
    data       BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, id)
);

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
