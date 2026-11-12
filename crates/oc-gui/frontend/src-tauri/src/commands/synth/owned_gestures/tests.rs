use super::*;

fn source(title: &str, gain: f64) -> String {
    format!(
        "@project \"{title}\"\n@preset Lead\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{output.gain: {gain}}}\n@track Lead | instrument: synth/lead_saw, preset: Lead\n"
    )
}
fn loaded() -> AppState {
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(), &source("First", 0.5), None).unwrap();
    state
}
fn begin(inner: &mut AppStateInner) -> UndoGestureToken {
    begin_owned_synth_param_gesture_inner(inner, "Lead", 0, "output.gain").unwrap()
}
fn point(inner: &mut AppStateInner, token: &UndoGestureToken, physical: f64) -> Result<(), String> {
    update_owned_synth_param_gesture_inner(
        inner,
        "Lead",
        "output.gain",
        OwnedSynthPoint {
            normalized: 0.6,
            physical: Some(physical),
            position: ast::BarBeat::new(1, 1, 0),
            token,
        },
    )
}
fn end(
    inner: &mut AppStateInner,
    token: &UndoGestureToken,
) -> Result<OwnedSynthGestureEnd, String> {
    end_owned_synth_param_gesture_inner(inner, "Lead", "output.gain", token)
}
fn batch(value: &str) -> PresetParameterBatch {
    PresetParameterBatch {
        name: "Lead".into(),
        params: vec![("output.gain".into(), Some(value.into()))],
        replace_synth: false,
        effects: Vec::new(),
        patch_label: None,
        ticket: None,
    }
}
#[derive(Debug, PartialEq)]
struct Observed {
    project: ast::Project,
    dsl: String,
    revision: u64,
    content_epoch: u64,
    render_generation: u64,
    dirty: bool,
    undo: usize,
    redo: usize,
    gestures: usize,
}
fn observe(inner: &AppStateInner) -> Observed {
    Observed {
        project: inner.project.clone(),
        dsl: inner.dsl_source.clone(),
        revision: inner.revision,
        content_epoch: inner.content_epoch,
        render_generation: inner.render_generation,
        dirty: inner.dirty,
        undo: inner.undo.undo.len(),
        redo: inner.undo.redo.len(),
        gestures: inner.plugin_automation_gestures.len(),
    }
}

#[test]
fn owned_synth_mutations_queued_before_same_name_document_replacement_are_refused() {
    let state = loaded();
    let stale = state
        .lock_inner()
        .undo
        .begin_owned_gesture(UndoScope::Preset)
        .unwrap();
    // This request was admitted before a blocking worker could acquire state.
    let queued = batch("0.1");
    {
        let mut inner = state.lock_inner();
        crate::commands::open_project(&mut inner, &source("Replacement", 0.9), None).unwrap();
    }
    let before = observe(&state.lock_inner());
    assert!(apply_parameter_batch(&state, queued, Some(&stale)).is_err());
    {
        let mut inner = state.lock_inner();
        assert!(
            set_preset_param_owned_inner(
                &mut inner,
                "Lead".into(),
                "output.gain".into(),
                Some("0.1".into()),
                &stale
            )
            .is_err()
        );
        assert_eq!(observe(&inner), before);
        assert_eq!(inner.project.name.as_deref(), Some("Replacement"));
        let current = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
        assert!(!inner.undo.end_owned_gesture(&stale));
        assert!(inner.undo.is_owned_gesture_active(&current));
        set_preset_param_owned_inner(
            &mut inner,
            "Lead".into(),
            "output.gain".into(),
            Some("0.7".into()),
            &current,
        )
        .unwrap();
        assert!(inner.undo.end_owned_gesture(&current));
    }
}

#[test]
fn owned_synth_late_point_and_end_cannot_consume_a_new_same_key_session() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let stale = begin(&mut inner);
    point(&mut inner, &stale, 0.6).unwrap();
    crate::commands::open_project(&mut inner, &source("Replacement", 0.9), None).unwrap();
    let current = begin(&mut inner);
    let before = observe(&inner);
    assert!(point(&mut inner, &stale, 0.1).is_err());
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert_eq!(observe(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&current));
    assert_eq!(
        inner.plugin_automation_gestures[&synth_gesture_key("Lead", "output.gain")]
            .owner
            .as_ref(),
        Some(&current)
    );
    assert!(
        update_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "output.gain",
            0.1,
            Some(0.1),
            ast::BarBeat::new(1, 1, 0)
        )
        .is_err()
    );
    assert!(end_synth_param_gesture_inner(&mut inner, "Lead", "output.gain").is_err());
    assert_eq!(observe(&inner), before);
    point(&mut inner, &current, 0.7).unwrap();
    assert!(end(&mut inner, &current).unwrap().closed);
    assert!(!end(&mut inner, &current).unwrap().closed);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn owned_synth_undo_redo_expire_updates_even_when_the_session_map_still_matches() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    set_preset_param_owned_inner(
        &mut inner,
        "Lead".into(),
        "output.gain".into(),
        Some("0.7".into()),
        &ordinary,
    )
    .unwrap();
    inner.undo.end_owned_gesture(&ordinary);
    let stale = begin(&mut inner);
    inner.undo_step().unwrap();
    let before = observe(&inner);
    assert!(point(&mut inner, &stale, 0.1).is_err());
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert_eq!(observe(&inner), before);
    let current = begin(&mut inner); // The exact expired session is replaced, not another owner.
    let mut project = inner.project.clone();
    inner.undo.redo(&mut project).unwrap();
    inner.project = project;
    inner.resync_after_history();
    let before = observe(&inner);
    assert!(point(&mut inner, &current, 0.1).is_err());
    assert!(!end(&mut inner, &current).unwrap().closed);
    assert_eq!(observe(&inner), before);
    let newest = begin(&mut inner);
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert!(inner.undo.is_owned_gesture_active(&newest));
    assert!(end(&mut inner, &newest).unwrap().closed);
}

#[test]
fn owned_synth_failed_first_point_has_no_migration_or_parameter_side_effect() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let other = inner.undo.begin_owned_gesture(UndoScope::Project).unwrap();
    let before = observe(&inner);
    let token = begin(&mut inner);
    assert_eq!(inner.project, before.project);
    assert!(point(&mut inner, &token, 99.0).is_err());
    let mut expected = before;
    expected.gestures = 1;
    assert_eq!(observe(&inner), expected);
    assert!(inner.project.properties.automation_normalization.is_none());
    assert!(end(&mut inner, &token).unwrap().closed);
    assert!(inner.undo.is_owned_gesture_active(&other));
    assert!(inner.undo.is_in_gesture());
    assert!(inner.undo.undo.is_empty());
    assert!(inner.undo.end_owned_gesture(&other));
    assert!(!inner.undo.is_in_gesture());
}

#[test]
fn owned_synth_tokens_do_not_own_other_parameter_sessions_or_legacy_levels() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let first = begin(&mut inner);
    let second =
        begin_owned_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").unwrap();
    assert!(
        update_owned_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "filter.cutoff",
            OwnedSynthPoint {
                normalized: 0.5,
                physical: Some(400.0),
                position: ast::BarBeat::new(1, 1, 0),
                token: &first,
            }
        )
        .is_err()
    );
    assert!(
        !end_owned_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff", &first)
            .unwrap()
            .closed
    );
    inner.undo.begin_gesture(UndoScope::Project);
    assert!(end(&mut inner, &first).unwrap().closed);
    assert!(inner.undo.is_owned_gesture_active(&second));
    assert!(
        end_owned_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff", &second)
            .unwrap()
            .closed
    );
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
}

#[test]
fn owned_synth_randomize_and_learn_refuse_replaced_document_without_side_effects() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let stale = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    crate::commands::open_project(&mut inner, &source("Replacement", 0.9), None).unwrap();
    let current = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    let before = observe(&inner);
    assert!(randomize_preset_owned_inner(&mut inner, "Lead", 42, &stale).is_err());
    assert!(macro_learn_owned_inner(&mut inner, "Lead", 2, "filter.cutoff", &stale).is_err());
    assert_eq!(observe(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&current));
    assert!(!inner.undo.end_owned_gesture(&stale));
    assert!(inner.undo.end_owned_gesture(&current));
    assert!(randomize_preset_owned_inner(&mut inner, "Lead", 42, &current).is_err());
    assert!(macro_learn_owned_inner(&mut inner, "Lead", 2, "filter.cutoff", &current).is_err());
    assert_eq!(observe(&inner), before);
}

#[test]
fn owned_synth_randomize_preserves_seeded_domain_result_and_current_owner() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let mut expected = inner.project.clone();
    randomize_preset_in_project(&mut expected, "Lead", 42).unwrap();
    let token = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    randomize_preset_owned_inner(&mut inner, "Lead", 42, &token).unwrap();
    assert_eq!(inner.project, expected);
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(inner.undo.is_owned_gesture_active(&token));
    assert!(inner.undo.end_owned_gesture(&token));
}

#[test]
fn owned_synth_learn_preserves_domain_toggle_and_failed_request_is_atomic() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    let before = observe(&inner);
    assert!(macro_learn_owned_inner(&mut inner, "Lead", 9, "filter.cutoff", &token).is_err());
    assert_eq!(observe(&inner), before);
    let unsupported =
        macro_learn_owned_inner(&mut inner, "Lead", 2, "unison.width", &token).unwrap();
    assert_eq!(unsupported.result.status, MacroLearnStatus::Unsupported);
    assert_eq!(observe(&inner), before);
    let mut expected = inner.project.clone();
    let result = macro_learn_inner(&mut expected, "Lead", 2, "filter.cutoff", None).unwrap();
    let actual = macro_learn_owned_inner(&mut inner, "Lead", 2, "filter.cutoff", &token).unwrap();
    assert_eq!(actual.result.status, result.status);
    assert_eq!(actual.result.slot, result.slot);
    assert_eq!(inner.project, expected);
    assert_eq!(actual.result.status, MacroLearnStatus::Bound);
    let unbound = macro_learn_owned_inner(&mut inner, "Lead", 2, "filter.cutoff", &token).unwrap();
    assert_eq!(unbound.result.status, MacroLearnStatus::Unbound);
    assert!(inner.undo.is_owned_gesture_active(&token));
    assert!(inner.undo.end_owned_gesture(&token));
}

fn topology_app() -> tauri::App<tauri::test::MockRuntime> {
    use tauri::test::{mock_builder, mock_context, noop_assets};
    mock_builder()
        .manage(loaded())
        .build(mock_context(noop_assets()))
        .unwrap()
}

fn assert_topology_refuses_stale_owners(
    inner: &mut AppStateInner,
    mutation: &UndoGestureToken,
    automation: &UndoGestureToken,
) {
    let before = observe(inner);
    assert!(!inner.undo.is_owned_gesture_active(mutation));
    assert!(!inner.undo.is_owned_gesture_active(automation));
    assert!(
        set_preset_param_owned_inner(
            inner,
            "Lead".into(),
            "output.gain".into(),
            Some("0.1".into()),
            mutation
        )
        .is_err()
    );
    assert!(randomize_preset_owned_inner(inner, "Lead", 42, mutation).is_err());
    assert!(macro_learn_owned_inner(inner, "Lead", 1, "filter.cutoff", mutation).is_err());
    assert!(point(inner, automation, 0.1).is_err());
    assert!(!end(inner, automation).unwrap().closed);
    assert!(!inner.undo.end_owned_gesture(mutation));
    assert_eq!(observe(inner), before);
}

#[test]
fn owned_synth_topology_actual_preset_remove_and_readd_expire_old_owners() {
    use tauri::Manager;
    let app = topology_app();
    let state = app.state::<AppState>();
    let (mutation, automation) = {
        let mut inner = state.lock_inner();
        (
            inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap(),
            begin(&mut inner),
        )
    };
    remove_preset("Lead".into(), app.state()).unwrap();
    assert!(!state.lock_inner().undo.is_owned_gesture_active(&mutation));
    add_preset(
        "Lead".into(),
        "builtin".into(),
        None,
        Some("builtin.open_synth".into()),
        None,
        None,
        app.state(),
    )
    .unwrap();
    // Restore the same name and assignment through the ordinary DSL command.
    crate::commands::update_dsl(source("First", 0.9), app.state()).unwrap();
    let mut inner = state.lock_inner();
    assert_topology_refuses_stale_owners(&mut inner, &mutation, &automation);
    let current = begin(&mut inner);
    point(&mut inner, &current, 0.7).unwrap();
    assert!(end(&mut inner, &current).unwrap().closed);
}

#[test]
fn owned_synth_topology_actual_track_remove_and_readd_preserve_legacy_depth() {
    use tauri::Manager;
    let app = topology_app();
    let state = app.state::<AppState>();
    let (mutation, automation) = {
        let mut inner = state.lock_inner();
        inner.undo.begin_gesture(UndoScope::Project);
        (
            inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap(),
            begin(&mut inner),
        )
    };
    crate::commands::remove_track(0, app.state()).unwrap();
    {
        let mut inner = state.lock_inner();
        crate::commands::add_track_checked(&mut inner, "Lead", "synth", "lead_saw", Some("Lead"))
            .unwrap();
        assert_topology_refuses_stale_owners(&mut inner, &mutation, &automation);
        assert!(
            inner.undo.is_in_gesture(),
            "structural edits retain legacy nesting"
        );
        inner.undo.end_gesture();
        assert!(!inner.undo.is_in_gesture());
    }
}

#[test]
fn owned_synth_topology_dsl_reorder_and_binding_changes_expire_old_owners() {
    let state = loaded();
    let mut inner = state.lock_inner();
    crate::commands::add_track_checked(&mut inner, "Other", "synth", "lead_saw", Some("Lead"))
        .unwrap();
    add_preset_inner(
        &mut inner,
        "OtherPreset".into(),
        "builtin".into(),
        None,
        Some("builtin.open_synth".into()),
        None,
        None,
    )
    .unwrap();
    for change in 0..5 {
        let mutation = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
        let automation = begin_owned_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            if change == 0 { 0 } else { 1 },
            "output.gain",
        )
        .unwrap();
        let mut candidate = inner.project.clone();
        match change {
            0 => candidate.tracks.swap(0, 1),
            1 => candidate.tracks[0].name = "Renamed".into(),
            2 => candidate.tracks[0].instrument = ast::InstrumentPath::new("synth", "lead_square"),
            3 => candidate.tracks[0].preset = None,
            _ => candidate.presets.swap(0, 1),
        }
        crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
        assert_topology_refuses_stale_owners(&mut inner, &mutation, &automation);
    }
}

#[test]
fn owned_synth_same_named_patch_replacement_and_inheritance_detachment_retain_owner() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let mut inherited = inner.project.clone();
    let mut parent = inherited.presets[0].clone();
    parent.name = "Parent".into();
    inherited.presets.insert(0, parent);
    inherited.presets[1].extends = Some("Parent".into());
    crate::commands::update_dsl_inner(oc_dsl::serialize(&inherited), &mut inner).unwrap();
    let token = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    randomize_preset_owned_inner(&mut inner, "Lead", 42, &token).unwrap();
    assert!(inner.project.presets[1].extends.is_none());
    assert!(inner.undo.is_owned_gesture_active(&token));
    macro_learn_owned_inner(&mut inner, "Lead", 1, "filter.cutoff", &token).unwrap();
    let mut replacement = inner.project.clone();
    replacement.presets[1].patch = Some("Replacement".into());
    replacement.presets[1].params = vec![("output.gain".into(), ast::ParamValue::Number(0.7))];
    crate::commands::update_dsl_inner(oc_dsl::serialize(&replacement), &mut inner).unwrap();
    assert!(inner.undo.is_owned_gesture_active(&token));
    assert!(inner.undo.end_owned_gesture(&token));
}

#[test]
fn owned_synth_points_survive_dsl_reorder_delete_and_binding_invalidation() {
    for structural in ["reorder", "delete", "binding"] {
        let state = loaded();
        let mut inner = state.lock_inner();
        let mut initial = inner.project.clone();
        let mut other = initial.tracks[0].clone();
        other.name = "Other".into();
        initial.tracks.push(other);
        crate::commands::update_dsl_inner(oc_dsl::serialize(&initial), &mut inner).unwrap();
        inner.undo.clear();
        let original = inner.project.clone();
        let token = begin(&mut inner);
        point(&mut inner, &token, 0.7).unwrap();
        let recorded = inner.project.clone();
        assert_eq!(inner.undo.undo.len(), 1);
        let mut candidate = recorded.clone();
        match structural {
            "reorder" => candidate.tracks.swap(0, 1),
            "delete" => {
                candidate.tracks.remove(0);
            }
            _ => candidate.tracks[0].preset = None,
        }
        crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
        let changed = inner.project.clone();
        assert!(!inner.undo.is_owned_gesture_active(&token));
        assert_eq!(inner.undo.undo.len(), 2, "{structural}");
        let before = observe(&inner);
        assert!(!end(&mut inner, &token).unwrap().closed);
        assert_eq!(observe(&inner), before);
        inner.undo_step().unwrap();
        assert_eq!(inner.project, recorded, "undo {structural}");
        inner.undo_step().unwrap();
        assert_eq!(inner.project, original, "undo recording");
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, recorded);
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, changed);
    }
}

#[test]
fn owned_synth_concurrent_points_and_ordinary_sibling_do_not_replay_begin_snapshot() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    let first = begin(&mut inner);
    point(&mut inner, &first, 0.6).unwrap();
    let second =
        begin_owned_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").unwrap();
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Project).unwrap();
    let old = inner.project.clone();
    inner.project.name = Some("ordinary sibling".into());
    resync_with_undo(&mut inner, old, UndoEdit::new("name", UndoScope::Project)).unwrap();
    update_owned_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.cutoff",
        OwnedSynthPoint {
            normalized: 0.5,
            physical: Some(1200.0),
            position: ast::BarBeat::new(1, 2, 0),
            token: &second,
        },
    )
    .unwrap();
    let final_project = inner.project.clone();
    assert!(end(&mut inner, &first).unwrap().closed);
    assert!(
        end_owned_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff", &second)
            .unwrap()
            .closed
    );
    assert!(inner.undo.is_owned_gesture_active(&ordinary));
    assert!(inner.undo.end_owned_gesture(&ordinary));
    assert_eq!(inner.undo.undo.len(), 1);
    inner.undo_step().unwrap();
    assert_eq!(inner.project, original);
    redo_recorded_history(&mut inner);
    assert_eq!(inner.project, final_project);
}

#[test]
fn owned_synth_history_encoding_failure_does_not_publish_physical_or_lane_values() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin(&mut inner);
    let before = observe(&inner);
    inner.set_history_encoding_failure_for_test(true);
    assert!(point(&mut inner, &token, 0.75).is_err());
    inner.set_history_encoding_failure_for_test(false);
    assert_eq!(observe(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&token));
}

fn redo_recorded_history(inner: &mut AppStateInner) {
    inner.undo.redo(&mut inner.project).unwrap();
    inner.resync_after_history();
}

#[test]
fn mixed_legacy_synth_then_owned_recording_survives_structural_and_late_close() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "output.gain").unwrap();
    update_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "output.gain",
        0.6,
        Some(0.6),
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap();
    let owned =
        begin_owned_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").unwrap();
    update_owned_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.cutoff",
        OwnedSynthPoint {
            normalized: 0.5,
            physical: Some(1200.0),
            position: ast::BarBeat::new(1, 2, 0),
            token: &owned,
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
    let before = observe(&inner);
    end_synth_param_gesture_inner(&mut inner, "Lead", "output.gain").unwrap();
    assert!(
        !end_owned_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff", &owned)
            .unwrap()
            .closed
    );
    assert_eq!(observe(&inner), before);
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
}
#[test]
fn legacy_synth_begin_and_failed_first_point_do_not_migrate_or_change_patch() {
    let state = loaded();
    let mut inner = state.lock_inner();
    inner.project.properties.automation_normalization = None;
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner.undo.begin_gesture(UndoScope::Project);
    let original = inner.project.clone();
    let dsl = inner.dsl_source.clone();
    let revision = inner.revision;
    begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "output.gain").unwrap();
    assert_eq!(inner.project, original);
    assert_eq!(inner.dsl_source, dsl);
    assert_eq!(inner.revision, revision);
    let before = observe(&inner);
    assert!(
        update_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "output.gain",
            0.7,
            Some(0.75),
            ast::BarBeat::new(1, 99, 0)
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(true);
    assert!(
        update_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "output.gain",
            0.7,
            Some(0.75),
            ast::BarBeat::new(1, 1, 0)
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(false);
    assert_eq!(observe(&inner), before);
    end_synth_param_gesture_inner(&mut inner, "Lead", "output.gain").unwrap();
    assert_eq!(inner.project, original);
    assert!(inner.undo.undo.is_empty());
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
}
