//! Graph wiring for audio-derived control sources.
//!
//! The follower runs once per mix block over the source track's post-insert
//! buffer, and the destination reads those values in the same block. Nothing
//! is deferred to the next block, so a duck starts on the transient sample.

use oc_dsl::ast::ControlSource;

use crate::dsp::control_source::{ControlFollower, ControlSettings};

/// Table resolution for the dB-to-gain ride; lerp error stays under 1e-5 dB.
const GAIN_TABLE_POINTS: usize = 2048;

pub(super) struct ControlRuntime {
    pub(super) name: String,
    pub(super) source_track: usize,
    follower: ControlFollower,
    values: Vec<f32>,
}

impl ControlRuntime {
    pub(super) fn new(
        source: &ControlSource,
        source_track: usize,
        sample_rate: u32,
    ) -> Result<Self, String> {
        let formula = source
            .formula
            .as_deref()
            .map(oc_dsl::formula::Formula::parse)
            .transpose()
            .map_err(|error| format!("control source '{}': {error}", source.name))?;
        let settings = ControlSettings {
            attack_seconds: source.attack,
            release_seconds: source.release,
            lfo: source.lfo.map(|lfo| (lfo.shape, lfo.rate_hz)),
            lfo_depth: source.lfo_depth,
            formula,
            scale: source.scale,
            offset: source.offset,
            curve: source.curve,
        };
        Ok(Self {
            name: source.name.clone(),
            source_track,
            follower: ControlFollower::new(sample_rate, settings),
            values: Vec::new(),
        })
    }

    pub(super) fn prepare(&mut self, frames: usize) -> Result<(), super::PreparationError> {
        if self.values.capacity() < frames {
            self.values
                .try_reserve(frames - self.values.len())
                .map_err(|_| super::PreparationError::CapacityExceeded)?;
        }
        if self.values.len() < frames {
            self.values.resize(frames, 0.0);
        }
        Ok(())
    }

    pub(super) fn reset(&mut self) {
        self.follower.reset();
    }

    /// Advance the follower over one block; silence past the input counts as 0.
    pub(super) fn update(&mut self, source: &[f32], frames: usize) {
        if self.values.len() < frames {
            self.values.resize(frames, 0.0);
        }
        self.follower.process(source, &mut self.values[..frames]);
    }

    pub(super) fn values(&self, frames: usize) -> &[f32] {
        let end = frames.min(self.values.len());
        &self.values[..end]
    }

    pub(super) fn last_values(&self) -> &[f32] {
        &self.values
    }
}

/// One track's gain ride: a control source and a dB depth.
pub(super) struct GainRide {
    pub(super) source: usize,
    table: Vec<f32>,
}

impl GainRide {
    pub(super) fn new(source: usize, depth_db: f64) -> Self {
        let depth_db = if depth_db.is_finite() { depth_db } else { 0.0 };
        let table = (0..=GAIN_TABLE_POINTS)
            .map(|point| {
                let control = point as f64 / GAIN_TABLE_POINTS as f64;
                10.0_f64.powf(depth_db * control / 20.0) as f32
            })
            .collect();
        Self { source, table }
    }

    /// Multiply per frame by the ride's gain for the matching control value.
    pub(super) fn apply(&self, buffer: &mut [f32], control: &[f32]) {
        for (frame, value) in buffer.chunks_exact_mut(2).zip(control) {
            let position = if value.is_finite() {
                value.clamp(0.0, 1.0) * GAIN_TABLE_POINTS as f32
            } else {
                0.0
            };
            let index = position as usize;
            let gain = if index >= GAIN_TABLE_POINTS {
                self.table[GAIN_TABLE_POINTS]
            } else {
                let fraction = position - index as f32;
                self.table[index] + (self.table[index + 1] - self.table[index]) * fraction
            };
            frame[0] *= gain;
            frame[1] *= gain;
        }
    }
}

/// Compile a control source into follower settings, rejecting non-finite input.
pub(super) fn check_source(source: &ControlSource) -> Result<(), String> {
    for (field, value, non_negative) in [
        ("attack", source.attack, true),
        ("release", source.release, true),
        ("scale", source.scale, false),
        ("offset", source.offset, false),
        ("lfo_depth", source.lfo_depth, false),
    ] {
        if !value.is_finite() || (non_negative && value < 0.0) {
            return Err(format!(
                "control source '{}' {field} is not a valid number",
                source.name
            ));
        }
    }
    if let Some(lfo) = &source.lfo
        && (!lfo.rate_hz.is_finite() || lfo.rate_hz <= 0.0)
    {
        return Err(format!(
            "control source '{}' lfo rate must be a positive number",
            source.name
        ));
    }
    if let Some(formula) = &source.formula {
        oc_dsl::formula::Formula::parse(formula)
            .map_err(|error| format!("control source '{}': {error}", source.name))?;
    }
    Ok(())
}
