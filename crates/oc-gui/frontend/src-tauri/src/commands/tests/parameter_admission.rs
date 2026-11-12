//! A delayed interaction must retain the topology it actually observed.

use crate::commands;
use crate::state::{AppState, ParameterTargetStamp, UndoScope};
use tauri::{
    Manager,
    test::{MockRuntime, mock_builder, mock_context, noop_assets},
};

const SOURCE: &str = "@project \"Admission\"\n@preset Lead\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track Lead | instrument: synth/lead_saw, preset: Lead\n  effects:\n    - gain: factor=0.5\n    - gain: factor=0.8\n";

fn app() -> tauri::App<MockRuntime> {
    let state = AppState::new();
    commands::open_project(&mut state.lock_inner(), SOURCE, None).unwrap();
    mock_builder()
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap()
}
fn stamp(app: &tauri::App<MockRuntime>) -> ParameterTargetStamp {
    app.state::<AppState>()
        .lock_inner()
        .undo
        .parameter_target_stamp()
        .unwrap()
        .clone()
}
fn observed(app: &tauri::App<MockRuntime>) -> serde_json::Value {
    let state = app.state::<AppState>();
    let inner = state.lock_inner();
    serde_json::json!({"snapshot":commands::snapshot(&inner),"epoch":inner.content_epoch,
        "render":inner.render_generation,"undo":inner.undo.undo.len(),"redo":inner.undo.redo.len(),
        "gesture":inner.undo.is_in_gesture(),"pluginSessions":inner.plugin_automation_gestures.len(),
        "effectSessions":inner.effect_write_gestures.len()})
}

#[test]
fn parameter_admission_delayed_begin_after_actual_same_type_move_is_refused() {
    let app = app();
    let expected = stamp(&app);
    // The event captured A at index 0, but its begin has not entered the mutex.
    let delayed =
        || commands::begin_parameter_gesture(UndoScope::Preset, expected.clone(), app.state());
    commands::move_effect("track:0".into(), 0, 1, app.state()).unwrap();
    assert_ne!(stamp(&app), expected);
    let before = observed(&app);
    assert_eq!(
        delayed().unwrap_err(),
        "Parameter target changed before gesture admission"
    );
    assert_eq!(observed(&app), before);
    let state = app.state::<AppState>();
    {
        let inner = state.lock_inner();
        let effects = &inner.project.tracks[0].effects;
        assert_eq!(effects[0].params[0].1.as_f64(), Some(0.8));
        assert_eq!(effects[1].params[0].1.as_f64(), Some(0.5));
    }
    // A new intent can explicitly target A at its new location with a fresh capture.
    let token =
        commands::begin_parameter_gesture(UndoScope::Preset, stamp(&app), app.state()).unwrap();
    commands::set_effect_param_owned(
        "track:0".into(),
        1,
        "factor".into(),
        "0.2".into(),
        token.clone(),
        app.state(),
    )
    .unwrap();
    assert!(commands::end_owned_undo_gesture(token, app.state()));
    assert_eq!(
        state.lock_inner().project.tracks[0].effects[0].params[0]
            .1
            .as_f64(),
        Some(0.8)
    );
    assert_eq!(
        state.lock_inner().project.tracks[0].effects[1].params[0]
            .1
            .as_f64(),
        Some(0.2)
    );
}

#[test]
fn parameter_admission_automation_begins_refuse_stale_capture_before_any_session_mint() {
    let app = app();
    let expected = stamp(&app);
    commands::move_effect("track:0".into(), 0, 1, app.state()).unwrap();
    let before = observed(&app);
    assert!(
        commands::begin_guarded_effect_param_write(
            "track:0".into(),
            0,
            "factor".into(),
            expected.clone(),
            app.state()
        )
        .is_err()
    );
    assert!(
        commands::begin_guarded_synth_param_gesture(
            "Lead".into(),
            0,
            "output.gain".into(),
            expected.clone(),
            app.state()
        )
        .is_err()
    );
    // Stale admission precedes even the hosted-instance eligibility query.
    assert_eq!(
        commands::begin_guarded_plugin_param_gesture(
            "Lead".into(),
            0,
            "Gain".into(),
            expected,
            app.state()
        )
        .unwrap_err(),
        "Parameter target changed before gesture admission"
    );
    assert_eq!(observed(&app), before);
    let effect = commands::begin_guarded_effect_param_write(
        "track:0".into(),
        0,
        "factor".into(),
        stamp(&app),
        app.state(),
    )
    .unwrap();
    assert!(
        commands::end_owned_effect_param_write(
            "track:0".into(),
            0,
            "factor".into(),
            effect,
            app.state()
        )
        .unwrap()
        .closed
    );
    let synth = commands::begin_guarded_synth_param_gesture(
        "Lead".into(),
        0,
        "output.gain".into(),
        stamp(&app),
        app.state(),
    )
    .unwrap();
    assert!(
        commands::end_owned_synth_param_gesture(
            "Lead".into(),
            "output.gain".into(),
            synth,
            app.state()
        )
        .unwrap()
        .closed
    );
}

#[test]
fn parameter_admission_is_stable_across_normal_edits_close_and_failed_or_noop_structure() {
    let app = app();
    let expected = stamp(&app);
    commands::set_effect_param(
        "track:0".into(),
        0,
        "factor".into(),
        "0.7".into(),
        app.state(),
    )
    .unwrap();
    assert_eq!(stamp(&app), expected);
    let before = observed(&app);
    assert!(commands::add_effect("invalid-target".into(), "gain".into(), app.state()).is_err());
    assert!(commands::remove_effect("master".into(), 0, app.state()).is_err());
    assert!(commands::move_effect("track:0".into(), 0, 99, app.state()).is_err());
    assert_eq!(observed(&app), before);
    commands::move_effect("track:0".into(), 0, 0, app.state()).unwrap();
    assert_eq!(observed(&app), before);
    let token = commands::begin_parameter_gesture(UndoScope::Preset, expected.clone(), app.state())
        .unwrap();
    assert!(commands::end_owned_undo_gesture(token, app.state()));
    assert_eq!(stamp(&app), expected);
}

#[test]
fn parameter_admission_refuses_transaction_topology_even_when_revision_is_unchanged() {
    let app = app();
    let state = app.state::<AppState>();
    state
        .lock_inner()
        .begin_transaction("Reorder".into(), "test".into())
        .unwrap();
    let expected = stamp(&app);
    let revision = state.lock_inner().revision;
    commands::move_effect("track:0".into(), 0, 1, app.state()).unwrap();
    assert_eq!(state.lock_inner().revision, revision);
    let before = observed(&app);
    assert!(commands::begin_parameter_gesture(UndoScope::Preset, expected, app.state()).is_err());
    assert_eq!(observed(&app), before);
    let in_transaction = stamp(&app);
    state.lock_inner().rollback_transaction().unwrap();
    assert_ne!(stamp(&app), in_transaction);
    assert!(
        commands::begin_parameter_gesture(UndoScope::Preset, in_transaction, app.state()).is_err()
    );
}

#[test]
fn parameter_admission_history_and_same_name_document_replacement_never_reuse_capture() {
    let app = app();
    commands::set_effect_param(
        "track:0".into(),
        0,
        "factor".into(),
        "0.7".into(),
        app.state(),
    )
    .unwrap();
    let before_history = stamp(&app);
    commands::undo(app.state()).unwrap();
    let after_undo = stamp(&app);
    assert_ne!(after_undo, before_history);
    commands::redo(app.state()).unwrap();
    let after_redo = stamp(&app);
    assert_ne!(after_redo, after_undo);
    assert!(
        commands::begin_parameter_gesture(UndoScope::Preset, before_history, app.state()).is_err()
    );
    commands::open_project(&mut app.state::<AppState>().lock_inner(), SOURCE, None).unwrap();
    assert_ne!(stamp(&app), after_redo);
    let before = observed(&app);
    assert!(commands::begin_parameter_gesture(UndoScope::Preset, after_redo, app.state()).is_err());
    assert_eq!(observed(&app), before);
}

#[test]
fn parameter_admission_snapshot_is_an_opaque_string_and_old_snapshot_decodes_without_it() {
    let app = app();
    let snapshot = commands::snapshot(&app.state::<AppState>().lock_inner());
    let mut encoded = serde_json::to_value(&snapshot).unwrap();
    assert!(encoded["parameter_target_stamp"].is_string());
    let decoded: commands::ProjectSnapshot = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(
        decoded.parameter_target_stamp,
        snapshot.parameter_target_stamp
    );
    encoded
        .as_object_mut()
        .unwrap()
        .remove("parameter_target_stamp");
    let legacy: commands::ProjectSnapshot = serde_json::from_value(encoded).unwrap();
    assert!(legacy.parameter_target_stamp.is_none());
    assert_eq!(legacy.project, snapshot.project);
    let unknown = serde_json::from_value(serde_json::json!("unknown-stamp")).unwrap();
    let before = observed(&app);
    assert!(commands::begin_parameter_gesture(UndoScope::Preset, unknown, app.state()).is_err());
    assert_eq!(observed(&app), before);
}
