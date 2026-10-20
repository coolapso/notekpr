-- The version of the last signed recovery update (R3, [S-L8]): only higher
-- ones are accepted, so an older update can't be replayed. 0: none yet.
ALTER TABLE account_keys ADD COLUMN recovery_version INTEGER NOT NULL DEFAULT 0;
