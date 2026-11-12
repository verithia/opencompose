//! Touch override for automated parameters.
//!
//! One rule drives every automated parameter: while a touch is held the hand
//! value wins and the lane is suppressed; when the touch ends the lane takes
//! the parameter back through a fixed 20 ms crossfade. [`TouchLane::plan`] is
//! the single place that decides "lane vs hand" for one parameter and one
//! block; lane writers only apply the returned plan.
//!
//! The registry is live-only state. Offline rendering never receives it, so a
//! touch held during playback cannot change an offline render.

use crate::smoothing::frames_for_ms;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Length of the hand-to-lane fade after a touch is released.
///
/// Fixed rather than configurable: it is short enough to read as "the lane
/// took over" and long enough that a 20 dB jump cannot click.
pub const RELEASE_FADE_MS: f64 = 20.0;

/// Fade length in samples for a sample rate; always at least one sample.
pub fn release_fade_samples(sample_rate: u32) -> u32 {
    frames_for_ms(RELEASE_FADE_MS, sample_rate)
}

/// Longest span one touch decision covers. A release is noticed within this
/// span, so touch latency does not depend on the mix chunk size.
pub const POLL_FRAMES: u64 = 512;

/// Registry key of one effect parameter. `effect_index` is the position in the
/// owner's own `effects:` list, the same index the GUI edits.
pub fn effect_key(owner: &str, effect_index: usize, param: &str) -> String {
    format!("{owner}|fx{effect_index}|{param}")
}

/// Registry key of the post-insert gain clip of a bus or the master.
pub fn gain_key(owner: &str) -> String {
    format!("{owner}|gain")
}

static NEXT_TOUCH_IDENTITY: AtomicU64 = AtomicU64::new(1);

fn next_touch_identity(counter: &AtomicU64) -> Result<u64, TouchError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| TouchError::IdentityExhausted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why touch ownership could not be granted.
pub enum TouchError {
    IdentityExhausted,
    NonfiniteValue,
}
impl std::fmt::Display for TouchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IdentityExhausted => "Touch identity space is exhausted",
            Self::NonfiniteValue => "Touch value must be finite",
        })
    }
}
impl std::error::Error for TouchError {}

/// Opaque control-session identity, separate from the DSP fade serial.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TouchToken(String);
impl TouchToken {
    fn matches(&self, owner: Option<u64>) -> bool {
        !self.0.starts_with('0')
            && self.0.bytes().all(|byte| byte.is_ascii_digit())
            && self
                .0
                .parse::<u64>()
                .ok()
                .zip(owner)
                .is_some_and(|(token, owner)| token == owner)
    }
}

/// What the registry holds for one parameter right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchStatus {
    /// Hand value in the lane's own value domain.
    pub hand: f64,
    pub held: bool,
    /// Bumped on every begin and release, so a lane can spot a transition.
    pub serial: u64,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    owner: Option<u64>,
    hand: f64,
    held: bool,
    serial: u64,
}

/// Live touch state, shared between the GUI commands and the render thread.
#[derive(Default)]
pub struct TouchRegistry {
    slots: Mutex<HashMap<String, Slot>>,
    serial: AtomicU64,
}

impl TouchRegistry {
    /// An empty touch registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn next_serial(&self) -> u64 {
        self.serial.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Start a token-owned touch. Identity never repeats across registries.
    pub fn begin_owned(&self, key: &str, hand: f64) -> Result<TouchToken, TouchError> {
        if !hand.is_finite() {
            return Err(TouchError::NonfiniteValue);
        }
        let identity = next_touch_identity(&NEXT_TOUCH_IDENTITY)?;
        let mut slots = self.lock();
        slots.insert(
            key.to_owned(),
            Slot {
                owner: Some(identity),
                hand,
                held: true,
                serial: self.next_serial(),
            },
        );
        Ok(TouchToken(identity.to_string()))
    }

    /// Update a held touch when the token owns it.
    pub fn update_owned(&self, key: &str, token: &TouchToken, hand: f64) -> bool {
        if !hand.is_finite() {
            return false;
        }
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(key)
            && slot.held
            && token.matches(slot.owner)
        {
            slot.hand = hand;
            return true;
        }
        false
    }

    /// Release a held touch when the token owns it.
    pub fn release_owned(&self, key: &str, token: &TouchToken) -> bool {
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(key)
            && slot.held
            && token.matches(slot.owner)
        {
            slot.held = false;
            slot.serial = self.next_serial();
            return true;
        }
        false
    }

    /// Retire owned history entirely: a replacement graph must not inherit an
    /// old target's hand value or pending release fade. Legacy slots stay live.
    pub fn release_all_owned(&self) {
        self.lock().retain(|_, slot| slot.owner.is_none());
    }

    /// Start or continue a touch. A second begin on a held parameter only
    /// moves the hand value, so the lane keeps its current fade state.
    pub fn begin(&self, key: &str, hand: f64) {
        self.begin_legacy(key, hand);
    }

    /// Begin a legacy touch without a token.
    pub fn begin_legacy(&self, key: &str, hand: f64) -> bool {
        let mut slots = self.lock();
        if slots
            .get(key)
            .is_some_and(|slot| slot.held && slot.owner.is_some())
        {
            return false;
        }
        let serial = match slots.get(key) {
            Some(slot) if slot.held => slot.serial,
            _ => self.next_serial(),
        };
        slots.insert(
            key.to_string(),
            Slot {
                owner: None,
                hand,
                held: true,
                serial,
            },
        );
        true
    }

    /// Move the hand value while the touch is held; ignored once released.
    pub fn update(&self, key: &str, hand: f64) {
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(key)
            && slot.held
            && slot.owner.is_none()
        {
            slot.hand = hand;
        }
    }

    /// End a touch. The lane takes over again through the release fade.
    pub fn release(&self, key: &str) {
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(key)
            && slot.held
            && slot.owner.is_none()
        {
            slot.held = false;
            slot.serial = self.next_serial();
        }
    }

    /// True while the parameter is touched.
    pub fn is_held(&self, key: &str) -> bool {
        self.lock().get(key).is_some_and(|slot| slot.held)
    }

    /// Drop every touch. Called when playback stops, so a touch that outlived
    /// its gesture cannot leak into the next run.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Current touch status for a parameter.
    pub fn status(&self, key: &str) -> Option<TouchStatus> {
        self.lock().get(key).map(|slot| TouchStatus {
            hand: slot.hand,
            held: slot.held,
            serial: slot.serial,
        })
    }
}

/// What one parameter does for one block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchPlan {
    /// No touch state: the lane writes as usual.
    Lane,
    /// The hand owns the parameter; apply `value` at the block start and ignore
    /// lane points.
    Hand(f64),
    /// The touch just ended: crossfade from `from` toward the lane. Progress at
    /// a block offset is `(elapsed + offset) / total`.
    Release { from: f64, elapsed: u32, total: u32 },
}

impl TouchPlan {
    /// Effective value at `offset`, with the lane value at that offset.
    /// `None` means the lane owns the block and the caller applies it normally.
    pub fn value(self, lane: f64, offset: u32) -> Option<f64> {
        match self {
            Self::Lane => None,
            Self::Hand(hand) => Some(hand),
            Self::Release {
                from,
                elapsed,
                total,
            } => {
                let progress = ((elapsed + offset) as f64 / total as f64).clamp(0.0, 1.0);
                Some(from + (lane - from) * progress)
            }
        }
    }

    /// Offset where the fade reaches the lane. `0` is the catch-up block that
    /// writes the lane value at the block start after a fade ended on the
    /// previous boundary. Lane points before it are suppressed.
    pub fn fade_end(self) -> Option<u32> {
        match self {
            Self::Release { elapsed, total, .. } if elapsed <= total => {
                Some(total.saturating_sub(elapsed))
            }
            _ => None,
        }
    }
}

/// Per-parameter arbitration state on the live render path.
pub struct TouchLane {
    key: String,
    serial: u64,
    hand: Option<f64>,
    fade_from: f64,
    fade_total: u32,
    fade_elapsed: u32,
    /// A fade finished on the previous block: the next block writes the lane
    /// value once before the lane takes over on its own points.
    catch_up: bool,
}

impl TouchLane {
    /// A touch lane for one parameter key.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            serial: 0,
            hand: None,
            fade_from: 0.0,
            fade_total: 0,
            fade_elapsed: 0,
            catch_up: false,
        }
    }

    /// Parameter key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The one arbitration decision: who owns this parameter for this block.
    pub fn plan(
        &mut self,
        status: Option<TouchStatus>,
        frames: u32,
        sample_rate: u32,
    ) -> TouchPlan {
        match status {
            Some(status) if status.held => {
                self.serial = status.serial;
                self.hand = Some(status.hand);
                self.fade_total = 0;
                self.fade_elapsed = 0;
                self.catch_up = false;
                TouchPlan::Hand(status.hand)
            }
            Some(status) => {
                if self.hand.is_none() {
                    // This lane never saw the held state (the graph was rebuilt
                    // after the release): the fade already happened.
                    self.serial = status.serial;
                    return TouchPlan::Lane;
                }
                if self.serial != status.serial || self.fade_total == 0 {
                    self.serial = status.serial;
                    self.fade_from = self.hand.unwrap_or(status.hand);
                    self.fade_total = release_fade_samples(sample_rate);
                    self.fade_elapsed = 0;
                }
                if self.fade_elapsed >= self.fade_total {
                    if !self.catch_up {
                        return TouchPlan::Lane;
                    }
                    self.catch_up = false;
                    return TouchPlan::Release {
                        from: self.fade_from,
                        elapsed: self.fade_total,
                        total: self.fade_total.max(1),
                    };
                }
                let elapsed = self.fade_elapsed;
                self.fade_elapsed = self.fade_elapsed.saturating_add(frames);
                self.catch_up = self.fade_elapsed >= self.fade_total;
                TouchPlan::Release {
                    from: self.fade_from,
                    elapsed,
                    total: self.fade_total.max(1),
                }
            }
            None => {
                self.fade_total = 0;
                self.fade_elapsed = 0;
                self.catch_up = false;
                TouchPlan::Lane
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_touch_hands_the_parameter_to_the_hand() {
        let registry = TouchRegistry::new();
        let key = effect_key("track:0", 1, "factor");
        registry.begin(&key, 0.1);
        let status = registry.status(&key).unwrap();
        assert!(status.held);
        assert_eq!(status.hand, 0.1);

        let mut lane = TouchLane::new(&key);
        assert_eq!(lane.plan(Some(status), 128, 48_000), TouchPlan::Hand(0.1));
        registry.update(&key, 0.25);
        let status = registry.status(&key).unwrap();
        assert_eq!(lane.plan(Some(status), 128, 48_000), TouchPlan::Hand(0.25));
    }

    #[test]
    fn release_fades_to_the_lane_over_twenty_ms() {
        let registry = TouchRegistry::new();
        let key = gain_key("master");
        let mut lane = TouchLane::new(&key);
        registry.begin(&key, 0.1);
        assert_eq!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Hand(0.1)
        );
        registry.release(&key);
        let total = release_fade_samples(48_000);
        assert_eq!(total, 960);

        let first = lane.plan(registry.status(&key), 64, 48_000);
        assert_eq!(
            first,
            TouchPlan::Release {
                from: 0.1,
                elapsed: 0,
                total
            }
        );
        // Halfway through the fade the value is halfway to the lane.
        let value = first.value(1.0, 480).unwrap();
        assert!((value - 0.55).abs() < 1e-9);
        // The fade is done after `total` samples; the lane owns the parameter.
        let mut elapsed = 64;
        let mut plan = first;
        while elapsed < total {
            plan = lane.plan(registry.status(&key), 64, 48_000);
            elapsed += 64;
        }
        assert!(matches!(plan, TouchPlan::Release { .. }));
        // One catch-up block writes the lane value, then the lane owns it.
        assert_eq!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Release {
                from: 0.1,
                elapsed: total,
                total
            }
        );
        assert_eq!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Lane
        );
    }

    #[test]
    fn a_new_touch_during_a_fade_snaps_back_to_the_hand() {
        let registry = TouchRegistry::new();
        let key = effect_key("bus:drums", 0, "threshold");
        let mut lane = TouchLane::new(&key);
        registry.begin(&key, -20.0);
        lane.plan(registry.status(&key), 64, 48_000);
        registry.release(&key);
        assert!(matches!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Release { .. }
        ));
        registry.begin(&key, -6.0);
        assert_eq!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Hand(-6.0)
        );
    }

    #[test]
    fn a_lane_built_after_the_release_does_not_fade_again() {
        let registry = TouchRegistry::new();
        let key = effect_key("track:0", 0, "factor");
        registry.begin(&key, 0.1);
        registry.release(&key);
        // A rebuilt graph's lane never saw `Held`; the fade belongs to the old
        // lane, so the new one starts on the lane immediately.
        let mut rebuilt = TouchLane::new(&key);
        assert_eq!(
            rebuilt.plan(registry.status(&key), 128, 48_000),
            TouchPlan::Lane
        );
    }

    #[test]
    fn clearing_the_registry_returns_lanes_to_their_writers() {
        let registry = TouchRegistry::new();
        let key = effect_key("master", 2, "ceiling");
        let mut lane = TouchLane::new(&key);
        registry.begin(&key, -3.0);
        assert!(matches!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Hand(_)
        ));
        registry.clear();
        assert_eq!(
            lane.plan(registry.status(&key), 64, 48_000),
            TouchPlan::Lane
        );
        assert!(!registry.is_held(&key));
    }
}

#[cfg(test)]
mod owned_tests {
    use super::*;

    #[test]
    fn ownership_is_exact_and_legacy_cannot_consume_it() {
        let registry = TouchRegistry::new();
        let first = registry.begin_owned("a", 0.2).unwrap();
        let serial = registry.status("a").unwrap().serial;
        assert!(!registry.begin_legacy("a", 0.9));
        registry.update("a", 0.9);
        registry.release("a");
        assert_eq!(registry.status("a").unwrap().hand, 0.2);
        assert!(registry.is_held("a"));
        let second = registry.begin_owned("a", 0.4).unwrap();
        assert_ne!(first, second);
        assert_ne!(registry.status("a").unwrap().serial, serial);
        assert!(!registry.update_owned("a", &first, 0.8));
        assert!(!registry.release_owned("a", &first));
        assert!(!registry.update_owned("b", &second, 0.8));
        assert!(registry.update_owned("a", &second, 0.5));
        let held = registry.status("a").unwrap();
        assert!(registry.release_owned("a", &second));
        let released = registry.status("a").unwrap();
        assert!(!released.held);
        assert_ne!(held.serial, released.serial);
        assert!(!registry.release_owned("a", &second));
        let mut lane = TouchLane::new("a");
        assert_eq!(lane.plan(Some(held), 1, 48_000), TouchPlan::Hand(0.5));
        assert_eq!(
            lane.plan(Some(released), 1, 48_000),
            TouchPlan::Release {
                from: 0.5,
                elapsed: 0,
                total: 960
            }
        );
    }

    #[test]
    fn retirement_clear_and_registry_replacement_never_reuse_identity() {
        let registry = TouchRegistry::new();
        registry.begin("legacy", 0.7);
        let old = registry.begin_owned("a", 0.1).unwrap();
        registry.release_all_owned();
        assert!(!registry.is_held("a"));
        assert!(registry.is_held("legacy"));
        let fresh = registry.begin_owned("a", 0.2).unwrap();
        assert!(!registry.update_owned("a", &old, 0.8));
        registry.clear();
        assert!(!registry.update_owned("a", &fresh, 0.8));
        let replacement = TouchRegistry::new();
        let newest = replacement.begin_owned("a", 0.3).unwrap();
        assert_ne!(newest, old);
        assert_ne!(newest, fresh);
        assert!(!replacement.release_owned("a", &fresh));
        assert!(replacement.is_held("a"));
    }

    #[test]
    fn retirement_removes_held_and_released_provenance_without_a_new_graph_fade() {
        let registry = TouchRegistry::new();
        registry.begin("legacy", 0.7);
        let held = registry.begin_owned("held", 0.2).unwrap();
        let released = registry.begin_owned("released", 0.4).unwrap();
        assert!(registry.release_owned("released", &released));
        assert!(registry.status("released").is_some());
        registry.release_all_owned();
        assert_eq!(registry.status("held"), None);
        assert_eq!(registry.status("released"), None);
        for key in ["held", "released"] {
            let mut fresh_lane = TouchLane::new(key);
            assert_eq!(
                fresh_lane.plan(registry.status(key), 128, 48_000),
                TouchPlan::Lane
            );
        }
        assert!(registry.is_held("legacy"));
        assert!(!registry.release_owned("held", &held));
        assert!(!registry.release_owned("released", &released));
        // A released slot is available to legacy callers, which establish new provenance.
        let prior = registry.begin_owned("replace", 0.3).unwrap();
        assert!(registry.release_owned("replace", &prior));
        assert!(registry.begin_legacy("replace", 0.8));
        registry.release_all_owned();
        assert!(registry.is_held("replace"));
        assert_eq!(registry.status("replace").unwrap().hand, 0.8);
    }

    #[test]
    fn exhausted_identity_and_nonfinite_values_fail_closed() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_touch_identity(&counter), Ok(u64::MAX - 1));
        assert!(next_touch_identity(&counter).is_err());
        assert!(next_touch_identity(&counter).is_err());
        let registry = TouchRegistry::new();
        assert!(registry.begin_owned("a", f64::NAN).is_err());
        assert_eq!(registry.status("a"), None);
        let token = registry.begin_owned("a", 0.2).unwrap();
        assert!(!registry.update_owned("a", &token, f64::INFINITY));
        assert_eq!(registry.status("a").unwrap().hand, 0.2);
    }
}
