use super::*;

fn source(title: &str) -> String {
    format!(
        "@project \"{title}\"\n@track Lead | instrument: synth/lead_saw\n  effects:\n    - gain: factor=0.5\n    - gain: factor=0.8\n"
    )
}
fn loaded() -> AppState {
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(), &source("First"), None).unwrap();
    state
}
fn observed(inner: &AppStateInner) -> serde_json::Value {
    serde_json::json!({"project":inner.project,"dsl":inner.dsl_source,"revision":inner.revision,
        "epoch":inner.content_epoch,"render":inner.render_generation,"dirty":inner.dirty,
        "undo":inner.undo.undo.len(),"redo":inner.undo.redo.len(),"sessions":inner.effect_write_gestures.len()})
}
fn begin(inner: &mut AppStateInner) -> UndoGestureToken {
    begin_owned_effect_param_write_inner(inner, "track:0", 0, "factor").unwrap()
}
fn point(
    inner: &mut AppStateInner,
    token: &UndoGestureToken,
    position: ast::BarBeat,
) -> Result<(), String> {
    update_owned_effect_param_write_inner(inner, "track:0", 0, "factor", position, "0.7", token)
}
fn end(
    inner: &mut AppStateInner,
    token: &UndoGestureToken,
) -> Result<OwnedEffectGestureEnd, String> {
    end_owned_effect_param_write_inner(inner, "track:0", 0, "factor", token)
}
#[test]
fn owned_effect_replaced_document_refuses_mutation_point_and_stale_close() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let stale = begin(&mut inner);
    crate::commands::open_project(&mut inner, &source("Replacement"), None).unwrap();
    let current = begin(&mut inner);
    let before = observed(&inner);
    assert!(
        set_effect_params_owned_inner(
            &mut inner,
            "track:0",
            0,
            &[("factor".into(), "0.1".into())],
            &stale
        )
        .is_err()
    );
    assert!(point(&mut inner, &stale, ast::BarBeat::new(1, 1, 0)).is_err());
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert_eq!(observed(&inner), before);
    assert!(
        update_effect_param_write_inner(
            &mut inner,
            "track:0",
            0,
            "factor",
            ast::BarBeat::new(1, 1, 0),
            "0.1"
        )
        .is_err()
    );
    assert!(end_effect_param_write_inner(&mut inner, "track:0", 0, "factor").is_err());
    assert_eq!(observed(&inner), before);
    point(&mut inner, &current, ast::BarBeat::new(1, 1, 0)).unwrap();
    assert!(end(&mut inner, &current).unwrap().closed);
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(!end(&mut inner, &current).unwrap().closed);
}
#[test]
fn owned_effect_same_type_move_and_remove_readd_expire_exact_target_owner() {
    let state = loaded();
    let mut inner = state.lock_inner();
    inner.undo.begin_gesture(UndoScope::Project);
    let stale = begin(&mut inner);
    move_effect_inner(&mut inner, "track:0", 0, 1).unwrap();
    assert!(!inner.undo.is_owned_gesture_active(&stale));
    let current = begin(&mut inner);
    let before = observed(&inner);
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert_eq!(observed(&inner), before);
    remove_effect_inner(&mut inner, "track:0", 0).unwrap();
    add_effect_inner(&mut inner, "track:0", "gain").unwrap();
    assert!(point(&mut inner, &current, ast::BarBeat::new(1, 1, 0)).is_err());
    assert!(!end(&mut inner, &current).unwrap().closed);
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
}
#[test]
fn owned_effect_failed_clock_batch_and_absent_master_leave_document_unchanged() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let owner = begin(&mut inner);
    let before = observed(&inner);
    assert!(point(&mut inner, &owner, ast::BarBeat::new(1, 99, 0)).is_err());
    assert!(
        set_effect_params_owned_inner(
            &mut inner,
            "track:0",
            0,
            &[
                ("factor".into(), "0.9".into()),
                ("unknown".into(), "1".into())
            ],
            &owner
        )
        .is_err()
    );
    assert!(
        set_effect_params_owned_inner(
            &mut inner,
            "master",
            0,
            &[("factor".into(), "0.9".into())],
            &owner
        )
        .is_err()
    );
    assert!(remove_effect_inner(&mut inner, "master", 0).is_err());
    assert!(move_effect_inner(&mut inner, "master", 0, 1).is_err());
    assert_eq!(observed(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&owner));
    assert!(end(&mut inner, &owner).unwrap().closed);
    assert_eq!(inner.undo.undo.len(), 0);
}
#[test]
fn owned_effect_history_restore_expires_owner_and_valid_batch_preserves_it() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    set_effect_params_owned_inner(
        &mut inner,
        "track:0",
        0,
        &[("factor".into(), "0.9".into())],
        &token,
    )
    .unwrap();
    assert!(inner.undo.is_owned_gesture_active(&token));
    inner.undo.end_owned_gesture(&token);
    let stale = begin(&mut inner);
    inner.undo_step().unwrap();
    let before = observed(&inner);
    assert!(point(&mut inner, &stale, ast::BarBeat::new(1, 1, 0)).is_err());
    assert!(!end(&mut inner, &stale).unwrap().closed);
    assert_eq!(observed(&inner), before);
}

#[test]
fn owned_effect_structural_dsl_publication_succeeds_while_expiring_old_membership() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let owner = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    let mut candidate = inner.project.clone();
    candidate.presets[0].effects.push(ast::Effect {
        effect_type: ast::EffectType::Reverb,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    });
    let (published, _) =
        crate::commands::update_dsl_inner(oc_dsl::serialize(&candidate), &mut inner).unwrap();
    assert_eq!(published.project.presets[0].effects.len(), 1);
    assert!(!inner.undo.is_owned_gesture_active(&owner));
    assert!(!inner.undo.end_owned_gesture(&owner));
    assert_eq!(inner.project, published.project);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn owned_effect_points_survive_structural_invalidation_and_stale_close_through_history() {
    for structural in ["move", "remove", "add"] {
        let state = loaded();
        let mut inner = state.lock_inner();
        let original = inner.project.clone();
        let token = begin(&mut inner);
        point(&mut inner, &token, ast::BarBeat::new(1, 1, 0)).unwrap();
        point(&mut inner, &token, ast::BarBeat::new(1, 2, 0)).unwrap();
        let recorded = inner.project.clone();
        assert_eq!(
            inner.undo.undo.len(),
            1,
            "points checkpoint as one owned entry"
        );
        match structural {
            "move" => move_effect_inner(&mut inner, "track:0", 0, 1).unwrap(),
            "remove" => remove_effect_inner(&mut inner, "track:0", 0).unwrap(),
            _ => add_effect_inner(&mut inner, "track:0", "gain").unwrap(),
        }
        let changed = inner.project.clone();
        assert_eq!(inner.undo.undo.len(), 2, "{structural}");
        let before = observed(&inner);
        assert!(!end(&mut inner, &token).unwrap().closed);
        assert_eq!(
            observed(&inner),
            before,
            "stale close must not restore a project"
        );
        inner.undo_step().unwrap();
        assert_eq!(inner.project, recorded, "undo {structural}");
        inner.undo_step().unwrap();
        assert_eq!(inner.project, original, "undo recording after {structural}");
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, recorded);
        redo_recorded_history(&mut inner);
        assert_eq!(inner.project, changed);
    }
}

#[test]
fn owned_effect_concurrent_recordings_and_ordinary_sibling_share_exact_original_baseline() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Project).unwrap();
    let first = begin(&mut inner);
    point(&mut inner, &first, ast::BarBeat::new(1, 1, 0)).unwrap();
    let second = begin_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor").unwrap();
    let old = inner.project.clone();
    inner.project.name = Some("ordinary sibling".into());
    resync_with_undo(&mut inner, old, UndoEdit::new("name", UndoScope::Project)).unwrap();
    update_owned_effect_param_write_inner(
        &mut inner,
        "track:0",
        1,
        "factor",
        ast::BarBeat::new(1, 2, 0),
        "0.9",
        &second,
    )
    .unwrap();
    let final_project = inner.project.clone();
    assert!(end(&mut inner, &first).unwrap().closed);
    assert!(inner.undo.is_owned_gesture_active(&second));
    assert!(
        end_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor", &second)
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
fn owned_effect_history_encoding_failure_does_not_publish_recorded_point() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let token = begin(&mut inner);
    let before = observed(&inner);
    inner.set_history_encoding_failure_for_test(true);
    assert!(point(&mut inner, &token, ast::BarBeat::new(1, 1, 0)).is_err());
    inner.set_history_encoding_failure_for_test(false);
    assert_eq!(observed(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&token));
}

fn redo_recorded_history(inner: &mut AppStateInner) {
    inner.undo.redo(&mut inner.project).unwrap();
    inner.resync_after_history();
}

#[test]
fn mixed_legacy_effect_then_owned_recording_survives_structural_and_late_close() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    update_effect_param_write_inner(
        &mut inner,
        "track:0",
        0,
        "factor",
        ast::BarBeat::new(1, 1, 0),
        "0.6",
    )
    .unwrap();
    let owned = begin_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor").unwrap();
    update_owned_effect_param_write_inner(
        &mut inner,
        "track:0",
        1,
        "factor",
        ast::BarBeat::new(1, 2, 0),
        "0.9",
        &owned,
    )
    .unwrap();
    let old = inner.project.clone();
    inner.project.name = Some("ordinary sibling".into());
    resync_with_undo(&mut inner, old, UndoEdit::new("name", UndoScope::Project)).unwrap();
    let recorded = inner.project.clone();
    assert_eq!(inner.undo.undo.len(), 1);
    move_effect_inner(&mut inner, "track:0", 0, 1).unwrap();
    let changed = inner.project.clone();
    assert_eq!(inner.undo.undo.len(), 2);
    inner.undo.begin_gesture(UndoScope::Project);
    let ordinary = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
    let before = observed(&inner);
    end_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    assert!(
        !end_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor", &owned)
            .unwrap()
            .closed
    );
    assert_eq!(observed(&inner), before);
    assert!(inner.undo.is_owned_gesture_active(&ordinary));
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
fn legacy_effect_begin_failed_point_and_close_preserve_outer_legacy_depth() {
    let state = loaded();
    let mut inner = state.lock_inner();
    inner.undo.begin_gesture(UndoScope::Project);
    let original = inner.project.clone();
    let dsl = inner.dsl_source.clone();
    let revision = inner.revision;
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    assert_eq!(inner.project, original);
    assert_eq!(inner.dsl_source, dsl);
    assert_eq!(inner.revision, revision);
    let before = observed(&inner);
    assert!(
        update_effect_param_write_inner(
            &mut inner,
            "track:0",
            0,
            "factor",
            ast::BarBeat::new(1, 99, 0),
            "0.7"
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(true);
    assert!(
        update_effect_param_write_inner(
            &mut inner,
            "track:0",
            0,
            "factor",
            ast::BarBeat::new(1, 1, 0),
            "0.7"
        )
        .is_err()
    );
    inner.set_history_encoding_failure_for_test(false);
    assert_eq!(observed(&inner), before);
    end_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
    assert_eq!(inner.project, original);
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn mixed_effect_recording_inside_explicit_outer_scope_keeps_its_merge_and_depth() {
    let state = loaded();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    inner.undo.begin_gesture(UndoScope::Project);
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    update_effect_param_write_inner(
        &mut inner,
        "track:0",
        0,
        "factor",
        ast::BarBeat::new(1, 1, 0),
        "0.6",
    )
    .unwrap();
    let owned = begin_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor").unwrap();
    update_owned_effect_param_write_inner(
        &mut inner,
        "track:0",
        1,
        "factor",
        ast::BarBeat::new(1, 2, 0),
        "0.9",
        &owned,
    )
    .unwrap();
    move_effect_inner(&mut inner, "track:0", 0, 1).unwrap();
    let changed = inner.project.clone();
    end_effect_param_write_inner(&mut inner, "track:0", 0, "factor").unwrap();
    assert!(
        !end_owned_effect_param_write_inner(&mut inner, "track:0", 1, "factor", &owned)
            .unwrap()
            .closed
    );
    assert!(inner.undo.is_in_gesture());
    inner.undo.end_gesture();
    assert!(!inner.undo.is_in_gesture());
    assert_eq!(
        inner.undo.undo.len(),
        1,
        "explicit outer scope still intentionally merges structural edit"
    );
    inner.undo_step().unwrap();
    assert_eq!(inner.project, original);
    redo_recorded_history(&mut inner);
    assert_eq!(inner.project, changed);
}
