//! Declared audio port topology of a hosted instance.
//!
//! A port is one declared bus. Its channel count comes from the plugin, and its
//! default target pair is the stereo pair its first channel occupies in the
//! host's flat pair numbering used by `process_block_*_outputs_into`.
//!
//! The signal-kind model is shared with the built-in devices
//! (`oc_device_schema::PortKind`): a hosted bus is always an audio port, while a
//! built-in device may also declare a control port.

pub use oc_device_schema::PortKind;

/// One declared audio port: channel count and default target stereo pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPort {
    pub channels: u32,
    /// Stereo pair the port's first channel lands on, 0-based.
    pub first_pair: usize,
    /// A hosted bus carries audio; the kind exists so host ports and device
    /// ports are one model rather than two.
    pub kind: PortKind,
}

impl AudioPort {
    /// The single stereo port every one-pair host exposes.
    pub const STEREO: Self = Self {
        channels: 2,
        first_pair: 0,
        kind: PortKind::Audio,
    };

    /// Stereo pairs this port spans; a four-channel port covers two of them.
    pub fn pair_count(&self) -> usize {
        (self.channels as usize).div_ceil(2).max(1)
    }

    /// Pairs the port occupies, in host pair numbering.
    pub fn pair_range(&self) -> std::ops::Range<usize> {
        self.first_pair..self.first_pair + self.pair_count()
    }
}

/// Declared audio buses of one instance, in the plugin's own order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BusTopology {
    pub inputs: Vec<AudioPort>,
    pub outputs: Vec<AudioPort>,
}

impl BusTopology {
    /// Create a bus topology from its input and output ports.
    pub fn new(inputs: Vec<AudioPort>, outputs: Vec<AudioPort>) -> Self {
        Self { inputs, outputs }
    }

    /// Declared channels across every input bus.
    pub fn input_channels(&self) -> u32 {
        self.inputs.iter().map(|port| port.channels).sum()
    }

    /// Declared channels across every output bus.
    pub fn output_channels(&self) -> u32 {
        self.outputs.iter().map(|port| port.channels).sum()
    }

    /// Stereo pairs the declared outputs span.
    pub fn output_pairs(&self) -> usize {
        (self.output_channels() as usize).div_ceil(2)
    }

    /// Declares outputs but no audio input channel, the shape of an instrument.
    ///
    /// An insert has one stereo input: it could never feed this plugin, and the
    /// plugin's own input-buffer contract has nothing to describe.
    pub fn is_instrument(&self) -> bool {
        self.input_channels() == 0 && self.output_channels() > 0
    }
}

/// Ports from per-bus channel counts, in declaration order.
///
/// A bus wider than two channels becomes one port spanning consecutive pairs.
/// Empty input keeps the historical one-stereo-pair assumption.
pub fn ports_from_channels(channels: &[i32]) -> Vec<AudioPort> {
    ports_from_iter(channels.iter().map(|count| (*count).max(0) as u32))
}

/// Ports from per-bus channel counts already stored as unsigned.
pub fn ports_from_unsigned(channels: &[u32]) -> Vec<AudioPort> {
    ports_from_iter(channels.iter().copied())
}

fn ports_from_iter(channels: impl Iterator<Item = u32>) -> Vec<AudioPort> {
    let mut ports = Vec::new();
    let mut channel = 0usize;
    for channels in channels {
        ports.push(AudioPort {
            channels,
            first_pair: channel / 2,
            kind: PortKind::Audio,
        });
        channel += channels as usize;
    }
    if ports.is_empty() {
        ports.push(AudioPort::STEREO);
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_buses_map_one_to_one_and_wide_buses_span_pairs() {
        assert_eq!(
            ports_from_channels(&[2, 2, 2, 2]),
            vec![
                AudioPort {
                    channels: 2,
                    first_pair: 0,
                    kind: PortKind::Audio,
                },
                AudioPort {
                    channels: 2,
                    first_pair: 1,
                    kind: PortKind::Audio,
                },
                AudioPort {
                    channels: 2,
                    first_pair: 2,
                    kind: PortKind::Audio,
                },
                AudioPort {
                    channels: 2,
                    first_pair: 3,
                    kind: PortKind::Audio,
                },
            ]
        );
        let wide = ports_from_channels(&[4, 2]);
        assert_eq!(wide[0].pair_count(), 2);
        assert_eq!(wide[0].pair_range(), 0..2);
        assert_eq!(wide[1].first_pair, 2);
    }

    #[test]
    fn an_empty_layout_keeps_the_historical_stereo_pair() {
        assert_eq!(ports_from_channels(&[]), vec![AudioPort::STEREO]);
    }

    #[test]
    fn declared_outputs_without_an_input_channel_is_an_instrument() {
        // An instrument that declares 32 stereo output buses and no input bus.
        let instrument = BusTopology::new(Vec::new(), ports_from_channels(&[2; 32]));
        assert!(instrument.is_instrument());
        assert_eq!(instrument.input_channels(), 0);
        assert_eq!(instrument.output_pairs(), 32);

        // A zero-channel input bus carries no audio either.
        let inactive = BusTopology::new(ports_from_channels(&[0]), ports_from_channels(&[2]));
        assert!(inactive.is_instrument());
    }

    #[test]
    fn a_stereo_effect_and_a_source_without_outputs_are_not_instruments() {
        let effect = BusTopology::new(ports_from_channels(&[2]), ports_from_channels(&[2]));
        assert!(!effect.is_instrument());
        // A plugin that declares nothing is left to its own load path.
        assert!(!BusTopology::default().is_instrument());
    }
}
