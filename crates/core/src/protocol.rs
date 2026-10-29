//! Wire types shared by the server and all clients.
//!
//! JSON over HTTPS for the REST API and JSON text frames for the WebSocket.
//! Binary fields (keys, ciphertexts) are base64 strings. Timestamps are Unix
//! milliseconds. See `docs/protocol.md` for semantics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::b64;
use crate::crypto::{CryptoError, Key32};
use crate::keys::AccountPublic;
pub use crate::memberlog::LogEntry;

pub const API_PREFIX: &str = "/api/v1";
/// The header every app sends with each request: which app it is and its
/// version ([`app_id`]). Browsers can't set headers on WebSockets, so the
/// `auth` message carries the same text ([`ClientMessage::Auth`]).
pub const APP_HEADER: &str = "notekpr-app";

/// What an app sends in [`APP_HEADER`]: its kind (`web`, `cli`) and the
/// version it was built as, `web/0.3.0`.
pub fn app_id(kind: &str) -> String {
    format!("{kind}/{}", crate::VERSION)
}

/// Object id of each vault's index document (file tree, paths, trash).
pub const INDEX_OBJECT_ID: &str = "index";
/// Prefix of the object holding a note's comments: `c_<note id>`
/// ([`comments_object`]). Note ids never contain `_`, so the two can't meet.
pub const COMMENTS_PREFIX: &str = "c_";

/// The object holding the comments on note `note_id` (docs/comments.md).
pub fn comments_object(note_id: &str) -> String {
    format!("{COMMENTS_PREFIX}{note_id}")
}

/// The note whose comments object `object_id` holds, if it is one.
pub fn comments_note(object_id: &str) -> Option<&str> {
    object_id
        .strip_prefix(COMMENTS_PREFIX)
        .filter(|n| !n.is_empty() && *n != INDEX_OBJECT_ID && !n.starts_with(COMMENTS_PREFIX))
}
/// Issuer used for identities created by the development login.
pub const DEV_ISSUER: &str = "urn:notekpr:dev";

/// A role in a vault or on a single note. In JSON its name: `"editor"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(into = "String", from = "String")]
pub enum Role {
    Owner,
    /// An editor the owner trusts with the vault's tasks
    /// ([`Role::can_administer`]). Only people, not agents; never for
    /// single notes.
    Admin,
    Editor,
    Viewer,
    /// A role from a newer release, kept as written (the membership log's
    /// signatures cover its name). It may do nothing an app would have to
    /// allow: not write, administer or manage (the server enforces what it
    /// really may). Servers refuse it in requests ([`Role::is_known`]).
    Other(String),
}

impl Role {
    pub fn can_write(&self) -> bool {
        matches!(self, Role::Owner | Role::Admin | Role::Editor)
    }

    /// Members, keys and the vault's settings: the owner's alone (the
    /// owner signs the membership log).
    pub fn can_manage(&self) -> bool {
        matches!(self, Role::Owner)
    }

    /// The vault's tasks: emptying the trash, deleting notes and
    /// attachments for good, reading the activity log.
    pub fn can_administer(&self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
            Role::Other(name) => name,
        }
    }

    /// One of the roles this release knows (never [`Role::Other`]).
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Role::Other(_))
    }
}

impl From<String> for Role {
    fn from(s: String) -> Role {
        Role::parse(&s).unwrap_or(Role::Other(s))
    }
}

impl From<Role> for String {
    fn from(r: Role) -> String {
        match r {
            Role::Other(name) => name,
            known => known.as_str().to_string(),
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
    /// With [`ERROR_UPDATE_REQUIRED`]: the oldest version of the apps the
    /// server accepts (`0.6.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_version: Option<String>,
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
    /// Largest snapshot (a note's full state) the server stores; absent on
    /// older servers (then `max(16 × max_update_bytes, 32 MiB)`).
    #[serde(default)]
    pub max_snapshot_bytes: u64,
    /// Every note can have its own key (`PUT …/objects/{o}/key` with
    /// `private`); absent on older servers.
    #[serde(default)]
    pub note_keys: bool,
    /// A demo (development mode only): when everything on it will be
    /// deleted, in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demo_resets_at: Option<i64>,
    /// Set while the server refuses changes (see [`ServerMessage::Mode`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<ReadOnly>,
    /// Days during which deleting something for good can be undone: the
    /// server keeps it that long. 0: at once gone (and older servers).
    #[serde(default)]
    pub purge_undo_days: u32,
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

/// What a vault is for: set when it is created, never changed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum VaultKind {
    #[default]
    Regular,
    /// A person's quick notes: one per account, never shared (no members,
    /// note shares, agents or comments), at most
    /// [`VaultInfo::note_limit`] notes. Apps keep it flat and out of their
    /// vault lists.
    QuickNotes,
    /// A kind from a newer release: apps don't show such a vault (servers
    /// refuse to make one).
    #[serde(other)]
    Other,
}

impl VaultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VaultKind::Regular => "regular",
            VaultKind::QuickNotes => "quick_notes",
            VaultKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "regular" => Some(VaultKind::Regular),
            "quick_notes" => Some(VaultKind::QuickNotes),
            _ => None,
        }
    }

    pub fn is_regular(&self) -> bool {
        *self == VaultKind::Regular
    }

    /// A section of one's own (the quick notes, in Personal storage): one
    /// per person, never shared, its name and settings fixed, counted as no
    /// vault. Apps show it apart from their vaults.
    pub fn is_section(&self) -> bool {
        matches!(self, VaultKind::QuickNotes)
    }
}

/// What an object of a vault is, as far as the server needs to know to
/// count it: a drawing, or anything else (a note; the index and comments
/// too). Set by what creates the object (its first key, or the first
/// update pushed), never changed afterwards. The server never learns more:
/// paths, titles and content stay encrypted.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    #[default]
    Note,
    Drawing,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Note => "note",
            ObjectKind::Drawing => "drawing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "note" => Some(ObjectKind::Note),
            "drawing" => Some(ObjectKind::Drawing),
            _ => None,
        }
    }

    pub fn is_note(&self) -> bool {
        *self == ObjectKind::Note
    }
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
    /// Viewers may comment on notes (the owner's setting); absent on older
    /// servers, which have no comments.
    #[serde(default)]
    pub viewers_comment: bool,
    /// What the vault is for; absent on older servers (regular).
    #[serde(default)]
    pub kind: VaultKind,
    /// Notes the vault may hold: for quick notes the owner's allowance
    /// (`quick_notes_per_user` unless an administrator set their own),
    /// unlimited for other vaults.
    #[serde(default)]
    pub note_limit: Limit,
    /// Drawings the vault may hold, whoever makes them: its own allowance,
    /// set by an administrator, else the instance's `drawings_per_vault`;
    /// unlimited from older servers. Those in the trash count until deleted
    /// for good.
    #[serde(default)]
    pub drawing_limit: Limit,
    /// Bytes the vault's encrypted content takes on the server: its log,
    /// history and files, not what was deleted for good.
    #[serde(default)]
    pub storage_bytes: u64,
    /// The vault's storage limit (MiB): its own, set by an administrator,
    /// else the instance's `storage_per_vault_mb`; for quick notes, their
    /// owner's own ([`AdminUser::quick_notes_storage_allowance`]), else
    /// `quick_notes_storage_mb`. At it, the vault takes no
    /// new content ([`ERROR_VAULT_FULL`]) until something is deleted for
    /// good or the limit is raised.
    #[serde(default)]
    pub storage_limit_mb: Limit,
    /// For the owner: someone who no longer has access still knows the
    /// vault's key (they left, or their agent was deleted). The owner's apps
    /// rotate it as soon as they can.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rotation_pending: bool,
    /// For the owner: notes whose key someone who no longer has access to
    /// them still knows (they removed the note from their list, or it was
    /// deleted for good and brought back); rotated the same way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes_rotation_pending: Vec<String>,
}

impl VaultInfo {
    pub fn is_quick_notes(&self) -> bool {
        self.kind == VaultKind::QuickNotes
    }

    /// Whether the vault reached its storage limit (when the list was
    /// loaded; [`ServerMessage::VaultFull`] says when that changes).
    pub fn is_full(&self) -> bool {
        self.storage_limit_mb.reached_mb(self.storage_bytes)
    }

    /// Whether these are `user_id`'s own quick notes: of that kind, and
    /// theirs alone. Apps take nothing else for them, so a vault someone
    /// else owns or shares never passes for one's quick notes.
    pub fn is_own_quick_notes(&self, user_id: &str) -> bool {
        self.is_quick_notes() && self.is_own(user_id)
    }

    /// Owned by `user_id` and shared with nobody.
    fn is_own(&self, user_id: &str) -> bool {
        self.owner_id == user_id && self.role == Role::Owner && self.member_count <= 1
    }
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
    /// `quick_notes`: the account's quick-notes vault (at most one).
    #[serde(default, skip_serializing_if = "VaultKind::is_regular")]
    pub kind: VaultKind,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UpdateVaultRequest {
    #[serde(default, with = "b64::opt", skip_serializing_if = "Option::is_none")]
    pub encrypted_meta: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// Let viewers (and people notes are shared with read-only) comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewers_comment: Option<bool>,
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
    /// For those notes: the first document key epoch the people they're
    /// shared with have (content under earlier keys isn't theirs to read).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub share_from: BTreeMap<String, u32>,
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
    /// People the note is shared with who are deliberately left without the
    /// new key: their account key changed and the owner hasn't verified it.
    /// They stay recipients; sharing the note with them again gives it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without: Vec<String>,
    /// What the object is, when this key is the first thing stored of it
    /// (ignored afterwards: an object's kind never changes).
    #[serde(default, skip_serializing_if = "ObjectKind::is_note")]
    pub kind: ObjectKind,
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
    /// The caller may comment on it (editors always; viewers while the
    /// vault's owner lets them). Absent on older servers, which have no
    /// comments.
    #[serde(default)]
    pub can_comment: bool,
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
    /// An event from a newer release: apps reload what they know of the note.
    #[serde(other)]
    Other,
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

/// Body of `DELETE /vaults/{v}/members/{u}` (optional when leaving).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RemoveMemberRequest {
    /// `Remove` (signed by the owner, required once the vault has a log) or
    /// `Leave` (signed by the member; leaving works without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
    /// The owner's removal carries the vault's new key, for the members who
    /// stay (its `Rotate` entry signed right after the `Remove`): both happen
    /// together, so nothing is ever written under a key the removed person
    /// knows. Required when the owner removes someone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<RotateKeysRequest>,
}

/// Body of `DELETE /vaults/{v}/objects/{o}/shares/{u}` (optional when
/// removing a note from one's own list).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RemoveShareRequest {
    /// `Unshare`, signed by the owner (required once the vault has a log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntry>,
    /// The note's new key for the people it stays shared with (a rotation),
    /// stored together with the removal. Required when the owner stops
    /// sharing a note with someone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<ObjectKeyUpdate>,
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
    /// Members deliberately left without the new key: their account key
    /// changed and the owner hasn't verified it. They stay members and get
    /// the key once verified (`POST /vaults/{v}/keys/grant`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without: Vec<String>,
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

/// A deletion for good that was undone: the object is back, with
/// everything it had. Clients that dropped it fetch its changes again
/// (`GET …/objects/{o}/changes`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RestoredObject {
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
    /// Absent from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<RestoredObject>,
    pub head_seq: i64,
    pub has_more: bool,
    pub next_since: i64,
}

/// Something of a vault deleted for good that can still be undone
/// (`GET /vaults/{v}/purged`): the server keeps it until `kept_until`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PurgedItem {
    /// The note's object id, or the attachment's id.
    pub id: String,
    #[serde(default)]
    pub attachment: bool,
    pub purged_at: i64,
    pub kept_until: i64,
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
    /// What the object is, when this update creates it (ignored afterwards).
    #[serde(default, skip_serializing_if = "ObjectKind::is_note")]
    pub kind: ObjectKind,
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
    /// [`REJECT_UNSIGNED`], [`REJECT_BAD_SIGNATURE`],
    /// [`REJECT_QUICK_NOTES_FULL`], [`REJECT_DRAWINGS_FULL`] or
    /// `bad_request`.
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
/// A new note in the quick-notes vault while it holds as many notes as it
/// may ([`VaultInfo::note_limit`]): kept, and sent again once there's room.
pub const REJECT_QUICK_NOTES_FULL: &str = "quick_notes_full";
/// A new drawing in a vault holding as many drawings as it may
/// ([`VaultInfo::drawing_limit`]): kept, and sent again once there's room.
pub const REJECT_DRAWINGS_FULL: &str = "drawings_full";
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
    /// It carries changes the log doesn't have: an update too large to push
    /// on its own. The server sends it to everyone subscribed and with every
    /// catch-up from before it (protocol §6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub carries: bool,
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
    /// The address the code was requested from, as the server saw it
    /// (absent from older servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_from: Option<String>,
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
    /// Vaults this account is a member of.
    pub vault_count: i64,
    pub device_count: i64,
    /// Vaults this person owns (what their vault allowance counts).
    #[serde(default)]
    pub owned_vault_count: i64,
    /// Agents this person has.
    #[serde(default)]
    pub agent_count: i64,
    /// Quick notes this person keeps (what their quick-note allowance counts).
    #[serde(default)]
    pub quick_note_count: i64,
    /// Bytes the vaults this person owns take ([`VaultInfo::storage_bytes`]),
    /// their quick notes apart.
    #[serde(default)]
    pub storage_bytes: u64,
    /// Bytes their quick notes take.
    #[serde(default)]
    pub quick_notes_storage_bytes: u64,
    #[serde(default)]
    pub vault_allowance: Allowance,
    #[serde(default)]
    pub agent_allowance: Allowance,
    #[serde(default)]
    pub quick_note_allowance: Allowance,
    /// Their quick notes' storage limit (MiB), else the instance's
    /// `quick_notes_storage_mb`.
    #[serde(default)]
    pub quick_notes_storage_allowance: Allowance,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AdminUpdateUser {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub is_admin: Option<bool>,
    /// How many vaults this person may own (absent: unchanged).
    #[serde(default)]
    pub vault_allowance: Option<Allowance>,
    /// How many agents this person may have (absent: unchanged).
    #[serde(default)]
    pub agent_allowance: Option<Allowance>,
    /// How many quick notes this person may keep (absent: unchanged).
    #[serde(default)]
    pub quick_note_allowance: Option<Allowance>,
    /// How much their quick notes may take, in MiB (absent: unchanged).
    #[serde(default)]
    pub quick_notes_storage_allowance: Option<Allowance>,
}

/// A vault as administrators see it (`GET /admin/vaults`): its id, owner and
/// size — never its name, members or content. People find a vault's id in
/// its settings, to ask for more room. Someone's quick notes aren't among
/// them: their limit is the person's ([`AdminUser`]).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AdminVault {
    pub id: String,
    pub owner_id: String,
    pub owner_name: String,
    /// Bytes its encrypted content takes ([`VaultInfo::storage_bytes`]).
    pub storage_bytes: u64,
    /// Its own storage limit (MiB), else the instance's `storage_per_vault_mb`.
    #[serde(default)]
    pub storage_allowance: Allowance,
    /// Drawings it holds (not deleted for good, the trash's too).
    #[serde(default)]
    pub drawing_count: u64,
    /// How many drawings it may hold, else the instance's
    /// `drawings_per_vault`.
    #[serde(default)]
    pub drawing_allowance: Allowance,
    pub created_at: i64,
}

/// `PATCH /admin/vaults/{id}`: only the fields given change.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AdminUpdateVault {
    /// Its own storage limit in MiB (absent: unchanged; `"default"`: the
    /// instance's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_allowance: Option<Allowance>,
    /// How many drawings it may hold (absent: unchanged; `"default"`: the
    /// instance's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawing_allowance: Option<Allowance>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminStats {
    /// The server's version ([`crate::VERSION`]); absent from older servers.
    #[serde(default)]
    pub version: String,
    pub users: i64,
    pub agents: i64,
    pub vaults: i64,
    pub objects: i64,
    pub updates: i64,
    pub snapshots: i64,
    pub attachments: i64,
    /// Bytes the database uses (freed pages not counted): what
    /// [`InstanceSettings::max_storage_mb`] is compared with.
    pub storage_bytes: i64,
}

/// A limit on how many of something there may be: a number, or none. In
/// JSON a number or `"unlimited"`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(into = "LimitRepr", try_from = "LimitRepr")]
pub enum Limit {
    #[default]
    Unlimited,
    Max(u64),
}

impl Limit {
    /// Whether one more fits when there are `count` already.
    pub fn allows_one_more(self, count: u64) -> bool {
        match self {
            Limit::Unlimited => true,
            Limit::Max(n) => count < n,
        }
    }

    pub fn max(self) -> Option<u64> {
        match self {
            Limit::Unlimited => None,
            Limit::Max(n) => Some(n),
        }
    }

    /// For a limit in MiB: whether `bytes` reached it.
    pub fn reached_mb(self, bytes: u64) -> bool {
        match self {
            Limit::Unlimited => false,
            Limit::Max(mb) => bytes >= mb.saturating_mul(MIB),
        }
    }
}

/// Storage limits are in MiB.
pub const MIB: u64 = 1024 * 1024;

/// A person's or vault's own limit (vaults, agents, quick notes, storage):
/// the instance's default for everyone, or theirs. In JSON `"default"`,
/// `"unlimited"` or a number.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(into = "LimitRepr", from = "LimitRepr")]
pub enum Allowance {
    #[default]
    Default,
    Unlimited,
    Max(u64),
}

impl Allowance {
    /// The limit that applies, given the instance's default.
    pub fn resolve(self, default: Limit) -> Limit {
        match self {
            Allowance::Default => default,
            Allowance::Unlimited => Limit::Unlimited,
            Allowance::Max(n) => Limit::Max(n),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(untagged)]
enum LimitRepr {
    Max(u64),
    Word(LimitWord),
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum LimitWord {
    Unlimited,
    Default,
}

impl From<Limit> for LimitRepr {
    fn from(l: Limit) -> Self {
        match l {
            Limit::Unlimited => LimitRepr::Word(LimitWord::Unlimited),
            Limit::Max(n) => LimitRepr::Max(n),
        }
    }
}

impl TryFrom<LimitRepr> for Limit {
    type Error = &'static str;

    fn try_from(r: LimitRepr) -> Result<Self, Self::Error> {
        match r {
            LimitRepr::Max(n) => Ok(Limit::Max(n)),
            LimitRepr::Word(LimitWord::Unlimited) => Ok(Limit::Unlimited),
            LimitRepr::Word(LimitWord::Default) => Err("expected a number or \"unlimited\""),
        }
    }
}

impl From<Allowance> for LimitRepr {
    fn from(a: Allowance) -> Self {
        match a {
            Allowance::Default => LimitRepr::Word(LimitWord::Default),
            Allowance::Unlimited => LimitRepr::Word(LimitWord::Unlimited),
            Allowance::Max(n) => LimitRepr::Max(n),
        }
    }
}

impl From<LimitRepr> for Allowance {
    fn from(r: LimitRepr) -> Self {
        match r {
            LimitRepr::Max(n) => Allowance::Max(n),
            LimitRepr::Word(LimitWord::Unlimited) => Allowance::Unlimited,
            LimitRepr::Word(LimitWord::Default) => Allowance::Default,
        }
    }
}

/// Agents a person may have unless an administrator says otherwise.
pub const DEFAULT_AGENTS_PER_USER: u64 = 50;

fn default_agents_per_user() -> Limit {
    Limit::Max(DEFAULT_AGENTS_PER_USER)
}

/// Quick notes a person may keep unless an administrator says otherwise.
pub const DEFAULT_QUICK_NOTES_PER_USER: u64 = 20;

fn default_quick_notes_per_user() -> Limit {
    Limit::Max(DEFAULT_QUICK_NOTES_PER_USER)
}

/// Settings an instance's administrators change while it runs
/// (`GET`/`PATCH /admin/settings`). Limits never take anything away: below
/// them nothing changes, at them nothing more can be created.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstanceSettings {
    /// Accounts of people that aren't disabled (agents don't count).
    #[serde(default)]
    pub max_users: Limit,
    /// Agents on the whole server.
    #[serde(default)]
    pub max_agents: Limit,
    /// Vaults on the whole server.
    #[serde(default)]
    pub max_vaults: Limit,
    /// Vaults a person may own, unless they have their own allowance.
    /// Vaults shared with them don't count.
    #[serde(default)]
    pub vaults_per_user: Limit,
    /// Agents a person may have, unless they have their own allowance.
    #[serde(default = "default_agents_per_user")]
    pub agents_per_user: Limit,
    /// Quick notes a person may keep, unless they have their own allowance.
    /// Lowering it takes none away: only new ones wait for room.
    #[serde(default = "default_quick_notes_per_user")]
    pub quick_notes_per_user: Limit,
    /// Drawings a vault may hold, unless it has its own allowance
    /// ([`AdminVault::drawing_allowance`]). Lowering it takes none away:
    /// only new ones wait for room.
    #[serde(default)]
    pub drawings_per_vault: Limit,
    /// The server refuses every change; clients keep theirs and send them
    /// when it's writable again.
    #[serde(default)]
    pub read_only: bool,
    /// The database size (MiB) at which the server turns read-only by
    /// itself, until it is below it again (e.g. the limit was raised).
    #[serde(default)]
    pub max_storage_mb: Limit,
    /// The size (MiB) at which a vault takes no new content, unless it has
    /// its own limit ([`AdminVault::storage_allowance`]). What was deleted
    /// for good doesn't count, so emptying the trash makes room.
    #[serde(default)]
    pub storage_per_vault_mb: Limit,
    /// The size (MiB) at which someone's quick notes take no new content,
    /// unless they have their own allowance
    /// ([`AdminUser::quick_notes_storage_allowance`]). Quick notes have their
    /// own limit, not `storage_per_vault_mb`: like how many they may keep,
    /// it is the person's.
    #[serde(default)]
    pub quick_notes_storage_mb: Limit,
    /// The oldest version of the apps the server accepts (`0.6.0`), above
    /// what its build accepts ([`AdminSettings::build_oldest_app_version`]);
    /// empty: only that. Older apps are told to update
    /// ([`ERROR_UPDATE_REQUIRED`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub oldest_app_version: String,
}

impl Default for InstanceSettings {
    fn default() -> Self {
        InstanceSettings {
            max_users: Limit::Unlimited,
            max_agents: Limit::Unlimited,
            max_vaults: Limit::Unlimited,
            vaults_per_user: Limit::Unlimited,
            agents_per_user: default_agents_per_user(),
            quick_notes_per_user: default_quick_notes_per_user(),
            drawings_per_vault: Limit::Unlimited,
            read_only: false,
            max_storage_mb: Limit::Unlimited,
            storage_per_vault_mb: Limit::Unlimited,
            quick_notes_storage_mb: Limit::Unlimited,
            oldest_app_version: String::new(),
        }
    }
}

/// `PATCH /admin/settings`: only the fields given change.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct InstanceSettingsUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_users: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agents: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_vaults: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vaults_per_user: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_per_user: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_notes_per_user: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawings_per_vault: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_storage_mb: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_per_vault_mb: Option<Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_notes_storage_mb: Option<Limit>,
    /// `""`: only what the server's build accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_app_version: Option<String>,
}

impl InstanceSettings {
    /// The storage limit (MiB) of a vault of `kind` that has none of its own:
    /// quick notes have theirs.
    pub fn storage_default(&self, kind: VaultKind) -> Limit {
        match kind {
            VaultKind::Regular | VaultKind::Other => self.storage_per_vault_mb,
            VaultKind::QuickNotes => self.quick_notes_storage_mb,
        }
    }

    pub fn apply(&mut self, u: &InstanceSettingsUpdate) {
        let InstanceSettingsUpdate {
            max_users,
            max_agents,
            max_vaults,
            vaults_per_user,
            agents_per_user,
            quick_notes_per_user,
            drawings_per_vault,
            read_only,
            max_storage_mb,
            storage_per_vault_mb,
            quick_notes_storage_mb,
            oldest_app_version,
        } = u.clone();
        self.max_users = max_users.unwrap_or(self.max_users);
        self.max_agents = max_agents.unwrap_or(self.max_agents);
        self.max_vaults = max_vaults.unwrap_or(self.max_vaults);
        self.vaults_per_user = vaults_per_user.unwrap_or(self.vaults_per_user);
        self.agents_per_user = agents_per_user.unwrap_or(self.agents_per_user);
        self.quick_notes_per_user = quick_notes_per_user.unwrap_or(self.quick_notes_per_user);
        self.drawings_per_vault = drawings_per_vault.unwrap_or(self.drawings_per_vault);
        self.read_only = read_only.unwrap_or(self.read_only);
        self.max_storage_mb = max_storage_mb.unwrap_or(self.max_storage_mb);
        self.storage_per_vault_mb = storage_per_vault_mb.unwrap_or(self.storage_per_vault_mb);
        self.quick_notes_storage_mb = quick_notes_storage_mb.unwrap_or(self.quick_notes_storage_mb);
        if let Some(v) = oldest_app_version {
            self.oldest_app_version = v.trim().to_string();
        }
    }
}

/// Why the server refuses changes right now.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnly {
    /// An administrator switched read-only mode on.
    Manual,
    /// The database reached the size limit (`max_storage_mb`).
    StorageFull,
    /// A reason from a newer release: read-only all the same.
    #[serde(other)]
    Other,
}

/// `GET`/`PATCH /admin/settings`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdminSettings {
    pub settings: InstanceSettings,
    /// Whether, and why, the server refuses changes now.
    #[serde(default)]
    pub read_only: Option<ReadOnly>,
    /// Bytes the database uses now.
    #[serde(default)]
    pub storage_bytes: u64,
    /// The oldest version of the apps the server's build accepts (`0.0.0`:
    /// any); [`InstanceSettings::oldest_app_version`] can only raise it.
    #[serde(default)]
    pub build_oldest_app_version: String,
}

/// The error code of everything refused while the server is read-only
/// (HTTP 503; the WebSocket's `error`). Clients keep the change and send it
/// once the server says it's writable again ([`ServerMessage::Mode`]).
pub const ERROR_READ_ONLY: &str = "read_only";
/// The error code of a creation refused by an instance limit (HTTP 403):
/// accounts, vaults, agents, quick notes, drawings.
pub const ERROR_LIMIT_REACHED: &str = "limit_reached";
/// The error code of new content refused because its vault reached its
/// storage limit (HTTP 507; the WebSocket's `error` with the vault's id).
/// Clients keep the change and send it once a [`ServerMessage::VaultFull`]
/// says the vault has room again.
pub const ERROR_VAULT_FULL: &str = "vault_full";
/// The error code of everything an app too old for the server asks (HTTP
/// 426; the WebSocket's `error` before closing): it says so and keeps its
/// unsent changes. [`ApiError::oldest_version`] says which version it needs.
pub const ERROR_UPDATE_REQUIRED: &str = "update_required";

/// What an admin client (or a person with the admin role) may do on the
/// administration API. Every admin client can `read`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AdminScope {
    /// Accounts, usage statistics, the instance settings, admin clients.
    Read,
    /// Change the instance settings (limits, read-only mode).
    Settings,
    /// Disable and enable accounts, set their allowances and vaults'
    /// storage limits.
    Accounts,
    /// Give and take the admin role.
    Admins,
    /// Create, change and delete admin clients and their tokens — never
    /// with a scope the one doing it doesn't have.
    Clients,
}

impl AdminScope {
    pub const ALL: [AdminScope; 5] = [
        AdminScope::Read,
        AdminScope::Settings,
        AdminScope::Accounts,
        AdminScope::Admins,
        AdminScope::Clients,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AdminScope::Read => "read",
            AdminScope::Settings => "settings",
            AdminScope::Accounts => "accounts",
            AdminScope::Admins => "admins",
            AdminScope::Clients => "clients",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        AdminScope::ALL.into_iter().find(|a| a.as_str() == s)
    }
}

/// Admin client tokens start with this, so the server tells them from
/// session tokens (which no administration-only route accepts, and the
/// other way round).
pub const ADMIN_TOKEN_PREFIX: &str = "notekpr_a_";

/// A script or service administering the instance (`/admin/clients`). It is
/// no account: it has no keys, sessions or memberships and can't reach any
/// vault.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AdminClientInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<AdminScope>,
    pub created_at: i64,
    /// Its scopes and tokens come from the server's configuration
    /// (`notekpr-server admin-client ensure`), which puts them back each
    /// time it runs.
    #[serde(default)]
    pub managed: bool,
    pub tokens: Vec<AdminTokenInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AdminTokenInfo {
    pub id: String,
    /// The token's last characters, to tell tokens apart.
    pub hint: String,
    pub created_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

/// `POST /admin/clients`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateAdminClient {
    pub name: String,
    pub scopes: Vec<AdminScope>,
}

/// `PATCH /admin/clients/{id}`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UpdateAdminClient {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<AdminScope>>,
}

/// A new token, shown only in this answer (the server keeps its hash).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreatedAdminToken {
    pub client: AdminClientInfo,
    pub token: String,
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
    /// An event from a newer release: apps reload the vault.
    #[serde(other)]
    Other,
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
    /// An event from a newer release: apps reload their lists (vaults,
    /// shared notes, devices).
    #[serde(other)]
    Other,
}

/// The WebSocket close code a server sends when it shuts down to be
/// started again (an update): "service restart" (RFC 6455 registry). Clients
/// reconnect within a second or so, again and again for a while, instead of
/// backing off as after a failure.
pub const CLOSE_RESTARTING: u16 = 1012;

/// WebSocket messages sent by clients.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Must be the first message; browsers cannot set headers on WebSockets.
    Auth {
        token: String,
        /// The app and its version, as in [`APP_HEADER`]; absent from
        /// older apps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
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
    /// Deleting `object_id` for good was undone ([`RestoredObject`]).
    Restored {
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
    /// Whether the server accepts changes: sent after `ready` and whenever
    /// it changes. While `read_only` is set, clients keep their changes
    /// (pushes are refused with [`ERROR_READ_ONLY`]) and send them as soon
    /// as a `mode` without it arrives.
    Mode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<ReadOnly>,
    },
    /// Whether a vault reached its storage limit: sent when it turns full or
    /// has room again, and on subscribing to a full vault (before `synced`).
    /// While it is full, clients keep its changes (pushes are refused with
    /// [`ERROR_VAULT_FULL`]) and send them once it has room. Goes to
    /// people a single note of it is shared with too.
    VaultFull {
        vault_id: String,
        full: bool,
    },
}

// ---------------------------------------------------------------- encrypted export

/// Format version of [`EncryptedExport`].
pub const EXPORT_FORMAT: u32 = 1;

/// Everything one person needs to read their notes without the server or
/// the apps: the ciphertexts and key envelopes of what they can access.
/// Nothing in it is readable without their account key (the recovery key or
/// one of their devices). Produced by `GET /me/export` and by
/// `notekpr-server export-user`; decrypted by `notekpr recover`.
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
    /// The names of the people in it (the vaults' members and whoever wrote
    /// something in it), to say who wrote each comment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<ExportedUser>,
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
    /// The vault's owner (who may delete anyone's comments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportedSharedNote {
    pub vault_id: String,
    pub keys: ObjectKeysResponse,
    pub changes: ChangesResponse,
    /// The owner of the note's vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExportedUser {
    pub id: String,
    pub display_name: String,
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
                kind: ObjectKind::Note,
            }],
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""type":"push""#));
        assert!(s.contains(r#""payload":"AQID""#));
        let back: ClientMessage = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ClientMessage::Push { .. }));
    }

    #[test]
    fn apps_say_which_version_they_are() {
        assert!(!crate::VERSION.is_empty());
        assert_eq!(app_id("web"), format!("web/{}", crate::VERSION));
        let m = ClientMessage::Auth {
            token: "t".into(),
            app: Some(app_id("cli")),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(&format!(r#""app":"cli/{}""#, crate::VERSION)));
        // Older apps authenticate without it.
        let old: ClientMessage = serde_json::from_str(r#"{"type":"auth","token":"t"}"#).unwrap();
        assert!(matches!(old, ClientMessage::Auth { app: None, .. }));
    }

    #[test]
    fn roles_read_as_they_are_written() {
        for r in [Role::Owner, Role::Admin, Role::Editor, Role::Viewer] {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.as_str()));
            assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), r);
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        // Admins write and run the vault's tasks; only the owner manages it.
        assert!(Role::Admin.can_write() && Role::Admin.can_administer());
        assert!(!Role::Admin.can_manage());
        assert!(Role::Editor.can_write() && !Role::Editor.can_administer());
        assert!(Role::Owner.can_administer() && Role::Owner.can_manage());
    }

    #[test]
    fn roles_from_newer_releases_are_kept_and_may_do_nothing() {
        let r: Role = serde_json::from_str(r#""commenter""#).unwrap();
        assert_eq!(r, Role::Other("commenter".into()));
        assert!(!r.is_known() && !r.can_write() && !r.can_administer() && !r.can_manage());
        assert_eq!(r.as_str(), "commenter");
        assert_eq!(serde_json::to_string(&r).unwrap(), r#""commenter""#);
        assert_eq!(Role::parse("commenter"), None);
        // A response naming one still reads.
        let m = MemberInfo {
            user: PublicUser {
                id: "u".into(),
                display_name: "U".into(),
                email: None,
                kind: PrincipalKind::Human,
                account_key: None,
            },
            role: Role::Editor,
            has_current_key: true,
            added_at: 0,
            added_by: None,
        };
        let json = serde_json::to_string(&m)
            .unwrap()
            .replace("editor", "commenter");
        let m: MemberInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(m.role.as_str(), "commenter");
    }

    #[test]
    fn reasons_and_events_from_newer_releases_still_read() {
        let mode: ServerMessage =
            serde_json::from_str(r#"{"type":"mode","read_only":"maintenance"}"#).unwrap();
        assert!(matches!(
            mode,
            ServerMessage::Mode {
                read_only: Some(ReadOnly::Other)
            }
        ));
        let m: ServerMessage =
            serde_json::from_str(r#"{"type":"vault_event","vault_id":"v","event":"archived"}"#)
                .unwrap();
        assert!(matches!(
            m,
            ServerMessage::VaultEvent {
                event: VaultEventKind::Other,
                ..
            }
        ));
        let m: ServerMessage = serde_json::from_str(
            r#"{"type":"object_event","vault_id":"v","object_id":"n","event":"locked"}"#,
        )
        .unwrap();
        assert!(matches!(
            m,
            ServerMessage::ObjectEvent {
                event: ObjectEventKind::Other,
                ..
            }
        ));
        let m: ServerMessage =
            serde_json::from_str(r#"{"type":"user_event","event":"moved"}"#).unwrap();
        assert!(matches!(
            m,
            ServerMessage::UserEvent {
                event: UserEventKind::Other
            }
        ));
        // Known ones read as before.
        let m: ServerMessage =
            serde_json::from_str(r#"{"type":"mode","read_only":"storage_full"}"#).unwrap();
        assert!(matches!(
            m,
            ServerMessage::Mode {
                read_only: Some(ReadOnly::StorageFull)
            }
        ));
    }

    #[test]
    fn limits_are_numbers_or_words() {
        let json = |v: &str| serde_json::from_str::<serde_json::Value>(v).unwrap();
        assert_eq!(serde_json::to_value(Limit::Max(3)).unwrap(), json("3"));
        assert_eq!(
            serde_json::to_value(Limit::Unlimited).unwrap(),
            json(r#""unlimited""#)
        );
        assert_eq!(serde_json::from_str::<Limit>("0").unwrap(), Limit::Max(0));
        assert_eq!(
            serde_json::from_str::<Limit>(r#""unlimited""#).unwrap(),
            Limit::Unlimited
        );
        // A setting has no default to fall back on; negative numbers aren't limits.
        assert!(serde_json::from_str::<Limit>(r#""default""#).is_err());
        assert!(serde_json::from_str::<Limit>("-1").is_err());
        assert!(serde_json::from_str::<Limit>("null").is_err());

        for (a, s) in [
            (Allowance::Default, r#""default""#),
            (Allowance::Unlimited, r#""unlimited""#),
            (Allowance::Max(2), "2"),
        ] {
            assert_eq!(serde_json::to_value(a).unwrap(), json(s));
            assert_eq!(serde_json::from_str::<Allowance>(s).unwrap(), a);
        }
        assert_eq!(Allowance::Default.resolve(Limit::Max(1)), Limit::Max(1));
        assert_eq!(Allowance::Max(5).resolve(Limit::Max(1)), Limit::Max(5));
        assert_eq!(
            Allowance::Unlimited.resolve(Limit::Max(1)),
            Limit::Unlimited
        );
        assert!(Limit::Max(1).allows_one_more(0));
        assert!(!Limit::Max(1).allows_one_more(1));
        assert!(!Limit::Max(0).allows_one_more(0));
        assert!(Limit::Unlimited.allows_one_more(u64::MAX));
    }

    #[test]
    fn a_vault_is_full_at_its_limit() {
        assert!(!Limit::Max(1).reached_mb(MIB - 1));
        assert!(Limit::Max(1).reached_mb(MIB));
        assert!(Limit::Max(0).reached_mb(0));
        assert!(!Limit::Unlimited.reached_mb(u64::MAX));
        assert!(!Limit::Max(u64::MAX).reached_mb(u64::MAX - 1));
        let v: VaultInfo = serde_json::from_value(serde_json::json!({
            "id": "v", "owner_id": "u", "role": "owner", "encrypted_meta": "",
            "key_epoch": 1, "head_seq": 0, "archived": false, "member_count": 1,
            "created_at": 0, "updated_at": 0,
            "storage_bytes": 2 * MIB, "storage_limit_mb": 2,
        }))
        .unwrap();
        assert!(v.is_full());
        // From servers without storage limits: never full.
        let mut v: serde_json::Value = serde_json::to_value(&v).unwrap();
        v.as_object_mut().unwrap().remove("storage_limit_mb");
        assert!(!serde_json::from_value::<VaultInfo>(v).unwrap().is_full());
    }

    #[test]
    fn settings_fill_in_what_is_missing() {
        let s: InstanceSettings = serde_json::from_str(r#"{"max_vaults": 10}"#).unwrap();
        assert_eq!(
            s,
            InstanceSettings {
                max_vaults: Limit::Max(10),
                ..Default::default()
            }
        );
        assert_eq!(s.agents_per_user, Limit::Max(DEFAULT_AGENTS_PER_USER));
        assert_eq!(
            s.quick_notes_per_user,
            Limit::Max(DEFAULT_QUICK_NOTES_PER_USER)
        );
        let mut s = InstanceSettings::default();
        s.apply(&serde_json::from_str(r#"{"read_only": true, "vaults_per_user": 1}"#).unwrap());
        assert!(s.read_only);
        assert_eq!(s.vaults_per_user, Limit::Max(1));
        assert_eq!(s.max_users, Limit::Unlimited);
        s.apply(&serde_json::from_str(r#"{"quick_notes_per_user": "unlimited"}"#).unwrap());
        assert_eq!(s.quick_notes_per_user, Limit::Unlimited);
        assert_eq!(s.vaults_per_user, Limit::Max(1));
    }

    #[test]
    fn only_ones_own_quick_notes_count() {
        let mine = VaultInfo {
            id: "q".into(),
            owner_id: "me".into(),
            role: Role::Owner,
            encrypted_meta: vec![],
            key_epoch: 1,
            head_seq: 0,
            archived: false,
            member_count: 1,
            created_at: 0,
            updated_at: 0,
            viewers_comment: false,
            kind: VaultKind::QuickNotes,
            note_limit: Limit::Max(20),
            drawing_limit: Limit::Unlimited,
            storage_bytes: 0,
            storage_limit_mb: Limit::Unlimited,
            rotation_pending: false,
            notes_rotation_pending: vec![],
        };
        assert!(mine.is_own_quick_notes("me"));
        assert!(!mine.is_own_quick_notes("you"));
        let shared = VaultInfo {
            member_count: 2,
            ..mine.clone()
        };
        assert!(!shared.is_own_quick_notes("me"));
        let theirs = VaultInfo {
            owner_id: "you".into(),
            role: Role::Editor,
            ..mine.clone()
        };
        assert!(!theirs.is_own_quick_notes("me"));
        let regular = VaultInfo {
            kind: VaultKind::Regular,
            ..mine
        };
        assert!(!regular.is_own_quick_notes("me"));
    }

    #[test]
    fn vault_kinds_default_to_regular() {
        let req: CreateVaultRequest = serde_json::from_value(serde_json::json!({
            "id": "v1",
            "encrypted_meta": "AA==",
            "key_envelope": {"user_id": "u", "epoch": 1, "envelope": "AA=="},
        }))
        .unwrap();
        assert_eq!(req.kind, VaultKind::Regular);
        // Regular is the default and isn't written out (older servers).
        assert!(serde_json::to_value(&req).unwrap().get("kind").is_none());
        let quick = CreateVaultRequest {
            kind: VaultKind::QuickNotes,
            ..req
        };
        assert_eq!(
            serde_json::to_value(&quick).unwrap()["kind"],
            serde_json::json!("quick_notes")
        );
        for k in [VaultKind::Regular, VaultKind::QuickNotes] {
            assert_eq!(VaultKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(VaultKind::parse("other"), None);
        assert!(VaultKind::QuickNotes.is_section());
        assert!(!VaultKind::Regular.is_section() && !VaultKind::Other.is_section());
    }

    #[test]
    fn vault_kinds_from_newer_releases_still_read() {
        let mut v = serde_json::json!({
            "id": "v", "owner_id": "u", "role": "owner", "encrypted_meta": "",
            "key_epoch": 1, "head_seq": 0, "archived": false, "member_count": 1,
            "created_at": 0, "updated_at": 0, "kind": "quick_notes",
        });
        let info: VaultInfo = serde_json::from_value(v.clone()).unwrap();
        assert!(info.is_own_quick_notes("u"));
        assert!(!info.is_own_quick_notes("someone else"));
        v["kind"] = "boards".into();
        let info: VaultInfo = serde_json::from_value(v).unwrap();
        assert_eq!(info.kind, VaultKind::Other);
        assert!(!info.is_own_quick_notes("u") && !info.kind.is_section());
        // Older servers say nothing of drawings: no limit.
        assert_eq!(info.drawing_limit, Limit::Unlimited);
    }

    #[test]
    fn objects_are_notes_unless_said_otherwise() {
        // Notes say nothing new: older servers and apps see what they know.
        let update = PushUpdate {
            object_id: "o".into(),
            client_update_id: "c".into(),
            payload: vec![1],
            signature: None,
            kind: ObjectKind::Note,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert!(json.get("kind").is_none());
        let back: PushUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind, ObjectKind::Note);
        let drawing = PushUpdate {
            kind: ObjectKind::Drawing,
            ..update
        };
        assert_eq!(
            serde_json::to_value(&drawing).unwrap()["kind"],
            serde_json::json!("drawing")
        );
        // A key says what it is for.
        let key: ObjectKeyUpdate = serde_json::from_value(serde_json::json!({
            "epoch": 1, "vault_envelope": "AA==", "envelopes": [], "encrypted_meta": "AA==",
        }))
        .unwrap();
        assert_eq!(key.kind, ObjectKind::Note);
        let json = serde_json::to_value(&key).unwrap();
        assert!(json.get("kind").is_none());
        let drawing = ObjectKeyUpdate {
            kind: ObjectKind::Drawing,
            ..key
        };
        let json = serde_json::to_value(&drawing).unwrap();
        assert_eq!(json["kind"], serde_json::json!("drawing"));
        let back: ObjectKeyUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind, ObjectKind::Drawing);
        for k in [ObjectKind::Note, ObjectKind::Drawing] {
            assert_eq!(ObjectKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn drawings_are_limited_per_vault() {
        let s = InstanceSettings::default();
        // Self-hosters have no limit on drawings.
        assert_eq!(s.drawings_per_vault, Limit::Unlimited);
        let mut s: InstanceSettings = serde_json::from_str("{}").unwrap();
        s.apply(
            &serde_json::from_str(r#"{"drawings_per_vault": 1, "storage_per_vault_mb": 9}"#)
                .unwrap(),
        );
        assert_eq!(s.drawings_per_vault, Limit::Max(1));
        assert_eq!(s.storage_default(VaultKind::Regular), Limit::Max(9));
        // A vault's own allowance changes alone.
        let u: AdminUpdateVault = serde_json::from_str(r#"{"drawing_allowance": 3}"#).unwrap();
        assert_eq!(
            (u.drawing_allowance, u.storage_allowance),
            (Some(Allowance::Max(3)), None)
        );
    }
}
