//! Audio-derived control: ducking depth, time constants and phase.
//!
//! Every measurement reads the rendered stereo output. The project-level test
//! renders a kick track that rides a bass track's gain and measures the gain
//! curve from the audio (depth in dB, one-pole time constants in seconds). The
//! graph-level tests drive the same law with synthetic buffers, so the onset
//! sample and the block split can be checked exactly.

#[cfg(feature = "plugin-host")]
use std::collections::HashMap;
#[cfg(feature = "plugin-host")]
use std::path::Path;

use oc_audio::PluginTransport;
use oc_dsl::ast::Project;
use oc_engine::graph::AudioGraph;
#[cfg(feature = "plugin-host")]
use oc_engine::render_cache::{RangeRenderRequest, RenderCache};

const RATE: u32 = 48_000;
/// 32nd note at 120 BPM: the kick's note length in the rendered project.
#[cfg(feature = "plugin-host")]
const KICK_NOTE_SECONDS: f64 = 0.0625;

#[test]
#[cfg(feature = "plugin-host")]
fn kick_duck_depth_and_release_match_the_configuration() {
    // The control is driven into its ceiling so the plateau reads the
    // configured depth exactly. The attack time constant is measured on a unit
    // step at graph level instead: a synth note's own 1 ms amplitude attack and
    // its filter envelope would otherwise be fitted as part of the follower.
    let ducked = oc_dsl::parse(&project_text(4.0)).unwrap();
    let mut reference = ducked.clone();
    reference.tracks[1].mix.control_gain = None;
    reference.tracks[0].mix.mute = true;
    let mut kick_only = ducked.clone();
    kick_only.tracks[1].mix.mute = true;

    let mixed = render(&ducked);
    let bass_only = render(&reference);
    let kick = render(&kick_only);
    assert_eq!(mixed.len(), bass_only.len());
    assert_eq!(mixed.len(), kick.len());

    let gain_db = gain_curve(&mixed, &kick, &bass_only);
    let plateau = (0.045 * RATE as f64) as usize..(0.058 * RATE as f64) as usize;
    let depth = gain_db[plateau]
        .iter()
        .fold(0.0_f64, |deepest, value| deepest.min(*value));

    let release_start = (KICK_NOTE_SECONDS * RATE as f64) as usize;
    let release_tau = fit_one_pole(&gain_db, release_start..gain_db.len(), depth, true);

    println!(
        "ducking depth {depth:.3} dB (configured -12), release {:.2} ms (120)",
        release_tau * 1000.0
    );
    // Median gain over a 4 ms window, so a sine zero crossing cannot decide a row.
    let gain_at = |millis: f64| -> f64 {
        let center = (millis * RATE as f64 / 1000.0) as usize;
        let half = (0.002 * RATE as f64) as usize;
        let start = center.saturating_sub(half);
        let end = (center + half).min(mixed.len() / 2);
        let mut values = Vec::new();
        for frame in start..end {
            for channel in 0..2 {
                let index = frame * 2 + channel;
                let reference = f64::from(bass_only[index]);
                if reference.abs() < 0.05 {
                    continue;
                }
                let ducked = f64::from(mixed[index]) - f64::from(kick[index]);
                values.push(20.0 * (ducked / reference).abs().max(1e-9).log10());
            }
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        values[values.len() / 2]
    };
    print!("gain curve (ms: dB), note 0..62.5 ms:\n  ");
    for millis in [
        0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 60.0, 70.0, 80.0, 100.0, 150.0, 200.0, 300.0, 500.0,
    ] {
        print!("{millis:.0}: {:.2}  ", gain_at(millis));
    }
    println!();

    assert!(
        (depth + 12.0).abs() < 0.1,
        "ducking depth {depth:.3} dB, expected -12 dB within 0.1 dB"
    );
    assert!(
        (release_tau - 0.12).abs() < 0.0036,
        "release time constant {:.2} ms, expected 120 ms within 3 %",
        release_tau * 1000.0
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn an_unused_control_source_leaves_the_render_untouched() {
    let mut declared = oc_dsl::parse(&project_text(1.0)).unwrap();
    declared.tracks[1].mix.control_gain = None;
    let mut plain = declared.clone();
    plain.properties.control_sources.clear();
    let (plain, declared) = (render(&plain), render(&declared));
    assert_eq!(plain.len(), declared.len());
    assert!(
        plain.iter().zip(declared.iter()).all(|(a, b)| a == b),
        "a declared but unused control source is inert"
    );
}

#[test]
fn silence_in_means_zero_control_and_unity_gain() {
    let project = graph_project();
    let graph = || AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
    let (silent_kick, bass) = synthetic_buffers(64, None);
    let mut with_control = graph();
    let mut without_control = project.clone();
    without_control.tracks[1].mix.control_gain = None;
    let mut plain =
        AudioGraph::try_from_project_with_source_latencies(&without_control, RATE, &[]).unwrap();

    let mut with_control_buffers = vec![silent_kick.clone(), bass.clone()];
    let mut plain_buffers = vec![silent_kick, bass];
    let output = with_control.mix_processed(&mut with_control_buffers, None);
    let reference = plain.mix_processed(&mut plain_buffers, None);
    assert_eq!(output, reference, "silence must leave the gain at unity");
    assert!(output.iter().all(|sample| sample.is_finite()));
    let values = with_control.control_values("kick_env").unwrap();
    assert_eq!(values.len(), 64);
    assert!(values.iter().all(|value| *value == 0.0), "{values:?}");
}

#[test]
fn the_duck_starts_on_the_transient_sample_in_any_block_split() {
    let project = graph_project();
    let mut reference = project.clone();
    reference.tracks[1].mix.control_gain = None;
    let mut kick_only = project.clone();
    kick_only.tracks[1].mix.mute = true;
    let frame_count = 4_096;
    for onset in [0_usize, 63, 64, 1_000] {
        let (kick, bass) = synthetic_buffers(frame_count, Some(onset));

        // Expected one-pole state after n samples of a unit step.
        let coefficient = 1.0 - (-1.0 / (0.005 * RATE as f64)).exp();
        let state = |samples: usize| {
            let mut value = 0.0;
            for _ in 0..samples {
                value += (1.0 - value) * coefficient;
            }
            value
        };
        let expected_gain = |samples: usize| {
            let control = state(samples) as f32;
            10.0_f64.powf(-12.0 * f64::from(control) / 20.0)
        };

        let mixed = render_blocks(&project, &kick, &bass, None);
        let plain = render_blocks(&reference, &kick, &bass, None);
        let kick_part = render_blocks(&kick_only, &kick, &bass, None);
        let gain = isolate_gain(&mixed, &plain, &kick_part);

        if onset > 0 {
            assert!(gain[onset - 1] == 1.0, "onset {onset}: pre-onset gain");
        }
        let measured = gain[onset];
        let want = expected_gain(1);
        assert!(
            (measured - want).abs() < 1e-5,
            "onset {onset}: first ducked sample {measured} vs {want}"
        );
        for offset in 1..=64 {
            let want = expected_gain(offset + 1);
            assert!(
                (gain[onset + offset] - want).abs() < 2e-5,
                "onset {onset}: sample {offset} after onset {} vs {want}",
                gain[onset + offset]
            );
        }

        // Fit the configured attack constant from this unit-step render.
        let mut curve_db = Vec::with_capacity(gain.len());
        for value in &gain {
            curve_db.push(20.0 * value.max(1e-6).log10());
        }
        let attack_tau = fit_one_pole(&curve_db, onset..onset + 2_000, -12.0, false);
        println!("onset {onset}: attack {:.3} ms", attack_tau * 1000.0);
        assert!(
            (attack_tau - 0.005).abs() < 0.0001,
            "onset {onset}: attack time constant {:.3} ms, expected 5 ms within 2 %",
            attack_tau * 1000.0
        );

        // A one-block-late duck would move the onset by the block length, so the
        // same program split into 64-frame blocks must agree sample for sample.
        let blocked = render_blocks(&project, &kick, &bass, Some(64));
        assert_eq!(blocked.len(), mixed.len(), "onset {onset}: block length");
        for (index, (a, b)) in blocked.iter().zip(mixed.iter()).enumerate() {
            assert!(
                (a - b).abs() <= f32::EPSILON.max(b.abs() * 1e-6),
                "onset {onset}: split block differs at sample {index}: {a} vs {b}"
            );
        }
    }
}

fn render_blocks(project: &Project, kick: &[f32], bass: &[f32], block: Option<usize>) -> Vec<f32> {
    let mut graph = AudioGraph::try_from_project_with_source_latencies(project, RATE, &[]).unwrap();
    let frames = kick.len() / 2;
    let mut output = Vec::with_capacity(frames * 2);
    let mut position = 0;
    while position < frames {
        let end = block.map_or(frames, |size| (position + size).min(frames));
        let mut chunk = vec![
            kick[position * 2..end * 2].to_vec(),
            bass[position * 2..end * 2].to_vec(),
        ];
        let mixed = graph.mix_with_transport(&mut chunk, Some(transport(position as u64)));
        output.extend_from_slice(&mixed);
        position = end;
    }
    output
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_bass_only_window_keeps_the_full_mix_duck() {
    // A subset window must be the full mix with the other tracks zeroed: the
    // kick still feeds the follower even though the caller selected the bass.
    let project = oc_dsl::parse(&project_text(4.0)).unwrap();
    let mut muted = project.clone();
    muted.tracks[1].mix.mute = true;
    let path = Path::new("/nonexistent/soundfont.sf2");
    let full = render_with_cache(&project, path);
    let without_bass = render_with_cache(&muted, path);
    assert_eq!(full.len(), without_bass.len());

    let frames = full.len() / 2;
    let start = frames / 4;
    let end = start + frames / 8;
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(3.0).unwrap();
    let mut plugins = HashMap::new();
    cache
        .render_full(&project, path, RATE, &mut plugins)
        .unwrap();
    let window = cache
        .render_range(
            &project,
            path,
            RATE,
            &mut plugins,
            RangeRenderRequest {
                start_sample: start as u64,
                end_sample: end as u64,
                tracks: Some(vec![1]),
                stems: false,
            },
        )
        .unwrap();
    assert_eq!(window.audio.len(), (end - start) * 2);

    let mut worst = 0.0_f32;
    for frame in 0..(end - start) {
        for channel in 0..2 {
            let index = (start + frame) * 2 + channel;
            let expected = full[index] - without_bass[index];
            worst = worst.max((window.audio[frame * 2 + channel] - expected).abs());
        }
    }
    assert!(
        worst < 1e-5,
        "bass-only window differs from the full mix by {worst}"
    );
}

#[cfg(feature = "plugin-host")]
fn render_with_cache(project: &Project, soundfont: &Path) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(3.0).unwrap();
    let mut plugins = HashMap::new();
    cache
        .render_full(project, soundfont, RATE, &mut plugins)
        .expect("render")
}

fn transport(sample_pos: u64) -> PluginTransport {
    PluginTransport::new(sample_pos, RATE, 120.0)
}

fn graph_project() -> Project {
    oc_dsl::parse(
        "@project\n\n@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms\n\n@track kick | instrument: drums/kick\n\n@track bass | instrument: synth/bass\n  mix: control(kick_env=-12dB)\n",
    )
    .unwrap()
}

/// A unit step on the kick track and a constant bass, both mono.
fn synthetic_buffers(frames: usize, onset: Option<usize>) -> (Vec<f32>, Vec<f32>) {
    let mut kick = vec![0.0; frames * 2];
    if let Some(onset) = onset {
        for frame in onset..frames {
            kick[frame * 2] = 1.0;
            kick[frame * 2 + 1] = 1.0;
        }
    }
    let mut bass = vec![0.0; frames * 2];
    for frame in 0..frames {
        bass[frame * 2] = 0.5;
        bass[frame * 2 + 1] = 0.5;
    }
    (kick, bass)
}

/// The ride's gain per sample: the ducked bass divided by the un-ridden bass.
///
/// The kick component is identical in all three renders, so it cancels out of
/// the subtraction and the fader/pan law cancels out of the division.
fn isolate_gain(mixed: &[f32], plain: &[f32], kick_part: &[f32]) -> Vec<f64> {
    let mut gain = Vec::with_capacity(mixed.len() / 2);
    for frame in 0..mixed.len() / 2 {
        let index = frame * 2;
        let kick_sample = f64::from(kick_part[index]);
        let reference = f64::from(plain[index]) - kick_sample;
        if reference.abs() < 1e-6 {
            gain.push(1.0);
            continue;
        }
        gain.push((f64::from(mixed[index]) - kick_sample) / reference);
    }
    gain
}

/// Gain in dB per sample: (mix - kick) / bass reference.
#[cfg(feature = "plugin-host")]
fn gain_curve(mixed: &[f32], kick: &[f32], bass: &[f32]) -> Vec<f64> {
    let mut curve = Vec::with_capacity(mixed.len() / 2);
    for frame in 0..mixed.len() / 2 {
        let index = frame * 2;
        let bass_sample = f64::from(bass[index]);
        if bass_sample.abs() < 0.05 {
            curve.push(0.0);
            continue;
        }
        let ducked = f64::from(mixed[index]) - f64::from(kick[index]);
        curve.push(20.0 * (ducked / bass_sample).abs().max(1e-6).log10());
    }
    curve
}

/// Least-squares fit of one exponential time constant from the measured curve.
///
/// `ln(1 - |gain| / |depth|)` is linear in time while the duck is still rising;
/// `ln(|gain| / |depth|)` is linear while it falls back to unity.
fn fit_one_pole(gain_db: &[f64], range: std::ops::Range<usize>, depth: f64, falling: bool) -> f64 {
    let depth = depth.abs();
    let (mut sum_t, mut sum_y, mut sum_tt, mut sum_ty, mut count) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (index, value) in gain_db[range].iter().enumerate() {
        let level = value.abs() / depth;
        let y = if falling {
            level.ln()
        } else {
            (1.0 - level).ln()
        };
        // Stay inside the middle of the exponential: the source note's own
        // short attack and release perturb both ends.
        if !y.is_finite() || !(0.2..=0.8).contains(&level) {
            continue;
        }
        let t = index as f64 / RATE as f64;
        sum_t += t;
        sum_y += y;
        sum_tt += t * t;
        sum_ty += t * y;
        count += 1.0;
    }
    assert!(count > 200.0, "fit window has {count} usable samples");
    let slope = (count * sum_ty - sum_t * sum_y) / (count * sum_tt - sum_t * sum_t);
    assert!(slope < 0.0, "expected a decaying fit, slope {slope}");
    -1.0 / slope
}

/// The kick drives the bass; `scale` calibrates the control against the kick level.
#[cfg(feature = "plugin-host")]
fn project_text(scale: f64) -> String {
    format!(
        "@project \"duck\"\n  tempo: 120\n  time_signature: 4/4\n\n@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms, lfo: none, formula: none, scale: {:.6}\n\n@preset kick_tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{osc.a.waveform: 0, osc.a.level: 1, amp.attack: 0.001s, amp.decay: 0.01s, amp.sustain: 1, amp.release: 0.005s, voice.polyphony: 1, filter.cutoff: 6000Hz, filter.env.amount: 0, output.gain: 1, output.loudness_trim_db: 0}}\n\n@preset bass_tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.5}}\n\n@track kick | preset: kick_tone, instrument: drums/kick\n  @phrase kick_hit [1:1:0 - 2:1:0]\n    C2 32nd fff\n\n@track bass | preset: bass_tone, instrument: synth/bass\n  @phrase bass_line [1:1:0 - 5:1:0]\n    C3 w mp\n  mix: control(kick_env=-12dB)\n",
        scale
    )
}

#[cfg(feature = "plugin-host")]
fn render(project: &Project) -> Vec<f32> {
    oc_dsl::validate(project).expect("valid control project");
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(3.0).unwrap();
    let mut plugins = HashMap::new();
    cache
        .render_full(
            project,
            Path::new("/nonexistent/soundfont.sf2"),
            RATE,
            &mut plugins,
        )
        .expect("render")
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_zero_depth_ride_is_bit_identical_to_no_ride() {
    // Same project, a ride that reaches 0 dB: the source track keeps its level.
    let mut project = oc_dsl::parse(&project_text(1.0)).unwrap();
    project.tracks[1].mix.control_gain = Some(oc_dsl::ast::ControlGain {
        source: "kick_env".into(),
        depth_db: 0.0,
    });
    let flat = render(&project);
    let mut no_ride = project.clone();
    no_ride.tracks[1].mix.control_gain = None;
    assert_eq!(flat, render(&no_ride));
}
