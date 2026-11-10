use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use notekpr_core::protocol::InstanceSettings;

use crate::error::AppError;
use crate::state::AppState;

/// Requests per client address, at the rates the instance settings say
/// (`rate_limit_rps`, `auth_rate_limit_per_min`): new rates take effect at
/// once, every address starting afresh ([`Limiters::follow`]).
pub struct Limiters {
    current: RwLock<Arc<Keyed>>,
    clock: DefaultClock,
}

struct Keyed {
    api: DefaultKeyedRateLimiter<IpAddr>,
    auth: DefaultKeyedRateLimiter<IpAddr>,
    rates: (u32, u32),
}

impl Keyed {
    fn new(rps: u32, per_min: u32) -> Keyed {
        let nonzero = |n: u32| NonZeroU32::new(n.max(1)).unwrap();
        Keyed {
            api: RateLimiter::keyed(
                Quota::per_second(nonzero(rps)).allow_burst(nonzero(rps.saturating_mul(4))),
            ),
            auth: RateLimiter::keyed(Quota::per_minute(nonzero(per_min))),
            rates: (rps, per_min),
        }
    }
}

impl Limiters {
    pub fn new(settings: &InstanceSettings) -> Self {
        Limiters {
            current: RwLock::new(Arc::new(Keyed::new(
                settings.rate_limit_rps,
                settings.auth_rate_limit_per_min,
            ))),
            clock: DefaultClock::default(),
        }
    }

    /// Take the settings' rates, if they changed.
    pub fn follow(&self, settings: &InstanceSettings) {
        let rates = (settings.rate_limit_rps, settings.auth_rate_limit_per_min);
        let mut current = self.current.write().unwrap();
        if current.rates != rates {
            *current = Arc::new(Keyed::new(rates.0, rates.1));
        }
    }

    fn current(&self) -> Arc<Keyed> {
        self.current.read().unwrap().clone()
    }

    pub fn cleanup(&self) {
        let keyed = self.current();
        keyed.api.retain_recent();
        keyed.auth.retain_recent();
    }
}

/// The client's address. Behind a trusted proxy, the last entry of
/// `X-Forwarded-For` is the one that proxy added; earlier entries come from
/// the client and can be anything.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        let forwarded = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .last()
            .and_then(|v| v.trim().parse().ok())
            .or_else(|| {
                headers
                    .get("x-real-ip")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok())
            });
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    peer.map(|p| p.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

fn peer(req: &Request) -> Option<SocketAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
}

pub async fn api_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.headers(), peer(&req), state.config.trust_proxy);
    let limiters = state.instance.limiters();
    if let Err(until) = limiters.current().api.check_key(&ip) {
        metrics::counter!("notekpr_rate_limited_total", "scope" => "api").increment(1);
        return limited(until.wait_time_from(limiters.clock.now()));
    }
    next.run(req).await
}

pub async fn auth_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.headers(), peer(&req), state.config.trust_proxy);
    let limiters = state.instance.limiters();
    if let Err(until) = limiters.current().auth.check_key(&ip) {
        metrics::counter!("notekpr_rate_limited_total", "scope" => "auth").increment(1);
        return limited(until.wait_time_from(limiters.clock.now()));
    }
    next.run(req).await
}

/// `429` with `Retry-After`: the request wasn't handled at all, so the client
/// may send it again, unchanged, after that many seconds.
fn limited(wait: Duration) -> Response {
    let mut r = AppError::RateLimited.into_response();
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_to_come_back_in_whole_seconds() {
        let after = |d| {
            limited(d).headers()[header::RETRY_AFTER]
                .to_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(after(Duration::from_millis(1200)), "2");
        assert_eq!(after(Duration::from_secs(3)), "3");
        assert_eq!(after(Duration::ZERO), "1");
        assert_eq!(limited(Duration::ZERO).status(), 429);
    }

    #[test]
    fn new_rates_take_effect_at_once() {
        let mut s = InstanceSettings {
            auth_rate_limit_per_min: 1,
            ..Default::default()
        };
        let l = Limiters::new(&s);
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let sign_in = || l.current().auth.check_key(&ip).is_ok();
        assert!(sign_in());
        assert!(!sign_in());
        // The same rates: nobody starts afresh.
        l.follow(&s);
        assert!(!sign_in());
        s.auth_rate_limit_per_min = 2;
        l.follow(&s);
        assert!(sign_in());
        assert!(sign_in());
        assert!(!sign_in());
    }

    #[test]
    fn uses_the_address_the_proxy_added() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 203.0.113.7".parse().unwrap());
        let peer = Some("10.0.0.2:4000".parse().unwrap());
        assert_eq!(
            client_ip(&h, peer, true),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(&h, peer, false),
            "10.0.0.2".parse::<IpAddr>().unwrap()
        );
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(
            client_ip(&h, peer, true),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }
}
