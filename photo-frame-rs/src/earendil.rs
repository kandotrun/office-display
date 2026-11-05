//! Loopback client for the Earendil agent (`127.0.0.1:8101`): the values
//! shown in the band, the emblem outline and the live conversation, plus
//! the band's timing (rotation, the panel opening, the voice level).
//! Data and state only; all painting lives in `earendil_paint`.

use crate::{atlas::Wake, http, space::ease_out};
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

pub const EARENDIL_PORT: u16 = 8101;
pub const MAX_STATUS_BYTES: usize = 64 * 1024;
pub const MAX_VALUES_BYTES: usize = 64 * 1024;
pub const MAX_EMBLEM_BYTES: usize = 512 * 1024;
/// Contract limits: text longer than these is cut, counts beyond them
/// are refused.
pub const MAX_VALUES: usize = 32;
pub const TITLE_CHARS: usize = 64;
pub const JA_CHARS: usize = 40;
pub const GLOSS_CHARS: usize = 120;
pub const QUESTION_CHARS: usize = 200;
pub const HEARD_CHARS: usize = 200;
pub const ANSWER_CHARS: usize = 400;
pub const MAX_CITED: usize = 3;
pub const MAX_LEVELS: usize = 1200;
pub const MAX_RINGS: usize = 64;
pub const MAX_POINTS: usize = 4096;
/// One-line labels the contract does not bound; generous on purpose.
const PERSONA_CHARS: usize = 32;
const HINT_CHARS: usize = 80;
const CREDIT_CHARS: usize = 64;
pub const PERSONA: &str = "エンデルヒール";
/// Shown while the agent has not answered (recently).
pub const PREPARING: &str = "エンデルヒールを準備しています";

const STATUS_POLL: Duration = Duration::from_millis(250);
const STATUS_RETRY: Duration = Duration::from_secs(2);
const DOCUMENT_RETRY: Duration = Duration::from_secs(10);
const DOCUMENT_REFRESH: Duration = Duration::from_secs(600);

/// A status older than this counts as none: unavailable, and idle.
pub const STALE_SECONDS: f64 = 5.0;
/// Each value stays this long; the next one slides in over `SLIDE_SECONDS`.
pub const DWELL_SECONDS: f64 = 18.0;
pub const SLIDE_SECONDS: f64 = 0.9;
/// The conversation panel opens and closes in this long (ease-out).
pub const OPEN_SECONDS: f64 = 0.7;
pub const RIPPLE_SECONDS: f64 = 1.4;
pub const MAX_RIPPLES: usize = 4;
const RIPPLE_GAP_SECONDS: f64 = 0.25;
/// A syllable this loud starts a ripple; the voice must drop below
/// `QUIET` before the next one can.
const LOUD: f32 = 0.62;
const QUIET: f32 = 0.4;
const ATTACK_SECONDS: f64 = 0.04;
const RELEASE_SECONDS: f64 = 0.16;

/// Zero-width and bidi controls: harmless to drop, confusing to draw.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
    )
}
/// Display text: control and invisible characters removed (line breaks
/// become spaces), trimmed, and cut to `max` characters — counted and cut
/// as chars, never bytes — ending in "…" when it was longer.
pub fn clean(s: &str, max: usize) -> String {
    let kept: String = s
        .chars()
        .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
        .filter(|c| !c.is_control() && !invisible(*c))
        .collect();
    let kept = kept.trim();
    if kept.chars().count() <= max {
        return kept.to_string();
    }
    let mut out: String = kept.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Value,
    Principle,
}
impl Kind {
    /// The band's small label: `EARENDIL · VALUES` or `· OPERATING PRINCIPLES`.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Value => "VALUES",
            Kind::Principle => "OPERATING PRINCIPLES",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub title: String,
    pub kind: Kind,
    pub ja: String,
    pub gloss: String,
}
#[derive(Deserialize)]
struct RawValue {
    title: String,
    kind: Kind,
    ja: String,
    gloss: String,
}
#[derive(Deserialize)]
struct RawValues {
    values: Vec<RawValue>,
}
pub fn parse_values(bytes: &[u8]) -> Result<Vec<Value>, &'static str> {
    if bytes.len() > MAX_VALUES_BYTES {
        return Err("earendil_values_too_large");
    }
    let raw: RawValues = serde_json::from_slice(bytes).map_err(|_| "earendil_values_json")?;
    if raw.values.is_empty() || raw.values.len() > MAX_VALUES {
        return Err("earendil_values_count");
    }
    raw.values
        .into_iter()
        .map(|v| {
            let title = clean(&v.title, TITLE_CHARS);
            if title.is_empty() {
                return Err("earendil_values_title");
            }
            Ok(Value {
                title,
                kind: v.kind,
                ja: clean(&v.ja, JA_CHARS),
                gloss: clean(&v.gloss, GLOSS_CHARS),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Answered,
    Error,
}
impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Listening => "listening",
            State::Thinking => "thinking",
            State::Speaking => "speaking",
            State::Answered => "answered",
            State::Error => "error",
        }
    }
    /// The chip next to the persona name.
    pub fn chip(self) -> &'static str {
        match self {
            State::Idle => "",
            State::Listening => "聞いています",
            State::Thinking => "考えています",
            State::Speaking => "話しています",
            State::Answered | State::Error => "こたえ",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    NoApiKey,
    LlmFailed,
    LlmRefused,
    NotUnderstood,
    /// A code this build does not know yet.
    Other,
}
impl Failure {
    fn parse(code: &str) -> Self {
        match code {
            "no_api_key" => Failure::NoApiKey,
            "llm_failed" => Failure::LlmFailed,
            "llm_refused" => Failure::LlmRefused,
            "not_understood" => Failure::NotUnderstood,
            _ => Failure::Other,
        }
    }
    /// The friendly line the panel shows instead of an answer.
    pub fn message(self) -> &'static str {
        match self {
            Failure::NoApiKey => "APIキーが設定されていません",
            Failure::LlmFailed => "いまは答えられません。少しおいて、もう一度どうぞ",
            Failure::LlmRefused => "その質問にはお答えできません",
            Failure::NotUnderstood => "うまく聞き取れませんでした。もう一度どうぞ",
            Failure::Other => "いまは答えられません",
        }
    }
}
/// The sentence being spoken: loudness sampled every `step` seconds from
/// `started` (unix seconds, the same host clock as ours).
#[derive(Clone, Debug, PartialEq)]
pub struct Voice {
    pub started: f64,
    pub step: f64,
    pub levels: Vec<f32>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub state: State,
    pub conversation: u64,
    pub persona: String,
    pub hint: String,
    pub question: String,
    pub heard: String,
    pub answer: String,
    /// Titles of the values the answer is grounded in.
    pub values: Vec<String>,
    pub voice: Option<Voice>,
    pub voice_credit: String,
    pub error: Option<Failure>,
}
impl Status {
    /// Drop what was said: the frame keeps it only while it is on screen.
    fn forget(&mut self) {
        self.question.clear();
        self.heard.clear();
        self.answer.clear();
    }
}
#[derive(Deserialize)]
struct RawVoice {
    started: f64,
    step: f64,
    levels: Vec<f64>,
}
#[derive(Deserialize)]
struct RawStatus {
    state: State,
    #[serde(default)]
    conversation: u64,
    #[serde(default)]
    persona: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    heard: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    voice: Option<RawVoice>,
    #[serde(default)]
    voice_credit: String,
    #[serde(default)]
    error: Option<String>,
}
fn voice_ok(v: &RawVoice) -> bool {
    v.started.is_finite()
        && v.started >= 0.0
        && v.step.is_finite()
        && (0.005..=1.0).contains(&v.step)
        && v.levels.len() <= MAX_LEVELS
        && v.levels.iter().all(|l| l.is_finite() && (0.0..=1.0).contains(l))
}
pub fn parse_status(bytes: &[u8]) -> Result<Status, &'static str> {
    if bytes.len() > MAX_STATUS_BYTES {
        return Err("earendil_status_too_large");
    }
    let raw: RawStatus = serde_json::from_slice(bytes).map_err(|_| "earendil_status_json")?;
    if raw.values.len() > MAX_CITED {
        return Err("earendil_status_values");
    }
    let voice = match raw.voice {
        Some(v) if voice_ok(&v) => Some(Voice {
            started: v.started,
            step: v.step,
            levels: v.levels.into_iter().map(|l| l as f32).collect(),
        }),
        Some(_) => return Err("earendil_status_voice"),
        None => None,
    };
    let persona = clean(&raw.persona, PERSONA_CHARS);
    Ok(Status {
        state: raw.state,
        conversation: raw.conversation,
        persona: if persona.is_empty() { PERSONA.into() } else { persona },
        hint: clean(&raw.hint, HINT_CHARS),
        question: clean(&raw.question, QUESTION_CHARS),
        heard: clean(&raw.heard, HEARD_CHARS),
        answer: clean(&raw.answer, ANSWER_CHARS),
        values: raw
            .values
            .iter()
            .map(|t| clean(t, TITLE_CHARS))
            .filter(|t| !t.is_empty())
            .collect(),
        voice,
        voice_credit: clean(&raw.voice_credit, CREDIT_CHARS),
        error: raw.error.as_deref().map(Failure::parse),
    })
}

/// The emblem outline: closed rings in view-box fractions (y down),
/// filled with the even-odd rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Emblem {
    /// Width / height of the view box.
    pub aspect: f32,
    pub rings: Vec<Vec<[f32; 2]>>,
}
#[derive(Deserialize)]
struct RawEmblem {
    aspect: f64,
    rings: Vec<Vec<[f64; 2]>>,
}
pub fn parse_emblem(bytes: &[u8]) -> Result<Emblem, &'static str> {
    if bytes.len() > MAX_EMBLEM_BYTES {
        return Err("earendil_emblem_too_large");
    }
    let raw: RawEmblem = serde_json::from_slice(bytes).map_err(|_| "earendil_emblem_json")?;
    if !raw.aspect.is_finite() || !(0.1..=10.0).contains(&raw.aspect) {
        return Err("earendil_emblem_aspect");
    }
    if raw.rings.is_empty() || raw.rings.len() > MAX_RINGS || raw.rings.iter().any(|r| r.len() < 3) {
        return Err("earendil_emblem_rings");
    }
    let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
    let points: usize = raw.rings.iter().map(Vec::len).sum();
    if points > MAX_POINTS || raw.rings.iter().flatten().any(|[x, y]| !unit(*x) || !unit(*y)) {
        return Err("earendil_emblem_points");
    }
    Ok(Emblem {
        aspect: raw.aspect as f32,
        rings: raw
            .rings
            .into_iter()
            .map(|r| r.into_iter().map(|[x, y]| [x as f32, y as f32]).collect())
            .collect(),
    })
}
impl Emblem {
    /// The top-most point, where the sparkle sits.
    pub fn tip(&self) -> [f32; 2] {
        self.rings
            .iter()
            .flatten()
            .copied()
            .reduce(|a, b| if b[1] < a[1] { b } else { a })
            .unwrap_or([0.5, 0.0])
    }
    /// Our own four-point star, drawn until (or unless) the agent has the
    /// real emblem: concave curved sides, the vertical arms the longer.
    pub fn fallback() -> Emblem {
        let n = 96;
        let ring = (0..n)
            .map(|i| {
                let a = i as f64 / n as f64 * std::f64::consts::TAU;
                let (s, c) = a.sin_cos();
                let bend = |v: f64| v.signum() * v.abs().powf(2.6);
                [
                    (0.5 + 0.5 * bend(c)).clamp(0.0, 1.0) as f32,
                    (0.5 - 0.5 * bend(s)).clamp(0.0, 1.0) as f32,
                ]
            })
            .collect();
        Emblem {
            aspect: 0.78,
            rings: vec![ring],
        }
    }
}

/// What the worker has fetched since the frame last looked: only the
/// newest of each document is kept, so it can never pile up.
#[derive(Default)]
pub struct Mailbox {
    pub status: Option<Result<Status, &'static str>>,
    pub values: Option<Vec<Value>>,
    pub emblem: Option<Emblem>,
}
impl Mailbox {
    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.values.is_none() && self.emblem.is_none()
    }
    pub fn merge(&mut self, newer: Mailbox) {
        if newer.status.is_some() {
            self.status = newer.status;
        }
        if newer.values.is_some() {
            self.values = newer.values;
        }
        if newer.emblem.is_some() {
            self.emblem = newer.emblem;
        }
    }
}
pub type EarendilSlot = Arc<Mutex<Mailbox>>;

/// When each document is next due: `/status` every 250 ms while it
/// answers (2 s after an error); `/values.json` and `/emblem.json` at
/// start, every 10 s until they succeed, then every 10 minutes.
pub struct Poller {
    status: Instant,
    values: Instant,
    emblem: Instant,
}
impl Poller {
    pub fn new(now: Instant) -> Self {
        Self {
            status: now,
            values: now,
            emblem: now,
        }
    }
    pub fn next(&self) -> Instant {
        self.status.min(self.values).min(self.emblem)
    }
    /// Fetch whatever is due with `fetch(path, max_bytes)`.
    pub fn poll(
        &mut self,
        now: Instant,
        fetch: &mut impl FnMut(&'static str, usize) -> Result<Vec<u8>, &'static str>,
    ) -> Mailbox {
        let mut got = Mailbox::default();
        let document = |ok: bool| now + if ok { DOCUMENT_REFRESH } else { DOCUMENT_RETRY };
        if now >= self.status {
            let status = fetch("/status", MAX_STATUS_BYTES).and_then(|b| parse_status(&b));
            self.status = now + if status.is_ok() { STATUS_POLL } else { STATUS_RETRY };
            got.status = Some(status);
        }
        if now >= self.values {
            let values = fetch("/values.json", MAX_VALUES_BYTES).and_then(|b| parse_values(&b));
            self.values = document(values.is_ok());
            got.values = values.ok();
        }
        if now >= self.emblem {
            // 404 until the agent has an emblem: retried like any failure.
            let emblem = fetch("/emblem.json", MAX_EMBLEM_BYTES).and_then(|b| parse_emblem(&b));
            self.emblem = document(emblem.is_ok());
            got.emblem = emblem.ok();
        }
        got
    }
}

/// Polls the agent and leaves what it gets in the mailbox for the frame.
/// Mirrors the honeypot client's shape; reads only, never `/ask`.
pub fn start(port: u16, stop: Arc<AtomicBool>, wake: Wake) -> std::io::Result<(EarendilSlot, JoinHandle<()>)> {
    let slot: EarendilSlot = Arc::new(Mutex::new(Mailbox::default()));
    let producer = slot.clone();
    let handle = thread::Builder::new().name("earendil".into()).spawn(move || {
        let mut fetch = |path: &'static str, max: usize| http::get(port, path, max).map(|r| r.body);
        let mut poller = Poller::new(Instant::now());
        while !stop.load(Ordering::Relaxed) {
            let got = poller.poll(Instant::now(), &mut fetch);
            if !got.is_empty() {
                if let Ok(mut mail) = producer.lock() {
                    mail.merge(got);
                }
                if let Ok(ctx) = wake.lock()
                    && let Some(ctx) = ctx.as_ref()
                {
                    ctx.request_repaint();
                }
            }
            let idle = poller.next().saturating_duration_since(Instant::now());
            thread::sleep(idle.min(Duration::from_millis(50)));
        }
        if let Ok(mut mail) = producer.lock() {
            *mail = Mailbox::default();
        }
    })?;
    Ok((slot, handle))
}

/// One value on show, and the one sliding away while `progress` < 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slide {
    pub index: usize,
    pub previous: Option<usize>,
    /// 0 → the slide starts, 1 → only `index` is shown.
    pub progress: f32,
}
#[derive(Default)]
pub struct Rotation {
    current: Option<(usize, f64)>,
    previous: Option<usize>,
}
impl Rotation {
    pub fn update(&mut self, now: f64, count: usize) -> Option<Slide> {
        if count == 0 {
            *self = Self::default();
            return None;
        }
        let (mut index, mut started) = match self.current {
            Some((i, t)) if i < count => (i, t),
            _ => {
                self.previous = None;
                (0, now)
            }
        };
        if now - started >= DWELL_SECONDS {
            if count > 1 {
                self.previous = Some(index);
                index = (index + 1) % count;
                started = now;
            } else {
                // A single value stays fully shown; start its next dwell.
                started = now - SLIDE_SECONDS;
            }
        }
        self.current = Some((index, started));
        let progress = ((now - started) / SLIDE_SECONDS).clamp(0.0, 1.0) as f32;
        if progress >= 1.0 {
            self.previous = None;
        }
        Some(Slide {
            index,
            previous: self.previous,
            progress,
        })
    }
    /// Put `index` on show now, for a full dwell.
    pub fn show(&mut self, index: usize, now: f64) {
        match self.current {
            Some((i, _)) if i == index => self.current = Some((index, now - SLIDE_SECONDS)),
            other => {
                self.previous = other.map(|(i, _)| i);
                self.current = Some((index, now));
            }
        }
    }
}

/// How far the conversation panel has opened: eases toward open or closed
/// over `OPEN_SECONDS` (scaled by the distance left), and restarts from
/// closed when a new conversation begins while it is open.
#[derive(Default)]
pub struct Opening {
    target: bool,
    from: f32,
    at: f64,
    seen: Option<u64>,
}
impl Opening {
    pub fn set(&mut self, open: bool, conversation: Option<u64>, now: f64) {
        let changed = matches!((self.seen, conversation), (Some(a), Some(b)) if a != b);
        if conversation.is_some() {
            self.seen = conversation;
        }
        if open && changed {
            (self.target, self.from, self.at) = (true, 0.0, now);
        } else if open != self.target {
            self.from = self.value(now);
            (self.target, self.at) = (open, now);
        }
    }
    pub fn target(&self) -> bool {
        self.target
    }
    pub fn value(&self, now: f64) -> f32 {
        let to = if self.target { 1.0 } else { 0.0 };
        let span = (to - self.from).abs();
        if span <= 0.0 {
            return to;
        }
        let t = ((now - self.at) / (OPEN_SECONDS * span as f64)).clamp(0.0, 1.0) as f32;
        self.from + (to - self.from) * ease_out(t)
    }
}

/// Loudness of `voice` at `unix`, linearly between samples; 0 outside it.
pub fn level_at(voice: &Voice, unix: f64) -> f32 {
    let t = (unix - voice.started) / voice.step;
    if !(0.0..voice.levels.len() as f64).contains(&t) {
        return 0.0;
    }
    let i = t as usize;
    let a = voice.levels[i];
    let b = voice.levels.get(i + 1).copied().unwrap_or(0.0);
    a + (b - a) * (t - i as f64) as f32
}
/// Fast-attack, slower-release follower for the voice level.
#[derive(Default)]
pub struct Smooth {
    value: f32,
    at: Option<f64>,
}
impl Smooth {
    pub fn step(&mut self, target: f32, now: f64) -> f32 {
        let dt = self.at.map_or(0.0, |at| (now - at).max(0.0));
        self.at = Some(now);
        let tau = if target > self.value { ATTACK_SECONDS } else { RELEASE_SECONDS };
        self.value += (target - self.value) * (1.0 - (-dt / tau).exp()) as f32;
        self.value
    }
}
/// Rings that leave the emblem on loud syllables.
#[derive(Default)]
pub struct Ripples {
    starts: VecDeque<f64>,
    held: bool,
}
impl Ripples {
    pub fn update(&mut self, level: f32, now: f64) {
        while self
            .starts
            .front()
            .is_some_and(|s| now - s >= RIPPLE_SECONDS || now < *s)
        {
            self.starts.pop_front();
        }
        if level >= LOUD {
            let spaced = self.starts.back().is_none_or(|s| now - s >= RIPPLE_GAP_SECONDS);
            if !self.held && spaced {
                if self.starts.len() == MAX_RIPPLES {
                    self.starts.pop_front();
                }
                self.starts.push_back(now);
            }
            self.held = true;
        } else if level < QUIET {
            self.held = false;
        }
    }
    /// Each live ripple's age, 0 (just left) → 1 (gone).
    pub fn ages(&self, now: f64) -> impl Iterator<Item = f32> + '_ {
        self.starts
            .iter()
            .map(move |s| ((now - s) / RIPPLE_SECONDS) as f32)
            .filter(|a| (0.0..1.0).contains(a))
    }
}

/// What the band paints.
pub struct BandView<'a> {
    pub values: &'a [Value],
    pub slide: Option<Slide>,
    /// The agent's hint, or `PREPARING` while it is unavailable.
    pub hint: &'a str,
    pub available: bool,
}
/// What the conversation panel paints.
pub struct PanelView<'a> {
    /// Eased 0 (the band) → 1 (the whole right column).
    pub open: f32,
    /// The live conversation, or the last one while the panel closes.
    pub status: &'a Status,
    pub level: f32,
    pub ripples: Vec<f32>,
}

/// The band's state: the agent's documents as last received, and all of
/// the band's timing. `now` is the frame's monotonic clock; `unix` is only
/// used to read the voice samples.
pub struct Earendil {
    slot: Option<EarendilSlot>,
    status: Option<Status>,
    status_at: f64,
    failed: Option<&'static str>,
    values: Vec<Value>,
    emblem: Option<Emblem>,
    emblem_rev: u64,
    rotation: Rotation,
    slide: Option<Slide>,
    opening: Opening,
    /// The last non-idle status, kept on the panel while it closes and
    /// dropped once it has.
    shown: Option<Status>,
    /// The first value the current conversation cited.
    cited: Option<String>,
    smooth: Smooth,
    level: f32,
    ripples: Ripples,
}
impl Earendil {
    pub fn new(slot: Option<EarendilSlot>) -> Self {
        Self {
            slot,
            status: None,
            status_at: f64::NEG_INFINITY,
            failed: None,
            values: Vec::new(),
            emblem: None,
            emblem_rev: 0,
            rotation: Rotation::default(),
            slide: None,
            opening: Opening::default(),
            shown: None,
            cited: None,
            smooth: Smooth::default(),
            level: 0.0,
            ripples: Ripples::default(),
        }
    }
    fn receive(&mut self, now: f64) {
        let Some(mail) = self
            .slot
            .as_ref()
            .and_then(|s| s.lock().ok().map(|mut m| std::mem::take(&mut *m)))
        else {
            return;
        };
        match mail.status {
            Some(Ok(status)) => self.apply_status(status, now),
            Some(Err(e)) => self.apply_error(e),
            None => {}
        }
        if let Some(values) = mail.values {
            self.apply_values(values);
        }
        if let Some(emblem) = mail.emblem {
            self.apply_emblem(emblem);
        }
    }
    pub fn apply_status(&mut self, status: Status, now: f64) {
        if self
            .status
            .as_ref()
            .is_some_and(|old| old.conversation != status.conversation)
        {
            self.cited = None;
        }
        if status.state != State::Idle {
            if let Some(first) = status.values.first() {
                self.cited = Some(first.clone());
            }
            self.shown = Some(status.clone());
        }
        self.status = Some(status);
        self.status_at = now;
        self.failed = None;
    }
    pub fn apply_error(&mut self, reason: &'static str) {
        self.failed = Some(reason);
    }
    pub fn apply_values(&mut self, values: Vec<Value>) {
        self.values = values;
    }
    pub fn apply_emblem(&mut self, emblem: Emblem) {
        if self.emblem.as_ref() != Some(&emblem) {
            self.emblem = Some(emblem);
            self.emblem_rev += 1;
        }
    }
    fn live(&self, now: f64) -> Option<&Status> {
        self.status
            .as_ref()
            .filter(|_| now - self.status_at <= STALE_SECONDS)
    }
    /// Once per frame: take the worker's mail and advance the timing.
    /// Conversation text lives only as long as it is shown: a stale status
    /// loses it at once, the panel's copy when the panel has closed.
    pub fn tick(&mut self, now: f64, unix: f64) {
        self.receive(now);
        if self.live(now).is_none()
            && let Some(s) = self.status.as_mut()
        {
            s.forget();
        }
        let live = self.live(now);
        let open = live.is_some_and(|s| s.state != State::Idle);
        let conversation = live.map(|s| s.conversation);
        let target = live
            .filter(|s| s.state == State::Speaking)
            .and_then(|s| s.voice.as_ref())
            .map_or(0.0, |v| level_at(v, unix));
        let was_open = self.opening.target();
        self.opening.set(open, conversation, now);
        if !open && self.opening.value(now) <= 0.0 {
            self.shown = None;
        }
        if was_open && !open {
            // Back to the band: first the value the answer leaned on.
            let cited = self.cited.take();
            if let Some(i) = cited.and_then(|t| self.values.iter().position(|v| v.title == t)) {
                self.rotation.show(i, now);
            }
        }
        self.slide = self.rotation.update(now, self.values.len());
        self.level = self.smooth.step(target, now);
        self.ripples.update(self.level, now);
    }
    pub fn band(&self, now: f64) -> BandView<'_> {
        let live = self.live(now);
        BandView {
            values: &self.values,
            slide: self.slide,
            hint: live.map_or(PREPARING, |s| s.hint.as_str()),
            available: live.is_some(),
        }
    }
    /// The panel while it is open or still closing.
    pub fn panel(&self, now: f64) -> Option<PanelView<'_>> {
        let open = self.opening.value(now);
        let status = self.shown.as_ref()?;
        (self.opening.target() || open > 0.0).then(|| PanelView {
            open,
            status,
            level: self.level,
            ripples: self.ripples.ages(now).collect(),
        })
    }
    pub fn state_name(&self, now: f64) -> &'static str {
        match self.live(now) {
            Some(s) => s.state.name(),
            None if self.status.is_none() && self.failed.is_none() => "connecting",
            None => "unavailable",
        }
    }
    pub fn conversation(&self) -> u64 {
        self.status.as_ref().map_or(0, |s| s.conversation)
    }
    pub fn values_loaded(&self) -> usize {
        self.values.len()
    }
    /// The emblem, with a revision that changes only when the outline does.
    pub fn emblem(&self) -> (u64, Option<&Emblem>) {
        (self.emblem_rev, self.emblem.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const VALUES: &str = r#"{"source": "https://earendil.com/values/", "note": "x", "values": [
        {"title": "We not I", "kind": "value", "ja": "「私」ではなく「私たち」",
         "gloss": "最高のチームは個の総和を超える。", "extra": 1},
        {"title": "Craft Beauty", "kind": "principle", "ja": "美しく作る", "gloss": "世界には美しさが必要。"},
        {"title": "Family First", "kind": "value", "ja": "家族が第一", "gloss": "家族を優先する。"}
    ]}"#;
    fn status_json() -> serde_json::Value {
        serde_json::json!({
            "state": "speaking", "conversation": 12, "persona": "エンデルヒール",
            "hint": "「エンデルヒールなら？」と話しかけてみてください",
            "question": "締め切り前に徹夜するのってどう思う", "heard": "",
            "answer": "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。",
            "values": ["We not I", "Family First"],
            "voice": {"started": 1000.0, "step": 0.05, "levels": [0.0, 0.42, 0.81]},
            "voice_credit": "VOICEVOX:青山龍星", "error": null, "unknown_field": [1, 2]
        })
    }
    fn status(v: &serde_json::Value) -> Result<Status, &'static str> {
        parse_status(&serde_json::to_vec(v).unwrap())
    }
    fn idle(conversation: u64) -> Status {
        let mut v = status_json();
        v["state"] = "idle".into();
        v["conversation"] = conversation.into();
        v["question"] = "".into();
        v["answer"] = "".into();
        v["values"] = serde_json::json!([]);
        v["voice"] = serde_json::Value::Null;
        status(&v).unwrap()
    }
    fn talking(state: &str, conversation: u64, cited: &[&str]) -> Status {
        let mut v = status_json();
        v["state"] = state.into();
        v["conversation"] = conversation.into();
        v["values"] = serde_json::json!(cited);
        status(&v).unwrap()
    }

    #[test]
    fn clean_strips_controls_and_cuts_on_char_boundaries() {
        assert_eq!(clean("We\u{0} not\u{7f} I", 64), "We not I");
        assert_eq!(clean("一行目\n二行目\t終わり", 64), "一行目 二行目 終わり");
        assert_eq!(clean("\u{202e}逆\u{200b}向き\u{feff}", 64), "逆向き");
        assert_eq!(clean("  空白  ", 64), "空白");
        // Every character is three bytes: a byte cut would panic or split one.
        let s = "あいうえおかきくけこ";
        assert_eq!(clean(s, 10), s);
        assert_eq!(clean(s, 4), "あいう…");
        assert_eq!(clean(s, 4).chars().count(), 4);
        assert_eq!(clean("🙂🙂🙂", 2), "🙂…");
    }

    #[test]
    fn parses_values_in_page_order_and_ignores_unknown_fields() {
        let v = parse_values(VALUES.as_bytes()).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].title, "We not I");
        assert_eq!(v[0].kind, Kind::Value);
        assert_eq!(v[0].ja, "「私」ではなく「私たち」");
        assert_eq!(v[1].kind, Kind::Principle);
        assert_eq!(v[2].gloss, "家族を優先する。");
    }

    #[test]
    fn values_limits_are_enforced() {
        let mut doc: serde_json::Value = serde_json::from_str(VALUES).unwrap();
        doc["values"][0]["title"] = "T".repeat(80).into();
        doc["values"][0]["ja"] = "和".repeat(50).into();
        doc["values"][0]["gloss"] = "説".repeat(130).into();
        let v = parse_values(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(v[0].title.chars().count(), TITLE_CHARS);
        assert_eq!(v[0].ja.chars().count(), JA_CHARS);
        assert_eq!(v[0].gloss.chars().count(), GLOSS_CHARS);
        assert!(v[0].gloss.ends_with('…'));
        let bad = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut d: serde_json::Value = serde_json::from_str(VALUES).unwrap();
            f(&mut d);
            parse_values(&serde_json::to_vec(&d).unwrap())
        };
        assert_eq!(bad(&|d| d["values"] = serde_json::json!([])), Err("earendil_values_count"));
        assert_eq!(
            bad(&|d| d["values"] = serde_json::Value::Array(vec![d["values"][0].clone(); 33])),
            Err("earendil_values_count")
        );
        assert_eq!(bad(&|d| d["values"][1]["kind"] = "motto".into()), Err("earendil_values_json"));
        assert_eq!(bad(&|d| d["values"][1]["title"] = "\u{1}  ".into()), Err("earendil_values_title"));
        assert_eq!(bad(&|d| d["values"] = "x".into()), Err("earendil_values_json"));
        assert_eq!(parse_values(&vec![b' '; MAX_VALUES_BYTES + 1]), Err("earendil_values_too_large"));
    }

    #[test]
    fn parses_the_contract_status() {
        let s = status(&status_json()).unwrap();
        assert_eq!(s.state, State::Speaking);
        assert_eq!(s.conversation, 12);
        assert_eq!(s.persona, "エンデルヒール");
        assert_eq!(s.values, vec!["We not I", "Family First"]);
        let voice = s.voice.as_ref().unwrap();
        assert_eq!((voice.started, voice.step, voice.levels.len()), (1000.0, 0.05, 3));
        assert_eq!(s.voice_credit, "VOICEVOX:青山龍星");
        assert_eq!(s.error, None);
        for (code, failure) in [
            ("no_api_key", Failure::NoApiKey),
            ("llm_failed", Failure::LlmFailed),
            ("llm_refused", Failure::LlmRefused),
            ("not_understood", Failure::NotUnderstood),
            ("something_new", Failure::Other),
        ] {
            let mut v = status_json();
            v["state"] = "error".into();
            v["error"] = code.into();
            let s = status(&v).unwrap();
            assert_eq!((s.state, s.error), (State::Error, Some(failure)));
            assert!(!failure.message().is_empty());
        }
        let mut v = status_json();
        v["persona"] = "".into();
        assert_eq!(status(&v).unwrap().persona, "エンデルヒール", "the name is never blank");
    }

    #[test]
    fn status_text_is_cut_to_the_contract_limits() {
        let mut v = status_json();
        v["question"] = "問".repeat(250).into();
        v["heard"] = "聞".repeat(250).into();
        v["answer"] = "答".repeat(500).into();
        v["values"] = serde_json::json!(["V".repeat(90)]);
        let s = status(&v).unwrap();
        assert_eq!(s.question.chars().count(), QUESTION_CHARS);
        assert_eq!(s.heard.chars().count(), HEARD_CHARS);
        assert_eq!(s.answer.chars().count(), ANSWER_CHARS);
        assert_eq!(s.values[0].chars().count(), TITLE_CHARS);
    }

    #[test]
    fn rejects_invalid_status() {
        let bad = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v = status_json();
            f(&mut v);
            status(&v)
        };
        assert_eq!(bad(&|v| v["state"] = "dreaming".into()), Err("earendil_status_json"));
        assert_eq!(bad(&|v| v["conversation"] = (-1).into()), Err("earendil_status_json"));
        assert_eq!(
            bad(&|v| v["values"] = serde_json::json!(["a", "b", "c", "d"])),
            Err("earendil_status_values")
        );
        for voice in [
            serde_json::json!({"started": 1.0, "step": 0.0, "levels": []}),
            serde_json::json!({"started": 1.0, "step": 0.05, "levels": [1.5]}),
            serde_json::json!({"started": 1.0, "step": 0.05, "levels": [-0.1]}),
            serde_json::json!({"started": 1.0, "step": 0.05, "levels": vec![0.5; 1201]}),
        ] {
            assert_eq!(bad(&|v| v["voice"] = voice.clone()), Err("earendil_status_voice"), "{voice}");
        }
        assert!(parse_status(br#"{"state": "idle", "voice": {"started": 1e999, "step": 0.05, "levels": []}}"#).is_err());
        assert_eq!(parse_status(b"[]"), Err("earendil_status_json"));
        assert_eq!(parse_status(&vec![b' '; MAX_STATUS_BYTES + 1]), Err("earendil_status_too_large"));
        // Only `state` is required; everything else may be missing.
        let s = parse_status(br#"{"state": "idle"}"#).unwrap();
        assert_eq!((s.conversation, s.voice.is_none(), s.values.len()), (0, true, 0));
    }

    fn emblem_json(rings: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"aspect": 0.549, "rings": rings})
    }
    fn emblem(v: &serde_json::Value) -> Result<Emblem, &'static str> {
        parse_emblem(&serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn parses_emblem_rings_and_finds_the_tip() {
        let e = emblem(&emblem_json(serde_json::json!([
            [[0.98, 0.67], [0.5, 0.02], [0.02, 0.67]],
            [[0.4, 0.5], [0.6, 0.5], [0.5, 0.3]]
        ])))
        .unwrap();
        assert!((e.aspect - 0.549).abs() < 1e-6);
        assert_eq!(e.rings.len(), 2);
        assert_eq!(e.tip(), [0.5, 0.02]);
        let f = Emblem::fallback();
        assert!(emblem(&serde_json::json!({"aspect": f.aspect, "rings": f.rings})).is_ok(), "the fallback obeys the contract");
        assert!(f.tip()[1] < 0.01, "a star points up");
    }

    #[test]
    fn rejects_invalid_emblems() {
        let tri = serde_json::json!([[0.1, 0.1], [0.9, 0.1], [0.5, 0.9]]);
        let bad = |v: serde_json::Value| emblem(&v);
        assert_eq!(bad(serde_json::json!({"aspect": 0.05, "rings": [tri]})), Err("earendil_emblem_aspect"));
        assert_eq!(bad(serde_json::json!({"aspect": 11, "rings": [tri]})), Err("earendil_emblem_aspect"));
        assert_eq!(bad(emblem_json(serde_json::json!([]))), Err("earendil_emblem_rings"));
        assert_eq!(bad(emblem_json(serde_json::Value::Array(vec![tri.clone(); 65]))), Err("earendil_emblem_rings"));
        assert_eq!(bad(emblem_json(serde_json::json!([[[0.1, 0.1], [0.9, 0.1]]]))), Err("earendil_emblem_rings"));
        let big = serde_json::Value::Array(vec![serde_json::json!([0.5, 0.5]); 4097]);
        assert_eq!(bad(emblem_json(serde_json::json!([big]))), Err("earendil_emblem_points"));
        assert_eq!(bad(emblem_json(serde_json::json!([[[0.1, 0.1], [1.2, 0.1], [0.5, 0.9]]]))), Err("earendil_emblem_points"));
        assert_eq!(bad(emblem_json(serde_json::json!([[[0.1, -0.1], [0.9, 0.1], [0.5, 0.9]]]))), Err("earendil_emblem_points"));
        assert_eq!(parse_emblem(b"{}"), Err("earendil_emblem_json"));
        assert_eq!(parse_emblem(&vec![b' '; MAX_EMBLEM_BYTES + 1]), Err("earendil_emblem_too_large"));
    }

    #[test]
    fn poller_keeps_its_own_schedule_per_document() {
        use std::cell::{Cell, RefCell};
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let ok_status = serde_json::to_vec(&status_json()).unwrap();
        let (status_ok, values_ok) = (Cell::new(true), Cell::new(false));
        let asked = RefCell::new(Vec::new());
        let mut fetch = |path: &'static str, _max: usize| -> Result<Vec<u8>, &'static str> {
            asked.borrow_mut().push(path);
            match path {
                "/status" if status_ok.get() => Ok(ok_status.clone()),
                "/values.json" if values_ok.get() => Ok(VALUES.as_bytes().to_vec()),
                "/emblem.json" => Err("http_not_found"),
                _ => Err("connect_failed"),
            }
        };
        let mut poller = Poller::new(t0);
        let got = poller.poll(t0, &mut fetch);
        assert!(matches!(got.status, Some(Ok(_))) && got.values.is_none() && got.emblem.is_none());
        assert_eq!(asked.borrow().as_slice(), ["/status", "/values.json", "/emblem.json"]);
        assert_eq!(poller.next(), at(0.25));
        assert!(poller.poll(at(0.1), &mut fetch).is_empty(), "nothing due");
        status_ok.set(false);
        values_ok.set(true);
        let got = poller.poll(at(0.25), &mut fetch);
        assert_eq!(got.status.unwrap().err(), Some("connect_failed"));
        assert!(got.values.is_none(), "not due again yet");
        assert_eq!(poller.next(), at(2.25), "status backs off to 2 s after an error");
        // The documents retry every 10 s until they succeed, then every 10 min.
        let got = poller.poll(at(10.0), &mut fetch);
        assert_eq!(got.values.map(|v| v.len()), Some(3));
        assert!(got.emblem.is_none());
        assert_eq!(poller.values, at(610.0));
        assert_eq!(poller.emblem, at(20.0));
        let mut mail = Mailbox::default();
        mail.merge(poller.poll(at(20.0), &mut fetch));
        mail.merge(Mailbox::default());
        assert!(mail.status.is_some(), "merging nothing keeps what is waiting");
    }

    #[test]
    fn rotation_dwells_slides_and_can_jump() {
        let mut r = Rotation::default();
        assert_eq!(r.update(0.0, 0), None);
        let s = r.update(1.0, 3).unwrap();
        assert_eq!((s.index, s.previous), (0, None));
        assert_eq!(r.update(1.0 + SLIDE_SECONDS, 3).unwrap().progress, 1.0);
        let s = r.update(1.0 + DWELL_SECONDS, 3).unwrap();
        assert_eq!((s.index, s.previous, s.progress), (1, Some(0), 0.0));
        let s = r.update(1.0 + DWELL_SECONDS + SLIDE_SECONDS / 2.0, 3).unwrap();
        assert!((s.progress - 0.5).abs() < 1e-3 && s.previous == Some(0));
        let s = r.update(1.0 + DWELL_SECONDS + SLIDE_SECONDS, 3).unwrap();
        assert_eq!((s.index, s.previous), (1, None));
        assert_eq!(r.update(1.0 + 2.0 * DWELL_SECONDS, 3).unwrap().index, 2);
        assert_eq!(r.update(1.0 + 3.0 * DWELL_SECONDS, 3).unwrap().index, 0, "wraps around");
        // Jumping to a value restarts its dwell with a slide from the old one.
        r.show(2, 100.0);
        let s = r.update(100.0, 3).unwrap();
        assert_eq!((s.index, s.previous, s.progress), (2, Some(0), 0.0));
        assert_eq!(r.update(100.0 + DWELL_SECONDS - 0.1, 3).unwrap().index, 2);
        // Jumping to the value on show only restarts the dwell.
        r.show(2, 200.0);
        let s = r.update(200.0, 3).unwrap();
        assert_eq!((s.index, s.previous, s.progress), (2, None, 1.0));
        // A single value stays put without sliding into itself.
        let mut one = Rotation::default();
        one.update(0.0, 1);
        let s = one.update(DWELL_SECONDS + 1.0, 1).unwrap();
        assert_eq!((s.index, s.previous, s.progress), (0, None, 1.0));
        // Fewer values than the index on show: start over.
        let s = r.update(201.0, 1).unwrap();
        assert_eq!((s.index, s.previous), (0, None));
    }

    #[test]
    fn opening_eases_out_both_ways_and_restarts_on_a_new_conversation() {
        let mut o = Opening::default();
        assert_eq!(o.value(0.0), 0.0);
        o.set(true, Some(1), 10.0);
        assert_eq!(o.value(10.0), 0.0);
        let mid = o.value(10.0 + OPEN_SECONDS / 2.0);
        assert!(mid > 0.8, "ease-out: most of the way at half time ({mid})");
        assert_eq!(o.value(10.0 + OPEN_SECONDS), 1.0);
        o.set(true, Some(1), 11.0);
        assert_eq!(o.value(11.0), 1.0, "same conversation: stays open");
        o.set(true, Some(2), 12.0);
        assert_eq!(o.value(12.0), 0.0, "a new conversation restarts the open animation");
        assert_eq!(o.value(12.0 + OPEN_SECONDS), 1.0);
        o.set(false, Some(2), 20.0);
        assert_eq!(o.value(20.0), 1.0);
        assert!(o.value(20.0 + OPEN_SECONDS / 2.0) < 0.2, "closes with an ease-out too");
        assert_eq!(o.value(20.0 + OPEN_SECONDS), 0.0);
        // Reopened halfway through closing: from where it is, not from zero.
        o.set(true, Some(2), 30.0);
        o.set(false, Some(2), 30.0 + OPEN_SECONDS);
        o.set(true, Some(2), 30.0 + OPEN_SECONDS + 0.1);
        let v = o.value(30.0 + OPEN_SECONDS + 0.1);
        assert!(v > 0.3 && v < 1.0, "{v}");
        assert_eq!(o.value(40.0), 1.0);
        // A stale feed closes without forgetting the conversation number.
        o.set(false, None, 50.0);
        o.set(true, Some(2), 51.0);
        assert_eq!(o.value(51.0), 0.0, "reopened from closed");
        assert_eq!(o.value(51.0 + OPEN_SECONDS), 1.0);
    }

    #[test]
    fn voice_level_follows_the_samples_at_the_wall_clock() {
        let v = Voice { started: 1000.0, step: 0.1, levels: vec![0.0, 1.0, 0.5] };
        assert_eq!(level_at(&v, 999.0), 0.0, "not started");
        assert_eq!(level_at(&v, 1000.0), 0.0);
        assert!((level_at(&v, 1000.05) - 0.5).abs() < 1e-4, "interpolated");
        assert!((level_at(&v, 1000.1) - 1.0).abs() < 1e-4);
        assert!((level_at(&v, 1000.25) - 0.25).abs() < 1e-4, "fades out after the last sample");
        assert_eq!(level_at(&v, 1000.31), 0.0, "finished");
        let mut s = Smooth::default();
        assert_eq!(s.step(1.0, 0.0), 0.0, "no time has passed yet");
        let up = s.step(1.0, 0.05);
        assert!(up > 0.6 && up < 1.0, "fast attack: {up}");
        let down = s.step(0.0, 0.10);
        assert!(down > 0.3 && down < up, "slower release: {down}");
        assert!(s.step(0.0, 2.0) < 0.01);
        assert_eq!(s.step(0.5, 1.0), s.step(0.5, 1.0), "time going backwards is a no-op");
    }

    #[test]
    fn ripples_start_on_loud_syllables_only() {
        let mut r = Ripples::default();
        r.update(0.3, 0.0);
        assert_eq!(r.ages(0.0).count(), 0);
        r.update(0.8, 0.1);
        assert_eq!(r.ages(0.1).collect::<Vec<_>>(), vec![0.0]);
        r.update(0.9, 0.2);
        assert_eq!(r.ages(0.2).count(), 1, "a held syllable is one ripple");
        r.update(0.2, 0.3);
        r.update(0.8, 0.4);
        assert_eq!(r.ages(0.4).count(), 2);
        let ages: Vec<f32> = r.ages(0.1 + RIPPLE_SECONDS / 2.0).collect();
        assert!((ages[0] - 0.5).abs() < 1e-3);
        assert_eq!(r.ages(10.0).count(), 0, "they fade away");
        for i in 0..20 {
            r.update(if i % 2 == 0 { 0.9 } else { 0.0 }, 20.0 + i as f64 * 0.3);
        }
        assert!(r.ages(26.0).count() <= MAX_RIPPLES);
    }

    fn model() -> Earendil {
        let mut m = Earendil::new(None);
        m.apply_values(parse_values(VALUES.as_bytes()).unwrap());
        m
    }

    #[test]
    fn panel_opens_on_any_non_idle_state_and_closes_on_idle() {
        let mut m = model();
        m.apply_status(idle(3), 0.0);
        m.tick(0.0, 5000.0);
        assert!(m.panel(0.0).is_none());
        assert_eq!(m.state_name(0.0), "idle");
        for (i, state) in ["listening", "thinking", "speaking", "answered", "error"].iter().enumerate() {
            let mut m = model();
            let t = i as f64;
            m.apply_status(talking(state, 4, &[]), t);
            m.tick(t, 5000.0);
            m.tick(t + OPEN_SECONDS, 5000.0);
            assert_eq!(m.panel(t + OPEN_SECONDS).unwrap().open, 1.0, "{state}");
            assert_eq!(m.state_name(t), *state);
        }
        let mut m = model();
        m.apply_status(talking("answered", 4, &["Family First"]), 1.0);
        m.tick(1.0, 5000.0);
        m.tick(2.0, 5000.0);
        m.apply_status(idle(4), 3.0);
        m.tick(3.0, 5000.0);
        let p = m.panel(3.0).expect("still closing");
        assert_eq!(p.status.state, State::Answered, "the closing panel keeps the last answer on it");
        assert!(!p.status.answer.is_empty());
        m.tick(3.0 + OPEN_SECONDS, 5000.0);
        assert!(m.panel(3.0 + OPEN_SECONDS).is_none());
    }

    /// Whether the model still holds any of a conversation's text.
    fn remembers(m: &Earendil) -> bool {
        [&m.status, &m.shown]
            .into_iter()
            .flatten()
            .any(|s| !(s.question.is_empty() && s.heard.is_empty() && s.answer.is_empty()))
    }

    #[test]
    fn the_conversation_is_forgotten_once_the_panel_has_closed() {
        // Back to idle: the agent clears its text, the closed panel's copy goes too.
        let mut m = model();
        m.apply_status(talking("answered", 4, &["We not I"]), 0.0);
        m.tick(0.0, 5000.0);
        m.tick(OPEN_SECONDS, 5000.0);
        m.apply_status(idle(4), 25.0);
        m.tick(25.0, 5000.0);
        assert!(remembers(&m), "still on the closing panel");
        m.tick(25.0 + OPEN_SECONDS, 5000.0);
        assert!(m.panel(25.0 + OPEN_SECONDS).is_none());
        assert!(!remembers(&m), "nothing kept once the panel has closed");
        // The agent gone mid-conversation: its last status keeps no text
        // either, but the panel still closes with what it showed.
        let mut m = model();
        let mut s = talking("listening", 5, &[]);
        s.heard = "締め切り前に".into();
        m.apply_status(s, 0.0);
        m.tick(0.0, 5000.0);
        let late = STALE_SECONDS + 0.1;
        m.tick(late, 5000.0);
        assert_eq!(m.panel(late).map(|p| p.status.heard.as_str()), Some("締め切り前に"), "closing");
        assert!(m.status.as_ref().is_some_and(|s| s.heard.is_empty() && s.question.is_empty()));
        m.tick(late + OPEN_SECONDS, 5000.0);
        assert!(m.panel(late + OPEN_SECONDS).is_none());
        assert!(!remembers(&m));
        assert_eq!((m.state_name(late + OPEN_SECONDS), m.conversation()), ("unavailable", 5));
        // Back again with the same conversation: open on its fresh text.
        m.apply_status(talking("answered", 5, &[]), 20.0);
        m.tick(20.0, 5000.0);
        m.tick(20.0 + OPEN_SECONDS, 5000.0);
        assert!(m.panel(20.0 + OPEN_SECONDS).is_some_and(|p| !p.status.answer.is_empty()));
    }

    #[test]
    fn after_a_conversation_the_band_shows_the_first_cited_value() {
        let mut m = model();
        m.apply_status(idle(1), 0.0);
        m.tick(0.0, 5000.0);
        assert_eq!(m.band(0.0).slide.unwrap().index, 0);
        m.apply_status(talking("speaking", 2, &[]), 1.0);
        m.tick(1.0, 5000.0);
        m.apply_status(talking("speaking", 2, &["Family First", "We not I"]), 2.0);
        m.tick(2.0, 5000.0);
        m.apply_status(talking("answered", 2, &[]), 3.0);
        m.tick(3.0, 5000.0);
        m.apply_status(idle(2), 4.0);
        m.tick(4.0, 5000.0);
        let slide = m.band(4.0).slide.unwrap();
        assert_eq!((slide.index, slide.previous), (2, Some(0)), "Family First is the third value");
        m.tick(4.0 + DWELL_SECONDS - 0.5, 5000.0);
        assert_eq!(m.band(4.0 + DWELL_SECONDS - 0.5).slide.unwrap().index, 2, "with a full dwell");
        // A conversation that cited nothing leaves the rotation alone.
        m.apply_status(talking("answered", 3, &["Unknown Title"]), 30.0);
        m.tick(30.0, 5000.0);
        m.apply_status(idle(3), 31.0);
        m.tick(31.0, 5000.0);
        assert_eq!(m.band(31.0).slide.unwrap().index, 0, "rotation went on from Family First");
    }

    #[test]
    fn a_new_conversation_forgets_the_old_citation() {
        let mut m = model();
        m.apply_status(talking("answered", 5, &["Craft Beauty"]), 0.0);
        m.tick(0.0, 5000.0);
        m.apply_status(talking("listening", 6, &[]), 1.0);
        m.tick(1.0, 5000.0);
        assert_eq!(m.panel(1.0).unwrap().open, 0.0, "restarted");
        m.apply_status(talking("error", 6, &[]), 2.0);
        m.tick(2.0, 5000.0);
        m.apply_status(idle(6), 3.0);
        m.tick(3.0, 5000.0);
        assert_eq!(m.band(3.0).slide.unwrap().previous, None, "no jump");
    }

    #[test]
    fn stale_status_counts_as_unavailable_and_idle() {
        let mut m = model();
        assert_eq!(m.state_name(0.0), "connecting");
        m.apply_status(talking("speaking", 7, &[]), 10.0);
        m.tick(10.0, 5000.0);
        m.tick(10.0 + OPEN_SECONDS, 5000.0);
        assert!(m.band(11.0).available);
        assert_eq!(m.state_name(10.0 + STALE_SECONDS), "speaking");
        let late = 10.0 + STALE_SECONDS + 0.1;
        m.tick(late, 5000.0);
        assert_eq!(m.state_name(late), "unavailable");
        assert!(!m.band(late).available);
        assert_eq!(m.band(late).hint, PREPARING);
        m.tick(late + OPEN_SECONDS, 5000.0);
        assert!(m.panel(late + OPEN_SECONDS).is_none(), "closed");
        m.apply_error("connect_failed");
        assert_eq!(m.state_name(late), "unavailable");
    }

    #[test]
    fn voice_level_and_ripples_only_while_speaking() {
        let mut m = model();
        let mut s = talking("speaking", 8, &[]);
        s.voice = Some(Voice { started: 5000.0, step: 0.05, levels: vec![0.9; 40] });
        m.apply_status(s, 0.0);
        m.tick(0.0, 5000.0);
        m.tick(0.2, 5000.2);
        let p = m.panel(0.2).unwrap();
        assert!(p.level > 0.8, "{}", p.level);
        assert_eq!(p.ripples.len(), 1);
        let mut s = talking("answered", 8, &[]);
        s.voice = Some(Voice { started: 5000.0, step: 0.05, levels: vec![0.9; 400] });
        m.apply_status(s, 0.3);
        m.tick(1.5, 5001.5);
        assert!(m.panel(1.5).unwrap().level < 0.01, "voice is only read while speaking");
    }

    #[test]
    fn hint_comes_from_the_status() {
        let mut m = model();
        assert_eq!(m.band(0.0).hint, PREPARING);
        m.apply_status(idle(1), 0.0);
        m.tick(0.0, 0.0);
        assert_eq!(m.band(0.0).hint, "「エンデルヒールなら？」と話しかけてみてください");
        assert_eq!(m.band(0.0).values.len(), 3);
    }

    #[test]
    fn a_new_emblem_bumps_the_revision_once() {
        let mut m = model();
        assert_eq!(m.emblem(), (0, None));
        m.apply_emblem(Emblem::fallback());
        assert_eq!(m.emblem().0, 1);
        m.apply_emblem(Emblem::fallback());
        assert_eq!(m.emblem().0, 1, "the 10-minute refresh of the same emblem is free");
        let mut other = Emblem::fallback();
        other.aspect = 0.5;
        m.apply_emblem(other);
        assert_eq!(m.emblem().0, 2);
    }

    #[test]
    fn receive_drains_the_mailbox() {
        let slot: EarendilSlot = Arc::new(Mutex::new(Mailbox::default()));
        let mut m = Earendil::new(Some(slot.clone()));
        {
            let mut mail = slot.lock().unwrap();
            mail.status = Some(Ok(idle(9)));
            mail.values = Some(parse_values(VALUES.as_bytes()).unwrap());
            mail.emblem = Some(Emblem::fallback());
        }
        m.tick(1.0, 0.0);
        assert_eq!((m.conversation(), m.values_loaded(), m.emblem().0), (9, 3, 1));
        let mail = slot.lock().unwrap();
        assert!(mail.status.is_none() && mail.values.is_none() && mail.emblem.is_none());
    }
}
