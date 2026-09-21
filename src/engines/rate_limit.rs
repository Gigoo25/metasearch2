//! Per-engine request pacing and rate-limit cool-downs.
//!
//! Search engines serve captchas or empty "anomaly" pages when queried too
//! often. Every engine gets a minimum interval between requests (overridable
//! per engine with `min_interval_ms` in the engine config) and a cool-down
//! after an explicit rate-limit response.

use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use super::Engine;
use crate::config::EngineConfig;

/// How long to wait between two requests to the same engine.
fn default_min_interval(engine: Engine) -> Duration {
    match engine {
        // duckduckgo is quick to serve captchas
        Engine::DuckDuckGo => Duration::from_secs(3),
        // marginalia throttles short bursts
        Engine::Marginalia => Duration::from_secs(2),
        Engine::Google | Engine::GoogleScholar => Duration::from_secs(2),
        // local, no need to pace
        Engine::Kiwix => Duration::ZERO,
        _ => Duration::from_secs(1),
    }
}

fn min_interval(config: &EngineConfig, engine: Engine) -> Duration {
    config
        .extra
        .get("min_interval_ms")
        .and_then(toml::Value::as_integer)
        .and_then(|ms| u64::try_from(ms).ok())
        .map_or_else(|| default_min_interval(engine), Duration::from_millis)
}

/// Base cool-down after a rate-limit response; doubles on repeated hits.
const COOLDOWN_BASE: Duration = Duration::from_secs(300);
const COOLDOWN_MAX: Duration = Duration::from_secs(3600);

struct State {
    next_request: Instant,
    cooldown_until: Option<Instant>,
    cooldown: Duration,
}

impl State {
    fn new() -> Self {
        Self {
            next_request: Instant::now(),
            cooldown_until: None,
            cooldown: Duration::ZERO,
        }
    }
}

static STATES: LazyLock<Mutex<HashMap<Engine, State>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remaining cool-down if the engine is currently skipped after rate limiting.
pub fn cooldown_remaining(engine: Engine) -> Option<Duration> {
    let mut states = STATES.lock();
    let state = states.entry(engine).or_insert_with(State::new);
    match state.cooldown_until {
        Some(until) if until > Instant::now() => Some(until - Instant::now()),
        _ => {
            state.cooldown_until = None;
            None
        }
    }
}

/// Waits until this engine's minimum interval has passed.
pub async fn wait_for_slot(config: &EngineConfig, engine: Engine) {
    let interval = min_interval(config, engine);
    if interval.is_zero() {
        return;
    }

    let wait = {
        let mut states = STATES.lock();
        let state = states.entry(engine).or_insert_with(State::new);
        let now = Instant::now();
        let wait = state.next_request.saturating_duration_since(now);
        state.next_request = state.next_request.max(now) + interval;
        wait
    };

    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

/// Clears any cool-down after a successful request.
pub fn report_success(engine: Engine) {
    let mut states = STATES.lock();
    let state = states.entry(engine).or_insert_with(State::new);
    state.cooldown_until = None;
    state.cooldown = Duration::ZERO;
}

/// Puts an engine in cool-down after an explicit rate-limit response.
pub fn report_rate_limited(engine: Engine) {
    let mut states = STATES.lock();
    let state = states.entry(engine).or_insert_with(State::new);
    state.cooldown = if state.cooldown.is_zero() {
        COOLDOWN_BASE
    } else {
        (state.cooldown * 2).min(COOLDOWN_MAX)
    };
    state.cooldown_until = Some(Instant::now() + state.cooldown);
}

/// Response bodies that indicate a bot/captcha challenge instead of results.
/// Only specific markers are used to avoid false positives on real pages.
const CHALLENGE_MARKERS: &[&str] = &[
    "just a moment...",
    "challenge-platform",
    "cf-chl",
    "anomaly-modal",
    "ddg anomaly",
    "unusual traffic",
    "enter the characters you see below",
];

/// What kind of throttle an engine response looks like.
pub enum Challenge {
    /// A short "wait a moment and retry" page (e.g. marginalia).
    Wait(Duration),
    /// A hard block that should put the engine in cool-down.
    Hard,
}

/// Detects bot challenges so they are not parsed as "no results".
#[must_use]
pub fn detect_challenge(body: &str) -> Option<Challenge> {
    let body = body.to_lowercase();

    // marginalia asks for a short wait instead of blocking outright
    if body.contains("barraged by queries from bots") || body.contains("wait for a moment") {
        return Some(Challenge::Wait(Duration::from_millis(1500)));
    }

    CHALLENGE_MARKERS
        .iter()
        .any(|marker| body.contains(marker))
        .then_some(Challenge::Hard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_challenge_pages() {
        assert!(matches!(
            detect_challenge("<title>Just a moment...</title>"),
            Some(Challenge::Hard)
        ));
        assert!(matches!(
            detect_challenge("window.__CF$cv$params = { r: 'challenge-platform' }"),
            Some(Challenge::Hard)
        ));
        assert!(matches!(
            detect_challenge("Our systems have detected unusual traffic"),
            Some(Challenge::Hard)
        ));
        assert!(matches!(
            detect_challenge("The search engine is currently barraged by queries from bots"),
            Some(Challenge::Wait(_))
        ));
        assert!(detect_challenge("<html>results about captchas and challenges</html>").is_none());
    }

    #[test]
    fn engine_config_overrides_min_interval() {
        let config = EngineConfig::default();
        assert_eq!(
            min_interval(&config, Engine::DuckDuckGo),
            Duration::from_secs(3)
        );
        assert_eq!(min_interval(&config, Engine::Kiwix), Duration::ZERO);

        let mut config = EngineConfig::default();
        config
            .extra
            .insert("min_interval_ms".to_string(), toml::Value::Integer(0));
        assert_eq!(min_interval(&config, Engine::DuckDuckGo), Duration::ZERO);
    }

    #[test]
    fn cooldown_starts_and_clears() {
        let engine = Engine::Bing;
        assert!(cooldown_remaining(engine).is_none());
        report_rate_limited(engine);
        assert!(cooldown_remaining(engine).is_some());
        report_success(engine);
        assert!(cooldown_remaining(engine).is_none());
    }
}
