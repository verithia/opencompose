//! The preset click path: the load runs off the webview thread, one click at a
//! time, and the newest click is the one that lands.

use std::sync::mpsc;
use std::time::Duration;

use crate::commands::shared::run_blocking;
use crate::commands::synth::{set_preset_params_blocking, set_track_preset_blocking};

use super::support::*;

const SOURCE: &str = r#"@ocp "0.2.0"

@project "preset load"
  tempo: 120
  time_signature: 4/4

@preset lead
  source: builtin
  plugin: "builtin.open_synth"
  params: { osc.a.level: 0.5, filter.cutoff: 12000 }

@preset keys
  file: "media/grand.sf2"

@pattern riff | instrument: synth/lead_square, preset: keys
  events:
    E4 q mf

@track lead | preset: lead, instrument: synth/lead_square
  @phrase line [1:1 - 2:1]
    C4 q mf
"#;

/// A whole-patch batch the way the editor sends it.
fn batch() -> Vec<(String, Option<String>)> {
    vec![
        ("osc.a.level".into(), Some("0.58".into())),
        ("filter.cutoff".into(), Some("4200".into())),
        ("amp.attack".into(), Some("0.72".into())),
        ("wavetable.file".into(), None),
    ]
}

fn state() -> (crate::state::AppState, TempDir) {
    let dir = TempDir::new("preset-load");
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/grand.sf2"), b"soundfont").unwrap();
    let state = state_with_project(SOURCE, dir.path());
    (state, dir)
}

#[test]
fn a_patch_load_never_runs_on_the_calling_thread() {
    let caller = std::thread::current().id();
    let (worker_tx, worker_rx) = mpsc::channel();
    tauri::async_runtime::block_on(run_blocking(move || {
        worker_tx.send(std::thread::current().id()).unwrap();
        Ok::<(), String>(())
    }))
    .unwrap();
    let worker = worker_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_ne!(
        worker, caller,
        "the load body must not run on the thread that dispatched it"
    );
}

#[test]
fn a_patch_load_leaves_the_caller_free_while_it_runs() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let task = tauri::async_runtime::spawn(async move {
        run_blocking(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        })
        .await
    });
    // The body has started and is still holding on, yet the caller is free.
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        !task.inner().is_finished(),
        "the caller must not wait for the load body to return"
    );
    release_tx.send(()).unwrap();
    tauri::async_runtime::block_on(task).unwrap().unwrap();
}

#[test]
fn a_superseded_click_leaves_the_project_untouched() {
    let (state, _dir) = state();
    let stale = state.patch_load.begin();
    let _newer = state.patch_load.begin();
    let before = state.lock_inner().project.presets[0].clone();

    let snapshot = set_preset_params_blocking(
        &state,
        "lead".into(),
        batch(),
        true,
        Vec::new(),
        Some("Glass Pad".to_string()),
        Some(stale),
    )
    .unwrap();

    assert_eq!(
        state.lock_inner().project.presets[0],
        before,
        "a click superseded by a newer one must not mutate the project"
    );
    assert_eq!(snapshot.project.presets[0], before);
    assert_eq!(
        snapshot.revision, 0,
        "no revision is spent on a stale click"
    );
}

#[test]
fn the_newest_click_still_replaces_the_whole_patch() {
    let (state, _dir) = state();
    let ticket = state.patch_load.begin();

    let snapshot = set_preset_params_blocking(
        &state,
        "lead".into(),
        batch(),
        true,
        Vec::new(),
        Some("Glass Pad".to_string()),
        Some(ticket),
    )
    .unwrap();

    let preset = &snapshot.project.presets[0];
    assert_eq!(preset.patch.as_deref(), Some("Glass Pad"));
    assert_eq!(
        preset
            .params
            .iter()
            .find(|(key, _)| key == "filter.cutoff")
            .map(|(_, value)| value.clone()),
        Some(oc_dsl::ast::ParamValue::Number(4200.0))
    );
    // Three values land; `wavetable.file` is a removal, so it is not stored.
    assert_eq!(preset.params.len(), 3);
    assert!(!preset.params.iter().any(|(key, _)| key == "wavetable.file"));
}

#[test]
fn a_parameter_batch_is_never_treated_as_a_superseded_click() {
    let (state, _dir) = state();
    state.patch_load.begin();
    state.patch_load.begin();
    let before = state.lock_inner().project.presets[0].params.len();

    let snapshot = set_preset_params_blocking(
        &state,
        "lead".into(),
        vec![("osc.a.level".into(), Some("0.8".into()))],
        false,
        Vec::new(),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        snapshot.project.presets[0].params.len(),
        before,
        "a single-parameter edit keeps the patch it started from"
    );
    assert_eq!(
        snapshot.project.presets[0]
            .params
            .iter()
            .find(|(key, _)| key == "osc.a.level")
            .map(|(_, value)| value.clone()),
        Some(oc_dsl::ast::ParamValue::Number(0.8))
    );
}

#[test]
fn switching_a_patterns_track_preset_twice_keeps_the_last_one() {
    let (state, _dir) = state();

    set_track_preset_blocking(&state, 0, Some("keys".into())).unwrap();
    let snapshot = set_track_preset_blocking(&state, 0, Some("lead".into())).unwrap();

    assert_eq!(snapshot.project.tracks[0].preset.as_deref(), Some("lead"));
    // The pattern keeps its own preset reference; only the track moved.
    assert_eq!(
        snapshot.project.patterns[0].preset.as_deref(),
        Some("keys"),
        "a track preset change must not rewrite a pattern's own preset"
    );
}

#[test]
fn assigning_a_soundfont_preset_to_a_patterns_track_survives_the_resync() {
    let (state, _dir) = state();

    let snapshot = set_track_preset_blocking(&state, 0, Some("keys".into())).unwrap();
    assert_eq!(snapshot.project.tracks[0].preset.as_deref(), Some("keys"));
    assert!(!snapshot.project.tracks[0].phrases[0].events.is_empty());
    // The dropdown's empty option takes the same path; the instrument family
    // then gets its default patch, which is what the editor shows next.
    let snapshot = set_track_preset_blocking(&state, 0, None).unwrap();
    let assigned = snapshot.project.tracks[0].preset.clone();
    assert_ne!(assigned.as_deref(), Some("keys"));
    assert!(assigned.is_some(), "the resync assigns a family default");
}

/// The click path that stamps a factory patch resolves the test store and
/// leaves the user's `~/.opencompose/preferences.json` untouched.
#[test]
fn the_patch_store_stays_out_of_the_operator_home() {
    use crate::commands::patch::{preset_library, preset_preferences};

    let library_dir = preset_library().dir().to_path_buf();
    let preferences_path = preset_preferences().path();
    assert!(
        library_dir.starts_with(std::env::temp_dir()),
        "the test library resolved the operator's home: {}",
        library_dir.display()
    );
    assert!(
        preferences_path.starts_with(std::env::temp_dir()),
        "the test preferences resolved the operator's home: {}",
        preferences_path.display()
    );
    assert_eq!(
        preferences_path,
        oc_engine::preset::library::PresetPreferences::path_beside(&library_dir),
        "the stamp store sits beside the library, inside the test root"
    );
}
