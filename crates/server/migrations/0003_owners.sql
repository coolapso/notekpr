-- Several owners per vault (docs/security.md): any owner may share a note,
-- so a note's key for someone it is shared with says which owner signed
-- the grant, and they check it with that owner's key.
ALTER TABLE object_keys ADD COLUMN signed_by TEXT;
-- Until now the vault's one owner signed every grant.
UPDATE object_keys SET signed_by = (SELECT owner_id FROM vaults WHERE vaults.id = object_keys.vault_id)
WHERE signature IS NOT NULL;
