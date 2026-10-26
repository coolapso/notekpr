use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "notekpr-server",
    version,
    about = "Zero-knowledge sync server for notekpr"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub config: Config,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the server (default).
    Serve,
    /// Write a consistent copy of the database (all ciphertext, keys envelopes,
    /// metadata and attachments) to a file. Safe while the server is running.
    Backup {
        /// Destination file (must not exist).
        #[arg(long, conflicts_with = "out_dir")]
        out: Option<PathBuf>,
        /// Write a timestamped file into this directory instead.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// With --out-dir: keep only the newest N backups.
        #[arg(long)]
        keep: Option<usize>,
    },
    /// Replace the database with a backup. Stop the server first.
    Restore {
        /// Backup file produced by `backup`.
        #[arg(long)]
        from: PathBuf,
    },
    /// Write one person's encrypted export: their vaults and the notes shared
    /// with them, exactly as stored (ciphertext and key envelopes). They
    /// decrypt it offline with `notekpr recover` and their recovery key.
    /// Works on a backup file too, without a running server.
    ExportUser {
        /// User id, e-mail address or display name.
        #[arg(long)]
        user: String,
        /// Read this backup file instead of the server's database.
        #[arg(long)]
        backup: Option<PathBuf>,
        /// Destination file (must not exist).
        #[arg(long)]
        out: PathBuf,
    },
    /// Admin clients: scripts and services that administer the instance over
    /// the admin API and never reach a vault. Safe while the server runs.
    AdminClient {
        #[command(subcommand)]
        command: AdminClientCommand,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Probe /readyz on the configured listen address (for container health checks).
    Healthcheck,
}

#[derive(Subcommand, Debug)]
pub enum AdminClientCommand {
    /// Create an admin client and print its first token (shown only once).
    Create {
        #[arg(long)]
        name: String,
        /// Comma-separated: read, settings, accounts, admins, clients, or all
        /// (read is always included).
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
    },
    /// Make sure an admin client exists with these scopes and exactly the
    /// tokens in a file, one per line (`-`: standard input): for declarative
    /// setups, such as an init container. Running it again changes nothing.
    Ensure {
        #[arg(long)]
        name: String,
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
        /// Tokens made with e.g. `echo "notekpr_a_$(openssl rand -hex 32)"`;
        /// blank lines and lines starting with `#` are skipped.
        #[arg(long)]
        tokens_file: PathBuf,
    },
    /// List the admin clients, their scopes and tokens (never the tokens themselves).
    List,
    /// Delete an admin client and its tokens.
    Delete {
        #[arg(long)]
        name: String,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum TokenAuth {
    Auto,
    #[value(name = "client_secret_basic")]
    ClientSecretBasic,
    #[value(name = "client_secret_post")]
    ClientSecretPost,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum UserDirectory {
    /// Other users can be found by display-name prefix or exact e-mail.
    Search,
    /// Other users can only be found by their exact e-mail address.
    ExactEmail,
}

#[derive(Args, Debug, Clone)]
pub struct Config {
    /// Address to listen on.
    #[arg(
        long,
        env = "NOTEKPR_LISTEN",
        default_value = "127.0.0.1:8080",
        global = true
    )]
    pub listen: SocketAddr,

    /// Directory holding the SQLite database.
    #[arg(
        long,
        env = "NOTEKPR_DATA_DIR",
        default_value = "./data",
        global = true
    )]
    pub data_dir: PathBuf,

    /// Public base URL (scheme://host[:port]) as seen by browsers. Used for the
    /// OIDC redirect URI and device-login links.
    #[arg(
        long,
        env = "NOTEKPR_PUBLIC_URL",
        default_value = "http://localhost:8080",
        global = true
    )]
    pub public_url: String,

    /// Directory with the built web client. If unset, the server is API-only.
    #[arg(long, env = "NOTEKPR_WEB_DIR", global = true)]
    pub web_dir: Option<PathBuf>,

    /// OIDC issuer URL (e.g. https://auth.example.com, https://kc.example.com/realms/home).
    #[arg(long, env = "NOTEKPR_OIDC_ISSUER", global = true)]
    pub oidc_issuer: Option<String>,

    #[arg(long, env = "NOTEKPR_OIDC_CLIENT_ID", global = true)]
    pub oidc_client_id: Option<String>,

    /// Client secret (omit for a public client; PKCE is always used).
    #[arg(
        long,
        env = "NOTEKPR_OIDC_CLIENT_SECRET",
        global = true,
        hide_env_values = true
    )]
    pub oidc_client_secret: Option<String>,

    #[arg(
        long,
        env = "NOTEKPR_OIDC_SCOPES",
        default_value = "openid profile email",
        global = true
    )]
    pub oidc_scopes: String,

    /// How the client secret is sent to the token endpoint: `auto` uses what
    /// the provider advertises (`client_secret_basic` when both are offered).
    #[arg(
        long,
        env = "NOTEKPR_OIDC_TOKEN_AUTH",
        value_enum,
        default_value = "auto",
        global = true
    )]
    pub oidc_token_auth: TokenAuth,

    /// Extra CA certificates (PEM bundle) to trust for the identity provider,
    /// e.g. a private homelab CA. The system's trust store is always used.
    #[arg(long, env = "NOTEKPR_OIDC_CA_FILE", global = true)]
    pub oidc_ca_file: Option<PathBuf>,

    /// Accept e-mail addresses the provider doesn't mark as verified
    /// (`email_verified`). They are used for finding people to share with.
    #[arg(
        long,
        env = "NOTEKPR_OIDC_ALLOW_UNVERIFIED_EMAIL",
        default_value_t = false,
        global = true
    )]
    pub oidc_allow_unverified_email: bool,

    /// Name shown on the login button.
    #[arg(
        long,
        env = "NOTEKPR_OIDC_DISPLAY_NAME",
        default_value = "Single sign-on",
        global = true
    )]
    pub oidc_display_name: String,

    /// DEVELOPMENT ONLY: allow signing in as any user name without OIDC.
    #[arg(long, env = "NOTEKPR_DEV_MODE", default_value_t = false, global = true)]
    pub dev_mode: bool,

    /// Allow development mode where it could be reachable by others: a
    /// non-loopback listen address (e.g. inside a container whose port is
    /// only published on localhost), an https public URL, or with OIDC.
    #[arg(
        long,
        env = "NOTEKPR_DEV_MODE_ANYWHERE",
        default_value_t = false,
        global = true
    )]
    pub dev_mode_anywhere: bool,

    /// Demo only (needs --dev-mode): when everything on this server will be
    /// deleted, in Unix seconds. The web app tells people (`task demo`'s
    /// `--lifetime` sets it).
    #[arg(long, env = "NOTEKPR_DEMO_RESETS_AT", global = true)]
    pub demo_resets_at: Option<i64>,

    /// Identities ("issuer|subject", or "dev:<name>" in dev mode) that are administrators.
    #[arg(
        long = "admin",
        env = "NOTEKPR_ADMINS",
        value_delimiter = ',',
        global = true
    )]
    pub admins: Vec<String>,

    /// Make the first user that signs in an administrator.
    #[arg(long, env = "NOTEKPR_FIRST_USER_ADMIN", default_value_t = true, action = clap::ArgAction::Set, global = true)]
    pub first_user_admin: bool,

    /// An instance without administrators among its people: nobody signing
    /// in administers anything (the admin role, `--admin` and
    /// `--first-user-admin` do nothing, the web app has no Administration),
    /// and only admin clients (`notekpr-server admin-client`) administer it.
    #[arg(long, env = "NOTEKPR_ADMINLESS", default_value_t = false, action = clap::ArgAction::Set, global = true)]
    pub adminless: bool,

    #[arg(
        long,
        env = "NOTEKPR_USER_DIRECTORY",
        value_enum,
        default_value = "search",
        global = true
    )]
    pub user_directory: UserDirectory,

    #[arg(
        long,
        env = "NOTEKPR_SESSION_TTL_DAYS",
        default_value_t = 30,
        global = true
    )]
    pub session_ttl_days: i64,

    #[arg(
        long,
        env = "NOTEKPR_DEVICE_SESSION_TTL_DAYS",
        default_value_t = 180,
        global = true
    )]
    pub device_session_ttl_days: i64,

    #[arg(
        long,
        env = "NOTEKPR_AGENT_TOKEN_TTL_SECS",
        default_value_t = 3600,
        global = true
    )]
    pub agent_token_ttl_secs: i64,

    #[arg(
        long,
        env = "NOTEKPR_MAX_ATTACHMENT_MB",
        default_value_t = 25,
        global = true
    )]
    pub max_attachment_mb: u64,

    #[arg(
        long,
        env = "NOTEKPR_MAX_UPDATE_KB",
        default_value_t = 2048,
        global = true
    )]
    pub max_update_kb: u64,

    /// Largest snapshot (a note's full state). Default: 16 × the update
    /// limit, at least 32 MiB; never below the update limit.
    #[arg(long, env = "NOTEKPR_MAX_SNAPSHOT_KB", global = true)]
    pub max_snapshot_kb: Option<u64>,

    /// Snapshots kept per document for revision history.
    #[arg(
        long,
        env = "NOTEKPR_SNAPSHOT_RETENTION",
        default_value_t = 100,
        global = true
    )]
    pub snapshot_retention: i64,

    /// Minimum seconds between two snapshots of the same note by the same
    /// person (keeps history from being flooded).
    #[arg(
        long,
        env = "NOTEKPR_SNAPSHOT_COOLDOWN_SECS",
        default_value_t = 30,
        global = true
    )]
    pub snapshot_cooldown_secs: i64,

    /// Days during which deleting something for good (emptying the trash)
    /// can be undone by the vault's owner and admins: the server keeps it
    /// that long, then deletes it. 0: deleted at once.
    #[arg(
        long,
        env = "NOTEKPR_PURGE_UNDO_DAYS",
        default_value_t = 30,
        global = true
    )]
    pub purge_undo_days: u32,

    /// Trust X-Forwarded-For / X-Real-IP (only behind a reverse proxy you control).
    #[arg(
        long,
        env = "NOTEKPR_TRUST_PROXY",
        default_value_t = false,
        global = true
    )]
    pub trust_proxy: bool,

    /// Requests per second allowed per client IP on the API.
    #[arg(
        long,
        env = "NOTEKPR_RATE_LIMIT_RPS",
        default_value_t = 50,
        global = true
    )]
    pub rate_limit_rps: u32,

    /// Requests per minute allowed per client IP on login endpoints.
    #[arg(
        long,
        env = "NOTEKPR_AUTH_RATE_LIMIT_PER_MIN",
        default_value_t = 30,
        global = true
    )]
    pub auth_rate_limit_per_min: u32,

    /// Expose Prometheus metrics at /metrics.
    #[arg(long, env = "NOTEKPR_METRICS", default_value_t = true, action = clap::ArgAction::Set, global = true)]
    pub metrics: bool,

    /// If set, /metrics requires `Authorization: Bearer <token>`.
    #[arg(
        long,
        env = "NOTEKPR_METRICS_TOKEN",
        global = true,
        hide_env_values = true
    )]
    pub metrics_token: Option<String>,

    #[arg(
        long,
        env = "NOTEKPR_LOG_FORMAT",
        value_enum,
        default_value = "pretty",
        global = true
    )]
    pub log_format: LogFormat,
}

impl Config {
    pub fn oidc_enabled(&self) -> bool {
        self.oidc_issuer.is_some() && self.oidc_client_id.is_some()
    }

    pub fn public_url(&self) -> String {
        self.public_url.trim_end_matches('/').to_string()
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("notekpr.db")
    }

    pub fn max_attachment_bytes(&self) -> u64 {
        self.max_attachment_mb * 1024 * 1024
    }

    pub fn max_update_bytes(&self) -> u64 {
        self.max_update_kb * 1024
    }

    /// Snapshots can be larger than single updates (full document state).
    pub fn max_snapshot_bytes(&self) -> u64 {
        match self.max_snapshot_kb {
            Some(kb) => (kb * 1024).max(self.max_update_bytes()),
            None => (self.max_update_bytes() * 16).max(32 * 1024 * 1024),
        }
    }

    /// How long something deleted for good is kept, in milliseconds.
    pub fn purge_undo_ms(&self) -> i64 {
        self.purge_undo_days as i64 * 24 * 3600 * 1000
    }

    pub fn is_admin_identity(&self, issuer: &str, subject: &str) -> bool {
        !self.adminless
            && self.admins.iter().any(|a| {
                let a = a.trim();
                if let Some(name) = a.strip_prefix("dev:") {
                    return issuer == notekpr_core::protocol::DEV_ISSUER && subject == name;
                }
                match a.split_once('|') {
                    Some((i, s)) => {
                        i.trim_end_matches('/') == issuer.trim_end_matches('/') && s == subject
                    }
                    None => false,
                }
            })
    }

    /// Configuration for tests.
    /// Treat empty values (e.g. `FOO=` in a compose or Kubernetes file) as
    /// not set, rather than as an empty secret or token.
    pub fn normalize(&mut self) {
        for v in [
            &mut self.oidc_issuer,
            &mut self.oidc_client_id,
            &mut self.oidc_client_secret,
            &mut self.metrics_token,
        ] {
            if v.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *v = None;
            }
        }
        for p in [&mut self.web_dir, &mut self.oidc_ca_file] {
            if p.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
                *p = None;
            }
        }
        self.admins.retain(|a| !a.trim().is_empty());
    }

    pub fn for_tests(data_dir: PathBuf) -> Self {
        let cli = Cli::parse_from([
            "notekpr-server",
            "--dev-mode",
            "--data-dir",
            data_dir.to_str().unwrap(),
        ]);
        let mut c = cli.config;
        c.normalize();
        c.rate_limit_rps = 10_000;
        c.snapshot_cooldown_secs = 0;
        c.auth_rate_limit_per_min = 10_000;
        c
    }
}
