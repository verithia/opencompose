use super::*;
use std::cell::Cell;

fn loaded() -> AppState {
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(),"@project \"First\"\n@preset Lead\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track Lead | instrument: synth/lead_saw, preset: Lead\n",None).unwrap();
    state
}
fn observed(inner: &AppStateInner) -> serde_json::Value {
    serde_json::json!({"project":inner.project,"dsl":inner.dsl_source,"revision":inner.revision,"epoch":inner.content_epoch,"dirty":inner.dirty,"undo":inner.undo.undo.len(),"redo":inner.undo.redo.len(),"sessions":inner.plugin_automation_gestures.len()})
}
#[test]
fn owned_hosted_point_and_end_cannot_cross_document_or_steal_new_same_key_session() {
    let state = loaded();
    let mut inner = state.lock_inner();
    // Explicit lifecycle fixture; native parameter application is injected below.
    let stale = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let replacement = oc_dsl::serialize(&inner.project);
    crate::commands::open_project(&mut inner, &replacement, None).unwrap();
    let current = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let before = observed(&inner);
    let writes = Cell::new(0);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.5,
            ast::BarBeat::new(1, 1, 0),
            Some(&stale),
            |_, _, _, _| {
                writes.set(writes.get() + 1);
                Ok(0.5)
            }
        )
        .is_err()
    );
    assert!(
        !end_owned_plugin_session(&mut inner, "Lead", "Gain", &stale)
            .unwrap()
            .closed
    );
    assert!(
        update_plugin_param_gesture_inner(
            &mut inner,
            "Lead",
            "Gain",
            0.5,
            ast::BarBeat::new(1, 1, 0)
        )
        .is_err()
    );
    assert!(end_plugin_param_gesture_inner(&mut inner, "Lead", "Gain").is_err());
    assert_eq!(writes.get(), 0);
    assert_eq!(observed(&inner), before);
    apply_plugin_point(
        &mut inner,
        "Lead",
        "Gain",
        0.7,
        ast::BarBeat::new(1, 1, 0),
        Some(&current),
        |_, _, _, value| {
            writes.set(writes.get() + 1);
            Ok(value)
        },
    )
    .unwrap();
    assert_eq!(writes.get(), 1);
    assert_eq!(
        inner.project.tracks[0].automation().next().unwrap().points[0].value,
        ast::ParamValue::Number(0.7)
    );
    assert!(
        end_owned_plugin_session(&mut inner, "Lead", "Gain", &current)
            .unwrap()
            .closed
    );
    assert!(
        !end_owned_plugin_session(&mut inner, "Lead", "Gain", &current)
            .unwrap()
            .closed
    );
    assert_eq!(inner.undo.undo.len(), 1);
}
#[test]
fn owned_hosted_invalid_clock_or_native_refusal_has_no_project_or_history_side_effects() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let before = observed(&inner);
    let writes = Cell::new(0);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.5,
            ast::BarBeat::new(1, 99, 0),
            Some(&token),
            |_, _, _, _| {
                writes.set(1);
                Ok(0.5)
            }
        )
        .is_err()
    );
    assert_eq!(writes.get(), 0);
    assert_eq!(observed(&inner), before);
    let error = apply_plugin_point(
        &mut inner,
        "Lead",
        "Gain",
        0.5,
        ast::BarBeat::new(1, 1, 0),
        Some(&token),
        |_, _, _, _| Err("Native parameter refused".into()),
    )
    .unwrap_err();
    assert_eq!(error, "Native parameter refused");
    assert_eq!(observed(&inner), before);
    assert!(
        end_owned_plugin_session(&mut inner, "Lead", "Gain", &token)
            .unwrap()
            .closed
    );
    assert!(inner.undo.undo.is_empty());
}
#[test]
fn owned_hosted_track_rebinding_expires_membership_before_any_native_write() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let mut candidate = inner.project.clone();
    candidate.tracks[0].preset = None;
    crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
    let before = observed(&inner);
    let writes = Cell::new(0);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.5,
            ast::BarBeat::new(1, 1, 0),
            Some(&token),
            |_, _, _, _| {
                writes.set(1);
                Ok(0.5)
            }
        )
        .is_err()
    );
    assert_eq!(writes.get(), 0);
    assert!(
        !end_owned_plugin_session(&mut inner, "Lead", "Gain", &token)
            .unwrap()
            .closed
    );
    assert_eq!(observed(&inner), before);
}

#[test]
fn owned_hosted_begin_without_native_instance_does_not_open_a_session() {
    use tauri::{
        Manager,
        test::{mock_builder, mock_context, noop_assets},
    };
    let app = mock_builder()
        .manage(loaded())
        .build(mock_context(noop_assets()))
        .unwrap();
    let state = app.state::<AppState>();
    let before = observed(&state.lock_inner());
    assert!(
        begin_owned_plugin_param_gesture("Lead".into(), 0, "Gain".into(), app.state()).is_err()
    );
    let inner = state.lock_inner();
    assert_eq!(observed(&inner), before);
    assert!(!inner.undo.is_in_gesture());
}

#[test]
fn owned_hosted_recording_survives_structural_invalidation_with_native_writes_exact_once() {
    for structural in ["rename", "delete"] {
        let state = loaded();
        let mut inner = state.lock_inner();
        let original = inner.project.clone();
        let token = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
        let writes = Cell::new(0);
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.75,
            ast::BarBeat::new(1, 1, 0),
            Some(&token),
            |_, _, _, value| {
                writes.set(writes.get() + 1);
                Ok(value)
            },
        )
        .unwrap();
        let recorded = inner.project.clone();
        assert_eq!(inner.undo.undo.len(), 1);
        let mut candidate = recorded.clone();
        if structural == "rename" {
            candidate.tracks[0].name = "Renamed".into();
        } else {
            candidate.tracks.clear();
        }
        crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
        let changed = inner.project.clone();
        let before = observed(&inner);
        assert!(
            !end_owned_plugin_session(&mut inner, "Lead", "Gain", &token)
                .unwrap()
                .closed
        );
        assert_eq!(observed(&inner), before);
        assert_eq!(writes.get(), 1);
        assert_eq!(inner.undo.undo.len(), 2);
        inner.undo_step().unwrap();
        assert_eq!(inner.project, recorded);
        inner.undo_step().unwrap();
        assert_eq!(inner.project, original);
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, recorded);
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, changed);
        assert_eq!(
            writes.get(),
            1,
            "history must not replay an old native setter closure"
        );
    }
}
#[test]
fn owned_hosted_concurrent_sessions_and_ordinary_sibling_have_one_original_baseline() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    let first = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    apply_plugin_point(
        &mut inner,
        "Lead",
        "Gain",
        0.7,
        ast::BarBeat::new(1, 1, 0),
        Some(&first),
        |_, _, _, v| Ok(v),
    )
    .unwrap();
    let second = begin_owned_plugin_session(&mut inner, "Lead", 0, "Depth").unwrap();
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Project).unwrap();
    let old = inner.project.clone();
    inner.project.name = Some("ordinary sibling".into());
    resync_with_undo(&mut inner, old, UndoEdit::new("name", UndoScope::Project)).unwrap();
    apply_plugin_point(
        &mut inner,
        "Lead",
        "Depth",
        0.4,
        ast::BarBeat::new(1, 2, 0),
        Some(&second),
        |_, _, _, v| Ok(v),
    )
    .unwrap();
    let final_project = inner.project.clone();
    assert!(
        end_owned_plugin_session(&mut inner, "Lead", "Gain", &first)
            .unwrap()
            .closed
    );
    assert!(
        end_owned_plugin_session(&mut inner, "Lead", "Depth", &second)
            .unwrap()
            .closed
    );
    assert!(inner.undo.end_owned_gesture(&ordinary));
    assert_eq!(inner.undo.undo.len(), 1);
    inner.undo_step().unwrap();
    assert_eq!(inner.project, original);
    redo_recorded_history(&mut inner);
    assert_eq!(inner.project, final_project);
}
#[test]
fn owned_hosted_callback_cannot_publish_over_a_newer_document() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let old = inner.project.clone();
    let writes = Cell::new(0);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.75,
            ast::BarBeat::new(1, 1, 0),
            Some(&token),
            |inner, _, _, v| {
                writes.set(writes.get() + 1);
                inner.project.name = Some("newer callback document".into());
                Ok(v)
            }
        )
        .is_err()
    );
    assert_eq!(writes.get(), 1);
    assert_eq!(
        inner.project.name.as_deref(),
        Some("newer callback document")
    );
    assert_eq!(
        inner.project.tracks, old.tracks,
        "unpublished lane must not overwrite callback state"
    );
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn owned_hosted_history_encoding_failure_precedes_every_native_setter() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin_owned_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let writes = Cell::new(0);
    for merged in [false, true] {
        if merged {
            apply_plugin_point(
                &mut inner,
                "Lead",
                "Gain",
                0.6,
                ast::BarBeat::new(1, 1, 0),
                Some(&token),
                |_, _, _, v| Ok(v),
            )
            .unwrap();
        }
        let before = observed(&inner);
        inner.set_history_encoding_failure_for_test(true);
        assert!(
            apply_plugin_point(
                &mut inner,
                "Lead",
                "Gain",
                0.75,
                ast::BarBeat::new(1, 2, 0),
                Some(&token),
                |_, _, _, v| {
                    writes.set(writes.get() + 1);
                    Ok(v)
                }
            )
            .is_err()
        );
        inner.set_history_encoding_failure_for_test(false);
        assert_eq!(writes.get(), 0);
        assert_eq!(observed(&inner), before);
        assert!(inner.undo.is_owned_gesture_active(&token));
    }
}

fn redo_recorded_history(inner: &mut AppStateInner) {
    inner.undo.redo(&mut inner.project).unwrap();
    inner.resync_after_history();
}

#[test]
fn mixed_legacy_hosted_then_owned_recording_survives_structural_and_late_close() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    // The actual native availability gate is covered by command admission tests;
    // this shared session/point pipeline injects its native setter explicitly.
    begin_legacy_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    let writes = Cell::new(0);
    apply_plugin_point(
        &mut inner,
        "Lead",
        "Gain",
        0.6,
        ast::BarBeat::new(1, 1, 0),
        None,
        |_, _, _, v| {
            writes.set(writes.get() + 1);
            Ok(v)
        },
    )
    .unwrap();
    let owned = begin_owned_plugin_session(&mut inner, "Lead", 0, "Depth").unwrap();
    apply_plugin_point(
        &mut inner,
        "Lead",
        "Depth",
        0.8,
        ast::BarBeat::new(1, 2, 0),
        Some(&owned),
        |_, _, _, v| {
            writes.set(writes.get() + 1);
            Ok(v)
        },
    )
    .unwrap();
    let old = inner.project.clone();
    inner.project.name = Some("ordinary sibling".into());
    resync_with_undo(&mut inner, old, UndoEdit::new("name", UndoScope::Project)).unwrap();
    let recorded = inner.project.clone();
    assert_eq!(inner.undo.undo.len(), 1);
    let mut candidate = recorded.clone();
    candidate.tracks[0].name = "Renamed".into();
    crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
    let changed = inner.project.clone();
    assert_eq!(inner.undo.undo.len(), 2);
    inner.undo.begin_gesture(UndoScope::Project);
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    let before = observed(&inner);
    end_plugin_param_gesture_inner(&mut inner, "Lead", "Gain").unwrap();
    assert!(
        !end_owned_plugin_session(&mut inner, "Lead", "Depth", &owned)
            .unwrap()
            .closed
    );
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.9,
            ast::BarBeat::new(1, 3, 0),
            None,
            |_, _, _, v| {
                writes.set(writes.get() + 1);
                Ok(v)
            }
        )
        .is_err()
    );
    assert_eq!(writes.get(), 2);
    assert_eq!(observed(&inner), before);
    assert!(inner.undo.end_owned_gesture(&ordinary));
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
    inner.undo_step().unwrap();
    assert_eq!(inner.project, recorded);
    inner.undo_step().unwrap();
    assert_eq!(inner.project, original);
    redo_recorded_history(&mut inner);
    assert_eq!(inner.project, recorded);
    redo_recorded_history(&mut inner);
    assert_eq!(inner.project, changed);
    assert_eq!(writes.get(), 2);
}
#[test]
fn legacy_hosted_begin_failed_point_and_close_preserve_outer_depth_and_native_state() {
    let state = loaded();
    let mut inner = state.lock_inner();
    inner.undo.begin_gesture(UndoScope::Project);
    let original = inner.project.clone();
    let dsl = inner.dsl_source.clone();
    let revision = inner.revision;
    begin_legacy_plugin_session(&mut inner, "Lead", 0, "Gain").unwrap();
    assert_eq!(inner.project, original);
    assert_eq!(inner.dsl_source, dsl);
    assert_eq!(inner.revision, revision);
    let before = observed(&inner);
    let writes = Cell::new(0);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.6,
            ast::BarBeat::new(1, 99, 0),
            None,
            |_, _, _, v| {
                writes.set(writes.get() + 1);
                Ok(v)
            }
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(true);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.6,
            ast::BarBeat::new(1, 1, 0),
            None,
            |_, _, _, v| {
                writes.set(writes.get() + 1);
                Ok(v)
            }
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(false);
    assert_eq!(writes.get(), 0);
    assert_eq!(observed(&inner), before);
    assert!(
        apply_plugin_point(
            &mut inner,
            "Lead",
            "Gain",
            0.6,
            ast::BarBeat::new(1, 1, 0),
            None,
            |_, _, _, _| Err("Native refused".into())
        )
        .is_err()
    );
    assert_eq!(observed(&inner), before);
    end_plugin_param_gesture_inner(&mut inner, "Lead", "Gain").unwrap();
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
    assert_eq!(inner.project, original);
    assert!(inner.undo.undo.is_empty());
}
