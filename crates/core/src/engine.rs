//! Platform-independent client logic shared by the browser app, the desktop
//! client and the agent gateway. No I/O happens here: callers move the
//! request/response types over HTTP or WebSocket and feed results back in.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::crypto::{CryptoError, Purpose};
use crate::doc::{DocError, IndexDoc, NoteDoc};
use crate::keys::{
    self, AccountPublic, AccountSecret, DeviceKey, RecoveryKey, VaultKey, VaultKeyring,
    wrap_account_for_device, wrap_vault_key,
};
use crate::memberlog;
use crate::protocol::*;
use crate::random_id;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Doc(#[from] DocError),
    #[error("{0}")]
    Invalid(String),
    /// Content whose author can't be confirmed (R3): refused, not applied.
    #[error(transparent)]
    Author(#[from] memberlog::AuthorError),
    /// A shared note's key without the owner's grant signature (yet): not
    /// used. The owner's app signs it the next time it opens the vault.
    #[error("the note's key isn't confirmed by its owner yet; they need to open the app once")]
    UnconfirmedKey,
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Plaintext vault metadata (stored encrypted in `encrypted_meta`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct VaultMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A short text identifying the vault, usually one emoji
    /// ([`VaultMeta::set_icon`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A small picture identifying the vault, shown instead of `icon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<VaultPicture>,
}

/// A vault's picture: a small raster image. Clients make it from what was
/// chosen: a square of at most [`VaultPicture::MAX_SIDE`] pixels, encoded
/// again, so it is never the original file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VaultPicture {
    pub mime: String,
    #[serde(with = "crate::b64")]
    pub data: Vec<u8>,
}

impl VaultPicture {
    pub const MAX_SIDE: u32 = 128;
    /// Encoded size limit: keeps the metadata well inside the server's
    /// 16 KiB for it.
    pub const MAX_BYTES: usize = 8 * 1024;
    /// Raster types only.
    pub const TYPES: [&'static str; 3] = ["image/webp", "image/png", "image/jpeg"];

    pub fn new(mime: &str, data: Vec<u8>) -> Result<Self> {
        let p = VaultPicture {
            mime: mime.to_string(),
            data,
        };
        if !p.is_valid() {
            return Err(EngineError::Invalid(format!(
                "a vault picture must be a WebP, PNG or JPEG image of at most {} KiB",
                Self::MAX_BYTES / 1024
            )));
        }
        Ok(p)
    }

    /// Checked before showing one too: the owner chose what is stored.
    pub fn is_valid(&self) -> bool {
        Self::TYPES.contains(&self.mime.as_str())
            && !self.data.is_empty()
            && self.data.len() <= Self::MAX_BYTES
    }
}

impl VaultMeta {
    pub fn named(name: &str) -> Self {
        VaultMeta {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Set the icon, or clear it with a blank one ([`crate::doc::clean_icon`]).
    pub fn set_icon(&mut self, icon: &str) -> Result<()> {
        self.icon = crate::doc::clean_icon(icon).map_err(EngineError::Invalid)?;
        Ok(())
    }

    pub fn encrypt(&self, ring: &VaultKeyring) -> Result<Vec<u8>> {
        Ok(ring.encrypt_meta(&serde_json::to_vec(self).expect("serializable"))?)
    }

    pub fn decrypt(ring: &VaultKeyring, ct: &[u8]) -> Result<Self> {
        let pt = ring.decrypt_meta(ct)?;
        serde_json::from_slice(&pt)
            .map_err(|e| EngineError::Invalid(format!("vault metadata: {e}")))
    }
}

// ---------------------------------------------------------------- accounts

/// Everything produced when a person sets up encryption on their first device.
pub struct AccountBootstrap {
    pub account: AccountSecret,
    pub device: DeviceKey,
    pub device_id: String,
    pub recovery: RecoveryKey,
    pub request: AccountSetupRequest,
}

pub fn bootstrap_account(user_id: &str, device_name: &str) -> Result<AccountBootstrap> {
    let account = AccountSecret::generate();
    let device = DeviceKey::generate();
    let device_id = random_id();
    let recovery = RecoveryKey::generate();
    let request = AccountSetupRequest {
        key: PublicAccountKey::new(
            user_id,
            1,
            &account.public(),
            &account.sign_account_key(user_id, 1),
        ),
        recovery_envelope: Some(recovery.wrap_account(&account, user_id)),
        device: Some(FirstDevice {
            registration: DeviceRegistration {
                device_id: device_id.clone(),
                name: device_name.to_string(),
                enc_public: device.public.to_vec(),
            },
            account_envelope: wrap_account_for_device(
                &account,
                &device.public,
                user_id,
                &device_id,
            )?,
            approval_signature: account
                .sign_device_approval(user_id, &device_id, &device.public)
                .to_vec(),
        }),
    };
    Ok(AccountBootstrap {
        account,
        device,
        device_id,
        recovery,
        request,
    })
}

/// Account setup for an agent identity: keys only, no device or recovery.
pub fn bootstrap_agent(agent_id: &str) -> (AccountSecret, AccountSetupRequest) {
    let account = AccountSecret::generate();
    let request = AccountSetupRequest {
        key: PublicAccountKey::new(
            agent_id,
            1,
            &account.public(),
            &account.sign_account_key(agent_id, 1),
        ),
        recovery_envelope: None,
        device: None,
    };
    (account, request)
}

/// A new (pending) device: key pair and registration request.
pub fn new_device(device_name: &str) -> (DeviceKey, DeviceRegistration) {
    let device = DeviceKey::generate();
    let reg = DeviceRegistration {
        device_id: random_id(),
        name: device_name.to_string(),
        enc_public: device.public.to_vec(),
    };
    (device, reg)
}

/// Approve a pending device (run on a device that holds the account secret,
/// or on the new device itself after unlocking with the recovery key).
pub fn approve_device(
    account: &AccountSecret,
    user_id: &str,
    device_id: &str,
    device_public: &[u8],
) -> Result<ActivateDeviceRequest> {
    let pk: [u8; 32] = device_public
        .try_into()
        .map_err(|_| EngineError::Invalid("device key must be 32 bytes".into()))?;
    Ok(ActivateDeviceRequest {
        account_envelope: wrap_account_for_device(account, &pk, user_id, device_id)?,
        signature: account
            .sign_device_approval(user_id, device_id, &pk)
            .to_vec(),
    })
}

/// The approval code for a device (see [`keys::device_verification_code`]).
/// `account` is the account key: on the approving device its own, on the new
/// device the published key it pinned, which an approved secret must then
/// match ([`check_approved_account`]).
pub fn verification_code(
    user_id: &str,
    device_id: &str,
    device_public: &[u8],
    account: &AccountPublic,
) -> Option<String> {
    let pk: [u8; 32] = device_public.try_into().ok()?;
    Some(keys::device_verification_code(
        user_id, device_id, &pk, account,
    ))
}

/// The account key a new device expects: the published one, checked.
pub fn published_account(user_id: &str, key: &PublicAccountKey) -> Result<AccountPublic> {
    if key.user_id != user_id {
        return Err(EngineError::Invalid(
            "the server returned an account key for a different user".into(),
        ));
    }
    Ok(key.verify()?)
}

/// Refuse an account secret that doesn't belong to the account key a device
/// was approved for: anyone (the server too) can encrypt a secret to a
/// device's public key.
pub fn check_approved_account(secret: &AccountSecret, expected: &AccountPublic) -> Result<()> {
    if secret.public() != *expected {
        return Err(EngineError::Invalid(
            "the keys sent to this device belong to a different account key than the one it was approved for"
                .into(),
        ));
    }
    Ok(())
}

/// New recovery key replacing the old one; `version` (the time in ms) must
/// be higher than that of the update it replaces.
pub fn rotate_recovery(
    account: &AccountSecret,
    user_id: &str,
    version: u64,
) -> (RecoveryKey, RecoveryUpdate) {
    let rk = RecoveryKey::generate();
    let env = rk.wrap_account(account, user_id);
    let signature = account
        .sign_recovery_update_v2(user_id, version, &env)
        .to_vec();
    (
        rk,
        RecoveryUpdate {
            recovery_envelope: env,
            signature,
            version: Some(version),
        },
    )
}

// ---------------------------------------------------------------- trust on first use

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinStatus {
    /// First time we see this user's key; it is now pinned.
    New,
    Match,
    /// The server presented a different key than the one pinned before.
    Changed,
}

/// Remember account-key fingerprints per user id; report changes.
pub fn check_pin(
    pins: &mut BTreeMap<String, String>,
    user_id: &str,
    fingerprint: &str,
) -> PinStatus {
    match pins.get(user_id) {
        None => {
            pins.insert(user_id.to_string(), fingerprint.to_string());
            PinStatus::New
        }
        Some(p) if p == fingerprint => PinStatus::Match,
        Some(_) => PinStatus::Changed,
    }
}

// ---------------------------------------------------------------- vault keys

/// A new vault: its first key wrapped to ourselves, and the first entry of
/// its membership log (signed at `now`, ms).
pub fn new_vault(
    account: &AccountSecret,
    user_id: &str,
    meta: &VaultMeta,
    now: i64,
) -> Result<(CreateVaultRequest, VaultKeyring)> {
    let id = random_id();
    let key = VaultKey::generate();
    let mut ring = VaultKeyring::new(&id);
    ring.insert(1, key.clone());
    let envelope = wrap_vault_key(&key, &id, 1, user_id, &account.public().enc_public)?;
    let log_entry = memberlog::LogEntry::sign(
        account,
        user_id,
        &id,
        None,
        memberlog::LogBody::Create {
            owner: memberlog::LogMember::new(user_id, Role::Owner, &account.public()),
            key_commit: memberlog::key_commitment(&id, 1, &key),
        },
        now,
    );
    let req = CreateVaultRequest {
        id: id.clone(),
        encrypted_meta: meta.encrypt(&ring)?,
        key_envelope: KeyEnvelope {
            user_id: user_id.to_string(),
            epoch: 1,
            envelope,
            signature: None,
        },
        log_entry: Some(log_entry),
    };
    Ok((req, ring))
}

pub fn keyring_from_response(
    account: &AccountSecret,
    user_id: &str,
    vault_id: &str,
    r: &VaultKeysResponse,
) -> Result<VaultKeyring> {
    let mut ring = VaultKeyring::new(vault_id);
    for e in &r.envelopes {
        if e.user_id != user_id {
            continue;
        }
        ring.insert(
            e.epoch,
            keys::unwrap_vault_key(account, &e.envelope, vault_id, e.epoch, user_id)?,
        );
    }
    if ring.current_epoch().is_none() {
        return Err(EngineError::Invalid(
            "no key has been shared with you for this vault yet".into(),
        ));
    }
    let chain: BTreeMap<u32, Vec<u8>> = r.chain.iter().map(|c| (c.epoch, c.link.clone())).collect();
    ring.fill_from_chain(&chain)?;
    Ok(ring)
}

/// Wrap the current vault key for `user` after verifying their key signature.
pub fn share_envelope(ring: &VaultKeyring, user: &PublicUser) -> Result<KeyEnvelope> {
    let key = user
        .account_key
        .as_ref()
        .ok_or_else(|| EngineError::Invalid("user has not set up encryption yet".into()))?;
    if key.user_id != user.id {
        return Err(EngineError::Invalid(
            "key does not belong to this user".into(),
        ));
    }
    let public = key.verify()?;
    let (epoch, vk) = ring.current().ok_or(CryptoError::MissingEpoch(0))?;
    Ok(KeyEnvelope {
        user_id: user.id.clone(),
        epoch,
        envelope: wrap_vault_key(vk, ring.vault_id(), epoch, &user.id, &public.enc_public)?,
        signature: None,
    })
}

/// Envelopes for members who are missing the current key (and have keys now).
pub fn missing_grants(ring: &VaultKeyring, members: &[MemberInfo]) -> Vec<KeyEnvelope> {
    members
        .iter()
        .filter(|m| !m.has_current_key && m.user.account_key.is_some())
        .filter_map(|m| share_envelope(ring, &m.user).ok())
        .collect()
}

/// Move the vault to a new epoch for the given (remaining) members. The new key
/// is inserted into `ring` so subsequent writes use it.
pub fn rotate(
    ring: &mut VaultKeyring,
    members: &[MemberInfo],
    meta: &VaultMeta,
) -> Result<RotateKeysRequest> {
    let (epoch, old) = {
        let (e, k) = ring.current().ok_or(CryptoError::MissingEpoch(0))?;
        (e, k.clone())
    };
    let new_epoch = epoch + 1;
    let new = VaultKey::generate();
    let mut envelopes = Vec::new();
    for m in members {
        let Some(k) = &m.user.account_key else {
            continue;
        };
        let public = k.verify()?;
        envelopes.push(KeyEnvelope {
            user_id: m.user.id.clone(),
            epoch: new_epoch,
            envelope: wrap_vault_key(
                &new,
                ring.vault_id(),
                new_epoch,
                &m.user.id,
                &public.enc_public,
            )?,
            signature: None,
        });
    }
    let chain_link = keys::encrypt_chain_link(&new, new_epoch, &old, ring.vault_id());
    let mut next = ring.clone();
    next.insert(new_epoch, new);
    let encrypted_meta = meta.encrypt(&next)?;
    *ring = next;
    Ok(RotateKeysRequest {
        new_epoch,
        chain_link,
        encrypted_meta,
        envelopes,
        objects: vec![],
        log_entry: None,
        without: vec![],
    })
}

/// [`rotate`], and move every note's document key to a new epoch wrapped
/// under the new vault key (removed members knew the old ones). `notes` are
/// the notes' keyrings with their metadata and current recipients. Returns the
/// notes' new keyrings, to use once the server accepted the rotation.
pub fn rotate_with_notes(
    ring: &mut VaultKeyring,
    members: &[MemberInfo],
    meta: &VaultMeta,
    notes: &[(String, &VaultKeyring, ObjectMeta, Vec<PublicUser>)],
) -> Result<(RotateKeysRequest, HashMap<String, VaultKeyring>)> {
    let mut req = rotate(ring, members, meta)?;
    let mut rings = HashMap::new();
    for (object_id, doc_ring, meta, recipients) in notes {
        let (next, update) = rotate_object_key(ring, doc_ring, object_id, meta, recipients)?;
        req.objects.push(ObjectKeyChange {
            object_id: object_id.clone(),
            update,
        });
        rings.insert(object_id.clone(), next);
    }
    Ok((req, rings))
}

/// The notes of a vault that are shared with someone individually, from its
/// keys response (servers before per-note keys: those with a document key).
pub fn shared_objects(r: &VaultKeysResponse) -> BTreeSet<String> {
    match &r.shared {
        Some(list) => list.iter().cloned().collect(),
        None => r.objects.iter().map(|e| e.object_id.clone()).collect(),
    }
}

// ---------------------------------------------------------------- shared notes

/// Plaintext metadata of an individually shared note (what recipients see,
/// since they cannot read the vault index).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ObjectMeta {
    pub title: String,
    /// The note's icon (see [`crate::doc::Entry::icon`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl ObjectMeta {
    pub fn titled(title: &str) -> Self {
        ObjectMeta {
            title: title.to_string(),
            icon: None,
        }
    }

    /// What recipients see of a note's index entry.
    pub fn of(e: &crate::doc::Entry) -> Self {
        ObjectMeta {
            title: e.title().to_string(),
            icon: e.icon.clone(),
        }
    }

    pub fn encrypt(&self, doc_ring: &VaultKeyring, object_id: &str) -> Result<Vec<u8>> {
        Ok(doc_ring.encrypt(
            Purpose::ObjectMeta,
            object_id,
            &serde_json::to_vec(self).expect("serializable"),
        )?)
    }

    pub fn decrypt(doc_ring: &VaultKeyring, object_id: &str, ct: &[u8]) -> Result<Self> {
        let pt = doc_ring.decrypt(Purpose::ObjectMeta, object_id, ct)?;
        let mut m: Self = serde_json::from_slice(&pt)
            .map_err(|e| EngineError::Invalid(format!("note metadata: {e}")))?;
        // Any writer of the note sets it: only a valid one is shown.
        m.icon = m
            .icon
            .and_then(|i| crate::doc::clean_icon(&i).ok().flatten());
        Ok(m)
    }
}

/// Document keyrings of a vault's shared notes, for vault members (from the
/// vault keys response). Notes whose keys cannot be unwrapped are skipped.
pub fn object_keyrings(
    vault_ring: &VaultKeyring,
    r: &VaultKeysResponse,
) -> HashMap<String, VaultKeyring> {
    let mut rings: HashMap<String, VaultKeyring> = HashMap::new();
    for e in &r.objects {
        if let Ok(k) =
            keys::unwrap_doc_key_for_vault(vault_ring, &e.object_id, e.epoch, &e.envelope)
        {
            rings
                .entry(e.object_id.clone())
                .or_insert_with(|| VaultKeyring::new(vault_ring.vault_id()))
                .insert(e.epoch, k);
        }
    }
    for (oid, ring) in rings.iter_mut() {
        let chain: BTreeMap<u32, Vec<u8>> = r
            .object_chain
            .iter()
            .filter(|c| &c.object_id == oid)
            .map(|c| (c.epoch, c.link.clone()))
            .collect();
        let _ = ring.fill_from_chain_with(&chain, &keys::object_scope(vault_ring.vault_id(), oid));
    }
    rings
}

/// A vault member's keyring of one note (from `GET …/objects/{o}/keys`):
/// `None` if the answer has no keys for members (older servers: reload the
/// whole vault's keys instead).
pub fn object_keyring_for_member(
    vault_ring: &VaultKeyring,
    r: &ObjectKeysResponse,
) -> Option<VaultKeyring> {
    let mut ring = VaultKeyring::new(vault_ring.vault_id());
    for e in &r.member_envelopes {
        if let Ok(k) =
            keys::unwrap_doc_key_for_vault(vault_ring, &r.object_id, e.epoch, &e.envelope)
        {
            ring.insert(e.epoch, k);
        }
    }
    ring.current_epoch()?;
    let chain: BTreeMap<u32, Vec<u8>> = r.chain.iter().map(|c| (c.epoch, c.link.clone())).collect();
    let _ = ring.fill_from_chain_with(
        &chain,
        &keys::object_scope(vault_ring.vault_id(), &r.object_id),
    );
    Some(ring)
}

/// Document keyring of a note shared with us individually.
/// With `owner` (the vault owner's key, as pinned): only the newest key
/// given to us is used, and only if the owner signed giving it to us; older
/// ones follow from the key chain. So a server can't hand us a key of its
/// own. Without: every envelope as is (tests, older owners).
pub fn object_keyring_for_recipient(
    account: &AccountSecret,
    user_id: &str,
    vault_id: &str,
    r: &ObjectKeysResponse,
    owner: Option<&AccountPublic>,
) -> Result<VaultKeyring> {
    let scope = keys::object_scope(vault_id, &r.object_id);
    let mut ring = VaultKeyring::new(vault_id);
    let mine: Vec<&KeyEnvelope> = r
        .envelopes
        .iter()
        .filter(|e| e.user_id == user_id)
        .collect();
    match owner {
        None => {
            for e in &mine {
                ring.insert(
                    e.epoch,
                    keys::unwrap_vault_key(account, &e.envelope, &scope, e.epoch, user_id)?,
                );
            }
        }
        Some(owner) => {
            if let Some(e) = mine.iter().max_by_key(|e| e.epoch) {
                let k = keys::unwrap_vault_key(account, &e.envelope, &scope, e.epoch, user_id)?;
                let confirmed = e.signature.as_deref().is_some_and(|sig| {
                    memberlog::verify_note_grant(
                        owner,
                        vault_id,
                        &r.object_id,
                        user_id,
                        e.epoch,
                        &k,
                        sig,
                    )
                });
                if !confirmed {
                    return Err(EngineError::UnconfirmedKey);
                }
                ring.insert(e.epoch, k);
            }
        }
    }
    if ring.current_epoch().is_none() {
        return Err(EngineError::Invalid(
            "no key has been shared with you for this note".into(),
        ));
    }
    let chain: BTreeMap<u32, Vec<u8>> = r.chain.iter().map(|c| (c.epoch, c.link.clone())).collect();
    ring.fill_from_chain_with(&chain, &scope)?;
    Ok(ring)
}

/// A new note's own document key (R2): private (only vault members get it,
/// wrapped under the vault key), registered before the note's first content
/// is uploaded, so nothing of it is ever encrypted with the vault key itself.
pub fn new_document_key(
    vault_ring: &VaultKeyring,
    object_id: &str,
    title: &str,
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    first_document_key(vault_ring, object_id, title, None, true)
}

/// A private document key for a note whose content so far is under the vault
/// key (moving older notes to their own key, e.g. when compacting them):
/// `state` and `covers_seq` as for [`start_sharing`].
pub fn adopt_document_key(
    vault_ring: &VaultKeyring,
    object_id: &str,
    title: &str,
    state: &[u8],
    covers_seq: i64,
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    first_document_key(
        vault_ring,
        object_id,
        title,
        Some((state, covers_seq)),
        true,
    )
}

fn first_document_key(
    vault_ring: &VaultKeyring,
    object_id: &str,
    title: &str,
    state: Option<(&[u8], i64)>,
    private: bool,
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    let dk = VaultKey::generate();
    let epoch = keys::FIRST_DOC_EPOCH;
    let mut ring = VaultKeyring::new(vault_ring.vault_id());
    ring.insert(epoch, dk.clone());
    let snapshot = match state {
        Some((state, covers_seq)) => Some(CreateSnapshotRequest {
            snapshot_id: random_id(),
            covers_seq,
            payload: ring.encrypt(Purpose::Snapshot, object_id, state)?,
            signature: None,
            carries: false,
        }),
        None => None,
    };
    let req = ObjectKeyUpdate {
        epoch,
        vault_envelope: keys::wrap_doc_key_for_vault(vault_ring, object_id, epoch, &dk)?,
        chain_link: None,
        envelopes: vec![],
        encrypted_meta: ObjectMeta::titled(title).encrypt(&ring, object_id)?,
        snapshot,
        private,
        without: vec![],
    };
    Ok((ring, req))
}

/// Start sharing a note that already has its own (private) document key: a
/// fresh epoch *without* a chain link to the earlier ones, so recipients can
/// never decrypt what was written before, and a snapshot of the note's full
/// state under it so they can read the current content. Vault members keep
/// every epoch (each is wrapped under the vault key).
pub fn start_sharing_keyed(
    vault_ring: &VaultKeyring,
    doc_ring: &VaultKeyring,
    object_id: &str,
    meta: &ObjectMeta,
    state: &[u8],
    covers_seq: i64,
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    let epoch = doc_ring
        .current_epoch()
        .ok_or(CryptoError::MissingEpoch(0))?
        + 1;
    let dk = VaultKey::generate();
    let mut next = doc_ring.clone();
    next.insert(epoch, dk.clone());
    let req = ObjectKeyUpdate {
        epoch,
        vault_envelope: keys::wrap_doc_key_for_vault(vault_ring, object_id, epoch, &dk)?,
        chain_link: None,
        envelopes: vec![],
        encrypted_meta: meta.encrypt(&next, object_id)?,
        snapshot: Some(CreateSnapshotRequest {
            snapshot_id: random_id(),
            covers_seq,
            payload: next.encrypt(Purpose::Snapshot, object_id, state)?,
            signature: None,
            carries: false,
        }),
        private: false,
        without: vec![],
    };
    Ok((next, req))
}

/// Give a note its own document key so it can be shared without the vault
/// key. `state` is the note's full CRDT state; it is re-encrypted as a
/// snapshot under the new key so recipients can read the current content.
/// `covers_seq` must cover every update of this note the caller has applied.
pub fn start_sharing(
    vault_ring: &VaultKeyring,
    object_id: &str,
    meta: &ObjectMeta,
    state: &[u8],
    covers_seq: i64,
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    let dk = VaultKey::generate();
    let epoch = keys::FIRST_DOC_EPOCH;
    let mut ring = VaultKeyring::new(vault_ring.vault_id());
    ring.insert(epoch, dk.clone());
    let req = ObjectKeyUpdate {
        epoch,
        vault_envelope: keys::wrap_doc_key_for_vault(vault_ring, object_id, epoch, &dk)?,
        chain_link: None,
        envelopes: vec![],
        encrypted_meta: meta.encrypt(&ring, object_id)?,
        snapshot: Some(CreateSnapshotRequest {
            snapshot_id: random_id(),
            covers_seq,
            payload: ring.encrypt(Purpose::Snapshot, object_id, state)?,
            signature: None,
            carries: false,
        }),
        private: false,
        without: vec![],
    };
    Ok((ring, req))
}

/// New document key epoch for the given (remaining) recipients, e.g. after
/// removing someone or after the vault key itself was rotated.
pub fn rotate_object_key(
    vault_ring: &VaultKeyring,
    doc_ring: &VaultKeyring,
    object_id: &str,
    meta: &ObjectMeta,
    recipients: &[PublicUser],
) -> Result<(VaultKeyring, ObjectKeyUpdate)> {
    let (epoch, old) = doc_ring.current().ok_or(CryptoError::MissingEpoch(0))?;
    let new_epoch = epoch + 1;
    let nk = VaultKey::generate();
    let scope = keys::object_scope(vault_ring.vault_id(), object_id);
    let mut envelopes = Vec::new();
    for u in recipients {
        let Some(k) = &u.account_key else { continue };
        if k.user_id != u.id {
            return Err(EngineError::Invalid(
                "key does not belong to this user".into(),
            ));
        }
        let public = k.verify()?;
        envelopes.push(KeyEnvelope {
            user_id: u.id.clone(),
            epoch: new_epoch,
            envelope: wrap_vault_key(&nk, &scope, new_epoch, &u.id, &public.enc_public)?,
            signature: None,
        });
    }
    let mut next = doc_ring.clone();
    next.insert(new_epoch, nk.clone());
    let req = ObjectKeyUpdate {
        epoch: new_epoch,
        vault_envelope: keys::wrap_doc_key_for_vault(vault_ring, object_id, new_epoch, &nk)?,
        chain_link: Some(keys::encrypt_chain_link(&nk, new_epoch, old, &scope)),
        envelopes,
        encrypted_meta: meta.encrypt(&next, object_id)?,
        snapshot: None,
        private: false,
        without: vec![],
    };
    Ok((next, req))
}

/// Wrap a shared note's current document key for one more person.
pub fn object_share_envelope(
    doc_ring: &VaultKeyring,
    object_id: &str,
    user: &PublicUser,
) -> Result<KeyEnvelope> {
    let key = user
        .account_key
        .as_ref()
        .ok_or_else(|| EngineError::Invalid("this person has not set up encryption yet".into()))?;
    if key.user_id != user.id {
        return Err(EngineError::Invalid(
            "key does not belong to this user".into(),
        ));
    }
    let public = key.verify()?;
    let (epoch, dk) = doc_ring.current().ok_or(CryptoError::MissingEpoch(0))?;
    let scope = keys::object_scope(doc_ring.vault_id(), object_id);
    Ok(KeyEnvelope {
        user_id: user.id.clone(),
        epoch,
        envelope: wrap_vault_key(dk, &scope, epoch, &user.id, &public.enc_public)?,
        signature: None,
    })
}

// ---------------------------------------------------------------- vault replica

/// A local update not yet acknowledged by the server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub client_update_id: String,
    pub object_id: String,
    /// Plaintext CRDT update (re-encrypted with the current epoch on retry).
    pub update: Vec<u8>,
}

#[derive(Default, Debug, Clone)]
pub struct Applied {
    /// Objects whose content changed.
    pub objects: BTreeSet<String>,
    pub index_changed: bool,
    pub purged: BTreeSet<String>,
}

impl Applied {
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && !self.index_changed && self.purged.is_empty()
    }

    pub fn merge(&mut self, o: Applied) {
        self.objects.extend(o.objects);
        self.index_changed |= o.index_changed;
        self.purged.extend(o.purged);
    }
}

/// Updates since the last snapshot after which a client should compact.
pub const COMPACT_AFTER: u32 = 200;

/// Content received before we had the key to read it (e.g. a note just got
/// its own key, or the vault key was rotated, and the update arrived before
/// the new key). Kept and replayed by [`VaultReplica::retry_deferred`] once the
/// keys are refreshed, so nothing is skipped.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Deferred {
    Update(Update),
    Snapshot(Snapshot),
}

const MAX_DEFERRED: usize = 20_000;

/// True if the error means "we don't hold this key yet" (refresh keys and retry).
pub fn missing_key(e: &EngineError) -> bool {
    matches!(e, EngineError::Crypto(CryptoError::MissingEpoch(_)))
}

/// Local replica of one vault: keys, index, notes and sync bookkeeping.
pub struct VaultReplica {
    pub id: String,
    pub role: Role,
    pub keyring: VaultKeyring,
    pub meta: VaultMeta,
    pub index: IndexDoc,
    pub notes: HashMap<String, NoteDoc>,
    /// Highest sequence number applied (see protocol §6).
    pub seq: i64,
    /// Highest `head_seq` ever reported by the server (rollback detection).
    pub max_head_seen: i64,
    pub since_snapshot: HashMap<String, u32>,
    pub pending: Vec<Pending>,
    /// Objects permanently deleted on the server.
    pub purged: BTreeSet<String>,
    /// Document keyrings of individually shared notes (object id → keys).
    /// Content of these notes is encrypted with their own key instead of the vault key.
    pub doc_keys: HashMap<String, VaultKeyring>,
    /// False when we only have access to individually shared notes of this
    /// vault, not to the vault itself.
    pub member: bool,
    /// Notes shared with someone individually (see [`shared_objects`]).
    /// Every note may have a document key; only these are shared.
    pub shared: BTreeSet<String>,
    /// Received items waiting for a key we don't have yet.
    pub deferred: Vec<Deferred>,
    /// Updates that could not even be deferred (queue full), by sequence
    /// number. `seq` never moves past the first of them, so the next catch-up
    /// fetches them again instead of skipping them for good.
    pub dropped: BTreeMap<i64, String>,
    /// Who may author content here, from the vault's verified membership
    /// log (`None`: not checked, e.g. no log, or a recipient's replica).
    /// Set it with [`VaultReplica::set_authors`].
    pub authors: Option<memberlog::Authors>,
    /// Content whose author our copy of the log doesn't know (or knows with
    /// another key): perhaps the log moved on. Kept until the log is
    /// reloaded ([`VaultReplica::needs_log`]); updates also stay in
    /// `dropped`, so the cursor never passes them before that.
    pub unverified: Vec<Deferred>,
}

impl VaultReplica {
    pub fn new(id: &str, role: Role, keyring: VaultKeyring, meta: VaultMeta) -> Self {
        VaultReplica {
            id: id.to_string(),
            role,
            keyring,
            meta,
            index: IndexDoc::new(),
            notes: HashMap::new(),
            seq: 0,
            max_head_seen: 0,
            since_snapshot: HashMap::new(),
            pending: Vec::new(),
            purged: BTreeSet::new(),
            doc_keys: HashMap::new(),
            shared: BTreeSet::new(),
            member: true,
            deferred: Vec::new(),
            dropped: BTreeMap::new(),
            authors: None,
            unverified: Vec::new(),
        }
    }

    /// Replica for someone who can only access individually shared notes.
    pub fn for_shared_notes(vault_id: &str) -> Self {
        let mut r = VaultReplica::new(
            vault_id,
            Role::Viewer,
            VaultKeyring::new(vault_id),
            VaultMeta::default(),
        );
        r.member = false;
        r
    }

    pub fn can_write(&self) -> bool {
        self.role.can_write()
    }

    /// Whether a note is shared with someone individually.
    pub fn is_shared_object(&self, object_id: &str) -> bool {
        self.shared.contains(object_id)
    }

    /// Keyring used to encrypt new content of an object.
    pub fn ring_for(&self, object_id: &str) -> &VaultKeyring {
        self.doc_keys.get(object_id).unwrap_or(&self.keyring)
    }

    /// Encrypt content of an object with the right key (document key if shared).
    pub fn encrypt_object(
        &self,
        purpose: Purpose,
        object_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        Ok(self
            .ring_for(object_id)
            .encrypt(purpose, object_id, plaintext)?)
    }

    /// Decrypt content of an object; the header epoch selects vault or document key.
    pub fn decrypt_object(
        &self,
        purpose: Purpose,
        object_id: &str,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        let epoch = crate::crypto::sym_epoch(ciphertext)?;
        let ring = if keys::is_doc_epoch(epoch) {
            self.doc_keys
                .get(object_id)
                .ok_or(CryptoError::MissingEpoch(epoch))?
        } else {
            &self.keyring
        };
        Ok(ring.decrypt(purpose, object_id, ciphertext)?)
    }

    fn apply_plain(&mut self, object_id: &str, plain: &[u8]) -> Result<Applied> {
        let mut a = Applied::default();
        if object_id == INDEX_OBJECT_ID {
            self.index.apply_update(plain)?;
            a.index_changed = true;
        } else {
            if self.purged.contains(object_id) {
                return Ok(a);
            }
            self.notes
                .entry(object_id.to_string())
                .or_default()
                .apply_update(plain)?;
            a.objects.insert(object_id.to_string());
        }
        Ok(a)
    }

    /// Keep content we can't decrypt yet; false if there is no room.
    fn defer(&mut self, d: Deferred) -> bool {
        if self.deferred.len() < MAX_DEFERRED {
            self.deferred.push(d);
            true
        } else {
            false
        }
    }

    /// Move the sequence cursor forward, but never past an update that was
    /// dropped because it couldn't be kept (see [`VaultReplica::dropped`]).
    fn advance(&mut self, seq: i64) {
        self.seq = self.seq.max(seq);
        if let Some((&first, _)) = self.dropped.first_key_value() {
            self.seq = self.seq.min(first - 1);
        }
    }

    /// Whether some content of this object is still waiting for a key; its
    /// local state then lacks changes that `seq` already counts.
    fn object_incomplete(&self, object_id: &str) -> bool {
        self.dropped.values().any(|o| o == object_id)
            || self.deferred.iter().any(|d| match d {
                Deferred::Update(u) => u.object_id == object_id,
                Deferred::Snapshot(s) => s.object_id == object_id,
            })
    }

    pub fn apply_snapshot(&mut self, s: &Snapshot) -> Result<Applied> {
        self.apply_snapshot_checked(s, false)
    }

    fn apply_snapshot_checked(&mut self, s: &Snapshot, recheck: bool) -> Result<Applied> {
        if let Some(a) = &self.authors
            && let Err(e) = a.check_snapshot(
                &s.object_id,
                s.covers_seq,
                &s.author_id,
                &s.payload,
                s.signature.as_deref(),
            )
        {
            if !recheck && e.may_be_outdated() && self.unverified.len() < MAX_DEFERRED {
                self.unverified.push(Deferred::Snapshot(s.clone()));
            }
            return Err(e.into());
        }
        let plain = match self.decrypt_object(Purpose::Snapshot, &s.object_id, &s.payload) {
            Ok(p) => p,
            Err(e) => {
                if missing_key(&e) {
                    self.defer(Deferred::Snapshot(s.clone()));
                }
                return Err(e);
            }
        };
        let a = self.apply_plain(&s.object_id, &plain)?;
        self.since_snapshot.insert(s.object_id.clone(), 0);
        Ok(a)
    }

    /// Apply a server update. Always applied (CRDT merges are idempotent), so
    /// out-of-order delivery can never drop content. Updates we cannot read
    /// yet are deferred (see [`Deferred`]).
    pub fn apply_update(&mut self, u: &Update) -> Result<Applied> {
        self.apply_update_checked(u, false)
    }

    fn apply_update_checked(&mut self, u: &Update, recheck: bool) -> Result<Applied> {
        if let Some(a) = &self.authors
            && let Err(e) = a.check_update(
                &u.object_id,
                u.seq,
                &u.author_id,
                &u.payload,
                u.signature.as_deref(),
            )
        {
            if !recheck && e.may_be_outdated() && self.unverified.len() < MAX_DEFERRED {
                // Until the log is reloaded: the cursor stops before it.
                self.unverified.push(Deferred::Update(u.clone()));
                self.dropped.insert(u.seq, u.object_id.clone());
            } else {
                // Refused for good (unsigned, or checked against a fresh log).
                self.dropped.remove(&u.seq);
            }
            self.advance(u.seq);
            return Err(e.into());
        }
        let plain = match self.decrypt_object(Purpose::Update, &u.object_id, &u.payload) {
            Ok(p) => p,
            Err(e) => {
                if missing_key(&e) && !self.defer(Deferred::Update(u.clone())) {
                    self.dropped.insert(u.seq, u.object_id.clone());
                }
                self.advance(u.seq);
                return Err(e);
            }
        };
        self.dropped.remove(&u.seq);
        self.advance(u.seq);
        let a = self.apply_plain(&u.object_id, &plain)?;
        *self.since_snapshot.entry(u.object_id.clone()).or_default() += 1;
        Ok(a)
    }

    /// Whether some received content waits for a fresh copy of the vault's
    /// membership log (reload it, then call [`set_authors`](Self::set_authors)).
    pub fn needs_log(&self) -> bool {
        !self.unverified.is_empty()
    }

    /// Use who may author content from a (re)loaded log, and check the
    /// content that waited for it once more; what fails now is refused.
    pub fn set_authors(
        &mut self,
        authors: Option<memberlog::Authors>,
    ) -> (Applied, Vec<EngineError>) {
        self.authors = authors;
        let mut all = Applied::default();
        let mut errors = Vec::new();
        for d in std::mem::take(&mut self.unverified) {
            let r = match &d {
                Deferred::Snapshot(s) => self.apply_snapshot_checked(s, true),
                Deferred::Update(u) => self.apply_update_checked(u, true),
            };
            match r {
                Ok(a) => all.merge(a),
                Err(e) => errors.push(e),
            }
        }
        (all, errors)
    }

    /// Whether some received content is waiting for keys (refresh them, then
    /// call [`retry_deferred`](Self::retry_deferred)).
    pub fn needs_keys(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Replay deferred content after keys were refreshed.
    pub fn retry_deferred(&mut self) -> (Applied, Vec<EngineError>) {
        let mut all = Applied::default();
        let mut errors = Vec::new();
        for d in std::mem::take(&mut self.deferred) {
            let r = match &d {
                Deferred::Snapshot(s) => self.apply_snapshot(s),
                Deferred::Update(u) => self.apply_update(u),
            };
            match r {
                Ok(a) => all.merge(a),
                Err(e) => errors.push(e),
            }
        }
        (all, errors)
    }

    pub fn apply_purge(&mut self, object_id: &str, seq: i64) -> Applied {
        self.notes.remove(object_id);
        self.since_snapshot.remove(object_id);
        self.pending.retain(|p| p.object_id != object_id);
        self.purged.insert(object_id.to_string());
        self.dropped.retain(|_, o| o != object_id);
        self.advance(seq);
        let mut a = Applied::default();
        a.purged.insert(object_id.to_string());
        a
    }

    /// Apply one catch-up page. Decryption failures of individual items are
    /// collected rather than aborting, so one bad ciphertext cannot hide the rest.
    pub fn apply_changes(&mut self, ch: &ChangesResponse) -> (Applied, Vec<EngineError>) {
        let mut all = Applied::default();
        let mut errors = Vec::new();
        for s in &ch.snapshots {
            match self.apply_snapshot(s) {
                Ok(a) => all.merge(a),
                Err(e) => errors.push(e),
            }
        }
        for u in &ch.updates {
            match self.apply_update(u) {
                Ok(a) => all.merge(a),
                Err(e) => errors.push(e),
            }
        }
        for p in &ch.purged {
            all.merge(self.apply_purge(&p.object_id, p.seq));
        }
        self.advance(ch.next_since.min(ch.head_seq));
        self.observe_head(ch.head_seq);
        (all, errors)
    }

    /// Record the server's head; returns true if it went backwards (possible
    /// rollback or restore from backup).
    pub fn observe_head(&mut self, head: i64) -> bool {
        let rolled_back = head < self.max_head_seen;
        self.max_head_seen = self.max_head_seen.max(head);
        rolled_back
    }

    /// The server delivered everything up to `head` (WebSocket `synced`).
    pub fn synced(&mut self, head: i64) -> bool {
        let rb = self.observe_head(head);
        self.advance(head);
        rb
    }

    /// Record and encrypt a local change for sending.
    pub fn local_change(&mut self, object_id: &str, update: Vec<u8>) -> Result<PushUpdate> {
        let client_update_id = random_id();
        let payload = self.encrypt_object(Purpose::Update, object_id, &update)?;
        self.pending.push(Pending {
            client_update_id: client_update_id.clone(),
            object_id: object_id.to_string(),
            update,
        });
        Ok(PushUpdate {
            object_id: object_id.to_string(),
            client_update_id,
            payload,
            signature: None,
        })
    }

    /// All unacknowledged updates, encrypted with the current epoch.
    pub fn pending_pushes(&self) -> Result<Vec<PushUpdate>> {
        self.pending
            .iter()
            .map(|p| {
                Ok(PushUpdate {
                    object_id: p.object_id.clone(),
                    client_update_id: p.client_update_id.clone(),
                    payload: self.encrypt_object(Purpose::Update, &p.object_id, &p.update)?,
                    signature: None,
                })
            })
            .collect()
    }

    /// Mark pushed updates as stored. `in_order` is true for WebSocket acks,
    /// which arrive in log order and so also advance `seq`; REST responses can
    /// overtake other clients' updates still in flight and must not.
    pub fn ack(&mut self, results: &[PushResult], in_order: bool) {
        for r in results {
            if let Some(i) = self
                .pending
                .iter()
                .position(|p| p.client_update_id == r.client_update_id)
            {
                let p = self.pending.remove(i);
                *self.since_snapshot.entry(p.object_id).or_default() += 1;
            }
            if in_order {
                self.advance(r.seq);
            }
        }
    }

    /// Objects that have accumulated enough updates to be worth compacting.
    /// Only vault members compact: their stream covers every object, so `seq`
    /// is a valid `covers_seq` for any of them.
    pub fn compaction_candidates(&self) -> Vec<String> {
        if !self.can_write() || !self.member {
            return vec![];
        }
        self.since_snapshot
            .iter()
            .filter(|(o, n)| {
                **n >= COMPACT_AFTER
                    && !self.pending.iter().any(|p| &p.object_id == *o)
                    && !self.object_incomplete(o)
            })
            .map(|(o, _)| o.clone())
            .collect()
    }

    /// Encrypted snapshot of an object's full state covering `self.seq`.
    /// Refused while some of the object's content waits for a key: the
    /// snapshot would claim to cover changes it doesn't contain, and the
    /// server would then drop them.
    pub fn snapshot_request(&self, object_id: &str) -> Result<CreateSnapshotRequest> {
        if self.object_incomplete(object_id) {
            return Err(EngineError::Invalid(
                "some changes of this note are still waiting for a key".into(),
            ));
        }
        let state = if object_id == INDEX_OBJECT_ID {
            self.index.encode_state()
        } else {
            self.notes
                .get(object_id)
                .ok_or_else(|| EngineError::Invalid("unknown object".into()))?
                .encode_state()
        };
        Ok(CreateSnapshotRequest {
            snapshot_id: random_id(),
            covers_seq: self.seq,
            payload: self.encrypt_object(Purpose::Snapshot, object_id, &state)?,
            signature: None,
            carries: false,
        })
    }

    pub fn snapshot_created(&mut self, object_id: &str) {
        self.since_snapshot.insert(object_id.to_string(), 0);
    }

    pub fn note(&self, id: &str) -> Option<&NoteDoc> {
        self.notes.get(id)
    }

    pub fn note_text(&self, id: &str) -> String {
        self.notes.get(id).map(|n| n.text()).unwrap_or_default()
    }

    /// Decrypt a historical snapshot into note text.
    pub fn decrypt_revision(&self, s: &Snapshot) -> Result<String> {
        let plain = self.decrypt_object(Purpose::Snapshot, &s.object_id, &s.payload)?;
        Ok(NoteDoc::from_updates([plain.as_slice()])?.text())
    }

    pub fn encrypt_attachment(&self, attachment_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        Ok(self
            .keyring
            .encrypt(Purpose::Attachment, attachment_id, data)?)
    }

    pub fn decrypt_attachment(&self, attachment_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        Ok(self
            .keyring
            .decrypt(Purpose::Attachment, attachment_id, data)?)
    }

    pub fn encrypt_awareness(&self, object_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        self.encrypt_object(Purpose::Awareness, object_id, data)
    }

    pub fn decrypt_awareness(&self, object_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        self.decrypt_object(Purpose::Awareness, object_id, data)
    }
}

/// Plaintext presence record, encrypted with purpose `Awareness`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Presence {
    pub name: String,
    pub color: String,
    /// Note being viewed, if any.
    pub object_id: Option<String>,
    /// Cursor as (anchor, head) UTF-8 byte offsets.
    pub cursor: Option<(usize, usize)>,
    pub at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::EntryKind;

    #[test]
    fn a_shared_notes_icon_reaches_its_recipients() {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let meta = ObjectMeta {
            title: "Trip".into(),
            icon: Some("🏖️".into()),
        };
        let (doc, req) = start_sharing(&ring, "n", &meta, &[], 0).unwrap();
        assert_eq!(
            ObjectMeta::decrypt(&doc, "n", &req.encrypted_meta).unwrap(),
            meta
        );
        // Metadata from before icons reads; an icon breaking the rules is dropped.
        let old = doc
            .encrypt(Purpose::ObjectMeta, "n", br#"{"title":"Trip"}"#)
            .unwrap();
        assert_eq!(
            ObjectMeta::decrypt(&doc, "n", &old).unwrap(),
            ObjectMeta::titled("Trip")
        );
        let long = format!(r#"{{"title":"Trip","icon":"{}"}}"#, "x".repeat(40));
        let bad = doc
            .encrypt(Purpose::ObjectMeta, "n", long.as_bytes())
            .unwrap();
        assert_eq!(ObjectMeta::decrypt(&doc, "n", &bad).unwrap().icon, None);
    }

    #[test]
    fn vault_icon_and_picture() {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        // Metadata from before icons and pictures still reads.
        let old = ring.encrypt_meta(br#"{"name":"Home"}"#).unwrap();
        assert_eq!(
            VaultMeta::decrypt(&ring, &old).unwrap(),
            VaultMeta::named("Home")
        );

        let mut meta = VaultMeta::named(&"n".repeat(200));
        meta.set_icon("  🏡\n").unwrap();
        assert_eq!(meta.icon.as_deref(), Some("🏡"));
        // A family emoji is one icon of several characters.
        meta.set_icon("👨‍👩‍👧‍👦").unwrap();
        assert!(
            meta.set_icon(&"x".repeat(crate::doc::MAX_ICON_CHARS + 1))
                .is_err()
        );
        meta.set_icon(" ").unwrap();
        assert_eq!(meta.icon, None);

        assert!(VaultPicture::new("image/svg+xml", vec![1]).is_err());
        assert!(VaultPicture::new("image/png", vec![]).is_err());
        assert!(VaultPicture::new("image/png", vec![0; VaultPicture::MAX_BYTES + 1]).is_err());
        // The largest picture, a long name and an icon fit the server's limit.
        meta.set_icon("👨‍👩‍👧‍👦").unwrap();
        meta.picture =
            Some(VaultPicture::new("image/webp", vec![0xff; VaultPicture::MAX_BYTES]).unwrap());
        let ct = meta.encrypt(&ring).unwrap();
        assert!(ct.len() <= 16 * 1024, "{} bytes", ct.len());
        assert_eq!(VaultMeta::decrypt(&ring, &ct).unwrap(), meta);
    }

    #[test]
    fn content_by_an_author_the_log_doesnt_know_yet_waits_for_it() {
        use crate::memberlog::{self, LogBody, LogEntry, LogMember, LogState, key_commitment};
        let alice = AccountSecret::generate();
        let bob = AccountSecret::generate();
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let mut log = vec![LogEntry::sign(
            &alice,
            "alice",
            "v",
            None,
            LogBody::Create {
                owner: LogMember::new("alice", Role::Owner, &alice.public()),
                key_commit: key_commitment("v", 1, ring.get(1).unwrap()),
            },
            1,
        )];
        let next = |log: &mut Vec<LogEntry>, body| {
            let e = LogEntry::sign(&alice, "alice", "v", log.last(), body, 2);
            log.push(e);
        };
        next(&mut log, LogBody::RequireSigned { from_seq: 0 });
        let before_bob = LogState::verify("v", &alice.public(), &log)
            .unwrap()
            .authors();
        next(
            &mut log,
            LogBody::Add {
                member: LogMember::new("bob", Role::Editor, &bob.public()),
            },
        );
        let with_bob = LogState::verify("v", &alice.public(), &log)
            .unwrap()
            .authors();

        let mut r = VaultReplica::new("v", Role::Owner, ring.clone(), VaultMeta::named("V"));
        r.set_authors(Some(before_bob.clone()));
        let n = NoteDoc::new();
        let payload = ring
            .encrypt(Purpose::Update, "n1", &n.set_text("by bob").unwrap())
            .unwrap();
        let u = Update {
            object_id: "n1".into(),
            seq: 1,
            author_id: "bob".into(),
            signature: Some(memberlog::sign_update(&bob, "v", "n1", &payload)),
            payload,
            created_at: 0,
        };
        assert!(r.apply_update(&u).is_err());
        assert!(r.needs_log());
        assert_eq!(r.seq, 0, "the cursor waits for it");
        // A fresh log knows Bob: his change is applied.
        let (a, errors) = r.set_authors(Some(with_bob));
        assert!(errors.is_empty() && a.objects.contains("n1"));
        assert_eq!(r.note_text("n1"), "by bob");
        assert!(!r.needs_log());

        // Checked against a fresh log that still doesn't allow it: refused,
        // and the cursor moves on.
        let mut r2 = VaultReplica::new("v", Role::Owner, ring, VaultMeta::named("V"));
        r2.set_authors(Some(before_bob.clone()));
        assert!(r2.apply_update(&u).is_err());
        let (_, errors) = r2.set_authors(Some(before_bob));
        assert_eq!(errors.len(), 1);
        assert!(!r2.needs_log() && r2.dropped.is_empty());
        assert_eq!(r2.note_text("n1"), "");
    }

    fn member(user_id: &str, account: &AccountSecret, role: Role, has_key: bool) -> MemberInfo {
        MemberInfo {
            user: PublicUser {
                id: user_id.into(),
                display_name: user_id.into(),
                email: None,
                kind: PrincipalKind::Human,
                account_key: Some(PublicAccountKey::new(
                    user_id,
                    1,
                    &account.public(),
                    &account.sign_account_key(user_id, 1),
                )),
            },
            role,
            has_current_key: has_key,
            added_at: 0,
            added_by: None,
        }
    }

    #[test]
    fn replica_roundtrip_with_rotation() {
        let alice = AccountSecret::generate();
        let bob = AccountSecret::generate();
        let (req, mut ring) = new_vault(&alice, "alice", &VaultMeta::named("Home"), 1).unwrap();
        let mut a = VaultReplica::new(&req.id, Role::Owner, ring.clone(), VaultMeta::named("Home"));
        let up = a.index.create("n1", "A.md", EntryKind::Note, 1).unwrap();
        let p1 = a.local_change(INDEX_OBJECT_ID, up).unwrap();
        let note = a.notes.entry("n1".into()).or_default();
        let up = note.set_text("hello").unwrap();
        let p2 = a.local_change("n1", up).unwrap();
        assert_eq!(a.pending.len(), 2);
        a.ack(
            &[PushResult {
                client_update_id: p1.client_update_id.clone(),
                seq: 1,
            }],
            true,
        );
        a.ack(
            &[PushResult {
                client_update_id: p2.client_update_id.clone(),
                seq: 2,
            }],
            true,
        );
        assert!(a.pending.is_empty());

        // Bob gets the key via a share envelope and replays the log.
        let bob_user = member("bob", &bob, Role::Editor, false);
        let env = share_envelope(&ring, &bob_user.user).unwrap();
        let resp = VaultKeysResponse {
            current_epoch: 1,
            envelopes: vec![env],
            chain: vec![],
            objects: vec![],
            object_chain: vec![],
            shared: None,
        };
        let bring = keyring_from_response(&bob, "bob", &req.id, &resp).unwrap();
        let mut b = VaultReplica::new(&req.id, Role::Editor, bring, VaultMeta::default());
        let ch = ChangesResponse {
            updates: vec![
                Update {
                    object_id: p1.object_id.clone(),
                    seq: 1,
                    author_id: "alice".into(),
                    payload: p1.payload.clone(),
                    created_at: 0,
                    signature: None,
                },
                Update {
                    object_id: p2.object_id.clone(),
                    seq: 2,
                    author_id: "alice".into(),
                    payload: p2.payload.clone(),
                    created_at: 0,
                    signature: None,
                },
            ],
            snapshots: vec![],
            purged: vec![],
            head_seq: 2,
            has_more: false,
            next_since: 2,
        };
        let (applied, errs) = b.apply_changes(&ch);
        assert!(errs.is_empty());
        assert!(applied.index_changed && applied.objects.contains("n1"));
        assert_eq!(b.note_text("n1"), "hello");
        assert_eq!(b.seq, 2);

        // Rotation for remaining members; new writes use epoch 2.
        let members = vec![
            member("alice", &alice, Role::Owner, true),
            member("bob", &bob, Role::Editor, true),
        ];
        let rot = rotate(&mut ring, &members, &VaultMeta::named("Home")).unwrap();
        assert_eq!(rot.new_epoch, 2);
        assert_eq!(rot.envelopes.len(), 2);
        let resp = VaultKeysResponse {
            current_epoch: 2,
            envelopes: vec![
                rot.envelopes
                    .iter()
                    .find(|e| e.user_id == "bob")
                    .unwrap()
                    .clone(),
            ],
            chain: vec![ChainLink {
                epoch: 2,
                link: rot.chain_link.clone(),
            }],
            objects: vec![],
            object_chain: vec![],
            shared: None,
        };
        let bring2 = keyring_from_response(&bob, "bob", &req.id, &resp).unwrap();
        assert_eq!(bring2.epochs().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            VaultMeta::decrypt(&bring2, &rot.encrypted_meta)
                .unwrap()
                .name,
            "Home"
        );

        // Rollback detection.
        assert!(!b.observe_head(5));
        assert!(b.observe_head(3));
    }

    #[test]
    fn note_sharing_keys() {
        let alice = AccountSecret::generate();
        let bob = AccountSecret::generate();
        let carol = AccountSecret::generate();
        let user = |id: &str, a: &AccountSecret| member(id, a, Role::Viewer, true).user;
        let (vreq, vring) = new_vault(&alice, "alice", &VaultMeta::named("Private"), 1).unwrap();
        let mut a = VaultReplica::new(
            &vreq.id,
            Role::Owner,
            vring.clone(),
            VaultMeta::named("Private"),
        );
        a.seq = 3;
        let note = a.notes.entry("n".into()).or_default();
        note.set_text("hello bob").unwrap();
        let state = note.encode_state();
        let (ring, req) =
            start_sharing(&vring, "n", &ObjectMeta::titled("Hello"), &state, a.seq).unwrap();
        a.doc_keys.insert("n".into(), ring.clone());
        let env = object_share_envelope(&ring, "n", &user("bob", &bob)).unwrap();

        // Bob builds his ring from the object keys response and reads the snapshot.
        let resp = ObjectKeysResponse {
            object_id: "n".into(),
            role: Role::Editor,
            current_epoch: req.epoch,
            envelopes: vec![env],
            chain: vec![],
            encrypted_meta: Some(req.encrypted_meta.clone()),
            member_envelopes: vec![],
            shared: None,
        };
        let bring = object_keyring_for_recipient(&bob, "bob", &vreq.id, &resp, None).unwrap();
        assert_eq!(
            ObjectMeta::decrypt(&bring, "n", &req.encrypted_meta)
                .unwrap()
                .title,
            "Hello"
        );
        let mut b = VaultReplica::for_shared_notes(&vreq.id);
        b.role = Role::Editor;
        b.doc_keys.insert("n".into(), bring);
        let snap = req.snapshot.clone().unwrap();
        b.apply_snapshot(&Snapshot {
            id: snap.snapshot_id,
            object_id: "n".into(),
            covers_seq: 3,
            author_id: "alice".into(),
            payload: snap.payload,
            created_at: 0,
            signature: None,
        })
        .unwrap();
        assert_eq!(b.note_text("n"), "hello bob");
        // Bob's edit is encrypted with the document key and Alice can read it.
        let up = b.notes.get("n").unwrap().set_text("hello bob!").unwrap();
        let p = b.local_change("n", up).unwrap();
        assert!(keys::is_doc_epoch(
            crate::crypto::sym_epoch(&p.payload).unwrap()
        ));
        a.apply_update(&Update {
            object_id: "n".into(),
            seq: 4,
            author_id: "bob".into(),
            payload: p.payload.clone(),
            created_at: 0,
            signature: None,
        })
        .unwrap();
        assert_eq!(a.note_text("n"), "hello bob!");
        // Bob cannot read the vault index or other notes.
        let ix = a
            .index
            .create("m", "Secret.md", crate::doc::EntryKind::Note, 1)
            .unwrap();
        let other = a.local_change(INDEX_OBJECT_ID, ix).unwrap();
        assert!(
            b.decrypt_object(Purpose::Update, INDEX_OBJECT_ID, &other.payload)
                .is_err()
        );
        assert!(!b.can_write() || b.compaction_candidates().is_empty());

        // Members get the document key through the vault keys response.
        let vresp = VaultKeysResponse {
            current_epoch: 1,
            envelopes: vec![],
            chain: vec![],
            objects: vec![ObjectKeyEnvelope {
                object_id: "n".into(),
                epoch: req.epoch,
                envelope: req.vault_envelope.clone(),
            }],
            object_chain: vec![],
            shared: None,
        };
        let rings = object_keyrings(&vring, &vresp);
        assert!(
            !rings["n"]
                .decrypt(Purpose::Update, "n", &p.payload)
                .unwrap()
                .is_empty()
        );

        // Rotation for Carol only: Bob's old ring can't read new content.
        let (next, rot) = rotate_object_key(
            &vring,
            &ring,
            "n",
            &ObjectMeta::titled("Hello"),
            &[user("carol", &carol)],
        )
        .unwrap();
        assert_eq!(rot.envelopes.len(), 1);
        a.doc_keys.insert("n".into(), next);
        let up = a
            .notes
            .get("n")
            .unwrap()
            .set_text("only carol now")
            .unwrap();
        let p2 = a.local_change("n", up).unwrap();
        assert!(b.decrypt_object(Purpose::Update, "n", &p2.payload).is_err());
        let resp = ObjectKeysResponse {
            object_id: "n".into(),
            role: Role::Viewer,
            current_epoch: rot.epoch,
            envelopes: rot.envelopes.clone(),
            chain: vec![ChainLink {
                epoch: rot.epoch,
                link: rot.chain_link.clone().unwrap(),
            }],
            encrypted_meta: None,
            member_envelopes: vec![],
            shared: None,
        };
        let cring = object_keyring_for_recipient(&carol, "carol", &vreq.id, &resp, None).unwrap();
        assert_eq!(cring.epochs().count(), 2);
        assert!(cring.decrypt(Purpose::Update, "n", &p.payload).is_ok());
        assert!(cring.decrypt(Purpose::Update, "n", &p2.payload).is_ok());
    }

    #[test]
    fn no_snapshot_covers_content_waiting_for_a_key() {
        let alice = AccountSecret::generate();
        let (_, ring) = new_vault(&alice, "alice", &VaultMeta::named("V"), 1).unwrap();
        let mut a = VaultReplica::new(
            ring.vault_id(),
            Role::Owner,
            ring.clone(),
            VaultMeta::default(),
        );
        let mut b = VaultReplica::new(
            ring.vault_id(),
            Role::Editor,
            ring.clone(),
            VaultMeta::default(),
        );
        let base = a
            .notes
            .entry("n".into())
            .or_default()
            .set_text("one")
            .unwrap();
        let p0 = a.local_change("n", base).unwrap();
        let up = |seq, payload| Update {
            object_id: "n".into(),
            seq,
            author_id: "a".into(),
            payload,
            created_at: 0,
            signature: None,
        };
        b.apply_update(&up(1, p0.payload)).unwrap();
        let (doc, _) = start_sharing(
            &ring,
            "n",
            &ObjectMeta::titled("N"),
            &a.notes["n"].encode_state(),
            1,
        )
        .unwrap();
        a.doc_keys.insert("n".into(), doc.clone());
        let u2 = a.notes.get("n").unwrap().set_text("one two").unwrap();
        let p1 = a.local_change("n", u2).unwrap();
        assert!(b.apply_update(&up(2, p1.payload)).is_err());
        b.since_snapshot.insert("n".into(), COMPACT_AFTER);
        // Bob's state lacks update 2 although his seq counts it.
        assert!(b.snapshot_request("n").is_err());
        assert!(!b.compaction_candidates().contains(&"n".to_string()));
        b.doc_keys.insert("n".into(), doc);
        b.retry_deferred();
        assert!(b.snapshot_request("n").is_ok());
    }

    #[test]
    fn updates_that_cannot_be_kept_are_fetched_again() {
        let alice = AccountSecret::generate();
        let (_, ring) = new_vault(&alice, "alice", &VaultMeta::named("V"), 1).unwrap();
        let mut a = VaultReplica::new(
            ring.vault_id(),
            Role::Owner,
            ring.clone(),
            VaultMeta::default(),
        );
        let mut b = VaultReplica::new(
            ring.vault_id(),
            Role::Editor,
            ring.clone(),
            VaultMeta::default(),
        );
        let (doc, _) = start_sharing(&ring, "n", &ObjectMeta::titled("N"), &[], 0).unwrap();
        a.doc_keys.insert("n".into(), doc);
        let note = a.notes.entry("n".into()).or_default();
        let u = note.set_text("x").unwrap();
        let p = a.local_change("n", u).unwrap();
        let up = |seq: i64| Update {
            object_id: "n".into(),
            seq,
            author_id: "a".into(),
            payload: p.payload.clone(),
            created_at: 0,
            signature: None,
        };
        // Fill the deferral queue, then one more update can't be kept.
        for seq in 1..=MAX_DEFERRED as i64 {
            assert!(b.apply_update(&up(seq)).is_err());
        }
        assert_eq!(b.seq, MAX_DEFERRED as i64);
        let first_dropped = MAX_DEFERRED as i64 + 1;
        assert!(b.apply_update(&up(first_dropped)).is_err());
        assert!(b.apply_update(&up(first_dropped + 1)).is_err());
        // seq stays before the dropped update, also when the server says it's synced.
        b.synced(first_dropped + 10);
        assert_eq!(b.seq, first_dropped - 1);
    }

    #[test]
    fn content_waiting_for_keys_is_replayed() {
        let alice = AccountSecret::generate();
        let (_, ring) = new_vault(&alice, "alice", &VaultMeta::named("V"), 1).unwrap();
        let mut a = VaultReplica::new(
            ring.vault_id(),
            Role::Owner,
            ring.clone(),
            VaultMeta::default(),
        );
        let mut b = VaultReplica::new(
            ring.vault_id(),
            Role::Editor,
            ring.clone(),
            VaultMeta::default(),
        );
        let n = a.notes.entry("n".into()).or_default();
        let base = n.set_text("one").unwrap();
        let p0 = a.local_change("n", base).unwrap();
        b.apply_update(&Update {
            object_id: "n".into(),
            seq: 1,
            author_id: "a".into(),
            payload: p0.payload,
            created_at: 0,
            signature: None,
        })
        .unwrap();
        // Alice gives the note its own key; Bob receives an update before the key.
        let (doc, _) = start_sharing(
            &ring,
            "n",
            &ObjectMeta::titled("N"),
            &a.notes["n"].encode_state(),
            1,
        )
        .unwrap();
        a.doc_keys.insert("n".into(), doc.clone());
        let up = a.notes.get("n").unwrap().set_text("one two").unwrap();
        let p1 = a.local_change("n", up).unwrap();
        let e = b
            .apply_update(&Update {
                object_id: "n".into(),
                seq: 2,
                author_id: "a".into(),
                payload: p1.payload,
                created_at: 0,
                signature: None,
            })
            .unwrap_err();
        assert!(missing_key(&e));
        assert!(b.needs_keys());
        assert_eq!(b.seq, 2);
        b.doc_keys.insert("n".into(), doc);
        let (applied, errs) = b.retry_deferred();
        assert!(errs.is_empty());
        assert!(applied.objects.contains("n"));
        assert_eq!(b.note_text("n"), "one two");
        assert!(!b.needs_keys());
    }

    #[test]
    fn a_device_accepts_only_the_account_it_was_approved_for() {
        let real = AccountSecret::generate();
        let fake = AccountSecret::generate();
        let key = PublicAccountKey::new("u1", 1, &real.public(), &real.sign_account_key("u1", 1));
        let expected = published_account("u1", &key).unwrap();
        assert!(published_account("u2", &key).is_err());
        assert!(check_approved_account(&real, &expected).is_ok());
        // The server could encrypt its own secret to the device's public key.
        assert!(check_approved_account(&fake, &expected).is_err());
    }

    #[test]
    fn pins() {
        let mut p = BTreeMap::new();
        assert_eq!(check_pin(&mut p, "u", "A"), PinStatus::New);
        assert_eq!(check_pin(&mut p, "u", "A"), PinStatus::Match);
        assert_eq!(check_pin(&mut p, "u", "B"), PinStatus::Changed);
    }

    #[test]
    fn bootstrap_is_consistent() {
        let b = bootstrap_account("u1", "Firefox").unwrap();
        let dev = b.request.device.as_ref().unwrap();
        let back = b
            .device
            .unwrap_account(&dev.account_envelope, "u1", &b.device_id)
            .unwrap();
        assert_eq!(back.public(), b.account.public());
        b.request.key.verify().unwrap();
        let rk = RecoveryKey::parse(&b.recovery.to_display()).unwrap();
        assert!(
            rk.unwrap_account(b.request.recovery_envelope.as_ref().unwrap(), "u1")
                .is_ok()
        );
    }
}
