//! OpenID Connect relying party (authorization code flow with PKCE).
//!
//! Works with any standards-compliant provider (Authelia, Keycloak, Authentik,
//! Dex, ...). ID tokens are verified by `openidconnect`: signature against the
//! provider's JWKS, issuer, audience, expiry and nonce.

use std::time::{Duration, Instant};

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::error::{AppError, AppResult};

const METADATA_TTL: Duration = Duration::from_secs(3600);

pub struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: Vec<String>,
    redirect_uri: String,
    http: openidconnect::reqwest::Client,
    meta: RwLock<Option<(CoreProviderMetadata, Instant)>>,
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
    pub fn from_config(cfg: &Config) -> Option<Self> {
        let (issuer, client_id) = (cfg.oidc_issuer.clone()?, cfg.oidc_client_id.clone()?);
        let http = openidconnect::reqwest::ClientBuilder::new()
            // Following redirects would open the door to SSRF.
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .expect("HTTP client");
        Some(Oidc {
            issuer,
            client_id,
            client_secret: cfg.oidc_client_secret.clone(),
            scopes: cfg.oidc_scopes.split_whitespace().map(str::to_string).collect(),
            redirect_uri: format!("{}/api/v1/auth/oidc/callback", cfg.public_url()),
            http,
            meta: RwLock::new(None),
        })
    }

    async fn metadata(&self, force: bool) -> AppResult<CoreProviderMetadata> {
        if !force
            && let Some((m, at)) = &*self.meta.read().await
                && at.elapsed() < METADATA_TTL {
                    return Ok(m.clone());
                }
        let issuer = IssuerUrl::new(self.issuer.clone()).map_err(upstream)?;
        let m = CoreProviderMetadata::discover_async(issuer, &self.http).await.map_err(upstream)?;
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
        let mut req = client.authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random);
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

    pub async fn finish(&self, code: String, pkce_verifier: String, nonce: String) -> AppResult<Identity> {
        let meta = self.metadata(false).await?;
        let client = CoreClient::from_provider_metadata(
            meta,
            ClientId::new(self.client_id.clone()),
            self.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(RedirectUrl::new(self.redirect_uri.clone()).map_err(upstream)?);
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(upstream)?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
            .request_async(&self.http)
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
        let email = claims.email().map(|e| e.as_str().to_string());
        let name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.as_str().to_string())
            .or_else(|| claims.preferred_username().map(|u| u.as_str().to_string()))
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
