use std::ops::Deref;
use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::Config;
use crate::db::Db;
use crate::hub::Hub;
use crate::oidc::Oidc;
use crate::ratelimit::Limiters;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub db: Db,
    pub config: Config,
    pub hub: Hub,
    /// SQLite has a single writer. Serialising writes in-process avoids busy
    /// errors and, because events are published while the lock is held,
    /// guarantees subscribers see each vault's log in sequence order.
    pub write_lock: tokio::sync::Mutex<()>,
    pub oidc: Option<Oidc>,
    pub limiters: Limiters,
    pub metrics: Option<PrometheusHandle>,
}

impl AppState {
    pub fn new(db: Db, config: Config, metrics: Option<PrometheusHandle>) -> Self {
        let oidc = Oidc::from_config(&config);
        let limiters = Limiters::new(&config);
        AppState(Arc::new(Inner {
            db,
            config,
            hub: Hub::new(),
            write_lock: tokio::sync::Mutex::new(()),
            oidc,
            limiters,
            metrics,
        }))
    }
}

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}
