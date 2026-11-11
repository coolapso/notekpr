//! The person's synced, encrypted preferences, which the web app keeps:
//! read and changed field by field, so every field the web app keeps stays
//! as it is. Agents have none.
//!
//! Whom the person's apps trust (`pins`, `share_roots`, `log_heads`) and
//! the counter are also kept in the profile, as last seen: the server can
//! hand us an older copy, but what we save again never lacks them nor
//! counts lower (else the web app would take it as the newest and forget).

use anyhow::{Context, Result, bail};
use notekpr_core::protocol::{PrefsBlob, PrincipalKind, PutPrefsRequest};
use notekpr_sync::members::{self, LogHeads};
use serde_json::{Map, Value};

use crate::profile::SyncedTrust;
use crate::session::Session;

/// The preferences' fields as saved (empty if none were saved yet), with
/// whom the person's apps trust as far as we have seen kept in them.
pub async fn read(session: &Session) -> Result<(Map<String, Value>, PrefsBlob)> {
    let blob: PrefsBlob = session
        .api
        .get("/me/prefs")
        .await
        .context("reading your settings")?;
    let mut fields = match &blob.ciphertext {
        Some(ct) => {
            let pt = session.account().open_private(&session.user_id, ct)?;
            serde_json::from_slice(&pt).context("your settings can't be read")?
        }
        None => Map::new(),
    };
    let seen = session.profiles.update(|p| p.synced_trust.clone())?;
    let trust = keep_trust(&mut fields, &seen);
    if trust != seen {
        session.profiles.update(|p| p.synced_trust = trust)?;
    }
    Ok((fields, blob))
}

fn trust_of(prefs: &Map<String, Value>) -> SyncedTrust {
    SyncedTrust {
        counter: prefs.get("counter").and_then(Value::as_u64).unwrap_or(0),
        pins: field(prefs, "pins"),
        share_roots: field(prefs, "share_roots"),
        log_heads: field(prefs, "log_heads"),
    }
}

fn field<T: serde::de::DeserializeOwned + Default>(prefs: &Map<String, Value>, name: &str) -> T {
    prefs
        .get(name)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Keep in `prefs` what `seen` has (`notekpr_sync::members::keep_known`),
/// and its counter at least as far: what the two hold together.
fn keep_trust(prefs: &mut Map<String, Value>, seen: &SyncedTrust) -> SyncedTrust {
    let mut t = trust_of(prefs);
    members::keep_known(&mut t.pins, &seen.pins);
    members::keep_known(&mut t.share_roots, &seen.share_roots);
    members::keep_heads(&mut t.log_heads, &seen.log_heads);
    t.counter = t.counter.max(seen.counter);
    let set = |prefs: &mut Map<String, Value>, name: &str, v: Value| {
        prefs.insert(name.to_string(), v);
    };
    set(prefs, "counter", t.counter.into());
    for (name, v) in [
        ("pins", serde_json::to_value(&t.pins)),
        ("share_roots", serde_json::to_value(&t.share_roots)),
        ("log_heads", serde_json::to_value(&t.log_heads)),
    ] {
        if let Ok(v) = v {
            set(prefs, name, v);
        }
    }
    t
}

/// Change the preferences with `change` (which says whether it changed
/// anything) and save them, again from a fresh copy if another device
/// saved meanwhile.
pub async fn update(
    session: &Session,
    change: impl Fn(&mut Map<String, Value>) -> bool,
) -> Result<()> {
    for _ in 0..3 {
        let (mut prefs, blob) = read(session).await?;
        if !change(&mut prefs) {
            return Ok(());
        }
        // Goes up with every save, inside the encryption (the web app keeps
        // the newest copy it has seen).
        let counter = prefs.get("counter").and_then(Value::as_u64).unwrap_or(0);
        prefs.insert("counter".into(), (counter + 1).into());
        let pt = serde_json::to_vec(&prefs)?;
        let req = PutPrefsRequest {
            ciphertext: session.account().seal_private(&session.user_id, &pt),
            expected_version: blob.version,
        };
        match session.api.put::<_, PrefsBlob>("/me/prefs", &req).await {
            Ok(_) => {
                let saved = trust_of(&prefs);
                session.profiles.update(|p| p.synced_trust = saved)?;
                return Ok(());
            }
            // Saved meanwhile on another device: read it again.
            Err(e) if e.status() == Some(409) => continue,
            Err(e) => return Err(anyhow::anyhow!(e).context("saving your settings")),
        }
    }
    bail!("your settings keep changing on another device; try again")
}

fn heads_of(prefs: &Map<String, Value>) -> LogHeads {
    field(prefs, "log_heads")
}

/// Where the person's apps last saw each vault's membership log
/// (`notekpr_sync::members::LogHead`). Empty for agents.
pub async fn log_heads(session: &Session) -> Result<LogHeads> {
    if session.kind == PrincipalKind::Agent {
        return Ok(LogHeads::new());
    }
    Ok(heads_of(&read(session).await?.0))
}

/// Save where a verified log stands, unless a further head is saved.
pub async fn save_log_head(session: &Session, vault_id: &str, head: (u64, Vec<u8>)) -> Result<()> {
    if session.kind == PrincipalKind::Agent {
        return Ok(());
    }
    update(session, |prefs| {
        let mut heads = heads_of(prefs);
        if !members::note_head(&mut heads, vault_id, Some(head.clone())) {
            return false;
        }
        match serde_json::to_value(&heads) {
            Ok(v) => {
                prefs.insert("log_heads".into(), v);
                true
            }
            Err(_) => false,
        }
    })
    .await
}
