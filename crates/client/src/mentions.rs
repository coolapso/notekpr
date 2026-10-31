//! Mentions in comments (docs/comments.md §1): who may be mentioned, the
//! signed-in person's mentions across their vaults, and which of those
//! they have read — kept in the encrypted preferences the web app syncs,
//! so a mention read anywhere is read everywhere. Mentions are found on
//! this device, in the comments it synced: the server never learns who is
//! mentioned.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use notekpr_core::comments::{self, Found, MentionsRead, Person};
use notekpr_core::doc::EntryKind;
use notekpr_core::protocol::{PrefsBlob, PutPrefsRequest};
use serde::Serialize;

use crate::hub::Hub;
use crate::ops::{OpError, OpResult};
use crate::session::{Session, SharedVault};

/// The people who may be mentioned in a vault's comments: its members, as
/// its owner-signed membership log vouches for them. `None` when the server
/// can't be asked.
pub async fn people(session: &Session, vault: &SharedVault) -> Option<Vec<Person>> {
    let (id, log) = {
        let v = vault.lock().await;
        (v.id().to_string(), v.log.clone())
    };
    let members = session.api.members(&id).await.ok()?;
    Some(
        members
            .into_iter()
            .filter(|m| log.vouches_for(m))
            .map(|m| (m.user.id, m.user.display_name))
            .collect(),
    )
}

/// Whom a comment's `text` mentions among `people` (`None`: they couldn't
/// be looked up, so nobody is mentioned).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Resolved {
    /// The people mentioned, by user id.
    pub mentions: Vec<String>,
    /// Words after `@` that are nobody who may be mentioned here.
    pub not_mentioned: Vec<String>,
    /// Who may be mentioned couldn't be looked up (offline).
    pub people_unknown: bool,
}

pub fn resolve(text: &str, people: Option<&[Person]>) -> Resolved {
    let Found { ids, unknown } = comments::find_mentions(text, people.unwrap_or(&[]));
    Resolved {
        mentions: ids,
        people_unknown: people.is_none() && !unknown.is_empty(),
        not_mentioned: unknown,
    }
}

/// A comment or reply that mentions the signed-in person.
#[derive(Debug, Clone, Serialize)]
pub struct MentionItem {
    pub vault_id: String,
    pub vault: String,
    pub note_id: String,
    pub path: String,
    /// The thread (its first comment's id) and the comment that mentions.
    pub thread: String,
    pub comment: String,
    pub author: String,
    pub author_name: Option<String>,
    pub body: String,
    pub created: i64,
    /// The text the thread is about.
    pub quote: String,
    pub resolved: bool,
    /// Not marked read on any device.
    pub unread: bool,
    #[serde(skip)]
    pub mention: comments::Mention,
}

/// The read state from the synced preferences (`None`: unreadable).
async fn read_state(session: &Session) -> Option<(MentionsRead, PrefsBlob)> {
    let blob: PrefsBlob = session.api.get("/me/prefs").await.ok()?;
    let state = match &blob.ciphertext {
        Some(ct) => {
            let pt = session.account().open_private(&session.user_id, ct).ok()?;
            serde_json::from_slice(&pt).ok()?
        }
        None => MentionsRead::default(),
    };
    Some((state, blob))
}

/// The signed-in person's mentions in the hub's vaults (or vault `vref`),
/// newest first. The second value says whether read state could be read
/// (if not, every mention counts as unread).
pub async fn list(hub: &Hub, vref: Option<&str>) -> OpResult<(Vec<MentionItem>, bool)> {
    let vaults = match vref {
        Some(r) => vec![
            hub.resolve(r)
                .await
                .map_err(|e| OpError::NotFound(format!("{e:#}")))?,
        ],
        None => hub.vaults(),
    };
    let me = hub.session.user_id.clone();
    let mut items = Vec::new();
    for v in vaults {
        let g = v.lock().await;
        let (vault_id, vault) = (g.id().to_string(), g.name().to_string());
        for (note, log) in &g.replica.comments {
            if !comments::mentions_anyone(log, &me) {
                continue;
            }
            let Some(entry) = g
                .replica
                .index
                .get(note)
                .filter(|e| e.is_live() && e.kind == EntryKind::Note)
            else {
                continue;
            };
            let threads = g.replica.comment_threads(note, |_| None, |u| g.owns(u));
            for m in comments::mentions_of(log, &threads, &me) {
                items.push(MentionItem {
                    vault_id: vault_id.clone(),
                    vault: vault.clone(),
                    note_id: note.clone(),
                    path: entry.path.clone(),
                    thread: m.thread.clone(),
                    comment: m.comment.clone(),
                    author: m.author.clone(),
                    author_name: None,
                    body: m.body.clone(),
                    created: m.created,
                    quote: m.quote.clone(),
                    resolved: m.resolved,
                    unread: true,
                    mention: m,
                });
            }
        }
    }
    let authors = items.iter().map(|i| i.author.clone()).collect();
    let names = crate::ops::user_names(&hub.session, authors).await;
    let state = read_state(&hub.session).await;
    for i in &mut items {
        i.author_name = names.get(&i.author).cloned();
        if let Some((s, _)) = &state {
            i.unread = !s.is_read(&i.vault_id, &i.mention);
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse((i.created, i.mention.seq)));
    Ok((items, state.is_some()))
}

/// Mark `items` read, and with them everything before them in their
/// vaults, in the synced preferences (so on every device).
pub async fn mark_read(session: &Session, items: &[MentionItem]) -> Result<()> {
    let mut by_vault: BTreeMap<&str, Vec<comments::Mention>> = BTreeMap::new();
    for i in items {
        by_vault
            .entry(&i.vault_id)
            .or_default()
            .push(i.mention.clone());
    }
    if by_vault.is_empty() {
        return Ok(());
    }
    for _ in 0..3 {
        let blob: PrefsBlob = session
            .api
            .get("/me/prefs")
            .await
            .context("reading your settings")?;
        // The web app's preferences, with every field it keeps: only the
        // read state changes.
        let mut prefs: serde_json::Map<String, serde_json::Value> = match &blob.ciphertext {
            Some(ct) => {
                let pt = session.account().open_private(&session.user_id, ct)?;
                serde_json::from_slice(&pt).context("your settings can't be read")?
            }
            None => serde_json::Map::new(),
        };
        let mut state: MentionsRead =
            serde_json::from_value(serde_json::Value::Object(prefs.clone())).unwrap_or_default();
        let mut changed = false;
        for (v, ms) in &by_vault {
            changed |= state.mark_all(v, ms);
        }
        if !changed {
            return Ok(());
        }
        let serde_json::Value::Object(fields) = serde_json::to_value(&state)? else {
            bail!("unexpected settings");
        };
        prefs.extend(fields);
        // Goes up with every save, inside the encryption (the web app keeps
        // the newest copy it has seen).
        let counter = prefs
            .get("counter")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        prefs.insert("counter".into(), (counter + 1).into());
        let pt = serde_json::to_vec(&prefs)?;
        let req = PutPrefsRequest {
            ciphertext: session.account().seal_private(&session.user_id, &pt),
            expected_version: blob.version,
        };
        match session.api.put::<_, PrefsBlob>("/me/prefs", &req).await {
            Ok(_) => return Ok(()),
            // Saved meanwhile on another device: read it again.
            Err(e) if e.status() == Some(409) => continue,
            Err(e) => return Err(anyhow::anyhow!(e).context("saving your settings")),
        }
    }
    bail!("your settings keep changing on another device; try again")
}
