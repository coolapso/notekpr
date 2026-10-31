# Operating notekpr

notekpr is a single binary (`notekpr-server`) plus static files for
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
  the examples in §4 (Caddy, Traefik, nginx). notekpr serves plain HTTP
  and leaves certificates and HTTPS to it, by design;
* **an OpenID Connect provider** for sign-in (§3).

The server is published as a container image for amd64 and arm64,
`ghcr.io/coolapso/notekpr`, tagged with each release's version (`0.6.0`)
and `latest`. The Compose file and the Kubernetes manifests use `latest`:
pin a version to choose when you upgrade (§7).

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
  2,500 people online, a bigger machine helps little.
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
git clone https://github.com/coolapso/notekpr && cd notekpr
export NOTEKPR_PUBLIC_URL=https://notes.example.com   # what people type in the browser
export NOTEKPR_OIDC_ISSUER=https://auth.example.com   # your identity provider (§3)
export NOTEKPR_OIDC_CLIENT_ID=notekpr
export NOTEKPR_OIDC_CLIENT_SECRET=…                    # from wherever you keep secrets
docker compose up -d          # notekpr on 127.0.0.1:8080
```

Compose needs only `docker-compose.yml`: downloading that file works as
well as cloning (the clone also has the reverse proxy examples of §4).

Compose passes these on from the environment it runs in, along with
`NOTEKPR_ADMINS` and `NOTEKPR_METRICS_TOKEN` if set (§2). Hand
secrets over from where you keep them — a secret manager, your CI — when you
deploy, rather than writing them into a file on the machine. Settings left
unset aren't passed on: without the OIDC issuer and client the server
refuses to start (`docker compose logs`), and without the public URL sign-in
can't work.

Point your reverse proxy at `127.0.0.1:8080`, with TLS for the public URL's
host (§4). Then open the public URL and sign in: the first person to sign in
becomes the administrator (§5).

### Kubernetes

Edit and apply [`deploy/kubernetes/notekpr.yaml`](../deploy/kubernetes/notekpr.yaml)
(single replica, `Recreate` strategy, a `ReadWriteOnce` volume) and optionally
[`backup-cronjob.yaml`](../deploy/kubernetes/backup-cronjob.yaml). Both use
`ghcr.io/coolapso/notekpr:latest`: pin a version in both to choose when you
upgrade (§7). Put your ingress in front of the `notekpr` Service, with TLS
(§4; for Traefik:
[`deploy/examples/traefik/`](../deploy/examples/traefik/ingressroute.yaml)).

To run an image of your own instead (a fork, a patch), build it, push it to
your registry and set it in both files. `docker buildx build --platform
linux/amd64,linux/arm64 -t registry.example.com/notekpr:0.6.0 --push .`
builds and pushes both architectures in one image. The Dockerfile
cross-compiles, so this doesn't need QEMU and takes about as long as one
native build per architecture.

### From source

```sh
cargo build --release --locked -p notekpr-server
./scripts/build-web.sh                 # needs the wasm32 target and wasm-bindgen 0.2.128
sudo install -Dm755 target/release/notekpr-server /usr/local/bin/notekpr-server
sudo mkdir -p /usr/local/share/notekpr && sudo cp -r dist/web /usr/local/share/notekpr/
```

Serve a copy of the web client outside the checkout: `build-web.sh` replaces
`dist/web` every time it runs (also during development). Builds from source
say they are version `dev`; to have them name a release, set
`NOTEKPR_VERSION` (say `NOTEKPR_VERSION=0.2.0`) for both commands.

```ini
# /etc/systemd/system/notekpr.service
[Unit]
Description=notekpr
After=network-online.target
[Service]
ExecStart=/usr/local/bin/notekpr-server
EnvironmentFile=/etc/notekpr.env
DynamicUser=yes
StateDirectory=notekpr
Environment=NOTEKPR_DATA_DIR=/var/lib/notekpr
Environment=NOTEKPR_WEB_DIR=/usr/local/share/notekpr/web
Restart=on-failure
[Install]
WantedBy=multi-user.target
```

```sh
# /etc/notekpr.env (mode 0600): secrets don't belong on the command line
NOTEKPR_PUBLIC_URL=https://notes.example.com
NOTEKPR_OIDC_ISSUER=https://auth.example.com
NOTEKPR_OIDC_CLIENT_ID=notekpr
NOTEKPR_OIDC_CLIENT_SECRET=…
NOTEKPR_TRUST_PROXY=true
```

Put a TLS reverse proxy in front (§4).

## 2. Configuration

Every flag below can be set as an environment variable (the subcommands' own
flags, such as `backup --out`, can't). An empty value (`FOO=` in a compose or
env file) counts as not set.

| Flag / variable | Default | Meaning |
|---|---|---|
| `--listen` / `NOTEKPR_LISTEN` | `127.0.0.1:8080` | bind address (container image: `0.0.0.0:8080`) |
| `--data-dir` / `NOTEKPR_DATA_DIR` | `./data` | holds `notekpr.db`, the attachments (`files/`, unless `--file-store` says otherwise) and automatic pre-migration backups (container image: `/data`) |
| `--public-url` / `NOTEKPR_PUBLIC_URL` | `http://localhost:8080` | external URL; used for the OIDC redirect URI and device-login links |
| `--web-dir` / `NOTEKPR_WEB_DIR` | unset | built browser client; unset = API only (container image: `/usr/share/notekpr/web`) |
| `--oidc-issuer` / `NOTEKPR_OIDC_ISSUER` | unset | issuer URL (discovery at `/.well-known/openid-configuration`) |
| `--oidc-client-id` / `NOTEKPR_OIDC_CLIENT_ID` | unset | client id |
| `--oidc-client-secret` / `NOTEKPR_OIDC_CLIENT_SECRET` | unset | client secret (omit for a public client; PKCE is always used) |
| `--oidc-token-auth` / `NOTEKPR_OIDC_TOKEN_AUTH` | `auto` | how the secret is sent: `auto` (what the provider advertises), `client_secret_basic` or `client_secret_post` |
| `--oidc-ca-file` / `NOTEKPR_OIDC_CA_FILE` | unset | extra CA certificates (PEM) to trust for the provider, e.g. a private CA; the system trust store is always used |
| `--oidc-allow-unverified-email` / `NOTEKPR_OIDC_ALLOW_UNVERIFIED_EMAIL` | `false` | also accept e-mail addresses the provider doesn't mark `email_verified` |
| `--oidc-scopes` / `NOTEKPR_OIDC_SCOPES` | `openid profile email` | requested scopes |
| `--oidc-display-name` / `NOTEKPR_OIDC_DISPLAY_NAME` | `Single sign-on` | login button label |
| `--dev-mode` / `NOTEKPR_DEV_MODE` | `false` | **development only**: password-less login as any user name (§9) |
| `--dev-mode-anywhere` / `NOTEKPR_DEV_MODE_ANYWHERE` | `false` | allow dev mode on a non-loopback address, with an https URL or with OIDC (§9) |
| `--admin` / `NOTEKPR_ADMINS` | empty | comma-separated `issuer\|subject` (or `dev:<name>`) administrators |
| `--first-user-admin <true\|false>` / `NOTEKPR_FIRST_USER_ADMIN` | `true` | first person to sign in becomes administrator |
| `--adminless <true\|false>` / `NOTEKPR_ADMINLESS` | `false` | no administrators among the people: nobody signing in administers anything (the admin role, `--admin` and `--first-user-admin` do nothing; the web app has no Administration); only admin clients do (§5) |
| `--user-directory` / `NOTEKPR_USER_DIRECTORY` | `search` | `search` (name prefix or exact e-mail) or `exact-email` |
| `--session-ttl-days` / `NOTEKPR_SESSION_TTL_DAYS` | `30` | browser session lifetime |
| `--device-session-ttl-days` / `NOTEKPR_DEVICE_SESSION_TTL_DAYS` | `180` | CLI/desktop session lifetime |
| `--agent-token-ttl-secs` / `NOTEKPR_AGENT_TOKEN_TTL_SECS` | `3600` | agent access token lifetime |
| `--max-attachment-mb` / `NOTEKPR_MAX_ATTACHMENT_MB` | `25` | maximum attachment size |
| `--max-update-kb` / `NOTEKPR_MAX_UPDATE_KB` | `2048` | maximum size of one encrypted update |
| `--max-snapshot-kb` / `NOTEKPR_MAX_SNAPSHOT_KB` | 16 × the update limit, at least 32 MiB | maximum size of one snapshot (a note's full state; also how a change too large for one update still reaches others) |
| `--snapshot-retention` / `NOTEKPR_SNAPSHOT_RETENTION` | `100` | revisions kept per document (never fewer than 2) |
| `--snapshot-cooldown-secs` / `NOTEKPR_SNAPSHOT_COOLDOWN_SECS` | `30` | minimum time between two snapshots of one note by one person |
| `--purge-undo-days` / `NOTEKPR_PURGE_UNDO_DAYS` | `30` | how long something deleted for good (emptying the trash) is kept so the vault's owner and admins can undo it; then it is deleted. `0`: deleted at once. It takes room meanwhile |
| `--file-store` / `NOTEKPR_FILE_STORE` | `<data dir>/files` | where attachments are kept: a directory, `s3://bucket/prefix` (S3 or anything compatible), or `database` (inside `notekpr.db`, so a backup is one file); see [Where attachments are kept](#where-attachments-are-kept) |
| `--keep-deleted-files-days` / `NOTEKPR_KEEP_DELETED_FILES_DAYS` | `14` | how long the file store keeps a file after the database let go of it (deleted for good and no longer undoable, or its vault deleted), so that restoring a database backup that recent finds its files: at least as long as you keep backups |
| `--trust-proxy` / `NOTEKPR_TRUST_PROXY` | `false` | use `X-Forwarded-For` (or, without a usable one, `X-Real-IP`) for rate limiting (only behind your proxy, §4) |
| `--rate-limit-rps` / `NOTEKPR_RATE_LIMIT_RPS` | `50` | API requests per second per IP (burst ×4) |
| `--auth-rate-limit-per-min` / `NOTEKPR_AUTH_RATE_LIMIT_PER_MIN` | `30` | login/token requests per minute per IP |
| `--metrics <true\|false>` / `NOTEKPR_METRICS` | `true` | expose Prometheus metrics at `/metrics`; `--metrics false` turns them off (`/metrics` then answers `404`) |
| `--metrics-token` / `NOTEKPR_METRICS_TOKEN` | unset | require `Authorization: Bearer <token>` for `/metrics` |
| `--log-format` / `NOTEKPR_LOG_FORMAT` | `pretty` | `json` for structured logs (container image: `json`); level via `NOTEKPR_LOG` (e.g. `info,sqlx=warn`) |

The server refuses to start without a login method (OIDC or dev mode).

### Where attachments are kept

Attachments (files and pictures in notes) are end-to-end encrypted like
everything else; the server keeps their bytes in a **file store**, and
everything about them (size, who added them, when, deleted for good) in the
database. `--file-store` says which store:

* **A folder** (the default: `files/` in the data directory). Files are
  named by vault and attachment ids plus a random part, and synced to disk
  before an upload is answered.
* **S3 or anything compatible** (Garage, Ceph, MinIO, Cloudflare R2,
  Backblaze B2, Hetzner, …): `--file-store s3://bucket/prefix`, with the
  usual variables for the rest:

  ```sh
  NOTEKPR_FILE_STORE=s3://notekpr/family
  AWS_ACCESS_KEY_ID=…  AWS_SECRET_ACCESS_KEY=…   # from your secret store
  AWS_REGION=eu-central-1
  AWS_ENDPOINT=https://s3.example.com          # not for AWS itself
  AWS_ALLOW_HTTP=true                          # only for a plain-HTTP endpoint
  ```

  Requests use path-style addressing (`AWS_VIRTUAL_HOSTED_STYLE_REQUEST=true`
  for the other). Several servers can share a bucket, each under its own
  prefix: a server only ever deletes files it wrote, and never lists the
  bucket. Never point two servers at the same prefix.
* **The database** (`--file-store database`): inside `notekpr.db`, where
  every attachment used to be. A backup is then one file, at the cost of a
  larger database.

The store holds only ciphertext under random names; it learns sizes and
times, which the database holds anyway
([security.md](security.md#where-the-metadata-is-and-who-can-see-it)).
Downloads always go through the server, never straight from the bucket.

**Moving.** Attachments kept in the database move to a configured folder or
bucket by themselves when the server starts: in the background, one at a
time, while it serves (the log says when it starts and ends; stopping
midway is fine, it carries on next time). The database file doesn't shrink
by itself: its free pages are used again, and a `backup` (§6) is compact.
To move between folders and buckets, stop the server, copy the files
keeping their names (e.g. `rclone copy /data/files s3:notekpr/family`), and
start it with the new `--file-store`. Moving back into the database isn't
supported.

**Safety checks at start.** The server refuses to start with a store that
holds none of its newest attachments (another folder or bucket, or an empty
one), and with `--file-store database` while attachments are in a file
store — it says which, rather than serving notes whose files seem lost.

**Deleting.** Files of attachments deleted for good (once the undo period,
`--purge-undo-days`, is over) or of deleted vaults and accounts are deleted
from the store `--keep-deleted-files-days` later. An upload that never
became an attachment (refused, a retry, or a crash halfway) is deleted
after an hour.

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
      - client_id: notekpr
        client_name: notekpr
        client_secret: '$pbkdf2-sha512$…'   # hash of the secret
        authorization_policy: two_factor
        redirect_uris: ['https://notes.example.com/api/v1/auth/oidc/callback']
        scopes: ['openid', 'profile', 'email']
        require_pkce: true
        pkce_challenge_method: S256
        token_endpoint_auth_method: client_secret_basic   # client_secret_post works too
```

Issuer: `https://auth.example.com`. Recent Authelia versions put the profile
only in the userinfo response by default; notekpr reads it from there.

**Keycloak**: create client `notekpr`, *Client authentication* on,
*Standard flow* only, valid redirect URI as above, advanced setting
*Proof Key for Code Exchange* = S256. Issuer:
`https://kc.example.com/realms/<realm>`.

**Authentik**: Applications → Providers → *OAuth2/OpenID Provider*,
confidential client, redirect URI as above, signing key selected, scopes
`openid email profile`. Issuer: `https://authentik.example.com/application/o/<slug>/`.

**Dex** (used in development): see [`deploy/dex/config.yaml`](../deploy/dex/config.yaml).

Agents (AI assistants, automation) do not use the identity provider: their
owners create them in Settings → AI agents, which issues OAuth client
credentials from notekpr itself (see [agents.md](agents.md)).

## 4. Reverse proxy and TLS

notekpr serves plain HTTP. TLS — required in production — comes from a
reverse proxy in front of it, whichever you use. What the server needs from
it:

* **HTTPS** for the public URL's host, forwarding every path to the server (Compose:
  `127.0.0.1:8080`; Kubernetes: the `notekpr` Service) — except
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
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml up -d
```

On a home network without public DNS, Caddy can issue certificates from its
own certificate authority instead:

```sh
export NOTEKPR_PUBLIC_URL=https://notes.home.arpa   # any name your devices resolve to this machine (router DNS, /etc/hosts)
export CADDYFILE=deploy/examples/caddy/Caddyfile.lan
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml up -d
# Caddy's root certificate, to install once on every device that uses notekpr:
docker compose -f docker-compose.yml -f deploy/examples/caddy/compose.yml cp caddy:/data/caddy/pki/authorities/local/root.crt ./caddy-root.crt
```

Install `caddy-root.crt` as a trusted root on each computer and phone (and on
the machine running your identity provider's clients, if it uses the same
CA). The CLI uses the operating system's trust store, so it works once the
root is installed there.

**Traefik on Kubernetes** ([`deploy/examples/traefik/ingressroute.yaml`](../deploy/examples/traefik/ingressroute.yaml)):
an IngressRoute for the `notekpr` Service with TLS from a cert-manager
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
never see content. Nor do they see who is in which vault or when anyone
works: that metadata is only in the database file (§6).

To name administrators up front instead, set `NOTEKPR_ADMINS` to
`issuer|subject` pairs: the issuer URL and your account's `sub` claim at the
provider (in Keycloak, the user's ID; for other providers see their
documentation). Signing in first is usually simpler. Removing someone from
`NOTEKPR_ADMINS` does not take the role away; do that in Settings →
Administration.

Don't reuse a development-mode data directory for real use: start with an
empty one.

### Instance settings: limits and read-only mode

Some settings change while the server runs, without a restart: Settings →
Administration → Instance in the web app, or the admin API (`GET`/`PATCH
/api/v1/admin/settings`, [protocol.md §9](protocol.md#9-administration)).
They are kept in the database, so they survive restarts and backups.

* **Limits**: accounts (people who aren't disabled), agents and vaults on
  the server; vaults, agents and quick notes per person, by default and for
  one person (their own allowance, under Accounts → Limits); drawings per
  vault, by default and for one vault (Administration → Vaults → Limit).
  Each is a number or no limit; `0` means none at all (defaults: no limit,
  but 50 agents and 20 quick notes per person). A limit only stops new ones:
  lowering it takes nothing away, and nothing stops working. Everyone's
  Personal storage (their quick notes and saved shapes) counts as no vault.
  Drawings count in their vault, whoever drew them; moving one to a full
  vault is refused. The server learns which objects are drawings to count
  them — nothing else of them.
* **Read-only**: the server keeps everything and serves it, but accepts no
  changes. The apps keep people's edits (in the browser, in the CLI's local
  copies) and send them the moment you switch it off; signing in and out,
  removing devices and revoking access keep working, but nobody new can
  sign up.
* **Instance storage**: at this size every vault is full, as below (checked
  every 10 seconds), until it is below it again — raise the limit, or make
  room: deleting things for good keeps working. It counts the database's
  pages in use (less than the file once data was deleted) and every
  attachment in the file store, those deleted for good but still undoable
  too. On an instance of its own, this is the room a family or
  a team shares however they like (with *Vaults per person* and *Storage
  per vault* left unlimited). The apps say the server's storage is full,
  not each vault.
* **Storage per vault**: at this size (MiB) a vault takes no new content —
  its members' apps keep their edits and send them once it has room — while
  every other vault goes on. The size is what its encrypted notes, history
  and files take; what was deleted for good doesn't count, so emptying its
  trash makes room at once. Give one vault more (or less) room under
  Administration → Vaults → Limit: the list shows every vault's id, owner
  and size, the largest first — never a name. People find a vault's id in
  its settings (and `notekpr vault list`), and give it to you when they ask
  for more room; *Find* takes it. Per vault rather than per person, so a full
  personal vault doesn't stop someone's team vaults; with *vaults per
  person*, a person's total stays bounded. To hear of full vaults, alert on
  `notekpr_vaults_full > 0` (§8).
* **Personal storage**: the same for someone's Personal storage — their
  quick notes and saved shapes —, which doesn't follow *Storage per vault*
  and isn't in the Vaults list: like how many quick notes someone may keep,
  it is the person's — give one person more under Accounts → Limits. A quick
  note can link any number of files, so their number alone doesn't bound
  what they take. (In the API it keeps its first name:
  `quick_notes_storage_mb`, `quick_notes_storage_allowance`.)
* **Oldest app version**: apps older than this (`0.6.0`) are told to update
  instead of syncing; they keep their unsent edits, and browsers get the
  current app with a reload. Each server version accepts apps from some
  version on by itself (shown next to the field); this only raises it. Empty:
  only that.

### Admin clients: scripts and services

Scripts and services that administer the server — a billing system setting
limits and allowances, an orchestrator switching read-only mode for
maintenance, a dashboard — each get an **admin client**: a name, scopes and
one or more bearer tokens. Give every consumer its own, with only what it
needs; one leak is then one revocation, and the audit log says who did
what. An admin client is no account and never reaches a vault, a note or a
key.

| Scope | Allows |
|---|---|
| `read` | accounts, statistics, the instance settings, admin clients (every client has it) |
| `settings` | change limits and read-only mode |
| `accounts` | disable and enable accounts, set their allowances and vaults' storage limits |
| `admins` | give and take the admin role |
| `clients` | create, change and delete admin clients and their tokens — never with a scope it doesn't have itself |

Administrators make them in Settings → Administration → Admin clients, or
over the API (`/admin/clients`, a client with the `clients` scope). A token
is shown once; rotate by adding a token, switching to it, and revoking the
old one — no restart. Calls send it as a bearer token:

```sh
t=notekpr_a_…            # the client's token
url=https://notes.example.com/api/v1
curl -H "Authorization: Bearer $t" $url/admin/settings
curl -H "Authorization: Bearer $t" -X PATCH $url/admin/settings -H 'Content-Type: application/json' \
     -d '{"vaults_per_user": 1, "quick_notes_per_user": 30, "quick_notes_storage_mb": 100, "drawings_per_vault": 1, "max_storage_mb": 20480}'
# one person, by e-mail or by their sign-in identity (issuer|subject)
curl -H "Authorization: Bearer $t" "$url/admin/users?email=ana@example.com"
curl -H "Authorization: Bearer $t" -X PATCH $url/admin/users/<id> -H 'Content-Type: application/json' \
     -d '{"vault_allowance": 3, "quick_note_allowance": 50, "quick_notes_storage_allowance": 500}'   # each a number, "unlimited" or "default"
# vaults by size (or the one with an id someone gave you, or someone's), and more room for one (MiB)
# or more drawings
curl -H "Authorization: Bearer $t" "$url/admin/vaults?id=<vault id>"
curl -H "Authorization: Bearer $t" -X PATCH $url/admin/vaults/<vault id> -H 'Content-Type: application/json' \
     -d '{"storage_allowance": 2048, "drawing_allowance": 10}'   # a number, "unlimited" or "default" (the instance's)
```

**From the server's command line** — for the first client on an instance
without administrators, or to keep clients in your deployment's
configuration. `notekpr-server admin-client` writes to the database
directly, so it works while the server runs (with the same `--data-dir`),
and a new client works at once:

```sh
# once, by hand: prints the token (only now)
kubectl -n notekpr exec deploy/notekpr -- \
  notekpr-server admin-client create --name billing --scopes settings,accounts,clients
docker compose run --rm notekpr admin-client create --name billing --scopes settings,accounts,clients
notekpr-server admin-client list      # names, scopes, token hints
notekpr-server admin-client delete --name billing
```

The image runs `notekpr-server`, so in a container the subcommand is just
its arguments (like the backup CronJob's `backup`), and needs no shell.

**Declarative (GitOps)**: make the token yourself and keep it in your secret
store, mounted for both notekpr and the service that uses it. An init
container (same image, same volume) runs `ensure` before every start: it
creates the client if it's missing, sets its scopes and makes its tokens
exactly the ones in the file (one per line); running it again changes
nothing.

```sh
echo "notekpr_a_$(openssl rand -hex 32)"      # a token: the prefix and ≥ 32 characters
```

```yaml
initContainers:
  - name: admin-clients
    image: ghcr.io/coolapso/notekpr:<version>
    args: ["admin-client", "ensure", "--name", "billing",
           "--scopes", "settings,accounts", "--tokens-file", "/secrets/billing/tokens"]
    volumeMounts:
      - { name: data, mountPath: /data }
      - { name: billing-token, mountPath: /secrets/billing, readOnly: true }
```

The file wins: a token revoked through the API comes back at the next start
if it is still listed (such clients show as *managed*). Rotate by listing
the new token next to the old one, rolling out the service with it, then
removing the old line; it is revoked when notekpr next starts (or run
`ensure` by hand). A Secret change alone doesn't restart the pod.

**An admin-less instance** (e.g. one shared by many customers):
`NOTEKPR_ADMINLESS=true`, and admin clients made from the command line
(`create` once, or `ensure` in your deployment). Nobody signing in
administers anything — people only see their own account and vaults, the
web app shows no Administration, and the admin role can't be given — while
your billing system or orchestrator manages limits, read-only mode and
accounts through its admin clients.

## 6. Backup and restore

Everything — accounts, memberships, encrypted notes, history and key
envelopes — is in `notekpr.db`; so are attachments with `--file-store
database`, otherwise their bytes are in the file store
([Where attachments are kept](#where-attachments-are-kept)). Note content in
it is end-to-end encrypted, but the file still contains metadata — accounts, who is in which
vault and who shares notes with whom, and who changed something when —
readable by anyone with the file or a backup; store backups securely.
[security.md](security.md#where-the-metadata-is-and-who-can-see-it) lists
what each table holds.

```sh
# consistent online backup (safe while the server runs)
notekpr-server backup --data-dir /var/lib/notekpr --out /backups/notekpr-$(date +%F).db
# or timestamped into a directory (created if missing), keeping the newest 14
notekpr-server backup --data-dir /var/lib/notekpr --out-dir /backups --keep 14
```

**Attachments in a file store** are backed up with your usual tools,
after the database: files are never changed once written, so a copy of the
folder (restic, rsync, a volume snapshot) taken after a database backup has
every file that backup names. For S3, turn on the bucket's versioning or
your provider's replication. Deleted files stay in the store for
`--keep-deleted-files-days` (14), so a database backup that recent finds its
files even without a copy; keep it at least as long as you keep backups.

`--out` needs an existing directory and a file name that doesn't exist yet.
`--out-dir` names files `notekpr-YYYYMMDD-HHMMSS.db` (UTC); `--keep`
prunes only files named `notekpr-*.db` there, so the automatic
`pre-migration-*.db` backups (§7), which land in `<data dir>/backups`, stay
until you delete them.

**Docker Compose**: backups land in the data volume, next to the
attachments (`/data/files`); copy both off the machine.

```sh
docker compose exec notekpr notekpr-server backup --out-dir /data/backups --keep 14
docker compose cp notekpr:/data/backups ./backups      # then store ./backups elsewhere
docker compose cp notekpr:/data/files ./files          # attachments, after the database
```

Schedule it with cron or a systemd timer on the host, for example daily:

```
15 3 * * *  cd /srv/notekpr && docker compose exec -T notekpr notekpr-server backup --out-dir /data/backups --keep 14 && docker compose cp notekpr:/data/backups /srv/backups/notekpr
```

If you replace the named volume with a host directory (`./data:/data`), make
it writable by the image's user: `sudo chown -R 65532:65532 ./data`.

**Restore** (stop the server first):

```sh
notekpr-server restore --data-dir /var/lib/notekpr --from /backups/notekpr-2026-10-01.db

# docker compose
docker compose stop notekpr
docker compose cp ./backups/notekpr-2026-10-01.db notekpr:/data/backups/restore.db
docker compose run --rm --no-deps notekpr restore --from /data/backups/restore.db
docker compose start notekpr
```

The backup file and its directory must be writable by the server's user
during the restore. `restore` checks integrity, moves the current database
aside (`notekpr.db.pre-restore-<timestamp>`), installs the backup and
applies migrations for the current version. The restored server uses the
same file store; restored somewhere else, give it a copy of the files (or
the same bucket and prefix) — it refuses to start without them.

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
notekpr-server export-user --data-dir /var/lib/notekpr --user alice@example.com --out alice.json
notekpr-server export-user --backup /backups/notekpr-2026-10-01.db --user alice@example.com --out alice.json

# docker compose
docker compose exec notekpr notekpr-server export-user --user alice@example.com --out /data/alice.json
docker compose cp notekpr:/data/alice.json ./alice.json
```

`--user` takes a user id, an e-mail address or a display name. A backup is
read through a migrated temporary copy; the backup itself isn't changed.
Nothing in the file is readable without the person's keys: they decrypt it
offline with `notekpr recover` ([recovery.md](recovery.md)).

Users should additionally export important vaults as Markdown (vault
settings → Export, or `notekpr export`) — exports do not depend on
notekpr at all.

## 7. Upgrades

1. Take a backup (§6).
2. Install the new version and start it. Pending database migrations run at
   startup; before migrating an existing database the server writes
   `data/backups/pre-migration-<timestamp>.db` automatically.

   ```sh
   # docker compose (with a pinned version, set the new one in docker-compose.yml first)
   docker compose pull && docker compose up -d
   # kubernetes: set the new version in notekpr.yaml and backup-cronjob.yaml, apply them;
   # on latest: kubectl -n notekpr rollout restart deployment notekpr
   # from source
   git pull && cargo build --release --locked -p notekpr-server && ./scripts/build-web.sh
   # then install the binary and web client as in §1 and restart the service
   ```
3. Clients are forward compatible within a protocol version (`/api/v1`):
   apps tolerate what newer servers send, and a server too new for an
   installed app tells it to update rather than letting it misbehave
   ([protocol.md §11](protocol.md#11-older-apps)).

`notekpr-server migrate` applies migrations without starting the server.

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
  `notekpr-server healthcheck` probes `/readyz` (used by the image).
* `GET /metrics` — Prometheus: `notekpr_http_requests_total{method,status}`
  (`status` is the class: `2xx`, `4xx`, …),
  `notekpr_http_request_duration_seconds` (a summary with quantiles: no
  histogram buckets are configured), `notekpr_updates_total`,
  `notekpr_attachment_bytes_total`, `notekpr_ws_connections` (live
  connections) and `notekpr_ws_connected_users` (people with at least
  one), `notekpr_rate_limited_total{scope}`, `notekpr_read_only` (1 while
  the server refuses changes), `notekpr_storage_used_bytes` (what
  `max_storage_mb` is compared with), `notekpr_storage_full` (1 while it is
  reached: every vault is full) and `notekpr_vaults_full` (how many
  vaults are at their storage limit, counted every 10 seconds;
  Administration → Vaults shows which — the metrics never name a vault),
  and on Linux the process's own `process_cpu_seconds_total` (a gauge) and
  `process_resident_memory_bytes`. Keep it internal (§4).
* Logs: one line per request (method, redacted path, status, latency) plus
  lifecycle events; `--log-format json` for log shippers. No user ids, IP
  addresses or names.
* The version that runs: `notekpr-server --version`, the start-up log
  (`notekpr server ready`), `version` in `GET /api/v1/info`.
* Administrators see accounts, storage statistics and the server's version
  in Settings → Administration (never content or vault membership); admin
  clients get the statistics and version from `GET /api/v1/admin/stats`. The
  server logs when it reaches the storage limit and when it is below it
  again.

## 9. Development mode

`notekpr-server --dev-mode` (or `docker compose -f docker-compose.dev.yml up`,
or `task dev`) lets anyone sign in as any user name without a password. It is
meant for trying things out and testing multi-user features locally:

* open `http://localhost:8080`, sign in as `alice`; open another tab and sign in
  as `bob` — each tab keeps its own session;
* the CLI supports the same with
  `notekpr --profile bob login --server http://localhost:8080 --dev-user bob`.

Safety rails:

* The server refuses to start in dev mode if it listens on a non-loopback
  address, has an `https` public URL or has OIDC configured — unless
  `--dev-mode-anywhere`. The dev compose file sets it, because its port is
  published on `127.0.0.1` only.
* `docker-compose.dev.yml` is its own compose project (`notekpr-dev`), so
  it can't replace production containers on the same machine.
* When the server runs without dev mode, sessions of accounts created in dev
  mode are ended, and those accounts never count as "the first user" for the
  administrator role.
* The server logs a warning at start and every page shows a "DEV MODE" badge.

Never expose a dev-mode server to a network you don't control — with one
deliberate exception, a demo that starts over on its own (§10).

## 10. A public demo

To let a few people try notekpr without accounts: the demo of
`task demo` (users, vaults, shared notes, history — [development.md](development.md#a-demo-with-data))
on a server that deletes everything and starts over every hour.

The demo image is published with every release (`task release`), for
amd64 and arm64: `ghcr.io/coolapso/notekpr-demo`, tagged with the version
and `latest` (`task docker:push:demo` pushes one in between,
[development.md](development.md#releases)). To build your own:

```sh
task docker:build:demo    # notekpr-demo:latest — tag and push it to your registry
# or for amd64 and arm64 at once, straight to your registry:
docker buildx build --platform linux/amd64,linux/arm64 --target demo -t registry.example.com/notekpr-demo:0.6.0 --push .
```

The image (the Dockerfile's `demo` stage) runs `scripts/demo.py` with
`--fresh --lifetime 1h --dev-mode-anywhere`: it deletes the previous demo
data, starts a development-mode server that only this machine can reach,
seeds it through the CLI (about half a minute), then opens the server's
port, approves visitors' browsers, and exits an hour after it started.
Whatever runs the container restarts it, and it starts over — no scheduler
needed. Visitors see the time of the next reset on the sign-in page and in
the corner of every page (the server's `--demo-resets-at`, which only works
in development mode).

On Kubernetes: [`deploy/kubernetes/demo.yaml`](../deploy/kubernetes/demo.yaml)
(its own namespace; the data on an `emptyDir`, the only place the demo
writes to — the root file system can stay read-only; ready once the data is
seeded, so the ingress waits during the reset). Set `--public-url` to
your host name and route to the `notekpr-demo` Service with TLS as for
the real server (§4). Elsewhere, any restart policy works the same, e.g.
`docker run --restart always -p 127.0.0.1:8096:8080 ghcr.io/coolapso/notekpr-demo --public-url https://demo.example.com`
behind your proxy. `--lifetime 30m` (after the image's own) resets more often.

What to know before sharing the link:

* **Anyone who opens it can sign in as anyone** — alice, bob, … or any new
  name — read and change everything, and upload files (up to
  `--max-attachment-mb` each) until the next reset. Put it on its own host
  name, never next to real data, and don't use it for anything you'd mind
  others seeing. Share the link with whom you like; gate it at your proxy
  (e.g. basic auth) if it should be fewer.
* **Visitors share the demo users**: two people signed in as alice see each
  other's edits live.
* **Each reset signs everyone out**: open pages go to the sign-in page
  once the demo is back, with the new reset time. Nothing answers while it
  seeds.
