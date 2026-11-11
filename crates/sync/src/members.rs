//! The vault membership log on the client side (R3): verifying what the
//! server hands out, signing the next entry, and deciding whom to trust.
//!
//! The rules every client applies:
//! * The log is verified against the owner's key (our own, or the one pinned
//!   for the owner); a log that doesn't verify, or that no longer contains
//!   the head we verified before, is a security error, not a sync problem.
//! * Vault keys are checked against the owner's commitments; a key the owner
//!   never committed to is not used.
//! * The owner wraps vault keys only for members the log vouches for (role
//!   and pinned account key); the server's member list alone is not enough.

use notekpr_core::keys::{AccountPublic, AccountSecret, VaultKeyring};
use notekpr_core::memberlog::{
    self, Authors, KeyCheckError, LogBody, LogEntry, LogError, LogMember, LogState, key_commitment,
};
use notekpr_core::protocol::{
    CreateSnapshotRequest, GrantSignature, KeyEnvelope, MemberInfo, ObjectKeyUpdate,
    ObjectShareInfo, PushUpdate, Role,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a vault's membership log stood when it was last verified: its
/// last entry. Kept in the person's synced, encrypted preferences
/// (`log_heads`, by vault id) as well as on each device, so that every app
/// of theirs, a new one too, refuses a log that no longer contains it, or
/// the vault shown without a log, as the app that saw it would.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LogHead {
    pub seq: u64,
    #[serde(with = "notekpr_core::b64")]
    pub hash: Vec<u8>,
}

/// Vault id → where its log stood ([`LogHead`]).
pub type LogHeads = BTreeMap<String, LogHead>;

/// Note where a verified log stands (`MemberLog::head`), unless a further
/// head is known already. Returns whether `heads` changed.
pub fn note_head(heads: &mut LogHeads, vault_id: &str, head: Option<(u64, Vec<u8>)>) -> bool {
    let Some((seq, hash)) = head else {
        return false;
    };
    if heads.get(vault_id).is_some_and(|h| h.seq >= seq) {
        return false;
    }
    heads.insert(vault_id.to_string(), LogHead { seq, hash });
    true
}

/// Join heads saved on another device: the further one of each vault.
pub fn merge_heads(theirs: &mut LogHeads, mine: &LogHeads) {
    for (v, h) in mine {
        note_head(theirs, v, Some((h.seq, h.hash.clone())));
    }
}

/// Keep what this device trusts when a synced copy of the person's
/// settings comes in (`theirs`): pins (user → key fingerprint) or vault
/// roots (vault → who started its log). The server can't make up a copy,
/// but it can hand out an older one, and once one of our apps saves that
/// again its counter is the newest. So a synced copy never drops what we
/// have, nor changes it (ours is kept: a key accepted as changed elsewhere
/// is asked about again here). `true`: the copy lacked some (save it again).
pub fn keep_known(theirs: &mut BTreeMap<String, String>, mine: &BTreeMap<String, String>) -> bool {
    let mut lacked = false;
    for (k, v) in mine {
        match theirs.get(k) {
            Some(t) if t == v => {}
            Some(_) => {
                theirs.insert(k.clone(), v.clone());
            }
            None => {
                theirs.insert(k.clone(), v.clone());
                lacked = true;
            }
        }
    }
    lacked
}

/// [`keep_known`] for log heads: a synced copy never takes one back, and at
/// the same position, ours stays (another one there is a fork the vault's
/// check then refuses). `true`: the copy lacked some.
pub fn keep_heads(theirs: &mut LogHeads, mine: &LogHeads) -> bool {
    let mut lacked = false;
    for (v, h) in mine {
        match theirs.get(v) {
            Some(t) if t.seq > h.seq || t == h => {}
            Some(t) if t.seq == h.seq => {
                theirs.insert(v.clone(), h.clone());
            }
            _ => {
                theirs.insert(v.clone(), h.clone());
                lacked = true;
            }
        }
    }
    lacked
}

/// A vault's verified membership log (or the lack of one: vaults from
/// before the log until their owner signs a checkpoint).
#[derive(Clone, Debug, Default)]
pub struct MemberLog {
    pub entries: Vec<LogEntry>,
    pub state: Option<LogState>,
}

impl MemberLog {
    /// Verify a complete log as fetched from the server. `seen` is the head
    /// verified earlier on this device (`seq`, hash): the log must still
    /// contain it, so a server can't roll the log back or swap its history.
    pub fn verify(
        vault_id: &str,
        owner_key: &AccountPublic,
        entries: Vec<LogEntry>,
        seen: Option<(u64, &[u8])>,
    ) -> Result<MemberLog, LogError> {
        if entries.is_empty() {
            return match seen {
                Some((seq, _)) => Err(LogError::Fork(seq)),
                None => Ok(MemberLog::default()),
            };
        }
        let state = LogState::verify(vault_id, owner_key, &entries)?;
        if let Some((seq, hash)) = seen {
            let ok = entries
                .get(seq as usize)
                .is_some_and(|e| e.seq == seq && e.hash().as_slice() == hash);
            if !ok {
                return Err(LogError::Fork(seq));
            }
        }
        Ok(MemberLog {
            entries,
            state: Some(state),
        })
    }

    pub fn has_log(&self) -> bool {
        self.state.is_some()
    }

    /// Check that the log still contains `head`, where it stood when verified
    /// before (on another device: the synced [`LogHead`]). A log without
    /// it, or no log at all, was rolled back or swapped.
    pub fn check_head(&self, head: &LogHead) -> Result<(), LogError> {
        let ok = self
            .entries
            .get(head.seq as usize)
            .is_some_and(|e| e.seq == head.seq && e.hash().as_slice() == head.hash);
        if ok {
            Ok(())
        } else {
            Err(LogError::Fork(head.seq))
        }
    }

    /// `(seq, hash)` of the last verified entry, to remember.
    pub fn head(&self) -> Option<(u64, Vec<u8>)> {
        self.state
            .as_ref()
            .map(|s| (s.head_seq, s.head_hash.to_vec()))
    }

    /// Sign `body` as the next entry (`None` if the vault has no log yet and
    /// `body` doesn't start one).
    pub fn next(
        &self,
        account: &AccountSecret,
        signer: &str,
        vault_id: &str,
        body: LogBody,
        now: i64,
    ) -> Option<LogEntry> {
        let starts = matches!(body, LogBody::Create { .. } | LogBody::Checkpoint { .. });
        if self.has_log() == starts {
            return None;
        }
        Some(LogEntry::sign(
            account,
            signer,
            vault_id,
            self.entries.last(),
            body,
            now,
        ))
    }

    /// Sign `body` as the entry after `prev` — signed, but not sent yet: two
    /// changes in one request, such as removing someone and the new key —
    /// or, without one, as the next entry (as [`MemberLog::next`]).
    pub fn next_after(
        &self,
        account: &AccountSecret,
        signer: &str,
        vault_id: &str,
        prev: Option<&LogEntry>,
        body: LogBody,
        now: i64,
    ) -> Option<LogEntry> {
        match prev {
            Some(p) => Some(LogEntry::sign(
                account,
                signer,
                vault_id,
                Some(p),
                body,
                now,
            )),
            None => self.next(account, signer, vault_id, body, now),
        }
    }

    /// Whether the server's member is who the log says (role and key; for
    /// someone without keys, that they are pending in the log too). Without
    /// a log, everyone is taken as listed (vaults from before the log).
    pub fn vouches_for(&self, m: &MemberInfo) -> bool {
        let Some(s) = &self.state else {
            return true;
        };
        let Some(logged) = s.member(&m.user.id) else {
            return false;
        };
        if logged.role != m.role {
            return false;
        }
        match (logged.public(), &m.user.account_key) {
            (Ok(None), None) => true,
            (Ok(Some(pinned)), Some(k)) => k.verify().is_ok_and(|k| k == pinned),
            _ => false,
        }
    }

    /// A member the log lists, in this role, with a different account key
    /// than they show now: they reset their account — or the server swaps
    /// keys. Their new key is pinned (`Rekey`) only once the owner verified it.
    pub fn key_changed(&self, m: &MemberInfo) -> bool {
        let Some(logged) = self.state.as_ref().and_then(|s| s.member(&m.user.id)) else {
            return false;
        };
        logged.role == m.role
            && matches!(
                (logged.public(), &m.user.account_key),
                (Ok(Some(pinned)), Some(k)) if k.verify().is_ok_and(|k| k != pinned)
            )
    }

    /// Members the log doesn't vouch for (to warn about; never give them keys).
    pub fn unvouched<'a>(&self, members: &'a [MemberInfo]) -> Vec<&'a MemberInfo> {
        members.iter().filter(|m| !self.vouches_for(m)).collect()
    }

    /// Members that now have account keys while the log has them as pending:
    /// the owner pins their keys (`Rekey`) before giving them the vault key.
    pub fn to_pin<'a>(&self, members: &'a [MemberInfo]) -> Vec<&'a MemberInfo> {
        let Some(s) = &self.state else {
            return vec![];
        };
        members
            .iter()
            .filter(|m| {
                m.user.account_key.is_some()
                    && s.member(&m.user.id)
                        .is_some_and(|l| l.role == m.role && matches!(l.public(), Ok(None)))
            })
            .collect()
    }

    /// Who may author content (for `VaultReplica::authors`).
    pub fn authors(&self) -> Option<Authors> {
        self.state.as_ref().map(|s| s.authors())
    }

    /// Whether the owner still has to require signed content (the log has
    /// started, but without `require_signed` yet).
    pub fn needs_signatures(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.signed_from.is_none())
    }

    /// Whether the log has `user_id` on `object_id` with this role and key.
    pub fn has_share(
        &self,
        object_id: &str,
        user_id: &str,
        role: &Role,
        key: &AccountPublic,
    ) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.shares
                .get(&(object_id.to_string(), user_id.to_string()))
                .is_some_and(|m| m.role == *role && m.public().ok().flatten().as_ref() == Some(key))
        })
    }

    /// Whether the server's recipient of a note is who the log says: the
    /// note shared with them in this role, with this key. Only they get its
    /// keys. Without a log, everyone is taken as listed (vaults from before
    /// the log).
    pub fn vouches_for_share(&self, object_id: &str, s: &ObjectShareInfo) -> bool {
        if self.state.is_none() {
            return true;
        }
        let Some(Ok(key)) = s.user.account_key.as_ref().map(|k| k.verify()) else {
            return false;
        };
        self.has_share(object_id, &s.user.id, &s.role, &key)
    }

    /// The role the log has `object_id` shared with `user_id` in (`None`:
    /// not shared with them; without a log, `listed`, the server's word).
    pub fn share_role(&self, object_id: &str, user_id: &str, listed: &Role) -> Option<Role> {
        let Some(s) = &self.state else {
            return Some(listed.clone());
        };
        s.shares
            .get(&(object_id.to_string(), user_id.to_string()))
            .map(|m| m.role.clone())
    }

    /// Check every vault key we hold against the owner's commitments.
    /// Epochs from before a checkpoint have none and pass.
    pub fn check_keyring(&self, ring: &VaultKeyring) -> Result<(), KeyCheckError> {
        let Some(s) = &self.state else {
            return Ok(());
        };
        for epoch in ring.epochs() {
            if let Some(k) = ring.get(epoch) {
                s.check_key(epoch, k)?;
            }
        }
        Ok(())
    }
}

/// The member entry for someone being added (keys pinned if they have any).
pub fn log_member(m_user_id: &str, role: Role, key: Option<&AccountPublic>) -> LogMember {
    match key {
        Some(k) => LogMember::new(m_user_id, role, k),
        None => LogMember::pending(m_user_id, role),
    }
}

/// The checkpoint that starts the log of an existing vault: the members as
/// the owner's client sees them (keys already checked against pins by the
/// caller), and the current epoch's key commitment.
pub fn checkpoint(
    vault_id: &str,
    ring: &VaultKeyring,
    members: &[(String, Role, Option<AccountPublic>)],
) -> Option<LogBody> {
    let (epoch, key) = ring.current()?;
    Some(LogBody::Checkpoint {
        members: members
            .iter()
            .map(|(id, role, key)| log_member(id, role.clone(), key.as_ref()))
            .collect(),
        epoch,
        key_commit: key_commitment(vault_id, epoch, key),
    })
}

/// Sign every update of planned pushes as their author.
pub fn sign_pushes(account: &AccountSecret, vault_id: &str, pushes: &mut [Vec<PushUpdate>]) {
    for u in pushes.iter_mut().flatten() {
        sign_push(account, vault_id, u);
    }
}

pub fn sign_push(account: &AccountSecret, vault_id: &str, u: &mut PushUpdate) {
    u.signature = Some(memberlog::sign_update(
        account,
        vault_id,
        &u.object_id,
        &u.payload,
    ));
}

/// Sign a snapshot as its author.
pub fn sign_snapshot(
    account: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    req: &mut CreateSnapshotRequest,
) {
    req.signature = Some(memberlog::sign_snapshot(
        account,
        vault_id,
        object_id,
        req.covers_seq,
        &req.payload,
    ));
}

/// Sign the snapshot a note key change carries, if any.
pub fn sign_key_update(
    account: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    u: &mut ObjectKeyUpdate,
) {
    if let Some(s) = &mut u.snapshot {
        sign_snapshot(account, vault_id, object_id, s);
    }
}

/// Owner: sign giving each of `envelopes` its note key (taken from `ring`,
/// the note's keyring), so the people it is shared with can check it.
pub fn sign_grants(
    owner: &AccountSecret,
    vault_id: &str,
    object_id: &str,
    ring: &VaultKeyring,
    envelopes: &mut [KeyEnvelope],
) {
    for e in envelopes {
        if let Some(k) = ring.get(e.epoch) {
            e.signature = Some(memberlog::sign_note_grant(
                owner, vault_id, object_id, &e.user_id, e.epoch, k,
            ));
        }
    }
}

/// Owner: grant signatures for the people a note is shared with whose
/// current key carries none yet (shared before grants were signed), or one
/// by someone `log` no longer has as an owner (their apps take a key only
/// from an owner), as far as `log` has it shared with them.
pub fn missing_grant_signatures(
    owner: &AccountSecret,
    log: &MemberLog,
    vault_id: &str,
    object_id: &str,
    ring: &VaultKeyring,
    shares: &[ObjectShareInfo],
) -> Vec<GrantSignature> {
    let Some((epoch, key)) = ring.current() else {
        return vec![];
    };
    shares
        .iter()
        .filter(|s| {
            let by_former_owner = match (&log.state, &s.signed_by) {
                (Some(st), Some(by)) => !st.is_owner(by),
                _ => false,
            };
            s.has_current_key && (!s.key_signed || by_former_owner)
        })
        .filter(|s| log.vouches_for_share(object_id, s))
        .map(|s| GrantSignature {
            user_id: s.user.id.clone(),
            epoch,
            signature: memberlog::sign_note_grant(
                owner, vault_id, object_id, &s.user.id, epoch, key,
            ),
        })
        .collect()
}

/// The entry for sharing a note with someone (their key pinned).
pub fn share(object_id: &str, user_id: &str, role: Role, key: &AccountPublic) -> LogBody {
    LogBody::Share {
        object_id: object_id.to_string(),
        member: LogMember::new(user_id, role, key),
    }
}

/// The entry for moving to the newest epoch of `ring` (after `rotate`).
pub fn rotation(vault_id: &str, ring: &VaultKeyring) -> Option<LogBody> {
    let (epoch, key) = ring.current()?;
    Some(LogBody::Rotate {
        epoch,
        key_commit: key_commitment(vault_id, epoch, key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notekpr_core::keys::VaultKey;
    use notekpr_core::protocol::{PrincipalKind, PublicAccountKey, PublicUser};

    struct P {
        id: String,
        secret: AccountSecret,
    }

    fn p(id: &str) -> P {
        P {
            id: id.into(),
            secret: AccountSecret::generate(),
        }
    }

    fn info(p: &P, role: Role, with_key: bool) -> MemberInfo {
        let public = p.secret.public();
        MemberInfo {
            user: PublicUser {
                id: p.id.clone(),
                display_name: p.id.clone(),
                email: None,
                kind: PrincipalKind::Human,
                account_key: with_key.then(|| {
                    PublicAccountKey::new(&p.id, 1, &public, &p.secret.sign_account_key(&p.id, 1))
                }),
            },
            role,
            has_current_key: false,
            added_at: 0,
            added_by: None,
        }
    }

    fn vault(owner: &P) -> (MemberLog, VaultKeyring) {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let body = LogBody::Create {
            owner: LogMember::new(&owner.id, Role::Owner, &owner.secret.public()),
            key_commit: key_commitment("v", 1, ring.get(1).unwrap()),
        };
        let first = MemberLog::default()
            .next(&owner.secret, &owner.id, "v", body, 1)
            .unwrap();
        let log = MemberLog::verify("v", &owner.secret.public(), vec![first], None).unwrap();
        (log, ring)
    }

    fn push(log: &mut MemberLog, by: &P, body: LogBody, anchor: &AccountPublic) {
        let e = log.next(&by.secret, &by.id, "v", body, 2).unwrap();
        let mut entries = log.entries.clone();
        entries.push(e);
        *log = MemberLog::verify(
            "v",
            anchor,
            entries,
            log.head().as_ref().map(|(s, h)| (*s, h.as_slice())),
        )
        .unwrap();
    }

    #[test]
    fn only_members_the_owner_vouched_for_are_trusted() {
        let (alice, bob, eve) = (p("alice"), p("bob"), p("eve"));
        let anchor = alice.secret.public();
        let (mut log, _) = vault(&alice);
        push(
            &mut log,
            &alice,
            LogBody::Add {
                member: log_member("bob", Role::Editor, Some(&bob.secret.public())),
            },
            &anchor,
        );
        assert!(log.vouches_for(&info(&bob, Role::Editor, true)));
        assert!(
            !log.vouches_for(&info(&bob, Role::Viewer, true)),
            "other role"
        );
        assert!(
            !log.vouches_for(&info(&eve, Role::Editor, true)),
            "never added"
        );
        // The server swaps bob's key.
        let mut fake = info(&bob, Role::Editor, true);
        fake.user.account_key = info(&eve, Role::Editor, true)
            .user
            .account_key
            .map(|mut k| {
                k.user_id = "bob".into();
                k
            });
        assert!(!log.vouches_for(&fake));
        // A changed key (Bob reset his account) is told apart from someone
        // never added; an invalid one is neither.
        assert!(!log.key_changed(&fake));
        let reset = info(&p("bob"), Role::Editor, true);
        assert!(!log.vouches_for(&reset) && log.key_changed(&reset));
        assert!(!log.key_changed(&info(&bob, Role::Editor, true)));
        assert!(!log.key_changed(&info(&eve, Role::Editor, true)));
        let all = [
            info(&alice, Role::Owner, true),
            info(&bob, Role::Editor, true),
            info(&eve, Role::Editor, true),
        ];
        let bad: Vec<&str> = log
            .unvouched(&all)
            .iter()
            .map(|m| m.user.id.as_str())
            .collect();
        assert_eq!(bad, vec!["eve"]);
        // Without a log (older vaults) the list is taken as is.
        assert!(MemberLog::default().vouches_for(&info(&eve, Role::Editor, true)));
    }

    #[test]
    fn pending_members_get_pinned_before_they_get_keys() {
        let (alice, bob) = (p("alice"), p("bob"));
        let anchor = alice.secret.public();
        let (mut log, _) = vault(&alice);
        push(
            &mut log,
            &alice,
            LogBody::Add {
                member: log_member("bob", Role::Viewer, None),
            },
            &anchor,
        );
        assert!(log.vouches_for(&info(&bob, Role::Viewer, false)));
        let now_with_keys = [info(&bob, Role::Viewer, true)];
        assert!(!log.vouches_for(&now_with_keys[0]));
        assert_eq!(log.to_pin(&now_with_keys).len(), 1);
        push(
            &mut log,
            &alice,
            LogBody::Rekey {
                member: log_member("bob", Role::Viewer, Some(&bob.secret.public())),
            },
            &anchor,
        );
        assert!(log.vouches_for(&now_with_keys[0]));
        assert!(log.to_pin(&now_with_keys).is_empty());
    }

    #[test]
    fn keys_the_owner_never_committed_to_are_refused() {
        let alice = p("alice");
        let anchor = alice.secret.public();
        let (mut log, mut ring) = vault(&alice);
        assert_eq!(log.check_keyring(&ring), Ok(()));
        ring.insert(2, VaultKey::generate());
        assert_eq!(log.check_keyring(&ring), Err(KeyCheckError::Unannounced(2)));
        push(&mut log, &alice, rotation("v", &ring).unwrap(), &anchor);
        assert_eq!(log.check_keyring(&ring), Ok(()));
        let mut injected = VaultKeyring::new("v");
        injected.insert(1, ring.get(1).unwrap().clone());
        injected.insert(2, VaultKey::generate());
        assert_eq!(
            log.check_keyring(&injected),
            Err(KeyCheckError::Mismatch(2))
        );
    }

    #[test]
    fn a_rolled_back_or_rewritten_log_is_noticed() {
        let (alice, bob) = (p("alice"), p("bob"));
        let anchor = alice.secret.public();
        let (mut log, _) = vault(&alice);
        push(
            &mut log,
            &alice,
            LogBody::Add {
                member: log_member("bob", Role::Editor, Some(&bob.secret.public())),
            },
            &anchor,
        );
        let (seq, hash) = log.head().unwrap();
        // Served only the first entry again (rolled back), or nothing.
        let older = log.entries[..1].to_vec();
        assert!(MemberLog::verify("v", &anchor, older, Some((seq, &hash))).is_err());
        assert!(MemberLog::verify("v", &anchor, vec![], Some((seq, &hash))).is_err());
        assert!(MemberLog::verify("v", &anchor, log.entries.clone(), Some((seq, &hash))).is_ok());
    }

    /// A note's keys go only to whom the log has it shared with, in that role
    /// and with that key: never to someone the server lists on its own.
    #[test]
    fn only_logged_recipients_get_a_notes_keys() {
        let (alice, bob, eve) = (p("alice"), p("bob"), p("eve"));
        let anchor = alice.secret.public();
        let share_of = |who: &P, role: Role| ObjectShareInfo {
            user: info(who, role.clone(), true).user,
            role,
            added_at: 0,
            added_by: None,
            has_current_key: false,
            key_signed: false,
            signed_by: None,
        };
        // Before the log, the server's word (vaults from before it).
        assert!(MemberLog::default().vouches_for_share("n", &share_of(&eve, Role::Viewer)));

        let (mut log, _) = vault(&alice);
        push(
            &mut log,
            &alice,
            share("n", "bob", Role::Viewer, &bob.secret.public()),
            &anchor,
        );
        assert!(log.vouches_for_share("n", &share_of(&bob, Role::Viewer)));
        // Not as an editor, not another note, not Eve.
        assert!(!log.vouches_for_share("n", &share_of(&bob, Role::Editor)));
        assert!(!log.vouches_for_share("m", &share_of(&bob, Role::Viewer)));
        assert!(!log.vouches_for_share("n", &share_of(&eve, Role::Viewer)));
        // Nor Bob with a key the log never pinned for him.
        let swapped = P {
            id: "bob".into(),
            secret: AccountSecret::generate(),
        };
        assert!(!log.vouches_for_share("n", &share_of(&swapped, Role::Viewer)));
        // Given again: in the role the log has, whatever the server says.
        assert_eq!(
            log.share_role("n", "bob", &Role::Editor),
            Some(Role::Viewer)
        );
        assert_eq!(log.share_role("n", "eve", &Role::Viewer), None);
    }

    /// The people a note is shared with take its key only from an owner:
    /// one signed by someone who no longer is gets signed again, as does one
    /// never signed; one an owner signed doesn't.
    #[test]
    fn a_key_signed_by_a_former_owner_is_signed_again() {
        let (alice, bob, carol) = (p("alice"), p("bob"), p("carol"));
        let anchor = alice.secret.public();
        let (mut log, _) = vault(&alice);
        for body in [
            LogBody::Add {
                member: log_member("bob", Role::Editor, Some(&bob.secret.public())),
            },
            LogBody::MakeOwner {
                user_id: "bob".into(),
            },
            share("n", "carol", Role::Viewer, &carol.secret.public()),
        ] {
            push(&mut log, &alice, body, &anchor);
        }
        let mut ring = VaultKeyring::new("v");
        ring.insert(0x8000_0001, VaultKey::generate());
        let carols = |signed_by: Option<&str>| ObjectShareInfo {
            user: info(&carol, Role::Viewer, true).user,
            role: Role::Viewer,
            added_at: 0,
            added_by: None,
            has_current_key: true,
            key_signed: signed_by.is_some(),
            signed_by: signed_by.map(str::to_string),
        };
        let missing = |log: &MemberLog, signed_by| {
            missing_grant_signatures(&alice.secret, log, "v", "n", &ring, &[carols(signed_by)])
        };
        assert!(missing(&log, Some("bob")).is_empty());
        assert!(missing(&log, Some("alice")).is_empty());
        assert_eq!(missing(&log, None).len(), 1);

        push(
            &mut log,
            &alice,
            LogBody::Role {
                user_id: "bob".into(),
                role: Role::Editor,
            },
            &anchor,
        );
        let again = missing(&log, Some("bob"));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].user_id, "carol");
        assert!(missing(&log, Some("alice")).is_empty());
    }

    /// A synced copy of the settings, even one saved as the newest, never
    /// makes a device forget or change whom it trusts, nor take a log head
    /// back.
    #[test]
    fn a_synced_copy_never_drops_what_this_device_trusts() {
        let map = |kv: &[(&str, &str)]| -> BTreeMap<String, String> {
            kv.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let mine = map(&[("bob", "fp-bob"), ("carol", "fp-carol")]);
        let mut theirs = map(&[("carol", "fp-other"), ("dave", "fp-dave")]);
        assert!(keep_known(&mut theirs, &mine));
        assert_eq!(
            theirs,
            map(&[
                ("bob", "fp-bob"),
                ("carol", "fp-carol"),
                ("dave", "fp-dave")
            ])
        );
        // Only a different one: ours is kept, but nothing lacked (nothing
        // to save again).
        let mut theirs = map(&[("bob", "fp-bob"), ("carol", "fp-other")]);
        assert!(!keep_known(&mut theirs, &mine));
        assert_eq!(theirs, mine);

        let head = |seq, h: u8| LogHead {
            seq,
            hash: vec![h; 32],
        };
        let mine: LogHeads = [("v".to_string(), head(4, 1)), ("w".to_string(), head(2, 1))].into();
        let mut theirs: LogHeads = [
            ("v".to_string(), head(3, 9)),
            ("w".to_string(), head(2, 9)),
            ("x".to_string(), head(1, 1)),
        ]
        .into();
        assert!(keep_heads(&mut theirs, &mine));
        assert_eq!(theirs["v"], head(4, 1), "never taken back");
        assert_eq!(theirs["w"], head(2, 1), "at the same position, ours");
        assert_eq!(theirs["x"], head(1, 1));
        let mut further: LogHeads = [("v".to_string(), head(5, 2))].into();
        keep_heads(&mut further, &mine);
        assert_eq!(further["v"], head(5, 2), "a further one stays");
        assert_eq!(further["w"], head(2, 1));
    }

    /// A device that never saw the vault knows where its log stood from the
    /// synced heads: shown an older log, or none at all (a vault "from
    /// before the log"), it refuses it.
    #[test]
    fn a_synced_head_holds_on_a_new_device() {
        let (alice, bob) = (p("alice"), p("bob"));
        let anchor = alice.secret.public();
        let (mut log, _) = vault(&alice);
        let mut heads = LogHeads::new();
        assert!(note_head(&mut heads, "v", log.head()));
        push(
            &mut log,
            &alice,
            LogBody::Add {
                member: log_member("bob", Role::Editor, Some(&bob.secret.public())),
            },
            &anchor,
        );
        assert!(note_head(&mut heads, "v", log.head()));
        // An older head never replaces a further one.
        let first = log.entries[0].hash().to_vec();
        assert!(!note_head(&mut heads, "v", Some((0, first.clone()))));
        let head = heads["v"].clone();
        assert_eq!(head.seq, 1);

        let fresh = |entries| MemberLog::verify("v", &anchor, entries, None).unwrap();
        assert_eq!(fresh(log.entries.clone()).check_head(&head), Ok(()));
        assert_eq!(
            fresh(log.entries[..1].to_vec()).check_head(&head),
            Err(LogError::Fork(1))
        );
        assert_eq!(fresh(vec![]).check_head(&head), Err(LogError::Fork(1)));
        // Another history with the same length doesn't pass either.
        let (other, _) = vault(&alice);
        let mut other = other;
        push(
            &mut other,
            &alice,
            LogBody::Add {
                member: log_member("eve", Role::Editor, Some(&p("eve").secret.public())),
            },
            &anchor,
        );
        assert_eq!(
            fresh(other.entries).check_head(&head),
            Err(LogError::Fork(1))
        );

        // Saved on two devices: the further head of each vault stays.
        let mut theirs = LogHeads::new();
        note_head(&mut theirs, "v", Some((0, first)));
        note_head(&mut theirs, "w", Some((3, vec![7; 32])));
        merge_heads(&mut theirs, &heads);
        assert_eq!(theirs["v"], head);
        assert_eq!(theirs["w"].seq, 3);
    }

    #[test]
    fn a_log_starts_once() {
        let alice = p("alice");
        let (log, ring) = vault(&alice);
        let cp = checkpoint(
            "v",
            &ring,
            &[("alice".into(), Role::Owner, Some(alice.secret.public()))],
        )
        .unwrap();
        assert!(
            log.next(&alice.secret, "alice", "v", cp.clone(), 3)
                .is_none()
        );
        assert!(
            MemberLog::default()
                .next(&alice.secret, "alice", "v", cp, 3)
                .is_some()
        );
        assert!(
            MemberLog::default()
                .next(
                    &alice.secret,
                    "alice",
                    "v",
                    LogBody::Remove {
                        user_id: "x".into()
                    },
                    3
                )
                .is_none()
        );
    }

    #[test]
    fn a_removal_and_the_new_key_are_signed_one_after_the_other() {
        let (alice, bob) = (p("alice"), p("bob"));
        let anchor = alice.secret.public();
        let (mut log, mut ring) = vault(&alice);
        push(
            &mut log,
            &alice,
            LogBody::Add {
                member: log_member("bob", Role::Editor, Some(&bob.secret.public())),
            },
            &anchor,
        );
        ring.insert(2, VaultKey::generate());
        let remove = log.next(
            &alice.secret,
            "alice",
            "v",
            LogBody::Remove {
                user_id: "bob".into(),
            },
            3,
        );
        let rotate = log
            .next_after(
                &alice.secret,
                "alice",
                "v",
                remove.as_ref(),
                rotation("v", &ring).unwrap(),
                3,
            )
            .unwrap();
        let mut entries = log.entries.clone();
        entries.extend(remove);
        entries.push(rotate);
        let after = MemberLog::verify("v", &anchor, entries, None).unwrap();
        assert!(!after.vouches_for(&info(&bob, Role::Editor, true)));
        assert_eq!(after.check_keyring(&ring), Ok(()));
        // Without a log, neither is signed.
        let none = MemberLog::default();
        assert!(
            none.next_after(
                &alice.secret,
                "alice",
                "v",
                None,
                rotation("v", &ring).unwrap(),
                3
            )
            .is_none()
        );
    }
}
