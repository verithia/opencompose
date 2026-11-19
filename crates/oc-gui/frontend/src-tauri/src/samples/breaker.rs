//! Per-market back-off for searches.
//!
//! Two different signals keep a market out of a search for a while. A market
//! that times out, cannot be reached or answers 5xx twice in a row is skipped
//! for [`BREAKER_COOLDOWN`]: one dead market otherwise costs every search the
//! full request deadline. A market that answers 429 is skipped for as long as
//! its `Retry-After` asks, bounded, because searching it sooner only extends
//! the throttle. Every other failure (a bad key, a parse error, a missing
//! record) says nothing about the market's health and leaves it searchable.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use oc_sample::model::FailureKind;
use serde::Serialize;

/// Consecutive health failures that open the breaker.
pub(crate) const BREAKER_FAILURES: u32 = 2;
/// How long an open breaker skips the market.
pub(crate) const BREAKER_COOLDOWN: Duration = Duration::from_secs(600);
/// The wait after a 429 that names no `Retry-After`.
pub(crate) const RATE_LIMIT_DEFAULT: Duration = Duration::from_secs(60);
/// The longest wait a 429 can impose, whatever `Retry-After` says.
pub(crate) const RATE_LIMIT_MAX: Duration = Duration::from_secs(600);

/// Why a market was not searched. The wire spelling is the result's
/// `skip_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SkipReason {
    /// The market answered 429 and its wait has not passed.
    RateLimited,
    /// The market failed [`BREAKER_FAILURES`] times in a row.
    CircuitOpen,
}

/// A market that is being skipped, and for how much longer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Skip {
    pub(crate) reason: SkipReason,
    pub(crate) remaining: Duration,
}

/// What one search of a market showed about its health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation {
    Answered,
    Failed {
        kind: FailureKind,
        retry_after_secs: Option<u64>,
    },
}

#[derive(Default)]
struct MarketState {
    failures: u32,
    skip: Option<(SkipReason, Instant)>,
}

/// Back-off state for every market, keyed by provider id.
#[derive(Default)]
pub(crate) struct Breakers {
    markets: Mutex<HashMap<String, MarketState>>,
}

impl Breakers {
    /// The skip in force for `id` at `now`, if any.
    pub(crate) fn skip(&self, id: &str, now: Instant) -> Option<Skip> {
        let markets = self.markets.lock().ok()?;
        let (reason, until) = markets.get(id)?.skip?;
        (until > now).then(|| Skip {
            reason,
            remaining: until - now,
        })
    }

    /// Record what a search of `id` showed at `now`.
    pub(crate) fn observe(&self, id: &str, observation: Observation, now: Instant) {
        let Ok(mut markets) = self.markets.lock() else {
            return;
        };
        match observation {
            Observation::Answered => {
                markets.remove(id);
            }
            Observation::Failed {
                kind,
                retry_after_secs,
            } => match kind {
                FailureKind::Timeout | FailureKind::Network | FailureKind::Unavailable => {
                    let state = markets.entry(id.to_string()).or_default();
                    state.failures = state.failures.saturating_add(1);
                    if state.failures >= BREAKER_FAILURES {
                        state.skip = Some((SkipReason::CircuitOpen, now + BREAKER_COOLDOWN));
                    }
                }
                FailureKind::RateLimited => {
                    let wait = rate_limit_wait(retry_after_secs);
                    let state = markets.entry(id.to_string()).or_default();
                    state.skip = Some((SkipReason::RateLimited, now + wait));
                }
                FailureKind::Unauthorized
                | FailureKind::NotFound
                | FailureKind::Parse
                | FailureKind::Other => {}
            },
        }
    }
}

/// How long a 429 keeps the market out: its `Retry-After`, at least one
/// second, at most [`RATE_LIMIT_MAX`], or [`RATE_LIMIT_DEFAULT`] without one.
pub(crate) fn rate_limit_wait(retry_after_secs: Option<u64>) -> Duration {
    match retry_after_secs {
        Some(seconds) => Duration::from_secs(seconds.clamp(1, RATE_LIMIT_MAX.as_secs())),
        None => RATE_LIMIT_DEFAULT,
    }
}

/// The breakers every search in this process shares.
pub(crate) fn shared() -> &'static Breakers {
    static SHARED: std::sync::OnceLock<Breakers> = std::sync::OnceLock::new();
    SHARED.get_or_init(Breakers::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(kind: FailureKind) -> Observation {
        Observation::Failed {
            kind,
            retry_after_secs: None,
        }
    }

    #[test]
    fn two_health_failures_open_the_breaker_and_an_answer_closes_it() {
        for kind in [
            FailureKind::Timeout,
            FailureKind::Network,
            FailureKind::Unavailable,
        ] {
            let breakers = Breakers::default();
            let now = Instant::now();
            breakers.observe("m", failed(kind), now);
            assert_eq!(breakers.skip("m", now), None, "one failure is not enough");
            breakers.observe("m", failed(kind), now);
            let skip = breakers.skip("m", now).expect("open after two failures");
            assert_eq!(skip.reason, SkipReason::CircuitOpen);
            assert_eq!(skip.remaining, BREAKER_COOLDOWN);
            assert_eq!(breakers.skip("m", now + BREAKER_COOLDOWN), None);
            breakers.observe("m", Observation::Answered, now);
            assert_eq!(breakers.skip("m", now), None);
        }
    }

    #[test]
    fn a_failure_that_says_nothing_about_health_never_opens_the_breaker() {
        let breakers = Breakers::default();
        let now = Instant::now();
        for kind in [
            FailureKind::Unauthorized,
            FailureKind::NotFound,
            FailureKind::Parse,
            FailureKind::Other,
        ] {
            for _ in 0..5 {
                breakers.observe("m", failed(kind), now);
            }
        }
        assert_eq!(breakers.skip("m", now), None);
        // Nor does it count toward a later health failure.
        breakers.observe("m", failed(FailureKind::Timeout), now);
        assert_eq!(breakers.skip("m", now), None);
    }

    #[test]
    fn a_throttle_waits_for_its_retry_after_within_bounds() {
        let breakers = Breakers::default();
        let now = Instant::now();
        breakers.observe(
            "m",
            Observation::Failed {
                kind: FailureKind::RateLimited,
                retry_after_secs: Some(30),
            },
            now,
        );
        let skip = breakers.skip("m", now).unwrap();
        assert_eq!(skip.reason, SkipReason::RateLimited);
        assert_eq!(skip.remaining, Duration::from_secs(30));
        assert_eq!(breakers.skip("m", now + Duration::from_secs(30)), None);
        // Other markets are not affected.
        assert_eq!(breakers.skip("other", now), None);

        assert_eq!(rate_limit_wait(None), RATE_LIMIT_DEFAULT);
        assert_eq!(rate_limit_wait(Some(0)), Duration::from_secs(1));
        assert_eq!(rate_limit_wait(Some(86_400)), RATE_LIMIT_MAX);
    }

    #[test]
    fn a_skip_reason_has_a_snake_case_wire_spelling() {
        assert_eq!(
            serde_json::to_value(SkipReason::RateLimited).unwrap(),
            "rate_limited"
        );
        assert_eq!(
            serde_json::to_value(SkipReason::CircuitOpen).unwrap(),
            "circuit_open"
        );
    }
}
