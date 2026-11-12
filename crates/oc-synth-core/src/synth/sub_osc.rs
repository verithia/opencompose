//! Sub-oscillator shapes for the preset importer.
//!
//! Six 2048-point tables are built from the shape functions, each scaled by
//! `-1/3 * trim`. Round rect and triangle are precomputed tables; the other
//! four are formulas.

use super::{MIP_CROSSFADE_SAMPLES, MIP_LEVEL_LIMIT, MipChain};
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;
use std::sync::OnceLock;

pub(super) const SUB_TABLE_SIZE: usize = 2048;

/// The imported `SubOsc.kParamShape` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum SubShape {
    #[default]
    #[serde(alias = "Wavetable")]
    Sine = 0,
    RoundRect = 1,
    Triangle = 2,
    Saw = 3,
    Square = 4,
    Pulse = 5,
}

impl SubShape {
    /// Sub-oscillator shape at a selector index.
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::RoundRect,
            2 => Self::Triangle,
            3 => Self::Saw,
            4 => Self::Square,
            5 => Self::Pulse,
            _ => Self::Sine,
        }
    }

    /// Selector value rounded and clamped to a known shape.
    pub fn from_value(value: f64) -> Self {
        Self::from_index(value.round().clamp(0.0, 5.0) as usize)
    }
}

const TRIMS: [f64; 6] = [
    1.972_423,
    2.660_725,
    2.238_721,
    2.660_725,
    1.341_247_7,
    1.470_648_9,
];

fn raw_sample(shape: SubShape, index: usize) -> f64 {
    match shape {
        SubShape::Sine => (TAU * index as f64 / SUB_TABLE_SIZE as f64).sin(),
        SubShape::RoundRect => super::sub_shapes::ROUND_RECT[index] as f64,
        SubShape::Triangle => super::sub_shapes::TRIANGLE[index] as f64,
        SubShape::Saw => (index as f64 * 0.000_498_533_2).min(1.0) * -1.66 + 0.924_56,
        SubShape::Square => {
            if index <= 0x400 {
                1.0
            } else {
                -1.0
            }
        }
        SubShape::Pulse => {
            if index <= 0x229 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

fn build_table(shape: SubShape) -> Vec<f64> {
    let trim = -1.0 / 3.0 * TRIMS[shape as usize];
    (0..SUB_TABLE_SIZE)
        .map(|index| raw_sample(shape, index) * trim)
        .collect()
}

pub(super) struct SubTables {
    mips: [MipChain; 6],
}

impl SubTables {
    fn new() -> Self {
        Self {
            mips: std::array::from_fn(|index| {
                MipChain::single_table(build_table(SubShape::from_index(index)))
            }),
        }
    }
}

static TABLES: OnceLock<SubTables> = OnceLock::new();

pub(super) fn tables() -> &'static SubTables {
    TABLES.get_or_init(SubTables::new)
}

/// Build the shared shape pyramids and readout kernel before playback starts.
pub(super) fn prepare() {
    tables();
    super::polyphase_kernel();
}

/// Mip selection from the phase increment: the biased f32 exponent, plus one
/// when a mantissa is present.
pub(super) fn mip_index(increment: f64) -> usize {
    let bits = (increment as f32).to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 0x7f + i32::from(bits & 0x7f_ffff != 0);
    (11 + exponent).clamp(0, MIP_LEVEL_LIMIT as i32 - 1) as usize
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SubMipState {
    level: usize,
    previous: usize,
    blend: u8,
    initialized: bool,
}

fn read_level(tables: &SubTables, shape: SubShape, level: usize, phase: f64) -> f64 {
    let levels = &tables.mips[shape as usize].levels;
    let level = level.min(levels.len() - 1);
    super::read_polyphase(&levels[level], SUB_TABLE_SIZE >> level, phase)
}

/// Read one sub sample with the mip law above and the 64-sample crossfade.
pub(super) fn sub_sample(
    shape: SubShape,
    state: &mut SubMipState,
    phase: f64,
    increment: f64,
) -> f64 {
    let tables = tables();
    let target = mip_index(increment);
    if !state.initialized {
        state.initialized = true;
        state.level = target;
        state.previous = target;
        return read_level(tables, shape, target, phase);
    }
    if target != state.level {
        state.previous = state.level;
        state.level = target;
        state.blend = MIP_CROSSFADE_SAMPLES;
    }
    let current = read_level(tables, shape, state.level, phase);
    if state.blend == 0 {
        return current;
    }
    let old = read_level(tables, shape, state.previous, phase);
    let weight = f64::from(state.blend) / f64::from(MIP_CROSSFADE_SAMPLES);
    state.blend -= 1;
    current + (old - current) * weight
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(shape: SubShape) -> Vec<f64> {
        tables().mips[shape as usize].levels[0].clone()
    }

    #[test]
    fn shape_tables_have_the_expected_amplitudes() {
        // Absolute peaks of the six generated tables.
        for (shape, peak) in [
            (SubShape::Sine, 0.657_474),
            (SubShape::RoundRect, 0.658_437),
            (SubShape::Triangle, 0.820_864),
            (SubShape::Saw, 0.820_000),
            (SubShape::Square, 0.447_083),
            (SubShape::Pulse, 0.490_216),
        ] {
            let measured = table(shape)
                .iter()
                .fold(0.0_f64, |peak, value| peak.max(value.abs()));
            assert!(
                (measured - peak).abs() < 1e-5,
                "{shape:?}: table peak {measured}, expected {peak}"
            );
        }
    }

    #[test]
    fn pulse_and_square_have_the_expected_duty_cycles() {
        // The trim is negative, so the raw +1 plateau is the negative half.
        let negative = |shape| table(shape).iter().filter(|&&value| value < 0.0).count();
        assert_eq!(negative(SubShape::Square), 0x401);
        assert_eq!(negative(SubShape::Pulse), 0x22a);
        assert!((negative(SubShape::Pulse) as f64 / SUB_TABLE_SIZE as f64 - 0.2705).abs() < 1e-3);
    }

    #[test]
    fn saw_and_pulse_discontinuities_sit_on_phase_zero() {
        for shape in [SubShape::Saw, SubShape::Pulse] {
            let values = table(shape);
            assert!(values[0] < 0.0 && values[SUB_TABLE_SIZE - 1] > 0.0);
            let jump = (values[SUB_TABLE_SIZE - 1] - values[0]).abs();
            assert!(jump > 0.9, "{shape:?} jump {jump}");
        }
        // The continuous shapes start at phase zero.
        assert!(table(SubShape::Sine)[0].abs() < 1e-6);
        assert!(table(SubShape::RoundRect)[0].abs() < 1e-3);
    }

    #[test]
    fn mip_selection_follows_the_exponent_law() {
        let level = |hz: f64| mip_index(hz / 48_000.0);
        // C1..C7: 1024, 256, 256, 128, 64, 32 and 16 points per cycle.
        for (hz, expected) in [
            (32.703, 1),
            (65.406, 2),
            (130.8128, 3),
            (261.6256, 4),
            (523.251, 5),
            (1046.502, 6),
            (2093.005, 7),
        ] {
            assert_eq!(level(hz), expected, "{hz} Hz");
        }
        // A power-of-two increment has no mantissa, so the +1 drops out.
        assert_eq!(mip_index(1.0 / 2048.0), 0);
        assert_eq!(mip_index(1.0 / 512.0), 2);
        assert_eq!(mip_index(0.0), 0);
        assert_eq!(mip_index(2.0), 11);
    }

    #[test]
    fn mip_change_crossfades_over_64_samples() {
        let shape = SubShape::Sine;
        let phase = 0.3;
        let low = 130.8128 / 48_000.0;
        let high = 1046.5 / 48_000.0;
        let mut state = SubMipState::default();
        let first = sub_sample(shape, &mut state, phase, low);
        assert_eq!(state.level, mip_index(low));
        assert!((first - read_level(tables(), shape, state.level, phase)).abs() < 1e-12);

        let switched = sub_sample(shape, &mut state, phase, high);
        let old = read_level(tables(), shape, mip_index(low), phase);
        assert!((switched - old).abs() < 1e-12);
        assert_eq!(state.blend, MIP_CROSSFADE_SAMPLES - 1);

        while state.blend > 0 {
            sub_sample(shape, &mut state, phase, high);
        }
        let last = sub_sample(shape, &mut state, phase, high);
        let target = read_level(tables(), shape, mip_index(high), phase);
        assert!((last - target).abs() < 1e-12);
    }

    #[test]
    fn shape_ids_follow_the_imported_enum() {
        for index in 0..6 {
            assert_eq!(SubShape::from_index(index) as usize, index);
            assert_eq!(SubShape::from_value(index as f64) as usize, index);
        }
        assert_eq!(SubShape::from_value(9.0), SubShape::Pulse);
    }

    #[test]
    fn synth_construction_builds_the_shared_tables() {
        let _synth = super::super::OpenSynth::new(super::super::SynthPatch {
            sub_level: 0.5,
            ..Default::default()
        });
        assert!(
            TABLES.get().is_some(),
            "constructor must build the sub mips"
        );
    }
}
