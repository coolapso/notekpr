use std::ops::Deref;
use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::Config;
use crate::db::Db;
use crate::hub::EventBus;
use crate::instance::Instance;
use crate::oidc::Oidc;
use crate::ratelimit::Limiters;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub db: Db,
    pub config: Config,
    /// Event fan-out to live connections (see `hub.rs` for its guarantees).
    pub hub: EventBus,
    /// SQLite has a single writer. Serialising writes in-process avoids busy
    /// errors and, because events are published while the lock is held,
    /// guarantees subscribers see each vault's log in sequence order.
    pub write_lock: tokio::sync::Mutex<()>,
    pub oidc: Option<Oidc>,
    pub blobs: crate::blobs::Blobs,
    pub limiters: Limiters,
    pub metrics: Option<PrometheusHandle>,
    /// Runtime settings (limits) and whether the server accepts changes.
    pub instance: Instance,
    /// Becomes true when the server shuts down: live connections close with
    /// [`CLOSE_RESTARTING`](notekpr_core::protocol::CLOSE_RESTARTING).
    pub shutting_down: tokio::sync::watch::Sender<bool>,
}

impl AppState {
    pub async fn new(
        db: Db,
        config: Config,
        metrics: Option<PrometheusHandle>,
    ) -> anyhow::Result<Self> {
        let oidc = Oidc::from_config(&config)?;
        let limiters = Limiters::new(&config);
        let blobs = crate::blobs::Blobs::Sqlite(db.clone());
        let instance = Instance::load(&db).await?;
        Ok(AppState(Arc::new(Inner {
            db,
            config,
            hub: EventBus::new(),
            blobs,
            write_lock: tokio::sync::Mutex::new(()),
            oidc,
            limiters,
            metrics,
            instance,
            shutting_down: tokio::sync::watch::Sender::new(false),
        })))
    }

    /// Tell every live connection the server is going away to be started
    /// again, then wait (at most `wait`) until they have closed.
    pub async fn close_connections(&self, wait: std::time::Duration) {
        self.shutting_down.send_replace(true);
        let deadline = tokio::time::Instant::now() + wait;
        while self.hub.connection_count() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}
