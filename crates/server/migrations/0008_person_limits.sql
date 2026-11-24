-- Limits per person (protocol.md §9, "Limits per person"): a person's own
-- request rate (per second) and daily write allowance (MiB), encoded like
-- the other allowances (NULL: the instance's; -1: unlimited), and until
-- when they are suspended (ms; NULL or a time past: not). Agents have none
-- of their own: theirs are their owner's.
ALTER TABLE users ADD COLUMN rate_allowance INTEGER;
ALTER TABLE users ADD COLUMN write_allowance INTEGER;
ALTER TABLE users ADD COLUMN suspended_until INTEGER;
