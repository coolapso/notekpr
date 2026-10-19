//! Append-only audit log of plaintext-API writes (gateway and MCP).
//!
//! One JSON object per line in `<data>/<profile>/audit.jsonl`: time, channel,
//! operation, vault id, document id and byte count — never content, titles or
//! paths, so the log itself does not leak what was written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

#[derive(Serialize)]
struct Line<'a> {
    at: i64,
    via: &'a str,
    op: &'a str,
    vault: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<&'a str>,
    bytes: usize,
}

impl AuditLog {
    pub fn open(data_dir: &Path) -> AuditLog {
        AuditLog { path: data_dir.join("audit.jsonl"), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record one write. Failures are reported but never block the write
    /// itself (it already happened locally).
    pub fn record(&self, via: &str, op: &str, vault: &str, document: Option<&str>, bytes: usize) {
        let line = Line { at: coolpsonotes_core::now_ms(), via, op, vault, document, bytes };
        let Ok(mut json) = serde_json::to_vec(&line) else { return };
        json.push(b'\n');
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        if let Err(e) = o.open(&self.path).and_then(|mut f| f.write_all(&json)) {
            crate::ui::warn(&format!("could not write the audit log: {e}"));
        }
    }
}
