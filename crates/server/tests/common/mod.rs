#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use coolpsonotes_core::doc::{IndexDoc, NoteDoc};
use coolpsonotes_core::keys::{
    self, AccountSecret, DeviceKey, RecoveryKey, VaultKey, VaultKeyring,
};
use coolpsonotes_core::protocol::*;
use coolpsonotes_core::{crypto::Purpose, random_id};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_tungstenite::tungstenite::Message;

pub struct TestServer {
    pub base: String,
    pub ws: String,
    pub dir: tempfile::TempDir,
}

pub async fn start() -> TestServer {
    start_with(|_| {}).await
}

pub async fn start_with(f: impl FnOnce(&mut coolpsonotes_server::config::Config)) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = coolpsonotes_server::config::Config::for_tests(dir.path().to_path_buf());
    f(&mut cfg);
    let (app, _state) = coolpsonotes_server::build(cfg, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestServer {
        base: format!("http://{addr}/api/v1"),
        ws: format!("ws://{addr}/api/v1/ws"),
        dir,
    }
}

#[derive(Clone)]
pub struct Api {
    pub base: String,
    pub token: Option<String>,
    pub http: reqwest::Client,
}

impl Api {
    pub fn new(base: &str) -> Self {
        Api {
            base: base.to_string(),
            token: None,
            http: reqwest::Client::new(),
        }
    }

    pub fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(m, format!("{}{}", self.base, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    pub async fn status<B: Serialize>(
        &self,
        m: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> u16 {
        let mut r = self.req(m, path);
        if let Some(b) = body {
            r = r.json(b);
        }
        r.send().await.unwrap().status().as_u16()
    }

    pub async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        m: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, (u16, String)> {
        let mut r = self.req(m, path);
        if let Some(b) = body {
            r = r.json(b);
        }
        let resp = r.send().await.unwrap();
        let st = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        if !(200..300).contains(&st) {
            return Err((st, text));
        }
        let text = if text.is_empty() {
            "null".to_string()
        } else {
            text
        };
        Ok(serde_json::from_str(&text).unwrap_or_else(|e| panic!("decode {path}: {e}: {text}")))
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> T {
        self.call::<(), T>(reqwest::Method::GET, path, None)
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e:?}"))
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, b: &B) -> T {
        self.call(reqwest::Method::POST, path, Some(b))
            .await
            .unwrap_or_else(|e| panic!("POST {path}: {e:?}"))
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, b: &B) -> T {
        self.call(reqwest::Method::PUT, path, Some(b))
            .await
            .unwrap_or_else(|e| panic!("PUT {path}: {e:?}"))
    }

    pub fn raw(&self) -> reqwest::Client {
        self.http.clone()
    }
}

/// A user with one enrolled device, as the web client would set it up.
pub struct User {
    pub api: Api,
    pub id: String,
    pub name: String,
    pub account: AccountSecret,
    pub device: DeviceKey,
    pub device_id: String,
    pub recovery: RecoveryKey,
    pub keyrings: BTreeMap<String, VaultKeyring>,
}

pub async fn dev_login(base: &str, name: &str) -> (Api, SessionResponse) {
    let mut api = Api::new(base);
    let s: SessionResponse = api
        .post(
            "/auth/dev-login",
            &DevLoginRequest {
                username: name.into(),
                display_name: None,
            },
        )
        .await;
    api.token = Some(s.token.clone());
    (api, s)
}

pub async fn new_user(base: &str, name: &str) -> User {
    let (api, s) = dev_login(base, name).await;
    let account = AccountSecret::generate();
    let device = DeviceKey::generate();
    let device_id = random_id();
    let recovery = RecoveryKey::generate();
    let uid = s.user.id.clone();
    let req = AccountSetupRequest {
        key: PublicAccountKey::new(
            &uid,
            1,
            &account.public(),
            &account.sign_account_key(&uid, 1),
        ),
        recovery_envelope: Some(recovery.wrap_account(&account, &uid)),
        device: Some(FirstDevice {
            registration: DeviceRegistration {
                device_id: device_id.clone(),
                name: "test browser".into(),
                enc_public: device.public.to_vec(),
            },
            account_envelope: keys::wrap_account_for_device(
                &account,
                &device.public,
                &uid,
                &device_id,
            )
            .unwrap(),
            approval_signature: account
                .sign_device_approval(&uid, &device_id, &device.public)
                .to_vec(),
        }),
    };
    let _: () = api.put("/me/account-key", &req).await;
    User {
        api,
        id: uid,
        name: name.into(),
        account,
        device,
        device_id,
        recovery,
        keyrings: BTreeMap::new(),
    }
}

impl User {
    pub async fn create_vault(&mut self, name: &str) -> String {
        let id = random_id();
        let key = VaultKey::generate();
        let mut ring = VaultKeyring::new(&id);
        ring.insert(1, key.clone());
        let meta = ring
            .encrypt_meta(serde_json::json!({ "name": name }).to_string().as_bytes())
            .unwrap();
        let env = keys::wrap_vault_key(&key, &id, 1, &self.id, &self.account.public().enc_public)
            .unwrap();
        let _: VaultInfo = self
            .api
            .post(
                "/vaults",
                &CreateVaultRequest {
                    id: id.clone(),
                    encrypted_meta: meta,
                    key_envelope: KeyEnvelope {
                        user_id: self.id.clone(),
                        epoch: 1,
                        envelope: env,
                        signature: None,
                    },
                    log_entry: None,
                },
            )
            .await;
        self.keyrings.insert(id.clone(), ring);
        id
    }

    /// A vault whose membership log starts with its creation (R3).
    pub async fn create_signed_vault(&mut self, name: &str) -> String {
        let meta = coolpsonotes_core::engine::VaultMeta::named(name);
        let (req, ring) =
            coolpsonotes_core::engine::new_vault(&self.account, &self.id, &meta, 1).unwrap();
        let _: VaultInfo = self.api.post("/vaults", &req).await;
        self.keyrings.insert(req.id.clone(), ring);
        req.id
    }

    /// The vault's membership log as the server has it.
    pub async fn member_log(&self, vault_id: &str) -> Vec<LogEntry> {
        let r: MemberLogResponse = self.api.get(&format!("/vaults/{vault_id}/log")).await;
        r.entries
    }

    /// Sign `body` as the next entry of the vault's log.
    pub async fn sign_next(
        &self,
        vault_id: &str,
        body: coolpsonotes_core::memberlog::LogBody,
    ) -> LogEntry {
        let log = self.member_log(vault_id).await;
        LogEntry::sign(&self.account, &self.id, vault_id, log.last(), body, 1)
    }

    /// Fetch and unwrap this user's vault keys (as any client does on startup).
    pub async fn load_keys(&mut self, vault_id: &str) -> Result<(), u16> {
        let r: VaultKeysResponse = self
            .api
            .call::<(), _>(
                reqwest::Method::GET,
                &format!("/vaults/{vault_id}/keys"),
                None,
            )
            .await
            .map_err(|e| e.0)?;
        let mut ring = VaultKeyring::new(vault_id);
        for e in &r.envelopes {
            ring.insert(
                e.epoch,
                keys::unwrap_vault_key(&self.account, &e.envelope, vault_id, e.epoch, &self.id)
                    .unwrap(),
            );
        }
        let chain = r.chain.iter().map(|c| (c.epoch, c.link.clone())).collect();
        ring.fill_from_chain(&chain).unwrap();
        self.keyrings.insert(vault_id.to_string(), ring);
        Ok(())
    }

    pub fn ring(&self, vault_id: &str) -> &VaultKeyring {
        &self.keyrings[vault_id]
    }

    pub fn encrypt_update(&self, vault_id: &str, object_id: &str, update: &[u8]) -> PushUpdate {
        PushUpdate {
            object_id: object_id.into(),
            client_update_id: random_id(),
            payload: self
                .ring(vault_id)
                .encrypt(Purpose::Update, object_id, update)
                .unwrap(),
            signature: None,
        }
    }

    pub async fn push(
        &self,
        vault_id: &str,
        updates: Vec<PushUpdate>,
    ) -> Result<PushResponse, (u16, String)> {
        self.api
            .call(
                reqwest::Method::POST,
                &format!("/vaults/{vault_id}/updates"),
                Some(&PushRequest { updates }),
            )
            .await
    }

    /// Share a vault: verify the recipient's key and wrap the current vault key to it.
    pub async fn share(
        &self,
        vault_id: &str,
        with: &str,
        role: Role,
    ) -> Result<MemberInfo, (u16, String)> {
        let u: PublicUser = self.api.get(&format!("/users/{with}")).await;
        let public = u.account_key.as_ref().unwrap().verify().unwrap();
        let (epoch, key) = self.ring(vault_id).current().unwrap();
        let env = keys::wrap_vault_key(key, vault_id, epoch, with, &public.enc_public).unwrap();
        self.api
            .call(
                reqwest::Method::POST,
                &format!("/vaults/{vault_id}/members"),
                Some(&AddMemberRequest {
                    user_id: with.into(),
                    role,
                    key_envelope: Some(KeyEnvelope {
                        user_id: with.into(),
                        epoch,
                        envelope: env,
                        signature: None,
                    }),
                    log_entry: None,
                }),
            )
            .await
    }

    /// Rotate the vault key to a new epoch for the remaining members.
    pub async fn rotate(&mut self, vault_id: &str) {
        let members: Vec<MemberInfo> = self.api.get(&format!("/vaults/{vault_id}/members")).await;
        let (epoch, old) = {
            let (e, k) = self.ring(vault_id).current().unwrap();
            (e, k.clone())
        };
        let new_epoch = epoch + 1;
        let new = VaultKey::generate();
        let envelopes = members
            .iter()
            .filter_map(|m| {
                m.user
                    .account_key
                    .as_ref()
                    .map(|k| (m, k.verify().unwrap()))
            })
            .map(|(m, pk)| KeyEnvelope {
                user_id: m.user.id.clone(),
                epoch: new_epoch,
                envelope: keys::wrap_vault_key(
                    &new,
                    vault_id,
                    new_epoch,
                    &m.user.id,
                    &pk.enc_public,
                )
                .unwrap(),
                signature: None,
            })
            .collect();
        // Every note with its own key moves on too (R2).
        let vk: VaultKeysResponse = self.api.get(&format!("/vaults/{vault_id}/keys")).await;
        let doc_rings = coolpsonotes_core::engine::object_keyrings(self.ring(vault_id), &vk);
        let ring = self.keyrings.get_mut(vault_id).unwrap();
        ring.insert(new_epoch, new.clone());
        let meta = ring.encrypt_meta(br#"{"name":"rotated"}"#).unwrap();
        let mut objects = Vec::new();
        for (oid, doc_ring) in &doc_rings {
            let shares: Vec<ObjectShareInfo> = self
                .api
                .get(&format!("/vaults/{vault_id}/objects/{oid}/shares"))
                .await;
            let users: Vec<PublicUser> = shares.into_iter().map(|s| s.user).collect();
            let (_, update) = coolpsonotes_core::engine::rotate_object_key(
                self.keyrings.get(vault_id).unwrap(),
                doc_ring,
                oid,
                "rotated",
                &users,
            )
            .unwrap();
            objects.push(ObjectKeyChange {
                object_id: oid.clone(),
                update,
            });
        }
        let _: () = self
            .api
            .post(
                &format!("/vaults/{vault_id}/keys/rotate"),
                &RotateKeysRequest {
                    new_epoch,
                    chain_link: keys::encrypt_chain_link(&new, new_epoch, &old, vault_id),
                    encrypted_meta: meta,
                    envelopes,
                    objects,
                    log_entry: None,
                },
            )
            .await;
    }

    /// Rebuild a vault's index and notes from the server, decrypting everything.
    pub async fn read_vault(&self, vault_id: &str) -> (IndexDoc, BTreeMap<String, NoteDoc>) {
        let ch: ChangesResponse = self
            .api
            .get(&format!("/vaults/{vault_id}/changes?since=0"))
            .await;
        decode_changes(self.ring(vault_id), &[ch])
    }
}

pub fn decode_changes(
    ring: &VaultKeyring,
    pages: &[ChangesResponse],
) -> (IndexDoc, BTreeMap<String, NoteDoc>) {
    let index = IndexDoc::new();
    let mut notes: BTreeMap<String, NoteDoc> = BTreeMap::new();
    let mut apply = |object_id: &str, plain: Vec<u8>| {
        if object_id == INDEX_OBJECT_ID {
            index.apply_update(&plain).unwrap();
        } else {
            notes
                .entry(object_id.to_string())
                .or_default()
                .apply_update(&plain)
                .unwrap();
        }
    };
    for ch in pages {
        for s in &ch.snapshots {
            apply(
                &s.object_id,
                ring.decrypt(Purpose::Snapshot, &s.object_id, &s.payload)
                    .unwrap(),
            );
        }
        for u in &ch.updates {
            apply(
                &u.object_id,
                ring.decrypt(Purpose::Update, &u.object_id, &u.payload)
                    .unwrap(),
            );
        }
    }
    (index, notes)
}

pub struct Ws {
    pub stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Ws {
    pub async fn connect(url: &str, token: &str) -> Ws {
        let (mut stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let auth = serde_json::to_string(&ClientMessage::Auth {
            token: token.into(),
        })
        .unwrap();
        stream.send(Message::Text(auth.into())).await.unwrap();
        let mut ws = Ws { stream };
        match ws.recv().await {
            ServerMessage::Ready { .. } => {}
            m => panic!("expected ready, got {m:?}"),
        }
        ws
    }

    pub async fn send(&mut self, m: &ClientMessage) {
        self.stream
            .send(Message::Text(serde_json::to_string(m).unwrap().into()))
            .await
            .unwrap();
    }

    pub async fn recv(&mut self) -> ServerMessage {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), self.stream.next())
                .await
                .expect("timed out waiting for a websocket message")
                .expect("stream ended")
                .unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    /// Receive until a message matches.
    pub async fn until<T>(&mut self, mut f: impl FnMut(&ServerMessage) -> Option<T>) -> T {
        loop {
            let m = self.recv().await;
            if let Some(t) = f(&m) {
                return t;
            }
        }
    }

    pub async fn expect_silence(&mut self, ms: u64) {
        if let Ok(Some(Ok(Message::Text(t)))) =
            tokio::time::timeout(Duration::from_millis(ms), self.stream.next()).await
        {
            panic!("unexpected message: {t}");
        }
    }
}
