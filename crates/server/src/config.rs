use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "notekpr-server",
    version = notekpr_core::VERSION,
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

/// Where attachments are kept ([`Config::file_store`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileStore {
    Database,
    Directory(PathBuf),
    S3 { bucket: String, prefix: String },
}

// Only what the server needs to start. What can change while it runs is an
// instance setting (`instance.rs`, [`NOW_SETTINGS`]), set by administrators,
// never here.
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

    /// Where attachments are kept: a directory (default: `files` in the
    /// data directory), `s3://bucket/prefix` (S3 or anything compatible;
    /// credentials, region and endpoint from the usual `AWS_*` variables),
    /// or `database` (inside it, so a backup is one file). Attachments
    /// kept elsewhere before move there by themselves.
    #[arg(long, env = "NOTEKPR_FILE_STORE", global = true)]
    pub file_store: Option<String>,

    /// Trust X-Forwarded-For / X-Real-IP (only behind a reverse proxy you control).
    #[arg(
        long,
        env = "NOTEKPR_TRUST_PROXY",
        default_value_t = false,
        global = true
    )]
    pub trust_proxy: bool,

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

    /// Where attachments are kept (`--file-store`).
    pub fn file_store(&self) -> FileStore {
        match self.file_store.as_deref().map(str::trim) {
            None | Some("") => FileStore::Directory(self.data_dir.join("files")),
            Some("database") => FileStore::Database,
            Some(s) => match s.strip_prefix("s3://") {
                Some(rest) => {
                    let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
                    FileStore::S3 {
                        bucket: bucket.to_string(),
                        prefix: prefix.trim_matches('/').to_string(),
                    }
                }
                None => FileStore::Directory(PathBuf::from(s)),
            },
        }
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
        c
    }
}

/// What used to be flags and is an instance setting now (`instance.rs`):
/// the variable and the setting. Nothing reads these variables: the server
/// says so when one is set, rather than ignoring it silently.
pub const NOW_SETTINGS: &[(&str, &str)] = &[
    ("NOTEKPR_MAX_ATTACHMENT_MB", "max_attachment_mb"),
    ("NOTEKPR_MAX_UPDATE_KB", "max_update_kb"),
    ("NOTEKPR_MAX_SNAPSHOT_KB", "max_snapshot_kb"),
    ("NOTEKPR_SNAPSHOT_RETENTION", "snapshot_retention"),
    ("NOTEKPR_SNAPSHOT_COOLDOWN_SECS", "snapshot_cooldown_secs"),
    ("NOTEKPR_PURGE_UNDO_DAYS", "purge_undo_days"),
    ("NOTEKPR_KEEP_DELETED_FILES_DAYS", "keep_deleted_files_days"),
    ("NOTEKPR_RATE_LIMIT_RPS", "rate_limit_rps"),
    ("NOTEKPR_AUTH_RATE_LIMIT_PER_MIN", "auth_rate_limit_per_min"),
    ("NOTEKPR_USER_DIRECTORY", "user_directory"),
    ("NOTEKPR_SESSION_TTL_DAYS", "session_ttl_days"),
    ("NOTEKPR_DEVICE_SESSION_TTL_DAYS", "device_session_ttl_days"),
    ("NOTEKPR_AGENT_TOKEN_TTL_SECS", "agent_token_ttl_secs"),
];

/// The variables of [`NOW_SETTINGS`] set (not empty) in `env`, with their
/// settings.
pub fn set_but_now_settings(
    env: impl Fn(&str) -> Option<String>,
) -> Vec<(&'static str, &'static str)> {
    NOW_SETTINGS
        .iter()
        .copied()
        .filter(|(var, _)| env(var).is_some_and(|v| !v.trim().is_empty()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_variables_that_became_settings() {
        let env = |v: &str| match v {
            "NOTEKPR_MAX_ATTACHMENT_MB" => Some("100".to_string()),
            "NOTEKPR_RATE_LIMIT_RPS" => Some(String::new()),
            "NOTEKPR_PUBLIC_URL" => Some("https://notes.example.com".to_string()),
            _ => None,
        };
        assert_eq!(
            set_but_now_settings(env),
            [("NOTEKPR_MAX_ATTACHMENT_MB", "max_attachment_mb")]
        );
        // They are settings, no longer flags.
        let flag = Cli::try_parse_from(["notekpr-server", "--max-attachment-mb", "100"]);
        assert!(flag.is_err());
    }

    #[test]
    fn says_which_version_it_is() {
        let e = Cli::try_parse_from(["notekpr-server", "--version"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(
            e.to_string().trim(),
            format!("notekpr-server {}", notekpr_core::VERSION)
        );
    }
}
