//! Invitations to a vault or a single note (docs/protocol.md §5.6).
//!
//! Sharing with someone is an invitation: they accept it, then an owner's
//! app adds them — wraps the vault's (or the note's) key to their account
//! key and signs them into the membership log. The server keeps the
//! invitations, but it can't make one, nor accept one for somebody:
//!
//! * the owner who invites signs it ([`sign`]): the vault, the note, whom
//!   (an account, or an e-mail address as typed), the role and when;
//! * the person invited signs their acceptance ([`sign_acceptance`]) with
//!   their account key — the key the owner's app then wraps to.
//!
//! An owner's app adds someone only after [`check_accepted`]: the
//! invitation signed by someone the vault's verified log has as an owner,
//! with the key it pinned for them, not older than [`MAX_AGE_MS`], accepted
//! by that very account (one invited by account) with a key that verifies
//! and that the app hasn't pinned differently.
//!
//! The signed bytes are `crypto::statement`s, never JSON.

use crate::crypto;
use crate::keys::{AccountPublic, AccountSecret};
use crate::memberlog::LogState;
use crate::protocol::{PublicUser, ReceivedInvitation, Role, SentInvitation};

const INVITE_DOMAIN: &str = "notekpr/v1/sig/invitation";
const ACCEPT_DOMAIN: &str = "notekpr/v1/sig/invitation-accept";

const DAY_MS: i64 = 24 * 3600 * 1000;

/// The most days an instance may let an invitation wait.
pub const MAX_DAYS: u32 = 90;

/// The oldest invitation an owner's app still completes: whatever the
/// server says of its expiry, it can't bring back one cancelled long ago.
pub const MAX_AGE_MS: i64 = MAX_DAYS as i64 * DAY_MS;

/// How far an invitation's time may be from the server's (clocks differ).
pub const CLOCK_SKEW_MS: i64 = DAY_MS;

/// Whom an invitation is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invitee<'a> {
    /// An account the inviter knows (they share with them already, or found
    /// them in the directory).
    User(&'a str),
    /// Whoever has this address, as [`normalize_email`] makes it.
    Email(&'a str),
}

impl Invitee<'_> {
    fn parts(&self) -> [&[u8]; 2] {
        match self {
            Invitee::User(id) => [b"user", id.as_bytes()],
            Invitee::Email(a) => [b"email", a.as_bytes()],
        }
    }
}

/// An e-mail address as invitations keep it: trimmed and lowercased, or
/// `None` if it isn't one (one `@`, something on both sides, no spaces, at
/// most 254 characters).
pub fn normalize_email(s: &str) -> Option<String> {
    let a = s.trim().to_lowercase();
    let (local, domain) = a.split_once('@')?;
    let ok = !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && a.len() <= 254
        && !a.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then_some(a)
}

/// What an invitation says, as its inviter signs it.
#[derive(Clone, Copy, Debug)]
pub struct Terms<'a> {
    pub id: &'a str,
    pub vault_id: &'a str,
    /// A single note; `None`: the vault.
    pub object_id: Option<&'a str>,
    pub invited_by: &'a str,
    pub invitee: Invitee<'a>,
    pub role: &'a Role,
    /// When the inviter's app made it (ms).
    pub created_at: i64,
    /// A note's comments so far go with it, to everyone it is shared with
    /// (docs/comments.md §5): the owner's app that adds them carries them.
    pub with_comments: bool,
}

impl Terms<'_> {
    fn parts(&self) -> Vec<&[u8]> {
        let [kind, whom] = self.invitee.parts();
        let (what, object): (&[u8], &[u8]) = match self.object_id {
            Some(o) => (b"note", o.as_bytes()),
            None => (b"vault", b""),
        };
        let comments: &[u8] = if self.with_comments {
            b"with comments"
        } else {
            b""
        };
        vec![
            self.id.as_bytes(),
            self.vault_id.as_bytes(),
            what,
            object,
            self.invited_by.as_bytes(),
            kind,
            whom,
            self.role.as_str().as_bytes(),
            comments,
        ]
    }

    fn statement(&self) -> Vec<u8> {
        let at = self.created_at.to_be_bytes();
        let mut parts = self.parts();
        parts.push(&at);
        crypto::statement(INVITE_DOMAIN, &parts)
    }

    fn acceptance(&self, user_id: &str) -> Vec<u8> {
        let at = self.created_at.to_be_bytes();
        let mut parts = self.parts();
        parts.push(&at);
        parts.push(user_id.as_bytes());
        crypto::statement(ACCEPT_DOMAIN, &parts)
    }
}

/// The inviting owner's signature.
pub fn sign(inviter: &AccountSecret, terms: &Terms) -> Vec<u8> {
    inviter.sign(&terms.statement()).to_vec()
}

pub fn verify(inviter: &AccountPublic, terms: &Terms, signature: &[u8]) -> bool {
    crypto::ed25519_verify(&inviter.sign_public, &terms.statement(), signature).is_ok()
}

/// The invited person's signature on accepting, as `user_id`.
pub fn sign_acceptance(account: &AccountSecret, terms: &Terms, user_id: &str) -> Vec<u8> {
    account.sign(&terms.acceptance(user_id)).to_vec()
}

pub fn verify_acceptance(
    account: &AccountPublic,
    terms: &Terms,
    user_id: &str,
    signature: &[u8],
) -> bool {
    crypto::ed25519_verify(&account.sign_public, &terms.acceptance(user_id), signature).is_ok()
}

impl SentInvitation {
    /// Its terms, as signed. `None` if it names nobody (or both kinds).
    pub fn terms(&self) -> Option<Terms<'_>> {
        Some(Terms {
            id: &self.id,
            vault_id: &self.vault_id,
            object_id: self.object_id.as_deref(),
            invited_by: &self.invited_by,
            invitee: invitee(
                self.user.as_ref().map(|u| u.id.as_str()),
                self.email.as_deref(),
            )?,
            role: &self.role,
            created_at: self.created_at,
            with_comments: self.with_comments,
        })
    }
}

impl ReceivedInvitation {
    /// Its terms, as signed. `None` if it names nobody (or both kinds).
    pub fn terms(&self) -> Option<Terms<'_>> {
        Some(Terms {
            id: &self.id,
            vault_id: &self.vault_id,
            object_id: self.object_id.as_deref(),
            invited_by: &self.from.id,
            invitee: invitee(self.user_id.as_deref(), self.email.as_deref())?,
            role: &self.role,
            created_at: self.created_at,
            with_comments: self.with_comments,
        })
    }

    /// The inviter's key, if their invitation verifies with it (their
    /// self-signed account key). Apps also check it against the key they
    /// pinned for them, if any.
    pub fn verified_inviter(&self) -> Option<AccountPublic> {
        let key = self.from.account_key.as_ref()?;
        if key.user_id != self.from.id {
            return None;
        }
        let key = key.verify().ok()?;
        verify(&key, &self.terms()?, &self.signature).then_some(key)
    }
}

fn invitee<'a>(user: Option<&'a str>, email: Option<&'a str>) -> Option<Invitee<'a>> {
    match (user, email) {
        (Some(u), None) => Some(Invitee::User(u)),
        (None, Some(e)) => Some(Invitee::Email(e)),
        _ => None,
    }
}

/// Why an owner's app doesn't add someone who accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotAdded {
    /// Not accepted (yet).
    NotAccepted,
    /// For another vault than the log's.
    OtherVault,
    /// Names nobody, or both an account and an address.
    Malformed,
    /// Its inviter isn't an owner in the log (any more), or has no keys there.
    NotAnOwner,
    /// Its signature doesn't verify with the key the log pinned for its
    /// inviter.
    BadSignature,
    /// Older than [`MAX_AGE_MS`], or from the future.
    TooOld,
    /// Accepted by someone else than the account it invited.
    WrongPerson,
    /// The account that accepted has no key, or one that doesn't verify.
    BadKey,
    /// Their acceptance doesn't verify with their key.
    BadAcceptance,
    /// Their key isn't the one the app pinned for them: the owner compares
    /// fingerprints with them first.
    KeyChanged,
    /// A role invitations can't give (owners are made from members; notes
    /// are shared as editor or viewer), or one this release doesn't know.
    BadRole,
}

impl std::fmt::Display for NotAdded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NotAdded::NotAccepted => "not accepted yet",
            NotAdded::OtherVault => "for another vault",
            NotAdded::Malformed => "malformed",
            NotAdded::NotAnOwner => "its inviter is no owner of the vault",
            NotAdded::BadSignature => "its signature doesn't verify",
            NotAdded::TooOld => "too old",
            NotAdded::WrongPerson => "accepted by someone it wasn't for",
            NotAdded::BadKey => "the account key of who accepted doesn't verify",
            NotAdded::BadAcceptance => "the acceptance isn't signed by who accepted",
            NotAdded::KeyChanged => "their key changed since it was pinned",
            NotAdded::BadRole => "a role invitations don't give",
        })
    }
}

/// May an owner's app add whoever accepted `inv`, on the vault's log as
/// verified just now, at `now` (ms)? `pinned`: the fingerprint the app
/// pinned for the person who accepted, if any. Gives who accepted and the
/// key to wrap to (to pin, if nothing was).
pub fn check_accepted<'a>(
    log: &LogState,
    inv: &'a SentInvitation,
    now: i64,
    pinned: Option<&str>,
) -> Result<(&'a PublicUser, AccountPublic), NotAdded> {
    let accepted = inv.accepted.as_ref().ok_or(NotAdded::NotAccepted)?;
    if inv.vault_id != log.vault_id {
        return Err(NotAdded::OtherVault);
    }
    let terms = inv.terms().ok_or(NotAdded::Malformed)?;
    let role_ok = match inv.object_id {
        None => matches!(inv.role, Role::Admin | Role::Editor | Role::Viewer),
        Some(_) => matches!(inv.role, Role::Editor | Role::Viewer),
    };
    if !role_ok {
        return Err(NotAdded::BadRole);
    }
    let inviter = log
        .member(&inv.invited_by)
        .filter(|m| m.role == Role::Owner)
        .and_then(|m| m.public().ok().flatten())
        .ok_or(NotAdded::NotAnOwner)?;
    if !verify(&inviter, &terms, &inv.signature) {
        return Err(NotAdded::BadSignature);
    }
    if inv.created_at > now + CLOCK_SKEW_MS || now - inv.created_at > MAX_AGE_MS {
        return Err(NotAdded::TooOld);
    }
    let user = &accepted.user;
    if let Invitee::User(id) = terms.invitee
        && id != user.id
    {
        return Err(NotAdded::WrongPerson);
    }
    let key = user
        .account_key
        .as_ref()
        .filter(|k| k.user_id == user.id)
        .ok_or(NotAdded::BadKey)?
        .verify()
        .map_err(|_| NotAdded::BadKey)?;
    if !verify_acceptance(&key, &terms, &user.id, &accepted.signature) {
        return Err(NotAdded::BadAcceptance);
    }
    if pinned.is_some_and(|p| p != key.fingerprint()) {
        return Err(NotAdded::KeyChanged);
    }
    Ok((user, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memberlog::{LogBody, LogEntry, LogMember};
    use crate::protocol::{InvitationAcceptance, PrincipalKind, PublicAccountKey};

    struct Person {
        id: &'static str,
        secret: AccountSecret,
    }

    impl Person {
        fn new(id: &'static str) -> Person {
            Person {
                id,
                secret: AccountSecret::generate(),
            }
        }

        fn user(&self) -> PublicUser {
            let public = self.secret.public();
            PublicUser {
                id: self.id.into(),
                display_name: self.id.into(),
                email: Some(format!("{}@example.com", self.id)),
                kind: PrincipalKind::Human,
                account_key: Some(PublicAccountKey::new(
                    self.id,
                    1,
                    &public,
                    &self.secret.sign_account_key(self.id, 1),
                )),
            }
        }
    }

    const NOW: i64 = 1_800_000_000_000;

    /// A vault created by `owner`, with `others` added as owners.
    fn log(owner: &Person, others: &[&Person]) -> LogState {
        LogState::verify("v1", &owner.secret.public(), &entries(owner, others)).unwrap()
    }

    fn entries(owner: &Person, others: &[&Person]) -> Vec<LogEntry> {
        let first = LogEntry::sign(
            &owner.secret,
            owner.id,
            "v1",
            None,
            LogBody::Create {
                owner: LogMember::new(owner.id, Role::Owner, &owner.secret.public()),
                key_commit: vec![1; 32],
            },
            NOW,
        );
        let mut entries = vec![first];
        for p in others {
            for body in [
                LogBody::Add {
                    member: LogMember::new(p.id, Role::Editor, &p.secret.public()),
                },
                LogBody::MakeOwner {
                    user_id: p.id.into(),
                },
            ] {
                let e = LogEntry::sign(&owner.secret, owner.id, "v1", entries.last(), body, NOW);
                entries.push(e);
            }
        }
        entries
    }

    fn invite(by: &Person, to: Invitee, role: Role, object: Option<&str>) -> SentInvitation {
        let (user, email) = match to {
            Invitee::User(id) => (
                Some(PublicUser {
                    id: id.into(),
                    display_name: id.into(),
                    email: None,
                    kind: PrincipalKind::Human,
                    account_key: None,
                }),
                None,
            ),
            Invitee::Email(a) => (None, Some(a.to_string())),
        };
        let mut inv = SentInvitation {
            id: "i1".into(),
            vault_id: "v1".into(),
            object_id: object.map(Into::into),
            user,
            email,
            role,
            invited_by: by.id.into(),
            created_at: NOW,
            expires_at: NOW + 14 * DAY_MS,
            signature: vec![],
            accepted: None,
            with_comments: false,
        };
        inv.signature = sign(&by.secret, &inv.terms().unwrap());
        inv
    }

    fn accept(inv: &mut SentInvitation, who: &Person) {
        let signature = sign_acceptance(&who.secret, &inv.terms().unwrap(), who.id);
        inv.accepted = Some(InvitationAcceptance {
            user: who.user(),
            accepted_at: NOW,
            signature,
        });
    }

    #[test]
    fn addresses_are_kept_lowercased_and_checked() {
        assert_eq!(
            normalize_email("  Bob@Example.COM "),
            Some("bob@example.com".into())
        );
        for bad in [
            "",
            "bob",
            "@example.com",
            "bob@",
            "b ob@x.y",
            "a@b@c",
            "bob\n@x.y",
        ] {
            assert_eq!(normalize_email(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_accepted_invitation_from_an_owner_is_added() {
        let (alice, bob) = (Person::new("alice"), Person::new("bob"));
        let log = log(&alice, &[]);
        for to in [Invitee::Email("bob@example.com"), Invitee::User("bob")] {
            let mut inv = invite(&alice, to, Role::Editor, None);
            assert_eq!(
                check_accepted(&log, &inv, NOW, None).unwrap_err(),
                NotAdded::NotAccepted
            );
            accept(&mut inv, &bob);
            let (user, key) = check_accepted(&log, &inv, NOW, None).unwrap();
            assert_eq!(user.id, "bob");
            assert_eq!(key, bob.secret.public());
            // Pinned already: the same key.
            let fp = bob.secret.public().fingerprint();
            assert!(check_accepted(&log, &inv, NOW, Some(&fp)).is_ok());
            // The recipient's side checks the inviter's signature too.
            let received = ReceivedInvitation {
                id: inv.id.clone(),
                vault_id: inv.vault_id.clone(),
                object_id: None,
                from: alice.user(),
                user_id: inv.user.as_ref().map(|u| u.id.clone()),
                email: inv.email.clone(),
                role: inv.role.clone(),
                created_at: inv.created_at,
                expires_at: inv.expires_at,
                signature: inv.signature.clone(),
                accepted: false,
                with_comments: false,
            };
            assert_eq!(received.verified_inviter(), Some(alice.secret.public()));
            let mut other = received.clone();
            other.role = Role::Admin;
            assert_eq!(other.verified_inviter(), None);
        }
        // Any owner's app adds them, for an invitation by any owner.
        let carol = Person::new("carol");
        let log = self::log(&alice, &[&carol]);
        let mut inv = invite(
            &carol,
            Invitee::Email("bob@example.com"),
            Role::Viewer,
            None,
        );
        accept(&mut inv, &bob);
        assert!(check_accepted(&log, &inv, NOW, None).is_ok());
        // A single note.
        let mut inv = invite(&alice, Invitee::User("bob"), Role::Viewer, Some("n1"));
        accept(&mut inv, &bob);
        assert!(check_accepted(&log, &inv, NOW, None).is_ok());
    }

    #[test]
    fn no_server_makes_or_accepts_one() {
        let (alice, bob, mallory) = (
            Person::new("alice"),
            Person::new("bob"),
            Person::new("mallory"),
        );
        let log = log(&alice, &[]);
        let fresh = |to| {
            let mut inv = invite(&alice, to, Role::Editor, None);
            accept(&mut inv, &bob);
            inv
        };
        let refused = |inv: &SentInvitation, why| {
            assert_eq!(check_accepted(&log, inv, NOW, None).unwrap_err(), why);
        };

        // Signed by someone who isn't an owner (a member, or nobody the log knows).
        let mut inv = invite(&mallory, Invitee::User("bob"), Role::Editor, None);
        accept(&mut inv, &bob);
        refused(&inv, NotAdded::NotAnOwner);
        // Claimed to be by the owner, signed by someone else.
        let mut inv = fresh(Invitee::User("bob"));
        inv.signature = sign(&mallory.secret, &inv.terms().unwrap());
        refused(&inv, NotAdded::BadSignature);
        // Anything changed after signing: the role, the note, the address, the time.
        let mut inv = fresh(Invitee::Email("bob@example.com"));
        inv.role = Role::Admin;
        refused(&inv, NotAdded::BadSignature);
        let mut inv = fresh(Invitee::Email("bob@example.com"));
        inv.object_id = Some("n1".into());
        refused(&inv, NotAdded::BadSignature);
        let mut inv = fresh(Invitee::Email("bob@example.com"));
        inv.email = Some("mallory@example.com".into());
        refused(&inv, NotAdded::BadSignature);
        let mut inv = fresh(Invitee::Email("bob@example.com"));
        inv.created_at += 1;
        refused(&inv, NotAdded::BadSignature);
        let mut inv = fresh(Invitee::Email("bob@example.com"));
        inv.with_comments = true;
        refused(&inv, NotAdded::BadSignature);
        // For another vault.
        let mut inv = fresh(Invitee::User("bob"));
        inv.vault_id = "v2".into();
        refused(&inv, NotAdded::OtherVault);
        // Brought back long after: refused whatever its expiry says.
        let inv = fresh(Invitee::User("bob"));
        assert_eq!(
            check_accepted(&log, &inv, NOW + MAX_AGE_MS + 1, None).unwrap_err(),
            NotAdded::TooOld
        );
        assert!(check_accepted(&log, &inv, NOW + MAX_AGE_MS, None).is_ok());
        assert_eq!(
            check_accepted(&log, &inv, NOW - CLOCK_SKEW_MS - 1, None).unwrap_err(),
            NotAdded::TooOld
        );
        // An invitation to bob's account, accepted by someone else.
        let mut inv = invite(&alice, Invitee::User("bob"), Role::Editor, None);
        accept(&mut inv, &mallory);
        refused(&inv, NotAdded::WrongPerson);
        // Accepted "by bob" with a key that isn't bob's: the acceptance must
        // be signed with the key wrapped to.
        let mut inv = fresh(Invitee::User("bob"));
        let a = inv.accepted.as_mut().unwrap();
        a.user.account_key = mallory.user().account_key.map(|mut k| {
            k.user_id = "bob".into();
            k
        });
        refused(&inv, NotAdded::BadKey);
        let mut inv = fresh(Invitee::User("bob"));
        let forged = sign_acceptance(&mallory.secret, &inv.terms().unwrap(), "bob");
        inv.accepted.as_mut().unwrap().signature = forged;
        refused(&inv, NotAdded::BadAcceptance);
        // An acceptance of another invitation.
        let mut inv = fresh(Invitee::User("bob"));
        let other = invite(&alice, Invitee::User("bob"), Role::Viewer, None);
        inv.accepted.as_mut().unwrap().signature =
            sign_acceptance(&bob.secret, &other.terms().unwrap(), "bob");
        refused(&inv, NotAdded::BadAcceptance);
        // A key other than the one pinned for them.
        let inv = fresh(Invitee::User("bob"));
        let fp = mallory.secret.public().fingerprint();
        assert_eq!(
            check_accepted(&log, &inv, NOW, Some(&fp)).unwrap_err(),
            NotAdded::KeyChanged
        );
        // Roles invitations don't give.
        let mut inv = invite(&alice, Invitee::User("bob"), Role::Owner, None);
        accept(&mut inv, &bob);
        refused(&inv, NotAdded::BadRole);
        let mut inv = invite(&alice, Invitee::User("bob"), Role::Admin, Some("n1"));
        accept(&mut inv, &bob);
        refused(&inv, NotAdded::BadRole);
        // Names both an account and an address.
        let mut inv = fresh(Invitee::User("bob"));
        inv.email = Some("bob@example.com".into());
        refused(&inv, NotAdded::Malformed);
    }

    #[test]
    fn an_owner_who_stepped_down_invites_no_one() {
        let (alice, carol, bob) = (
            Person::new("alice"),
            Person::new("carol"),
            Person::new("bob"),
        );
        let mut state = log(&alice, &[&carol]);
        let mut inv = invite(&carol, Invitee::User("bob"), Role::Editor, None);
        accept(&mut inv, &bob);
        assert!(check_accepted(&state, &inv, NOW, None).is_ok());
        let mut entries = entries(&alice, &[&carol]);
        let e = LogEntry::sign(
            &alice.secret,
            alice.id,
            "v1",
            entries.last(),
            LogBody::Role {
                user_id: "carol".into(),
                role: Role::Editor,
            },
            NOW,
        );
        entries.push(e);
        state = LogState::verify("v1", &alice.secret.public(), &entries).unwrap();
        assert_eq!(
            check_accepted(&state, &inv, NOW, None).unwrap_err(),
            NotAdded::NotAnOwner
        );
    }
}
