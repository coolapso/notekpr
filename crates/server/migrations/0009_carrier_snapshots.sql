-- Snapshots that carry changes the log doesn't have (an update too large to
-- push on its own): sent to everyone with every catch-up from before
-- `created_head`, the vault's head when the snapshot was stored.
ALTER TABLE snapshots ADD COLUMN carries INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snapshots ADD COLUMN created_head INTEGER NOT NULL DEFAULT 0;
CREATE INDEX snapshots_carried ON snapshots(vault_id, created_head) WHERE carries = 1;
