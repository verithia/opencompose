//! Monitoring controls: the mono check and the clip reset.
//!
//! The mono fold is transport monitoring, so the export path must be blind to
//! it; the first test here pins that with the production export command.

use std::path::PathBuf;

use crate::state::AppState;

const PROJECT: &str = r#"@ocp "0.2.0"

@project "monitor test"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 2:1]
    C4 q

@master
"#;

fn state_with_stream() -> std::sync::Arc<AppState> {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(PROJECT).unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.audio.sample_rate = 8_000;
        inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8_000));
        inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .meters
            .set_master_channels(oc_engine::graph::MASTER_CHANNELS);
    }
    std::sync::Arc::new(state)
}

fn output_dir(tag: &str) -> PathBuf {
    oc_test_support::temp_dir("oc-monitor", tag)
}

#[test]
fn the_mono_check_never_changes_an_export() {
    let state = state_with_stream();
    let dir = output_dir("mono-export");
    let export = |name: &str| -> Vec<u8> {
        let output = dir.join(name);
        crate::commands::render::render_offline_with(
            &state,
            output.display().to_string(),
            None,
            None,
            None,
            crate::commands::render::render_offline_snapshot,
        )
        .expect("the export writes");
        std::fs::read(&output).expect("the export reads back")
    };

    let stereo = export("stereo.wav");
    {
        let mut inner = state.lock_inner();
        let mono = inner.audio.set_monitor_mono(true).expect("a stereo master");
        assert!(mono.requested);
        assert!(!mono.applied, "nothing has folded a chunk yet");
        assert!(inner.audio.meter_snapshot().mono.requested);
    }
    let folded = export("mono.wav");

    // The file carries audio, so an empty-versus-empty comparison cannot pass.
    let path = dir.join("stereo.wav").display().to_string();
    let audio = oc_engine::analysis::read_wav(&path).expect("the exported WAV reads");
    assert!(
        audio
            .channels
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 1e-4),
        "the export is silent; the comparison would be meaningless"
    );
    assert_eq!(stereo.len(), folded.len());
    assert_eq!(stereo, folded, "the mono check reached the offline export");
}

#[test]
fn the_mono_check_refuses_a_non_stereo_master() {
    let state = state_with_stream();
    let mut inner = state.lock_inner();
    inner
        .audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .meters
        .set_master_channels(1);
    let error = inner.audio.set_monitor_mono(true).unwrap_err();
    assert_eq!(error, "Mono check needs a stereo master.");
    assert!(
        !inner.audio.meter_snapshot().mono.requested,
        "a refused request must not arm the fold"
    );
}

#[test]
fn the_mono_check_needs_a_streaming_playback() {
    let state = AppState::new();
    let mut inner = state.lock_inner();
    let error = inner.audio.set_monitor_mono(true).unwrap_err();
    assert_eq!(error, "Mono check needs streaming playback.");
}

#[test]
fn the_clip_reset_command_clears_every_latch() {
    let state = state_with_stream();
    {
        let inner = state.lock_inner();
        let hub = &inner.audio.render_thread.as_ref().unwrap().playback.meters;
        hub.latch_master_clip();
        assert!(inner.audio.meter_snapshot().master.node.clipped);
        inner.audio.reset_meter_clip();
        assert!(!inner.audio.meter_snapshot().master.node.clipped);
    }
}
