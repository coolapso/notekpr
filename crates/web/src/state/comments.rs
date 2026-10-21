//! Comments on notes (docs/comments.md): what the views show, sending
//! records, and the editor's highlights of what they are about.

use coolpsonotes_core::comments::{self, Action, Record, Signed, Trust};
use coolpsonotes_core::keys::AccountPublic;
use coolpsonotes_core::protocol::comments_object;
use leptos::prelude::*;

use super::{Core, step, ui};
use crate::editor::Mark;
use crate::util;

/// The one floating piece of comment UI on the open note.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CommentUi {
    #[default]
    Closed,
    /// The "Comment" button by selected text (byte offsets), at `(x, y)`
    /// within the note's text area.
    Offer {
        start: usize,
        end: usize,
        x: f64,
        y: f64,
    },
    /// Writing a comment on that text.
    Compose {
        start: usize,
        end: usize,
        x: f64,
        y: f64,
    },
    /// A thread, open.
    Thread { id: String, x: f64, y: f64 },
}

/// A comment or reply as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentView {
    pub id: String,
    pub author: String,
    pub name: String,
    pub body: String,
    pub created: i64,
    pub edited: bool,
    /// Not stored on the server yet.
    pub pending: bool,
    pub verified: bool,
    pub mine: bool,
    /// We may delete it (ours, or we own the vault).
    pub deletable: bool,
}

/// A comment with its replies, as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadView {
    pub id: String,
    pub quote: String,
    /// Where its text is now (byte offsets), if it still is.
    pub range: Option<(usize, usize)>,
    /// Who resolved it.
    pub resolved_by: Option<String>,
    pub comments: Vec<CommentView>,
}

/// Who owns the vault of `vault`'s note `note` (as far as we know).
fn owner_of(c: &Core, vault: &str, note: &str) -> Option<String> {
    c.vault_infos
        .get(vault)
        .map(|i| i.owner_id.clone())
        .or_else(|| {
            c.shares
                .get(&(vault.to_string(), note.to_string()))
                .map(|s| s.info.owner.id.clone())
        })
}

/// Whether we may comment on the note: anyone who can read it, viewers only
/// while the vault's owner lets them.
pub fn can_comment(c: &Core, vault: &str, note: &str) -> bool {
    match c.vault_infos.get(vault) {
        Some(i) => i.role.can_write() || i.viewers_comment,
        None => c
            .shares
            .get(&(vault.to_string(), note.to_string()))
            .is_some_and(|s| s.info.can_comment),
    }
}

/// A person's name, if we know it.
pub fn user_name(c: &Core, user_id: &str) -> String {
    if user_id == c.user_id {
        return "You".into();
    }
    c.users
        .get(user_id)
        .map(|u| u.display_name.clone())
        .unwrap_or_else(|| "Someone".into())
}

/// The note's comment threads, oldest first.
pub fn comment_threads(c: &Core, vault: &str, note: &str) -> Vec<ThreadView> {
    let Some(r) = c.replicas.get(vault) else {
        return Vec::new();
    };
    let owner = owner_of(c, vault, note);
    let i_own = owner.as_deref() == Some(c.user_id.as_str());
    let known = |u: &str| -> Option<AccountPublic> {
        c.comment_keys.get(u).cloned().or_else(|| {
            (owner.as_deref() == Some(u))
                .then(|| c.owner_keys.get(vault).cloned())
                .flatten()
        })
    };
    let doc = r.notes.get(note);
    r.comment_threads(note, known, |u| owner.as_deref() == Some(u))
        .into_iter()
        .map(|t| {
            let view = |cm: &comments::Comment| CommentView {
                id: cm.id.clone(),
                author: cm.author.clone(),
                name: user_name(c, &cm.author),
                body: cm.body.clone(),
                created: cm.created,
                edited: cm.edited,
                pending: cm.pending,
                verified: cm.trust == Trust::Verified,
                mine: cm.author == c.user_id,
                deletable: cm.author == c.user_id || i_own,
            };
            ThreadView {
                id: t.root.id.clone(),
                quote: t.anchor.quote.clone(),
                range: doc.and_then(|d| comments::locate(d, &t.anchor)),
                resolved_by: t.resolved_by.as_deref().map(|u| user_name(c, u)),
                comments: std::iter::once(&t.root)
                    .chain(&t.replies)
                    .map(view)
                    .collect(),
            }
        })
        .collect()
}

/// Authors of the note's comments we know neither the name nor the key of.
fn unknown_authors(c: &Core, vault: &str, note: &str) -> Vec<String> {
    let Some(log) = c.replicas.get(vault).and_then(|r| r.comments.get(note)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = log
        .entries()
        .iter()
        .map(|e| e.record.author.clone())
        .filter(|a| *a != c.user_id && !c.users.contains_key(a))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Look up the names (and, for people a note is shared with, the keys) of
/// its commenters we don't know yet. Keys are pinned on first use, like
/// everyone else's; a key that changed since isn't trusted.
pub async fn learn_commenters(vault: String, note: String) {
    let who = step(|c| unknown_authors(c, &vault, &note));
    let mut learned = false;
    for id in who {
        let Ok(u) = crate::api::get::<coolpsonotes_core::protocol::PublicUser>(&format!(
            "/users/{}",
            crate::api::q(&id)
        ))
        .await
        else {
            continue;
        };
        step(|c| {
            if let Some(k) = u.account_key.as_ref()
                && let Ok(key) = coolpsonotes_core::engine::published_account(&u.id, k)
            {
                let fp = key.fingerprint();
                use coolpsonotes_core::engine::{PinStatus, check_pin};
                match check_pin(&mut c.prefs.pins, &u.id, &fp) {
                    PinStatus::Changed => {}
                    status => {
                        c.comment_keys.insert(u.id.clone(), key);
                        if status == PinStatus::New {
                            super::save_prefs(c);
                        }
                    }
                }
            }
            c.users.insert(u.id.clone(), u);
        });
        learned = true;
    }
    if learned {
        ui().comments_rev.update(|n| *n += 1);
    }
}

/// Send a comment record on `note`. Returns its id.
pub fn send_comment(vault: &str, note: &str, action: Action) -> Result<String, String> {
    let id = step(|c| -> Result<String, String> {
        let account = c.account.as_ref().ok_or("Not signed in.")?;
        let record = Record::new(&c.user_id, util::now_ms(), action);
        let signed = Signed::sign(account, vault, note, &record).map_err(|e| e.to_string())?;
        let r = c.replicas.get_mut(vault).ok_or("Not loaded yet.")?;
        r.local_comment(note, &signed).map_err(|e| e.to_string())?;
        // (A shared note's local copy holds its comments.)
        let kept = if r.member {
            comments_object(note)
        } else {
            note.to_string()
        };
        crate::cache::note_local(c, vault, &kept);
        super::schedule_flush(c);
        Ok(record.id)
    })?;
    ui().comments_rev.update(|n| *n += 1);
    Ok(id)
}

/// Comment on bytes `start..end` of the note's text.
pub fn comment_on(
    vault: &str,
    note: &str,
    start: usize,
    end: usize,
    body: &str,
) -> Result<String, String> {
    let anchor = step(|c| {
        let doc = c.replicas.get(vault)?.notes.get(note)?;
        comments::anchor(doc, start, end)
    })
    .ok_or("That text can't be commented on.")?;
    send_comment(
        vault,
        note,
        Action::Comment {
            body: body.trim().to_string(),
            anchor,
        },
    )
}

/// Highlights for the open threads of the note, the open one marked.
pub fn comment_marks(c: &Core, vault: &str, note: &str, open: Option<&str>) -> Vec<Mark> {
    comment_threads(c, vault, note)
        .into_iter()
        .filter(|t| t.resolved_by.is_none())
        .filter_map(|t| {
            let (start, end) = t.range?;
            let class = if open == Some(t.id.as_str()) {
                "cm cm-open"
            } else {
                "cm"
            };
            Some(Mark {
                start,
                end,
                class: class.into(),
                id: t.id,
            })
        })
        .collect()
}

/// Show the open note's highlights in its editor (an edge).
pub fn refresh_comment_marks() {
    let open = match ui().comment_ui.get_untracked() {
        CommentUi::Thread { id, .. } => Some(id),
        _ => None,
    };
    let job = step(|c| {
        let (v, n, ed) = c.editor.as_ref()?;
        Some((ed.clone(), comment_marks(c, v, n, open.as_deref())))
    });
    if let Some((ed, marks)) = job {
        ed.set_marks(marks);
    }
}
