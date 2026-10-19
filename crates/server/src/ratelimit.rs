use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::config::Config;
use crate::error::AppError;
use crate::state::AppState;

pub struct Limiters {
    pub api: DefaultKeyedRateLimiter<IpAddr>,
    pub auth: DefaultKeyedRateLimiter<IpAddr>,
}

impl Limiters {
    pub fn new(cfg: &Config) -> Self {
        let rps = NonZeroU32::new(cfg.rate_limit_rps.max(1)).unwrap();
        let burst = NonZeroU32::new(cfg.rate_limit_rps.saturating_mul(4).max(1)).unwrap();
        let per_min = NonZeroU32::new(cfg.auth_rate_limit_per_min.max(1)).unwrap();
        Limiters {
            api: RateLimiter::keyed(Quota::per_second(rps).allow_burst(burst)),
            auth: RateLimiter::keyed(Quota::per_minute(per_min)),
        }
    }

    pub fn cleanup(&self) {
        self.api.retain_recent();
        self.auth.retain_recent();
    }
}

pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()));
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    peer.map(|p| p.ip()).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

fn peer(req: &Request) -> Option<SocketAddr> {
    req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0)
}

pub async fn api_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.headers(), peer(&req), state.config.trust_proxy);
    if state.limiters.api.check_key(&ip).is_err() {
        metrics::counter!("coolpsonotes_rate_limited_total", "scope" => "api").increment(1);
        return AppError::RateLimited.into_response();
    }
    next.run(req).await
}

pub async fn auth_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.headers(), peer(&req), state.config.trust_proxy);
    if state.limiters.auth.check_key(&ip).is_err() {
        metrics::counter!("coolpsonotes_rate_limited_total", "scope" => "auth").increment(1);
        return AppError::RateLimited.into_response();
    }
    next.run(req).await
}
