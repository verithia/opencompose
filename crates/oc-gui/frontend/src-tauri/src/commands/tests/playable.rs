//! Tool-path track creation: a created track gets a visible built-in default
//! and renders, without a SoundFont.

use crate::commands::*;
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

fn create_piano(state: &AppState) {
    let dsl = "@track keys | instrument: piano/acoustic_grand\n  @phrase riff [1:1:0 - 3:1:0]\n    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n";
    let mut inner = state.lock_inner();
    create_track_checked(&mut inner, "keys", "piano", "acoustic_grand", None, dsl).unwrap();
}

#[test]
fn a_created_piano_track_gets_a_default_and_renders_audible() {
    let dir = TempDir::new("piano-default");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project("@project\n  tempo: 120\n", dir.path());
    create_piano(&state);

    {
        let inner = state.lock_inner();
        assert_eq!(inner.project.tracks.len(), 1);
        assert_eq!(
            inner.project.tracks[0].preset.as_deref(),
            Some("oc_default_keys")
        );
        assert_eq!(inner.project.presets.len(), 1);
        assert!(
            inner.dsl_source.contains("@preset oc_default_keys"),
            "{}",
            inner.dsl_source
        );
        assert!(
            inner.dsl_source.contains("preset: oc_default_keys"),
            "the injected preset must be visible in the DSL: {}",
            inner.dsl_source
        );
    }

    let output = dir.path().join("range.wav");
    let rendered = render_range(
        output.to_string_lossy().into_owned(),
        RangeRequest {
            start_s: Some(0.0),
            end_s: Some(4.0),
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    let report = oc_engine::analysis::analyze_wav(&rendered.path).unwrap();
    assert!(
        report.level.peak_dbfs > -40.0,
        "the created track rendered silence: {:?}",
        report.level
    );

    // Not just a click: the second bar must still carry energy.
    let bar_output = dir.path().join("bar2.wav");
    let bar = render_range(
        bar_output.to_string_lossy().into_owned(),
        RangeRequest {
            start_s: Some(2.0),
            end_s: Some(4.0),
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    let bar_report = oc_engine::analysis::analyze_wav(&bar.path).unwrap();
    assert!(
        bar_report.level.rms_dbfs > -40.0,
        "the second bar is not sustained: {:?}",
        bar_report.level
    );
}

#[test]
fn tracks_add_also_assigns_the_family_default() {
    let dir = TempDir::new("add-default");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project("@project\n  tempo: 120\n", dir.path());
    {
        let mut inner = state.lock_inner();
        add_track_checked(&mut inner, "bass", "bass", "electric_finger", None).unwrap();
        assert_eq!(
            inner.project.tracks[0].preset.as_deref(),
            Some("oc_default_bass")
        );
        assert!(
            inner.dsl_source.contains("preset: oc_default_bass"),
            "{}",
            inner.dsl_source
        );
    }
}

#[test]
fn a_family_with_no_default_is_still_refused_and_leaves_nothing_behind() {
    let dir = TempDir::new("sample-refused");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project("@project\n  tempo: 120\n", dir.path());
    let dsl =
        "@track take | instrument: sample/take\n  @phrase riff [1:1:0 - 2:1:0]\n    A2 h mf\n";
    let error = {
        let mut inner = state.lock_inner();
        create_track_checked(&mut inner, "take", "sample", "take", None, dsl).unwrap_err()
    };
    assert!(error.contains("Track 'take'"), "{error}");
    assert!(error.contains("needs a SoundFont"), "{error}");
    assert!(error.contains("@preset"), "{error}");
    assert!(error.contains("builtin.open_synth"), "{error}");
    assert!(state.lock_inner().project.tracks.is_empty());
}
