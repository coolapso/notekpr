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

use coolpsonotes_core::keys::{AccountPublic, AccountSecret, VaultKeyring};
use coolpsonotes_core::memberlog::{
    self, Authors, KeyCheckError, LogBody, LogEntry, LogError, LogMember, LogState, key_commitment,
};
use coolpsonotes_core::protocol::{
    CreateSnapshotRequest, MemberInfo, ObjectKeyUpdate, PushUpdate, Role,
};

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
        role: Role,
        key: &AccountPublic,
    ) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.shares
                .get(&(object_id.to_string(), user_id.to_string()))
                .is_some_and(|m| m.role == role && m.public().ok().flatten().as_ref() == Some(key))
        })
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
            .map(|(id, role, key)| log_member(id, *role, key.as_ref()))
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
    use coolpsonotes_core::keys::VaultKey;
    use coolpsonotes_core::protocol::{PrincipalKind, PublicAccountKey, PublicUser};

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
}
