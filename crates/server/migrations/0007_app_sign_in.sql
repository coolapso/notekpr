-- Apps signing in through the system browser (protocol.md §3, "Apps"): the
-- loopback address the browser goes back to, and the PKCE challenge the
-- one-time code is bound to, so only the app that started the sign-in can
-- swap the code for a session. NULL for the web app's own sign-ins.
ALTER TABLE oidc_states ADD COLUMN app_redirect TEXT;
ALTER TABLE oidc_states ADD COLUMN code_challenge TEXT;
ALTER TABLE login_codes ADD COLUMN code_challenge TEXT;

-- An app's sign-in after the identity provider, waiting for the person to
-- confirm it in the browser (the code the app shows): only then does the
-- browser go back to the app with a one-time code. `token_hash` is the hash
-- of the browser's confirmation cookie.
CREATE TABLE app_sign_ins (
    token_hash     BLOB PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    app_redirect   TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at     INTEGER NOT NULL
);
