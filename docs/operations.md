# Operating coolpsonotes

coolpsonotes is a single binary (`coolpsonotes-server`) plus static files for
the browser client. State is one SQLite database in the data directory. Run one
instance per database.

## 1. Install

### Docker Compose (recommended)

```sh
cp .env.example .env          # set PUBLIC_HOST and your OIDC client
docker compose up -d --build  # coolpsonotes + Caddy with automatic HTTPS
```

### Kubernetes

Edit and apply [`deploy/kubernetes/coolpsonotes.yaml`](../deploy/kubernetes/coolpsonotes.yaml)
(single replica, `Recreate` strategy, a `ReadWriteOnce` volume) and optionally
[`backup-cronjob.yaml`](../deploy/kubernetes/backup-cronjob.yaml).
Make sure the ingress allows long-lived WebSocket connections.

### From source

```sh
cargo build --release -p coolpsonotes-server
./scripts/build-web.sh          # needs the wasm32 target and wasm-bindgen-cli 0.2.128
./target/release/coolpsonotes-server \
  --public-url https://notes.example.com --web-dir dist/web --data-dir /var/lib/coolpsonotes \
  --oidc-issuer https://auth.example.com --oidc-client-id coolpsonotes --oidc-client-secret …
```

## 2. Configuration

Every flag can be set as an environment variable.

| Flag / variable | Default | Meaning |
|---|---|---|
| `--listen` / `COOLPSONOTES_LISTEN` | `127.0.0.1:8080` | bind address (container image: `0.0.0.0:8080`) |
| `--data-dir` / `COOLPSONOTES_DATA_DIR` | `./data` | holds `coolpsonotes.db` and automatic pre-migration backups |
| `--public-url` / `COOLPSONOTES_PUBLIC_URL` | `http://localhost:8080` | external URL; used for the OIDC redirect URI and device-login links |
| `--web-dir` / `COOLPSONOTES_WEB_DIR` | unset | built browser client; unset = API only |
| `--oidc-issuer` / `COOLPSONOTES_OIDC_ISSUER` | unset | issuer URL (discovery at `/.well-known/openid-configuration`) |
| `--oidc-client-id` / `COOLPSONOTES_OIDC_CLIENT_ID` | unset | client id |
| `--oidc-client-secret` / `COOLPSONOTES_OIDC_CLIENT_SECRET` | unset | client secret (omit for a public client; PKCE is always used) |
| `--oidc-scopes` / `COOLPSONOTES_OIDC_SCOPES` | `openid profile email` | requested scopes |
| `--oidc-display-name` / `COOLPSONOTES_OIDC_DISPLAY_NAME` | `Single sign-on` | login button label |
| `--dev-mode` / `COOLPSONOTES_DEV_MODE` | `false` | **development only**: password-less login as any user name |
| `--admin` / `COOLPSONOTES_ADMINS` | empty | comma-separated `issuer\|subject` (or `dev:<name>`) administrators |
| `--first-user-admin` / `COOLPSONOTES_FIRST_USER_ADMIN` | `true` | first person to sign in becomes administrator |
| `--user-directory` / `COOLPSONOTES_USER_DIRECTORY` | `search` | `search` (name prefix or exact e-mail) or `exact-email` |
| `--session-ttl-days` | `30` | browser session lifetime |
| `--device-session-ttl-days` | `180` | CLI/desktop session lifetime |
| `--agent-token-ttl-secs` | `3600` | agent access token lifetime |
| `--max-attachment-mb` | `25` | maximum attachment size |
| `--max-update-kb` | `2048` | maximum size of one encrypted update |
| `--snapshot-retention` | `100` | revisions kept per document |
| `--trust-proxy` / `COOLPSONOTES_TRUST_PROXY` | `false` | use `X-Forwarded-For` for rate limiting (only behind your proxy) |
| `--rate-limit-rps` | `50` | API requests per second per IP (burst ×4) |
| `--auth-rate-limit-per-min` | `30` | login/token requests per minute per IP |
| `--metrics` / `COOLPSONOTES_METRICS` | `true` | expose Prometheus metrics at `/metrics` |
| `--metrics-token` / `COOLPSONOTES_METRICS_TOKEN` | unset | require `Authorization: Bearer <token>` for `/metrics` |
| `--log-format` / `COOLPSONOTES_LOG_FORMAT` | `pretty` | `json` for structured logs; level via `COOLPSONOTES_LOG` (e.g. `info,sqlx=warn`) |

The server refuses to start without a login method (OIDC or dev mode).

## 3. Identity provider setup

Create an OIDC client of type *confidential* (or public with PKCE) with:

* redirect URI: `https://notes.example.com/api/v1/auth/oidc/callback`
* grant type: authorization code; scopes `openid profile email`
* token endpoint auth: `client_secret_basic` or `client_secret_post`

Who may sign in is decided by your IdP (assign the client to a group).
Signing in never grants access to any vault by itself.

**Authelia** (`configuration.yml`):

```yaml
identity_providers:
  oidc:
    clients:
      - client_id: coolpsonotes
        client_name: coolpsonotes
        client_secret: '$pbkdf2-sha512$…'   # hash of the secret
        authorization_policy: two_factor
        redirect_uris: ['https://notes.example.com/api/v1/auth/oidc/callback']
        scopes: ['openid', 'profile', 'email']
        require_pkce: true
        pkce_challenge_method: S256
        token_endpoint_auth_method: client_secret_basic
```
Issuer: `https://auth.example.com`.

**Keycloak**: create client `coolpsonotes`, *Client authentication* on,
*Standard flow* only, valid redirect URI as above, advanced setting
*Proof Key for Code Exchange* = S256. Issuer:
`https://kc.example.com/realms/<realm>`.

**Authentik**: Applications → Providers → *OAuth2/OpenID Provider*,
confidential client, redirect URI as above, signing key selected, scopes
`openid email profile`. Issuer: `https://authentik.example.com/application/o/<slug>/`.

**Dex** (used in development): see [`deploy/dex/config.yaml`](../deploy/dex/config.yaml).

Agents (AI assistants, automation) do not use the IdP: their owners create
them in Settings → AI agents, which issues OAuth client credentials from
coolpsonotes itself (see [agents.md](agents.md)).

## 4. Reverse proxy

TLS is required in production. WebSockets live at `/api/v1/ws`; allow long
idle timeouts (the client pings every 20 s). Set `--trust-proxy`.

Caddy: [`deploy/Caddyfile`](../deploy/Caddyfile). nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_read_timeout 3600s;
    client_max_body_size 64m;
}
```

## 5. Backup and restore

Everything — accounts, memberships, encrypted notes, history, attachments and
key envelopes — is in `coolpsonotes.db`. Note content in it is end-to-end
encrypted, but the file still contains metadata; store backups securely.

```sh
# consistent online backup (safe while the server runs)
coolpsonotes-server backup --out /backups/coolpsonotes-$(date +%F).db
# or timestamped into a directory, keeping the newest 14
coolpsonotes-server backup --out-dir /backups --keep 14

# docker compose
docker compose exec coolpsonotes coolpsonotes-server backup --out-dir /data/backups --keep 14
```

Restore (stop the server first):

```sh
coolpsonotes-server restore --from /backups/coolpsonotes-2026-10-01.db
```

`restore` checks integrity, moves the current database aside
(`coolpsonotes.db.pre-restore-<timestamp>`), installs the backup and applies
migrations for the current version. Users keep their keys; their clients
notice that the server went back in time and re-upload anything newer that
they still have, so edits made after the backup are usually recovered
automatically.

Users should additionally export important vaults as Markdown (vault
settings → Export, or `coolpsonotes export`) — exports do not depend on
coolpsonotes at all.

## 6. Upgrades

1. Take a backup.
2. Replace the binary/image and start it. Pending database migrations run at
   startup; before migrating an existing database the server writes
   `data/backups/pre-migration-<timestamp>.db` automatically.
3. Clients are forward compatible within a protocol version (`/api/v1`).

`coolpsonotes-server migrate` applies migrations without starting the server.

## 7. Monitoring

* `GET /healthz` — process is alive. `GET /readyz` — database reachable.
  `coolpsonotes-server healthcheck` probes `/readyz` (used by the image).
* `GET /metrics` — Prometheus: `coolpsonotes_http_requests_total{method,status}`,
  `coolpsonotes_http_request_duration_seconds`, `coolpsonotes_updates_total`,
  `coolpsonotes_attachment_bytes_total`, `coolpsonotes_ws_connections`,
  `coolpsonotes_ws_connected_users`, `coolpsonotes_rate_limited_total{scope}`.
* Logs: one line per request (method, redacted path, status, latency) plus
  lifecycle events; `--log-format json` for log shippers.
* Administrators see accounts and storage statistics in Settings →
  Administration (never content).

## 8. Development mode

`coolpsonotes-server --dev-mode` (or `docker compose -f docker-compose.dev.yml up`)
lets anyone sign in as any user name without a password. It is meant for
trying things out and testing multi-user features locally:

* open `http://localhost:8080`, sign in as `alice`; open another tab and sign in
  as `bob` — each tab keeps its own session;
* the CLI supports the same with `coolpsonotes --profile bob login --dev-user bob`.

The server logs a warning at start and every page shows a "DEV MODE" badge.
Never expose a dev-mode server to a network you don't control.
