//! 24-PPQN MIDI clock generation from a playback sample position.

/// MIDI clock ticks per quarter note.
pub const PPQN: u16 = 24;

/// Emits one clock tick per 24th of a quarter note while enabled.
#[derive(Debug, Clone)]
pub struct MidiClock {
    enabled: bool,
    next_tick: f64,
    last_position: u64,
}

impl MidiClock {
    /// A clock, enabled or disabled.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            next_tick: 0.0,
            last_position: 0,
        }
    }

    /// Whether ticks are emitted.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable ticking at the last position.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.next_tick = self.last_position as f64;
        }
    }

    /// Reset the tick cursor to a sample position.
    pub fn reset(&mut self, position_samples: u64) {
        self.next_tick = position_samples as f64;
        self.last_position = position_samples;
    }

    /// Advance to `position_samples` and return how many ticks are due.
    pub fn advance(&mut self, position_samples: u64, sample_rate: u32, tempo_bpm: f64) -> u32 {
        if position_samples < self.last_position {
            self.next_tick = position_samples as f64;
        }
        self.last_position = position_samples;
        if !self.enabled || sample_rate == 0 || !tempo_bpm.is_finite() || tempo_bpm <= 0.0 {
            return 0;
        }
        let samples_per_tick = f64::from(sample_rate) * 60.0 / (tempo_bpm * f64::from(PPQN));
        if !samples_per_tick.is_finite() || samples_per_tick <= 0.0 {
            return 0;
        }
        let target = position_samples as f64;
        let mut ticks = 0;
        while self.next_tick <= target {
            ticks += 1;
            self.next_tick += samples_per_tick;
        }
        ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_24_ticks_per_quarter_note() {
        let mut clock = MidiClock::new(true);
        // 120 BPM at 48 kHz is one quarter note every 24,000 samples.
        let mut total = 0;
        for step in 0..=24 {
            total += clock.advance(step * 1000, 48_000, 120.0);
        }
        assert_eq!(total, 25); // tick at 0 plus 24 quarter-note ticks.
    }

    #[test]
    fn disabled_clock_emits_nothing_and_resumes_from_position() {
        let mut clock = MidiClock::new(false);
        assert_eq!(clock.advance(10_000, 48_000, 120.0), 0);
        clock.set_enabled(true);
        assert_eq!(clock.advance(10_000, 48_000, 120.0), 1);
        assert_eq!(clock.advance(10_999, 48_000, 120.0), 0);
        assert_eq!(clock.advance(11_000, 48_000, 120.0), 1);
    }

    #[test]
    fn backward_position_resets_without_a_tick_burst() {
        let mut clock = MidiClock::new(true);
        assert_eq!(clock.advance(100_000, 48_000, 120.0), 101);
        assert_eq!(clock.advance(0, 48_000, 120.0), 1);
    }
}
