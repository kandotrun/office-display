//! Loopback client for the honeypot data service: a small local proxy
//! that republishes the VPS attack snapshot (geo-enriched) as
//! `/status.json`. Data only; all painting lives in `strip_paint`.
use crate::{atlas::Wake, http};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const HONEYPOT_PORT: u16 = 8100;
pub const MAX_STATUS_BYTES: usize = 256 * 1024;
const QUEUE: usize = 4;
const POLL_OK_SECONDS: u64 = 10;
const POLL_ERR_SECONDS: u64 = 5;

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Geo {
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub country_name: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Attacker {
    pub ip: String,
    pub count: u32,
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub country_name: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
}
impl Attacker {
    /// "Paris, FR" style label; falls back to the country name, then the IP.
    pub fn place_label(&self) -> String {
        let cc = self.country.as_deref().unwrap_or("");
        match (self.city.as_deref().unwrap_or(""), cc) {
            (city, cc) if !city.is_empty() && !cc.is_empty() => format!("{city}, {cc}"),
            (city, _) if !city.is_empty() => city.to_string(),
            (_, cc) if !cc.is_empty() => cc.to_string(),
            _ => self
                .country_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.ip.clone()),
        }
    }
    pub fn country_label(&self) -> String {
        self.country_name
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.country.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "Unknown".to_string())
    }
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct FeedItem {
    pub ts: String,
    #[serde(default)]
    pub sensor: String,
    pub src: String,
    pub kind: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub geo: Option<Geo>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct ActiveSession {
    pub src: String,
    pub since: String,
}
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Cowrie {
    pub sessions_today: u32,
    pub sessions_1h: u32,
    pub logins_today: u32,
    #[serde(default)]
    pub active_sessions: Vec<ActiveSession>,
    #[serde(default)]
    pub top_credentials: Vec<(String, u32)>,
    #[serde(default)]
    pub top_commands: Vec<(String, u32)>,
}
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Canary {
    pub events_today: u32,
    pub events_1h: u32,
    #[serde(default)]
    pub by_port: Vec<(String, u32)>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Target {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub generated_at: String,
    pub target: Target,
    #[serde(default)]
    pub attackers: Vec<Attacker>,
    #[serde(default)]
    pub cowrie: Cowrie,
    #[serde(default)]
    pub canary: Canary,
    #[serde(default)]
    pub events_per_minute: Vec<u32>,
    #[serde(default)]
    pub feed: Vec<FeedItem>,
}

fn lat_ok(v: f64) -> bool {
    v.is_finite() && (-90.0..=90.0).contains(&v)
}
fn lon_ok(v: f64) -> bool {
    v.is_finite() && (-180.0..=180.0).contains(&v)
}
pub fn parse_status(bytes: &[u8]) -> Result<Status, &'static str> {
    if bytes.len() > MAX_STATUS_BYTES {
        return Err("honeypot_status_too_large");
    }
    let s: Status = serde_json::from_slice(bytes).map_err(|_| "honeypot_status_json")?;
    if s.generated_at.len() > 64 || s.target.name.len() > 64 {
        return Err("honeypot_status_strings");
    }
    if !lat_ok(s.target.lat) || !lon_ok(s.target.lon) {
        return Err("honeypot_status_target");
    }
    if s.attackers.len() > 64 || s.feed.len() > 64 || s.events_per_minute.len() > 120 {
        return Err("honeypot_status_sizes");
    }
    for a in &s.attackers {
        if a.ip.len() > 64
            || a.lat.is_some_and(|v| !lat_ok(v))
            || a.lon.is_some_and(|v| !lon_ok(v))
            || a.city.as_ref().is_some_and(|c| c.len() > 128)
        {
            return Err("honeypot_status_attacker");
        }
    }
    for f in &s.feed {
        if f.ts.len() > 64 || f.src.len() > 64 || f.kind.len() > 128 || f.detail.len() > 256 {
            return Err("honeypot_status_feed");
        }
    }
    Ok(s)
}

pub enum HoneypotEvent {
    Status(Status),
    Unavailable(&'static str),
}
pub type HoneypotSlot = Arc<Mutex<VecDeque<HoneypotEvent>>>;

/// Polls the local honeypot data service and republishes snapshots to
/// the HUD. Mirrors the atlas client's shape on purpose.
pub fn start(
    port: u16,
    stop: Arc<AtomicBool>,
    wake: Wake,
) -> std::io::Result<(HoneypotSlot, JoinHandle<()>)> {
    let slot: HoneypotSlot = Arc::new(Mutex::new(VecDeque::new()));
    let producer = slot.clone();
    let handle = thread::Builder::new().name("honeypot".into()).spawn(move || {
        let push = |event: HoneypotEvent| {
            if let Ok(mut q) = producer.lock() {
                if q.len() == QUEUE {
                    q.pop_front();
                }
                q.push_back(event);
            }
            if let Ok(ctx) = wake.lock()
                && let Some(ctx) = ctx.as_ref()
            {
                ctx.request_repaint();
            }
        };
        let mut next_poll = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            if Instant::now() < next_poll {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let status = http::get(port, "/status.json", MAX_STATUS_BYTES)
                .and_then(|r| parse_status(&r.body));
            match status {
                Ok(status) => {
                    next_poll = Instant::now() + Duration::from_secs(POLL_OK_SECONDS);
                    push(HoneypotEvent::Status(status));
                }
                Err(e) => {
                    next_poll = Instant::now() + Duration::from_secs(POLL_ERR_SECONDS);
                    push(HoneypotEvent::Unavailable(e));
                }
            }
        }
        if let Ok(mut q) = producer.lock() {
            q.clear();
        }
    })?;
    Ok((slot, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    const STATUS: &str = r#"{
      "generated_at": "2026-10-26T01:42:18+09:00",
      "target": {"name": "石狩", "lat": 43.17, "lon": 141.31},
      "attackers": [
        {"ip": "77.239.124.68", "count": 215, "last": "2026-10-26T01:30:00+09:00",
         "country": "FR", "country_name": "France", "city": "Paris", "lat": 48.8575, "lon": 2.35138},
        {"ip": "10.0.0.9", "count": 3, "last": null}
      ],
      "cowrie": {"sessions_today": 270, "sessions_1h": 21, "logins_today": 129,
                 "active_sessions": [{"src": "1.2.3.4", "since": "2026-10-26T01:41:00+09:00"}],
                 "top_credentials": [["root/admin", 9]], "top_commands": [["uname", 4]]},
      "canary": {"events_today": 45, "events_1h": 6, "by_port": [["MySQL(3306)", 12]]},
      "events_per_minute": [0, 1, 3],
      "feed": [{"ts": "2026-10-26T01:41:00+09:00", "sensor": "cowrie", "src": "77.239.124.68",
                "kind": "ログイン成功", "detail": "root/admin",
                "geo": {"country": "FR", "country_name": "France", "city": "Paris",
                        "lat": 48.8575, "lon": 2.35138}}]
    }"#;
    #[test]
    fn parses_a_full_status() {
        let s = parse_status(STATUS.as_bytes()).unwrap();
        assert_eq!(s.target.name, "石狩");
        assert_eq!(s.attackers.len(), 2);
        assert_eq!(s.attackers[0].place_label(), "Paris, FR");
        assert_eq!(s.attackers[1].place_label(), "10.0.0.9");
        assert_eq!(s.cowrie.active_sessions.len(), 1);
        assert_eq!(s.feed[0].geo.as_ref().unwrap().city, "Paris");
    }
    #[test]
    fn rejects_bad_coordinates() {
        let bad = STATUS.replace("43.17", "431.7");
        assert_eq!(parse_status(bad.as_bytes()), Err("honeypot_status_target"));
        assert_eq!(parse_status(b"{}"), Err("honeypot_status_json"));
    }
}
