//! Normalized synth automation reaches every engine enum value.
//!
//! These are render-level checks: a project lane writes a normalized value,
//! the interpreter turns it into a `PluginParam` event, and the built-in synth
//! renders the same audio as a preset that names the physical enum value.
//! The filter-model pair proves the appended ladder taps are audibly distinct.

#[cfg(feature = "plugin-host")]
use std::path::Path;

#[cfg(feature = "plugin-host")]
use oc_engine::render::render_to_buffer_per_track;

#[cfg(feature = "plugin-host")]
const RATE: u32 = 48_000;
const PRESET: &str = r#"@preset Lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {filter.cutoff: 8000, filter.resonance: 0.35, filter.model: MODEL, osc.a.waveform: WAVEFORM, amp.attack: 0.005, amp.release: 0.2, output.loudness_trim_db: 0}
"#;

fn lane_name(key: &str) -> String {
    let mut encoded = String::from("pluginparam_");
    for byte in key.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

/// One 2-bar phrase with an optional lane point at bar 1, so the parameter is
/// installed before the note starts.
fn project(lane: Option<(&str, f64)>, model: u8, waveform: u8, wide: bool) -> oc_dsl::ast::Project {
    let version = if wide {
        "  automation_normalization: 2\n"
    } else {
        ""
    };
    let automation = lane
        .map(|(key, value)| format!("  automation: {} {{ 1:1:0={value} }}\n", lane_name(key)))
        .unwrap_or_default();
    let preset = PRESET
        .replace("MODEL", &model.to_string())
        .replace("WAVEFORM", &waveform.to_string());
    let text = format!(
        r#"@ocp "0.2.0"
@project "Normalization"
{version}  tempo: 120
  time_signature: 4/4

{preset}
@track Lead | instrument: synth/lead_saw, preset: Lead
{automation}  @phrase riff [2:1:0 - 3:1:0]
    C3 w mf
"#
    );
    oc_dsl::parse(&text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

#[cfg(feature = "plugin-host")]
fn render(project: &oc_dsl::ast::Project) -> Vec<f32> {
    render_to_buffer_per_track(project, Path::new("unused.sf2"), RATE).expect("render")
}

#[cfg(feature = "plugin-host")]
fn max_difference(left: &[f32], right: &[f32]) -> f32 {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(left, right)| (left - right).abs())
        .fold(0.0, f32::max)
}

#[cfg(feature = "plugin-host")]
fn rms_difference(left: &[f32], right: &[f32]) -> f64 {
    assert_eq!(left.len(), right.len());
    let sum = left
        .iter()
        .zip(right)
        .map(|(left, right)| {
            let delta = f64::from(*left) - f64::from(*right);
            delta * delta
        })
        .sum::<f64>();
    (sum / left.len() as f64).sqrt()
}

#[test]
#[cfg(feature = "plugin-host")]
fn waveform_lane_reaches_every_engine_value() {
    for waveform in 0..=6u8 {
        let physical = project(None, 0, waveform, false);
        let lane = project(
            Some(("osc.a.waveform", f64::from(waveform) / 6.0)),
            0,
            0,
            true,
        );
        let expected = render(&physical);
        let actual = render(&lane);
        assert!(
            max_difference(&expected, &actual) < 1e-7,
            "waveform {waveform} through a lane does not match the physical preset"
        );
    }
}

#[test]
#[cfg(feature = "plugin-host")]
fn filter_models_3_and_11_are_reachable_and_render_differently() {
    let control_3 = render(&project(None, 3, 1, false));
    let control_11 = render(&project(None, 11, 1, false));
    let lane_3 = render(&project(Some(("filter.model", 3.0 / 11.0)), 0, 1, true));
    let lane_11 = render(&project(Some(("filter.model", 11.0 / 11.0)), 0, 1, true));

    assert!(
        max_difference(&control_3, &lane_3) < 1e-7,
        "lane 3/11 does not select model 3"
    );
    assert!(
        max_difference(&control_11, &lane_11) < 1e-7,
        "lane 11/11 does not select model 11"
    );
    let delta = rms_difference(&lane_3, &lane_11);
    assert!(
        delta > 1e-4,
        "models 3 and 11 render the same audio (RMS delta {delta})"
    );
    println!("model 3 vs model 11 RMS delta {delta:.6}");
}

#[test]
fn lanes_emit_every_widened_enum_position() {
    for (key, max) in [
        ("osc.a.waveform", 6usize),
        ("filter.model", 11),
        ("mod.1.source", 28),
        ("mod.1.destination", 34),
    ] {
        for value in 0..=max {
            let normalized = value as f64 / max as f64;
            let project = project(Some((key, normalized)), 0, 1, true);
            let events = oc_engine::interpret::try_interpret(
                &project,
                &oc_engine::interpret::InterpretSettings::default(),
            )
            .expect("interpret");
            let emitted = events
                .iter()
                .find_map(|event| match &event.event {
                    oc_engine::interpret::MidiMessage::PluginParam { name, value }
                        if name == key =>
                    {
                        Some(*value)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} lane emitted no PluginParam"));
            assert!(
                (emitted - normalized).abs() < 1e-12,
                "{key}={value}: emitted {emitted}, expected {normalized}"
            );
        }
    }
}

#[test]
#[cfg(feature = "plugin-host")]
fn legacy_lane_keeps_the_old_table_meaning() {
    // No `automation_normalization` marker: 1.0 is the old maximum, model 2.
    let legacy = project(Some(("filter.model", 1.0)), 0, 1, false);
    let control_2 = render(&project(None, 2, 1, false));
    let wide_11 = render(&project(Some(("filter.model", 1.0)), 0, 1, true));
    let legacy_audio = render(&legacy);

    assert!(
        max_difference(&control_2, &legacy_audio) < 1e-7,
        "a pre-widening lane must still select model 2"
    );
    assert!(
        rms_difference(&legacy_audio, &wide_11) > 1e-4,
        "the same normalized value under the wide table must select model 11"
    );
}

/// Per-bar share of power above 2 kHz, in dB, from an interleaved stereo buffer.
#[cfg(feature = "plugin-host")]
fn high_band_per_bar(buffer: &[f32], sample_rate: u32, bars: usize) -> Vec<f64> {
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

    let frames = buffer.len() / 2;
    let bar_frames = frames / bars;
    let fft = FftPlanner::<f64>::new().plan_fft_forward(bar_frames);
    (0..bars)
        .map(|bar| {
            let start = bar * bar_frames;
            let mut spectrum: Vec<Complex<f64>> = (0..bar_frames)
                .map(|frame| {
                    let index = (start + frame) * 2;
                    let sample = 0.5 * (f64::from(buffer[index]) + f64::from(buffer[index + 1]));
                    let window = 0.5
                        - 0.5 * (std::f64::consts::TAU * frame as f64 / bar_frames as f64).cos();
                    Complex::new(sample * window, 0.0)
                })
                .collect();
            fft.process(&mut spectrum);
            let bin_hz = sample_rate as f64 / bar_frames as f64;
            let (mut high, mut total) = (0.0f64, 0.0f64);
            for (bin, value) in spectrum.iter().enumerate().take(bar_frames / 2 + 1) {
                let power = value.norm_sqr();
                total += power;
                if bin as f64 * bin_hz >= 2_000.0 {
                    high += power;
                }
            }
            10.0 * (high.max(1e-12) / total.max(1e-12)).log10()
        })
        .collect()
}

/// An eight-bar sustained chord with an optional cutoff lane written in Hz.
#[cfg(feature = "plugin-host")]
fn sweep_project(lane: bool) -> oc_dsl::ast::Project {
    let mut bars = String::new();
    for bar in 0..8 {
        bars.push_str(&format!(
            "    @{} G3 w mf, Bb3 w mf, D4 w mf, G4 w mf\n",
            bar * 4
        ));
    }
    let automation = if lane {
        "  automation: pluginparam_66696c7465722e6375746f6666 {1:1=200Hz, 9:1=8000Hz}\n"
    } else {
        ""
    };
    let text = format!(
        r#"@ocp "0.4.0"
@project "Sweep"
  tempo: 120
  time_signature: 4/4

@preset Lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {{filter.cutoff: 6000, filter.resonance: 0.2, osc.a.waveform: 1, osc.a.level: 0.7, amp.attack: 0.01, amp.decay: 0.4, amp.sustain: 0.8, amp.release: 0.3, output.gain: 0.5}}

@track Lead | instrument: synth/lead_square, preset: Lead
{automation}  @phrase chord [1:1 - 9:1]
{bars}
@master
"#
    );
    oc_dsl::parse(&text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

/// A cutoff lane written in hertz sweeps the built-in filter; the same project
/// without the lane holds one band share.
#[test]
#[cfg(feature = "plugin-host")]
fn unit_lane_sweeps_the_builtin_filter() {
    let control = high_band_per_bar(&render(&sweep_project(false)), RATE, 8);
    let swept = high_band_per_bar(&render(&sweep_project(true)), RATE, 8);

    let control_span = control.iter().cloned().fold(f64::MIN, f64::max)
        - control.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        control_span < 1.0,
        "the lane-free render must hold one band share: {control:?}"
    );

    let rise = swept[7] - swept[0];
    assert!(
        rise > 20.0,
        "a 200 Hz to 8 kHz lane must open the filter: {swept:?}"
    );
    assert!(
        swept[0] < control[0] - 10.0,
        "the first bar is darker than the preset cutoff: {swept:?} vs {control:?}"
    );
    for pair in swept.windows(2) {
        assert!(
            pair[1] >= pair[0] - 0.5,
            "the sweep must not close: {swept:?}"
        );
    }
    println!("control {control:?}");
    println!("swept {swept:?}");
}
