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
