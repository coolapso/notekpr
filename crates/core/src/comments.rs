//! Comments on notes (docs/comments.md).
//!
//! A note's comments live in their own object, `c_<note id>`
//! ([`comments_object`](crate::protocol::comments_object)): an append-only
//! list of records, one per update in the vault's log, encrypted with the
//! note's key. Nothing is changed or removed in place: an edit, a deletion or
//! a resolve is a new record pointing at an earlier one, and every client
//! folds the list into threads the same way ([`threads`]).
//!
//! Each record is signed by its author over its content (not its
//! ciphertext), so it still verifies when it is encrypted again under
//! another key — when a note is shared with its existing comments.
//!
//! A comment's text may mention people (`@Name`): the record lists them by
//! user id ([`find_mentions`]), inside the encryption, and each person's own
//! devices notice it ([`mentions_of`]). Exports hold a note's threads as a
//! Markdown file of their own ([`to_markdown`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::b64;
use crate::crypto;
use crate::doc::NoteDoc;
use crate::keys::{AccountPublic, AccountSecret};

/// Longest comment, in characters.
pub const MAX_BODY_CHARS: usize = 10_000;
/// Longest quote of the commented text kept with a comment, in characters.
pub const MAX_QUOTE_CHARS: usize = 300;
/// Most people one comment mentions.
pub const MAX_MENTIONS: usize = 50;
/// Longest encoded position in the note's text.
const MAX_POSITION_BYTES: usize = 256;
const MAX_ID_LEN: usize = 64;
/// Longest user id (authors, mentions).
const MAX_USER_LEN: usize = 256;

const COMMENT_DOMAIN: &str = "notekpr/v1/sig/comment";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommentError {
    #[error("not a comment record: {0}")]
    Format(String),
    #[error("invalid comment: {0}")]
    Invalid(&'static str),
}

/// Where a comment is: the commented text's first and last characters (yrs
/// sticky positions, so they follow the text through edits), and the text
/// itself as it was.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    #[serde(with = "b64")]
    pub start: Vec<u8>,
    #[serde(with = "b64")]
    pub end: Vec<u8>,
    pub quote: String,
}

/// What a record does.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Starts a thread on some text.
    Comment {
        body: String,
        anchor: Anchor,
    },
    Reply {
        thread: String,
        body: String,
    },
    /// A new text for one of the author's own comments or replies.
    Edit {
        target: String,
        body: String,
    },
    /// Removes a comment (its whole thread) or a reply: by its author or a
    /// vault owner.
    Delete {
        target: String,
    },
    Resolve {
        thread: String,
    },
    Reopen {
        thread: String,
    },
}

/// One record of a note's comments.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Random, unique among the note's records.
    pub id: String,
    /// User id of who wrote it (and signed it).
    pub author: String,
    /// Unix milliseconds, as the author's device saw it.
    pub created: i64,
    #[serde(flatten)]
    pub action: Action,
    /// The people its text mentions (`@Name`), by user id: only with a text
    /// (a comment, a reply or an edit, whose list replaces the comment's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
}

fn valid_user(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_USER_LEN && !s.chars().any(char::is_control)
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn check_body(body: &str) -> Result<(), CommentError> {
    if body.trim().is_empty() {
        return Err(CommentError::Invalid("a comment can't be empty"));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(CommentError::Invalid(
            "a comment is at most 10 000 characters",
        ));
    }
    Ok(())
}

impl Record {
    pub fn new(author: &str, created: i64, action: Action) -> Self {
        Record {
            id: crate::random_id(),
            author: author.to_string(),
            created,
            action,
            mentions: Vec::new(),
        }
    }

    /// The same record, mentioning `people` (user ids, see [`find_mentions`]).
    pub fn mentioning(mut self, people: Vec<String>) -> Self {
        self.mentions = people;
        self
    }

    /// Whether it is well formed (records from others are checked too).
    pub fn check(&self) -> Result<(), CommentError> {
        if !valid_id(&self.id) {
            return Err(CommentError::Invalid("bad record id"));
        }
        if !valid_user(&self.author) {
            return Err(CommentError::Invalid("bad author"));
        }
        if !self.mentions.is_empty() {
            if !matches!(
                self.action,
                Action::Comment { .. } | Action::Reply { .. } | Action::Edit { .. }
            ) {
                return Err(CommentError::Invalid("only a text mentions people"));
            }
            if self.mentions.len() > MAX_MENTIONS {
                return Err(CommentError::Invalid(
                    "a comment mentions at most 50 people",
                ));
            }
            for (i, m) in self.mentions.iter().enumerate() {
                if !valid_user(m) || self.mentions[..i].contains(m) {
                    return Err(CommentError::Invalid("bad mention"));
                }
            }
        }
        match &self.action {
            Action::Comment { body, anchor } => {
                check_body(body)?;
                if anchor.start.len() > MAX_POSITION_BYTES
                    || anchor.end.len() > MAX_POSITION_BYTES
                    || anchor.quote.chars().count() > MAX_QUOTE_CHARS
                {
                    return Err(CommentError::Invalid("bad position"));
                }
            }
            Action::Reply { thread, body } => {
                check_body(body)?;
                if !valid_id(thread) {
                    return Err(CommentError::Invalid("bad thread"));
                }
            }
            Action::Edit { target, body } => {
                check_body(body)?;
                if !valid_id(target) {
                    return Err(CommentError::Invalid("bad target"));
                }
            }
            Action::Delete { target } => {
                if !valid_id(target) {
                    return Err(CommentError::Invalid("bad target"));
                }
            }
            Action::Resolve { thread } | Action::Reopen { thread } => {
                if !valid_id(thread) {
                    return Err(CommentError::Invalid("bad thread"));
                }
            }
        }
        Ok(())
    }
}

/// A record as its author signed it: the plaintext of one update of a
/// comments object.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// The record's JSON, exactly as signed.
    #[serde(with = "b64")]
    pub record: Vec<u8>,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

fn statement(vault_id: &str, note_id: &str, record: &[u8]) -> Vec<u8> {
    crypto::statement(
        COMMENT_DOMAIN,
        &[vault_id.as_bytes(), note_id.as_bytes(), record],
    )
}

impl Signed {
    /// Sign `record` (checked first) as its author, for note `note_id`.
    pub fn sign(
        account: &AccountSecret,
        vault_id: &str,
        note_id: &str,
        record: &Record,
    ) -> Result<Signed, CommentError> {
        record.check()?;
        let bytes = serde_json::to_vec(record).map_err(|e| CommentError::Format(e.to_string()))?;
        let signature = account.sign(&statement(vault_id, note_id, &bytes)).to_vec();
        Ok(Signed {
            record: bytes,
            signature,
        })
    }

    /// The record, if well formed (not whether its signature holds).
    pub fn decode(&self) -> Result<Record, CommentError> {
        let r: Record = serde_json::from_slice(&self.record)
            .map_err(|e| CommentError::Format(e.to_string()))?;
        r.check()?;
        Ok(r)
    }

    /// Whether `key` signed it for note `note_id` of vault `vault_id`.
    pub fn verify(&self, key: &AccountPublic, vault_id: &str, note_id: &str) -> bool {
        crypto::ed25519_verify(
            &key.sign_public,
            &statement(vault_id, note_id, &self.record),
            &self.signature,
        )
        .is_ok()
    }

    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_payload(payload: &[u8]) -> Result<Signed, CommentError> {
        serde_json::from_slice(payload).map_err(|e| CommentError::Format(e.to_string()))
    }
}

/// One record of a note's comments as kept locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub signed: Signed,
    pub record: Record,
    /// Its place in the vault's log; `None` while it isn't stored yet.
    pub seq: Option<i64>,
    /// The newest key epoch a stored copy of it is encrypted with (0 while
    /// it isn't stored): whether people the note is shared with can read it.
    pub epoch: u32,
}

/// A note's comment records, in log order.
#[derive(Clone, Debug, Default)]
pub struct CommentLog {
    entries: Vec<Entry>,
    by_id: HashMap<String, usize>,
}

impl CommentLog {
    /// Add a record (idempotent). The same record may come twice: sent
    /// before it was stored, or encrypted again when the note was shared.
    /// A different record under a known id is ignored. `epoch`: the key
    /// epoch this copy is encrypted with. Returns whether the log changed.
    pub fn insert(
        &mut self,
        signed: Signed,
        seq: Option<i64>,
        epoch: u32,
    ) -> Result<bool, CommentError> {
        let record = signed.decode()?;
        if let Some(&i) = self.by_id.get(&record.id) {
            let e = &mut self.entries[i];
            if e.signed != signed {
                return Ok(false);
            }
            e.epoch = e.epoch.max(epoch);
            return Ok(match (e.seq, seq) {
                (None, Some(s)) => {
                    e.seq = Some(s);
                    self.sort();
                    true
                }
                _ => false,
            });
        }
        self.entries.push(Entry {
            signed,
            record,
            seq,
            epoch,
        });
        self.sort();
        Ok(true)
    }

    /// Stored records by sequence number, then the ones not stored yet in
    /// the order they were made.
    fn sort(&mut self) {
        self.entries
            .sort_by_key(|e| (e.seq.is_none(), e.seq.unwrap_or(0)));
        self.by_id = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.record.id.clone(), i))
            .collect();
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The log as kept in a local copy (its records and their places).
    pub fn to_bytes(&self) -> Vec<u8> {
        let kept: Vec<Kept> = self
            .entries
            .iter()
            .map(|e| Kept {
                payload: e.signed.to_payload(),
                seq: e.seq,
                epoch: e.epoch,
            })
            .collect();
        serde_json::to_vec(&kept).unwrap_or_default()
    }

    /// A log kept with [`CommentLog::to_bytes`]; records that don't decode
    /// are left out.
    pub fn from_bytes(bytes: &[u8]) -> Result<CommentLog, CommentError> {
        let kept: Vec<Kept> =
            serde_json::from_slice(bytes).map_err(|e| CommentError::Format(e.to_string()))?;
        let mut log = CommentLog::default();
        for k in kept {
            if let Ok(s) = Signed::from_payload(&k.payload) {
                let _ = log.insert(s, k.seq, k.epoch);
            }
        }
        Ok(log)
    }
}

#[derive(Serialize, Deserialize)]
struct Kept {
    #[serde(with = "b64")]
    payload: Vec<u8>,
    seq: Option<i64>,
    #[serde(default)]
    epoch: u32,
}

/// How sure we are who wrote a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Signed with a key we know for its author.
    Verified,
    /// We know no key for its author yet: a comment or reply is shown,
    /// marked as such; an edit, deletion or resolve doesn't count.
    Unknown,
}

/// A comment or reply as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    /// The people `body` mentions (user ids).
    pub mentions: Vec<String>,
    pub created: i64,
    pub edited: bool,
    /// Not stored on the server yet.
    pub pending: bool,
    pub trust: Trust,
}

/// A comment with its replies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub root: Comment,
    pub anchor: Anchor,
    pub replies: Vec<Comment>,
    /// Who resolved it, when it is resolved.
    pub resolved_by: Option<String>,
}

/// Fold a note's records into its threads, oldest first. `check` says how
/// sure we are of each record's author (`None`: drop it, e.g. a bad
/// signature); `is_owner` whether a user owns the vault (owners delete any
/// comment). Edits count only by the target's author, deletions by its
/// author or an owner, and only from verified authors; records about
/// something unknown (deleted, or written before a note was shared with us)
/// are skipped.
pub fn threads(
    log: &CommentLog,
    check: impl Fn(&Entry) -> Option<Trust>,
    is_owner: impl Fn(&str) -> bool,
) -> Vec<Thread> {
    let mut threads: Vec<Thread> = Vec::new();
    // Comment or reply id → (thread index, reply index).
    let mut place: HashMap<String, (usize, Option<usize>)> = HashMap::new();
    let mut gone: Vec<bool> = Vec::new();
    for e in log.entries() {
        let Some(trust) = check(e) else { continue };
        let r = &e.record;
        let adds = matches!(r.action, Action::Comment { .. } | Action::Reply { .. });
        if !adds && trust != Trust::Verified {
            continue;
        }
        let shown = |body: &str| Comment {
            id: r.id.clone(),
            author: r.author.clone(),
            body: body.to_string(),
            mentions: r.mentions.clone(),
            created: r.created,
            edited: false,
            pending: e.seq.is_none(),
            trust,
        };
        let thread_of = |id: &str, place: &HashMap<String, (usize, Option<usize>)>| {
            place
                .get(id)
                .filter(|(_, reply)| reply.is_none())
                .map(|(t, _)| *t)
        };
        match &r.action {
            Action::Comment { body, anchor } => {
                if place.contains_key(&r.id) {
                    continue;
                }
                place.insert(r.id.clone(), (threads.len(), None));
                threads.push(Thread {
                    root: shown(body),
                    anchor: anchor.clone(),
                    replies: Vec::new(),
                    resolved_by: None,
                });
                gone.push(false);
            }
            Action::Reply { thread, body } => {
                let Some(t) = thread_of(thread, &place).filter(|t| !gone[*t]) else {
                    continue;
                };
                if place.contains_key(&r.id) {
                    continue;
                }
                place.insert(r.id.clone(), (t, Some(threads[t].replies.len())));
                threads[t].replies.push(shown(body));
            }
            Action::Edit { target, body } => {
                let Some(&(t, reply)) = place.get(target) else {
                    continue;
                };
                if gone[t] {
                    continue;
                }
                let c = match reply {
                    None => &mut threads[t].root,
                    Some(i) => &mut threads[t].replies[i],
                };
                if c.author == r.author && !c.body.is_empty() {
                    c.body = body.clone();
                    c.mentions = r.mentions.clone();
                    c.edited = true;
                }
            }
            Action::Delete { target } => {
                let Some(&(t, reply)) = place.get(target) else {
                    continue;
                };
                let author = match reply {
                    None => &threads[t].root.author,
                    Some(i) => &threads[t].replies[i].author,
                };
                if *author != r.author && !is_owner(&r.author) {
                    continue;
                }
                match reply {
                    None => gone[t] = true,
                    // Emptied, not removed: later replies keep their places.
                    Some(i) => threads[t].replies[i].body.clear(),
                }
            }
            Action::Resolve { thread } => {
                if let Some(t) = thread_of(thread, &place) {
                    threads[t].resolved_by = Some(r.author.clone());
                }
            }
            Action::Reopen { thread } => {
                if let Some(t) = thread_of(thread, &place) {
                    threads[t].resolved_by = None;
                }
            }
        }
    }
    threads
        .into_iter()
        .zip(gone)
        .filter(|(_, gone)| !gone)
        .map(|(mut t, _)| {
            t.replies.retain(|c| !c.body.is_empty());
            t
        })
        .collect()
}

/// An anchor on `doc`'s text from byte `start` to byte `end`; `None` for an
/// empty or out-of-range selection. The ends stick to the first and last
/// commented characters, so typing right before or after the text doesn't
/// add to it.
pub fn anchor(doc: &NoteDoc, start: usize, end: usize) -> Option<Anchor> {
    use yrs::updates::encoder::Encode;
    let text = doc.text();
    if start >= end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    let s = doc.sticky(start, true)?;
    let e = doc.sticky(end, false)?;
    let quote: String = text[start..end].chars().take(MAX_QUOTE_CHARS).collect();
    Some(Anchor {
        start: s.encode_v1(),
        end: e.encode_v1(),
        quote,
    })
}

/// Where `anchor`'s text is in `doc` now, as byte offsets; `None` when it
/// was deleted (or can't be found).
pub fn locate(doc: &NoteDoc, anchor: &Anchor) -> Option<(usize, usize)> {
    use yrs::updates::decoder::Decode;
    let s = yrs::StickyIndex::decode_v1(&anchor.start).ok()?;
    let e = yrs::StickyIndex::decode_v1(&anchor.end).ok()?;
    let (s, e) = (doc.resolve_sticky(&s)?, doc.resolve_sticky(&e)?);
    (s < e).then_some((s, e))
}

// ------------------------------------------------------------ mentions

/// Someone a comment may mention: their user id and their name.
pub type Person = (String, String);

/// What [`find_mentions`] found in a text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The people mentioned, by user id: each once, in the order they come.
    pub ids: Vec<String>,
    /// Words after an `@` that name nobody who may be mentioned.
    pub unknown: Vec<String>,
}

/// Whether an `@` after `prev` may start a mention: at the start, after a
/// space or punctuation — not inside a word or an e-mail address.
fn starts_mention(prev: Option<char>) -> bool {
    prev.is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '.' | '-' | '+' | '@'))
}

/// The length in bytes of `name` at the start of `rest` (in any case), when
/// it is there as a whole: not followed by a letter or a digit.
fn name_at(rest: &str, name: &str) -> Option<usize> {
    let mut chars = rest.char_indices();
    for n in name.chars() {
        let (_, c) = chars.next()?;
        if !c.to_lowercase().eq(n.to_lowercase()) {
            return None;
        }
    }
    match chars.next() {
        Some((_, c)) if c.is_alphanumeric() => None,
        Some((i, _)) => Some(i),
        None => Some(rest.len()),
    }
}

/// Every `@…` in `text` that may be a mention: its byte range (with the
/// `@`) and whom of `people` it names (`None`: nobody). Longer names win,
/// so `@Ann Lee` is Ann Lee even when there is an Ann too.
fn scan(text: &str, people: &[Person]) -> Vec<(std::ops::Range<usize>, Option<usize>)> {
    let mut order: Vec<usize> = (0..people.len())
        .filter(|&i| !people[i].1.trim().is_empty())
        .collect();
    order.sort_by_key(|&i| std::cmp::Reverse(people[i].1.trim().chars().count()));
    let mut out = Vec::new();
    let mut prev = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c == '@' && starts_mention(prev) {
            let rest = &text[at + 1..];
            let named = order
                .iter()
                .find_map(|&p| name_at(rest, people[p].1.trim()).map(|len| (p, len)));
            let (len, who) = match named {
                Some((p, len)) => (len, Some(p)),
                None => {
                    let word: usize = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
                        .map(char::len_utf8)
                        .sum();
                    (rest[..word].trim_end_matches(['.', '-', '_']).len(), None)
                }
            };
            if len > 0 {
                let end = at + 1 + len;
                out.push((at..end, who));
                while chars.peek().is_some_and(|(i, _)| *i < end) {
                    chars.next();
                }
                prev = text[..end].chars().next_back();
                continue;
            }
        }
        prev = Some(c);
    }
    out
}

/// Whom `text` mentions: `@` and the name of one of `people` (in any case),
/// at the start or after a space or punctuation, and not followed by a
/// letter or a digit. Nobody else can be mentioned: the people who may be
/// are the caller's choice (a vault's members).
pub fn find_mentions(text: &str, people: &[Person]) -> Found {
    let mut found = Found::default();
    for (range, who) in scan(text, people) {
        match who {
            Some(p) => {
                if !found.ids.contains(&people[p].0) {
                    found.ids.push(people[p].0.clone());
                }
            }
            None => {
                let word = text[range.start + 1..range.end].to_string();
                if !found.unknown.contains(&word) {
                    found.unknown.push(word);
                }
            }
        }
    }
    found
}

/// `text` in pieces to show: plain text, and each `@Name` of `people`
/// with the user id it mentions.
pub fn split_mentions<'t, 'p>(
    text: &'t str,
    people: &'p [Person],
) -> Vec<(&'t str, Option<&'p str>)> {
    let mut out = Vec::new();
    let mut last = 0;
    for (range, who) in scan(text, people) {
        let Some(p) = who else { continue };
        if range.start > last {
            out.push((&text[last..range.start], None));
        }
        out.push((&text[range.clone()], Some(people[p].0.as_str())));
        last = range.end;
    }
    if last < text.len() {
        out.push((&text[last..], None));
    }
    out
}

/// A comment or reply that mentions someone, as their devices show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The thread: its comment's id.
    pub thread: String,
    /// The comment or reply that mentions them.
    pub comment: String,
    pub author: String,
    pub body: String,
    pub created: i64,
    /// The place in the vault's log of the record that first mentioned them
    /// in it: what "read up to" is compared with (log places can't be
    /// faked by a device's clock).
    pub seq: i64,
    /// The text the thread is about.
    pub quote: String,
    pub resolved: bool,
}

/// Whether any record of `log` mentions `user` (cheap: nothing is
/// verified). Only then is it worth folding for [`mentions_of`].
pub fn mentions_anyone(log: &CommentLog, user: &str) -> bool {
    log.entries()
        .iter()
        .any(|e| e.record.mentions.iter().any(|m| m == user))
}

/// The comments and replies of `threads` (folded from `log`) that mention
/// `user` now, written by others, in log order.
pub fn mentions_of(log: &CommentLog, threads: &[Thread], user: &str) -> Vec<Mention> {
    let all = || {
        threads
            .iter()
            .flat_map(|t| std::iter::once(&t.root).chain(&t.replies))
    };
    let authors: HashMap<&str, &str> = all().map(|c| (c.id.as_str(), c.author.as_str())).collect();
    // Comment id → where its author first mentioned `user` in it.
    let mut first: HashMap<&str, i64> = HashMap::new();
    for e in log.entries() {
        let (Some(seq), r) = (e.seq, &e.record) else {
            continue;
        };
        if !r.mentions.iter().any(|m| m == user) {
            continue;
        }
        let target = match &r.action {
            Action::Comment { .. } | Action::Reply { .. } => r.id.as_str(),
            Action::Edit { target, .. } => target.as_str(),
            _ => continue,
        };
        if authors.get(target) == Some(&r.author.as_str()) {
            first.entry(target).or_insert(seq);
        }
    }
    let mut out = Vec::new();
    for t in threads {
        for c in std::iter::once(&t.root).chain(&t.replies) {
            if c.author == user || !c.mentions.iter().any(|m| m == user) {
                continue;
            }
            let Some(&seq) = first.get(c.id.as_str()) else {
                continue;
            };
            out.push(Mention {
                thread: t.root.id.clone(),
                comment: c.id.clone(),
                author: c.author.clone(),
                body: c.body.clone(),
                created: c.created,
                seq,
                quote: t.anchor.quote.clone(),
                resolved: t.resolved_by.is_some(),
            });
        }
    }
    out.sort_by_key(|m| m.seq);
    out
}

/// Which mentions someone has read, kept in their encrypted preferences
/// (synced between their devices): everything up to a place in each
/// vault's log, and single ones read after it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct MentionsRead {
    /// Vault id → read up to this place in its log ([`Mention::seq`]).
    #[serde(default)]
    pub mentions_seen: std::collections::BTreeMap<String, i64>,
    /// Comment ids read one by one, the latest last.
    #[serde(default)]
    pub mentions_read: Vec<String>,
}

/// Most single mentions kept as read; the oldest go first (by then they
/// are usually below a vault's "read up to").
const MAX_READ: usize = 500;

impl MentionsRead {
    pub fn is_read(&self, vault: &str, m: &Mention) -> bool {
        self.mentions_seen.get(vault).is_some_and(|s| m.seq <= *s)
            || self.mentions_read.contains(&m.comment)
    }

    /// Mark one read. Returns whether that changed anything.
    pub fn mark(&mut self, vault: &str, m: &Mention) -> bool {
        if self.is_read(vault, m) {
            return false;
        }
        self.mentions_read.push(m.comment.clone());
        let extra = self.mentions_read.len().saturating_sub(MAX_READ);
        self.mentions_read.drain(..extra);
        true
    }

    /// Mark all of a vault's `mentions` read. Returns whether that changed
    /// anything.
    pub fn mark_all(&mut self, vault: &str, mentions: &[Mention]) -> bool {
        let Some(top) = mentions.iter().map(|m| m.seq).max() else {
            return false;
        };
        let seen = self.mentions_seen.entry(vault.to_string()).or_insert(0);
        if top <= *seen {
            return false;
        }
        *seen = top;
        // What that covers needs no entry of its own.
        self.mentions_read
            .retain(|c| !mentions.iter().any(|m| m.comment == *c && m.seq <= top));
        true
    }

    /// Take in what another device marked read.
    pub fn merge(&mut self, other: &MentionsRead) {
        for (v, s) in &other.mentions_seen {
            let mine = self.mentions_seen.entry(v.clone()).or_insert(*s);
            *mine = (*mine).max(*s);
        }
        for c in &other.mentions_read {
            if !self.mentions_read.contains(c) {
                self.mentions_read.push(c.clone());
            }
        }
        let extra = self.mentions_read.len().saturating_sub(MAX_READ);
        self.mentions_read.drain(..extra);
    }
}

// ------------------------------------------------------------ exports

/// The folder of an export that holds the notes' comments: a note's at
/// [`export_path`]. Imports leave it out.
pub const EXPORT_DIR: &str = "_comments";

/// Where the comments of the note at `note_path` go in an export.
pub fn export_path(note_path: &str) -> String {
    format!("{EXPORT_DIR}/{note_path}")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `ms` (Unix milliseconds) as `2026-10-22 14:03 UTC`.
pub fn utc_minute(ms: i64) -> String {
    let minutes = ms.div_euclid(60_000);
    let (days, min) = (minutes.div_euclid(1440), minutes.rem_euclid(1440));
    let date = crate::dates::Date::from_days(days);
    format!("{date} {:02}:{:02} UTC", min / 60, min % 60)
}

/// A note's threads as a Markdown file of their own, for exports: each
/// under a heading quoting the text it is about, in the order of that text
/// in `doc` (threads whose text is gone last), then its comment and replies
/// with their authors' names (`name`) and times. `None` without threads.
pub fn to_markdown(
    title: &str,
    doc: Option<&NoteDoc>,
    threads: &[Thread],
    name: impl Fn(&str) -> String,
) -> Option<String> {
    if threads.is_empty() {
        return None;
    }
    let mut placed: Vec<(Option<usize>, &Thread)> = threads
        .iter()
        .map(|t| (doc.and_then(|d| locate(d, &t.anchor)).map(|r| r.0), t))
        .collect();
    placed.sort_by_key(|(at, _)| at.unwrap_or(usize::MAX));
    let mut out = format!("# Comments on {}\n\n", one_line(title));
    for (at, t) in placed {
        out.push_str(&format!("## On “{}”\n\n", one_line(&t.anchor.quote)));
        if at.is_none() && doc.is_some() {
            out.push_str("*The text this is about has been deleted.*\n\n");
        }
        if let Some(by) = &t.resolved_by {
            out.push_str(&format!("*Resolved by {}.*\n\n", name(by)));
        }
        for (i, c) in std::iter::once(&t.root).chain(&t.replies).enumerate() {
            out.push_str(&format!("**{}**", one_line(&name(&c.author))));
            if i > 0 {
                out.push_str(" · reply");
            }
            out.push_str(&format!(" · {}", utc_minute(c.created)));
            if c.edited {
                out.push_str(" · edited");
            }
            out.push_str("\n\n");
            for line in c.body.lines() {
                out.push_str(if line.trim().is_empty() { ">" } else { "> " });
                out.push_str(line.trim_end());
                out.push('\n');
            }
            out.push('\n');
        }
    }
    Some(out.trim_end().to_string() + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "vault";
    const N: &str = "note";

    fn record(author: &str, action: Action) -> Record {
        Record::new(author, 1, action)
    }

    fn comment(author: &str, body: &str) -> Record {
        record(
            author,
            Action::Comment {
                body: body.into(),
                anchor: Anchor {
                    start: vec![1],
                    end: vec![2],
                    quote: "text".into(),
                },
            },
        )
    }

    fn reply(author: &str, thread: &Record, body: &str) -> Record {
        record(
            author,
            Action::Reply {
                thread: thread.id.clone(),
                body: body.into(),
            },
        )
    }

    struct People {
        keys: HashMap<String, AccountSecret>,
    }

    impl People {
        fn new() -> Self {
            People {
                keys: ["alice", "bob", "owner"]
                    .into_iter()
                    .map(|n| (n.to_string(), AccountSecret::generate()))
                    .collect(),
            }
        }

        fn sign(&self, r: &Record) -> Signed {
            Signed::sign(&self.keys[&r.author], V, N, r).unwrap()
        }

        fn log(&self, records: &[&Record]) -> CommentLog {
            let mut log = CommentLog::default();
            for (i, r) in records.iter().enumerate() {
                log.insert(self.sign(r), Some(i as i64 + 1), 1).unwrap();
            }
            log
        }

        fn threads(&self, log: &CommentLog) -> Vec<Thread> {
            threads(
                log,
                |e| {
                    let key = self.keys.get(&e.record.author)?.public();
                    e.signed.verify(&key, V, N).then_some(Trust::Verified)
                },
                |u| u == "owner",
            )
        }
    }

    #[test]
    fn signing_and_payloads() {
        let p = People::new();
        let r = comment("alice", "Nice");
        let s = p.sign(&r);
        let alice = p.keys["alice"].public();
        assert!(s.verify(&alice, V, N));
        assert_eq!(Signed::from_payload(&s.to_payload()).unwrap(), s);
        assert_eq!(s.decode().unwrap(), r);
        // Bound to the vault, the note and the author's key.
        assert!(!s.verify(&alice, V, "other"));
        assert!(!s.verify(&alice, "other", N));
        assert!(!s.verify(&p.keys["bob"].public(), V, N));
        let mut forged = s.clone();
        forged.record = serde_json::to_vec(&comment("alice", "Changed")).unwrap();
        assert!(!forged.verify(&alice, V, N));
        // Nothing malformed is signed or read.
        assert!(Signed::sign(&p.keys["alice"], V, N, &comment("alice", " ")).is_err());
        let mut long = comment("alice", &"x".repeat(MAX_BODY_CHARS + 1));
        assert!(long.check().is_err());
        long.action = Action::Delete {
            target: "../x".into(),
        };
        assert!(long.check().is_err());
        assert!(Signed::from_payload(b"not json").is_err());
    }

    #[test]
    fn the_log_keeps_each_record_once() {
        let p = People::new();
        let a = comment("alice", "One");
        let b = reply("bob", &a, "Two");
        let mut log = CommentLog::default();
        // Sent, not stored yet.
        assert!(log.insert(p.sign(&b), None, 0).unwrap());
        assert!(log.insert(p.sign(&a), Some(5), 1).unwrap());
        assert_eq!(log.entries()[0].record, a);
        // Stored: it gets its place.
        let sb = log.entries()[1].signed.clone();
        assert!(log.insert(sb.clone(), Some(7), 1).unwrap());
        assert_eq!(log.entries()[1].seq, Some(7));
        // Again (encrypted anew when the note was shared): no change, but
        // it is under the newer key now too.
        let doc_epoch = crate::keys::FIRST_DOC_EPOCH;
        assert!(!log.insert(sb, Some(9), doc_epoch).unwrap());
        assert_eq!(
            (log.entries()[1].seq, log.entries()[1].epoch),
            (Some(7), doc_epoch)
        );
        // Another record under a known id: ignored.
        let mut other = comment("alice", "Other");
        other.id = a.id.clone();
        assert!(!log.insert(p.sign(&other), Some(10), 1).unwrap());
        assert_eq!(log.entries().len(), 2);
        assert_eq!(log.entries()[0].record, a);
        // Kept in a local copy and read back.
        let back = CommentLog::from_bytes(&log.to_bytes()).unwrap();
        assert_eq!(back.entries(), log.entries());
    }

    #[test]
    fn folding_into_threads() {
        let p = People::new();
        let t = comment("alice", "Is this right?");
        let r1 = reply("bob", &t, "Yes");
        let r2 = reply("alice", &t, "Thanks");
        let edit = |by: &str, target: &Record, body: &str| {
            record(
                by,
                Action::Edit {
                    target: target.id.clone(),
                    body: body.into(),
                },
            )
        };
        let delete = |by: &str, target: &Record| {
            record(
                by,
                Action::Delete {
                    target: target.id.clone(),
                },
            )
        };
        let resolve = record(
            "bob",
            Action::Resolve {
                thread: t.id.clone(),
            },
        );
        let e1 = edit("bob", &r1, "Yes, it is");
        let e2 = edit("alice", &r1, "No");
        let log = p.log(&[&t, &r1, &r2, &e1, &e2, &resolve]);
        let th = p.threads(&log);
        assert_eq!(th.len(), 1);
        assert_eq!(th[0].root.body, "Is this right?");
        let replies: Vec<_> = th[0]
            .replies
            .iter()
            .map(|c| (&*c.author, &*c.body, c.edited))
            .collect();
        // Only its author edits a reply.
        assert_eq!(
            replies,
            [("bob", "Yes, it is", true), ("alice", "Thanks", false)]
        );
        assert_eq!(th[0].resolved_by.as_deref(), Some("bob"));

        // Deleting: someone else's reply — no; one's own — yes; an owner — yes.
        let reopen = record(
            "alice",
            Action::Reopen {
                thread: t.id.clone(),
            },
        );
        let d1 = delete("alice", &r1);
        let d2 = delete("bob", &r1);
        let log = p.log(&[&t, &r1, &r2, &resolve, &reopen, &d1]);
        assert_eq!(p.threads(&log)[0].replies.len(), 2);
        assert_eq!(p.threads(&log)[0].resolved_by, None);
        let log = p.log(&[&t, &r1, &r2, &d2]);
        assert_eq!(p.threads(&log)[0].replies[0].body, "Thanks");
        let log = p.log(&[&t, &r1, &delete("owner", &r2)]);
        assert_eq!(p.threads(&log)[0].replies.len(), 1);
        // Deleting the comment removes its thread; later replies go nowhere.
        let r3 = reply("bob", &t, "Late");
        let log = p.log(&[&t, &r1, &delete("alice", &t), &r3]);
        assert!(p.threads(&log).is_empty());
        // A reply to a comment we can't see (e.g. from before the note was
        // shared with us) isn't shown.
        let log = p.log(&[&r1]);
        assert!(p.threads(&log).is_empty());
    }

    #[test]
    fn unverified_records_are_dropped() {
        let p = People::new();
        let t = comment("alice", "Real");
        // Claims to be alice, signed by bob.
        let fake = reply("alice", &t, "Fake");
        let mut log = p.log(&[&t]);
        log.insert(
            Signed::sign(&p.keys["bob"], V, N, &fake).unwrap(),
            Some(2),
            1,
        )
        .unwrap();
        // Deleting alice's thread, claiming to be the owner, signed by bob.
        let del = record(
            "owner",
            Action::Delete {
                target: t.id.clone(),
            },
        );
        log.insert(
            Signed::sign(&p.keys["bob"], V, N, &del).unwrap(),
            Some(3),
            1,
        )
        .unwrap();
        let th = p.threads(&log);
        assert_eq!(th.len(), 1);
        assert!(th[0].replies.is_empty());

        // Authors we have no key for yet: their comments show (marked), but
        // what they'd change doesn't count.
        let carol = AccountSecret::generate();
        let c = reply("carol", &t, "Hi");
        let d = record(
            "carol",
            Action::Delete {
                target: t.id.clone(),
            },
        );
        let mut log = p.log(&[&t]);
        for (r, seq) in [(&c, 2), (&d, 3)] {
            log.insert(Signed::sign(&carol, V, N, r).unwrap(), Some(seq), 1)
                .unwrap();
        }
        let th = threads(
            &log,
            |e| match p.keys.get(&e.record.author) {
                Some(k) => e
                    .signed
                    .verify(&k.public(), V, N)
                    .then_some(Trust::Verified),
                None => Some(Trust::Unknown),
            },
            |_| false,
        );
        assert_eq!(th.len(), 1);
        assert_eq!(th[0].replies[0].trust, Trust::Unknown);
    }

    fn people(names: &[(&str, &str)]) -> Vec<Person> {
        names
            .iter()
            .map(|(id, n)| (id.to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn finding_mentions() {
        let p = people(&[
            ("u1", "Ann"),
            ("u2", "Ann Lee"),
            ("u3", "Bob"),
            ("u4", "Zoë"),
        ]);
        let f = find_mentions("@ann lee and @Bob, (@zoë) — @ann!", &p);
        assert_eq!(f.ids, ["u2", "u3", "u4", "u1"]);
        assert!(f.unknown.is_empty());
        // Not inside words or e-mail addresses, nor names that go on.
        let f = find_mentions("bob@example.com @Bobby x@Ann @Annie", &p);
        assert!(f.ids.is_empty(), "{f:?}");
        assert_eq!(f.unknown, ["Bobby", "Annie"]);
        // Each person once; nobody who isn't one of `people`.
        let f = find_mentions("@Bob @bob @carol. @ @", &p);
        assert_eq!(
            (f.ids, f.unknown),
            (vec!["u3".to_string()], vec!["carol".to_string()])
        );
        // Pieces to show.
        let pieces = split_mentions("Hi @Ann Lee, ask @Bob.", &p);
        assert_eq!(
            pieces,
            [
                ("Hi ", None),
                ("@Ann Lee", Some("u2")),
                (", ask ", None),
                ("@Bob", Some("u3")),
                (".", None)
            ]
        );
        assert_eq!(split_mentions("no one", &p), [("no one", None)]);
    }

    #[test]
    fn records_mention_people_only_with_a_text() {
        let p = People::new();
        let c = comment("alice", "@bob look").mentioning(vec!["bob".into()]);
        let s = p.sign(&c);
        assert_eq!(s.decode().unwrap().mentions, ["bob"]);
        // Without mentions, the record reads as before the field existed.
        let plain = p.sign(&comment("alice", "x"));
        assert!(!String::from_utf8_lossy(&plain.record).contains("mentions"));
        let resolve = record(
            "alice",
            Action::Resolve {
                thread: c.id.clone(),
            },
        )
        .mentioning(vec!["bob".into()]);
        assert!(resolve.check().is_err());
        for bad in [
            vec!["bob".to_string(), "bob".into()],
            vec![String::new()],
            vec!["a\nb".into()],
        ] {
            assert!(comment("alice", "x").mentioning(bad).check().is_err());
        }
        let many = (0..=MAX_MENTIONS).map(|i| format!("u{i}")).collect();
        assert!(comment("alice", "x").mentioning(many).check().is_err());
    }

    #[test]
    fn mentions_of_a_person() {
        let p = People::new();
        let t = comment("alice", "@bob?").mentioning(vec!["bob".into()]);
        let r1 = reply("owner", &t, "Thanks");
        let r2 = reply("alice", &t, "Me too");
        // Edited to mention bob; and a fake edit of r2 by someone else.
        let e1 = record(
            "owner",
            Action::Edit {
                target: r1.id.clone(),
                body: "Thanks @bob".into(),
            },
        )
        .mentioning(vec!["bob".into()]);
        let fake = record(
            "bob",
            Action::Edit {
                target: r2.id.clone(),
                body: "@bob".into(),
            },
        )
        .mentioning(vec!["bob".into()]);
        let own = reply("bob", &t, "@bob note to self").mentioning(vec!["bob".into()]);
        let log = p.log(&[&t, &r1, &r2, &e1, &fake, &own]);
        let th = p.threads(&log);
        assert_eq!(th[0].root.mentions, ["bob"]);
        assert_eq!(th[0].replies[0].mentions, ["bob"]);
        let m = mentions_of(&log, &th, "bob");
        let got: Vec<_> = m.iter().map(|m| (m.comment.as_str(), m.seq)).collect();
        assert_eq!(got, [(t.id.as_str(), 1), (r1.id.as_str(), 4)]);
        assert_eq!(
            (m[0].thread.as_str(), m[0].quote.as_str()),
            (t.id.as_str(), "text")
        );
        // Edited again without the mention: gone.
        let e2 = record(
            "owner",
            Action::Edit {
                target: r1.id.clone(),
                body: "Thanks".into(),
            },
        );
        let log = p.log(&[&t, &r1, &e1, &e2]);
        let m = mentions_of(&log, &p.threads(&log), "bob");
        assert_eq!(m.len(), 1);
        assert!(mentions_of(&log, &p.threads(&log), "alice").is_empty());
    }

    #[test]
    fn mentions_read_and_unread() {
        let m = |comment: &str, seq: i64| Mention {
            thread: "t".into(),
            comment: comment.into(),
            author: "a".into(),
            body: String::new(),
            created: 0,
            seq,
            quote: String::new(),
            resolved: false,
        };
        let (a, b, c) = (m("a", 3), m("b", 5), m("c", 9));
        let mut r = MentionsRead::default();
        assert!(!r.is_read("v", &a));
        assert!(r.mark("v", &b) && !r.mark("v", &b));
        assert!(r.is_read("v", &b) && !r.is_read("v", &a));
        // All read: up to the newest, and single entries below it go.
        assert!(r.mark_all("v", &[a.clone(), b.clone()]));
        assert!(r.is_read("v", &a) && r.mentions_read.is_empty());
        assert!(!r.mark_all("v", std::slice::from_ref(&a)), "nothing new");
        assert!(
            !r.is_read("v", &c) && !r.is_read("w", &a),
            "newer, or another vault"
        );
        // Another device read c.
        let mut other = MentionsRead::default();
        other.mark("v", &c);
        other.mentions_seen.insert("w".into(), 4);
        r.merge(&other);
        assert!(r.is_read("v", &c) && r.is_read("w", &a));
        assert_eq!(r.mentions_seen["v"], 5);
        // In the preferences, beside other settings.
        let json = serde_json::json!({"favorites": [], "mentions_seen": {"v": 2}});
        let back: MentionsRead = serde_json::from_value(json).unwrap();
        assert_eq!(back.mentions_seen["v"], 2);
    }

    #[test]
    fn times_in_utc() {
        assert_eq!(utc_minute(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_minute(1_792_677_780_000), "2026-10-22 14:03 UTC");
        assert_eq!(utc_minute(951_868_740_000), "2000-02-29 23:59 UTC");
        assert_eq!(utc_minute(1_709_251_200_000), "2024-03-01 00:00 UTC");
        assert_eq!(utc_minute(-60_000), "1969-12-31 23:59 UTC");
    }

    #[test]
    fn threads_as_markdown() {
        let p = People::new();
        let doc = NoteDoc::new();
        doc.set_text("Buy milk and bread today");
        let on = |author: &str, start: usize, end: usize, body: &str| {
            let mut r = record(
                author,
                Action::Comment {
                    body: body.into(),
                    anchor: anchor(&doc, start, end).unwrap(),
                },
            );
            r.created = 1_792_677_780_000;
            r
        };
        let bread = on("bob", 13, 18, "Whole\n\ngrain?");
        let milk = on("alice", 4, 8, "Oat?");
        let mut answer = reply("bob", &milk, "Yes");
        answer.created = 1_792_681_380_000;
        let resolve = record(
            "alice",
            Action::Resolve {
                thread: milk.id.clone(),
            },
        );
        let log = p.log(&[&bread, &milk, &answer, &resolve]);
        let name = |u: &str| match u {
            "alice" => "Alice".to_string(),
            u => u.to_string(),
        };
        assert_eq!(to_markdown("Shopping", Some(&doc), &[], name), None);
        let md = to_markdown("Shopping", Some(&doc), &p.threads(&log), name).unwrap();
        assert_eq!(
            md,
            "# Comments on Shopping\n\n\
             ## On “milk”\n\n\
             *Resolved by Alice.*\n\n\
             **Alice** · 2026-10-22 14:03 UTC\n\n> Oat?\n\n\
             **bob** · reply · 2026-10-22 15:03 UTC\n\n> Yes\n\n\
             ## On “bread”\n\n\
             **bob** · 2026-10-22 14:03 UTC\n\n> Whole\n>\n> grain?\n"
        );
        // Its text deleted: said so, and last.
        doc.splice(4, 4, "");
        let md = to_markdown("Shopping", Some(&doc), &p.threads(&log), name).unwrap();
        assert!(md.find("“bread”").unwrap() < md.find("“milk”").unwrap());
        assert!(
            md.contains("“milk”\n\n*The text this is about has been deleted.*"),
            "{md}"
        );
        assert_eq!(export_path("Home/Plan.md"), "_comments/Home/Plan.md");
    }

    #[test]
    fn anchors_follow_the_text() {
        let doc = NoteDoc::new();
        doc.set_text("Buy milk and bread today");
        let a = anchor(&doc, 4, 8).unwrap();
        assert_eq!(a.quote, "milk");
        assert_eq!(locate(&doc, &a), Some((4, 8)));
        // Edits before it, inside it, and right after and before it.
        doc.splice(0, 0, "Please ");
        doc.splice(12, 0, "y");
        doc.splice(16, 0, "!");
        doc.splice(11, 0, "[");
        assert_eq!(doc.text(), "Please Buy [myilk! and bread today");
        assert_eq!(locate(&doc, &a), Some((12, 17)));
        // Gone with its text.
        doc.splice(12, 5, "");
        assert_eq!(locate(&doc, &a), None);
        // Nothing selected, beyond the end, or inside a character.
        let doc = NoteDoc::new();
        doc.set_text("héllo");
        assert!(anchor(&doc, 2, 2).is_none());
        assert!(anchor(&doc, 0, 99).is_none());
        assert!(anchor(&doc, 2, 3).is_none());
        let long = NoteDoc::new();
        long.set_text(&"a".repeat(1000));
        assert_eq!(anchor(&long, 0, 1000).unwrap().quote.len(), MAX_QUOTE_CHARS);
    }
}
