# Operating coolpsonotes

coolpsonotes is a single binary (`coolpsonotes-server`) plus static files for
the browser client. State is one SQLite database in the data directory. Run one
instance per database.

## 1. Choose a setup

| Setup | You need | Files |
|---|---|---|
| [Public server with automatic HTTPS](#public-server-with-automatic-https) | a domain with DNS pointing at the machine, ports 80 and 443 reachable from the internet, Docker | `docker-compose.yml`, `deploy/Caddyfile` |
| [On your home network](#on-your-home-network) | a machine on your LAN, Docker; no public DNS, no open ports | `docker-compose.yml`, `deploy/Caddyfile.lan` |
| [Behind a reverse proxy you already run](#behind-your-own-reverse-proxy) | nginx, Traefik, an existing Caddy, … with TLS | `docker-compose.yml`, `deploy/compose.local-port.yml` |
| [Kubernetes](#kubernetes) | a cluster with an ingress controller | `deploy/kubernetes/` |
| [From source](#from-source) | Rust toolchain, a service manager (systemd) | — |

Every setup also needs an **OpenID Connect provider** for sign-in (§3).

There are no published images yet: Compose builds the image on the machine.
The first build compiles the server and the browser client (several minutes
and around 2 GB of RAM); later builds are cached.

### Public server with automatic HTTPS

```sh
git clone https://github.com/coolapso/coolpsonotes && cd coolpsonotes
cp .env.example .env          # PUBLIC_HOST and your OIDC client (§3)
docker compose up -d --build  # coolpsonotes + Caddy, which gets certificates itself
```

Open `https://<PUBLIC_HOST>` and sign in: the first person to sign in becomes
the administrator (§5).

### On your home network

Caddy can issue certificates from its own certificate authority, so HTTPS
works without a public name. In `.env`:

```sh
PUBLIC_HOST=notes.home.arpa     # any name your devices resolve to this machine (router DNS, /etc/hosts)
CADDYFILE=deploy/Caddyfile.lan
```

```sh
docker compose up -d --build
# Caddy's root certificate, to install once on every device that uses coolpsonotes:
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ./caddy-root.crt
```

Install `caddy-root.crt` as a trusted root on each computer and phone (and on
the machine running your identity provider's clients, if it uses the same
CA). The CLI uses the operating system's trust store, so it works once the
root is installed there.

Plain `http://` on a LAN is possible but not recommended: browsers restrict
some features to secure origins (copying the recovery key to the clipboard),
the CLI refuses plain HTTP to non-local servers unless `--insecure-http`, and
sessions travel unencrypted.

### Behind your own reverse proxy

Run only the coolpsonotes service and publish it on `127.0.0.1:8080`:

```sh
docker compose -f docker-compose.yml -f deploy/compose.local-port.yml up -d --build coolpsonotes
```

Then point your proxy at `127.0.0.1:8080` with TLS for `PUBLIC_HOST` (§4).

### Kubernetes

Edit and apply [`deploy/kubernetes/coolpsonotes.yaml`](../deploy/kubernetes/coolpsonotes.yaml)
(single replica, `Recreate` strategy, a `ReadWriteOnce` volume) and optionally
[`backup-cronjob.yaml`](../deploy/kubernetes/backup-cronjob.yaml). The image
it references isn't published yet: build it (`docker build -t
registry.example.com/coolpsonotes:0.1.0 .`) and push it to your registry,
then set that image in both files. Make sure the ingress allows long-lived
WebSocket connections and request bodies larger than `--max-attachment-mb`.

### From source

```sh
cargo build --release --locked -p coolpsonotes-server
./scripts/build-web.sh                 # needs the wasm32 target and wasm-bindgen 0.2.128
sudo install -Dm755 target/release/coolpsonotes-server /usr/local/bin/coolpsonotes-server
sudo mkdir -p /usr/local/share/coolpsonotes && sudo cp -r dist/web /usr/local/share/coolpsonotes/
```

Serve a copy of the web client outside the checkout: `build-web.sh` replaces
`dist/web` every time it runs (also during development).

```ini
# /etc/systemd/system/coolpsonotes.service
[Unit]
Description=coolpsonotes
After=network-online.target
[Service]
ExecStart=/usr/local/bin/coolpsonotes-server
EnvironmentFile=/etc/coolpsonotes.env
DynamicUser=yes
StateDirectory=coolpsonotes
Environment=COOLPSONOTES_DATA_DIR=/var/lib/coolpsonotes
Environment=COOLPSONOTES_WEB_DIR=/usr/local/share/coolpsonotes/web
Restart=on-failure
[Install]
WantedBy=multi-user.target
```

```sh
# /etc/coolpsonotes.env (mode 0600): secrets don't belong on the command line
COOLPSONOTES_PUBLIC_URL=https://notes.example.com
COOLPSONOTES_OIDC_ISSUER=https://auth.example.com
COOLPSONOTES_OIDC_CLIENT_ID=coolpsonotes
COOLPSONOTES_OIDC_CLIENT_SECRET=…
COOLPSONOTES_TRUST_PROXY=true
```

Put a TLS reverse proxy in front (§4).

## 2. Configuration

Every flag can be set as an environment variable. An empty value (`FOO=` in a
compose or env file) counts as not set.

| Flag / variable | Default | Meaning |
|---|---|---|
| `--listen` / `COOLPSONOTES_LISTEN` | `127.0.0.1:8080` | bind address (container image: `0.0.0.0:8080`) |
| `--data-dir` / `COOLPSONOTES_DATA_DIR` | `./data` | holds `coolpsonotes.db` and automatic pre-migration backups |
| `--public-url` / `COOLPSONOTES_PUBLIC_URL` | `http://localhost:8080` | external URL; used for the OIDC redirect URI and device-login links |
| `--web-dir` / `COOLPSONOTES_WEB_DIR` | unset | built browser client; unset = API only |
| `--oidc-issuer` / `COOLPSONOTES_OIDC_ISSUER` | unset | issuer URL (discovery at `/.well-known/openid-configuration`) |
| `--oidc-client-id` / `COOLPSONOTES_OIDC_CLIENT_ID` | unset | client id |
| `--oidc-client-secret` / `COOLPSONOTES_OIDC_CLIENT_SECRET` | unset | client secret (omit for a public client; PKCE is always used) |
| `--oidc-token-auth` / `COOLPSONOTES_OIDC_TOKEN_AUTH` | `auto` | how the secret is sent: `auto` (what the provider advertises), `client_secret_basic` or `client_secret_post` |
| `--oidc-ca-file` / `COOLPSONOTES_OIDC_CA_FILE` | unset | extra CA certificates (PEM) to trust for the provider, e.g. a private CA; the system trust store is always used |
| `--oidc-allow-unverified-email` / `COOLPSONOTES_OIDC_ALLOW_UNVERIFIED_EMAIL` | `false` | also accept e-mail addresses the provider doesn't mark `email_verified` |
| `--oidc-scopes` / `COOLPSONOTES_OIDC_SCOPES` | `openid profile email` | requested scopes |
| `--oidc-display-name` / `COOLPSONOTES_OIDC_DISPLAY_NAME` | `Single sign-on` | login button label |
| `--dev-mode` / `COOLPSONOTES_DEV_MODE` | `false` | **development only**: password-less login as any user name (§8) |
| `--dev-mode-anywhere` / `COOLPSONOTES_DEV_MODE_ANYWHERE` | `false` | allow dev mode on a non-loopback address, with an https URL or with OIDC (§8) |
| `--admin` / `COOLPSONOTES_ADMINS` | empty | comma-separated `issuer\|subject` (or `dev:<name>`) administrators |
| `--first-user-admin` / `COOLPSONOTES_FIRST_USER_ADMIN` | `true` | first person to sign in becomes administrator |
| `--user-directory` / `COOLPSONOTES_USER_DIRECTORY` | `search` | `search` (name prefix or exact e-mail) or `exact-email` |
| `--session-ttl-days` / `COOLPSONOTES_SESSION_TTL_DAYS` | `30` | browser session lifetime |
| `--device-session-ttl-days` / `COOLPSONOTES_DEVICE_SESSION_TTL_DAYS` | `180` | CLI/desktop session lifetime |
| `--agent-token-ttl-secs` / `COOLPSONOTES_AGENT_TOKEN_TTL_SECS` | `3600` | agent access token lifetime |
| `--max-attachment-mb` / `COOLPSONOTES_MAX_ATTACHMENT_MB` | `25` | maximum attachment size |
| `--max-update-kb` / `COOLPSONOTES_MAX_UPDATE_KB` | `2048` | maximum size of one encrypted update |
| `--snapshot-retention` / `COOLPSONOTES_SNAPSHOT_RETENTION` | `100` | revisions kept per document |
| `--snapshot-cooldown-secs` / `COOLPSONOTES_SNAPSHOT_COOLDOWN_SECS` | `30` | minimum time between two snapshots of one note by one person |
| `--trust-proxy` / `COOLPSONOTES_TRUST_PROXY` | `false` | use `X-Forwarded-For` for rate limiting (only behind your proxy, §4) |
| `--rate-limit-rps` / `COOLPSONOTES_RATE_LIMIT_RPS` | `50` | API requests per second per IP (burst ×4) |
| `--auth-rate-limit-per-min` / `COOLPSONOTES_AUTH_RATE_LIMIT_PER_MIN` | `30` | login/token requests per minute per IP |
| `--metrics` / `COOLPSONOTES_METRICS` | `true` | expose Prometheus metrics at `/metrics` |
| `--metrics-token` / `COOLPSONOTES_METRICS_TOKEN` | unset | require `Authorization: Bearer <token>` for `/metrics` |
| `--log-format` / `COOLPSONOTES_LOG_FORMAT` | `pretty` | `json` for structured logs; level via `COOLPSONOTES_LOG` (e.g. `info,sqlx=warn`) |

The server refuses to start without a login method (OIDC or dev mode).

## 3. Identity provider setup

Create an OIDC client of type *confidential* (or public with PKCE) with:

* redirect URI: `https://<PUBLIC_HOST>/api/v1/auth/oidc/callback`
* grant type: authorization code; scopes `openid profile email`
* token endpoint auth: `client_secret_basic` or `client_secret_post` — the
  server uses what the provider advertises (`--oidc-token-auth` to force one)

Requirements and behaviour:

* **Reachable from the server.** The server fetches the provider's discovery
  document, keys and tokens itself, at the issuer URL you configure. Inside a
  container, `localhost` is the container: use a name or address the
  container can reach. Browsers are sent to the provider's own authorization
  URL, so that must be reachable from people's devices.
* **Trusted certificate.** The provider's certificate must be trusted by the
  server's operating system (inside the image: the standard CA bundle), or be
  signed by a CA you pass with `--oidc-ca-file` (e.g. Caddy's internal root
  for a LAN install).
* **Name and e-mail** come from the ID token's `name` / `preferred_username`
  and `email` claims, or from the userinfo endpoint when the ID token doesn't
  carry them. E-mail addresses are used only when `email_verified` is true
  (they are how people find each other to share), unless
  `--oidc-allow-unverified-email`.

Who may sign in is decided by your identity provider (assign the client to a
group). Signing in never grants access to any vault by itself.

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
        token_endpoint_auth_method: client_secret_basic   # client_secret_post works too
```

Issuer: `https://auth.example.com`. Recent Authelia versions put the profile
only in the userinfo response by default; coolpsonotes reads it from there.

**Keycloak**: create client `coolpsonotes`, *Client authentication* on,
*Standard flow* only, valid redirect URI as above, advanced setting
*Proof Key for Code Exchange* = S256. Issuer:
`https://kc.example.com/realms/<realm>`.

**Authentik**: Applications → Providers → *OAuth2/OpenID Provider*,
confidential client, redirect URI as above, signing key selected, scopes
`openid email profile`. Issuer: `https://authentik.example.com/application/o/<slug>/`.

**Dex** (used in development): see [`deploy/dex/config.yaml`](../deploy/dex/config.yaml).

Agents (AI assistants, automation) do not use the identity provider: their
owners create them in Settings → AI agents, which issues OAuth client
credentials from coolpsonotes itself (see [agents.md](agents.md)).

## 4. Reverse proxy

TLS is required in production. WebSockets live at `/api/v1/ws`; allow long
idle timeouts (the client pings every 20 s). Allow request bodies a little
larger than `--max-attachment-mb`. Set `--trust-proxy`: the server then takes
the client's address from the **last** entry of `X-Forwarded-For` (the one
your proxy added) for rate limiting. If several proxies are chained, the
outermost must be the only one that adds an entry, or set the header to the
client address at the edge.

Don't expose `/metrics` publicly (the bundled Caddyfiles answer it with 404;
scrape the server directly or set `--metrics-token`).

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
location /metrics { return 404; }
```

## 5. The first administrator

The first person to sign in through your identity provider becomes the
administrator (accounts created in development mode don't count). Sign in
yourself right after installing. Administrators manage accounts in Settings →
Administration, including giving other people the administrator role; they
never see content.

To name administrators up front instead, set `COOLPSONOTES_ADMINS` to
`issuer|subject` pairs: the issuer URL and your account's `sub` claim at the
provider (in Keycloak, the user's ID; for other providers see their
documentation). Signing in first is usually simpler. Removing someone from
`COOLPSONOTES_ADMINS` does not take the role away; do that in Settings →
Administration.

Don't reuse a development-mode data directory for real use: start with an
empty one.

## 6. Backup and restore

Everything — accounts, memberships, encrypted notes, history, attachments and
key envelopes — is in `coolpsonotes.db`. Note content in it is end-to-end
encrypted, but the file still contains metadata; store backups securely.

```sh
# consistent online backup (safe while the server runs)
coolpsonotes-server backup --data-dir /var/lib/coolpsonotes --out /backups/coolpsonotes-$(date +%F).db
# or timestamped into a directory (created if missing), keeping the newest 14
coolpsonotes-server backup --data-dir /var/lib/coolpsonotes --out-dir /backups --keep 14
```

`--out` needs an existing directory and a file name that doesn't exist yet.

**Docker Compose**: backups land in the data volume; copy them off the
machine.

```sh
docker compose exec coolpsonotes coolpsonotes-server backup --out-dir /data/backups --keep 14
docker compose cp coolpsonotes:/data/backups ./backups      # then store ./backups elsewhere
```

Schedule it with cron or a systemd timer on the host, for example daily:

```
15 3 * * *  cd /srv/coolpsonotes && docker compose exec -T coolpsonotes coolpsonotes-server backup --out-dir /data/backups --keep 14 && docker compose cp coolpsonotes:/data/backups /srv/backups/coolpsonotes
```

If you replace the named volume with a host directory (`./data:/data`), make
it writable by the image's user: `sudo chown -R 65532:65532 ./data`.

**Restore** (stop the server first):

```sh
coolpsonotes-server restore --data-dir /var/lib/coolpsonotes --from /backups/coolpsonotes-2026-10-01.db

# docker compose
docker compose stop coolpsonotes
docker compose cp ./backups/coolpsonotes-2026-10-01.db coolpsonotes:/data/backups/restore.db
docker compose run --rm --no-deps coolpsonotes restore --from /data/backups/restore.db
docker compose start coolpsonotes
```

The backup file and its directory must be writable by the server's user
during the restore. `restore` checks integrity, moves the current database
aside (`coolpsonotes.db.pre-restore-<timestamp>`), installs the backup and
applies migrations for the current version.

What a restore means for people: notes come back as they were in the backup,
and clients that still have newer edits re-upload them automatically (they
notice the server went back in time). Everything else created after the
backup is gone from the server: accounts, devices and sessions (people may
need to sign in again and re-approve devices), memberships and shares, key
rotations, agent credentials. Members' own keys are unaffected.

Users should additionally export important vaults as Markdown (vault
settings → Export, or `coolpsonotes export`) — exports do not depend on
coolpsonotes at all.

## 7. Upgrades

1. Take a backup (§6).
2. Install the new version and start it. Pending database migrations run at
   startup; before migrating an existing database the server writes
   `data/backups/pre-migration-<timestamp>.db` automatically.

   ```sh
   # docker compose
   git pull && docker compose build --pull && docker compose up -d
   # from source
   git pull && cargo build --release --locked -p coolpsonotes-server && ./scripts/build-web.sh
   # then install the binary and web client as in §1 and restart the service
   ```
3. Clients are forward compatible within a protocol version (`/api/v1`).

`coolpsonotes-server migrate` applies migrations without starting the server.

## 8. Monitoring

* `GET /healthz` — process is alive. `GET /readyz` — database reachable.
  `coolpsonotes-server healthcheck` probes `/readyz` (used by the image).
* `GET /metrics` — Prometheus: `coolpsonotes_http_requests_total{method,status}`,
  `coolpsonotes_http_request_duration_seconds`, `coolpsonotes_updates_total`,
  `coolpsonotes_attachment_bytes_total`, `coolpsonotes_ws_connections`,
  `coolpsonotes_ws_connected_users`, `coolpsonotes_rate_limited_total{scope}`.
  Keep it internal (§4).
* Logs: one line per request (method, redacted path, status, latency) plus
  lifecycle events; `--log-format json` for log shippers.
* Administrators see accounts and storage statistics in Settings →
  Administration (never content).

## 9. Development mode

`coolpsonotes-server --dev-mode` (or `docker compose -f docker-compose.dev.yml up`,
or `task dev`) lets anyone sign in as any user name without a password. It is
meant for trying things out and testing multi-user features locally:

* open `http://localhost:8080`, sign in as `alice`; open another tab and sign in
  as `bob` — each tab keeps its own session;
* the CLI supports the same with
  `coolpsonotes --profile bob login --server http://localhost:8080 --dev-user bob`.

Safety rails:

* The server refuses to start in dev mode if it listens on a non-loopback
  address, has an `https` public URL or has OIDC configured — unless
  `--dev-mode-anywhere`. The dev compose file sets it, because its port is
  published on `127.0.0.1` only.
* `docker-compose.dev.yml` is its own compose project (`coolpsonotes-dev`), so
  it can't replace production containers on the same machine.
* When the server runs without dev mode, sessions of accounts created in dev
  mode are ended, and those accounts never count as "the first user" for the
  administrator role.
* The server logs a warning at start and every page shows a "DEV MODE" badge.

Never expose a dev-mode server to a network you don't control.
