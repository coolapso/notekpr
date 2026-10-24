//! A signed-in, unlocked profile: API client + account secret + local key,
//! and the network half of vault synchronisation (REST catch-up, pushing
//! pending updates and attachments, compaction, key management, TOFU pins).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::engine::{self, Applied, PinStatus, VaultMeta, VaultReplica};
use notekpr_core::keys::{AccountPublic, AccountSecret, VaultKeyring};
use notekpr_core::memberlog::{KeyCheckError, LogBody, LogEntry};
use notekpr_core::protocol::*;
use notekpr_sync::SnapshotAction;
use notekpr_sync::members::{self, MemberLog};
use tokio::sync::{Mutex, OnceCell};

use crate::api::{Api, ApiError, Credentials};
use crate::profile::{Dirs, Profile, ProfileStore, ServerLimits};
use crate::secrets::{LocalKey, SecretStore};
use crate::store::{VaultStore, valid_id};
use crate::ui;
use crate::vault::{Keys, Vault, merge_applied};

pub type SharedVault = Arc<Mutex<Vault>>;

/// Server pages are capped at 5000; 1000 keeps responses small.
const PAGE: i64 = 1000;

pub struct Session {
    pub profiles: ProfileStore,
    pub profile: Profile,
    pub api: Api,
    pub user_id: String,
    pub kind: PrincipalKind,
    pub(crate) account: AccountSecret,
    pub local_key: Arc<LocalKey>,
    info: OnceCell<ServerInfo>,
    limits: std::sync::Mutex<(ServerLimits, bool)>,
}

/// What happened during a push.
#[derive(Debug, Default, Clone)]
pub struct PushOutcome {
    pub pushed: usize,
    pub blobs: usize,
    /// Updates still waiting (offline, or not allowed to write).
    pub remaining: usize,
    pub offline: bool,
    /// The server is read-only: everything stays queued until it accepts
    /// changes again.
    pub read_only: bool,
    /// Updates set aside in this push because the server can't accept them.
    pub quarantined: usize,
    /// Notes whose set-aside updates went out in a snapshot carrying them.
    pub carried: usize,
}

/// A fetched user whose account key passed verification.
#[derive(Debug, Clone)]
pub struct VerifiedUser {
    pub user: PublicUser,
    pub fingerprint: String,
    pub pin: PinStatus,
}

impl Session {
    /// Open a profile that has completed login (or agent setup).
    pub fn open(dirs: &Dirs, profile: &str) -> Result<Session> {
        let profiles = ProfileStore::new(dirs, profile)?;
        let p = profiles.require()?;
        let user_id = p.user_id.clone().ok_or_else(|| {
            anyhow!("profile '{profile}' is not signed in; run `notekpr --profile {profile} login`")
        })?;
        if p.device_pending {
            bail!(
                "this device is still waiting for approval; run `notekpr --profile {profile} login` again to \
                 finish (approve it from another device, or use your recovery key)"
            );
        }
        let secrets = SecretStore::open(&profiles.data_dir())?;
        // Temporary files left by a crash (never one of a write in progress:
        // those are younger than a minute).
        let stale = Duration::from_secs(60);
        crate::fsutil::remove_stale_temp_files(&profiles.data_dir(), 4, stale);
        crate::fsutil::remove_stale_temp_files(&dirs.profiles_dir(), 1, stale);
        let account = secrets.load_account(&user_id)?.ok_or_else(|| {
            anyhow!(
                "no account keys on this device for profile '{profile}'; run `notekpr --profile {profile} login`"
            )
        })?;
        let creds = match p.kind {
            PrincipalKind::Human => match &p.session_token {
                Some(t) => Credentials::Session(t.clone()),
                None => Credentials::None,
            },
            PrincipalKind::Agent => match &p.agent {
                Some(a) => Credentials::Agent {
                    client_id: a.client_id.clone(),
                    secret_file: a.client_secret_file.clone(),
                },
                None => Credentials::None,
            },
        };
        let api = Api::new(&p.server, creds)?;
        // (limits, whether they are the remembered ones rather than defaults)
        let limits = (
            p.server_limits.unwrap_or_default(),
            p.server_limits.is_some(),
        );
        Ok(Session {
            profiles,
            kind: p.kind,
            profile: p,
            api,
            user_id,
            account,
            local_key: Arc::new(secrets.into_local_key()),
            info: OnceCell::new(),
            limits: std::sync::Mutex::new(limits),
        })
    }

    pub fn account(&self) -> &AccountSecret {
        &self.account
    }

    pub fn fingerprint(&self) -> String {
        self.account.public().fingerprint()
    }

    pub fn name(&self) -> &str {
        &self.profiles.name
    }

    pub fn data_dir(&self) -> PathBuf {
        self.profiles.data_dir()
    }

    /// `/info` (fetched once per process). Also refreshes [`Session::limits`].
    pub async fn server_info(&self) -> Result<&ServerInfo, ApiError> {
        let info = self.info.get_or_try_init(|| self.api.info()).await?;
        let fresh = ServerLimits {
            max_attachment_bytes: info.max_attachment_bytes,
            max_update_bytes: info.max_update_bytes,
            max_snapshot_bytes: info.max_snapshot_bytes,
        };
        let changed = {
            let mut l = self.limits.lock().unwrap();
            let known = std::mem::replace(&mut *l, (fresh, true));
            known != (fresh, true)
        };
        if changed && let Err(e) = self.profiles.update(|p| p.server_limits = Some(fresh)) {
            tracing::debug!("could not remember the server limits: {e:#}");
        }
        Ok(info)
    }

    /// The server's size limits without touching the network: as last seen
    /// (remembered in the profile), or the server defaults. Safe to call
    /// while holding a vault lock and offline.
    pub fn limits(&self) -> ServerLimits {
        self.limits.lock().unwrap().0
    }

    fn vaults_dir(&self) -> PathBuf {
        self.data_dir().join("vaults")
    }

    pub fn vault_store(&self, vault_id: &str) -> Result<VaultStore> {
        VaultStore::new(self.vaults_dir(), self.local_key.clone(), vault_id)
    }

    /// Ids of vaults with a local replica.
    pub fn local_vault_ids(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.vaults_dir()) else {
            return vec![];
        };
        let mut v: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| valid_id(n))
            .filter(|n| self.vault_store(n).map(|s| s.exists()).unwrap_or(false))
            .collect();
        v.sort();
        v
    }

    pub fn load_local(&self, vault_id: &str) -> Result<Option<Vault>> {
        let mut v = Vault::load(self.vault_store(vault_id)?)
            .with_context(|| format!("loading the local copy of vault {vault_id}"))?;
        if let Some(v) = &mut v {
            v.set_limits(&self.limits());
        }
        Ok(v)
    }

    // ------------------------------------------------------------ keys

    /// Fetch and unwrap this principal's keys for a vault.
    pub async fn keyring(&self, vault_id: &str) -> Result<VaultKeyring> {
        Ok(self.keys(vault_id).await?.ring)
    }

    /// Vault keyring plus the document keyrings of its notes.
    pub async fn keys(&self, vault_id: &str) -> Result<Keys> {
        let r = self.api.vault_keys(vault_id).await?;
        let ring = engine::keyring_from_response(&self.account, &self.user_id, vault_id, &r)?;
        let docs = engine::object_keyrings(&ring, &r);
        Ok(Keys {
            ring,
            docs,
            shared: engine::shared_objects(&r),
            share_from: engine::share_from(&r),
            note_keys: r.shared.is_some(),
        })
    }

    /// Open a vault the server listed: the local replica if there is one
    /// (refreshing keys, role and metadata), otherwise a fresh replica.
    pub async fn open_vault(&self, info: &VaultInfo) -> Result<Vault> {
        let store = self.vault_store(&info.id)?;
        let mut v = match Vault::load(store.clone())? {
            Some(v) => v,
            None => {
                let keys = self.keys(&info.id).await?;
                let meta =
                    VaultMeta::decrypt(&keys.ring, &info.encrypted_meta).unwrap_or_else(|e| {
                        tracing::warn!("cannot decrypt vault metadata: {e}");
                        VaultMeta::named(&info.id)
                    });
                let replica = VaultReplica::new(&info.id, info.role, keys.ring.clone(), meta);
                let mut v = Vault::new(store, replica, &info.owner_id, info.archived);
                v.set_keys(keys);
                v.save()?;
                v
            }
        };
        self.apply_info(&mut v, info).await?;
        Ok(v)
    }

    /// Bring role, archive flag, keys and name in line with the server.
    pub async fn apply_info(&self, v: &mut Vault, info: &VaultInfo) -> Result<()> {
        v.set_limits(&self.limits());
        // (Copies from before notes had their own keys learn whether the
        // server keeps them.)
        if v.replica.keyring.current_epoch().unwrap_or(0) < info.key_epoch || !v.outbox.note_keys {
            let keys = self.keys(&info.id).await?;
            v.set_keys(keys);
        }
        v.replica.role = info.role;
        v.owner_id = info.owner_id.clone();
        v.archived = info.archived;
        v.viewers_comment = info.viewers_comment;
        v.read_only = None;
        self.load_member_log(v).await?;
        match VaultMeta::decrypt(&v.replica.keyring, &info.encrypted_meta) {
            Ok(m) => v.set_meta(m),
            Err(e) => tracing::warn!(vault = %info.id, "cannot decrypt vault metadata: {e}"),
        }
        v.mark_state_dirty();
        v.save()
    }

    // ------------------------------------------------------------ membership log

    /// The key a vault's membership log must be signed with: ours, or the
    /// owner's as pinned on this device (trust on first use).
    async fn owner_key(&self, owner_id: &str) -> Result<AccountPublic> {
        if owner_id == self.user_id {
            return Ok(self.account.public());
        }
        let owner = self.api.user(owner_id).await?;
        let vu = self
            .verify_user(&owner)?
            .ok_or_else(|| anyhow!("the vault's owner has no account key"))?;
        if vu.pin == PinStatus::Changed {
            bail!(
                "SECURITY: the account key of {}, who owns this vault, changed since it was pinned (now {}). \
                 Their membership changes can't be trusted until you verify the new key with them",
                owner.display_name,
                vu.fingerprint
            );
        }
        Ok(owner
            .account_key
            .as_ref()
            .expect("verified above")
            .verify()?)
    }

    /// Fetch and verify a vault's membership log, and check the vault keys
    /// we hold against the owner's commitments. Owners start the log of a
    /// vault that has none (a checkpoint of the current members).
    pub async fn load_member_log(&self, v: &mut Vault) -> Result<Applied> {
        let id = v.id().to_string();
        self.verify_member_log(v).await?;
        if v.role() == Role::Owner && v.read_only.is_none() {
            if !v.log.has_log()
                && let Err(e) = self.start_member_log(v).await
            {
                tracing::warn!(vault = %id, "could not start the membership log: {e:#}");
            }
            if v.log.needs_signatures()
                && let Err(e) = self.require_signatures(v).await
            {
                tracing::warn!(vault = %id, "could not require signed changes: {e:#}");
            }
            if let Err(e) = self.sign_missing_grants(v).await {
                tracing::warn!(vault = %id, "could not sign note keys: {e:#}");
            }
        }
        // Content is checked against who may write it from now on.
        let authors = v.log.authors();
        Ok(v.set_authors(authors))
    }

    /// Fetch the vault's log, verify it, and check our vault keys against it.
    async fn verify_member_log(&self, v: &mut Vault) -> Result<()> {
        let id = v.id().to_string();
        let anchor = self.owner_key(&v.owner_id).await?;
        for attempt in 0..2 {
            let entries = self
                .api
                .get::<MemberLogResponse>(&format!("/vaults/{id}/log"))
                .await?
                .entries;
            let seen = v.log_seen.clone();
            let log = MemberLog::verify(
                &id,
                &anchor,
                entries,
                seen.as_ref().map(|(s, h)| (*s, h.as_slice())),
            )
            .map_err(|e| {
                anyhow!(
                    "SECURITY: the membership history of vault '{}' does not check out ({e}). The server may \
                     have been tampered with; nothing was changed. Tell the vault's owner and your administrator",
                    v.name()
                )
            })?;
            match log.check_keyring(&v.replica.keyring) {
                Ok(()) => {
                    v.log_seen = log.head();
                    v.log = log;
                    v.mark_state_dirty();
                    break;
                }
                // A rotation between our reads of the keys and the log.
                Err(KeyCheckError::Unannounced(_)) if attempt == 0 => continue,
                Err(e) => bail!(
                    "SECURITY: {e} (vault '{}'). The server may be trying to read new notes; this vault is \
                     not used until that is resolved. Tell the vault's owner and your administrator",
                    v.name()
                ),
            }
        }
        Ok(())
    }

    /// Owner: sign `body` into the vault's log (`POST /vaults/{v}/log`).
    async fn append_log(&self, v: &mut Vault, body: LogBody) -> Result<()> {
        let id = v.id().to_string();
        let entry = v
            .log
            .next(
                &self.account,
                &self.user_id,
                &id,
                body,
                notekpr_core::now_ms(),
            )
            .ok_or_else(|| anyhow!("the vault has no membership log"))?;
        self.api
            .post::<_, ()>(&format!("/vaults/{id}/log"), &AppendLogRequest { entry })
            .await?;
        self.verify_member_log(v).await
    }

    /// Owner: sign the keys of shared notes given before grants were signed,
    /// so the people they are shared with can use them.
    async fn sign_missing_grants(&self, v: &mut Vault) -> Result<()> {
        let id = v.id().to_string();
        for oid in v.replica.shared.clone() {
            let Some(ring) = v.replica.doc_keys.get(&oid).cloned() else {
                continue;
            };
            let shares: Vec<ObjectShareInfo> = match self
                .api
                .get(&format!("/vaults/{id}/objects/{oid}/shares"))
                .await
            {
                Ok(s) => s,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            };
            let grants =
                members::missing_grant_signatures(&self.account, &id, &oid, &ring, &shares);
            if !grants.is_empty() {
                self.api
                    .put::<_, ()>(
                        &format!("/vaults/{id}/objects/{oid}/keys/signatures"),
                        &SignGrantsRequest { grants },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Owner: from now on, require every change to be signed by its author.
    /// The people single notes are already shared with are signed into the
    /// log first (their keys checked against our pins), so their edits keep
    /// counting.
    async fn require_signatures(&self, v: &mut Vault) -> Result<()> {
        let id = v.id().to_string();
        for oid in v.replica.shared.clone() {
            let shares: Vec<ObjectShareInfo> = match self
                .api
                .get(&format!("/vaults/{id}/objects/{oid}/shares"))
                .await
            {
                Ok(s) => s,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            };
            for sh in shares {
                let Some(vu) = self.verify_user(&sh.user)? else {
                    continue;
                };
                if vu.pin == PinStatus::Changed {
                    bail!(
                        "the account key of {} changed since it was pinned; verify it before their edits can count",
                        sh.user.display_name
                    );
                }
                let key = sh.user.account_key.as_ref().expect("verified").verify()?;
                if !v.log.has_share(&oid, &sh.user.id, sh.role, &key) {
                    self.append_log(v, members::share(&oid, &sh.user.id, sh.role, &key))
                        .await?;
                }
            }
        }
        let head = self.api.vault(&id).await?.head_seq;
        self.append_log(v, LogBody::RequireSigned { from_seq: head })
            .await
    }

    /// Owner: sign a checkpoint of the members as they are now (their keys
    /// checked against our pins) to start the vault's membership log.
    async fn start_member_log(&self, v: &mut Vault) -> Result<()> {
        let id = v.id().to_string();
        let members = self.api.members(&id).await?;
        // The checkpoint vouches for everyone's key: never for a changed one.
        let (_, changed) =
            self.split_changed_keys(members.iter().map(|m| m.user.clone()).collect());
        if let Some(u) = changed.first() {
            bail!(
                "the account key of {} changed since it was pinned; not starting the vault's membership \
                 log. Verify it with them, then re-share with --accept-new-key and run this again",
                u.display_name
            );
        }
        let list: Vec<(String, Role, Option<AccountPublic>)> = members
            .iter()
            .map(|m| {
                (
                    m.user.id.clone(),
                    m.role,
                    m.user.account_key.as_ref().and_then(|k| k.verify().ok()),
                )
            })
            .collect();
        let body = members::checkpoint(&id, &v.replica.keyring, &list)
            .ok_or_else(|| anyhow!("no vault key"))?;
        let entry = v
            .log
            .next(
                &self.account,
                &self.user_id,
                &id,
                body,
                notekpr_core::now_ms(),
            )
            .ok_or_else(|| anyhow!("the vault has a membership log already"))?;
        self.api
            .post::<_, ()>(
                &format!("/vaults/{id}/log"),
                &AppendLogRequest {
                    entry: entry.clone(),
                },
            )
            .await?;
        v.log = MemberLog::verify(&id, &self.account.public(), vec![entry], None)?;
        v.log_seen = v.log.head();
        v.mark_state_dirty();
        Ok(())
    }

    /// [`Session::load_member_log`] for an open vault (after a change).
    /// Returns what content that waited for it could be applied.
    pub async fn reload_member_log(&self, vault: &SharedVault) -> Result<Applied> {
        let mut v = vault.lock().await;
        let a = self.load_member_log(&mut v).await?;
        v.save()?;
        Ok(a)
    }

    /// Owner: pin a member's (new) account key in the vault's log, after the
    /// caller checked it against our pin.
    pub async fn pin_member_key(
        &self,
        vault: &SharedVault,
        user_id: &str,
        role: Role,
        key: &AccountPublic,
    ) -> Result<()> {
        let body = LogBody::Rekey {
            member: members::log_member(user_id, role, Some(key)),
        };
        let Some(entry) = self.log_entry(vault, body).await else {
            return Ok(());
        };
        let id = vault.lock().await.id().to_string();
        self.api
            .post::<_, ()>(&format!("/vaults/{id}/log"), &AppendLogRequest { entry })
            .await?;
        self.reload_member_log(vault).await.map(|_| ())
    }

    /// Sign `body` as the vault's next membership log entry (`None`: the
    /// vault has no log yet, older servers).
    pub async fn log_entry(&self, vault: &SharedVault, body: LogBody) -> Option<LogEntry> {
        let v = vault.lock().await;
        v.log.next(
            &self.account,
            &self.user_id,
            v.id(),
            body,
            notekpr_core::now_ms(),
        )
    }

    pub async fn refresh_keys(&self, vault: &SharedVault) -> Result<()> {
        let id = vault.lock().await.id().to_string();
        let keys = self.keys(&id).await?;
        let mut v = vault.lock().await;
        v.set_keys(keys);
        v.save()
    }

    /// Reload one note's keys (it got or rotated its own key), or the whole
    /// vault's from servers that can't hand out one note's keys to members.
    pub async fn refresh_note_keys(&self, vault: &SharedVault, object_id: &str) -> Result<()> {
        let (id, ring) = {
            let v = vault.lock().await;
            (v.id().to_string(), v.replica.keyring.clone())
        };
        let r: ObjectKeysResponse = match self
            .api
            .get(&format!("/vaults/{id}/objects/{object_id}/keys"))
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_not_found() => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match engine::object_keyring_for_member(&ring, &r) {
            Some(doc) => {
                let mut v = vault.lock().await;
                v.set_note_keys(object_id, doc, r.shared.unwrap_or(false));
                v.save()
            }
            None => self.refresh_keys(vault).await,
        }
    }

    pub async fn refresh_vault(&self, vault: &SharedVault) -> Result<()> {
        let id = vault.lock().await.id().to_string();
        let info = self.api.vault(&id).await?;
        let mut v = vault.lock().await;
        self.apply_info(&mut v, &info).await
    }

    // ------------------------------------------------------------ sync

    /// Fetch everything after the local cursor over REST (one-shot commands).
    pub async fn catch_up(&self, vault: &SharedVault) -> Result<Applied, ApiError> {
        let (id, mut since) = {
            let v = vault.lock().await;
            (v.id().to_string(), v.replica.seq)
        };
        let mut first = true;
        let mut all = Applied::default();
        loop {
            let ch = self.api.changes(&id, since, PAGE, first).await?;
            let mut v = vault.lock().await;
            if v.check_rollback(ch.head_seq) {
                since = v.replica.seq;
                first = true;
                continue;
            }
            first = false;
            merge_applied(&mut all, v.apply_changes(&ch));
            since = ch.next_since;
            if !ch.has_more {
                v.last_sync = Some(notekpr_core::now_ms());
                if let Err(e) = v.save() {
                    ui::warn(&format!("could not save the local copy: {e:#}"));
                }
                let needs_log = v.replica.needs_log();
                drop(v);
                // Some content is by someone our copy of the membership log
                // doesn't know yet: reload it and check again.
                if needs_log && let Err(e) = self.reload_member_log(vault).await {
                    ui::warn(&format!("could not reload the membership log: {e:#}"));
                }
                // Some content needs a key we don't have yet (a note got its own
                // key, or the vault key rotated): fetch keys and replay it.
                if vault.lock().await.replica.needs_keys()
                    && let Err(e) = self.refresh_keys(vault).await
                {
                    ui::warn(&format!("could not refresh keys: {e:#}"));
                }
                return Ok(all);
            }
        }
    }

    /// Upload pending attachments, then pending updates in batches.
    /// Network failures leave everything queued (and report `offline`).
    pub async fn push(&self, vault: &SharedVault) -> Result<PushOutcome> {
        let mut out = PushOutcome::default();
        let (id, blobs, locked, writable) = {
            let v = vault.lock().await;
            (
                v.id().to_string(),
                v.pending_blobs.clone(),
                v.read_only.is_some(),
                v.replica.can_write(),
            )
        };
        if locked {
            out.remaining = vault.lock().await.replica.pending.len();
            return Ok(out);
        }
        // Viewers send comments only (the outbox's plan picks them).
        for blob in blobs.into_iter().filter(|_| writable) {
            let ct = vault.lock().await.store().load_blob(&blob)?;
            let Some(ct) = ct else {
                tracing::warn!("a queued attachment is missing from the local cache");
                vault.lock().await.pending_blobs.remove(&blob);
                continue;
            };
            match self
                .api
                .put_bytes(&format!("/vaults/{id}/attachments/{blob}"), &ct)
                .await
            {
                Ok(()) => {
                    let mut v = vault.lock().await;
                    v.pending_blobs.remove(&blob);
                    v.mark_state_dirty();
                    v.save()?;
                    out.blobs += 1;
                }
                Err(e) if e.is_read_only() || e.is_offline() => {
                    out.read_only = e.is_read_only();
                    out.offline = !out.read_only;
                    out.remaining = vault.lock().await.replica.pending.len();
                    return Ok(out);
                }
                Err(e) => return Err(anyhow!(e).context("uploading an attachment")),
            }
        }
        let mut rekeyed = false;
        // Known even offline (remembered), never "unlimited".
        let limits = self.limits();
        {
            let mut v = vault.lock().await;
            v.set_limits(&limits);
            v.new_push_session();
        }
        'rounds: loop {
            let plan = {
                let mut v = vault.lock().await;
                let plan = v.plan_push();
                if !plan.held.is_empty() {
                    out.quarantined += plan.held.len();
                    warn_too_large(v.name(), plan.held.len(), limits.max_update_bytes as usize);
                }
                v.save()?;
                plan
            };
            if plan.is_empty() {
                break;
            }
            // New notes' own keys first; their content follows next round.
            for k in &plan.register {
                let path = format!("/vaults/{id}/objects/{}/key", k.object_id);
                match self.api.put::<_, ()>(&path, &k.update).await {
                    Ok(()) => {
                        let mut v = vault.lock().await;
                        v.key_registered(&k.object_id);
                        v.save()?;
                    }
                    Err(e) if e.is_read_only() => {
                        out.read_only = true;
                        break 'rounds;
                    }
                    Err(e) if e.is_offline() => {
                        out.offline = true;
                        break 'rounds;
                    }
                    // Wrapped under a vault key that was replaced meanwhile:
                    // with fresh keys, a new key under the current one.
                    Err(e) if e.status() == Some(412) => {
                        self.refresh_keys(vault).await?;
                        let mut v = vault.lock().await;
                        v.key_refused(&k.object_id);
                        v.save()?;
                    }
                    // Refused (403: e.g. no write access any more, which the
                    // pushes below find out). Perhaps it is registered and
                    // the answer got lost: fresh keys show
                    // (`Outbox::keys_reloaded`).
                    Err(e) if matches!(e.status(), Some(400 | 403 | 409)) => {
                        self.refresh_keys(vault).await?;
                        let mut v = vault.lock().await;
                        if v.outbox.waits_for_key(&k.object_id) {
                            tracing::warn!(vault = %id, "the server refused a new note's own key ({e}); it is encrypted with the vault key instead");
                            v.key_refused(&k.object_id);
                        }
                        v.save()?;
                    }
                    Err(e) if e.status() == Some(404) => {
                        return Err(anyhow!(e)
                            .context("the vault no longer exists or you lost access to it"));
                    }
                    Err(e) => return Err(anyhow!(e).context("registering a new note's key")),
                }
            }
            let mut planned = plan.pushes;
            members::sign_pushes(&self.account, &id, &mut planned);
            let mut pushes = planned.into_iter();
            while let Some(batch) = pushes.next() {
                let ids: Vec<String> = batch.iter().map(|p| p.client_update_id.clone()).collect();
                // Whatever was not delivered goes out in a later round.
                let not_sent =
                    |v: &mut crate::vault::Vault, rest: std::vec::IntoIter<Vec<PushUpdate>>| {
                        v.not_sent(&ids);
                        for b in rest {
                            let rest_ids: Vec<String> =
                                b.iter().map(|p| p.client_update_id.clone()).collect();
                            v.not_sent(&rest_ids);
                        }
                    };
                match self.api.push(&id, batch).await {
                    Ok(resp) => {
                        let mut v = vault.lock().await;
                        v.check_rollback(resp.head_seq);
                        out.pushed += resp.results.len();
                        let reaction = v.push_answered(&resp.results, &resp.rejected);
                        v.save()?;
                        if !reaction.held.is_empty() {
                            out.quarantined += reaction.held.len();
                            warn_too_large(
                                v.name(),
                                reaction.held.len(),
                                limits.max_update_bytes as usize,
                            );
                        }
                        if !reaction.purged.is_empty() {
                            tracing::warn!(vault = %id, "{} document(s) were permanently deleted elsewhere (unsent edits are kept as recovered notes)", reaction.purged.len());
                        }
                        if reaction.reload_keys {
                            not_sent(&mut v, pushes);
                            drop(v);
                            if rekeyed {
                                // Still refused with fresh keys: try again later.
                                break 'rounds;
                            }
                            // A note became individually shared (or its key
                            // rotated): fetch the new keys; the next round
                            // re-encrypts.
                            rekeyed = true;
                            self.refresh_keys(vault).await?;
                            continue 'rounds;
                        }
                    }
                    Err(e) if e.is_read_only() => {
                        not_sent(&mut *vault.lock().await, pushes);
                        out.read_only = true;
                        break 'rounds;
                    }
                    Err(e) if e.is_offline() => {
                        not_sent(&mut *vault.lock().await, pushes);
                        out.offline = true;
                        break 'rounds;
                    }
                    // Older servers refuse a whole batch for one bad update.
                    Err(e) if e.status() == Some(409) || e.status() == Some(413) => {
                        let mut v = vault.lock().await;
                        let reaction = v.push_refused(&ids, e.status() == Some(413));
                        not_sent(&mut v, pushes);
                        v.save()?;
                        if !reaction.held.is_empty() {
                            out.quarantined += reaction.held.len();
                            warn_too_large(
                                v.name(),
                                reaction.held.len(),
                                limits.max_update_bytes as usize,
                            );
                        }
                        drop(v);
                        if e.status() == Some(409) {
                            let _ = self.catch_up(vault).await;
                        }
                        continue 'rounds;
                    }
                    Err(e) if e.status() == Some(412) && !rekeyed => {
                        not_sent(&mut *vault.lock().await, pushes);
                        rekeyed = true;
                        self.refresh_keys(vault).await?;
                        continue 'rounds;
                    }
                    Err(e) if e.status() == Some(403) => {
                        not_sent(&mut *vault.lock().await, pushes);
                        let _ = self.refresh_vault(vault).await;
                        out.remaining = vault.lock().await.replica.pending.len();
                        ui::warn(&format!(
                            "you no longer have write access to vault '{}'; {} local change(s) kept but not uploaded",
                            vault.lock().await.name(),
                            out.remaining
                        ));
                        return Ok(out);
                    }
                    Err(e) if e.status() == Some(404) => {
                        not_sent(&mut *vault.lock().await, pushes);
                        return Err(anyhow!(e)
                            .context("the vault no longer exists or you lost access to it"));
                    }
                    Err(e) => {
                        not_sent(&mut *vault.lock().await, pushes);
                        return Err(anyhow!(e).context("uploading changes"));
                    }
                }
            }
        }
        if !out.offline && !out.read_only {
            out.carried = self.carry(vault).await?;
        }
        out.remaining = vault.lock().await.replica.pending.len();
        Ok(out)
    }

    /// Updates set aside as too large go out in snapshots of their notes,
    /// which carry them to everyone (`notekpr_sync::carrier_now`).
    /// Returns how many notes went out.
    async fn carry(&self, vault: &SharedVault) -> Result<usize> {
        let (id, candidates) = {
            let v = vault.lock().await;
            (v.id().to_string(), v.outbox.carry_candidates())
        };
        if candidates.is_empty() {
            return Ok(0);
        }
        // A snapshot's `covers_seq` is our cursor: catch up first (protocol §6).
        match self.catch_up(vault).await {
            Ok(_) => {}
            Err(e) if e.is_offline() => return Ok(0),
            Err(e) => return Err(anyhow!(e).context("catching up before a snapshot")),
        }
        let mut n = 0;
        for oid in candidates {
            let carrier = {
                let v = vault.lock().await;
                notekpr_sync::carrier_now(&v.replica, &v.outbox, &oid, None, &v.limits())
            };
            let Some(mut c) = carrier else { continue };
            members::sign_snapshot(&self.account, &id, &oid, &mut c.request);
            match self.api.create_snapshot(&id, &oid, &c.request).await {
                Ok(_) => {
                    let mut v = vault.lock().await;
                    v.carried(&oid, &c.held);
                    v.save()?;
                    n += 1;
                }
                // Offline, or the server's snapshot cooldown: the next push.
                Err(e) if e.is_offline() || e.status() == Some(429) => {}
                Err(e) => {
                    return Err(anyhow!(e).context("uploading a note's changes in a snapshot"));
                }
            }
        }
        Ok(n)
    }

    /// Upload compacted snapshots for objects with many updates. `jitter`
    /// spreads concurrent clients so they rarely compact the same object.
    pub async fn compact(&self, vault: &SharedVault, jitter: Option<Duration>) -> Result<usize> {
        let (id, candidates) = {
            let v = vault.lock().await;
            if v.read_only.is_some() {
                return Ok(0);
            }
            (v.id().to_string(), v.replica.compaction_candidates())
        };
        let mut n = 0;
        for oid in candidates {
            if let Some(max) = jitter {
                tokio::time::sleep(random_duration(max)).await;
            }
            self.snapshot_object(&id, vault, &oid).await?;
            n += 1;
        }
        Ok(n)
    }

    /// Store the current state of one object as a snapshot (revision).
    pub async fn snapshot_object(&self, id: &str, vault: &SharedVault, oid: &str) -> Result<bool> {
        // `covers_seq` is our cursor, which REST acknowledgements do not move;
        // catching up first makes it current and guarantees every update of
        // the object up to it has been applied (protocol §6).
        match self.catch_up(vault).await {
            Ok(_) => {}
            Err(e) if e.is_offline() => return Ok(false),
            Err(e) => return Err(anyhow!(e).context("catching up before a snapshot")),
        }
        let action = {
            let v = vault.lock().await;
            match notekpr_sync::snapshot_action(&v.replica, &v.outbox, oid, None) {
                Some(SnapshotAction::Snapshot(mut req)) => {
                    members::sign_snapshot(&self.account, id, oid, &mut req);
                    SnapshotAction::Snapshot(req)
                }
                Some(SnapshotAction::AdoptKey { ring, mut update }) => {
                    members::sign_key_update(&self.account, id, oid, &mut update);
                    SnapshotAction::AdoptKey { ring, update }
                }
                None => return Ok(false),
            }
        };
        let res = match &action {
            SnapshotAction::Snapshot(req) => {
                self.api.create_snapshot(id, oid, req).await.map(|_| ())
            }
            // An older note moves to its own key, with this snapshot under it.
            SnapshotAction::AdoptKey { update, .. } => {
                self.api
                    .put::<_, ()>(&format!("/vaults/{id}/objects/{oid}/key"), update)
                    .await
            }
        };
        match res {
            Ok(()) => {
                let mut v = vault.lock().await;
                if let SnapshotAction::AdoptKey { ring, .. } = action {
                    v.set_note_keyring(oid, ring);
                }
                v.replica.snapshot_created(oid);
                v.mark_state_dirty();
                v.save()?;
                Ok(true)
            }
            Err(e) if e.is_offline() => Ok(false),
            // Someone else gave it a key, or changes arrived meanwhile: next time.
            Err(e)
                if matches!(action, SnapshotAction::AdoptKey { .. })
                    && matches!(e.status(), Some(409 | 412)) =>
            {
                self.refresh_keys(vault).await?;
                Ok(false)
            }
            Err(e) => Err(anyhow!(e).context("uploading a snapshot")),
        }
    }

    /// Pending + catch-up + compaction for one vault.
    pub async fn sync_vault(&self, vault: &SharedVault) -> Result<PushOutcome> {
        self.catch_up(vault).await?;
        let out = self.push(vault).await?;
        if !out.offline && !out.read_only {
            self.compact(vault, None).await?;
        }
        Ok(out)
    }

    /// Download (or read from cache) and decrypt an attachment.
    pub async fn attachment(&self, vault: &SharedVault, attachment_id: &str) -> Result<Vec<u8>> {
        let (id, cached) = {
            let v = vault.lock().await;
            (v.id().to_string(), v.cached_attachment(attachment_id)?)
        };
        if let Some(d) = cached {
            return Ok(d);
        }
        let ct = self
            .api
            .get_bytes(&format!("/vaults/{id}/attachments/{attachment_id}"))
            .await?;
        let v = vault.lock().await;
        let pt = v.replica.decrypt_attachment(attachment_id, &ct)?;
        if !v.is_detached() {
            v.store().save_blob(attachment_id, &ct)?;
        }
        Ok(pt)
    }

    // ------------------------------------------------------------ trust

    /// Verify a user's account key signature and check it against the pinned
    /// fingerprint (trust on first use). `None` if they have no keys yet.
    pub fn verify_user(&self, user: &PublicUser) -> Result<Option<VerifiedUser>> {
        let Some(key) = &user.account_key else {
            return Ok(None);
        };
        if key.user_id != user.id {
            bail!("the server returned a key that belongs to a different user");
        }
        let public = key.verify().map_err(|e| {
            anyhow!(
                "the account key of {} failed verification: {e}",
                user.display_name
            )
        })?;
        let fingerprint = public.fingerprint();
        let pin = if user.id == self.user_id {
            if public == self.account.public() {
                PinStatus::Match
            } else {
                PinStatus::Changed
            }
        } else {
            self.profiles
                .update(|p| engine::check_pin(&mut p.pins, &user.id, &fingerprint))?
        };
        Ok(Some(VerifiedUser {
            user: user.clone(),
            fingerprint,
            pin,
        }))
    }

    /// Replace a pin after the user confirmed a changed key out of band.
    pub fn accept_pin(&self, user_id: &str, fingerprint: &str) -> Result<()> {
        self.profiles.update(|p| {
            p.pins.insert(user_id.to_string(), fingerprint.to_string());
        })
    }

    /// As owner, wrap the current vault key for members who do not have it
    /// yet (added before they set up encryption). Members whose key changed
    /// since it was pinned are skipped with a warning.
    pub async fn grant_missing_keys(&self, vault: &SharedVault) -> Result<Vec<String>> {
        let (id, ring) = {
            let v = vault.lock().await;
            if v.role() != Role::Owner || v.read_only.is_some() {
                return Ok(vec![]);
            }
            (v.id().to_string(), v.replica.keyring.clone())
        };
        let members = self.api.members(&id).await?;
        let mut ok = Vec::new();
        for m in members
            .iter()
            .filter(|m| !m.has_current_key && m.user.account_key.is_some())
        {
            match self.verify_user(&m.user) {
                Ok(Some(vu)) if vu.pin == PinStatus::Changed => ui::warn(&format!(
                    "NOT sharing keys with {}: their account key changed since it was pinned (now {}). \
                     Verify it with them, then run `notekpr vault share` again with --accept-new-key",
                    m.user.display_name, vu.fingerprint
                )),
                Ok(Some(vu)) => {
                    if vu.pin == PinStatus::New {
                        ui::status(&format!(
                            "pinned the key of {} ({})",
                            m.user.display_name, vu.fingerprint
                        ));
                    }
                    // Added before they had keys: pin them in the log first.
                    let pending = !vault
                        .lock()
                        .await
                        .log
                        .to_pin(std::slice::from_ref(m))
                        .is_empty();
                    if pending {
                        let key = m.user.account_key.as_ref().expect("filtered").verify()?;
                        self.pin_member_key(vault, &m.user.id, m.role, &key).await?;
                    }
                    if vault.lock().await.log.vouches_for(m) {
                        ok.push(m.clone());
                    } else {
                        ui::warn(&format!(
                            "NOT sharing keys with {}: the vault's membership log doesn't list them with this \
                             key and role. The server may be adding people you didn't; tell your administrator",
                            m.user.display_name
                        ));
                    }
                }
                Ok(None) => {}
                Err(e) => ui::warn(&format!("skipping {}: {e:#}", m.user.display_name)),
            }
        }
        let envelopes = engine::missing_grants(&ring, &ok);
        if envelopes.is_empty() {
            return Ok(vec![]);
        }
        let names: Vec<String> = envelopes
            .iter()
            .filter_map(|e| {
                ok.iter()
                    .find(|m| m.user.id == e.user_id)
                    .map(|m| m.user.display_name.clone())
            })
            .collect();
        self.api
            .post::<_, ()>(
                &format!("/vaults/{id}/keys/grant"),
                &GrantKeysRequest { envelopes },
            )
            .await?;
        Ok(names)
    }

    /// `users` whose account key still matches its pin (or who have none),
    /// and those whose key changed or doesn't verify: a new key is never
    /// wrapped to the latter — they are left out until it is verified.
    pub fn split_changed_keys(&self, users: Vec<PublicUser>) -> (Vec<PublicUser>, Vec<PublicUser>) {
        users.into_iter().partition(|u| {
            matches!(self.verify_user(u), Ok(None))
                || matches!(self.verify_user(u), Ok(Some(vu)) if vu.pin != PinStatus::Changed)
        })
    }

    /// Move the vault to a new key epoch for the current members, and every
    /// note's key with it (removed members knew those too). Members and
    /// recipients whose key no longer matches its pin (or the vault's log)
    /// are left out on purpose, and told how to give them access again.
    pub async fn rotate_keys(&self, vault: &SharedVault) -> Result<u32> {
        let id = vault.lock().await.id().to_string();
        for attempt in 0..2 {
            // The rotation must cover exactly the notes the server has keys for.
            self.refresh_keys(vault).await?;
            self.reload_member_log(vault).await?;
            let members = self.api.members(&id).await?;
            // The new key goes only to members the owner's log vouches for
            // and whose key still matches its pin; anyone else is left out on
            // purpose and gets it once verified.
            let (_, changed) =
                self.split_changed_keys(members.iter().map(|m| m.user.clone()).collect());
            let (members, left_out): (Vec<MemberInfo>, Vec<MemberInfo>) = {
                let v = vault.lock().await;
                members.into_iter().partition(|m| {
                    m.user.account_key.is_none()
                        || (v.log.vouches_for(m) && !changed.iter().any(|u| u.id == m.user.id))
                })
            };
            let mut not_given: Vec<String> = left_out
                .iter()
                .map(|m| m.user.display_name.clone())
                .collect();
            let mut recipients_out = std::collections::HashMap::new();
            let (keyed, shared) = {
                let v = vault.lock().await;
                let keyed: Vec<String> = v
                    .replica
                    .doc_keys
                    .keys()
                    .filter(|o| !v.outbox.waits_for_key(o))
                    .cloned()
                    .collect();
                (keyed, v.replica.shared.clone())
            };
            let mut recipients = std::collections::HashMap::new();
            for oid in keyed.iter().filter(|o| shared.contains(*o)) {
                let shares: Vec<notekpr_core::protocol::ObjectShareInfo> = match self
                    .api
                    .get(&format!("/vaults/{id}/objects/{oid}/shares"))
                    .await
                {
                    Ok(s) => s,
                    Err(e) if e.is_not_found() => vec![],
                    Err(e) => {
                        return Err(anyhow!(e).context("listing a shared note's recipients"));
                    }
                };
                let (users, changed) =
                    self.split_changed_keys(shares.into_iter().map(|s| s.user).collect());
                not_given.extend(changed.iter().map(|u| u.display_name.clone()));
                recipients_out.insert(
                    oid.clone(),
                    changed.into_iter().map(|u| u.id).collect::<Vec<_>>(),
                );
                recipients.insert(oid.clone(), users);
            }
            let (req, ring, rings) = {
                let v = vault.lock().await;
                let notes: Vec<_> = keyed
                    .iter()
                    .map(|oid| {
                        let meta = v
                            .replica
                            .index
                            .get(oid)
                            .map(|e| engine::ObjectMeta::of(&e))
                            .unwrap_or_default();
                        (
                            oid.clone(),
                            &v.replica.doc_keys[oid],
                            meta,
                            recipients.remove(oid).unwrap_or_default(),
                        )
                    })
                    .collect();
                let mut ring = v.replica.keyring.clone();
                let (mut req, rings) =
                    engine::rotate_with_notes(&mut ring, &members, &v.replica.meta, &notes)?;
                // Left out on purpose, so the server knows it's no oversight.
                req.without = left_out
                    .iter()
                    .filter(|m| m.user.account_key.is_some())
                    .map(|m| m.user.id.clone())
                    .collect();
                for o in &mut req.objects {
                    o.update.without = recipients_out.remove(&o.object_id).unwrap_or_default();
                }
                for o in &mut req.objects {
                    if let Some(next) = rings.get(&o.object_id) {
                        members::sign_grants(
                            &self.account,
                            &id,
                            &o.object_id,
                            next,
                            &mut o.update.envelopes,
                        );
                    }
                }
                req.log_entry = members::rotation(&id, &ring).and_then(|body| {
                    v.log.next(
                        &self.account,
                        &self.user_id,
                        &id,
                        body,
                        notekpr_core::now_ms(),
                    )
                });
                (req, ring, rings)
            };
            match self
                .api
                .post::<_, ()>(&format!("/vaults/{id}/keys/rotate"), &req)
                .await
            {
                Ok(()) => {
                    let mut v = vault.lock().await;
                    v.set_keyring(ring);
                    for (oid, next) in rings {
                        v.set_note_keyring(&oid, next);
                    }
                    if let Err(e) = self.load_member_log(&mut v).await {
                        ui::warn(&format!("{e:#}"));
                    }
                    // New notes' keys not registered yet were wrapped under
                    // the old vault key.
                    let waiting: Vec<String> = v
                        .outbox
                        .new_keys
                        .iter()
                        .map(|k| k.object_id.clone())
                        .collect();
                    for oid in waiting {
                        v.key_refused(&oid);
                    }
                    v.save()?;
                    if !not_given.is_empty() {
                        ui::warn(&format!(
                            "the new key of '{}' went to everyone except {}: their account key changed (or the \
                             owner never added them). Verify their fingerprint with them, then give them access \
                             again with `notekpr vault share` (or `share-note` for a single note) and \
                             --accept-new-key",
                            v.name(),
                            not_given.join(", ")
                        ));
                    }
                    return Ok(req.new_epoch);
                }
                // Membership or notes changed meanwhile, or someone else
                // rotated: retry once with fresh keys.
                Err(e) if e.status() == Some(409) && attempt == 0 => {}
                Err(e) => return Err(anyhow!(e).context("rotating the vault key")),
            }
        }
        bail!("could not rotate the vault key (membership keeps changing); try again")
    }
}

fn warn_too_large(vault_name: &str, n: usize, max: usize) {
    ui::warn(&format!(
        "vault '{vault_name}': {n} change(s) are larger than the server accepts ({max} bytes per change); they \
         go out with a copy of the whole note instead. If even that is too large, they stay on this device — \
         ask the administrator to raise NOTEKPR_MAX_UPDATE_KB. Other changes keep syncing"
    ));
}

/// Uniformly random duration in `[0, max)`.
pub fn random_duration(max: Duration) -> Duration {
    let r = u64::from_le_bytes(notekpr_core::crypto::random_bytes::<8>());
    let ms = max.as_millis().max(1) as u64;
    Duration::from_millis(r % ms)
}
