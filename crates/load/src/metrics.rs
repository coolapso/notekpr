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
            "coolpsonotes_http_requests_total" => s.http_requests += v,
            "coolpsonotes_updates_total" => s.updates += v,
            "coolpsonotes_ws_connections" => s.ws_connections += v,
            "coolpsonotes_http_request_duration_seconds" if v.is_finite() => {
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
    last: Option<(Instant, Scrape)>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Now {
    pub http_rps: f32,
    pub updates_ps: f32,
    pub ws_connections: u32,
    pub http_p50_ms: f32,
    pub http_p99_ms: f32,
}

impl Rates {
    pub fn new(server_url: &str) -> Rates {
        Rates {
            client: reqwest::Client::new(),
            url: format!("{server_url}/metrics"),
            last: None,
        }
    }

    /// `None` while the server is unreachable (a restart resets counters,
    /// so the next scrape starts over).
    pub async fn sample(&mut self) -> Option<Now> {
        let text = async {
            self.client
                .get(&self.url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .ok()?
                .text()
                .await
                .ok()
        }
        .await;
        let Some(text) = text else {
            self.last = None;
            return None;
        };
        let s = parse(&text);
        let now = Instant::now();
        let out = match &self.last {
            Some((t0, s0)) if s.http_requests >= s0.http_requests => {
                let dt = now.duration_since(*t0).as_secs_f64().max(0.001);
                Now {
                    http_rps: ((s.http_requests - s0.http_requests) / dt) as f32,
                    updates_ps: ((s.updates - s0.updates) / dt) as f32,
                    ws_connections: s.ws_connections as u32,
                    http_p50_ms: (s.http_p50 * 1000.0) as f32,
                    http_p99_ms: (s.http_p99 * 1000.0) as f32,
                }
            }
            _ => Now {
                ws_connections: s.ws_connections as u32,
                ..Default::default()
            },
        };
        self.last = Some((now, s));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_labels_and_reads_quantiles() {
        let text = r#"
# TYPE coolpsonotes_http_requests_total counter
coolpsonotes_http_requests_total{method="GET",status="2xx"} 10
coolpsonotes_http_requests_total{method="POST",status="2xx"} 5
coolpsonotes_updates_total 7
coolpsonotes_ws_connections 3
coolpsonotes_http_request_duration_seconds{quantile="0.5"} 0.002
coolpsonotes_http_request_duration_seconds{quantile="0.99"} 0.04
coolpsonotes_http_request_duration_seconds_sum 1.5
"#;
        let s = parse(text);
        assert_eq!(s.http_requests, 15.0);
        assert_eq!(s.updates, 7.0);
        assert_eq!(s.ws_connections, 3.0);
        assert_eq!((s.http_p50, s.http_p99), (0.002, 0.04));
    }
}
