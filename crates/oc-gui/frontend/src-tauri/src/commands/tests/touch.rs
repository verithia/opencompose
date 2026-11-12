//! Live touch commands: only lane-driven parameters are taken over, and only
//! while the transport is streaming.

use super::support::{make_track, test_state};
use crate::audio::PlaybackState;
use crate::commands::touch::{
    begin_effect_param_touch_inner, end_effect_param_touch_inner, update_effect_param_touch_inner,
};
use crate::state::AppStateInner;
use oc_dsl::ast::{
    AutomationLane, AutomationPoint, BarBeat, Curve, Effect, EffectType, ParamValue,
};

fn touch_inner() -> AppStateInner {
    let mut inner = test_state();
    let mut track = make_track("Lead");
    track.effects = vec![Effect {
        effect_type: EffectType::Gain,
        params: vec![("factor".into(), ParamValue::Number(1.0))],
        bypass: false,
        sidechain_source: None,
    }];
    track.set_automation_lanes(vec![AutomationLane {
        param: "gain.factor".into(),
        ppq: 0,
        smoothing_ms: 0.0,
        points: vec![
            AutomationPoint {
                position: BarBeat::new(1, 1, 0),
                value: ParamValue::Number(1.0),
                curve: Curve::Linear,
                tension: None,
            },
            AutomationPoint {
                position: BarBeat::new(1, 2, 0),
                value: ParamValue::Number(0.25),
                curve: Curve::Linear,
                tension: None,
            },
        ],
        ..Default::default()
    }]);
    inner.project.tracks = vec![track];
    inner.audio.playback_state = PlaybackState::Playing;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    inner
}

#[test]
fn a_touch_takes_over_an_automated_effect_parameter() {
    let mut inner = touch_inner();
    assert_eq!(
        begin_effect_param_touch_inner(&mut inner, "track:0", 0, "factor", 0.1),
        Ok(true)
    );
    let key = oc_engine::touch::effect_key("track:0", 0, "factor");
    let registry = inner.audio.render_thread.as_ref().unwrap().touch();
    assert_eq!(registry.status(&key).map(|status| status.hand), Some(0.1));
    assert!(registry.is_held(&key));

    update_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.25);
    assert_eq!(registry.status(&key).map(|status| status.hand), Some(0.25));

    end_effect_param_touch_inner(&inner, "track:0", 0, "factor");
    assert!(!registry.is_held(&key));
}

#[test]
fn a_parameter_without_a_lane_keeps_the_ordinary_edit_path() {
    let mut inner = touch_inner();
    assert_eq!(
        begin_effect_param_touch_inner(&mut inner, "track:0", 0, "nope", 0.1),
        Ok(false)
    );
    let key = oc_engine::touch::effect_key("track:0", 0, "nope");
    assert!(
        !inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .touch()
            .is_held(&key)
    );
}

#[test]
fn a_stopped_transport_never_takes_a_touch() {
    let mut inner = touch_inner();
    inner.audio.playback_state = PlaybackState::Stopped;
    assert_eq!(
        begin_effect_param_touch_inner(&mut inner, "track:0", 0, "factor", 0.1),
        Ok(false)
    );
    let key = oc_engine::touch::effect_key("track:0", 0, "factor");
    assert!(
        !inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .touch()
            .is_held(&key)
    );
}

use crate::commands::touch::{
    EffectTouchToken, begin_guarded_effect_param_touch_inner, end_owned_effect_param_touch_inner,
    update_owned_effect_param_touch_inner,
};

fn owned_begin(inner: &AppStateInner, value: f64) -> EffectTouchToken {
    begin_guarded_effect_param_touch_inner(
        inner,
        "track:0",
        0,
        "factor",
        value,
        inner.undo.parameter_target_stamp().unwrap().clone(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn owned_touch_rejects_delayed_begin_and_exact_stale_updates() {
    let mut inner = touch_inner();
    let capture = inner.undo.parameter_target_stamp().unwrap().clone();
    let old = owned_begin(&inner, 0.1);
    inner.invalidate_parameter_owners();
    let registry = inner.audio.render_thread.as_ref().unwrap().touch();
    let key = oc_engine::touch::effect_key("track:0", 0, "factor");
    assert!(!registry.is_held(&key));
    assert!(
        begin_guarded_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, capture)
            .is_err()
    );
    let fresh = owned_begin(&inner, 0.3);
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &old).unwrap()
    );
    assert!(!end_owned_effect_param_touch_inner(
        &inner, "track:0", 0, "factor", &old
    ));
    assert_eq!(registry.status(&key).unwrap().hand, 0.3);
    assert!(
        update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.4, &fresh).unwrap()
    );
    assert!(end_owned_effect_param_touch_inner(
        &inner, "track:0", 0, "factor", &fresh
    ));
    assert!(!end_owned_effect_param_touch_inner(
        &inner, "track:0", 0, "factor", &fresh
    ));
}

#[test]
fn owned_touch_retires_on_actual_identical_effect_move_history_and_transport() {
    let mut inner = touch_inner();
    let duplicate = inner.project.tracks[0].effects[0].clone();
    inner.project.tracks[0].effects.push(duplicate);
    let old = owned_begin(&inner, 0.1);
    crate::commands::fx::move_effect_inner(&mut inner, "track:0", 0, 0).unwrap();
    assert!(
        update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.2, &old).unwrap()
    );
    assert!(crate::commands::fx::move_effect_inner(&mut inner, "track:0", 0, 9).is_err());
    assert!(
        update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.3, &old).unwrap()
    );
    crate::commands::fx::move_effect_inner(&mut inner, "track:0", 0, 1).unwrap();
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &old).unwrap()
    );
    let fresh = owned_begin(&inner, 0.4);
    inner.undo_step().unwrap();
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &fresh)
            .unwrap()
    );
    let paused = owned_begin(&inner, 0.5);
    inner.audio.pause();
    inner.audio.playback_state = PlaybackState::Playing;
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &paused)
            .unwrap()
    );
    let stopped = owned_begin(&inner, 0.6);
    inner.audio.stop();
    inner.audio.playback_state = PlaybackState::Playing;
    assert!(!end_owned_effect_param_touch_inner(
        &inner, "track:0", 0, "factor", &stopped
    ));
}

#[test]
fn owned_touch_stamp_defense_and_legacy_refusal_preserve_current_owner() {
    let mut inner = touch_inner();
    let token = owned_begin(&inner, 0.2);
    assert!(!begin_effect_param_touch_inner(&mut inner, "track:0", 0, "factor", 0.8).unwrap());
    update_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.8);
    end_effect_param_touch_inner(&inner, "track:0", 0, "factor");
    let registry = inner.audio.render_thread.as_ref().unwrap().touch();
    let key = oc_engine::touch::effect_key("track:0", 0, "factor");
    assert_eq!(registry.status(&key).unwrap().hand, 0.2);
    assert!(registry.is_held(&key));
    // Stamp checking independently protects future callers lacking a retirement hook.
    inner.undo.invalidate_owned_gestures();
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &token)
            .unwrap()
    );
    assert!(!registry.is_held(&key));
    let wire = serde_json::to_string(&token).unwrap();
    let restored: EffectTouchToken = serde_json::from_str(&wire).unwrap();
    assert!(!end_owned_effect_param_touch_inner(
        &inner, "track:0", 0, "factor", &restored
    ));
}

#[test]
fn owned_touch_document_replacement_and_rollback_retire_native_override() {
    let mut inner = touch_inner();
    let registry = inner.audio.render_thread.as_ref().unwrap().touch();
    let key = oc_engine::touch::effect_key("track:0", 0, "factor");
    inner
        .begin_transaction("touch rollback".into(), "touch test".into())
        .unwrap();
    let rollback = owned_begin(&inner, 0.2);
    inner.rollback_transaction().unwrap();
    assert!(!registry.is_held(&key));
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &rollback)
            .unwrap()
    );
    let replacement = owned_begin(&inner, 0.3);
    let same_names = inner.project.clone();
    crate::commands::project::replace_project(&mut inner, same_names, None).unwrap();
    assert!(!registry.is_held(&key));
    assert!(!end_owned_effect_param_touch_inner(
        &inner,
        "track:0",
        0,
        "factor",
        &replacement
    ));
}

#[test]
fn owned_touch_actual_track_remove_expires_the_index_bound_override() {
    use tauri::{
        Manager,
        test::{mock_builder, mock_context, noop_assets},
    };
    let state = crate::state::AppState::new();
    *state.lock_inner() = touch_inner();
    let app = mock_builder()
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap();
    let state = app.state::<crate::state::AppState>();
    let (token, registry) = {
        let mut inner = state.lock_inner();
        let mut second = inner.project.tracks[0].clone();
        second.name = "Second".into();
        inner.project.tracks.push(second);
        (
            owned_begin(&inner, 0.2),
            inner.audio.render_thread.as_ref().unwrap().touch(),
        )
    };
    crate::commands::remove_track(0, app.state()).unwrap();
    let inner = state.lock_inner();
    assert_eq!(inner.project.tracks[0].name, "Second");
    assert!(!registry.is_held(&oc_engine::touch::effect_key("track:0", 0, "factor")));
    assert!(
        !update_owned_effect_param_touch_inner(&inner, "track:0", 0, "factor", 0.9, &token)
            .unwrap()
    );
}
