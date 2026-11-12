//! Dense OpenSynth performance smoke tests.
//!
//! Part of the `perf-tests` set, so a plain `cargo test` neither compiles nor
//! runs them. Run with `cargo test -p oc-engine --features perf-tests --profile perf`;
//! `SYNTH_PERF_SECONDS` extends the run.
#![cfg(feature = "perf-tests")]

use std::time::{Duration, Instant};

use oc_synth::synth::{
    FilterModel, LfoSettings, LfoShape, ModDestination, ModRoute, ModSource, OpenSynth, SynthFx,
    SynthFxType, SynthPatch, WarpMode, Waveform,
};

/// The benchmark patch. `warp` selects the three oscillator warp modes so the
/// warp stage can be ablated without changing anything else.
fn dense_patch(warp: (WarpMode, WarpMode, WarpMode)) -> SynthPatch {
    SynthPatch {
        oscillator_a_level: 0.4,
        oscillator_b_level: 0.3,
        oscillator_c_level: 0.3,
        sub_level: 0.1,
        noise_level: 0.02,
        unison_voices: 4,
        extra_lfos: std::array::from_fn(|_| LfoSettings {
            rate: 2.0,
            shape: std::sync::Arc::new(LfoShape::sine()),
            sync: false,
            retrigger: false,
            division: 2,
            ..LfoSettings::default()
        }),
        mod_routes: vec![
            ModRoute {
                source: ModSource::Lfo,
                destination: ModDestination::FilterCutoff,
                amount: 0.1,
                ..ModRoute::default()
            };
            64
        ],
        oscillator_a_warp_mode: warp.0,
        oscillator_a_warp_amount: 0.7,
        oscillator_b_warp_mode: warp.1,
        oscillator_b_warp_amount: 0.7,
        oscillator_c_warp_mode: warp.2,
        oscillator_c_warp_amount: 0.7,
        fx: vec![
            SynthFx::new(SynthFxType::Drive),
            SynthFx::new(SynthFxType::Compressor),
            SynthFx::new(SynthFxType::Eq3),
            SynthFx::new(SynthFxType::Delay),
            SynthFx::new(SynthFxType::Reverb),
            SynthFx::new(SynthFxType::Chorus),
        ],
        ..Default::default()
    }
}

#[test]
fn dense_patch_render_throughput() {
    let mut synth = OpenSynth::new(dense_patch((
        WarpMode::BendPos,
        WarpMode::AsymPos,
        WarpMode::RmOsc,
    )));
    for note in 48..64 {
        synth.note_on(0, note, 100);
    }

    let sample_rate = 48_000;
    // `SYNTH_PERF_SECONDS` keeps the smoke test short by default while letting
    // profilers attach to a multi-second run.
    let seconds = std::env::var("SYNTH_PERF_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    let total = sample_rate * seconds;
    let start = Instant::now();
    let mut peak = 0.0f32;
    for _ in 0..total {
        let [left, right] = synth.next_stereo(sample_rate);
        peak = peak.max(left.abs()).max(right.abs());
    }
    let elapsed = start.elapsed();
    let realtime_factor = elapsed.as_secs_f64() / seconds as f64;
    println!(
        "dense patch: {total} samples in {elapsed:?} ({realtime_factor:.3}x realtime), peak {peak:.3}"
    );
    assert!(peak.is_finite());
    assert!(
        elapsed < Duration::from_secs((seconds as u64) * 2),
        "dense patch rendered slower than 0.5x realtime: {elapsed:?}"
    );
}

#[test]
fn dense_wavetable_patch_render_throughput() {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_b_waveform: Waveform::Wavetable,
        oscillator_c_waveform: Waveform::Wavetable,
        oscillator_a_level: 0.4,
        oscillator_b_level: 0.3,
        oscillator_c_level: 0.3,
        sub_level: 0.1,
        noise_level: 0.02,
        unison_voices: 4,
        extra_lfos: std::array::from_fn(|_| LfoSettings {
            rate: 2.0,
            shape: std::sync::Arc::new(LfoShape::sine()),
            sync: false,
            retrigger: false,
            division: 2,
            ..LfoSettings::default()
        }),
        mod_routes: vec![
            ModRoute {
                source: ModSource::Lfo,
                destination: ModDestination::FilterCutoff,
                amount: 0.1,
                ..ModRoute::default()
            };
            64
        ],
        oscillator_a_warp_mode: WarpMode::BendPos,
        oscillator_a_warp_amount: 0.7,
        oscillator_b_warp_mode: WarpMode::AsymPos,
        oscillator_b_warp_amount: 0.7,
        oscillator_c_warp_mode: WarpMode::RmOsc,
        oscillator_c_warp_amount: 0.7,
        fx: vec![
            SynthFx::new(SynthFxType::Drive),
            SynthFx::new(SynthFxType::Compressor),
            SynthFx::new(SynthFxType::Eq3),
            SynthFx::new(SynthFxType::Delay),
            SynthFx::new(SynthFxType::Reverb),
            SynthFx::new(SynthFxType::Chorus),
        ],
        ..Default::default()
    };

    let mut synth = OpenSynth::new(patch);
    for note in 48..64 {
        synth.note_on(0, note, 100);
    }
    let sample_rate = 48_000;
    let seconds = 5;
    let total = sample_rate * seconds;
    let start = Instant::now();
    let mut peak = 0.0f32;
    for _ in 0..total {
        let [left, right] = synth.next_stereo(sample_rate);
        peak = peak.max(left.abs()).max(right.abs());
    }
    let elapsed = start.elapsed();
    let realtime_factor = elapsed.as_secs_f64() / seconds as f64;
    println!(
        "dense wavetable patch: {total} samples in {elapsed:?} ({realtime_factor:.3}x realtime), peak {peak:.3}"
    );
    assert!(peak.is_finite());
    assert!(elapsed < Duration::from_secs((seconds as u64) * 2));
}

fn simple_wavetable_patch(unison_voices: usize) -> SynthPatch {
    SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        unison_voices,
        ..Default::default()
    }
}

fn measure(label: &str, mut synth: OpenSynth, notes: std::ops::Range<u8>) {
    for note in notes {
        synth.note_on(0, note, 100);
    }
    let sample_rate = 48_000;
    let seconds = std::env::var("SYNTH_PERF_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    let total = sample_rate * seconds;
    let start = Instant::now();
    let mut peak = 0.0f32;
    for _ in 0..total {
        let [left, right] = synth.next_stereo(sample_rate);
        peak = peak.max(left.abs()).max(right.abs());
    }
    let elapsed = start.elapsed();
    let realtime_factor = elapsed.as_secs_f64() / seconds as f64;
    println!(
        "{label}: {total} samples in {elapsed:?} ({realtime_factor:.3}x realtime), peak {peak:.3}"
    );
    assert!(peak.is_finite());
    assert!(elapsed < Duration::from_secs((seconds as u64) * 2));
}

#[test]
fn simple_wavetable_16_notes_render_throughput() {
    measure(
        "simple wavetable 16 notes",
        OpenSynth::new(simple_wavetable_patch(1)),
        48..64,
    );
}

#[test]
fn simple_wavetable_8_notes_4_unison_render_throughput() {
    measure(
        "simple wavetable 8 notes 4 unison",
        OpenSynth::new(simple_wavetable_patch(4)),
        48..56,
    );
}

#[test]
fn filter_model_render_throughput() {
    for (label, model) in [
        ("svf", FilterModel::Svf),
        ("ladder", FilterModel::Ladder),
        ("dirty ladder", FilterModel::DirtyLadder),
    ] {
        let patch = SynthPatch {
            filter_model: model,
            filter2_model: model,
            filter2_enabled: true,
            filter_cutoff: 1_200.0,
            filter_resonance: 0.7,
            filter2_cutoff: 3_000.0,
            filter2_resonance: 0.7,
            ..Default::default()
        };
        measure(label, OpenSynth::new(patch), 48..64);
    }
}

/// Warp-stage ablation: `WARP_ABLATE` picks which oscillator keeps its warp
/// mode (`off`, `a`, `b`, `c`, `all`), everything else is the dense patch.
#[test]
fn warp_mode_attribution() {
    let variant = std::env::var("WARP_ABLATE").unwrap_or_else(|_| "all".to_string());
    let modes = match variant.as_str() {
        "off" => (WarpMode::Off, WarpMode::Off, WarpMode::Off),
        "a" => (WarpMode::BendPos, WarpMode::Off, WarpMode::Off),
        "b" => (WarpMode::Off, WarpMode::AsymPos, WarpMode::Off),
        "c" => (WarpMode::Off, WarpMode::Off, WarpMode::RmOsc),
        "all" => (WarpMode::BendPos, WarpMode::AsymPos, WarpMode::RmOsc),
        other => panic!("unknown WARP_ABLATE={other}"),
    };
    measure(
        &format!("warp ablation {variant}"),
        OpenSynth::new(dense_patch(modes)),
        48..64,
    );
}
