-- Authors sign their updates and snapshots (R3). Once the vault's
-- membership log requires it (`require_signed`), unsigned content after
-- `signed_from` is refused.
ALTER TABLE updates ADD COLUMN signature BLOB;
ALTER TABLE snapshots ADD COLUMN signature BLOB;
ALTER TABLE vaults ADD COLUMN signed_from INTEGER;
