//! Limits per person (protocol.md §9, "Limits per person"). On a server
//! many people share, one person can come from many addresses, so each
//! person — an account with its agents, which count as their owner
//! ([`Limits::person`]) — has a request rate, budgets for the WebSocket's
//! (un)subscriptions and awareness, at most [`MAX_CONNECTIONS`] connections
//! at once, and a daily write allowance. A session that keeps sending what
//! these refuse, not waiting as told, gets its person suspended
//! ([`suspend`]).
//!
//! Everything is counted here, in memory ([`People`]): a restart starts
//! everyone afresh. Only suspensions are written (`users.suspended_until`,
//! the audit log). For administrators, how many requests were refused in
//! the last 24 hours is kept per person ([`People::refused`]) — never what
//! was asked.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::MatchedPath;
use axum::http::Method;
use axum::http::request::Parts;
use notekpr_core::protocol::{Allowance, InstanceSettings, Limit, MIB, Refused};

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::hub::Control;
use crate::repo;
use crate::state::AppState;

/// A person's request rate allows bursts of this many seconds' worth.
const BURST: f64 = 4.0;
/// (Un)subscriptions per person and second, and their burst: a reconnect
/// of many tabs subscribes to every vault at once.
const SUBSCRIPTIONS_PER_SEC: f64 = 20.0;
const SUBSCRIPTION_BURST: f64 = 1000.0;
/// Awareness messages per person and second (a drawing's pointer sends up
/// to 20 a second per tab), and their burst.
const AWARENESS_PER_SEC: f64 = 60.0;
const AWARENESS_BURST: f64 = 120.0;
/// Live connections per person at once.
pub const MAX_CONNECTIONS: u32 = 50;
/// A session's count of refusals (a live connection's, on the WebSocket:
/// a browser's tabs share a session) drains by this many a second: apps
/// that wait as `Retry-After` says are refused at most once a second for
/// each request they have going at once, far below it.
const STRIKES_DRAIN_PER_SEC: f64 = 20.0;
/// At this count (ten minutes of 20 refusals a second) the person is
/// suspended.
const STRIKES_LIMIT: f64 = 12_000.0;
/// What a change counts at least against the write allowance, so that
/// small ones can't be repeated without end either.
pub const MIN_CHANGE_BYTES: u64 = 1024;
/// The write allowance refills over this long.
const DAY: Duration = Duration::from_secs(24 * 3600);
/// Someone unseen for this long is forgotten — unless they went past their
/// write allowance by more than has come back: their request budgets are
/// full again by then, and their refusals are older than a day.
const FORGET_AFTER: Duration = Duration::from_secs(25 * 3600);

/// Whose limits per person a request counts against, and what they are:
/// read with the session (`auth::authenticate`), for an agent its owner's.
#[derive(Debug, Clone, Default)]
pub struct Limits {
    /// The account, or an agent's owner.
    pub person: String,
    pub rate: Allowance,
    pub writes: Allowance,
    /// Until when (ms) the person is suspended, if they are or were.
    pub suspended_until: Option<i64>,
}

impl Limits {
    pub fn suspended(&self, now: i64) -> Option<i64> {
        self.suspended_until.filter(|t| *t > now)
    }

    /// Requests per second, if limited.
    pub fn rate(&self, s: &InstanceSettings) -> Option<f64> {
        let rate = self
            .rate
            .resolve(Limit::Max(u64::from(s.person_rate_limit_rps)));
        rate.max().map(|n| n.max(1) as f64)
    }

    /// The daily write allowance in bytes, if limited.
    fn writes(&self, s: &InstanceSettings) -> Option<u64> {
        self.writes
            .resolve(s.writes_per_day_mb)
            .max()
            .map(|mb| mb.saturating_mul(MIB))
    }
}

/// A refused request: when to come back, and whether its session has now
/// been refused so much that its person is to be suspended.
#[derive(Debug)]
pub struct Refusal {
    pub wait: Duration,
    pub suspend: bool,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn full(cap: f64, now: Instant) -> Bucket {
        Bucket {
            tokens: cap,
            at: now,
        }
    }

    /// Take one at `rate` per second, up to `cap` kept; else how long until
    /// there is one.
    fn take(&mut self, rate: f64, cap: f64, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(cap);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - self.tokens) / rate))
        }
    }
}

/// What is left of a write allowance of `cap` bytes (below zero after the
/// write that went past it), refilling evenly over a [`DAY`].
#[derive(Debug, Clone, Copy)]
struct Writes {
    left: f64,
    cap: u64,
    at: Instant,
}

impl Writes {
    fn reserve(&mut self, cap: u64, bytes: u64, now: Instant) -> Result<(), Duration> {
        // A new allowance applies at once to what is used of it: more room
        // now, or less.
        if cap != self.cap {
            self.left += cap as f64 - self.cap as f64;
            self.cap = cap;
        }
        let cap = cap as f64;
        let rate = cap / DAY.as_secs_f64();
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.left = (self.left + elapsed * rate).min(cap);
        self.at = now;
        if self.left > 0.0 {
            self.left -= bytes as f64;
            Ok(())
        } else if rate > 0.0 {
            // Until a byte is back.
            Err(Duration::from_secs_f64((1.0 - self.left) / rate))
        } else {
            Err(DAY)
        }
    }
}

/// Refusals in one hour (since the Unix epoch).
#[derive(Debug, Clone, Copy)]
struct Hour {
    hour: u64,
    refused: Refused,
}

#[derive(Debug)]
struct Person {
    requests: Option<Bucket>,
    subscriptions: Option<Bucket>,
    awareness: Option<Bucket>,
    codes: Option<Bucket>,
    writes: Option<Writes>,
    connections: u32,
    /// The last 24 hours' refusals, oldest first.
    refused: VecDeque<Hour>,
    seen: Instant,
}

impl Person {
    fn new(now: Instant) -> Person {
        Person {
            requests: None,
            subscriptions: None,
            awareness: None,
            codes: None,
            writes: None,
            connections: 0,
            refused: VecDeque::new(),
            seen: now,
        }
    }

    fn count(&mut self, hour: u64, f: impl FnOnce(&mut Refused)) {
        if self.refused.back().is_none_or(|h| h.hour != hour) {
            self.refused.push_back(Hour {
                hour,
                refused: Refused::default(),
            });
        }
        f(&mut self.refused.back_mut().unwrap().refused);
        self.forget_before(hour);
    }

    fn forget_before(&mut self, hour: u64) {
        while self.refused.front().is_some_and(|h| h.hour + 24 <= hour) {
            self.refused.pop_front();
        }
    }

    fn last_day(&self, hour: u64) -> Refused {
        let mut sum = Refused::default();
        for h in self.refused.iter().filter(|h| h.hour + 24 > hour) {
            sum.requests += h.refused.requests;
            sum.writes += h.refused.writes;
        }
        sum
    }
}

/// A session's refusals, draining by [`STRIKES_DRAIN_PER_SEC`].
#[derive(Debug)]
struct Strikes {
    count: f64,
    at: Instant,
    person: String,
}

#[derive(Default)]
struct Inner {
    people: HashMap<String, Person>,
    sessions: HashMap<String, Strikes>,
}

impl Inner {
    fn person(&mut self, id: &str, now: Instant) -> &mut Person {
        let p = self
            .people
            .entry(id.to_string())
            .or_insert_with(|| Person::new(now));
        p.seen = now;
        p
    }

    /// One more refusal of a request by the rate limits, on `session`:
    /// whether its person is to be suspended now (once: the count starts
    /// again).
    fn strike(&mut self, person: &str, session: &str, now: Instant, hour: u64) -> bool {
        self.person(person, now).count(hour, |r| r.requests += 1);
        let s = self
            .sessions
            .entry(session.to_string())
            .or_insert_with(|| Strikes {
                count: 0.0,
                at: now,
                person: person.to_string(),
            });
        let elapsed = now.saturating_duration_since(s.at).as_secs_f64();
        s.count = (s.count - elapsed * STRIKES_DRAIN_PER_SEC).max(0.0) + 1.0;
        s.at = now;
        if s.count >= STRIKES_LIMIT {
            s.count = 0.0;
            true
        } else {
            false
        }
    }
}

/// Everyone's limits per person on this server, in memory.
#[derive(Default)]
pub struct People {
    inner: Mutex<Inner>,
}

fn hour_now() -> u64 {
    (db::now().max(0) as u64) / 3_600_000
}

impl People {
    pub fn new() -> People {
        People::default()
    }

    /// A request at `rate` per second (none: unlimited) by `person` on
    /// `session` (what refusals count against for a suspension: the
    /// session, or a live connection of it).
    pub fn request(&self, person: &str, session: &str, rate: Option<f64>) -> Result<(), Refusal> {
        self.request_at(person, session, rate, Instant::now(), hour_now())
    }

    fn request_at(
        &self,
        person: &str,
        session: &str,
        rate: Option<f64>,
        now: Instant,
        hour: u64,
    ) -> Result<(), Refusal> {
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, now);
        let Some(rate) = rate else {
            return Ok(());
        };
        let cap = rate * BURST;
        let bucket = p.requests.get_or_insert(Bucket::full(cap, now));
        match bucket.take(rate, cap, now) {
            Ok(()) => Ok(()),
            Err(wait) => Err(Refusal {
                wait,
                suspend: inner.strike(person, session, now, hour),
            }),
        }
    }

    /// A WebSocket (un)subscription.
    pub fn subscription(&self, person: &str, session: &str) -> Result<(), Refusal> {
        self.subscription_at(person, session, Instant::now(), hour_now())
    }

    fn subscription_at(
        &self,
        person: &str,
        session: &str,
        now: Instant,
        hour: u64,
    ) -> Result<(), Refusal> {
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, now);
        let bucket = p
            .subscriptions
            .get_or_insert(Bucket::full(SUBSCRIPTION_BURST, now));
        match bucket.take(SUBSCRIPTIONS_PER_SEC, SUBSCRIPTION_BURST, now) {
            Ok(()) => Ok(()),
            Err(wait) => Err(Refusal {
                wait,
                suspend: inner.strike(person, session, now, hour),
            }),
        }
    }

    /// A WebSocket connection's own budget refused a request of `person`'s
    /// on `session`: it counts like the person's. Whether to suspend them.
    pub fn refused_on_connection(&self, person: &str, session: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.strike(person, session, Instant::now(), hour_now())
    }

    /// An awareness message: whether to relay it.
    pub fn awareness(&self, person: &str) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, now);
        p.awareness
            .get_or_insert(Bucket::full(AWARENESS_BURST, now))
            .take(AWARENESS_PER_SEC, AWARENESS_BURST, now)
            .is_ok()
    }

    /// Something sign-in-like — looking up or deciding on a device code, an
    /// agent's new token — at `per_min` a minute (the instance's sign-in
    /// rate), per person too, not only per address: codes can't be guessed
    /// from many addresses, nor tokens minted without end.
    pub fn sign_in(&self, person: &str, per_min: u32) -> Result<(), Duration> {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, now);
        let cap = f64::from(per_min.max(1));
        p.codes
            .get_or_insert(Bucket::full(cap, now))
            .take(cap / 60.0, cap, now)
    }

    /// Count `bytes` of a change against `person`'s write allowance of
    /// `cap` bytes a day (none: unlimited), before it is handled: accepted
    /// while any of it is left.
    pub fn reserve_write(
        &self,
        person: &str,
        cap: Option<u64>,
        bytes: u64,
    ) -> Result<(), Duration> {
        self.reserve_write_at(person, cap, bytes, Instant::now(), hour_now())
    }

    fn reserve_write_at(
        &self,
        person: &str,
        cap: Option<u64>,
        bytes: u64,
        now: Instant,
        hour: u64,
    ) -> Result<(), Duration> {
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, now);
        let Some(cap) = cap else {
            return Ok(());
        };
        let writes = p.writes.get_or_insert(Writes {
            left: cap as f64,
            cap,
            at: now,
        });
        let r = writes.reserve(cap, bytes.max(MIN_CHANGE_BYTES), now);
        if r.is_err() {
            p.count(hour, |r| r.writes += 1);
        }
        r
    }

    /// A new live connection of `person`'s: whether they may have another.
    pub fn connect(&self, person: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let p = inner.person(person, Instant::now());
        if p.connections >= MAX_CONNECTIONS {
            return false;
        }
        p.connections += 1;
        true
    }

    pub fn disconnect(&self, person: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(p) = inner.people.get_mut(person) {
            p.connections = p.connections.saturating_sub(1);
        }
    }

    /// How many of `person`'s requests were refused in the last 24 hours.
    pub fn refused(&self, person: &str) -> Refused {
        let inner = self.inner.lock().unwrap();
        inner
            .people
            .get(person)
            .map(|p| p.last_day(hour_now()))
            .unwrap_or_default()
    }

    /// A suspension of `person`'s was lifted: their sessions start counting
    /// afresh.
    pub fn lifted(&self, person: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.retain(|_, s| s.person != person);
    }

    /// Forget who hasn't been seen for long, and sessions whose count has
    /// drained.
    pub fn cleanup(&self) {
        self.cleanup_at(Instant::now(), hour_now());
    }

    fn cleanup_at(&self, now: Instant, hour: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.people.retain(|_, p| {
            p.forget_before(hour);
            // What went past a write allowance is kept until it is back.
            let in_debt = p.writes.is_some_and(|w| {
                let rate = w.cap as f64 / DAY.as_secs_f64();
                let elapsed = now.saturating_duration_since(w.at).as_secs_f64();
                w.left + elapsed * rate < w.cap as f64
            });
            in_debt || p.connections > 0 || now.saturating_duration_since(p.seen) < FORGET_AFTER
        });
        inner.sessions.retain(|_, s| {
            let elapsed = now.saturating_duration_since(s.at).as_secs_f64();
            s.count - elapsed * STRIKES_DRAIN_PER_SEC > 0.0
        });
    }
}

/// Every request with a session (`Principal`, and people administering):
/// refused while its person is suspended, past their request rate, or — a
/// change ([`change_bytes`]) — past their write allowance.
pub async fn admit(state: &AppState, who: &Principal, parts: &Parts) -> AppResult<()> {
    if let Some(until) = who.limits.suspended(db::now()) {
        return Err(AppError::Suspended(until));
    }
    let settings = state.instance.settings();
    let people = state.instance.people();
    if let Err(r) = people.request(
        &who.limits.person,
        &who.session_id,
        who.limits.rate(&settings),
    ) {
        refused(state, "person", &who.limits.person, r.suspend).await;
        return Err(AppError::RateLimitedFor(r.wait));
    }
    if let Some(bytes) = change_bytes(parts, &settings) {
        reserve(state, who, bytes)?;
    }
    Ok(())
}

/// Count a change of `bytes` (a WebSocket push) against the write
/// allowance of `who`'s person.
pub fn reserve(state: &AppState, who: &Principal, bytes: u64) -> AppResult<()> {
    let cap = who.limits.writes(&state.instance.settings());
    state
        .instance
        .people()
        .reserve_write(&who.limits.person, cap, bytes)
        .map_err(|wait| {
            metrics::counter!("notekpr_rate_limited_total", "scope" => "writes").increment(1);
            AppError::WriteLimit(wait)
        })
}

/// What a request counts against the write allowance, if it is a change:
/// a `POST`, `PUT` or `PATCH` that read-only mode refuses, with what it
/// sends — its `Content-Length`, or the largest body the server takes.
fn change_bytes(parts: &Parts, settings: &InstanceSettings) -> Option<u64> {
    if ![Method::POST, Method::PUT, Method::PATCH].contains(&parts.method) {
        return None;
    }
    let path = parts
        .extensions
        .get::<MatchedPath>()
        .map(|m| m.as_str())
        .unwrap_or_default();
    if !crate::routes::refused_when_read_only(parts.method.as_str(), path) {
        return None;
    }
    let length = parts
        .headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    Some(length.unwrap_or_else(|| crate::routes::largest_body(settings) as u64))
}

/// A request of `person`'s refused by a rate limit (`scope`, for the
/// metrics): suspend them if their session has been refused too much.
pub async fn refused(state: &AppState, scope: &'static str, person: &str, suspend_now: bool) {
    metrics::counter!("notekpr_rate_limited_total", "scope" => scope).increment(1);
    if !suspend_now {
        return;
    }
    let minutes = state.instance.settings().auto_suspend_minutes;
    if minutes == 0 {
        return;
    }
    let until = db::now() + i64::from(minutes) * 60_000;
    match suspend(state, person, until, None).await {
        Ok(()) => {
            metrics::counter!("notekpr_suspensions_total").increment(1);
            tracing::warn!(
                minutes,
                "suspended an account whose apps kept sending requests past its limits"
            );
        }
        Err(e) => tracing::warn!(error = %e, "could not suspend an account"),
    }
}

/// Suspend `person` (and their agents) until `until` (ms), or lift their
/// suspension (a time past): written with an audit entry, `by` an
/// administrator or the server (`None`). Their live connections end.
pub async fn suspend(
    state: &AppState,
    person: &str,
    until: i64,
    by: Option<(&str, serde_json::Value)>,
) -> AppResult<()> {
    let now = db::now();
    let lifting = until <= now;
    let agents = {
        let _w = state.write_lock.lock().await?;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if !repo::users::set_suspended(&mut *tx, person, (!lifting).then_some(until)).await? {
            return Err(AppError::NotFound);
        }
        let (actor, mut details) = match by {
            Some((actor, details)) => (Some(actor), details),
            None => (None, serde_json::json!({ "by": "server" })),
        };
        let action = if lifting {
            "account.unsuspended"
        } else {
            if let Some(d) = details.as_object_mut() {
                d.insert("until".into(), until.into());
            }
            "account.suspended"
        };
        repo::audit::record(
            &mut *tx,
            actor,
            Some(person),
            None,
            action,
            Some(person),
            details,
        )
        .await;
        let agents = repo::agents::ids(&mut *tx, person).await?;
        tx.commit().await?;
        agents
    };
    if lifting {
        state.instance.people().lifted(person);
    } else {
        for id in std::iter::once(person.to_string()).chain(agents) {
            state.hub.control(&id, Control::Suspended(until));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 500_000;

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn a_person_has_one_rate_from_every_session() {
        let people = People::new();
        let t = Instant::now();
        for i in 0..8 {
            let session = if i % 2 == 0 { "laptop" } else { "phone" };
            assert!(people.request_at("ana", session, Some(2.0), t, H).is_ok());
        }
        let r = people
            .request_at("ana", "laptop", Some(2.0), t, H)
            .unwrap_err();
        assert_eq!(r.wait, secs(0.5));
        assert!(!r.suspend);
        // Someone else has their own.
        assert!(people.request_at("bo", "tablet", Some(2.0), t, H).is_ok());
        // Half a second later, one more.
        let t = t + secs(0.5);
        assert!(people.request_at("ana", "phone", Some(2.0), t, H).is_ok());
        assert!(people.request_at("ana", "phone", Some(2.0), t, H).is_err());
        // Unlimited: never refused.
        for _ in 0..1000 {
            assert!(people.request_at("cy", "s", None, t, H).is_ok());
        }
    }

    #[test]
    fn writes_are_accepted_while_any_allowance_is_left_and_refill_over_a_day() {
        let people = People::new();
        let t = Instant::now();
        let cap = Some(10 * MIB);
        assert!(people.reserve_write_at("ana", cap, 9 * MIB, t, H).is_ok());
        // Past it, by the last write.
        assert!(people.reserve_write_at("ana", cap, 5 * MIB, t, H).is_ok());
        let wait = people.reserve_write_at("ana", cap, 1, t, H).unwrap_err();
        // 4 MiB over, at 10 MiB a day: a little over 9.6 hours.
        let hours = wait.as_secs_f64() / 3600.0;
        assert!((9.6..9.61).contains(&hours), "{hours}");
        assert!(people.reserve_write_at("ana", cap, 1, t + wait, H).is_ok());
        // Small changes count 1 KiB each.
        let people = People::new();
        for _ in 0..10 {
            assert!(
                people
                    .reserve_write_at("bo", Some(10 * 1024), 1, t, H)
                    .is_ok()
            );
        }
        assert!(
            people
                .reserve_write_at("bo", Some(10 * 1024), 1, t, H)
                .is_err()
        );
        // None at all: refused, come back tomorrow.
        assert_eq!(
            people.reserve_write_at("cy", Some(0), 1, t, H).unwrap_err(),
            DAY
        );
        // Unlimited.
        assert!(people.reserve_write_at("di", None, u64::MAX, t, H).is_ok());
        // A new allowance applies to what is used of it, at once.
        let people = People::new();
        assert!(
            people
                .reserve_write_at("ed", Some(MIB), 2 * MIB, t, H)
                .is_ok()
        );
        assert!(people.reserve_write_at("ed", Some(MIB), 1, t, H).is_err());
        assert!(
            people
                .reserve_write_at("ed", Some(3 * MIB), MIB, t, H)
                .is_ok()
        );
        assert!(
            people
                .reserve_write_at("ed", Some(3 * MIB), 1, t, H)
                .is_err()
        );
    }

    #[test]
    fn refusals_of_the_last_day_are_kept_per_person() {
        let people = People::new();
        let t = Instant::now();
        // Bursts of 4 seconds' worth.
        for _ in 0..4 {
            assert!(people.request_at("ana", "s", Some(1.0), t, H).is_ok());
        }
        for _ in 0..3 {
            assert!(people.request_at("ana", "s", Some(1.0), t, H).is_err());
        }
        assert!(people.reserve_write_at("bo", Some(0), 1, t, H + 3).is_err());
        let day = |p: &str, hour| {
            let inner = people.inner.lock().unwrap();
            inner.people[p].last_day(hour)
        };
        assert_eq!(
            day("ana", H + 23),
            Refused {
                requests: 3,
                writes: 0
            }
        );
        assert_eq!(day("ana", H + 24), Refused::default());
        assert_eq!(day("bo", H + 3).writes, 1);
    }

    #[test]
    fn a_session_not_waiting_as_told_gets_its_person_suspended_others_never() {
        let people = People::new();
        let t = Instant::now();
        // Apps that wait as told: 40 devices with 10 requests going each,
        // refused once a second each, at the lowest rate, for 20 minutes.
        let mut at = t;
        for _ in 0..1200 {
            at += secs(1.0);
            for d in 0..40 {
                for _ in 0..10 {
                    let r = people.request_at("ana", &format!("d{d}"), Some(1.0), at, H);
                    assert!(!r.is_err_and(|r| r.suspend));
                }
            }
        }
        // A script ignoring it: 100 a second past its rate.
        let mut at = t;
        let mut suspended_after = None;
        for i in 0..100_000u32 {
            at += secs(0.01);
            if let Err(r) = people.request_at("bo", "script", Some(1.0), at, H)
                && r.suspend
            {
                suspended_after = Some(f64::from(i) / 100.0);
                break;
            }
        }
        let after = suspended_after.expect("suspended");
        assert!((140.0..160.0).contains(&after), "{after}");
        // Once: its count starts again.
        assert!(
            !people
                .request_at("bo", "script", Some(1.0), at, H)
                .unwrap_err()
                .suspend
        );
    }

    #[test]
    fn connections_are_bounded_and_people_are_forgotten_when_idle() {
        let people = People::new();
        for _ in 0..MAX_CONNECTIONS {
            assert!(people.connect("ana"));
        }
        assert!(!people.connect("ana"));
        people.disconnect("ana");
        assert!(people.connect("ana"));
        let t = Instant::now();
        for _ in 0..4 {
            assert!(people.request_at("bo", "s", Some(1.0), t, H).is_ok());
        }
        assert!(people.request_at("bo", "s", Some(1.0), t, H).is_err());
        people.cleanup_at(t + FORGET_AFTER, H + 25);
        let inner = people.inner.lock().unwrap();
        // Ana is connected; Bo and his session are gone.
        assert!(inner.people.contains_key("ana"));
        assert!(!inner.people.contains_key("bo"));
        assert!(inner.sessions.is_empty());
    }

    #[test]
    fn what_went_past_a_write_allowance_is_kept_until_it_is_back() {
        let people = People::new();
        let t = Instant::now();
        // 25 MiB at 1 MiB a day: 24 days past it.
        assert!(
            people
                .reserve_write_at("ana", Some(MIB), 25 * MIB, t, H)
                .is_ok()
        );
        people.cleanup_at(t + FORGET_AFTER, H + 25);
        assert!(
            people
                .reserve_write_at("ana", Some(MIB), 1, t + FORGET_AFTER, H + 25)
                .is_err()
        );
        // Back after the 24 days: forgotten then.
        let later = t + DAY * 25;
        people.cleanup_at(later, H + 25 * 24);
        assert!(!people.inner.lock().unwrap().people.contains_key("ana"));
    }
}
