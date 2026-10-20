//! Wire types shared by the server and all clients.
//!
//! JSON over HTTPS for the REST API and JSON text frames for the WebSocket.
//! Binary fields (keys, ciphertexts) are base64 strings. Timestamps are Unix
//! milliseconds. See `docs/protocol.md` for semantics.

use serde::{Deserialize, Serialize};

use crate::b64;
use crate::crypto::{CryptoError, Key32};
use crate::keys::AccountPublic;
pub use crate::memberlog::LogEntry;

pub const API_PREFIX: &str = "/api/v1";
/// Object id of each vault's index document (file tree, paths, trash).
pub const INDEX_OBJECT_ID: &str = "index";
/// Issuer used for identities created by the development login.
pub const DEV_ISSUER: &str = "urn:coolpsonotes:dev";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Editor,
    Viewer,
}

impl Role {
    pub fn can_write(self) -> bool {
        matches!(self, Role::Owner | Role::Editor)
    }

    pub fn can_manage(self) -> bool {
        matches!(self, Role::Owner)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    Human,
    Agent,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::Human => "human",
            PrincipalKind::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// Error body returned by every failing REST call.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub dev_mode: bool,
    pub oidc_enabled: bool,
    pub oidc_display_name: Option<String>,
    pub max_attachment_bytes: u64,
    pub max_update_bytes: u64,
    /// Every note can have its own key (`PUT …/objects/{o}/key` with
    /// `private`); absent on older servers.
    #[serde(default)]
    pub note_keys: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserInfo {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub kind: PrincipalKind,
    pub is_admin: bool,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionResponse {
    pub token: String,
    pub expires_at: i64,
    pub user: UserInfo,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DevLoginRequest {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExchangeRequest {
    pub code: String,
}

/// An account's public keys plus their self-signature.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PublicAccountKey {
    pub user_id: String,
    pub key_version: u32,
    #[serde(with = "b64")]
    pub enc_public: Vec<u8>,
    #[serde(with = "b64")]
    pub sign_public: Vec<u8>,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

impl PublicAccountKey {
    pub fn new(user_id: &str, key_version: u32, public: &AccountPublic, signature: &[u8]) -> Self {
        PublicAccountKey {
            user_id: user_id.to_string(),
            key_version,
            enc_public: public.enc_public.to_vec(),
            sign_public: public.sign_public.to_vec(),
            signature: signature.to_vec(),
        }
    }

    /// Check sizes and the self-signature. Clients must call this before
    /// encrypting anything to a key fetched from the server.
    pub fn verify(&self) -> Result<AccountPublic, CryptoError> {
        let enc_public: Key32 = self
            .enc_public
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::InvalidKey)?;
        let sign_public: Key32 = self
            .sign_public
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::InvalidKey)?;
        let public = AccountPublic {
            enc_public,
            sign_public,
        };
        public.verify(&self.user_id, self.key_version, &self.signature)?;
        Ok(public)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub kind: String,
    pub device_id: Option<String>,
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MeResponse {
    pub user: UserInfo,
    pub account_key: Option<PublicAccountKey>,
    pub has_recovery: bool,
    pub session: SessionInfo,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceRegistration {
    pub device_id: String,
    pub name: String,
    #[serde(with = "b64")]
    pub enc_public: Vec<u8>,
}

/// First-time key setup for an account. Humans also register their first device
/// (already holding the account secret) and a recovery envelope.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountSetupRequest {
    pub key: PublicAccountKey,
    #[serde(default, with = "b64::opt")]
    pub recovery_envelope: Option<Vec<u8>>,
    #[serde(default)]
    pub device: Option<FirstDevice>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FirstDevice {
    pub registration: DeviceRegistration,
    #[serde(with = "b64")]
    pub account_envelope: Vec<u8>,
    #[serde(with = "b64")]
    pub approval_signature: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecoveryEnvelope {
    #[serde(with = "b64")]
    pub recovery_envelope: Vec<u8>,
}

/// Replace the recovery envelope (new recovery key). The signature by the
/// account key proves the caller holds the account secret.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecoveryUpdate {
    #[serde(with = "b64")]
    pub recovery_envelope: Vec<u8>,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
    /// Signed too (`sign_recovery_update_v2`); the server accepts only a
    /// higher one than the last, so older updates can't be replayed. Clients
    /// use the time in ms. Absent: the older signature without a version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionEntry {
    pub id: String,
    pub kind: String,
    pub client_name: Option<String>,
    pub device_id: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: i64,
    pub current: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BindDeviceRequest {
    pub device_id: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    Pending,
    Active,
    Revoked,
}

impl DeviceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceStatus::Pending => "pending",
            DeviceStatus::Active => "active",
            DeviceStatus::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub status: DeviceStatus,
    #[serde(with = "b64")]
    pub enc_public: Vec<u8>,
    pub created_at: i64,
    pub approved_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    /// True for the device bound to the requesting session.
    pub current: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceEnvelope {
    #[serde(with = "b64")]
    pub account_envelope: Vec<u8>,
}

/// Activate a pending device. `signature` is the account's signature over the
/// device approval statement and proves the caller holds the account secret
/// (either an existing device, or the new device after using the recovery key).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ActivateDeviceRequest {
    #[serde(with = "b64")]
    pub account_envelope: Vec<u8>,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RenameRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PublicUser {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub kind: PrincipalKind,
    pub account_key: Option<PublicAccountKey>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KeyEnvelope {
    pub user_id: String,
    pub epoch: u32,
    #[serde(with = "b64")]
    pub envelope: Vec<u8>,
    /// A note's key for someone it is shared with: the vault owner's grant
    /// signature (`memberlog::sign_note_grant`), which they check.
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

/// The owner's grant signature for a key someone already holds.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GrantSignature {
    pub user_id: String,
    pub epoch: u32,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

/// `PUT /vaults/{v}/objects/{o}/keys/signatures` (owner): sign the grants of
/// a note's keys made before grants were signed.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignGrantsRequest {
    pub grants: Vec<GrantSignature>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChainLink {
    pub epoch: u32,
    #[serde(with = "b64")]
    pub link: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VaultInfo {
    pub id: String,
    pub owner_id: String,
    pub role: Role,
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
    pub key_epoch: u32,
    pub head_seq: i64,
    pub archived: bool,
    pub member_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateVaultRequest {
    pub id: String,
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
    pub key_envelope: KeyEnvelope,
    /// The membership log's first entry (`Create`), signed by the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UpdateVaultRequest {
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub encrypted_meta: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VaultKeysResponse {
    pub current_epoch: u32,
    /// The requesting principal's own envelopes, one per epoch it was granted.
    pub envelopes: Vec<KeyEnvelope>,
    pub chain: Vec<ChainLink>,
    /// Document keys of notes, each wrapped under a vault key.
    #[serde(default)]
    pub objects: Vec<ObjectKeyEnvelope>,
    #[serde(default)]
    pub object_chain: Vec<ObjectChainLink>,
    /// Notes currently shared with someone individually. `None` from servers
    /// before per-note keys, where having a document key meant being shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Vec<String>>,
}

/// A note's document key (one epoch) encrypted under a vault key.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectKeyEnvelope {
    pub object_id: String,
    pub epoch: u32,
    #[serde(with = "b64")]
    pub envelope: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectChainLink {
    pub object_id: String,
    pub epoch: u32,
    #[serde(with = "b64")]
    pub link: Vec<u8>,
}

/// Keys of one shared note, for the people it is shared with.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectKeysResponse {
    pub object_id: String,
    pub role: Role,
    pub current_epoch: u32,
    /// The caller's own envelopes (HPKE to their account key).
    pub envelopes: Vec<KeyEnvelope>,
    pub chain: Vec<ChainLink>,
    #[serde(default, with = "b64::opt")]
    pub encrypted_meta: Option<Vec<u8>>,
    /// Vault members: the note's key at every epoch, wrapped under the vault
    /// key (`user_id` empty), so one note's keys can be reloaded alone; `chain`
    /// then has every link. Empty for others and from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_envelopes: Vec<KeyEnvelope>,
    /// Vault members: whether the note is shared with someone individually.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
}

/// A note's document key: its first key, a fresh key to start sharing it,
/// or a rotation (see protocol.md §5).
///
/// * `private: true` — a key only vault members get (every new note gets
///   one; older notes get one lazily). No recipients; any writer may set it.
///   A snapshot is needed only if the note already has content.
/// * `private: false` without `chain_link` — sharing starts: recipients get
///   content from this epoch on, never what was written before (a snapshot
///   of the note's full state under the new key is required).
/// * with `chain_link` — a rotation (owner), e.g. after removing someone.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectKeyUpdate {
    pub epoch: u32,
    /// The new document key wrapped under the vault's current key.
    #[serde(with = "b64")]
    pub vault_envelope: Vec<u8>,
    #[serde(default, with = "b64::opt")]
    pub chain_link: Option<Vec<u8>>,
    /// One envelope per person the note is shared with (exactly the current recipients).
    pub envelopes: Vec<KeyEnvelope>,
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
    #[serde(default)]
    pub snapshot: Option<CreateSnapshotRequest>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

/// A note's key rotated together with its vault's key.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectKeyChange {
    pub object_id: String,
    pub update: ObjectKeyUpdate,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectMetaUpdate {
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AddObjectShareRequest {
    pub user_id: String,
    pub role: Role,
    pub envelope: KeyEnvelope,
    /// `Share`, signed by the owner (required once the vault has a log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectShareInfo {
    pub user: PublicUser,
    pub role: Role,
    pub added_at: i64,
    pub added_by: Option<String>,
    pub has_current_key: bool,
    /// Their key for the current epoch carries the owner's grant signature.
    #[serde(default)]
    pub key_signed: bool,
}

/// A note someone shared with the caller individually.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SharedNote {
    pub vault_id: String,
    pub object_id: String,
    pub role: Role,
    pub owner: PublicUser,
    pub shared_by: Option<String>,
    pub key_epoch: u32,
    #[serde(default, with = "b64::opt")]
    pub encrypted_meta: Option<Vec<u8>>,
    pub added_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectEventKind {
    /// The note started being shared individually.
    Shared,
    KeyRotated,
    MetaChanged,
    SharesChanged,
    /// The caller can no longer access this note.
    AccessRevoked,
    Deleted,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemberInfo {
    pub user: PublicUser,
    pub role: Role,
    /// Whether this member holds an envelope for the current key epoch.
    pub has_current_key: bool,
    pub added_at: i64,
    pub added_by: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AddMemberRequest {
    pub user_id: String,
    pub role: Role,
    #[serde(default)]
    pub key_envelope: Option<KeyEnvelope>,
    /// `Add`, signed by the owner (required once the vault has a log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateMemberRequest {
    pub role: Role,
    /// `Role`, signed by the owner (required once the vault has a log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

/// Optional body of `DELETE /vaults/{v}/members/{u}`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RemoveMemberRequest {
    /// `Remove` (signed by the owner, required once the vault has a log) or
    /// `Leave` (signed by the member; leaving works without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

/// `POST /vaults/{v}/log`: entries that change nothing else on the server
/// (`Checkpoint` to start the log of an existing vault, `Rekey`, `Remove` of
/// someone who already left).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppendLogRequest {
    pub entry: LogEntry,
}

/// `GET /vaults/{v}/log?since=` (entries with `seq >= since`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MemberLogResponse {
    pub entries: Vec<LogEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GrantKeysRequest {
    pub envelopes: Vec<KeyEnvelope>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RotateKeysRequest {
    pub new_epoch: u32,
    #[serde(with = "b64")]
    pub chain_link: Vec<u8>,
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
    pub envelopes: Vec<KeyEnvelope>,
    /// Every note with a document key moves to a new epoch wrapped under the
    /// new vault key, so removed members can't read new content of any note.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectKeyChange>,
    /// `Rotate`, signed by the owner (required once the vault has a log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
}

/// One encrypted CRDT update as stored by the server.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Update {
    pub object_id: String,
    pub seq: i64,
    pub author_id: String,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
    pub created_at: i64,
    /// The author's signature (`memberlog::sign_update`/`sign_snapshot`);
    /// required once the vault's log says so.
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

/// An encrypted, compacted object state covering all updates up to `covers_seq`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub object_id: String,
    pub covers_seq: i64,
    pub author_id: String,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
    pub created_at: i64,
    /// The author's signature (`memberlog::sign_update`/`sign_snapshot`);
    /// required once the vault's log says so.
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SnapshotInfo {
    pub id: String,
    pub object_id: String,
    pub covers_seq: i64,
    pub author_id: String,
    pub created_at: i64,
    pub size: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PurgedObject {
    pub object_id: String,
    pub seq: i64,
}

/// A page of a vault's log after some sequence number. Objects whose latest
/// snapshot covers part of the range are delivered as that snapshot plus the
/// updates after it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChangesResponse {
    pub updates: Vec<Update>,
    pub snapshots: Vec<Snapshot>,
    pub purged: Vec<PurgedObject>,
    pub head_seq: i64,
    pub has_more: bool,
    pub next_since: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ObjectState {
    pub object_id: String,
    pub snapshot: Option<Snapshot>,
    pub updates: Vec<Update>,
    pub head_seq: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PushUpdate {
    pub object_id: String,
    /// Random client-chosen id making retries idempotent.
    pub client_update_id: String,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
    /// The author's signature (`memberlog::sign_update`/`sign_snapshot`);
    /// required once the vault's log says so.
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PushRequest {
    pub updates: Vec<PushUpdate>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PushResult {
    pub client_update_id: String,
    pub seq: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PushResponse {
    pub results: Vec<PushResult>,
    pub head_seq: i64,
    /// Updates of the batch the server refused one by one; the rest was
    /// stored. Older servers refuse the whole batch instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<RejectedUpdate>,
}

/// One update of a push the server refused, and why.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RejectedUpdate {
    pub client_update_id: String,
    pub object_id: String,
    /// [`REJECT_TOO_LARGE`], [`REJECT_PURGED`], [`REJECT_STALE_KEY`],
    /// [`REJECT_UNSIGNED`], [`REJECT_BAD_SIGNATURE`] or `bad_request`.
    pub code: String,
    pub message: String,
}

/// Larger than the server's `max_update_bytes`.
pub const REJECT_TOO_LARGE: &str = "too_large";
/// The object was permanently deleted.
pub const REJECT_PURGED: &str = "purged";
/// Encrypted with an outdated key: reload keys and send it again.
pub const REJECT_STALE_KEY: &str = "stale_key";
/// Not signed, but the vault requires signed content (update the app).
pub const REJECT_UNSIGNED: &str = "unsigned";
/// The signature doesn't match the author's account key.
pub const REJECT_BAD_SIGNATURE: &str = "bad_signature";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateSnapshotRequest {
    pub snapshot_id: String,
    pub covers_seq: i64,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
    /// The author's signature (`memberlog::sign_update`/`sign_snapshot`);
    /// required once the vault's log says so.
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttachmentInfo {
    pub id: String,
    pub size: i64,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrefsBlob {
    #[serde(default, with = "b64::opt")]
    pub ciphertext: Option<Vec<u8>>,
    pub version: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PutPrefsRequest {
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
    pub expected_version: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CredentialInfo {
    pub client_id: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentInfo {
    pub id: String,
    pub display_name: String,
    pub created_at: i64,
    pub account_key: Option<PublicAccountKey>,
    pub credentials: Vec<CredentialInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateAgentRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreatedCredential {
    pub agent_id: String,
    pub client_id: String,
    /// Shown once; only a hash is stored server-side.
    pub client_secret: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateAgentResponse {
    pub agent: AgentInfo,
    pub credential: CreatedCredential,
}

/// RFC 8628 device authorization response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// RFC 6749 token response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(default)]
    pub scope: Option<String>,
}

/// RFC 6749 error response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OAuthError {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceCodeLookup {
    pub user_code: String,
    pub client_name: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceCodeDecision {
    pub user_code: String,
    pub approve: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminUser {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub kind: PrincipalKind,
    pub owner_id: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    pub vault_count: i64,
    pub device_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AdminUpdateUser {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub is_admin: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminStats {
    pub users: i64,
    pub agents: i64,
    pub vaults: i64,
    pub objects: i64,
    pub updates: i64,
    pub snapshots: i64,
    pub attachments: i64,
    pub storage_bytes: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuditEntry {
    pub id: i64,
    pub at: i64,
    pub actor_id: Option<String>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub details: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultEventKind {
    MembersChanged,
    KeyRotated,
    MetaChanged,
    Deleted,
    AccessRevoked,
    RoleChanged,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserEventKind {
    /// A new device of this user is waiting for approval.
    DevicePending,
    /// A device was approved, renamed or revoked.
    DevicesChanged,
    /// The user was added to (or removed from) a vault.
    VaultsChanged,
    /// The session this connection uses was revoked.
    SessionRevoked,
    /// A note was shared with (or unshared from) this user.
    SharesChanged,
}

/// WebSocket messages sent by clients.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Must be the first message; browsers cannot set headers on WebSockets.
    Auth {
        token: String,
    },
    /// Start receiving a vault's updates. With `since`, the server first replays
    /// everything after that sequence number (including snapshots for compacted
    /// ranges) and then sends `synced`. Without it, only live updates are sent.
    /// With `object_id`, only events of that one note are delivered (used by
    /// people a single note is shared with).
    Subscribe {
        vault_id: String,
        since: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object_id: Option<String>,
    },
    Unsubscribe {
        vault_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object_id: Option<String>,
    },
    Push {
        vault_id: String,
        updates: Vec<PushUpdate>,
    },
    /// Ephemeral, encrypted presence data relayed to other subscribers.
    Awareness {
        vault_id: String,
        object_id: String,
        #[serde(with = "b64")]
        payload: Vec<u8>,
    },
    Ping {
        nonce: u64,
    },
}

/// WebSocket messages sent by the server.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        user_id: String,
        session_id: String,
        conn_id: u64,
    },
    Update {
        vault_id: String,
        update: Update,
    },
    Snapshot {
        vault_id: String,
        snapshot: Snapshot,
    },
    SnapshotCreated {
        vault_id: String,
        object_id: String,
        snapshot_id: String,
        covers_seq: i64,
    },
    Purged {
        vault_id: String,
        object_id: String,
        seq: i64,
    },
    /// One page of catch-up after `subscribe` with `since`.
    Changes {
        vault_id: String,
        changes: ChangesResponse,
    },
    Synced {
        vault_id: String,
        head_seq: i64,
        /// Set for per-note subscriptions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object_id: Option<String>,
    },
    Ack {
        vault_id: String,
        results: Vec<PushResult>,
        /// Updates of the push refused one by one (see [`PushResponse::rejected`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rejected: Vec<RejectedUpdate>,
    },
    Awareness {
        vault_id: String,
        object_id: String,
        from_user: String,
        from_conn: u64,
        #[serde(with = "b64")]
        payload: Vec<u8>,
    },
    VaultEvent {
        vault_id: String,
        event: VaultEventKind,
    },
    ObjectEvent {
        vault_id: String,
        object_id: String,
        event: ObjectEventKind,
    },
    UserEvent {
        event: UserEventKind,
    },
    Error {
        code: String,
        message: String,
        vault_id: Option<String>,
    },
    Pong {
        nonce: u64,
    },
}

// ---------------------------------------------------------------- encrypted export

/// Format version of [`EncryptedExport`].
pub const EXPORT_FORMAT: u32 = 1;

/// Everything one person needs to read their notes without the server or
/// the apps: the ciphertexts and key envelopes of what they can access.
/// Nothing in it is readable without their account key (the recovery key or
/// one of their devices). Produced by `GET /me/export` and by
/// `coolpsonotes-server export-user`; decrypted by `coolpsonotes recover`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptedExport {
    pub format: u32,
    pub user_id: String,
    pub created_at: i64,
    /// The account secret encrypted under the recovery key.
    #[serde(default, with = "b64::opt")]
    pub recovery_envelope: Option<Vec<u8>>,
    pub vaults: Vec<ExportedVault>,
    /// Notes shared with this person individually.
    #[serde(default)]
    pub shared_notes: Vec<ExportedSharedNote>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportedVault {
    pub vault_id: String,
    pub role: Role,
    #[serde(with = "b64")]
    pub encrypted_meta: Vec<u8>,
    pub keys: VaultKeysResponse,
    /// The whole log: latest snapshot of every object plus later updates.
    pub changes: ChangesResponse,
    pub attachments: Vec<ExportedAttachment>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportedSharedNote {
    pub vault_id: String,
    pub keys: ObjectKeysResponse,
    pub changes: ChangesResponse,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportedAttachment {
    pub id: String,
    #[serde(with = "b64")]
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_messages_are_tagged() {
        let m = ClientMessage::Push {
            vault_id: "v".into(),
            updates: vec![PushUpdate {
                object_id: "o".into(),
                client_update_id: "c".into(),
                payload: vec![1, 2, 3],
                signature: None,
            }],
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""type":"push""#));
        assert!(s.contains(r#""payload":"AQID""#));
        let back: ClientMessage = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ClientMessage::Push { .. }));
    }
}
