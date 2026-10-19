-- Bind an OIDC login to the browser that started it (login CSRF protection):
-- SHA-256 of a random value kept in an HttpOnly cookie on that browser.
ALTER TABLE oidc_states ADD COLUMN browser_hash BLOB;
