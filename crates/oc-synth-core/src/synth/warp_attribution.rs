//! Per-piece cost probes: the warp stages and the wavetable readout. Test
//! builds only; not on the audio path.
//!
//! Run with `cargo test --release -p oc-synth-core -- --ignored --nocapture`.

use super::*;
use std::hint::black_box;

fn bench(label: &str, volume: u64, mut f: impl FnMut(f64) -> f64) {
    let mut sink = 0.0;
    for index in 0..4096u64 {
        sink += f(black_box((index % 4800) as f64 / 4800.0));
    }
    let start = std::time::Instant::now();
    for index in 0..volume {
        let phase = black_box((index % 4800) as f64 / 4800.0);
        sink += f(phase);
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{label:40} {:7.2} ns/call   (sink {sink:e})",
        elapsed * 1e9 / volume as f64
    );
}

/// Per-piece cost of the warped readout: the phase map's parts, the readout,
/// and the whole stage for each of the three modes the dense patch uses.
#[test]
#[ignore = "performance probe; run with --ignored --nocapture"]
fn warp_stage_attribution_probe() {
    let volume = 20_000_000u64;
    let (bend, sym) = (0.22, 0.78);
    let step = 0.0041;
    let increment = 0.0082;
    let mut mip = MipState::default();

    bench("rem_euclid(1.0)", volume, |phase| phase.rem_euclid(1.0));
    bench("guarded wrap in [0,1)", volume, |phase| {
        if (0.0..1.0).contains(&phase) {
            phase
        } else {
            phase.rem_euclid(1.0)
        }
    });
    bench("phase_warp BendPos", volume, |phase| {
        let (warped, slope) = warp_mod::phase_warp(WarpMode::BendPos, bend, phase);
        warped + slope
    });
    bench("phase_warp AsymPos", volume, |phase| {
        let (warped, slope) = warp_mod::phase_warp(WarpMode::AsymPos, sym, phase);
        warped + slope
    });
    bench("bend denominator", volume, |phase| {
        if phase < 0.5 {
            1.0 - bend + phase * (4.0 * bend - 2.0)
        } else {
            (2.0 * bend - 1.0) * (2.0 * phase - 1.0) - bend
        }
    });
    bench("bend map without frac", volume, |phase| {
        if phase < 0.5 {
            phase * bend / (1.0 - bend + phase * (4.0 * bend - 2.0))
        } else {
            let p = 2.0 * phase - 1.0;
            0.5 * ((3.0 * bend - 2.0) * p - bend) / ((2.0 * bend - 1.0) * p - bend)
        }
    });
    bench("bend map with frac", volume, |phase| {
        if phase < 0.5 {
            (phase * bend / (1.0 - bend + phase * (4.0 * bend - 2.0))).rem_euclid(1.0)
        } else {
            let p = 2.0 * phase - 1.0;
            (0.5 * ((3.0 * bend - 2.0) * p - bend) / ((2.0 * bend - 1.0) * p - bend))
                .rem_euclid(1.0)
        }
    });
    bench("bend slope division", volume, |phase| {
        let denominator = if phase < 0.5 {
            1.0 - bend + phase * (4.0 * bend - 2.0)
        } else {
            (2.0 * bend - 1.0) * (2.0 * phase - 1.0) - bend
        };
        bend * (1.0 - bend) / (denominator * denominator)
    });
    bench("saw readout (bypass)", volume, |phase| {
        oscillator_sample_with_pulse_width(
            Waveform::Saw,
            phase,
            step,
            increment,
            0.5,
            0.0,
            None,
            &mut mip,
            0.0,
        )
    });

    bench("read_warped (bypass args)", volume, |phase| {
        read_warped(
            Waveform::Saw,
            phase,
            step,
            increment,
            0.5,
            0.0,
            None,
            &mut mip,
            IMPORTED_READOUT_DELAY * increment,
        )
    });
    bench("stage Off with explicit warp slot", volume, |phase| {
        let warp = WarpParams::new(WarpMode::Off, 0.0, 0.0, WarpMode::Off, 0.0, 0.0);
        oscillator_sample_with_warp(
            Waveform::Saw,
            phase,
            step,
            increment,
            0.5,
            0.0,
            None,
            warp,
            0.0,
            &mut mip,
        )
    });

    for (label, mode, knob) in [
        ("Off", WarpMode::Off, 0.0),
        ("BendPos", WarpMode::BendPos, 0.7),
        ("AsymPos", WarpMode::AsymPos, 0.7),
        ("RmOsc", WarpMode::RmOsc, 0.7),
    ] {
        let warp = WarpParams::new(mode, knob, 0.0, WarpMode::Off, 0.0, 0.0);
        bench(&format!("stage {label}: reference"), volume, |phase| {
            warp_laws::stage(
                Waveform::Saw,
                phase,
                step,
                increment,
                0.5,
                0.0,
                None,
                warp,
                0.0,
                &mut mip,
            )
        });
        bench(&format!("stage {label}: current"), volume, |phase| {
            oscillator_sample_with_warp(
                Waveform::Saw,
                phase,
                step,
                increment,
                0.5,
                0.0,
                None,
                warp,
                0.0,
                &mut mip,
            )
        });
    }

    let warp = WarpParams::new(WarpMode::BendPos, 0.7, 0.0, WarpMode::Off, 0.0, 0.0);
    bench("sync_factor, no sync", volume, |phase| {
        phase + warp.sync_factor()
    });
    bench("advance_sync_window, no sync", volume, |phase| {
        let mut window = phase;
        let mut oscillator_phase = phase;
        advance_sync_window(warp, increment, &mut window, &mut oscillator_phase).0
            + oscillator_phase
    });
}

/// Per-read cost of the built-in bank against the named-table kernel. One read
/// is one position frame; a sample takes two unless the position is frame-aligned.
#[test]
#[ignore = "performance probe; run with --ignored --nocapture"]
fn readout_cost_probe() {
    let volume = 20_000_000u64;
    let block: Vec<f64> = default_shapes_table().samples[..256].to_vec();
    bench("named-table read, 16 taps", volume, |phase| {
        read_polyphase(&block, block.len(), phase)
    });
    let second: Vec<f64> = default_shapes_table().samples[256..512].to_vec();
    for (tier, &taps) in READOUT_TAPS.iter().enumerate() {
        let table = &readout_tables()[tier][4];
        bench(&format!("built-in read, {taps:2} taps"), volume, |phase| {
            table.read(&block, block.len(), phase)
        });
        // The audio path reads the next position frame too, which shares the
        // blended coefficient row.
        bench(
            &format!("built-in read, {taps:2} taps, two frames"),
            volume,
            |phase| table.read_frames(&block, Some(&second), block.len(), phase, 0.37),
        );
    }
}
