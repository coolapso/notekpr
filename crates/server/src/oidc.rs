//! OpenID Connect relying party (authorization code flow with PKCE).
//!
//! Works with any standards-compliant provider (Authelia, Keycloak, Authentik,
//! Dex, ...). ID tokens are verified by `openidconnect`: signature against the
//! provider's JWKS, issuer, audience, expiry and nonce.

use std::time::{Duration, Instant};

use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreClientAuthMethod, CoreProviderMetadata,
};
use openidconnect::{
    AuthType, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpRequest, HttpResponse,
    IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};
use tokio::sync::RwLock;

use crate::config::{Config, TokenAuth};
use crate::error::{AppError, AppResult};

const METADATA_TTL: Duration = Duration::from_secs(3600);
/// Provider responses (discovery, JWKS, tokens, userinfo) are small.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

pub struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: Vec<String>,
    redirect_uri: String,
    token_auth: TokenAuth,
    allow_unverified_email: bool,
    http: reqwest::Client,
    meta: RwLock<Option<(CoreProviderMetadata, Instant)>>,
}

#[derive(Debug)]
struct HttpError(String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

/// HTTP for the OIDC library, through the server's own client: it trusts the
/// system's certificate store (plus `--oidc-ca-file`), never follows
/// redirects and caps response sizes.
async fn send(http: reqwest::Client, req: HttpRequest) -> Result<HttpResponse, HttpError> {
    let err = |e: &dyn std::fmt::Display| HttpError(e.to_string());
    let (parts, body) = req.into_parts();
    let mut resp = http
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(|e| err(&e))?;
    let mut out = openidconnect::http::Response::builder().status(resp.status());
    for (k, v) in resp.headers() {
        out = out.header(k, v);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| err(&e))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(HttpError("identity provider response too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    out.body(body).map_err(|e| err(&e))
}

pub struct AuthStart {
    pub url: String,
    pub state: String,
    pub pkce_verifier: String,
    pub nonce: String,
}

pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub display_name: String,
    pub email: Option<String>,
}

fn upstream(e: impl std::fmt::Display) -> AppError {
    tracing::warn!(error = %e, "OIDC provider error");
    AppError::Unavailable("identity provider unavailable or misconfigured".into())
}

impl Oidc {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Option<Self>> {
        let (Some(issuer), Some(client_id)) = (cfg.oidc_issuer.clone(), cfg.oidc_client_id.clone())
        else {
            return Ok(None);
        };
        let mut builder = reqwest::Client::builder()
            // Following redirects would open the door to SSRF.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15));
        if let Some(path) = &cfg.oidc_ca_file {
            let pem = std::fs::read(path)
                .map_err(|e| anyhow::anyhow!("reading --oidc-ca-file {}: {e}", path.display()))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|e| anyhow::anyhow!("--oidc-ca-file {}: {e}", path.display()))?;
            builder = builder.tls_certs_merge(certs);
        }
        let http = builder.build()?;
        Ok(Some(Oidc {
            issuer,
            client_id,
            client_secret: cfg.oidc_client_secret.clone(),
            scopes: cfg
                .oidc_scopes
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            redirect_uri: format!("{}/api/v1/auth/oidc/callback", cfg.public_url()),
            token_auth: cfg.oidc_token_auth,
            allow_unverified_email: cfg.oidc_allow_unverified_email,
            http,
            meta: RwLock::new(None),
        }))
    }

    fn http_client(
        &self,
    ) -> impl Fn(
        HttpRequest,
    )
        -> std::pin::Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send>> {
        let http = self.http.clone();
        move |req| Box::pin(send(http.clone(), req))
    }

    /// How to send the client secret: configured, or what the provider
    /// advertises (`client_secret_basic` is the default in the spec).
    fn auth_type(&self, meta: &CoreProviderMetadata) -> AuthType {
        match self.token_auth {
            TokenAuth::ClientSecretBasic => AuthType::BasicAuth,
            TokenAuth::ClientSecretPost => AuthType::RequestBody,
            TokenAuth::Auto => match meta.token_endpoint_auth_methods_supported() {
                Some(m)
                    if !m.contains(&CoreClientAuthMethod::ClientSecretBasic)
                        && m.contains(&CoreClientAuthMethod::ClientSecretPost) =>
                {
                    AuthType::RequestBody
                }
                _ => AuthType::BasicAuth,
            },
        }
    }

    async fn metadata(&self, force: bool) -> AppResult<CoreProviderMetadata> {
        if !force
            && let Some((m, at)) = &*self.meta.read().await
            && at.elapsed() < METADATA_TTL
        {
            return Ok(m.clone());
        }
        let issuer = IssuerUrl::new(self.issuer.clone()).map_err(upstream)?;
        let m = CoreProviderMetadata::discover_async(issuer, &self.http_client())
            .await
            .map_err(upstream)?;
        *self.meta.write().await = Some((m.clone(), Instant::now()));
        Ok(m)
    }

    pub async fn start(&self) -> AppResult<AuthStart> {
        let meta = self.metadata(false).await?;
        let client = CoreClient::from_provider_metadata(
            meta,
            ClientId::new(self.client_id.clone()),
            self.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(RedirectUrl::new(self.redirect_uri.clone()).map_err(upstream)?);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut req = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );
        for s in &self.scopes {
            if s != "openid" {
                req = req.add_scope(Scope::new(s.clone()));
            }
        }
        let (url, state, nonce) = req.set_pkce_challenge(challenge).url();
        Ok(AuthStart {
            url: url.to_string(),
            state: state.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
            nonce: nonce.secret().clone(),
        })
    }

    pub async fn finish(
        &self,
        code: String,
        pkce_verifier: String,
        nonce: String,
    ) -> AppResult<Identity> {
        let meta = self.metadata(false).await?;
        let auth_type = self.auth_type(&meta);
        let client = CoreClient::from_provider_metadata(
            meta,
            ClientId::new(self.client_id.clone()),
            self.client_secret.clone().map(ClientSecret::new),
        )
        .set_auth_type(auth_type)
        .set_redirect_uri(RedirectUrl::new(self.redirect_uri.clone()).map_err(upstream)?);
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(upstream)?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
            .request_async(&self.http_client())
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "OIDC code exchange failed");
                AppError::Unauthorized
            })?;
        let id_token = token.id_token().ok_or(AppError::Unauthorized)?;
        let nonce = Nonce::new(nonce);
        let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
            Ok(c) => c.clone(),
            Err(first) => {
                // The provider may have rotated its signing keys: refresh JWKS once.
                let meta = self.metadata(true).await?;
                let client = CoreClient::from_provider_metadata(
                    meta,
                    ClientId::new(self.client_id.clone()),
                    self.client_secret.clone().map(ClientSecret::new),
                );
                id_token
                    .claims(&client.id_token_verifier(), &nonce)
                    .map_err(|e| {
                        tracing::warn!(first = %first, error = %e, "ID token verification failed");
                        AppError::Unauthorized
                    })?
                    .clone()
            }
        };
        let accept_email =
            |verified: Option<bool>| verified == Some(true) || self.allow_unverified_email;
        let mut email = claims
            .email()
            .filter(|_| accept_email(claims.email_verified()))
            .map(|e| e.as_str().to_string());
        let mut name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.as_str().to_string())
            .or_else(|| claims.preferred_username().map(|u| u.as_str().to_string()));
        // Some providers (e.g. Authelia by default) only put the profile in
        // the userinfo response, not in the ID token.
        if name.is_none() || email.is_none() {
            let request = client
                .user_info(token.access_token().clone(), Some(claims.subject().clone()))
                .ok();
            let info: Option<openidconnect::core::CoreUserInfoClaims> = match request {
                Some(r) => r.request_async(&self.http_client()).await.ok(),
                None => None,
            };
            if let Some(info) = info {
                if name.is_none() {
                    name = info
                        .name()
                        .and_then(|n| n.get(None))
                        .map(|n| n.as_str().to_string())
                        .or_else(|| info.preferred_username().map(|u| u.as_str().to_string()));
                }
                if email.is_none() {
                    email = info
                        .email()
                        .filter(|_| accept_email(info.email_verified()))
                        .map(|e| e.as_str().to_string());
                }
            }
        }
        let name = name
            .or_else(|| email.clone())
            .unwrap_or_else(|| "User".to_string());
        Ok(Identity {
            issuer: claims.issuer().as_str().to_string(),
            subject: claims.subject().as_str().to_string(),
            display_name: name.chars().take(100).collect(),
            email,
        })
    }
}
