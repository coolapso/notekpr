//! The server's own numbers, from its Prometheus endpoint (`/metrics`).

use std::time::{Duration, Instant};

/// One scrape, summed over labels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scrape {
    pub http_requests: f64,
    pub updates: f64,
    pub ws_connections: f64,
    /// Request time quantiles (seconds) over the exporter's recent window.
    pub http_p50: f64,
    pub http_p99: f64,
    /// The server process's own CPU time and memory (Linux servers).
    pub cpu_seconds: f64,
    pub rss_bytes: f64,
    /// Requests turned away by its rate limits.
    pub rate_limited: f64,
}

pub fn parse(text: &str) -> Scrape {
    let mut s = Scrape::default();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(v) = value.trim().parse::<f64>() else {
            continue;
        };
        let (name, labels) = match key.split_once('{') {
            Some((n, l)) => (n, l),
            None => (key, ""),
        };
        match name {
            "notekpr_http_requests_total" => s.http_requests += v,
            "notekpr_updates_total" => s.updates += v,
            "notekpr_ws_connections" => s.ws_connections += v,
            "notekpr_rate_limited_total" => s.rate_limited += v,
            "process_cpu_seconds_total" => s.cpu_seconds = v,
            "process_resident_memory_bytes" => s.rss_bytes = v,
            "notekpr_http_request_duration_seconds" if v.is_finite() => {
                if labels.contains("quantile=\"0.5\"") {
                    s.http_p50 = v;
                } else if labels.contains("quantile=\"0.99\"") {
                    s.http_p99 = v;
                }
            }
            _ => {}
        }
    }
    s
}

/// Turns successive scrapes into rates.
pub struct Rates {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
    last: Option<(Instant, Scrape)>,
    /// The rates last worked out.
    rates: Now,
}

/// Scrapes closer together than this keep the rates of the one before: over
/// a few milliseconds, one request or one clock tick of CPU time more would
/// look like thousands a second.
const MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default)]
pub struct Now {
    pub http_rps: f32,
    pub updates_ps: f32,
    pub ws_connections: u32,
    pub http_p50_ms: f32,
    pub http_p99_ms: f32,
    /// CPU (100 = one core) and memory of the server process, by its own
    /// account.
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    pub rate_limited_ps: f32,
}

impl Rates {
    pub fn new(server_url: &str, token: Option<String>) -> Rates {
        Rates {
            client: reqwest::Client::new(),
            url: format!("{server_url}/metrics"),
            token,
            last: None,
            rates: Now::default(),
        }
    }

    /// `None` while the server is unreachable (a restart resets counters,
    /// so the next scrape starts over).
    pub async fn sample(&mut self) -> Option<Now> {
        let text = async {
            let mut req = self.client.get(&self.url).timeout(Duration::from_secs(2));
            if let Some(t) = &self.token {
                req = req.bearer_auth(t);
            }
            req.send()
                .await
                .ok()
                .filter(|r| r.status().is_success())?
                .text()
                .await
                .ok()
        }
        .await;
        let Some(text) = text else {
            self.last = None;
            return None;
        };
        Some(self.advance(parse(&text), Instant::now()))
    }

    /// The rates since the last scrape, with this one.
    fn advance(&mut self, s: Scrape, now: Instant) -> Now {
        if let Some((t0, _)) = &self.last
            && now.duration_since(*t0) < MIN_INTERVAL
        {
            return Now {
                ws_connections: s.ws_connections as u32,
                mem_bytes: s.rss_bytes as u64,
                ..self.rates
            };
        }
        let out = match &self.last {
            Some((t0, s0)) if s.http_requests >= s0.http_requests => {
                let dt = now.duration_since(*t0).as_secs_f64();
                Now {
                    http_rps: ((s.http_requests - s0.http_requests) / dt) as f32,
                    updates_ps: ((s.updates - s0.updates) / dt) as f32,
                    ws_connections: s.ws_connections as u32,
                    http_p50_ms: (s.http_p50 * 1000.0) as f32,
                    http_p99_ms: (s.http_p99 * 1000.0) as f32,
                    cpu_pct: ((s.cpu_seconds - s0.cpu_seconds).max(0.0) / dt * 100.0) as f32,
                    mem_bytes: s.rss_bytes as u64,
                    rate_limited_ps: ((s.rate_limited - s0.rate_limited).max(0.0) / dt) as f32,
                }
            }
            _ => Now {
                ws_connections: s.ws_connections as u32,
                mem_bytes: s.rss_bytes as u64,
                ..Default::default()
            },
        };
        self.last = Some((now, s));
        self.rates = out;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_labels_and_reads_quantiles() {
        let text = r#"
# TYPE notekpr_http_requests_total counter
notekpr_http_requests_total{method="GET",status="2xx"} 10
notekpr_http_requests_total{method="POST",status="2xx"} 5
notekpr_updates_total 7
notekpr_ws_connections 3
notekpr_http_request_duration_seconds{quantile="0.5"} 0.002
notekpr_http_request_duration_seconds{quantile="0.99"} 0.04
notekpr_http_request_duration_seconds_sum 1.5
notekpr_rate_limited_total{scope="api"} 2
notekpr_rate_limited_total{scope="auth"} 1
process_cpu_seconds_total 12.5
process_resident_memory_bytes 41943040
"#;
        let s = parse(text);
        assert_eq!(s.http_requests, 15.0);
        assert_eq!(s.updates, 7.0);
        assert_eq!(s.ws_connections, 3.0);
        assert_eq!((s.http_p50, s.http_p99), (0.002, 0.04));
        assert_eq!(s.rate_limited, 3.0);
        assert_eq!((s.cpu_seconds, s.rss_bytes), (12.5, 41_943_040.0));
    }

    #[test]
    fn rates_need_scrapes_far_enough_apart() {
        let mut r = Rates::new("http://localhost:1", None);
        let t0 = Instant::now();
        let at = |requests: f64, cpu: f64| Scrape {
            http_requests: requests,
            cpu_seconds: cpu,
            ..Default::default()
        };
        assert_eq!(r.advance(at(0.0, 1.0), t0).http_rps, 0.0);
        let now = r.advance(at(300.0, 1.5), t0 + Duration::from_secs(1));
        assert_eq!((now.http_rps, now.cpu_pct), (300.0, 50.0));
        // A scrape right after (timer rounds that caught up): not one
        // request per millisecond, the rates of a moment ago.
        let now = r.advance(at(302.0, 1.51), t0 + Duration::from_millis(1002));
        assert_eq!((now.http_rps, now.cpu_pct), (300.0, 50.0));
        let now = r.advance(at(500.0, 1.75), t0 + Duration::from_secs(2));
        assert_eq!((now.http_rps, now.cpu_pct), (200.0, 25.0));
    }
}
