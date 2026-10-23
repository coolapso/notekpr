//! Comments on notes (docs/comments.md): what the views show, sending
//! records, and the editor's highlights of what they are about.

use leptos::prelude::*;
use notekpr_core::comments::{self, Action, Person, Record, Signed, Trust};
use notekpr_core::keys::AccountPublic;
use notekpr_core::protocol::{MemberInfo, comments_object};

use super::{Core, step, ui};
use crate::editor::Mark;
use crate::{api, render, util};

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
    /// The people it mentions: their ids and names.
    pub mentions: Vec<Person>,
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

/// The name someone is mentioned by (ours too: not "You").
fn mentioned_name(c: &Core, user_id: &str) -> String {
    if user_id == c.user_id {
        return c
            .me
            .as_ref()
            .map(|m| m.user.display_name.clone())
            .unwrap_or_else(|| "You".into());
    }
    user_name(c, user_id)
}

/// Whether we read the note as a member of its vault (not as someone it is
/// shared with).
fn member_of(c: &Core, vault: &str) -> bool {
    c.replicas.get(vault).is_some_and(|r| r.member)
}

/// The people we may mention in the note's comments (docs/comments.md §1):
/// the vault's members; on a note shared with us, its owner and those who
/// commented on it (we see no members). Never ourselves.
pub fn mention_people(c: &Core, vault: &str, note: &str) -> Vec<Person> {
    let mut people: Vec<Person> = if member_of(c, vault) {
        c.mention_people
            .get(vault)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    } else {
        let mut people: Vec<Person> = c
            .shares
            .get(&(vault.to_string(), note.to_string()))
            .map(|s| (s.info.owner.id.clone(), s.info.owner.display_name.clone()))
            .into_iter()
            .collect();
        if let Some(log) = c.replicas.get(vault).and_then(|r| r.comments.get(note)) {
            for e in log.entries() {
                let u = &e.record.author;
                if !people.iter().any(|(p, _)| p == u)
                    && let Some(p) = c.users.get(u)
                {
                    people.push((u.clone(), p.display_name.clone()));
                }
            }
        }
        people
    };
    people.retain(|(u, _)| *u != c.user_id);
    people.sort_by_key(|(_, n)| n.to_lowercase());
    people
}

/// Ask who may be mentioned in a vault's comments: its members, as its
/// membership log vouches for them (asked again after a minute).
pub async fn load_mention_people(vault: String) {
    let fresh = step(|c| {
        !member_of(c, &vault)
            || c.mention_people
                .get(&vault)
                .is_some_and(|(at, _)| util::now_ms() - at < 60_000)
    });
    if fresh {
        return;
    }
    let Ok(members) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault}/members")).await else {
        return;
    };
    step(|c| {
        let people = members
            .iter()
            .filter(|m| super::vouched(c, &vault, m))
            .map(|m| (m.user.id.clone(), m.user.display_name.clone()))
            .collect();
        for m in members {
            c.users.entry(m.user.id.clone()).or_insert(m.user);
        }
        c.mention_people.insert(vault, (util::now_ms(), people));
    });
    ui().comments_rev.update(|n| *n += 1);
}

/// The note's comment threads as folded from its records (authors checked
/// against the membership log, the owner's key and pinned keys).
pub(super) fn folded(c: &Core, vault: &str, note: &str) -> Vec<comments::Thread> {
    let Some(r) = c.replicas.get(vault) else {
        return Vec::new();
    };
    let owner = owner_of(c, vault, note);
    let known = |u: &str| -> Option<AccountPublic> {
        c.comment_keys.get(u).cloned().or_else(|| {
            (owner.as_deref() == Some(u))
                .then(|| c.owner_keys.get(vault).cloned())
                .flatten()
        })
    };
    r.comment_threads(note, known, |u| owner.as_deref() == Some(u))
}

/// The note's comment threads, oldest first.
pub fn comment_threads(c: &Core, vault: &str, note: &str) -> Vec<ThreadView> {
    let Some(r) = c.replicas.get(vault) else {
        return Vec::new();
    };
    let i_own = owner_of(c, vault, note).as_deref() == Some(c.user_id.as_str());
    let doc = r.notes.get(note);
    folded(c, vault, note)
        .into_iter()
        .map(|t| {
            let view = |cm: &comments::Comment| CommentView {
                id: cm.id.clone(),
                author: cm.author.clone(),
                name: user_name(c, &cm.author),
                body: cm.body.clone(),
                mentions: cm
                    .mentions
                    .iter()
                    .map(|u| (u.clone(), mentioned_name(c, u)))
                    .collect(),
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
        let Ok(u) = crate::api::get::<notekpr_core::protocol::PublicUser>(&format!(
            "/users/{}",
            crate::api::q(&id)
        ))
        .await
        else {
            continue;
        };
        step(|c| {
            if let Some(k) = u.account_key.as_ref()
                && let Ok(key) = notekpr_core::engine::published_account(&u.id, k)
            {
                let fp = key.fingerprint();
                use notekpr_core::engine::{PinStatus, check_pin};
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

/// Send a comment record on `note`; its text mentions whom it names of
/// the people who may be mentioned there. Returns its id.
pub fn send_comment(vault: &str, note: &str, action: Action) -> Result<String, String> {
    let id = step(|c| -> Result<String, String> {
        let mentions = match &action {
            Action::Comment { body, .. }
            | Action::Reply { body, .. }
            | Action::Edit { body, .. }
                if body.contains('@') =>
            {
                comments::find_mentions(body, &mention_people(c, vault, note)).ids
            }
            _ => Vec::new(),
        };
        let account = c.account.as_ref().ok_or("Not signed in.")?;
        let record = Record::new(&c.user_id, util::now_ms(), action).mentioning(mentions);
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

/// The note's comments as a Markdown file for an export (`None`: it has
/// none), their authors by name.
pub fn comments_markdown(c: &Core, vault: &str, note: &str, title: &str) -> Option<String> {
    let threads = folded(c, vault, note);
    let doc = c.replicas.get(vault).and_then(|r| r.notes.get(note));
    comments::to_markdown(title, doc, &threads, |u| match c.users.get(u) {
        _ if u == c.user_id => mentioned_name(c, u),
        Some(p) => p.display_name.clone(),
        None => u.to_string(),
    })
}

/// The reading view's highlights for the open threads of the note.
pub fn comment_highlights(
    c: &Core,
    vault: &str,
    note: &str,
    open: Option<&str>,
) -> Vec<render::Highlight> {
    let mut out: Vec<render::Highlight> = comment_threads(c, vault, note)
        .into_iter()
        .filter(|t| t.resolved_by.is_none())
        .filter_map(|t| {
            let (start, end) = t.range?;
            Some(render::Highlight {
                start,
                end,
                open: open == Some(t.id.as_str()),
                id: t.id,
            })
        })
        .collect();
    out.sort_by_key(|h| h.start);
    out
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
