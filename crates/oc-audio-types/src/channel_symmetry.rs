use crate::MidiInputMessage;

/// Exact equality of the two source channels, before the track fader and pan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelSymmetry {
    DualMono,
    #[default]
    Unknown,
}

/// The complete future control program, not merely the requested render window.
/// Unknown event families and incomplete programs must retain the DSP witness.
#[derive(Debug)]
pub struct ChannelSymmetryContext<'a> {
    pub parameter_changes: &'a [&'a str],
    pub midi: &'a [MidiInputMessage],
    pub note_pans: &'a [f64],
    pub has_unclassified_events: bool,
}

impl Default for ChannelSymmetryContext<'_> {
    fn default() -> Self {
        Self {
            parameter_changes: &[],
            midi: &[],
            note_pans: &[],
            has_unclassified_events: true,
        }
    }
}

impl<'a> ChannelSymmetryContext<'a> {
    /// Call only after every event in the source's full lifetime was classified.
    pub fn complete(
        parameter_changes: &'a [&'a str],
        midi: &'a [MidiInputMessage],
        note_pans: &'a [f64],
    ) -> Self {
        Self {
            parameter_changes,
            midi,
            note_pans,
            has_unclassified_events: false,
        }
    }

    /// Basic input validity shared by owner-specific proofs. MIDI controller
    /// behavior and parameter semantics remain the responsibility of the owner.
    pub fn is_complete_and_centered(&self) -> bool {
        !self.has_unclassified_events
            && self
                .note_pans
                .iter()
                .all(|pan| pan.is_finite() && *pan == 0.0)
            && self.midi.iter().all(|message| message.bytes().is_ok())
    }
}
