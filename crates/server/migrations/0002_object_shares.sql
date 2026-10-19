-- Individually shared notes: per-note document keys and access grants.

-- Current document key epoch of an individually shared note (NULL = the note
-- is encrypted with the vault key) and its title encrypted with that key.
ALTER TABLE objects ADD COLUMN key_epoch INTEGER;
ALTER TABLE objects ADD COLUMN encrypted_meta BLOB;

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
