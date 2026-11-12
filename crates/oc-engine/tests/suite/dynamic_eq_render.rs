//! End-to-end render check for a four-bar project using the dynamic EQ.

use std::path::Path;

use oc_engine::render::render_to_buffer_per_track;

const PROJECT: &str = r#"@ocp "0.2.0"

@project "Dynamic EQ four bars"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.35, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  mix: vol=-3dB
  @phrase four_bars [1:1:0 - 5:1:0]
    C4 w mf
    C4 w mf
    C4 w mf
    C4 w mf

@master
  effects:
    - dynamic_eq: bypass=false, band1_enabled=true, band1_type=bell, band1_freq=80Hz, band1_q=1, band1_gain=+2dB, band1_dynamic=true, band1_threshold=-30dB, band1_range=-6dB, band1_attack=10ms, band1_release=150ms, band2_enabled=true, band2_type=bell, band2_freq=1kHz, band2_q=1, band2_gain=0dB, band2_dynamic=true, band2_threshold=-30dB, band2_range=-9dB, band2_attack=3ms, band2_release=100ms, band3_enabled=true, band3_type=high_shelf, band3_freq=8kHz, band3_q=0.707, band3_gain=-3dB, band4_enabled=true, band4_type=notch, band4_freq=60Hz, band4_q=4, band4_gain=0dB, band5_enabled=true, band5_type=low_shelf, band5_freq=200Hz, band5_q=0.707, band5_gain=+1dB, band6_enabled=true, band6_type=bell, band6_freq=4kHz, band6_q=1, band6_gain=-2dB, low_cut_enabled=true, low_cut_freq=30Hz, low_cut_q=0.707, high_cut_enabled=true, high_cut_freq=18kHz, high_cut_q=0.707
"#;

#[test]
fn four_bar_dynamic_eq_project_renders_finite_and_unclipped() {
    let project = oc_dsl::parse(PROJECT).expect("project should parse");
    oc_dsl::validate(&project).expect("project should validate");

    let audio = render_to_buffer_per_track(&project, Path::new("unused.sf2"), 48_000)
        .expect("render should succeed");
    assert!(
        audio.len() >= 8 * 48_000 * 2,
        "expected four bars of stereo audio, got {} samples",
        audio.len()
    );
    assert!(audio.iter().all(|sample| sample.is_finite()));
    assert!(audio.iter().any(|sample| sample.abs() > 0.001));
    let peak = audio
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak <= 1.0, "project clipped at {peak}");

    let mut dry = project.clone();
    dry.master.as_mut().unwrap().effects.clear();
    let reference = render_to_buffer_per_track(&dry, Path::new("unused.sf2"), 48_000)
        .expect("dry render should succeed");
    assert!(
        audio
            .iter()
            .zip(&reference)
            .any(|(wet, dry)| (wet - dry).abs() > 0.001),
        "dynamic EQ did not change the mix"
    );
}
