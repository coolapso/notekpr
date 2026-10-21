# Operating coolpsonotes

coolpsonotes is a single binary (`coolpsonotes-server`) plus static files for
the browser client. State is one SQLite database in the data directory. Run one
instance per database.

## 1. Choose a setup

| Setup | You need | Files |
|---|---|---|
| [Docker Compose](#docker-compose) | a machine with Docker | `docker-compose.yml` |
| [Kubernetes](#kubernetes) | a cluster with an ingress controller | `deploy/kubernetes/` |
| [From source](#from-source) | Rust toolchain, a service manager (systemd) | — |

Every setup also needs:

* **a reverse proxy with TLS in front** — the one you already use, or one of
  the examples in §4 (Caddy, Traefik, nginx). coolpsonotes serves plain HTTP
  and leaves certificates and HTTPS to it, by design;
* **an OpenID Connect provider** for sign-in (§3).

There are no published images yet: Compose builds the image on the machine.
The first build compiles the server and the browser client (several minutes
and around 2 GB of RAM); later builds are cached.

### How big a machine

Measured with the load test (`task load REALISTIC=1`, [load-test.md](load-test.md#realistic-people)):
people with the app open, mostly reading, now and then writing a sentence at
40 words a minute — about half a request a second each — against the server
image with Docker's limits, with a 10-second restart in every round:

| Server | People online | CPU while working | Memory | Keystroke → others, p50 / p99 | Back after the restart |
|---|---|---|---|---|---|
| ¼ core · 128 MB | 100 | 6 % | 33 MB | 1 / 83 ms | 7.5 s |
| | 300 | 13 % | 83 MB | 1 / 119 ms | 7.5 s |
| | 500 | 19 % | 125 MB | 2 / 134 ms | 7.8 s |
| | 600 | — | out of memory | | |
| ½ core · 256 MB | 500 | 21 % | 123 MB | 1 / 78 ms | 7.8 s |
| | 1,000 | 35 % | 232 MB | 2 / 146 ms | 8.0 s |
| | 1,250 | — | out of memory | | |
| 1 core · 512 MB | 1,000 | 36 % | 230 MB | 1 / 185 ms | 8.0 s |
| | 2,000 | 68 % | 437 MB | 8 / 357 ms | 7.8 s |
| | 2,500 | — | out of memory | | |
| 2 cores · 1 GB | 2,000 | 75 % | 436 MB | 12 / 267 ms | 7.8 s |
| | 3,000 | 98 % | 644 MB | 113 / 681 ms | 7.8 s |
| | 4,000 | 101 % | 862 MB | 726 ms / 16 s | 7.5 s |

(CPU: 100 % is one core; *while working* leaves out arrivals and restarts,
which take all the CPU there is for a few seconds.) What it comes to:

* **Memory** grows by about a quarter of a megabyte per person online, on
  about 10 MB: a WebSocket each, their subscriptions, and recent live events
  kept per vault. It runs out first. When it did, Docker killed the server;
  people kept writing offline, and once it could take their uploads again
  nothing was missing.
* **CPU** while people work is about 0.035–0.04 % of a core per person
  online.
* **One database writer**: every change goes through one write lock and an
  SQLite transaction, about 1,200 changes a second whatever the cores — at
  4,000 people on 2 cores it used one, and delivery fell behind. Above some
  2,500 people online, a bigger machine helps little (see TODO.md).
* People typing nonstop (the load test's default crowd, 4–5 requests a
  second each) reach that limit at about 300 people on one core.
* The numbers are the server's own: the load test ran on the same machine,
  over loopback. With TLS at a proxy and a real network, measure your own
  setup: [`task load:remote`](load-test.md#against-a-server-elsewhere) runs
  the crowd against a server elsewhere.

**Open files.** Every connected person holds a WebSocket and an HTTP
connection or two, each an open file. At start the server raises its
open-files limit to the hard limit the system allows (at most about a
million) and logs it; if that still allows fewer than about 2,700 people at
once, it warns — raise the hard limit then (`LimitNOFILE=` in a systemd unit,
`ulimits: nofile` in Compose). Past the limit the server keeps serving the
connections it has but can't accept new ones (`accept error: Too many open
files` in its log).

### Docker Compose

```sh
git clone https://github.com/coolapso/coolpsonotes && cd coolpsonotes
export COOLPSONOTES_PUBLIC_URL=https://notes.example.com   # what people type in the browser
export COOLPSONOTES_OIDC_ISSUER=https://auth.example.com   # your identity provider (§3)
export COOLPSONOTES_OIDC_CLIENT_ID=coolpsonotes
export COOLPSONOTES_OIDC_CLIENT_SECRET=…                    # from wherever you keep secrets
docker compose up -d --build  # coolpsonotes on 127.0.0.1:8080
```

Compose passes these on from the environment it runs in, along with
`COOLPSONOTES_ADMINS` and `COOLPSONOTES_METRICS_TOKEN` if set (§2). Hand
secrets over from where you keep them — a secret manager, your CI — when you
deploy, rather than writing them into a file on the machine. Settings left
unset aren't passed on: without the OIDC issuer and client the server
refuses to start (`docker compose logs`), and without the public URL sign-in
can't work.

Point your reverse proxy at `127.0.0.1:8080`, with TLS for the public URL's
host (§4). Then open the public URL and sign in: the first person to sign in
becomes the administrator (§5).

### Kubernetes

Edit and apply [`deploy/kubernetes/coolpsonotes.yaml`](../deploy/kubernetes/coolpsonotes.yaml)
(single replica, `Recreate` strategy, a `ReadWriteOnce` volume) and optionally
[`backup-cronjob.yaml`](../deploy/kubernetes/backup-cronjob.yaml). The image
it references isn't published yet: build it (`docker build -t
registry.example.com/coolpsonotes:0.1.0 .`) and push it to your registry,
then set that image in both files. Put your ingress in front of the
`coolpsonotes` Service, with TLS (§4; for Traefik:
[`deploy/examples/traefik/`](../deploy/examples/traefik/ingressroute.yaml)).

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

Every flag below can be set as an environment variable (the subcommands' own
flags, such as `backup --out`, can't). An empty value (`FOO=` in a compose or
env file) counts as not set.

| Flag / variable | Default | Meaning |
|---|---|---|
| `--listen` / `COOLPSONOTES_LISTEN` | `127.0.0.1:8080` | bind address (container image: `0.0.0.0:8080`) |
| `--data-dir` / `COOLPSONOTES_DATA_DIR` | `./data` | holds `coolpsonotes.db` and automatic pre-migration backups (container image: `/data`) |
| `--public-url` / `COOLPSONOTES_PUBLIC_URL` | `http://localhost:8080` | external URL; used for the OIDC redirect URI and device-login links |
| `--web-dir` / `COOLPSONOTES_WEB_DIR` | unset | built browser client; unset = API only (container image: `/usr/share/coolpsonotes/web`) |
| `--oidc-issuer` / `COOLPSONOTES_OIDC_ISSUER` | unset | issuer URL (discovery at `/.well-known/openid-configuration`) |
| `--oidc-client-id` / `COOLPSONOTES_OIDC_CLIENT_ID` | unset | client id |
| `--oidc-client-secret` / `COOLPSONOTES_OIDC_CLIENT_SECRET` | unset | client secret (omit for a public client; PKCE is always used) |
| `--oidc-token-auth` / `COOLPSONOTES_OIDC_TOKEN_AUTH` | `auto` | how the secret is sent: `auto` (what the provider advertises), `client_secret_basic` or `client_secret_post` |
| `--oidc-ca-file` / `COOLPSONOTES_OIDC_CA_FILE` | unset | extra CA certificates (PEM) to trust for the provider, e.g. a private CA; the system trust store is always used |
| `--oidc-allow-unverified-email` / `COOLPSONOTES_OIDC_ALLOW_UNVERIFIED_EMAIL` | `false` | also accept e-mail addresses the provider doesn't mark `email_verified` |
| `--oidc-scopes` / `COOLPSONOTES_OIDC_SCOPES` | `openid profile email` | requested scopes |
| `--oidc-display-name` / `COOLPSONOTES_OIDC_DISPLAY_NAME` | `Single sign-on` | login button label |
| `--dev-mode` / `COOLPSONOTES_DEV_MODE` | `false` | **development only**: password-less login as any user name (§9) |
| `--dev-mode-anywhere` / `COOLPSONOTES_DEV_MODE_ANYWHERE` | `false` | allow dev mode on a non-loopback address, with an https URL or with OIDC (§9) |
| `--admin` / `COOLPSONOTES_ADMINS` | empty | comma-separated `issuer\|subject` (or `dev:<name>`) administrators |
| `--first-user-admin <true\|false>` / `COOLPSONOTES_FIRST_USER_ADMIN` | `true` | first person to sign in becomes administrator |
| `--user-directory` / `COOLPSONOTES_USER_DIRECTORY` | `search` | `search` (name prefix or exact e-mail) or `exact-email` |
| `--session-ttl-days` / `COOLPSONOTES_SESSION_TTL_DAYS` | `30` | browser session lifetime |
| `--device-session-ttl-days` / `COOLPSONOTES_DEVICE_SESSION_TTL_DAYS` | `180` | CLI/desktop session lifetime |
| `--agent-token-ttl-secs` / `COOLPSONOTES_AGENT_TOKEN_TTL_SECS` | `3600` | agent access token lifetime |
| `--max-attachment-mb` / `COOLPSONOTES_MAX_ATTACHMENT_MB` | `25` | maximum attachment size |
| `--max-update-kb` / `COOLPSONOTES_MAX_UPDATE_KB` | `2048` | maximum size of one encrypted update |
| `--max-snapshot-kb` / `COOLPSONOTES_MAX_SNAPSHOT_KB` | 16 × the update limit, at least 32 MiB | maximum size of one snapshot (a note's full state; also how a change too large for one update still reaches others) |
| `--snapshot-retention` / `COOLPSONOTES_SNAPSHOT_RETENTION` | `100` | revisions kept per document (never fewer than 2) |
| `--snapshot-cooldown-secs` / `COOLPSONOTES_SNAPSHOT_COOLDOWN_SECS` | `30` | minimum time between two snapshots of one note by one person |
| `--trust-proxy` / `COOLPSONOTES_TRUST_PROXY` | `false` | use `X-Forwarded-For` (or, without a usable one, `X-Real-IP`) for rate limiting (only behind your proxy, §4) |
| `--rate-limit-rps` / `COOLPSONOTES_RATE_LIMIT_RPS` | `50` | API requests per second per IP (burst ×4) |
| `--auth-rate-limit-per-min` / `COOLPSONOTES_AUTH_RATE_LIMIT_PER_MIN` | `30` | login/token requests per minute per IP |
| `--metrics <true\|false>` / `COOLPSONOTES_METRICS` | `true` | expose Prometheus metrics at `/metrics`; `--metrics false` turns them off (`/metrics` then answers `404`) |
| `--metrics-token` / `COOLPSONOTES_METRICS_TOKEN` | unset | require `Authorization: Bearer <token>` for `/metrics` |
| `--log-format` / `COOLPSONOTES_LOG_FORMAT` | `pretty` | `json` for structured logs (container image: `json`); level via `COOLPSONOTES_LOG` (e.g. `info,sqlx=warn`) |

The server refuses to start without a login method (OIDC or dev mode).

## 3. Identity provider setup

Create an OIDC client of type *confidential* (or public with PKCE) with:

* redirect URI: `<public URL>/api/v1/auth/oidc/callback`, e.g.
  `https://notes.example.com/api/v1/auth/oidc/callback`
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
  signed by a CA you pass with `--oidc-ca-file` (e.g. the internal root of
  the Caddy example on a home network, §4).
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

## 4. Reverse proxy and TLS

coolpsonotes serves plain HTTP. TLS — required in production — comes from a
reverse proxy in front of it, whichever you use. What the server needs from
it:

* **HTTPS** for the public URL's host, forwarding every path to the server (Compose:
  `127.0.0.1:8080`; Kubernetes: the `coolpsonotes` Service) — except
  `/metrics` (below).
* **WebSockets** at `/api/v1/ws`, open for a long time: allow idle timeouts
  well above 30 s (the browser pings every 20 s, the CLI and the server every
  30 s).
* **Request bodies** of at least the larger of `--max-attachment-mb` and 4/3 ×
  the snapshot limit (`--max-snapshot-kb`; snapshots travel base64-encoded):
  about 43 MiB with the defaults.
* **The client's address**: set `--trust-proxy` (the Compose file and the
  Kubernetes manifest do). The server then takes it from the **last** entry
  of `X-Forwarded-For` (the one your proxy added) for rate limiting, or from
  `X-Real-IP` when there's no usable `X-Forwarded-For`, so the proxy must
  replace or remove an `X-Real-IP` a client sent. If several proxies are
  chained, the outermost must be the only one that adds an entry, or set the
  header to the client address at the edge.
* **`/metrics` kept inside**: don't route it from the internet; scrape the
  server directly, or set `--metrics-token`.

Plain `http://` on a LAN is possible but not recommended: browsers restrict
some features to secure origins (copying the recovery key to the clipboard,
"Keep this browser usable offline", which needs a service worker), the CLI
refuses plain HTTP to non-local servers unless `--insecure-http`, and
sessions travel unencrypted.

### Examples

All optional: use them as they are, adapt them, or ignore them.

**Caddy, with Docker Compose** ([`deploy/examples/caddy/`](../deploy/examples/caddy/)):
automatic HTTPS from Let's Encrypt, for a domain with DNS pointing at the
machine and ports 80 and 443 reachable from the internet:

```sh
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml up -d --build
```

On a home network without public DNS, Caddy can issue certificates from its
own certificate authority instead:

```sh
export COOLPSONOTES_PUBLIC_URL=https://notes.home.arpa   # any name your devices resolve to this machine (router DNS, /etc/hosts)
export CADDYFILE=deploy/examples/caddy/Caddyfile.lan
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml up -d --build
# Caddy's root certificate, to install once on every device that uses coolpsonotes:
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml cp caddy:/data/caddy/pki/authorities/local/root.crt ./caddy-root.crt
```

Install `caddy-root.crt` as a trusted root on each computer and phone (and on
the machine running your identity provider's clients, if it uses the same
CA). The CLI uses the operating system's trust store, so it works once the
root is installed there.

**Traefik on Kubernetes** ([`deploy/examples/traefik/ingressroute.yaml`](../deploy/examples/traefik/ingressroute.yaml)):
an IngressRoute for the `coolpsonotes` Service with TLS from a cert-manager
Certificate (or your own) and `/metrics` left out. Its comments say what to
check on the entry point: that it knows the client's real address, and its
timeouts for long-lived WebSockets and large uploads.

**nginx** (the web server, e.g. on the same machine as Compose):

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

(The ingress-nginx controller for Kubernetes was retired in March 2026 — no
releases or security fixes since — so there's no example for it.)

### Holding requests during an update

An update replaces the server: for a moment — seconds, mostly what Docker or
Kubernetes need to swap the container — nothing answers. The apps are fine
with that (§7). A proxy, though, usually answers requests meanwhile with an
error (502, 503 or 404), which is what someone opening the page or signing
in right then sees. Some proxies can hold such a request instead and try
again until the new server answers, so it only takes a moment longer. Only
requests that couldn't reach the server are tried again: nothing is sent
twice.

Measured with 120 requests sent while the server's container was swapped
(outside Kubernetes):

| Proxy | Requests that failed |
|---|---|
| Caddy, as it is by default | 33 (502) |
| Caddy holding requests (`lb_try_duration 15s`, `lb_try_interval 250ms`, as in the example) | 0 |
| Traefik with the server at a fixed address (file provider) and its `retry` middleware | 0 |

On Kubernetes, with `strategy: Recreate`, there is a moment without a ready
pod, and an ingress usually has nothing to send requests to. A possibility,
**not tested yet**: Traefik's `retry` middleware with the route sent to the
Service's own address (`nativeLB: true`) instead of to its pods — commented
out in the Traefik example. Whether it holds requests depends on how the
cluster routes Service traffic, so try it before relying on it.

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
`--out-dir` names files `coolpsonotes-YYYYMMDD-HHMMSS.db` (UTC); `--keep`
prunes only files named `coolpsonotes-*.db` there, so the automatic
`pre-migration-*.db` backups (§7), which land in `<data dir>/backups`, stay
until you delete them.

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

**One person's data.** `export-user` writes one person's encrypted export —
the same as `GET /me/export`: their vaults and the notes shared with them, as
stored — to a new file (mode 0600), from the live database or from a backup:

```sh
coolpsonotes-server export-user --data-dir /var/lib/coolpsonotes --user alice@example.com --out alice.json
coolpsonotes-server export-user --backup /backups/coolpsonotes-2026-10-01.db --user alice@example.com --out alice.json

# docker compose
docker compose exec coolpsonotes coolpsonotes-server export-user --user alice@example.com --out /data/alice.json
docker compose cp coolpsonotes:/data/alice.json ./alice.json
```

`--user` takes a user id, an e-mail address or a display name. A backup is
read through a migrated temporary copy; the backup itself isn't changed.
Nothing in the file is readable without the person's keys: they decrypt it
offline with `coolpsonotes recover` ([recovery.md](recovery.md)).

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

**What people notice.** On SIGTERM the server tells every connected app it is
restarting, finishes the requests under way and exits; the new one is up
within about a second. Meanwhile people keep working: their apps keep every
change and send it once the server is back, trying again about every second
rather than backing off, and the browser shows *Connecting* instead of
*Offline* (for up to 15 s). In the load test, an update with 1,500 people
online had no errors, and everyone was connected again about 2 s after the
new server answered — some 6 s in all, most of it Docker swapping the
container. Someone opening the page during those seconds gets whatever the
proxy answers then (§4, "Holding requests during an update").

## 8. Monitoring

* `GET /healthz` — process is alive. `GET /readyz` — database reachable.
  `coolpsonotes-server healthcheck` probes `/readyz` (used by the image).
* `GET /metrics` — Prometheus: `coolpsonotes_http_requests_total{method,status}`
  (`status` is the class: `2xx`, `4xx`, …),
  `coolpsonotes_http_request_duration_seconds` (a summary with quantiles: no
  histogram buckets are configured), `coolpsonotes_updates_total`,
  `coolpsonotes_attachment_bytes_total`, `coolpsonotes_ws_connections`,
  `coolpsonotes_ws_connected_users`, `coolpsonotes_rate_limited_total{scope}`,
  and on Linux the process's own `process_cpu_seconds_total` (a gauge) and
  `process_resident_memory_bytes`. Keep it internal (§4).
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
