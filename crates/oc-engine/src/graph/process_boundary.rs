//! First-failure admission for chain processing, before audio or ramps change.

use super::*;
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why an effect chain refused a block.
pub enum EffectChainProcessError {
    IncompleteStereoFrame,
    TransportRange(oc_audio::TransportRangeError),
}

impl std::fmt::Display for EffectChainProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteStereoFrame => {
                formatter.write_str("The effect chain requires complete stereo frames")
            }
            Self::TransportRange(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}
impl std::error::Error for EffectChainProcessError {}

#[derive(Default)]
pub(super) struct ProcessBoundary {
    failure: Cell<Option<EffectChainProcessError>>,
    reported: Cell<bool>,
}

impl ProcessBoundary {
    pub(super) fn failure(&self) -> Option<EffectChainProcessError> {
        self.failure.get()
    }
    pub(super) fn clear(&self) {
        self.failure.set(None);
        self.reported.set(false);
    }
    pub(super) fn admit(
        &self,
        samples: usize,
        transport: Option<&PluginTransport>,
    ) -> Result<(), EffectChainProcessError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let result = if !samples.is_multiple_of(2) {
            Err(EffectChainProcessError::IncompleteStereoFrame)
        } else {
            transport.map_or(Ok(()), |transport| {
                transport
                    .checked_block_end(samples / 2)
                    .map(|_| ())
                    .map_err(EffectChainProcessError::TransportRange)
            })
        };
        if let Err(error) = result {
            self.failure.set(Some(error));
        }
        result
    }
    pub(super) fn report(&self) -> oc_audio::ProcessingFailureReport {
        match self.failure() {
            None => oc_audio::ProcessingFailureReport::Unavailable,
            Some(_) if self.reported.replace(true) => oc_audio::ProcessingFailureReport::Reported,
            Some(error) => oc_audio::ProcessingFailureReport::Ready(error.to_string()),
        }
    }
}
