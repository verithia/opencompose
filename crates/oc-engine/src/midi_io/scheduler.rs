//! Schedule engine events and MIDI clock from a playback cursor.

use crate::interpret::MidiEvent;

use super::clock::MidiClock;
use super::message::{LiveEvent, encode_engine_message};

/// Converts due engine events plus clock ticks into wire events.
///
/// Construct once off the audio thread; `advance` only writes into the caller's
/// preallocated slice and never allocates.
pub struct OutputScheduler {
    events: Vec<MidiEvent>,
    next_event: usize,
    clock: MidiClock,
    last_position: u64,
}

impl OutputScheduler {
    /// Schedule events in time order with optional clock.
    pub fn new(mut events: Vec<MidiEvent>, clock_enabled: bool) -> Self {
        events.sort_by_key(|event| event.time_samples);
        Self {
            events,
            next_event: 0,
            clock: MidiClock::new(clock_enabled),
            last_position: 0,
        }
    }

    /// Enable or disable clock ticks.
    pub fn set_clock_enabled(&mut self, enabled: bool) {
        self.clock.set_enabled(enabled);
    }

    /// Whether clock ticks are emitted.
    pub fn clock_enabled(&self) -> bool {
        self.clock.enabled()
    }

    /// Fill `out` with events due at `position_samples`; returns the count.
    pub fn advance(
        &mut self,
        position_samples: u64,
        sample_rate: u32,
        tempo_bpm: f64,
        out: &mut [LiveEvent],
    ) -> usize {
        if position_samples < self.last_position {
            self.next_event = self
                .events
                .partition_point(|event| event.time_samples < position_samples);
            self.clock.reset(position_samples);
        }
        self.last_position = position_samples;
        let mut written = 0;
        while self.next_event < self.events.len()
            && self.events[self.next_event].time_samples <= position_samples
        {
            // Reserve room for the widest encoding (14-bit CC uses two events).
            if out.len().saturating_sub(written) < 2 {
                return written;
            }
            let event = &self.events[self.next_event];
            let mut encoded = [LiveEvent::default(); 2];
            let count = encode_engine_message(&event.event, &mut encoded);
            self.next_event += 1;
            for live in &encoded[..count] {
                out[written] = *live;
                written += 1;
            }
        }
        let ticks = self.clock.advance(position_samples, sample_rate, tempo_bpm);
        for _ in 0..ticks {
            if written == out.len() {
                break;
            }
            out[written] = LiveEvent::Clock;
            written += 1;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpret::{MidiEvent, MidiMessage};

    fn event(time_samples: u64, event: MidiMessage) -> MidiEvent {
        MidiEvent {
            time_samples,
            event,
            track_index: 0,
        }
    }

    #[test]
    fn due_events_and_clock_are_emitted_once() {
        let events = vec![
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            ),
            event(
                1000,
                MidiMessage::NoteOff {
                    channel: 0,
                    note: 60,
                },
            ),
        ];
        let mut scheduler = OutputScheduler::new(events, true);
        let mut out = [LiveEvent::default(); 32];
        // 120 BPM at 48 kHz: one clock per 1000 samples.
        assert_eq!(scheduler.advance(0, 48_000, 120.0, &mut out), 2);
        assert!(matches!(out[0], LiveEvent::NoteOn { .. }));
        assert_eq!(out[1], LiveEvent::Clock);
        assert_eq!(scheduler.advance(500, 48_000, 120.0, &mut out), 0);
        let written = scheduler.advance(1000, 48_000, 120.0, &mut out);
        assert_eq!(written, 2);
        assert!(matches!(out[0], LiveEvent::NoteOff { .. }));
        assert_eq!(out[1], LiveEvent::Clock);
        assert_eq!(scheduler.advance(1000, 48_000, 120.0, &mut out), 0);
    }

    #[test]
    fn clock_can_be_disabled() {
        let mut scheduler = OutputScheduler::new(Vec::new(), false);
        let mut out = [LiveEvent::default(); 4];
        assert_eq!(scheduler.advance(10_000, 48_000, 120.0, &mut out), 0);
        scheduler.set_clock_enabled(true);
        assert_eq!(scheduler.advance(10_000, 48_000, 120.0, &mut out), 1);
    }

    #[test]
    fn backward_cursor_resets_without_replaying_old_events() {
        let events = vec![
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 1,
                },
            ),
            event(
                100,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 61,
                    velocity: 1,
                },
            ),
        ];
        let mut scheduler = OutputScheduler::new(events, false);
        let mut out = [LiveEvent::default(); 4];
        assert_eq!(scheduler.advance(200, 48_000, 120.0, &mut out), 2);
        assert_eq!(scheduler.advance(0, 48_000, 120.0, &mut out), 1);
        assert!(matches!(out[0], LiveEvent::NoteOn { note: 60, .. }));
        assert_eq!(scheduler.advance(50, 48_000, 120.0, &mut out), 0);
    }
}
