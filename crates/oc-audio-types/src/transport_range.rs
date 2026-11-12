//! Checked sample-range arithmetic shared by processing adapters.

use crate::PluginTransport;

/// Why a requested processing block cannot be placed on the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRangeError {
    FrameCountOutOfRange,
    SamplePositionOverflow { start: u64, frames: u64 },
}

impl std::fmt::Display for TransportRangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameCountOutOfRange => formatter
                .write_str("The processing frame count exceeds the transport sample domain"),
            Self::SamplePositionOverflow { start, frames } => write!(
                formatter,
                "The processing range starting at sample {start} with {frames} frames exceeds the transport sample domain"
            ),
        }
    }
}
impl std::error::Error for TransportRangeError {}

/// Processing-clock admission, independent of any plugin format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportBlockError {
    InvalidSampleRate,
    SampleRateMismatch { prepared: u32, transport: u32 },
    InvalidTempo,
    InvalidMeter,
    Range(TransportRangeError),
}
impl std::fmt::Display for TransportBlockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSampleRate => {
                formatter.write_str("The processing sample rate must be positive")
            }
            Self::SampleRateMismatch {
                prepared,
                transport,
            } => write!(
                formatter,
                "The transport sample rate {transport} differs from the prepared sample rate {prepared}"
            ),
            Self::InvalidTempo => {
                formatter.write_str("The processing tempo must be finite and positive")
            }
            Self::InvalidMeter => formatter
                .write_str("The processing meter must have positive numerator and denominator"),
            Self::Range(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for TransportBlockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Range(error) => Some(error),
            _ => None,
        }
    }
}

impl PluginTransport {
    /// Validate a borrowed block clock against prepared processing resources.
    /// This only reads scalar fields and checked range arithmetic; no clock
    /// clone, allocation, native call or mutation occurs during admission.
    pub fn checked_process_block(
        &self,
        prepared_sample_rate: u32,
        frames: usize,
    ) -> Result<u64, TransportBlockError> {
        if prepared_sample_rate == 0 || self.sample_rate == 0 {
            return Err(TransportBlockError::InvalidSampleRate);
        }
        if self.sample_rate != prepared_sample_rate {
            return Err(TransportBlockError::SampleRateMismatch {
                prepared: prepared_sample_rate,
                transport: self.sample_rate,
            });
        }
        if !self.tempo.is_finite() || self.tempo <= 0.0 {
            return Err(TransportBlockError::InvalidTempo);
        }
        if self.time_sig_numerator == 0 || self.time_sig_denominator == 0 {
            return Err(TransportBlockError::InvalidMeter);
        }
        self.checked_block_end(frames)
            .map_err(TransportBlockError::Range)
    }

    /// Exclusive end of a processing block. Empty blocks admit every sample
    /// position; a nonempty block must also represent its following cursor.
    pub fn checked_block_end(&self, frames: usize) -> Result<u64, TransportRangeError> {
        let frames =
            u64::try_from(frames).map_err(|_| TransportRangeError::FrameCountOutOfRange)?;
        self.sample_pos
            .checked_add(frames)
            .ok_or(TransportRangeError::SamplePositionOverflow {
                start: self.sample_pos,
                frames,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_clock_admission_preserves_valid_and_empty_range_boundaries() {
        let transport = PluginTransport::new(u64::MAX - 2, 48_000, 120.0);
        assert_eq!(transport.checked_process_block(48_000, 2), Ok(u64::MAX));
        assert!(matches!(
            transport.checked_process_block(48_000, 3),
            Err(TransportBlockError::Range(
                TransportRangeError::SamplePositionOverflow { .. }
            ))
        ));
        assert_eq!(
            transport
                .at_sample(u64::MAX)
                .checked_process_block(48_000, 0),
            Ok(u64::MAX)
        );
    }
    #[test]
    fn process_clock_admission_refuses_rate_tempo_and_meter_without_mutation() {
        let original = PluginTransport::new(17, 48_000, 120.0);
        assert_eq!(
            original.checked_process_block(44_100, 128),
            Err(TransportBlockError::SampleRateMismatch {
                prepared: 44_100,
                transport: 48_000
            })
        );
        assert_eq!(
            original.checked_process_block(0, 128),
            Err(TransportBlockError::InvalidSampleRate)
        );
        let mut invalid = original.clone();
        invalid.sample_rate = 0;
        assert_eq!(
            invalid.checked_process_block(48_000, 128),
            Err(TransportBlockError::InvalidSampleRate)
        );
        for tempo in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut invalid = original.clone();
            invalid.tempo = tempo;
            assert_eq!(
                invalid.checked_process_block(48_000, 128),
                Err(TransportBlockError::InvalidTempo)
            );
        }
        for (numerator, denominator) in [(0, 4), (4, 0)] {
            let mut invalid = original.clone();
            invalid.time_sig_numerator = numerator;
            invalid.time_sig_denominator = denominator;
            assert_eq!(
                invalid.checked_process_block(48_000, 128),
                Err(TransportBlockError::InvalidMeter)
            );
        }
        assert_eq!(original.sample_pos, 17);
        assert_eq!(original.sample_rate, 48_000);
        assert_eq!(original.tempo, 120.0);
    }

    #[test]
    fn checked_transport_range_preserves_exclusive_end_and_empty_boundaries() {
        let transport = PluginTransport::new(u64::MAX - 2, 48_000, 120.0);
        assert_eq!(
            oc_test_support::operations(|| {
                assert_eq!(transport.checked_block_end(2), Ok(u64::MAX));
                assert!(transport.checked_block_end(3).is_err());
            }),
            0
        );
        assert_eq!(transport.checked_block_end(2), Ok(u64::MAX));
        assert_eq!(
            transport.checked_block_end(3),
            Err(TransportRangeError::SamplePositionOverflow {
                start: u64::MAX - 2,
                frames: 3
            })
        );
        assert_eq!(
            transport.at_sample(u64::MAX).checked_block_end(0),
            Ok(u64::MAX)
        );
        assert!(transport.at_sample(u64::MAX).checked_block_end(1).is_err());
    }
}
