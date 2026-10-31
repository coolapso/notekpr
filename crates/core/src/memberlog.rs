//! The vault membership log (R3).
//!
//! The owners sign every membership and key change of a vault into a
//! hash-chained log that every client verifies. The server stores the log and
//! enforces it, but cannot write it: it can neither add a member no owner
//! added (owners' clients give vault keys only to members in the verified
//! log) nor hand out a vault key no owner made (each epoch's key is committed
//! to in the log, and members check the key they receive).
//!
//! Entry `n` carries the hash of entry `n - 1`; the first entry (`Create`, or
//! `Checkpoint` for a vault that existed before the log) has `seq = 0` and an
//! all-zero `prev`. Every entry is signed by someone who is an owner at that
//! point, except `Leave`, which the leaving member signs. A vault starts with
//! one owner, its creator; `MakeOwner` makes a member another one, and owners
//! are equal from then on: any of them may remove or demote another, as long
//! as one stays. The creator's key is the trust anchor, also once they have
//! gone: clients verify against the key they pinned for the creator (or their
//! own), as [`creator`] names them.
//!
//! The signed bytes are a length-prefixed encoding (`crypto::statement`), not
//! JSON, so they never depend on a serializer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::b64;
use crate::crypto::{self, CryptoError, Key32};
use crate::keys::{AccountPublic, AccountSecret, VaultKey};
use crate::protocol::Role;

const SIG_DOMAIN: &str = "notekpr/v1/sig/member-log";
const HASH_CONTEXT: &str = "notekpr v1 member-log entry";
const COMMIT_CONTEXT: &str = "notekpr v1 vault key commitment";

/// A member as the owner vouches for them: their account keys are pinned
/// here, so a server can't swap them. Both keys are empty for someone added
/// before they set up encryption ("pending keys"); a `Rekey` entry pins
/// their keys once they have them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LogMember {
    pub user_id: String,
    pub role: Role,
    #[serde(with = "b64")]
    pub enc_public: Vec<u8>,
    #[serde(with = "b64")]
    pub sign_public: Vec<u8>,
}

impl LogMember {
    pub fn new(user_id: &str, role: Role, public: &AccountPublic) -> LogMember {
        LogMember {
            user_id: user_id.to_string(),
            role,
            enc_public: public.enc_public.to_vec(),
            sign_public: public.sign_public.to_vec(),
        }
    }

    /// Someone without account keys yet.
    pub fn pending(user_id: &str, role: Role) -> LogMember {
        LogMember {
            user_id: user_id.to_string(),
            role,
            enc_public: vec![],
            sign_public: vec![],
        }
    }

    /// The pinned account keys (`None`: none yet).
    pub fn public(&self) -> Result<Option<AccountPublic>, LogError> {
        if self.enc_public.is_empty() && self.sign_public.is_empty() {
            return Ok(None);
        }
        Ok(Some(AccountPublic {
            enc_public: key32(&self.enc_public)?,
            sign_public: key32(&self.sign_public)?,
        }))
    }

    fn keys(&self, seq: u64) -> Result<AccountPublic, LogError> {
        self.public()?
            .ok_or(LogError::Invalid(seq, "no account keys"))
    }

    fn encode(&self, out: &mut Vec<Vec<u8>>) {
        out.push(self.user_id.as_bytes().to_vec());
        out.push(role_tag(&self.role).to_vec());
        out.push(self.enc_public.clone());
        out.push(self.sign_public.clone());
    }
}

/// What one entry changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogBody {
    /// The vault was created by `owner`; `key_commit` commits to epoch 1.
    Create {
        owner: LogMember,
        #[serde(with = "b64")]
        key_commit: Vec<u8>,
    },
    /// The log starts for a vault that existed before it: the members as the
    /// owner's client saw them, trusted from here on, and the current epoch.
    Checkpoint {
        members: Vec<LogMember>,
        epoch: u32,
        #[serde(with = "b64")]
        key_commit: Vec<u8>,
    },
    Add {
        member: LogMember,
    },
    Role {
        user_id: String,
        role: Role,
    },
    /// A member's account key changed and an owner accepted the new one
    /// (another owner's, for an owner).
    Rekey {
        member: LogMember,
    },
    /// A member becomes an owner, equal to the others.
    MakeOwner {
        user_id: String,
    },
    Remove {
        user_id: String,
    },
    /// A member left the vault (signed by them).
    Leave {
        user_id: String,
    },
    /// The vault key moved to `epoch`.
    Rotate {
        epoch: u32,
        #[serde(with = "b64")]
        key_commit: Vec<u8>,
    },
    /// A note is shared with someone who is not a member (their key pinned),
    /// or their role on it changes.
    Share {
        object_id: String,
        member: LogMember,
    },
    /// A note is no longer shared with someone.
    Unshare {
        object_id: String,
        user_id: String,
    },
    /// From the log's sequence number `from_seq` on (updates with a higher
    /// one, snapshots covering more), content must be signed by its author.
    RequireSigned {
        from_seq: i64,
    },
    /// A kind of entry from a newer release. It can't be checked (what is
    /// signed depends on fields this release doesn't know), so the log can't
    /// be verified from here on: apps say to update ([`LogError::Newer`]).
    /// Servers refuse it.
    #[serde(other)]
    Unknown,
}

impl LogBody {
    fn encode(&self) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = Vec::new();
        match self {
            LogBody::Create { owner, key_commit } => {
                parts.push(b"create".to_vec());
                owner.encode(&mut parts);
                parts.push(key_commit.clone());
            }
            LogBody::Checkpoint {
                members,
                epoch,
                key_commit,
            } => {
                parts.push(b"checkpoint".to_vec());
                parts.push((members.len() as u32).to_be_bytes().to_vec());
                for m in members {
                    m.encode(&mut parts);
                }
                parts.push(epoch.to_be_bytes().to_vec());
                parts.push(key_commit.clone());
            }
            LogBody::Add { member } => {
                parts.push(b"add".to_vec());
                member.encode(&mut parts);
            }
            LogBody::Role { user_id, role } => {
                parts.push(b"role".to_vec());
                parts.push(user_id.as_bytes().to_vec());
                parts.push(role_tag(role).to_vec());
            }
            LogBody::Rekey { member } => {
                parts.push(b"rekey".to_vec());
                member.encode(&mut parts);
            }
            LogBody::MakeOwner { user_id } => {
                parts.push(b"make-owner".to_vec());
                parts.push(user_id.as_bytes().to_vec());
            }
            LogBody::Remove { user_id } => {
                parts.push(b"remove".to_vec());
                parts.push(user_id.as_bytes().to_vec());
            }
            LogBody::Leave { user_id } => {
                parts.push(b"leave".to_vec());
                parts.push(user_id.as_bytes().to_vec());
            }
            LogBody::Rotate { epoch, key_commit } => {
                parts.push(b"rotate".to_vec());
                parts.push(epoch.to_be_bytes().to_vec());
                parts.push(key_commit.clone());
            }
            LogBody::Share { object_id, member } => {
                parts.push(b"share".to_vec());
                parts.push(object_id.as_bytes().to_vec());
                member.encode(&mut parts);
            }
            LogBody::Unshare { object_id, user_id } => {
                parts.push(b"unshare".to_vec());
                parts.push(object_id.as_bytes().to_vec());
                parts.push(user_id.as_bytes().to_vec());
            }
            LogBody::RequireSigned { from_seq } => {
                parts.push(b"require-signed".to_vec());
                parts.push(from_seq.to_be_bytes().to_vec());
            }
            // Never verified (see `LogBody::Unknown`).
            LogBody::Unknown => parts.push(b"unknown".to_vec()),
        }
        let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
        crypto::statement("body", &refs)
    }

    /// The user this entry is about (none for the vault-wide kinds).
    pub fn subject(&self) -> Option<&str> {
        match self {
            LogBody::Add { member } | LogBody::Rekey { member } => Some(&member.user_id),
            LogBody::Role { user_id, .. }
            | LogBody::MakeOwner { user_id }
            | LogBody::Remove { user_id }
            | LogBody::Leave { user_id }
            | LogBody::Unshare { user_id, .. } => Some(user_id),
            LogBody::Share { member, .. } => Some(&member.user_id),
            LogBody::Create { .. }
            | LogBody::Checkpoint { .. }
            | LogBody::Rotate { .. }
            | LogBody::RequireSigned { .. }
            | LogBody::Unknown => None,
        }
    }
}

/// One signed entry of a vault's membership log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: u64,
    /// Hash of the previous entry (32 zero bytes for the first).
    #[serde(with = "b64")]
    pub prev: Vec<u8>,
    pub signer: String,
    /// Milliseconds since the epoch, as the signer's clock said.
    pub signed_at: i64,
    pub body: LogBody,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

impl LogEntry {
    /// Sign `body` as the entry after `prev` (`None`: the first entry).
    pub fn sign(
        account: &AccountSecret,
        signer: &str,
        vault_id: &str,
        prev: Option<&LogEntry>,
        body: LogBody,
        signed_at: i64,
    ) -> LogEntry {
        let (seq, prev) = match prev {
            Some(p) => (p.seq + 1, p.hash().to_vec()),
            None => (0, vec![0; 32]),
        };
        let mut e = LogEntry {
            seq,
            prev,
            signer: signer.to_string(),
            signed_at,
            body,
            signature: vec![],
        };
        e.signature = account.sign(&e.statement(vault_id)).to_vec();
        e
    }

    fn statement(&self, vault_id: &str) -> Vec<u8> {
        crypto::statement(
            SIG_DOMAIN,
            &[
                vault_id.as_bytes(),
                &self.seq.to_be_bytes(),
                &self.prev,
                self.signer.as_bytes(),
                &self.signed_at.to_be_bytes(),
                &self.body.encode(),
            ],
        )
    }

    /// Check the signature with the signer's public key.
    pub fn verify_signature(&self, vault_id: &str, signer: &AccountPublic) -> Result<(), LogError> {
        crypto::ed25519_verify(
            &signer.sign_public,
            &self.statement(vault_id),
            &self.signature,
        )
        .map_err(|_| LogError::BadSignature(self.seq))
    }

    /// What the next entry's `prev` must be.
    pub fn hash(&self) -> Key32 {
        let mut m = Vec::new();
        m.extend_from_slice(&(self.seq).to_be_bytes());
        m.extend_from_slice(&self.prev);
        m.extend_from_slice(&(self.signer.len() as u32).to_be_bytes());
        m.extend_from_slice(self.signer.as_bytes());
        m.extend_from_slice(&self.signed_at.to_be_bytes());
        m.extend_from_slice(&self.body.encode());
        m.extend_from_slice(&self.signature);
        crypto::kdf_hash(HASH_CONTEXT, &m)
    }
}

/// Commitment to a vault key, published in the log: lets members check that
/// the key they were given is the one the owner made. A key is 256 random
/// bits, so the commitment reveals nothing about it.
pub fn key_commitment(vault_id: &str, epoch: u32, key: &VaultKey) -> Vec<u8> {
    let m = crypto::statement("key", &[vault_id.as_bytes(), &epoch.to_be_bytes(), &key.0]);
    crypto::kdf_hash(COMMIT_CONTEXT, &m).to_vec()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LogError {
    #[error("the membership log is empty")]
    Empty,
    #[error("membership log entry {0} is out of sequence")]
    Sequence(u64),
    #[error("membership log entry {0} does not continue the log (its history was changed)")]
    Fork(u64),
    #[error("membership log entry {0} has an invalid signature")]
    BadSignature(u64),
    #[error("membership log entry {0} is not signed by someone allowed to make it")]
    Signer(u64),
    #[error("membership log entry {0} is not valid here: {1}")]
    Invalid(u64, &'static str),
    #[error("malformed key in the membership log")]
    Key,
    /// Not a security problem: the app is too old for this log.
    #[error("membership log entry {0} is of a kind a newer version of notekpr added")]
    Newer(u64),
}

impl From<CryptoError> for LogError {
    fn from(_: CryptoError) -> Self {
        LogError::Key
    }
}

/// Who started a log, and the account key its first entry pins for them:
/// what [`LogState::verify`] is anchored to. Apps check that key against
/// their own (their vault) or the one they pinned for that person before
/// verifying with it. `None`: no first entry, or one without their keys.
pub fn creator(entries: &[LogEntry]) -> Option<(String, AccountPublic)> {
    let first = entries.first()?;
    let pinned = match &first.body {
        LogBody::Create { owner, .. } => owner,
        LogBody::Checkpoint { members, .. } => {
            members.iter().find(|m| m.user_id == first.signer)?
        }
        _ => return None,
    };
    Some((first.signer.clone(), pinned.public().ok()??))
}

/// What a verified log says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogState {
    pub vault_id: String,
    pub members: BTreeMap<String, LogMember>,
    /// The vault key epoch as of the log, and each epoch's key commitment
    /// (epochs from before a `Checkpoint` have none).
    pub epoch: u32,
    pub key_commits: BTreeMap<u32, Vec<u8>>,
    /// The last verified entry's sequence number and hash.
    pub head_seq: u64,
    pub head_hash: Key32,
    /// Notes shared with people who are not members: (note, user) → them.
    pub shares: BTreeMap<(String, String), LogMember>,
    /// Content after this log sequence number must be signed.
    pub signed_from: Option<i64>,
    /// Every key ever pinned for someone while they could write the vault,
    /// and for editors of single notes: who may have authored content.
    writers: BTreeMap<String, Vec<AccountPublic>>,
    note_editors: BTreeMap<(String, String), Vec<AccountPublic>>,
    /// Every key ever pinned for a member (any role), and for people single
    /// notes were shared with (either role): who may have commented.
    readers: BTreeMap<String, Vec<AccountPublic>>,
    note_readers: BTreeMap<(String, String), Vec<AccountPublic>>,
}

fn remember(keys: &mut Vec<AccountPublic>, k: AccountPublic) {
    if !keys.contains(&k) {
        keys.push(k);
    }
}

impl LogState {
    /// Verify a complete log, starting from its first entry, against the
    /// creator's public key (pinned, or our own if we created the vault).
    pub fn verify(
        vault_id: &str,
        owner_key: &AccountPublic,
        entries: &[LogEntry],
    ) -> Result<LogState, LogError> {
        let first = entries.first().ok_or(LogError::Empty)?;
        if first.seq != 0 {
            return Err(LogError::Sequence(first.seq));
        }
        if first.prev != [0u8; 32] {
            return Err(LogError::Fork(0));
        }
        if first.body == LogBody::Unknown {
            return Err(LogError::Newer(0));
        }
        first.verify_signature(vault_id, owner_key)?;
        let owner = &first.signer;
        let mut state = match &first.body {
            LogBody::Create {
                owner: o,
                key_commit,
            } => {
                if o.user_id != *owner || o.role != Role::Owner || o.keys(0)? != *owner_key {
                    return Err(LogError::Invalid(0, "the creator must be the owner"));
                }
                check_commit(0, key_commit)?;
                LogState {
                    vault_id: vault_id.to_string(),
                    members: BTreeMap::from([(owner.clone(), o.clone())]),
                    epoch: 1,
                    key_commits: BTreeMap::from([(1, key_commit.clone())]),
                    head_seq: 0,
                    head_hash: first.hash(),
                    shares: BTreeMap::new(),
                    signed_from: None,
                    writers: BTreeMap::new(),
                    note_editors: BTreeMap::new(),
                    readers: BTreeMap::new(),
                    note_readers: BTreeMap::new(),
                }
            }
            LogBody::Checkpoint {
                members,
                epoch,
                key_commit,
            } => {
                check_commit(0, key_commit)?;
                let mut map = BTreeMap::new();
                for m in members {
                    m.public()?;
                    if map.insert(m.user_id.clone(), m.clone()).is_some() {
                        return Err(LogError::Invalid(0, "a member is listed twice"));
                    }
                }
                let me = map
                    .get(owner)
                    .ok_or(LogError::Invalid(0, "the owner must be a member"))?;
                if me.role != Role::Owner || me.keys(0)? != *owner_key {
                    return Err(LogError::Invalid(0, "the signer must be the owner"));
                }
                if map.values().filter(|m| m.role == Role::Owner).count() != 1 {
                    return Err(LogError::Invalid(0, "a vault has one owner"));
                }
                if *epoch == 0 {
                    return Err(LogError::Invalid(0, "epochs start at 1"));
                }
                LogState {
                    vault_id: vault_id.to_string(),
                    members: map,
                    epoch: *epoch,
                    key_commits: BTreeMap::from([(*epoch, key_commit.clone())]),
                    head_seq: 0,
                    head_hash: first.hash(),
                    shares: BTreeMap::new(),
                    signed_from: None,
                    writers: BTreeMap::new(),
                    note_editors: BTreeMap::new(),
                    readers: BTreeMap::new(),
                    note_readers: BTreeMap::new(),
                }
            }
            _ => {
                return Err(LogError::Invalid(
                    0,
                    "a log starts with create or checkpoint",
                ));
            }
        };
        for m in state.members.values().cloned().collect::<Vec<_>>() {
            state.note_writer(&m);
        }
        state.extend(&entries[1..])?;
        Ok(state)
    }

    /// Remember `m`'s key as one that may have authored content (if they
    /// can write) or comments.
    fn note_writer(&mut self, m: &LogMember) {
        let Ok(Some(k)) = m.public() else { return };
        if m.role.can_write() {
            remember(
                self.writers.entry(m.user_id.clone()).or_default(),
                k.clone(),
            );
        }
        remember(self.readers.entry(m.user_id.clone()).or_default(), k);
    }

    /// Verify and apply entries that continue the log (e.g. fetched since
    /// the last verified head).
    pub fn extend(&mut self, entries: &[LogEntry]) -> Result<(), LogError> {
        for e in entries {
            self.apply(e)?;
        }
        Ok(())
    }

    /// Verify and apply the next entry.
    pub fn apply(&mut self, e: &LogEntry) -> Result<(), LogError> {
        if e.seq != self.head_seq + 1 {
            return Err(LogError::Sequence(e.seq));
        }
        if e.prev != self.head_hash {
            return Err(LogError::Fork(e.seq));
        }
        if e.body == LogBody::Unknown {
            return Err(LogError::Newer(e.seq));
        }
        let signer_key = match &e.body {
            LogBody::Leave { user_id } => {
                if *user_id != e.signer {
                    return Err(LogError::Signer(e.seq));
                }
                self.members
                    .get(user_id)
                    .ok_or(LogError::Invalid(e.seq, "not a member"))?
                    .keys(e.seq)?
            }
            _ => self
                .members
                .get(&e.signer)
                .filter(|m| m.role == Role::Owner)
                .ok_or(LogError::Signer(e.seq))?
                .keys(e.seq)?,
        };
        e.verify_signature(&self.vault_id, &signer_key)?;
        let n = e.seq;
        match &e.body {
            LogBody::Create { .. } | LogBody::Checkpoint { .. } => {
                return Err(LogError::Invalid(n, "only the first entry starts the log"));
            }
            LogBody::Add { member } => {
                member.public()?;
                if member.role == Role::Owner {
                    return Err(LogError::Invalid(n, "owners are made from members"));
                }
                if self.members.contains_key(&member.user_id) {
                    return Err(LogError::Invalid(n, "already a member"));
                }
                // Membership supersedes their single-note shares.
                self.shares.retain(|(_, u), _| *u != member.user_id);
                self.members.insert(member.user_id.clone(), member.clone());
                self.note_writer(member);
            }
            LogBody::Role { user_id, role } => {
                if *role == Role::Owner {
                    return Err(LogError::Invalid(n, "owners are made with make_owner"));
                }
                if !self.another_owner(n, user_id)? {
                    return Err(LogError::Invalid(n, "a vault keeps an owner"));
                }
                let m = self.members.get_mut(user_id).expect("a member, checked");
                m.role = role.clone();
                let m = m.clone();
                self.note_writer(&m);
            }
            LogBody::Rekey { member } => {
                member.keys(n)?;
                let m = self
                    .members
                    .get_mut(&member.user_id)
                    .ok_or(LogError::Invalid(n, "not a member"))?;
                if member.role != m.role {
                    return Err(LogError::Invalid(n, "a new key keeps the role"));
                }
                if m.role == Role::Owner && member.user_id == e.signer {
                    return Err(LogError::Invalid(
                        n,
                        "another owner pins an owner's new key",
                    ));
                }
                *m = member.clone();
                self.note_writer(member);
            }
            LogBody::MakeOwner { user_id } => {
                let m = self
                    .members
                    .get_mut(user_id)
                    .ok_or(LogError::Invalid(n, "not a member"))?;
                if m.role == Role::Owner {
                    return Err(LogError::Invalid(n, "already an owner"));
                }
                // Owners sign: they need keys.
                m.keys(n)?;
                m.role = Role::Owner;
                let m = m.clone();
                self.note_writer(&m);
            }
            LogBody::Remove { user_id } | LogBody::Leave { user_id } => {
                if !self.another_owner(n, user_id)? {
                    return Err(LogError::Invalid(n, "the last owner can't leave"));
                }
                self.members.remove(user_id);
            }
            LogBody::Rotate { epoch, key_commit } => {
                check_commit(n, key_commit)?;
                if *epoch != self.epoch + 1 {
                    return Err(LogError::Invalid(n, "epochs go up one at a time"));
                }
                self.epoch = *epoch;
                self.key_commits.insert(*epoch, key_commit.clone());
            }
            LogBody::Share { object_id, member } => {
                let key = member.keys(n)?;
                if self.members.contains_key(&member.user_id) {
                    return Err(LogError::Invalid(
                        n,
                        "members don't need single notes shared",
                    ));
                }
                // Roles from newer releases are kept, and may only read.
                if matches!(member.role, Role::Owner | Role::Admin) {
                    return Err(LogError::Invalid(n, "notes are shared as editor or viewer"));
                }
                let who = (object_id.clone(), member.user_id.clone());
                if member.role == Role::Editor {
                    remember(
                        self.note_editors.entry(who.clone()).or_default(),
                        key.clone(),
                    );
                }
                remember(self.note_readers.entry(who).or_default(), key);
                self.shares
                    .insert((object_id.clone(), member.user_id.clone()), member.clone());
            }
            LogBody::Unshare { object_id, user_id } => {
                if self
                    .shares
                    .remove(&(object_id.clone(), user_id.clone()))
                    .is_none()
                {
                    return Err(LogError::Invalid(n, "the note isn't shared with them"));
                }
            }
            LogBody::RequireSigned { from_seq } => {
                if self.signed_from.is_some() || *from_seq < 0 {
                    return Err(LogError::Invalid(n, "signatures are required once"));
                }
                self.signed_from = Some(*from_seq);
            }
            LogBody::Unknown => return Err(LogError::Newer(n)),
        }
        self.head_seq = n;
        self.head_hash = e.hash();
        Ok(())
    }

    pub fn member(&self, user_id: &str) -> Option<&LogMember> {
        self.members.get(user_id)
    }

    /// The vault's owners: each signs changes and gives out keys.
    pub fn owners(&self) -> impl Iterator<Item = &LogMember> {
        self.members.values().filter(|m| m.role == Role::Owner)
    }

    pub fn is_owner(&self, user_id: &str) -> bool {
        self.members
            .get(user_id)
            .is_some_and(|m| m.role == Role::Owner)
    }

    /// Whether member `user_id` may stop being an owner (leave, be removed,
    /// be given another role): they aren't one, or another stays.
    fn another_owner(&self, n: u64, user_id: &str) -> Result<bool, LogError> {
        let m = self
            .members
            .get(user_id)
            .ok_or(LogError::Invalid(n, "not a member"))?;
        Ok(m.role != Role::Owner || self.owners().any(|o| o.user_id != user_id))
    }

    /// Who may have authored the vault's content.
    pub fn authors(&self) -> Authors {
        Authors {
            vault_id: self.vault_id.clone(),
            signed_from: self.signed_from,
            writers: self.writers.clone(),
            note_editors: self.note_editors.clone(),
            readers: self.readers.clone(),
            note_readers: self.note_readers.clone(),
        }
    }

    /// Whether a member's role and key are what the log says.
    pub fn vouches_for(&self, user_id: &str, role: Role, key: &AccountPublic) -> bool {
        self.members
            .get(user_id)
            .is_some_and(|m| m.role == role && m.public().ok().flatten().is_some_and(|k| k == *key))
    }

    /// Check a vault key against the log: `Ok(true)` if its epoch's
    /// commitment matches, `Ok(false)` if the log has no commitment for that
    /// epoch (from before a checkpoint), an error if it doesn't match or the
    /// epoch is newer than the log (a key the owner never announced).
    pub fn check_key(&self, epoch: u32, key: &VaultKey) -> Result<bool, KeyCheckError> {
        if epoch > self.epoch {
            return Err(KeyCheckError::Unannounced(epoch));
        }
        match self.key_commits.get(&epoch) {
            Some(c) if *c == key_commitment(&self.vault_id, epoch, key) => Ok(true),
            Some(_) => Err(KeyCheckError::Mismatch(epoch)),
            None => Ok(false),
        }
    }
}

// ---------------------------------------------------------------- note key grants

const GRANT_DOMAIN: &str = "notekpr/v1/sig/note-key";
const NOTE_COMMIT_CONTEXT: &str = "notekpr v1 note key commitment";

/// Commitment to a note's document key (as [`key_commitment`] for vault keys).
pub fn note_key_commitment(vault_id: &str, object_id: &str, epoch: u32, key: &VaultKey) -> Vec<u8> {
    let m = crypto::statement(
        "note-key",
        &[
            vault_id.as_bytes(),
            object_id.as_bytes(),
            &epoch.to_be_bytes(),
            &key.0,
        ],
    );
    crypto::kdf_hash(NOTE_COMMIT_CONTEXT, &m).to_vec()
}

fn grant_statement(
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    epoch: u32,
    commit: &[u8],
) -> Vec<u8> {
    crypto::statement(
        GRANT_DOMAIN,
        &[
            vault_id.as_bytes(),
            object_id.as_bytes(),
            user_id.as_bytes(),
            &epoch.to_be_bytes(),
            commit,
        ],
    )
}

/// The owner's signature on giving `user_id` a note's key at `epoch`: the
/// person it is shared with checks it, so a server can't hand them a key of
/// its own (and then read what they write).
pub fn sign_note_grant(
    owner: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    epoch: u32,
    key: &VaultKey,
) -> Vec<u8> {
    let commit = note_key_commitment(vault_id, object_id, epoch, key);
    owner
        .sign(&grant_statement(
            vault_id, object_id, user_id, epoch, &commit,
        ))
        .to_vec()
}

pub fn verify_note_grant(
    owner: &AccountPublic,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    epoch: u32,
    key: &VaultKey,
    signature: &[u8],
) -> bool {
    let commit = note_key_commitment(vault_id, object_id, epoch, key);
    crypto::ed25519_verify(
        &owner.sign_public,
        &grant_statement(vault_id, object_id, user_id, epoch, &commit),
        signature,
    )
    .is_ok()
}

// ---------------------------------------------------------------- authorship

const UPDATE_DOMAIN: &str = "notekpr/v1/sig/update";
const SNAPSHOT_DOMAIN: &str = "notekpr/v1/sig/snapshot";

fn update_statement(vault_id: &str, object_id: &str, payload: &[u8]) -> Vec<u8> {
    crypto::statement(
        UPDATE_DOMAIN,
        &[vault_id.as_bytes(), object_id.as_bytes(), payload],
    )
}

fn snapshot_statement(vault_id: &str, object_id: &str, covers_seq: i64, payload: &[u8]) -> Vec<u8> {
    crypto::statement(
        SNAPSHOT_DOMAIN,
        &[
            vault_id.as_bytes(),
            object_id.as_bytes(),
            &covers_seq.to_be_bytes(),
            payload,
        ],
    )
}

/// The author's signature over an update's ciphertext.
pub fn sign_update(
    account: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    payload: &[u8],
) -> Vec<u8> {
    account
        .sign(&update_statement(vault_id, object_id, payload))
        .to_vec()
}

/// The author's signature over a snapshot (its ciphertext and what it covers).
pub fn sign_snapshot(
    account: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    covers_seq: i64,
    payload: &[u8],
) -> Vec<u8> {
    account
        .sign(&snapshot_statement(
            vault_id, object_id, covers_seq, payload,
        ))
        .to_vec()
}

pub fn verify_update(
    key: &AccountPublic,
    vault_id: &str,
    object_id: &str,
    payload: &[u8],
    signature: &[u8],
) -> bool {
    crypto::ed25519_verify(
        &key.sign_public,
        &update_statement(vault_id, object_id, payload),
        signature,
    )
    .is_ok()
}

pub fn verify_snapshot(
    key: &AccountPublic,
    vault_id: &str,
    object_id: &str,
    covers_seq: i64,
    payload: &[u8],
    signature: &[u8],
) -> bool {
    crypto::ed25519_verify(
        &key.sign_public,
        &snapshot_statement(vault_id, object_id, covers_seq, payload),
        signature,
    )
    .is_ok()
}

/// Who may have authored a vault's content, from its log: every key pinned
/// for someone while they could write the vault, and for editors of single
/// notes (that note only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Authors {
    vault_id: String,
    signed_from: Option<i64>,
    writers: BTreeMap<String, Vec<AccountPublic>>,
    note_editors: BTreeMap<(String, String), Vec<AccountPublic>>,
    readers: BTreeMap<String, Vec<AccountPublic>>,
    note_readers: BTreeMap<(String, String), Vec<AccountPublic>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthorError {
    #[error("a change wasn't signed by its author, but this vault requires it")]
    Unsigned,
    #[error("a change claims to be by {0} but doesn't carry their signature")]
    BadSignature(String),
    #[error("a change is signed by {0}, who could never write here")]
    NotAWriter(String),
}

impl AuthorError {
    /// A newer copy of the log might allow it (the author was added, or got
    /// a new key, after our copy): check again after reloading the log.
    pub fn may_be_outdated(&self) -> bool {
        !matches!(self, AuthorError::Unsigned)
    }
}

impl Authors {
    fn keys<'a>(&'a self, object_id: &str, author: &str) -> Vec<&'a AccountPublic> {
        // Anyone who can read a note may comment on it.
        if let Some(note) = crate::protocol::comments_note(object_id) {
            return self.comment_keys(note, author);
        }
        let mut keys: Vec<&AccountPublic> =
            self.writers.get(author).into_iter().flatten().collect();
        keys.extend(
            self.note_editors
                .get(&(object_id.to_string(), author.to_string()))
                .into_iter()
                .flatten(),
        );
        keys
    }

    fn check(
        &self,
        object_id: &str,
        author: &str,
        required: bool,
        signature: Option<&[u8]>,
        verify: impl Fn(&AccountPublic, &[u8]) -> bool,
    ) -> Result<(), AuthorError> {
        let Some(sig) = signature else {
            return if required {
                Err(AuthorError::Unsigned)
            } else {
                Ok(())
            };
        };
        let keys = self.keys(object_id, author);
        if keys.is_empty() {
            // Content from before the log may be signed by people it doesn't
            // know (e.g. a note shared before it started).
            return if required {
                Err(AuthorError::NotAWriter(author.to_string()))
            } else {
                Ok(())
            };
        }
        if keys.iter().any(|k| verify(k, sig)) {
            Ok(())
        } else {
            Err(AuthorError::BadSignature(author.to_string()))
        }
    }

    /// Keys `user` may have signed comments on note `note_id` with: those of
    /// any member, and of people the note was shared with.
    pub fn comment_keys<'a>(&'a self, note_id: &str, user: &str) -> Vec<&'a AccountPublic> {
        let mut keys: Vec<&AccountPublic> = self.readers.get(user).into_iter().flatten().collect();
        keys.extend(
            self.note_readers
                .get(&(note_id.to_string(), user.to_string()))
                .into_iter()
                .flatten(),
        );
        keys
    }

    /// Check an update the server says `author` wrote, at log sequence `seq`.
    pub fn check_update(
        &self,
        object_id: &str,
        seq: i64,
        author: &str,
        payload: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<(), AuthorError> {
        let required = self.signed_from.is_some_and(|f| seq > f);
        self.check(object_id, author, required, signature, |k, sig| {
            verify_update(k, &self.vault_id, object_id, payload, sig)
        })
    }

    /// Check a snapshot the server says `author` made, covering `covers_seq`.
    pub fn check_snapshot(
        &self,
        object_id: &str,
        covers_seq: i64,
        author: &str,
        payload: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<(), AuthorError> {
        let required = self.signed_from.is_some_and(|f| covers_seq > f);
        self.check(object_id, author, required, signature, |k, sig| {
            verify_snapshot(k, &self.vault_id, object_id, covers_seq, payload, sig)
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyCheckError {
    #[error("the vault key of epoch {0} is not the one the owner made")]
    Mismatch(u32),
    #[error("the vault key of epoch {0} was never announced by the owner")]
    Unannounced(u32),
}

fn check_commit(seq: u64, c: &[u8]) -> Result<(), LogError> {
    if c.len() == 32 {
        Ok(())
    } else {
        Err(LogError::Invalid(seq, "a key commitment has 32 bytes"))
    }
}

fn key32(b: &[u8]) -> Result<Key32, LogError> {
    b.try_into().map_err(|_| LogError::Key)
}

/// What a role is signed as: its name, as written, so entries naming a role
/// from a newer release ([`Role::Other`]) still verify.
fn role_tag(r: &Role) -> &[u8] {
    r.as_str().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Person {
        id: String,
        secret: AccountSecret,
    }

    fn person(id: &str) -> Person {
        Person {
            id: id.into(),
            secret: AccountSecret::generate(),
        }
    }

    fn member(p: &Person, role: Role) -> LogMember {
        LogMember::new(&p.id, role, &p.secret.public())
    }

    const V: &str = "vault1";

    fn created(owner: &Person) -> (Vec<LogEntry>, VaultKey) {
        let k1 = VaultKey::generate();
        let e = LogEntry::sign(
            &owner.secret,
            &owner.id,
            V,
            None,
            LogBody::Create {
                owner: member(owner, Role::Owner),
                key_commit: key_commitment(V, 1, &k1),
            },
            1,
        );
        (vec![e], k1)
    }

    fn append(log: &mut Vec<LogEntry>, by: &Person, body: LogBody) {
        let e = LogEntry::sign(&by.secret, &by.id, V, log.last(), body, 2);
        log.push(e);
    }

    #[test]
    fn a_log_tracks_members_roles_and_keys() {
        let (alice, bob, carol) = (person("alice"), person("bob"), person("carol"));
        let (mut log, k1) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Editor),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&carol, Role::Viewer),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Role {
                user_id: "carol".into(),
                role: Role::Editor,
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Remove {
                user_id: "bob".into(),
            },
        );
        let k2 = VaultKey::generate();
        append(
            &mut log,
            &alice,
            LogBody::Rotate {
                epoch: 2,
                key_commit: key_commitment(V, 2, &k2),
            },
        );
        append(
            &mut log,
            &carol,
            LogBody::Leave {
                user_id: "carol".into(),
            },
        );
        let s = LogState::verify(V, &alice.secret.public(), &log).unwrap();
        assert_eq!(s.members.keys().collect::<Vec<_>>(), vec!["alice"]);
        assert_eq!(s.epoch, 2);
        assert_eq!(s.check_key(1, &k1), Ok(true));
        assert_eq!(s.check_key(2, &k2), Ok(true));
        assert_eq!(s.check_key(2, &k1), Err(KeyCheckError::Mismatch(2)));
        assert_eq!(s.check_key(3, &k2), Err(KeyCheckError::Unannounced(3)));
        assert_eq!(s.head_seq, 6);
    }

    #[test]
    fn roles_from_newer_releases_verify_as_written_and_may_only_read() {
        let (alice, carol, dave, rita) = (
            person("alice"),
            person("carol"),
            person("dave"),
            person("rita"),
        );
        let other = |name: &str| Role::Other(name.into());
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&dave, other("commenter")),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&carol, Role::Editor),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Role {
                user_id: "carol".into(),
                role: other("auditor"),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Share {
                object_id: "n9".into(),
                member: member(&rita, other("commenter")),
            },
        );
        append(&mut log, &alice, LogBody::RequireSigned { from_seq: 0 });
        // As an app of this release reads what a newer one wrote.
        let json = serde_json::to_string(&log).unwrap();
        let read: Vec<LogEntry> = serde_json::from_str(&json).unwrap();
        let s = LogState::verify(V, &alice.secret.public(), &read).unwrap();
        assert_eq!(s.member("dave").unwrap().role, other("commenter"));
        assert_eq!(s.member("carol").unwrap().role, other("auditor"));

        // They may comment, but nothing they write is theirs to write.
        let a = s.authors();
        let sig = sign_update(&dave.secret, V, "n1", b"ciphertext");
        assert_eq!(
            a.check_update("n1", 9, "dave", b"ciphertext", Some(&sig)),
            Err(AuthorError::NotAWriter("dave".into()))
        );
        assert!(!a.comment_keys("n1", "dave").is_empty());
        let sig = sign_update(&rita.secret, V, "n9", b"ciphertext");
        assert!(
            a.check_update("n9", 9, "rita", b"ciphertext", Some(&sig))
                .is_err()
        );
        assert!(!a.comment_keys("n9", "rita").is_empty());

        // The name is signed: a server can't turn it into a known role.
        let forged: Vec<LogEntry> =
            serde_json::from_str(&json.replace("\"commenter\"", "\"editor\"")).unwrap();
        assert!(LogState::verify(V, &alice.secret.public(), &forged).is_err());
    }

    #[test]
    fn entries_of_kinds_from_newer_releases_stop_the_check_plainly() {
        let (alice, bob) = (person("alice"), person("bob"));
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Editor),
            },
        );
        // As a newer release writes one, signed.
        let mut json = serde_json::to_value(&log).unwrap();
        json[1]["body"] = serde_json::json!({ "kind": "transfer", "to": "bob" });
        let read: Vec<LogEntry> = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(read[1].body, LogBody::Unknown);
        assert_eq!(
            LogState::verify(V, &alice.secret.public(), &read).err(),
            Some(LogError::Newer(1))
        );
        // Also as the first entry.
        json[0]["body"] = serde_json::json!({ "kind": "genesis" });
        let read: Vec<LogEntry> = serde_json::from_value(json).unwrap();
        assert_eq!(
            LogState::verify(V, &alice.secret.public(), &read).err(),
            Some(LogError::Newer(0))
        );
    }

    #[test]
    fn admins_are_signed_members_who_write_but_never_get_single_notes() {
        let (alice, bob, carol, dave) = (
            person("alice"),
            person("bob"),
            person("carol"),
            person("dave"),
        );
        let anchor = alice.secret.public();
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Admin),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&carol, Role::Viewer),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Role {
                user_id: "carol".into(),
                role: Role::Admin,
            },
        );
        append(&mut log, &alice, LogBody::RequireSigned { from_seq: 0 });
        let s = LogState::verify(V, &anchor, &log).unwrap();
        assert!(s.vouches_for("bob", Role::Admin, &bob.secret.public()));
        assert!(!s.vouches_for("bob", Role::Editor, &bob.secret.public()));
        assert_eq!(s.member("carol").unwrap().role, Role::Admin);

        // Admins write: what they sign is theirs.
        let sig = sign_update(&carol.secret, V, "n1", b"ciphertext");
        assert_eq!(
            s.authors()
                .check_update("n1", 5, "carol", b"ciphertext", Some(&sig)),
            Ok(())
        );

        // The role is signed: a server can't make an editor of an admin, or
        // the other way round.
        let mut forged = log.clone();
        if let LogBody::Add { member } = &mut forged[1].body {
            member.role = Role::Editor;
        }
        assert_eq!(
            LogState::verify(V, &anchor, &forged),
            Err(LogError::BadSignature(1))
        );

        // A single note is shared as editor or viewer, never as admin.
        append(
            &mut log,
            &alice,
            LogBody::Share {
                object_id: "n1".into(),
                member: member(&dave, Role::Admin),
            },
        );
        assert_eq!(
            LogState::verify(V, &anchor, &log),
            Err(LogError::Invalid(5, "notes are shared as editor or viewer"))
        );
    }

    #[test]
    fn the_server_cannot_add_anyone_or_change_history() {
        let (alice, bob, mallory) = (person("alice"), person("bob"), person("mallory"));
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Editor),
            },
        );
        let anchor = alice.secret.public();

        // An entry signed by anyone but the owner.
        let mut forged = log.clone();
        append(
            &mut forged,
            &mallory,
            LogBody::Add {
                member: member(&mallory, Role::Editor),
            },
        );
        assert_eq!(
            LogState::verify(V, &anchor, &forged),
            Err(LogError::Signer(2))
        );
        let mut forged = log.clone();
        let e = LogEntry::sign(
            &mallory.secret,
            "alice",
            V,
            log.last(),
            LogBody::Add {
                member: member(&mallory, Role::Editor),
            },
            3,
        );
        forged.push(e);
        assert_eq!(
            LogState::verify(V, &anchor, &forged),
            Err(LogError::BadSignature(2))
        );

        // A changed entry, a dropped entry, a swapped key, another vault.
        let mut changed = log.clone();
        if let LogBody::Add { member } = &mut changed[1].body {
            member.role = Role::Viewer;
        }
        assert_eq!(
            LogState::verify(V, &anchor, &changed),
            Err(LogError::BadSignature(1))
        );
        let mut dropped = log.clone();
        append(
            &mut dropped,
            &alice,
            LogBody::Remove {
                user_id: "bob".into(),
            },
        );
        dropped.remove(1);
        assert!(LogState::verify(V, &anchor, &dropped).is_err());
        let mut swapped = log.clone();
        if let LogBody::Add { member } = &mut swapped[1].body {
            member.enc_public = mallory.secret.public().enc_public.to_vec();
        }
        assert!(LogState::verify(V, &anchor, &swapped).is_err());
        assert!(LogState::verify("other", &anchor, &log).is_err());
        // A log made by someone else for the owner's vault.
        let (fake, _) = created(&mallory);
        assert!(LogState::verify(V, &anchor, &fake).is_err());

        // A fork: the client verified one history, the server continues
        // another (both signed by the owner at some point).
        let mut ours = LogState::verify(V, &anchor, &log).unwrap();
        let mut theirs = log[..1].to_vec();
        append(
            &mut theirs,
            &alice,
            LogBody::Add {
                member: member(&mallory, Role::Editor),
            },
        );
        append(
            &mut theirs,
            &alice,
            LogBody::Remove {
                user_id: "mallory".into(),
            },
        );
        assert_eq!(ours.extend(&theirs[2..]), Err(LogError::Fork(2)));
        assert_eq!(ours.extend(&theirs[1..]), Err(LogError::Sequence(1)));
    }

    #[test]
    fn rules_are_enforced() {
        let (alice, bob) = (person("alice"), person("bob"));
        let anchor = alice.secret.public();
        let check = |body: LogBody, signer: &Person| {
            let (mut log, _) = created(&alice);
            append(
                &mut log,
                &alice,
                LogBody::Add {
                    member: member(&bob, Role::Editor),
                },
            );
            append(&mut log, signer, body);
            LogState::verify(V, &anchor, &log)
        };
        assert!(
            check(
                LogBody::Add {
                    member: member(&bob, Role::Viewer)
                },
                &alice
            )
            .is_err(),
            "twice"
        );
        assert!(
            check(
                LogBody::Remove {
                    user_id: "alice".into()
                },
                &alice
            )
            .is_err(),
            "owner"
        );
        assert!(
            check(
                LogBody::Role {
                    user_id: "bob".into(),
                    role: Role::Owner
                },
                &alice
            )
            .is_err()
        );
        assert!(
            check(
                LogBody::Leave {
                    user_id: "bob".into()
                },
                &alice
            )
            .is_err(),
            "leave is self-signed"
        );
        assert!(
            check(
                LogBody::Leave {
                    user_id: "bob".into()
                },
                &bob
            )
            .is_ok()
        );
        assert!(
            check(
                LogBody::Remove {
                    user_id: "bob".into()
                },
                &bob
            )
            .is_err(),
            "only the owner removes"
        );
        let c = key_commitment(V, 3, &VaultKey::generate());
        assert!(
            check(
                LogBody::Rotate {
                    epoch: 3,
                    key_commit: c
                },
                &alice
            )
            .is_err(),
            "skipped an epoch"
        );
        let new_bob = person("bob");
        assert!(
            check(
                LogBody::Rekey {
                    member: member(&new_bob, Role::Editor)
                },
                &alice
            )
            .is_ok()
        );
        assert!(
            check(
                LogBody::Rekey {
                    member: member(&new_bob, Role::Viewer)
                },
                &alice
            )
            .is_err()
        );
    }

    #[test]
    fn owners_are_equal_and_a_vault_keeps_one() {
        let (alice, bob, carol, dave) = (
            person("alice"),
            person("bob"),
            person("carol"),
            person("dave"),
        );
        let anchor = alice.secret.public();
        let (mut log, _) = created(&alice);
        for p in [&bob, &carol] {
            append(
                &mut log,
                &alice,
                LogBody::Add {
                    member: member(p, Role::Editor),
                },
            );
        }
        let check = |log: &[LogEntry], by: &Person, body: LogBody| {
            let mut log = log.to_vec();
            append(&mut log, by, body);
            LogState::verify(V, &anchor, &log)
        };
        let owner = |id: &str| LogBody::MakeOwner { user_id: id.into() };
        // Only an owner makes owners, of members with keys; owners aren't
        // added or given the role any other way.
        assert_eq!(check(&log, &bob, owner("bob")), Err(LogError::Signer(3)));
        assert!(check(&log, &alice, owner("dave")).is_err(), "not a member");
        assert!(check(&log, &alice, owner("alice")).is_err(), "already one");
        assert!(
            check(
                &log,
                &alice,
                LogBody::Add {
                    member: member(&dave, Role::Owner)
                }
            )
            .is_err()
        );
        let mut pending = log.clone();
        append(
            &mut pending,
            &alice,
            LogBody::Add {
                member: LogMember::pending("dave", Role::Editor),
            },
        );
        assert!(check(&pending, &alice, owner("dave")).is_err(), "no keys");
        append(&mut log, &alice, owner("bob"));
        let s = LogState::verify(V, &anchor, &log).unwrap();
        assert!(s.is_owner("bob") && s.is_owner("alice") && !s.is_owner("carol"));
        assert_eq!(s.owners().count(), 2);

        // Bob signs as an owner, and may remove the one who made him owner:
        // owners are equal. The log still verifies from its creator.
        append(&mut log, &bob, owner("carol"));
        append(
            &mut log,
            &bob,
            LogBody::Role {
                user_id: "alice".into(),
                role: Role::Editor,
            },
        );
        append(
            &mut log,
            &carol,
            LogBody::Remove {
                user_id: "alice".into(),
            },
        );
        let s = LogState::verify(V, &anchor, &log).unwrap();
        assert_eq!(
            s.owners().map(|m| m.user_id.as_str()).collect::<Vec<_>>(),
            vec!["bob", "carol"]
        );
        assert_eq!(creator(&log), Some(("alice".to_string(), anchor.clone())));
        // Alice, gone, signs nothing any more.
        assert_eq!(
            check(&log, &alice, owner("alice")),
            Err(LogError::Signer(7))
        );

        // An owner leaves or is demoted only while another stays.
        let mut last = log.clone();
        append(
            &mut last,
            &bob,
            LogBody::Leave {
                user_id: "bob".into(),
            },
        );
        LogState::verify(V, &anchor, &last).unwrap();
        for (by, body) in [
            (
                &carol,
                LogBody::Leave {
                    user_id: "carol".into(),
                },
            ),
            (
                &carol,
                LogBody::Remove {
                    user_id: "carol".into(),
                },
            ),
            (
                &carol,
                LogBody::Role {
                    user_id: "carol".into(),
                    role: Role::Viewer,
                },
            ),
        ] {
            assert!(check(&last, by, body).is_err(), "the last owner stays");
        }

        // An owner's new key is pinned by another owner, never by the owner.
        let new_bob = person("bob");
        let rekey = LogBody::Rekey {
            member: member(&new_bob, Role::Owner),
        };
        assert!(check(&log, &bob, rekey.clone()).is_err());
        let mut rekeyed = log.clone();
        append(&mut rekeyed, &carol, rekey);
        let s = LogState::verify(V, &anchor, &rekeyed).unwrap();
        assert!(s.vouches_for("bob", Role::Owner, &new_bob.secret.public()));
        // And the old key signs nothing then.
        assert_eq!(
            check(&rekeyed, &bob, owner("alice")),
            Err(LogError::BadSignature(8))
        );
    }

    #[test]
    fn the_creator_is_named_with_the_key_the_log_pins() {
        let alice = person("alice");
        assert_eq!(creator(&[]), None);
        let (log, _) = created(&alice);
        assert_eq!(
            creator(&log),
            Some(("alice".to_string(), alice.secret.public()))
        );
        let checkpoint = LogEntry::sign(
            &alice.secret,
            "alice",
            V,
            None,
            LogBody::Checkpoint {
                members: vec![member(&alice, Role::Owner)],
                epoch: 1,
                key_commit: key_commitment(V, 1, &VaultKey::generate()),
            },
            1,
        );
        assert_eq!(
            creator(&[checkpoint]),
            Some(("alice".to_string(), alice.secret.public()))
        );
    }

    #[test]
    fn people_without_keys_yet_get_them_pinned_later() {
        let (alice, bob) = (person("alice"), person("bob"));
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: LogMember::pending("bob", Role::Editor),
            },
        );
        let s = LogState::verify(V, &alice.secret.public(), &log).unwrap();
        assert!(!s.vouches_for("bob", Role::Editor, &bob.secret.public()));
        append(
            &mut log,
            &alice,
            LogBody::Rekey {
                member: member(&bob, Role::Editor),
            },
        );
        let s = LogState::verify(V, &alice.secret.public(), &log).unwrap();
        assert!(s.vouches_for("bob", Role::Editor, &bob.secret.public()));
        // Leaving needs keys to sign with: a pending member can't sign.
        let (mut log2, _) = created(&alice);
        append(
            &mut log2,
            &alice,
            LogBody::Add {
                member: LogMember::pending("bob", Role::Editor),
            },
        );
        append(
            &mut log2,
            &bob,
            LogBody::Leave {
                user_id: "bob".into(),
            },
        );
        assert!(LogState::verify(V, &alice.secret.public(), &log2).is_err());
    }

    #[test]
    fn a_checkpoint_starts_the_log_of_an_existing_vault() {
        let (alice, bob) = (person("alice"), person("bob"));
        let k3 = VaultKey::generate();
        let e = LogEntry::sign(
            &alice.secret,
            "alice",
            V,
            None,
            LogBody::Checkpoint {
                members: vec![member(&alice, Role::Owner), member(&bob, Role::Viewer)],
                epoch: 3,
                key_commit: key_commitment(V, 3, &k3),
            },
            1,
        );
        let s = LogState::verify(V, &alice.secret.public(), &[e]).unwrap();
        assert!(s.vouches_for("bob", Role::Viewer, &bob.secret.public()));
        assert!(!s.vouches_for("bob", Role::Editor, &bob.secret.public()));
        assert_eq!(s.check_key(3, &k3), Ok(true));
        assert_eq!(
            s.check_key(2, &k3),
            Ok(false),
            "older epochs have no commitment"
        );
    }

    #[test]
    fn content_must_come_from_someone_who_could_write_it() {
        let (alice, bob, vic, rita, eve) = (
            person("alice"),
            person("bob"),
            person("vic"),
            person("rita"),
            person("eve"),
        );
        let anchor = alice.secret.public();
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Editor),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&vic, Role::Viewer),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::Share {
                object_id: "n1".into(),
                member: member(&rita, Role::Editor),
            },
        );
        let before = LogState::verify(V, &anchor, &log).unwrap().authors();
        append(&mut log, &alice, LogBody::RequireSigned { from_seq: 10 });
        // Bob is demoted later: what he wrote as an editor stays valid.
        append(
            &mut log,
            &alice,
            LogBody::Role {
                user_id: "bob".into(),
                role: Role::Viewer,
            },
        );
        let a = LogState::verify(V, &anchor, &log).unwrap().authors();
        let upd = |p: &Person, oid: &str| {
            (
                b"ciphertext".to_vec(),
                sign_update(&p.secret, V, oid, b"ciphertext"),
            )
        };

        let (pl, sig) = upd(&bob, "n1");
        assert_eq!(a.check_update("n1", 11, "bob", &pl, Some(&sig)), Ok(()));
        assert_eq!(
            a.check_update("n1", 11, "bob", &pl, None),
            Err(AuthorError::Unsigned)
        );
        assert_eq!(
            a.check_update("n1", 9, "bob", &pl, None),
            Ok(()),
            "from before the requirement"
        );
        // Signed by Eve, but the server says Bob wrote it.
        let (pl2, forged) = upd(&eve, "n1");
        assert_eq!(
            a.check_update("n1", 11, "bob", &pl2, Some(&forged)),
            Err(AuthorError::BadSignature("bob".into()))
        );
        // A different ciphertext under Bob's signature.
        assert!(
            a.check_update("n1", 11, "bob", b"other", Some(&sig))
                .is_err()
        );
        // Vic only ever viewed; Eve was never anywhere.
        let (pl3, vsig) = upd(&vic, "n1");
        assert_eq!(
            a.check_update("n1", 11, "vic", &pl3, Some(&vsig)),
            Err(AuthorError::NotAWriter("vic".into()))
        );
        assert!(
            a.check_update("n1", 11, "eve", &pl2, Some(&forged))
                .is_err()
        );
        // Rita may edit the one note shared with her, nothing else.
        let (pl4, rsig) = upd(&rita, "n1");
        assert_eq!(a.check_update("n1", 11, "rita", &pl4, Some(&rsig)), Ok(()));
        let (pl5, rsig2) = upd(&rita, "n2");
        assert!(
            a.check_update("n2", 11, "rita", &pl5, Some(&rsig2))
                .is_err()
        );
        // Snapshots: what they cover is signed too.
        let ssig = sign_snapshot(&bob.secret, V, "n1", 12, b"state");
        assert_eq!(
            a.check_snapshot("n1", 12, "bob", b"state", Some(&ssig)),
            Ok(())
        );
        assert!(
            a.check_snapshot("n1", 99, "bob", b"state", Some(&ssig))
                .is_err(),
            "relabelled"
        );
        assert_eq!(
            a.check_snapshot("n1", 12, "bob", b"state", None),
            Err(AuthorError::Unsigned)
        );
        // Before the requirement nothing unsigned is refused.
        assert_eq!(before.check_update("n1", 11, "bob", &pl, None), Ok(()));

        // Comments: anyone who can read the note, viewers too — on the note
        // they can read, under keys the log knows.
        let (cpl, csig) = upd(&vic, "c_n1");
        assert_eq!(a.check_update("c_n1", 11, "vic", &cpl, Some(&csig)), Ok(()));
        let (rpl, rcsig) = upd(&rita, "c_n1");
        assert_eq!(
            a.check_update("c_n1", 11, "rita", &rpl, Some(&rcsig)),
            Ok(())
        );
        let (rpl2, rcsig2) = upd(&rita, "c_n2");
        assert!(
            a.check_update("c_n2", 11, "rita", &rpl2, Some(&rcsig2))
                .is_err()
        );
        let (epl, esig) = upd(&eve, "c_n1");
        assert!(
            a.check_update("c_n1", 11, "eve", &epl, Some(&esig))
                .is_err()
        );
        assert_eq!(a.comment_keys("n1", "vic"), [&vic.secret.public()]);
        assert_eq!(a.comment_keys("n1", "rita"), [&rita.secret.public()]);
        assert!(a.comment_keys("n2", "rita").is_empty());
    }

    #[test]
    fn share_rules() {
        let (alice, bob, rita) = (person("alice"), person("bob"), person("rita"));
        let anchor = alice.secret.public();
        let check = |bodies: Vec<LogBody>| {
            let (mut log, _) = created(&alice);
            append(
                &mut log,
                &alice,
                LogBody::Add {
                    member: member(&bob, Role::Editor),
                },
            );
            for b in bodies {
                append(&mut log, &alice, b);
            }
            LogState::verify(V, &anchor, &log)
        };
        let share = |p: &Person, role| LogBody::Share {
            object_id: "n1".into(),
            member: member(p, role),
        };
        assert!(
            check(vec![share(&bob, Role::Viewer)]).is_err(),
            "members have the vault"
        );
        assert!(check(vec![share(&rita, Role::Owner)]).is_err());
        assert!(
            check(vec![LogBody::Share {
                object_id: "n1".into(),
                member: LogMember::pending("rita", Role::Viewer)
            }])
            .is_err(),
            "recipients need keys"
        );
        let un = LogBody::Unshare {
            object_id: "n1".into(),
            user_id: "rita".into(),
        };
        assert!(check(vec![un.clone()]).is_err(), "not shared");
        let s = check(vec![share(&rita, Role::Viewer), share(&rita, Role::Editor)]).unwrap();
        assert_eq!(
            s.shares[&("n1".to_string(), "rita".to_string())].role,
            Role::Editor
        );
        let s = check(vec![share(&rita, Role::Viewer), un]).unwrap();
        assert!(s.shares.is_empty());
        let req = LogBody::RequireSigned { from_seq: 0 };
        assert!(check(vec![req.clone(), req]).is_err(), "once");
        // Joining the vault ends their single-note shares.
        let s = check(vec![
            share(&rita, Role::Viewer),
            LogBody::Add {
                member: member(&rita, Role::Viewer),
            },
        ])
        .unwrap();
        assert!(s.shares.is_empty());
    }

    #[test]
    fn note_grants_bind_the_key_the_person_and_the_note() {
        let (alice, mallory) = (person("alice"), person("mallory"));
        let k = VaultKey::generate();
        let sig = sign_note_grant(&alice.secret, V, "n1", "rita", 0x8000_0002, &k);
        let ok = |owner: &Person, o: &str, u: &str, e: u32, key: &VaultKey| {
            verify_note_grant(&owner.secret.public(), V, o, u, e, key, &sig)
        };
        assert!(ok(&alice, "n1", "rita", 0x8000_0002, &k));
        assert!(
            !ok(&alice, "n1", "rita", 0x8000_0002, &VaultKey::generate()),
            "another key"
        );
        assert!(!ok(&alice, "n2", "rita", 0x8000_0002, &k));
        assert!(!ok(&alice, "n1", "eve", 0x8000_0002, &k));
        assert!(!ok(&alice, "n1", "rita", 0x8000_0003, &k));
        assert!(
            !ok(&mallory, "n1", "rita", 0x8000_0002, &k),
            "not by the owner"
        );
    }

    #[test]
    fn entries_survive_json() {
        let (alice, bob) = (person("alice"), person("bob"));
        let (mut log, _) = created(&alice);
        append(
            &mut log,
            &alice,
            LogBody::Add {
                member: member(&bob, Role::Editor),
            },
        );
        append(
            &mut log,
            &alice,
            LogBody::MakeOwner {
                user_id: "bob".into(),
            },
        );
        let j = serde_json::to_string(&log).unwrap();
        assert!(j.contains(r#""kind":"make_owner""#), "{j}");
        let back: Vec<LogEntry> = serde_json::from_str(&j).unwrap();
        assert_eq!(back, log);
        assert!(LogState::verify(V, &alice.secret.public(), &back).is_ok());
    }
}
