//! Loopback client for the room-survey service's live readings
//! (`/status`: motion level, audio level, face count). The MJPEG frame
//! carries the same numbers as pixels; this feed lets the strip panel
//! show them as data next to the frame. Data only; painting lives in
//! `strip_paint`.
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

pub const MAX_STATUS_BYTES: usize = 4 * 1024;
const QUEUE: usize = 4;
const POLL_OK_SECONDS: u64 = 2;
const POLL_ERR_SECONDS: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub motion: f64,
    pub db: f64,
    pub faces: u32,
}

#[derive(Deserialize)]
struct Raw {
    motion: f64,
    db: f64,
    faces: u32,
}

pub fn parse_reading(body: &[u8]) -> Result<Reading, &'static str> {
    let raw: Raw = serde_json::from_slice(body).map_err(|_| "bad_survey_json")?;
    if !raw.motion.is_finite() || !(0.0..=100.0).contains(&raw.motion) {
        return Err("bad_motion");
    }
    if !raw.db.is_finite() || !(-140.0..=10.0).contains(&raw.db) {
        return Err("bad_db");
    }
    if raw.faces > 64 {
        return Err("bad_faces");
    }
    Ok(Reading {
        motion: raw.motion,
        db: raw.db,
        faces: raw.faces,
    })
}

pub enum SurveyEvent {
    Reading(Reading),
    Unavailable(&'static str),
}
pub type SurveySlot = Arc<Mutex<VecDeque<SurveyEvent>>>;

/// Polls the survey service (on the camera port) and republishes
/// readings to the HUD. Mirrors the honeypot client's shape.
pub fn start(
    port: u16,
    stop: Arc<AtomicBool>,
    wake: Wake,
) -> std::io::Result<(SurveySlot, JoinHandle<()>)> {
    let slot: SurveySlot = Arc::new(Mutex::new(VecDeque::new()));
    let producer = slot.clone();
    let handle = thread::Builder::new().name("survey".into()).spawn(move || {
        let push = |event: SurveyEvent| {
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
            let reading = http::get(port, "/status", MAX_STATUS_BYTES)
                .and_then(|r| parse_reading(&r.body));
            match reading {
                Ok(reading) => {
                    next_poll = Instant::now() + Duration::from_secs(POLL_OK_SECONDS);
                    push(SurveyEvent::Reading(reading));
                }
                Err(e) => {
                    next_poll = Instant::now() + Duration::from_secs(POLL_ERR_SECONDS);
                    push(SurveyEvent::Unavailable(e));
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
    #[test]
    fn parses_and_validates_readings() {
        let r = parse_reading(br#"{"motion":1.920,"faces":2,"db":-64.0,"seq":1794466398}"#).unwrap();
        assert_eq!((r.motion, r.db, r.faces), (1.92, -64.0, 2));
        assert_eq!(parse_reading(br#"{"motion":-1,"faces":0,"db":-64}"#), Err("bad_motion"));
        assert_eq!(parse_reading(br#"{"motion":1,"faces":0,"db":-999}"#), Err("bad_db"));
        assert_eq!(parse_reading(br#"{"motion":1,"faces":99,"db":-64}"#), Err("bad_faces"));
        assert!(parse_reading(b"{}").is_err());
        assert!(parse_reading(b"not json").is_err());
    }
}
