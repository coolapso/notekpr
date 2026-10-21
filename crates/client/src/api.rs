//! Typed HTTP client for the server's REST API (`docs/protocol.md`).
//!
//! Handles bearer authentication for both session tokens (people) and
//! short-lived OAuth client-credentials tokens (agents, refreshed
//! automatically before they expire or after a 401). Tokens are never logged.

use std::path::PathBuf;
use std::time::Duration;

use notekpr_core::protocol::*;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The server answered with an error status.
    #[error("{message} (HTTP {status})")]
    Status {
        status: u16,
        code: String,
        message: String,
    },
    #[error("cannot reach the server: {0}")]
    Network(String),
    #[error("unexpected response from the server: {0}")]
    Decode(String),
    #[error("{0}")]
    Auth(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn is_network(&self) -> bool {
        matches!(self, ApiError::Network(_))
    }

    /// The server cannot be used right now: unreachable, or answering with a
    /// server error (a reverse proxy's 502/503/504 while it restarts). Work
    /// offline and retry later, as for a network failure.
    pub fn is_offline(&self) -> bool {
        self.is_network() || self.status().is_some_and(|s| (500..600).contains(&s))
    }

    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(401)
    }

    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// The server says this device was removed from the account (not just
    /// any 404, which a misrouted proxy can produce too).
    pub fn is_device_revoked(&self) -> bool {
        matches!(self, ApiError::Status { status: 404, code, .. } if code == "device_revoked")
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// How requests are authenticated.
#[derive(Clone, Debug)]
pub enum Credentials {
    None,
    /// A session token (dev login, device grant).
    Session(String),
    /// Agent client credentials; the secret is re-read from its file whenever
    /// a fresh access token is needed.
    Agent {
        client_id: String,
        secret_file: PathBuf,
    },
}

/// Outcome of one device-grant poll.
pub enum DevicePoll {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Token(TokenResponse),
}

pub struct Api {
    server: String,
    base: String,
    http: reqwest::Client,
    creds: Credentials,
    /// Cached agent access token and its expiry (Unix ms).
    agent_token: Mutex<Option<(String, i64)>>,
}

fn net(e: reqwest::Error) -> ApiError {
    // reqwest errors can include the URL; never include query strings or bodies.
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_request() {
        "request failed"
    } else {
        "transport error"
    };
    ApiError::Network(kind.to_string())
}

pub fn read_secret_file(path: &std::path::Path) -> Result<String, ApiError> {
    let s = std::fs::read_to_string(path).map_err(|e| {
        ApiError::Auth(format!(
            "cannot read the client secret file {}: {e}",
            path.display()
        ))
    })?;
    let s = s.trim().to_string();
    if s.is_empty() {
        return Err(ApiError::Auth(format!(
            "the client secret file {} is empty",
            path.display()
        )));
    }
    Ok(s)
}

impl Api {
    /// `server` is the origin as stored in the profile (see `normalize_server_url`).
    pub fn new(server: &str, creds: Credentials) -> ApiResult<Api> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("notekpr-client/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ApiError::Network(format!("cannot initialise HTTP client: {e}")))?;
        Ok(Api {
            server: server.trim_end_matches('/').to_string(),
            base: format!("{}{}", server.trim_end_matches('/'), API_PREFIX),
            http,
            creds,
            agent_token: Mutex::new(None),
        })
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn ws_url(&self) -> String {
        let rest = self
            .base
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"));
        rest.unwrap_or_else(|| self.base.replacen("http://", "ws://", 1)) + "/ws"
    }

    pub fn credentials(&self) -> &Credentials {
        &self.creds
    }

    /// The bearer token to use now, refreshing agent tokens when needed.
    pub async fn token(&self) -> ApiResult<String> {
        match &self.creds {
            Credentials::None => Err(ApiError::Auth("not signed in".into())),
            Credentials::Session(t) => Ok(t.clone()),
            Credentials::Agent {
                client_id,
                secret_file,
            } => {
                let mut cache = self.agent_token.lock().await;
                if let Some((t, exp)) = &*cache
                    && *exp - notekpr_core::now_ms() > 60_000
                {
                    return Ok(t.clone());
                }
                let secret = zeroize::Zeroizing::new(read_secret_file(secret_file)?);
                let tok = self.client_credentials(client_id, &secret).await?;
                let exp = notekpr_core::now_ms() + (tok.expires_in as i64) * 1000;
                *cache = Some((tok.access_token.clone(), exp));
                Ok(tok.access_token)
            }
        }
    }

    /// Forget a cached agent token (after the server rejected it).
    pub async fn invalidate_token(&self) {
        *self.agent_token.lock().await = None;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Body<'_>,
    ) -> ApiResult<reqwest::Response> {
        let mut retried = false;
        let mut waited = Duration::ZERO;
        loop {
            let token = self.token().await?;
            let mut req = self
                .http
                .request(method.clone(), self.url(path))
                .bearer_auth(&token);
            req = match &body {
                Body::None => req,
                Body::Json(v) => req.json(v),
                Body::Bytes(b) => req
                    .header("content-type", "application/octet-stream")
                    .body(b.to_vec()),
            };
            let resp = req.send().await.map_err(net)?;
            if let Some(wait) = retry_after(&resp)
                && waited + wait <= MAX_RATE_LIMIT_WAIT
            {
                // Rate limited before it was handled: the same request, later.
                tracing::debug!("rate limited; retrying in {} s", wait.as_secs());
                tokio::time::sleep(wait).await;
                waited += wait;
                continue;
            }
            if resp.status() == StatusCode::UNAUTHORIZED
                && matches!(self.creds, Credentials::Agent { .. })
                && !retried
            {
                // The agent token may have expired early or been rotated.
                self.invalidate_token().await;
                retried = true;
                continue;
            }
            return check(resp).await;
        }
    }

    pub async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> ApiResult<T> {
        let body = match body {
            Some(b) => {
                Body::Json(serde_json::to_value(b).map_err(|e| ApiError::Decode(e.to_string()))?)
            }
            None => Body::None,
        };
        let resp = self.send(method, path, body).await?;
        decode(resp).await
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        self.request(Method::GET, path, None::<&()>).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<T> {
        self.request(Method::POST, path, Some(body)).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<T> {
        self.request(Method::PUT, path, Some(body)).await
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<T> {
        self.request(Method::PATCH, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> ApiResult<()> {
        self.request(Method::DELETE, path, None::<&()>).await
    }

    /// `DELETE` with a JSON body (e.g. a membership log entry).
    pub async fn delete_with<B: Serialize>(&self, path: &str, body: &B) -> ApiResult<()> {
        self.request(Method::DELETE, path, Some(body)).await
    }

    pub async fn get_bytes(&self, path: &str) -> ApiResult<Vec<u8>> {
        let resp = self.send(Method::GET, path, Body::None).await?;
        Ok(resp.bytes().await.map_err(net)?.to_vec())
    }

    pub async fn put_bytes(&self, path: &str, data: &[u8]) -> ApiResult<()> {
        self.send(Method::PUT, path, Body::Bytes(data)).await?;
        Ok(())
    }

    // ------------------------------------------------------------ unauthenticated

    pub async fn info(&self) -> ApiResult<ServerInfo> {
        let resp = self.http.get(self.url("/info")).send().await.map_err(net)?;
        decode(check(resp).await?).await
    }

    pub async fn dev_login(&self, username: &str) -> ApiResult<SessionResponse> {
        let req = DevLoginRequest {
            username: username.to_string(),
            display_name: None,
        };
        let resp = self
            .http
            .post(self.url("/auth/dev-login"))
            .json(&req)
            .send()
            .await
            .map_err(net)?;
        decode(check(resp).await?).await
    }

    pub async fn device_authorization(
        &self,
        client_name: &str,
    ) -> ApiResult<DeviceAuthorizationResponse> {
        let resp = self
            .http
            .post(self.url("/oauth/device_authorization"))
            .form(&[("client_id", "notekpr-cli"), ("client_name", client_name)])
            .send()
            .await
            .map_err(net)?;
        decode(check(resp).await?).await
    }

    pub async fn poll_device_token(&self, device_code: &str) -> ApiResult<DevicePoll> {
        let resp = self
            .http
            .post(self.url("/oauth/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
            ])
            .send()
            .await
            .map_err(net)?;
        if resp.status().is_success() {
            return Ok(DevicePoll::Token(decode(resp).await?));
        }
        let status = resp.status().as_u16();
        let err: OAuthError = resp
            .json()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        Ok(match err.error.as_str() {
            "authorization_pending" => DevicePoll::Pending,
            "slow_down" => DevicePoll::SlowDown,
            "access_denied" => DevicePoll::Denied,
            "expired_token" => DevicePoll::Expired,
            other => {
                return Err(ApiError::Status {
                    status,
                    code: other.to_string(),
                    message: err.error_description.unwrap_or_else(|| other.to_string()),
                });
            }
        })
    }

    pub async fn client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> ApiResult<TokenResponse> {
        let resp = self
            .http
            .post(self.url("/oauth/token"))
            .basic_auth(client_id, Some(client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(net)?;
        if resp.status().is_success() {
            return decode(resp).await;
        }
        let status = resp.status().as_u16();
        let err: Option<OAuthError> = resp.json().await.ok();
        let code = err.map(|e| e.error).unwrap_or_else(|| "error".into());
        Err(ApiError::Auth(if code == "invalid_client" {
            "the server rejected the agent credentials (wrong, revoked or rotated client secret)"
                .into()
        } else {
            format!("token request failed: {code} (HTTP {status})")
        }))
    }

    // ------------------------------------------------------------ common calls

    pub async fn me(&self) -> ApiResult<MeResponse> {
        self.get("/me").await
    }

    pub async fn vaults(&self) -> ApiResult<Vec<VaultInfo>> {
        self.get("/vaults").await
    }

    pub async fn vault(&self, v: &str) -> ApiResult<VaultInfo> {
        self.get(&format!("/vaults/{v}")).await
    }

    pub async fn vault_keys(&self, v: &str) -> ApiResult<VaultKeysResponse> {
        self.get(&format!("/vaults/{v}/keys")).await
    }

    pub async fn members(&self, v: &str) -> ApiResult<Vec<MemberInfo>> {
        self.get(&format!("/vaults/{v}/members")).await
    }

    pub async fn changes(
        &self,
        v: &str,
        since: i64,
        limit: i64,
        snapshots: bool,
    ) -> ApiResult<ChangesResponse> {
        self.get(&format!(
            "/vaults/{v}/changes?since={since}&limit={limit}&snapshots={snapshots}"
        ))
        .await
    }

    pub async fn push(&self, v: &str, updates: Vec<PushUpdate>) -> ApiResult<PushResponse> {
        self.post(&format!("/vaults/{v}/updates"), &PushRequest { updates })
            .await
    }

    pub async fn create_snapshot(
        &self,
        v: &str,
        o: &str,
        req: &CreateSnapshotRequest,
    ) -> ApiResult<SnapshotInfo> {
        self.post(&format!("/vaults/{v}/objects/{o}/snapshots"), req)
            .await
    }

    pub async fn user(&self, id: &str) -> ApiResult<PublicUser> {
        self.get(&format!("/users/{id}")).await
    }

    pub async fn search_users(&self, q: &str) -> ApiResult<Vec<PublicUser>> {
        let q: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
        self.get(&format!("/users/search?q={q}")).await
    }
}

/// How long one request may keep waiting for the server's rate limit
/// (scripts and agents sending many commands); then it fails as `429`.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);

/// How long a `429` says to wait. Only a `Retry-After` means "the same
/// request will work then"; other `429`s (a snapshot within its cooldown)
/// are answers, not a busy server.
fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    if resp.status() != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs.clamp(1, 10)))
}

enum Body<'a> {
    None,
    Json(serde_json::Value),
    Bytes(&'a [u8]),
}

async fn check(resp: reqwest::Response) -> ApiResult<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = status.as_u16();
    let text = resp.text().await.unwrap_or_default();
    let (err, message) = match serde_json::from_str::<ApiError2>(&text) {
        Ok(e) => (e.error, e.message),
        Err(_) => (
            "http_error".to_string(),
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_string(),
        ),
    };
    Err(ApiError::Status {
        status: code,
        code: err,
        message,
    })
}

#[derive(serde::Deserialize)]
struct ApiError2 {
    error: String,
    message: String,
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> ApiResult<T> {
    let bytes = resp.bytes().await.map_err(net)?;
    let bytes: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
    serde_json::from_slice(bytes).map_err(|e| ApiError::Decode(e.to_string()))
}
