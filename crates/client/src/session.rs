//! A signed-in, unlocked profile: API client + account secret + local key,
//! and the network half of vault synchronisation (REST catch-up, pushing
//! pending updates and attachments, compaction, key management, TOFU pins).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::engine::{self, Applied, PinStatus, VaultMeta, VaultReplica};
use coolpsonotes_core::keys::{AccountSecret, VaultKeyring};
use coolpsonotes_core::protocol::*;
use tokio::sync::{Mutex, OnceCell};

use crate::api::{Api, ApiError, Credentials};
use crate::profile::{Dirs, Profile, ProfileStore};
use crate::secrets::{LocalKey, SecretStore};
use crate::store::{VaultStore, valid_id};
use crate::ui;
use crate::vault::{Vault, merge_applied};

pub type SharedVault = Arc<Mutex<Vault>>;

/// Server pages are capped at 5000; 1000 keeps responses small.
const PAGE: i64 = 1000;
/// Updates per push request (server maximum is 500).
const PUSH_BATCH: usize = 200;
/// Approximate payload bytes per push request.
const PUSH_BYTES: usize = 4 * 1024 * 1024;

pub struct Session {
    pub profiles: ProfileStore,
    pub profile: Profile,
    pub api: Api,
    pub user_id: String,
    pub kind: PrincipalKind,
    pub(crate) account: AccountSecret,
    pub local_key: Arc<LocalKey>,
    info: OnceCell<ServerInfo>,
}

/// What happened during a push.
#[derive(Debug, Default, Clone)]
pub struct PushOutcome {
    pub pushed: usize,
    pub blobs: usize,
    /// Updates still waiting (offline, or not allowed to write).
    pub remaining: usize,
    pub offline: bool,
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
            anyhow!(
                "profile '{profile}' is not signed in; run `coolpsonotes --profile {profile} login`"
            )
        })?;
        if p.device_pending {
            bail!(
                "this device is still waiting for approval; run `coolpsonotes --profile {profile} login` again to \
                 finish (approve it from another device, or use your recovery key)"
            );
        }
        let secrets = SecretStore::open(&profiles.data_dir())?;
        let account = secrets.load_account(&user_id)?.ok_or_else(|| {
            anyhow!(
                "no account keys on this device for profile '{profile}'; run `coolpsonotes --profile {profile} login`"
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
        Ok(Session {
            profiles,
            kind: p.kind,
            profile: p,
            api,
            user_id,
            account,
            local_key: Arc::new(secrets.into_local_key()),
            info: OnceCell::new(),
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

    pub async fn server_info(&self) -> Result<&ServerInfo, ApiError> {
        self.info.get_or_try_init(|| self.api.info()).await
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
        Vault::load(self.vault_store(vault_id)?)
            .with_context(|| format!("loading the local copy of vault {vault_id}"))
    }

    // ------------------------------------------------------------ keys

    /// Fetch and unwrap this principal's keys for a vault.
    pub async fn keyring(&self, vault_id: &str) -> Result<VaultKeyring> {
        Ok(self.keys(vault_id).await?.0)
    }

    /// Vault keyring plus the document keyrings of its individually shared notes.
    pub async fn keys(
        &self,
        vault_id: &str,
    ) -> Result<(
        VaultKeyring,
        std::collections::HashMap<String, VaultKeyring>,
    )> {
        let r = self.api.vault_keys(vault_id).await?;
        let ring = engine::keyring_from_response(&self.account, &self.user_id, vault_id, &r)?;
        let docs = engine::object_keyrings(&ring, &r);
        Ok((ring, docs))
    }

    /// Open a vault the server listed: the local replica if there is one
    /// (refreshing keys, role and metadata), otherwise a fresh replica.
    pub async fn open_vault(&self, info: &VaultInfo) -> Result<Vault> {
        let store = self.vault_store(&info.id)?;
        let mut v = match Vault::load(store.clone())? {
            Some(v) => v,
            None => {
                let (ring, docs) = self.keys(&info.id).await?;
                let meta = VaultMeta::decrypt(&ring, &info.encrypted_meta).unwrap_or_else(|e| {
                    tracing::warn!("cannot decrypt vault metadata: {e}");
                    VaultMeta::named(&info.id)
                });
                let mut replica = VaultReplica::new(&info.id, info.role, ring, meta);
                replica.doc_keys = docs;
                let mut v = Vault::new(store, replica, &info.owner_id, info.archived);
                v.save()?;
                v
            }
        };
        self.apply_info(&mut v, info).await?;
        Ok(v)
    }

    /// Bring role, archive flag, keys and name in line with the server.
    pub async fn apply_info(&self, v: &mut Vault, info: &VaultInfo) -> Result<()> {
        if v.replica.keyring.current_epoch().unwrap_or(0) < info.key_epoch {
            let (ring, docs) = self.keys(&info.id).await?;
            v.set_keys(ring, docs);
        }
        v.replica.role = info.role;
        v.owner_id = info.owner_id.clone();
        v.archived = info.archived;
        v.read_only = None;
        match VaultMeta::decrypt(&v.replica.keyring, &info.encrypted_meta) {
            Ok(m) => v.set_meta(m),
            Err(e) => tracing::warn!(vault = %info.id, "cannot decrypt vault metadata: {e}"),
        }
        v.mark_state_dirty();
        v.save()
    }

    pub async fn refresh_keys(&self, vault: &SharedVault) -> Result<()> {
        let id = vault.lock().await.id().to_string();
        let (ring, docs) = self.keys(&id).await?;
        let mut v = vault.lock().await;
        v.set_keys(ring, docs);
        v.save()
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
                v.last_sync = Some(coolpsonotes_core::now_ms());
                if let Err(e) = v.save() {
                    ui::warn(&format!("could not save the local copy: {e:#}"));
                }
                // Some content needs a key we don't have yet (a note got its own
                // key, or the vault key rotated): fetch keys and replay it.
                if v.replica.needs_keys() {
                    drop(v);
                    if let Err(e) = self.refresh_keys(vault).await {
                        ui::warn(&format!("could not refresh keys: {e:#}"));
                    }
                }
                return Ok(all);
            }
        }
    }

    /// Upload pending attachments, then pending updates in batches.
    /// Network failures leave everything queued (and report `offline`).
    pub async fn push(&self, vault: &SharedVault) -> Result<PushOutcome> {
        let mut out = PushOutcome::default();
        let (id, blobs, writable) = {
            let v = vault.lock().await;
            (
                v.id().to_string(),
                v.pending_blobs.clone(),
                v.read_only.is_none() && v.replica.can_write(),
            )
        };
        if !writable {
            out.remaining = vault.lock().await.replica.pending.len();
            return Ok(out);
        }
        for blob in blobs {
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
                Err(e) if e.is_network() => {
                    out.offline = true;
                    out.remaining = vault.lock().await.replica.pending.len();
                    return Ok(out);
                }
                Err(e) => return Err(anyhow!(e).context("uploading an attachment")),
            }
        }
        let mut individually = false;
        let mut rekeyed = false;
        loop {
            let batch = {
                let v = vault.lock().await;
                let all = v.replica.pending_pushes()?;
                let mut batch = Vec::new();
                let mut bytes = 0;
                let limit = if individually { 1 } else { PUSH_BATCH };
                for p in all {
                    if !batch.is_empty()
                        && (batch.len() >= limit || bytes + p.payload.len() > PUSH_BYTES)
                    {
                        break;
                    }
                    bytes += p.payload.len();
                    batch.push(p);
                }
                batch
            };
            if batch.is_empty() {
                break;
            }
            let ids: Vec<String> = batch.iter().map(|p| p.client_update_id.clone()).collect();
            match self.api.push(&id, batch).await {
                Ok(resp) => {
                    let mut v = vault.lock().await;
                    v.check_rollback(resp.head_seq);
                    out.pushed += resp.results.len();
                    v.ack(&resp.results, false);
                    v.save()?;
                }
                Err(e) if e.is_network() => {
                    out.offline = true;
                    break;
                }
                Err(e) if e.status() == Some(409) || e.status() == Some(413) => {
                    if !individually {
                        // Find the offending update(s) by pushing one at a time.
                        individually = true;
                        if e.status() == Some(409) {
                            let _ = self.catch_up(vault).await;
                        }
                        continue;
                    }
                    let mut v = vault.lock().await;
                    if e.status() == Some(409) {
                        // Writing to a document that was permanently deleted elsewhere.
                        v.drop_pending(&ids);
                        v.save()?;
                        tracing::warn!(vault = %id, "dropped a change to a document that was permanently deleted");
                    } else {
                        bail!(
                            "a change is larger than the server accepts ({e}); it stays queued locally — ask the \
                             administrator to raise COOLPSONOTES_MAX_UPDATE_KB"
                        );
                    }
                }
                // A note became individually shared (or its key rotated): fetch
                // the new keys; pending changes are re-encrypted on the next pass.
                Err(e) if e.status() == Some(412) && !rekeyed => {
                    rekeyed = true;
                    self.refresh_keys(vault).await?;
                    continue;
                }
                Err(e) if e.status() == Some(403) => {
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
                    return Err(
                        anyhow!(e).context("the vault no longer exists or you lost access to it")
                    );
                }
                Err(e) => return Err(anyhow!(e).context("uploading changes")),
            }
        }
        out.remaining = vault.lock().await.replica.pending.len();
        Ok(out)
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
            Err(e) if e.is_network() => return Ok(false),
            Err(e) => return Err(anyhow!(e).context("catching up before a snapshot")),
        }
        let req = {
            let v = vault.lock().await;
            if v.replica.pending.iter().any(|p| p.object_id == oid)
                || v.replica.purged.contains(oid)
            {
                return Ok(false);
            }
            if oid != INDEX_OBJECT_ID && !v.replica.notes.contains_key(oid) {
                return Ok(false);
            }
            if v.replica.seq < 1 {
                return Ok(false);
            }
            v.replica.snapshot_request(oid)?
        };
        match self.api.create_snapshot(id, oid, &req).await {
            Ok(_) => {
                let mut v = vault.lock().await;
                v.replica.snapshot_created(oid);
                v.mark_state_dirty();
                v.save()?;
                Ok(true)
            }
            Err(e) if e.is_network() => Ok(false),
            Err(e) => Err(anyhow!(e).context("uploading a snapshot")),
        }
    }

    /// Pending + catch-up + compaction for one vault.
    pub async fn sync_vault(&self, vault: &SharedVault) -> Result<PushOutcome> {
        self.catch_up(vault).await?;
        let out = self.push(vault).await?;
        if !out.offline {
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
        v.store().save_blob(attachment_id, &ct)?;
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
                     Verify it with them, then run `coolpsonotes vault share` again with --accept-new-key",
                    m.user.display_name, vu.fingerprint
                )),
                Ok(Some(vu)) => {
                    if vu.pin == PinStatus::New {
                        ui::status(&format!(
                            "pinned the key of {} ({})",
                            m.user.display_name, vu.fingerprint
                        ));
                    }
                    ok.push(m.clone());
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

    /// Move the vault to a new key epoch for the current members. Refuses if a
    /// member's key no longer matches its pin.
    pub async fn rotate_keys(&self, vault: &SharedVault) -> Result<u32> {
        let id = vault.lock().await.id().to_string();
        for attempt in 0..2 {
            let members = self.api.members(&id).await?;
            for m in &members {
                if let Some(vu) = self.verify_user(&m.user)?
                    && vu.pin == PinStatus::Changed
                {
                    bail!(
                        "the account key of {} changed since it was pinned (now {}); refusing to give them the \
                             new vault key. Verify it, then re-share with --accept-new-key",
                        m.user.display_name,
                        vu.fingerprint
                    );
                }
            }
            let (req, ring) = {
                let v = vault.lock().await;
                let mut ring = v.replica.keyring.clone();
                let req = engine::rotate(&mut ring, &members, &v.replica.meta)?;
                (req, ring)
            };
            match self
                .api
                .post::<_, ()>(&format!("/vaults/{id}/keys/rotate"), &req)
                .await
            {
                Ok(()) => {
                    {
                        let mut v = vault.lock().await;
                        v.set_keyring(ring);
                        v.save()?;
                    }
                    // Removed members knew the document keys of shared notes too.
                    self.rotate_document_keys(vault).await?;
                    return Ok(req.new_epoch);
                }
                // Membership changed meanwhile, or someone else rotated: retry once.
                Err(e) if e.status() == Some(409) && attempt == 0 => {
                    self.refresh_keys(vault).await?;
                }
                Err(e) => return Err(anyhow!(e).context("rotating the vault key")),
            }
        }
        bail!("could not rotate the vault key (membership keeps changing); try again")
    }
}

impl Session {
    /// Give every individually shared note of a vault a fresh document key,
    /// wrapped under the vault's current key and for its current recipients.
    pub async fn rotate_document_keys(&self, vault: &SharedVault) -> Result<()> {
        let (id, notes) = {
            let v = vault.lock().await;
            (
                v.id().to_string(),
                v.replica.doc_keys.keys().cloned().collect::<Vec<_>>(),
            )
        };
        for oid in notes {
            let shares: Vec<coolpsonotes_core::protocol::ObjectShareInfo> = self
                .api
                .get(&format!("/vaults/{id}/objects/{oid}/shares"))
                .await?;
            let users: Vec<_> = shares.into_iter().map(|s| s.user).collect();
            let (next, req) = {
                let v = vault.lock().await;
                let title = v
                    .replica
                    .index
                    .get(&oid)
                    .map(|e| e.title().to_string())
                    .unwrap_or_default();
                engine::rotate_object_key(
                    &v.replica.keyring,
                    &v.replica.doc_keys[&oid],
                    &oid,
                    &title,
                    &users,
                )?
            };
            self.api
                .put::<_, ()>(&format!("/vaults/{id}/objects/{oid}/key"), &req)
                .await?;
            let mut v = vault.lock().await;
            v.replica.doc_keys.insert(oid, next);
            v.mark_state_dirty();
            v.save()?;
        }
        Ok(())
    }
}

/// Uniformly random duration in `[0, max)`.
pub fn random_duration(max: Duration) -> Duration {
    let r = u64::from_le_bytes(coolpsonotes_core::crypto::random_bytes::<8>());
    let ms = max.as_millis().max(1) as u64;
    Duration::from_millis(r % ms)
}
