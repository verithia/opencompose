//! The automation read-out and the point edits the timeline row sends.

use super::support::test_state;
use crate::audio::PlaybackState;
use crate::commands::automation::{AutomationDomain, AutomationReadout, automation_value_at_inner};
use crate::commands::fx::{
    MUTED_LANE, add_automation_point_inner, apply_lane_point_curve, begin_effect_param_write_inner,
    end_effect_param_write_inner, move_automation_point_inner, remove_automation_point_inner,
    set_automation_lane_mute_inner, update_effect_param_write_inner,
};
use crate::state::AppStateInner;
use oc_dsl::ast;

/// A project at 120 bpm whose first track carries `automation` on `owner`.
fn playing(source: &str) -> AppStateInner {
    let mut inner = state(source);
    inner.audio.playback_state = PlaybackState::Playing;
    inner
}

/// The same project with the transport stopped.
fn state(source: &str) -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(source).expect("test project parses");
    inner.audio.sample_rate = 48_000;
    inner
}

fn readout(inner: &AppStateInner, target: &str, lane: &str, beat: f64) -> AutomationReadout {
    automation_value_at_inner(inner, target, lane, None, beat).expect("the read-out answers")
}

fn reason(readout: &AutomationReadout) -> &str {
    match readout {
        AutomationReadout::NotDriven { reason } => reason,
        other => panic!("expected a not-driven answer, got {other:?}"),
    }
}

fn driven(readout: &AutomationReadout) -> (f64, AutomationDomain, f64) {
    match readout {
        AutomationReadout::Driven {
            value,
            domain,
            smoothing_ms,
        } => (*value, *domain, *smoothing_ms),
        other => panic!("expected a driven answer, got {other:?}"),
    }
}

const EFFECT_LANE: &str = "gain.factor";

fn effect_project() -> String {
    "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 1:2:0=0.5}\n".into()
}

/// The lane encoding the synth and plugin panels write.
fn plugin_lane(param: &str) -> String {
    let mut lane = String::from("pluginparam_");
    for byte in param.bytes() {
        lane.push_str(&format!("{byte:02x}"));
    }
    lane
}

#[test]
fn an_effect_lane_answers_the_device_value_at_the_beat() {
    let inner = playing(&effect_project());
    // 120 bpm: a beat is a quarter note, and the lane ramps 1 -> 0.5.
    let (value, domain, smoothing) = driven(&readout(&inner, "track:0", EFFECT_LANE, 0.5));
    assert!((value - 0.75).abs() < 1e-12, "{value}");
    assert_eq!(domain, AutomationDomain::Device);
    assert_eq!(smoothing, 0.0);

    // Before the lane's first point nothing is written.
    let inner_moved =
        playing(&effect_project().replace("{1:1:0=1, 1:2:0=0.5}", "{1:3:0=1, 1:4:0=0.5}"));
    let answer = readout(&inner_moved, "track:0", EFFECT_LANE, 0.5);
    assert!(
        reason(&answer).contains("before the lane's first point"),
        "{answer:?}"
    );
}

#[test]
fn mapped_cutoff_edits_store_raw_anchors_and_show_hertz() {
    let mut inner = playing(
        "@project\n  tempo: 120\n@track tone | instrument: synth/lead\n  effects:\n    - dynamic_eq: band12_enabled=true\n  automation: fx1.band12_freq mapping(linear, scale=1000, offset=20) {1:1=0, 1:2=1}\n",
    );
    let lane = "fx1.band12_freq";
    let (value, domain, _) = driven(&readout(&inner, "track:0", lane, 0.5));
    assert_eq!(value, 520.0);
    assert_eq!(domain, AutomationDomain::Device);

    crate::commands::fx::apply_add_point(&mut inner.project, "track:0", lane, None, 0.5, 0.25)
        .unwrap();
    let (value, _, _) = driven(&readout(&inner, "track:0", lane, 0.5));
    assert_eq!(value, 270.0);
    let original = inner.project.tracks[0]
        .automation()
        .next()
        .unwrap()
        .points
        .clone();
    assert!(
        crate::commands::fx::apply_add_point(&mut inner.project, "track:0", lane, None, 0.75, -1.0)
            .is_err()
    );
    assert_eq!(
        inner.project.tracks[0].automation().next().unwrap().points,
        original
    );
}

#[test]
fn a_synth_lane_answers_the_normalized_position() {
    let inner = playing(&format!(
        "@project \"auto\"\n  tempo: 120\n@preset synth\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: synth, instrument: synth/lead\n  automation: {} {{1:1:0=0.2, 1:2:0=0.8}}\n",
        plugin_lane("filter.cutoff")
    ));
    let (value, domain, _) = driven(&readout(
        &inner,
        "track:0",
        &plugin_lane("filter.cutoff"),
        0.5,
    ));
    assert!((value - 0.5).abs() < 1e-12, "{value}");
    assert_eq!(domain, AutomationDomain::Normalized);
}

#[test]
fn a_hosted_plugin_lane_answers_the_normalized_position() {
    let inner = playing(&format!(
        "@project \"auto\"\n  tempo: 120\n@preset vst\n  source: vst3\n  plugin: \"vst3.test.fake\"\n@track lead | preset: vst, instrument: synth/lead\n  automation: {} {{1:1:0=0.25, 1:2:0=0.75}}\n",
        plugin_lane("Cutoff")
    ));
    let (value, domain, _) = driven(&readout(&inner, "track:0", &plugin_lane("Cutoff"), 0.5));
    assert!((value - 0.5).abs() < 1e-12, "{value}");
    assert_eq!(domain, AutomationDomain::Normalized);
}

#[test]
fn a_parameter_without_a_lane_is_not_driven() {
    let inner = playing(&effect_project());
    let answer = readout(&inner, "track:0", "gain.mix", 0.5);
    assert!(
        reason(&answer).contains("no automation lane targets 'gain.mix'"),
        "{answer:?}"
    );
}

#[test]
fn an_unknown_target_is_refused_in_a_sentence() {
    let inner = playing(&effect_project());
    let error = automation_value_at_inner(&inner, "channel:2", EFFECT_LANE, None, 0.5).unwrap_err();
    assert!(error.contains("Invalid target"), "{error}");
}

#[test]
fn a_stopped_transport_does_not_claim_a_driven_value() {
    let mut inner = playing(&effect_project());
    inner.audio.playback_state = PlaybackState::Stopped;
    let answer = readout(&inner, "track:0", EFFECT_LANE, 0.5);
    assert_eq!(reason(&answer), "the transport is stopped");
}

#[test]
fn a_muted_owner_does_not_claim_a_driven_value() {
    let inner = playing(
        "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  mix: mute=true\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 1:2:0=0.5}\n",
    );
    let answer = readout(&inner, "track:0", EFFECT_LANE, 0.5);
    assert_eq!(reason(&answer), "track 't' is muted");
}

#[test]
fn a_control_lane_is_not_a_panel_value() {
    let inner = playing(
        "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  automation: volume {1:1:0=0.2, 1:2:0=0.8}\n",
    );
    let answer = readout(&inner, "track:0", "volume", 0.5);
    assert!(reason(&answer).contains("MIDI controller"), "{answer:?}");
}

#[test]
fn a_bus_lane_answers_its_own_chain() {
    let inner = playing(
        "@project \"auto\"\n  tempo: 120\n@bus b\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=0.4, 1:2:0=0.8}\n",
    );
    let (value, domain, _) = driven(&readout(&inner, "bus:b", EFFECT_LANE, 0.5));
    assert!((value - 0.6).abs() < 1e-12, "{value}");
    assert_eq!(domain, AutomationDomain::Device);
}

#[test]
fn a_preset_chain_reports_that_it_has_no_lanes() {
    let inner = playing(&effect_project());
    let answer = readout(&inner, "preset:Lead", EFFECT_LANE, 0.5);
    assert!(reason(&answer).contains("preset chain"), "{answer:?}");
}

// ============================================================================
// Point editing on the timeline row
// ============================================================================

/// A clip on one effect parameter: `[1:1 - 2:1]` drives bars 1 only.
const CLIP: &str = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 2:1:0] {1:1:0=1, 1:3:0=0.5}\n";

/// Two clips on the same parameter, so the parameter alone is not a key.
const TWO_CLIPS: &str = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 2:1:0] {1:1:0=1, 1:3:0=0.5}\n  automation: gain.factor [3:1:0 - 4:1:0] {3:1:0=0.2, 3:3:0=0.8}\n";

fn lane(inner: &AppStateInner, index: usize) -> &ast::AutomationLane {
    inner.project.tracks[0].automation_lane(index).unwrap()
}

#[test]
fn adding_moving_and_removing_a_point_are_one_undo_entry_each() {
    let mut inner = state(&effect_project());

    // Add: the point lands on the notated grid at the asked position and value.
    add_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 4.0, 0.25).unwrap();
    assert_eq!(
        inner.undo.undo_entries().len(),
        1,
        "adding a point is one entry"
    );
    let points = &lane(&inner, 0).points;
    assert_eq!(points.len(), 3);
    assert_eq!(points[2].position, ast::BarBeat::new(2, 1, 0));
    assert_eq!(points[2].value, ast::ParamValue::Number(0.25));

    // Move: both axes, the same index, one more entry.
    move_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 2, 6.25, 0.75).unwrap();
    assert_eq!(
        inner.undo.undo_entries().len(),
        2,
        "moving a point is one entry"
    );
    let points = &lane(&inner, 0).points;
    assert_eq!(points[2].position, ast::BarBeat::new(2, 3, 1));
    assert_eq!(points[2].value, ast::ParamValue::Number(0.75));

    // Remove: one entry, and undo brings the point back.
    remove_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 1).unwrap();
    assert_eq!(
        inner.undo.undo_entries().len(),
        3,
        "removing a point is one entry"
    );
    assert_eq!(lane(&inner, 0).points.len(), 2);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(lane(&inner, 0).points.len(), 3);

    // The added point carries the unit the lane already uses.
    assert_eq!(
        lane(&inner, 0).points[2].value,
        ast::ParamValue::Number(0.75),
        "a number lane keeps bare numbers"
    );
}

#[test]
fn a_point_may_sit_outside_a_clip_span_because_only_writes_are_gated() {
    let mut inner = state(CLIP);
    // The clip writes bars 1 only; the move puts the point in bar 3.
    move_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 1, 8.0, 0.9).unwrap();
    assert_eq!(inner.undo.undo_entries().len(), 1);
    assert_eq!(
        lane(&inner, 0).points[1].position,
        ast::BarBeat::new(3, 1, 0)
    );

    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    let curve = |value: &ast::ParamValue| value.as_f64().ok_or_else(|| "not a number".to_string());
    assert_eq!(
        oc_dsl::automation::value_at(lane(&inner, 0), &clock, 48_000, 8.0, curve).unwrap(),
        None,
        "a point outside the span never writes there"
    );
    // Inside the span the moved point still shapes the last segment.
    assert!(
        oc_dsl::automation::value_at(lane(&inner, 0), &clock, 48_000, 1.0, curve)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_muted_lane_refuses_every_point_edit_with_its_reason() {
    let mut inner = state(&effect_project());
    set_automation_lane_mute_inner(&mut inner, "track:0", EFFECT_LANE, None, true).unwrap();
    assert!(lane(&inner, 0).muted);
    assert_eq!(inner.undo.undo_entries().len(), 1, "the mute is one entry");

    let added = add_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 4.0, 0.5);
    assert_eq!(added.unwrap_err(), MUTED_LANE);
    let moved = move_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 0, 4.0, 0.5);
    assert_eq!(moved.unwrap_err(), MUTED_LANE);
    let removed = remove_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, None, 0);
    assert_eq!(removed.unwrap_err(), MUTED_LANE);
    assert_eq!(
        inner.undo.undo_entries().len(),
        1,
        "a refused edit adds no history"
    );
    assert_eq!(lane(&inner, 0).points.len(), 2, "the points are untouched");
}

#[test]
fn two_clips_on_one_target_need_a_lane_index() {
    let mut inner = playing(TWO_CLIPS);

    // The read-out refuses the ambiguous key instead of picking a lane.
    let answer = readout(&inner, "track:0", EFFECT_LANE, 9.0);
    assert!(
        reason(&answer).contains("has 2 lanes (0, 1)"),
        "the reason names the ambiguity: {answer:?}"
    );
    let indexed = automation_value_at_inner(&inner, "track:0", EFFECT_LANE, Some(1), 9.0)
        .expect("the read-out answers");
    assert!((driven(&indexed).0 - 0.5).abs() < 1e-12, "{indexed:?}");

    // A point edit without an index is refused with the same sentence.
    let refused = apply_lane_point_curve(
        &mut inner.project,
        "track:0",
        EFFECT_LANE,
        None,
        0,
        "smooth",
        Some(0.4),
    );
    let error = refused.unwrap_err();
    assert!(error.contains("has 2 lanes (0, 1)"), "{error}");
    assert!(error.contains("pass a lane index"), "{error}");
    assert_eq!(
        lane(&inner, 0).points[0].curve,
        ast::Curve::Linear,
        "a refused edit changes nothing"
    );

    // With an index the edit lands on exactly that lane.
    apply_lane_point_curve(
        &mut inner.project,
        "track:0",
        EFFECT_LANE,
        Some(1),
        0,
        "smooth",
        Some(0.4),
    )
    .unwrap();
    assert_eq!(lane(&inner, 1).points[0].curve, ast::Curve::Smooth);
    assert_eq!(lane(&inner, 1).points[0].tension, Some(0.4));
    assert_eq!(lane(&inner, 0).points[0].curve, ast::Curve::Linear);

    // An index that names another lane, or no lane, is refused.
    let error = apply_lane_point_curve(
        &mut inner.project,
        "track:0",
        EFFECT_LANE,
        Some(2),
        0,
        "linear",
        None,
    )
    .unwrap_err();
    assert!(error.contains("out of range"), "{error}");
    let error =
        remove_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, Some(9), 0).unwrap_err();
    assert!(error.contains("out of range"), "{error}");

    // The indexed mute only silences the lane it names.
    set_automation_lane_mute_inner(&mut inner, "track:0", EFFECT_LANE, Some(1), true).unwrap();
    assert!(lane(&inner, 1).muted);
    assert!(!lane(&inner, 0).muted);
    let answer = readout(&inner, "track:0", EFFECT_LANE, 0.5);
    assert!(
        reason(&answer).contains("has 2 lanes (0, 1)"),
        "the whole-timeline lane stays ambiguous: {answer:?}"
    );
}

// ── Effect-parameter writes ────────────────────────────────────────────────

/// A chain whose first entry is a built-in device, so a write can resolve it.
fn write_project() -> String {
    "@project \"write\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB, ratio=4\n".into()
}

fn written_lane(inner: &AppStateInner) -> &ast::AutomationLane {
    inner.project.tracks[0]
        .automation()
        .find(|lane| lane.param == "fx1.threshold")
        .expect("the write created the lane")
}

#[test]
fn an_effect_write_creates_the_lane_and_records_one_undo_entry() {
    let mut inner = state(&write_project());
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();
    // A recorded drag: three samples on the grid, the last one holding.
    for (position, value) in [
        (ast::BarBeat::new(1, 1, 0), "-18dB"),
        (ast::BarBeat::new(1, 1, 1), "-12dB"),
        (ast::BarBeat::new(1, 2, 0), "-6dB"),
    ] {
        update_effect_param_write_inner(&mut inner, "track:0", 0, "threshold", position, value)
            .unwrap();
    }
    end_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();

    let lane = written_lane(&inner);
    assert_eq!(lane.points.len(), 3, "one point per sampled grid cell");
    assert!(matches!(
        lane.points[2].value,
        ast::ParamValue::Db(value) if (value + 6.0).abs() < 1e-9
    ));
    // The chain entry keeps the written value, so the panel and the render agree.
    let effect = &inner.project.tracks[0].effects[0];
    assert!(effect.params.iter().any(|(name, value)| {
        name == "threshold"
            && matches!(value, ast::ParamValue::Db(number) if (number + 6.0).abs() < 1e-9)
    }));

    assert_eq!(
        inner.undo.undo_entries().len(),
        1,
        "one drag is one undo entry"
    );
    assert_eq!(inner.undo.undo_entries()[0].tag, "write automation");
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("one undo entry");
    assert!(project.tracks[0].automation_count() == 0);
}

#[test]
fn a_write_on_an_existing_lane_replaces_the_point_at_one_position() {
    let mut inner = state(
        "@project \"write\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB, ratio=4\n  automation: fx1.threshold {1:1:0=-18dB, 1:3:0=-6dB}\n",
    );
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();
    update_effect_param_write_inner(
        &mut inner,
        "track:0",
        0,
        "threshold",
        ast::BarBeat::new(1, 1, 0),
        "-24dB",
    )
    .unwrap();
    end_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();

    let lane = written_lane(&inner);
    assert_eq!(lane.points.len(), 2, "the first position was replaced");
    assert!(matches!(
        lane.points[0].value,
        ast::ParamValue::Db(value) if (value + 24.0).abs() < 1e-9
    ));
    assert_eq!(inner.undo.undo_entries().len(), 1);
}

#[test]
fn an_unknown_effect_parameter_is_refused_before_any_point() {
    let mut inner = state(&write_project());
    let error = begin_effect_param_write_inner(&mut inner, "track:0", 0, "treshold").unwrap_err();
    assert!(error.contains("has no parameter 'treshold'"), "{error}");
    assert!(error.contains("expected one of"), "{error}");
    assert!(inner.project.tracks[0].automation_count() == 0);
    assert!(
        inner.undo.undo_entries().is_empty(),
        "a refusal records nothing"
    );

    // A value outside the device range is refused too, and keeps no point.
    begin_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();
    let error = update_effect_param_write_inner(
        &mut inner,
        "track:0",
        0,
        "threshold",
        ast::BarBeat::new(1, 1, 0),
        "6dB",
    )
    .unwrap_err();
    assert!(error.contains("outside"), "{error}");
    end_effect_param_write_inner(&mut inner, "track:0", 0, "threshold").unwrap();
    assert!(inner.project.tracks[0].automation_count() == 0);

    // A chain entry that is a plugin has no device schema to validate against.
    let mut plugin = state(
        "@project \"write\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - plugin:\"vst3.fixture\"\n",
    );
    let error = begin_effect_param_write_inner(&mut plugin, "track:0", 0, "Mix").unwrap_err();
    assert!(error.contains("use its own panel"), "{error}");
}

#[test]
fn a_point_outside_the_device_range_is_refused_with_the_range() {
    const RANGED: &str = "@project \"range\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB, ratio=4\n  automation: compression.threshold {1:1:0=-18dB, 1:3:0=-24dB}\n";
    let mut inner = state(RANGED);

    // The device takes -60..0 dB; a move to +9 dB names the range and writes
    // nothing.
    let error = move_automation_point_inner(
        &mut inner,
        "track:0",
        "compression.threshold",
        None,
        1,
        6.0,
        9.0,
    )
    .unwrap_err();
    assert!(error.contains("outside -60..0"), "{error}");
    assert_eq!(
        inner.undo.undo_entries().len(),
        0,
        "a refusal adds no history"
    );
    assert_eq!(
        lane(&inner, 0).points[1].value,
        ast::ParamValue::Db(-24.0),
        "the refused move changed the point"
    );

    // An added point is checked the same way.
    let error = add_automation_point_inner(
        &mut inner,
        "track:0",
        "compression.threshold",
        None,
        4.0,
        9.0,
    )
    .unwrap_err();
    assert!(error.contains("outside -60..0"), "{error}");
    assert_eq!(
        lane(&inner, 0).points.len(),
        2,
        "the refused add inserted a point"
    );

    // A value inside the range still lands, in the lane's own unit.
    add_automation_point_inner(
        &mut inner,
        "track:0",
        "compression.threshold",
        None,
        4.0,
        -6.0,
    )
    .unwrap();
    assert_eq!(inner.undo.undo_entries().len(), 1);
    assert_eq!(
        lane(&inner, 0).points[2].value,
        ast::ParamValue::Db(-6.0),
        "a dB lane keeps its unit"
    );
}

#[test]
fn exponential_coefficient_edit_rejects_invalid_input_without_losing_previous_shape() {
    let mut project=oc_dsl::parse("@project\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1=0 curve=exponential curvature=-4, 2:1=1}\n").unwrap();
    let before = project.clone();
    assert!(
        crate::commands::fx::apply_lane_point_curve_with_curvature(
            &mut project,
            "track:0",
            "gain.factor",
            Some(0),
            0,
            crate::commands::fx::PointCurveShape {
                curve: ast::Curve::Exponential,
                tension: None,
                curvature: Some(f64::NAN),
            }
        )
        .is_err()
    );
    assert_eq!(project, before);
    crate::commands::fx::apply_lane_point_curve_with_curvature(
        &mut project,
        "track:0",
        "gain.factor",
        Some(0),
        0,
        crate::commands::fx::PointCurveShape {
            curve: ast::Curve::Exponential,
            tension: None,
            curvature: Some(3.0),
        },
    )
    .unwrap();
    assert_eq!(
        project.tracks[0].automation().next().unwrap().points[0].curvature,
        Some(3.0)
    );
    crate::commands::fx::apply_lane_point_curve(
        &mut project,
        "track:0",
        "gain.factor",
        Some(0),
        0,
        "linear",
        None,
    )
    .unwrap();
    assert_eq!(
        project.tracks[0].automation().next().unwrap().points[0].curvature,
        None
    );
}

const HIGH_PPQ: &str = "@project\n  time_signature: 7/8\n  time_sig_map:\n    3:1 = 3/4\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor [ppq=96000] {1:1=0, 1:1:24000=0.25 curve=exponential curvature=-4, 1:2=0.5, 1:2:24000=0.75}\n";

#[test]
fn high_ppq_point_add_orders_on_ticks_and_move_preserves_original_index_across_meter() {
    let mut inner = state(HIGH_PPQ);
    let before = inner.project.clone();
    add_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, Some(0), 0.625, 0.6).unwrap();
    let points = &lane(&inner, 0).points;
    assert_eq!(points[3].position, ast::BarBeat::new(1, 2, 12_000));
    assert_eq!(
        points[1],
        before.tracks[0].automation().next().unwrap().points[1]
    );
    assert_eq!(inner.undo.undo_entries().len(), 1);

    let before_move = inner.project.clone();
    move_automation_point_inner(
        &mut inner,
        "track:0",
        EFFECT_LANE,
        Some(0),
        1,
        7.0 + 48_001.0 / 96_000.0,
        0.4,
    )
    .unwrap();
    let points = &lane(&inner, 0).points;
    assert_eq!(points[1].position, ast::BarBeat::new(3, 1, 48_001));
    assert_eq!(points[1].curvature, Some(-4.0));
    for index in [0, 2, 3, 4] {
        assert_eq!(
            points[index],
            before_move.tracks[0].automation().next().unwrap().points[index]
        );
    }
    assert_eq!(inner.undo.undo_entries().len(), 2);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before_move);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
}

#[test]
fn high_ppq_point_refusals_keep_project_and_undo_atomic() {
    for add in [true, false] {
        let mut inner = state(HIGH_PPQ);
        let before = inner.project.clone();
        let quarter = 1.0 / (96_000.0 * 3.0);
        let result = if add {
            add_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, Some(0), quarter, 0.5)
        } else {
            move_automation_point_inner(
                &mut inner,
                "track:0",
                EFFECT_LANE,
                Some(0),
                1,
                quarter,
                0.5,
            )
        };
        assert!(result.is_err());
        assert_eq!(inner.project, before);
        assert!(inner.undo.undo_entries().is_empty());
    }
}

#[test]
fn add_after_cross_neighbor_move_keeps_original_relative_order_and_jump_right_limit() {
    let mut inner = state(
        "@project\n  tempo: 120\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor discontinuities {1:1=0, 1:2=0.25, 1:3=0.5, 2:1=1}\n",
    );
    // The point keeps its identity while moving beyond its original neighbour.
    move_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, Some(0), 1, 3.0, 0.25).unwrap();
    let moved = lane(&inner, 0).points.clone();
    assert_eq!(moved[1].position, ast::BarBeat::new(1, 4, 0));
    assert_eq!(moved[2].position, ast::BarBeat::new(1, 3, 0));

    add_automation_point_inner(&mut inner, "track:0", EFFECT_LANE, Some(0), 3.0, 0.9).unwrap();
    let updated = lane(&inner, 0);
    assert_eq!(updated.points[1].position, ast::BarBeat::new(1, 4, 0));
    assert_eq!(updated.points[1].value, ast::ParamValue::Number(0.9));
    let original_order: Vec<_> = updated
        .points
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, point)| point.clone())
        .collect();
    assert_eq!(original_order, moved);

    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    let curve = oc_dsl::automation::LaneCurve::new(updated, &clock, 100, |value| {
        value.as_f64().ok_or_else(|| "not numeric".to_string())
    })
    .unwrap();
    // At 120 BPM/100 Hz, quarter3 is sample150: the new knot is the left
    // limit, while the authored coincident knot retains the rightmost write.
    assert!((curve.envelope_value(150.0 - 1e-6).unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(curve.envelope_value(150.0), Some(0.25));
    assert_eq!(inner.undo.undo_entries().len(), 2);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(lane(&inner, 0).points, moved);
}
