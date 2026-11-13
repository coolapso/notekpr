//! What the native side tells the window: one status, sent whole whenever
//! it changes. The window asks with the version it has (`next_event`) and
//! gets the next one, or the same after a while — so nothing is lost across
//! reloads, and any number of callers may ask.

use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub folder: FolderStatus,
    /// Goes up each time the tray asks the window to show the app's
    /// settings.
    pub show_settings: u64,
    /// While signing in: the code the server's page shows, to compare.
    pub sign_in_code: Option<String>,
}

/// The Markdown folder's sync. `dir` is the folder as people read it
/// (`~/Notes`), not a path anything uses.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FolderStatus {
    #[default]
    Off,
    Starting {
        dir: String,
    },
    Syncing {
        dir: String,
        vaults: usize,
    },
    /// It stopped on its own: `error` says why.
    Stopped {
        dir: String,
        error: String,
    },
}

pub struct Updates {
    tx: watch::Sender<(u64, Status)>,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            tx: watch::channel((1, Status::default())).0,
        }
    }
}

impl Updates {
    /// Change the status; callers waiting hear of it.
    pub fn update(&self, f: impl FnOnce(&mut Status)) {
        self.tx.send_if_modified(|(version, status)| {
            let before = status.clone();
            f(status);
            let changed = *status != before;
            if changed {
                *version += 1;
            }
            changed
        });
    }

    pub fn current(&self) -> (u64, Status) {
        self.tx.borrow().clone()
    }

    /// The status once its version differs from `since`, or the current one
    /// after `wait`.
    pub async fn next(&self, since: u64, wait: Duration) -> (u64, Status) {
        let mut rx = self.tx.subscribe();
        let _ = tokio::time::timeout(wait, rx.wait_for(|(v, _)| *v != since)).await;
        rx.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_a_change() {
        let u = std::sync::Arc::new(Updates::default());
        let (v, s) = u.current();
        assert_eq!(s.folder, FolderStatus::Off);
        // Nothing changed: the same after the wait.
        assert_eq!(u.next(v, Duration::from_millis(10)).await.0, v);
        // A change that changes nothing is no change.
        u.update(|s| s.folder = FolderStatus::Off);
        assert_eq!(u.current().0, v);
        let waiting = {
            let u = u.clone();
            tokio::spawn(async move { u.next(v, Duration::from_secs(10)).await })
        };
        u.update(|s| s.show_settings += 1);
        let (v2, s2) = waiting.await.unwrap();
        assert_eq!(v2, v + 1);
        assert_eq!(s2.show_settings, 1);
        // A caller with an older version gets the current one at once.
        assert_eq!(u.next(v, Duration::from_secs(10)).await.0, v2);
    }
}
