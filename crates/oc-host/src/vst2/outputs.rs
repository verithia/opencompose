//! Prepared capture of consecutive VST2 channel pairs.
use super::{Vst2Instance, Vst2ProcessError};

/// The plugin advances once, then appends each selected pair to its buffer.
/// Storage must be reserved by the caller before rendering.
pub struct Vst2OutputBlock<'a> {
    pub frames: usize,
    pub pairs: &'a [usize],
    pub buffers: &'a mut [Vec<f32>],
}

impl Vst2OutputBlock<'_> {
    pub(crate) fn validate(&self, pair_count: usize) -> Result<usize, Vst2ProcessError> {
        let samples = self
            .frames
            .checked_mul(2)
            .ok_or(Vst2ProcessError::CapacityExceeded)?;
        if self.pairs.is_empty()
            || self.pairs.len() != self.buffers.len()
            || self.pairs.len() > super::MAX_OUTPUT_PAIRS
            || self.pairs.iter().any(|&pair| pair >= pair_count)
        {
            return Err(Vst2ProcessError::InvalidOutput);
        }
        if self
            .buffers
            .iter()
            .any(|buffer| buffer.capacity() - buffer.len() < samples)
        {
            return Err(Vst2ProcessError::CapacityExceeded);
        }
        Ok(samples)
    }

    pub(super) fn append(&mut self, instance: &Vst2Instance) {
        for (&pair, buffer) in self.pairs.iter().zip(self.buffers.iter_mut()) {
            let start = buffer.len();
            buffer.resize(start + self.frames * 2, 0.0);
            instance.read_output_pair(pair, &mut buffer[start..]);
        }
    }
}
