//! Allocation-free single-channel arpeggiator clock. Route channels independently.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Timing and direction for one arpeggiator lane.
pub struct ArpSettings {
    pub beats_per_step: f64,
    pub gate: f64,
    pub descending: bool,
}

impl ArpSettings {
    /// Check step length and gate bounds.
    pub fn validate(self) -> Result<(), &'static str> {
        if !self.beats_per_step.is_finite()
            || !(0.03125..=16.0).contains(&self.beats_per_step)
            || !self.gate.is_finite()
            || !(0.01..=1.0).contains(&self.gate)
        {
            return Err("invalid arpeggiator timing");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// One generated note-on or note-off.
pub enum ArpEvent {
    On(u8, u8),
    Off(u8),
}

/// Allocation-free arpeggiator clock for one MIDI channel.
pub struct Arpeggiator {
    held: [u8; 128],
    pressed: [bool; 128],
    sustain: bool,
    held_count: usize,
    active: Option<u8>,
    settings: ArpSettings,
    period: f64,
    until_step: f64,
    until_off: f64,
    index: usize,
}

impl Arpeggiator {
    /// Whether keys are held or a note is active.
    pub fn has_notes(&self) -> bool {
        self.held_count > 0 || self.active.is_some()
    }

    /// Retime the running pattern to a new tempo and sample rate.
    pub fn set_clock(&mut self, tempo: f64, sample_rate: u32) {
        let period = sample_rate as f64 * 60.0 / tempo * self.settings.beats_per_step;
        if period.is_finite() && period >= 1.0 {
            let ratio = period / self.period;
            self.until_step *= ratio;
            self.until_off *= ratio;
            self.period = period;
        }
    }
    /// Create an arpeggiator, rejecting invalid settings or clock.
    pub fn new(settings: ArpSettings, tempo: f64, sample_rate: u32) -> Result<Self, &'static str> {
        settings.validate()?;
        if !tempo.is_finite() || tempo <= 0.0 || sample_rate == 0 {
            return Err("invalid clock");
        }
        let period = sample_rate as f64 * 60.0 / tempo * settings.beats_per_step;
        if !period.is_finite() || period < 1.0 {
            return Err("arp period below one sample");
        }
        Ok(Self {
            held: [0; 128],
            pressed: [false; 128],
            sustain: false,
            held_count: 0,
            active: None,
            settings,
            period,
            until_step: 0.0,
            until_off: 0.0,
            index: 0,
        })
    }

    /// Hold a note; velocity zero releases it.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        if note > 127 {
            return;
        }
        if velocity == 0 {
            self.note_off(note);
            return;
        }
        if self.held_count == 0 {
            self.until_step = 0.0;
            self.index = 0;
        }
        if self.held[note as usize] == 0 {
            self.held_count += 1;
        }
        self.held[note as usize] = velocity.min(127);
        self.pressed[note as usize] = true;
    }

    /// Release a note unless sustain keeps it held.
    pub fn note_off(&mut self, note: u8) {
        if note >= 128 {
            return;
        }
        self.pressed[note as usize] = false;
        if !self.sustain && self.held[note as usize] != 0 {
            self.held[note as usize] = 0;
            self.held_count -= 1;
        }
    }

    /// Enable or release the sustain pedal.
    pub fn set_sustain(&mut self, enabled: bool) {
        self.sustain = enabled;
        if !enabled {
            for note in 0..128 {
                if !self.pressed[note] && self.held[note] != 0 {
                    self.held[note] = 0;
                    self.held_count -= 1;
                }
            }
        }
    }

    /// Panic returns the generated note-off; callers must forward it immediately.
    pub fn reset(&mut self) -> Option<ArpEvent> {
        self.held.fill(0);
        self.pressed.fill(false);
        self.sustain = false;
        self.held_count = 0;
        self.until_step = 0.0;
        self.until_off = 0.0;
        self.index = 0;
        self.active.take().map(ArpEvent::Off)
    }

    /// Call once per sample after applying input events at that sample.
    /// At most one note-off followed by one note-on; no event allocation.
    /// Advance one sample and return the generated events.
    pub fn tick(&mut self) -> [Option<ArpEvent>; 2] {
        let mut events = [None, None];
        if let Some(note) = self.active
            && (self.until_off <= 0.0 || self.held[note as usize] == 0 || self.until_step <= 0.0)
        {
            events[0] = Some(ArpEvent::Off(note));
            self.active = None;
        }
        let count = self.held_count;
        if count > 0 && self.until_step <= 0.0 {
            let position = self.index % count;
            let position = if self.settings.descending {
                count - 1 - position
            } else {
                position
            };
            let (note, velocity) = self
                .held
                .iter()
                .enumerate()
                .filter(|(_, v)| **v != 0)
                .nth(position)
                .unwrap();
            events[1] = Some(ArpEvent::On(note as u8, *velocity));
            self.active = Some(note as u8);
            self.index = (self.index + 1) % count;
            self.until_step += self.period;
            self.until_off = (self.period * self.settings.gate).max(1.0);
        }
        if count > 0 {
            self.until_step -= 1.0;
        }
        self.until_off -= 1.0;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tempo_gate_order_and_release() {
        let mut arp = Arpeggiator::new(
            ArpSettings {
                beats_per_step: 0.5,
                gate: 0.5,
                descending: false,
            },
            60.0,
            200,
        )
        .unwrap();
        arp.note_on(64, 90);
        arp.note_on(60, 80);
        assert_eq!(arp.tick(), [None, Some(ArpEvent::On(60, 80))]);
        for _ in 1..50 {
            assert_eq!(arp.tick(), [None, None]);
        }
        assert_eq!(arp.tick()[0], Some(ArpEvent::Off(60)));
        for _ in 51..100 {
            arp.tick();
        }
        assert_eq!(arp.tick()[1], Some(ArpEvent::On(64, 90)));
        arp.note_off(64);
        assert_eq!(arp.tick()[0], Some(ArpEvent::Off(64)));
        arp.reset();
        for _ in 0..300 {
            assert_eq!(arp.tick(), [None, None]);
        }
    }
    #[test]
    fn clock_change_rescales_remaining_gate_and_step_without_retrigger() {
        let mut arp = Arpeggiator::new(
            ArpSettings {
                beats_per_step: 1.0,
                gate: 0.5,
                descending: false,
            },
            120.0,
            200,
        )
        .unwrap();
        arp.note_on(60, 80);
        for _ in 0..25 {
            arp.tick();
        }
        let remaining_step = arp.until_step;
        let remaining_gate = arp.until_off;
        let active = arp.active;
        let index = arp.index;
        arp.set_clock(60.0, 200);
        assert_eq!(arp.until_step, remaining_step * 2.0);
        assert_eq!(arp.until_off, remaining_gate * 2.0);
        assert_eq!(arp.active, active);
        assert_eq!(arp.index, index);
        assert_eq!(arp.tick(), [None, None]);
    }

    #[test]
    fn rejects_bad_clocks() {
        let settings = ArpSettings {
            beats_per_step: 0.5,
            gate: 0.5,
            descending: false,
        };
        assert!(Arpeggiator::new(settings, f64::NAN, 48000).is_err());
        assert!(Arpeggiator::new(settings, 120.0, 0).is_err());
    }

    #[test]
    fn pedal_holds_input_notes_but_preserves_output_gate() {
        let mut arp = Arpeggiator::new(
            ArpSettings {
                beats_per_step: 0.5,
                gate: 0.5,
                descending: true,
            },
            60.0,
            200,
        )
        .unwrap();
        arp.note_on(60, 80);
        arp.set_sustain(true);
        assert_eq!(arp.tick()[1], Some(ArpEvent::On(60, 80)));
        arp.note_off(60);
        for _ in 1..50 {
            arp.tick();
        }
        assert_eq!(arp.tick()[0], Some(ArpEvent::Off(60)));
        assert!(arp.has_notes());
        arp.set_sustain(false);
        assert!(!arp.has_notes());
    }
}
