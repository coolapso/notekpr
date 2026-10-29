-- Drawings (docs/protocol.md §5.5): the server learns which objects are
-- drawings — never their names or content — to count them per vault.

-- What an object is (`ObjectKind`): set when its row is made, never changed.
ALTER TABLE objects ADD COLUMN kind TEXT NOT NULL DEFAULT 'note' CHECK (kind IN ('note', 'drawing'));
-- Counting a vault's drawings reads only those.
CREATE INDEX objects_drawings ON objects(vault_id) WHERE kind = 'drawing' AND purged_at IS NULL;

-- How many drawings a vault may hold, set by administrators: NULL = the
-- instance's default (`drawings_per_vault`), -1 = unlimited.
ALTER TABLE vaults ADD COLUMN drawing_allowance INTEGER;
