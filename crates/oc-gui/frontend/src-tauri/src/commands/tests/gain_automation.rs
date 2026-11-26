use super::support::test_state;
use crate::commands::gain_automation::{GainAutomationEdit, replace_gain_automation_inner};
use crate::state::AppStateInner;
use oc_dsl::ast;

fn state() -> AppStateInner {
    let mut inner = test_state();
    inner.project=oc_dsl::parse("@project\n@track t | instrument: synth/keys\n  gain_automation: linear ppq=4 [1:1 - 2:1] {1:1=1,2:1=0.5}\n@bus b\n  gain_automation: linear ppq=4 [1:1 - 2:1] {1:1=1,2:1=0.5}\n@master\n  gain_automation: linear ppq=4 [1:1 - 2:1] {1:1=1,2:1=0.5}\n").unwrap();
    oc_dsl::instruments::apply_defaults(&mut inner.project);
    inner
}
fn envelope(inner: &AppStateInner, target: &str) -> ast::GainAutomation {
    match target {
        "track:0" => inner.project.tracks[0].gain_automation[0].clone(),
        "bus:b" => inner.project.buses[0].gain_automation[0].clone(),
        _ => inner.project.master.as_ref().unwrap().gain_automation[0].clone(),
    }
}
fn request(inner: &AppStateInner, target: &str) -> GainAutomationEdit {
    let expected = envelope(inner, target);
    GainAutomationEdit {
        target: target.into(),
        index: 0,
        expected_revision: inner.revision,
        expected_stamp: inner.undo.parameter_target_stamp().unwrap().clone(),
        replacement: expected.clone(),
        expected,
    }
}
#[test]
fn gain_envelope_replace_uses_existing_owners_and_one_undo_without_changing_stage() {
    for target in ["track:0", "bus:b", "master"] {
        let mut inner = state();
        let before = inner.project.clone();
        let mut edit = request(&inner, target);
        edit.replacement.points[0].value = ast::ParamValue::Number(0.75);
        replace_gain_automation_inner(&mut inner, edit).unwrap();
        assert_eq!(
            envelope(&inner, target).points[0].value,
            ast::ParamValue::Number(0.75)
        );
        assert_eq!(inner.project.tracks.len(), before.tracks.len());
        assert_eq!(inner.project.buses[0].routes, before.buses[0].routes);
        assert_eq!(inner.undo.undo_entries().len(), 1);
        inner.undo.undo(&mut inner.project).unwrap();
        assert_eq!(inner.project, before);
    }
}
#[test]
fn gain_envelope_stale_document_identical_payload_and_revision_are_rejected() {
    let first = state();
    let edit = request(&first, "bus:b");
    let mut other = state();
    assert_eq!(other.revision, first.revision);
    assert_eq!(envelope(&other, "bus:b"), edit.expected);
    let before = other.project.clone();
    assert!(replace_gain_automation_inner(&mut other, edit).is_err());
    assert_eq!(other.project, before);
    assert!(other.undo.undo_entries().is_empty());
}
#[test]
fn gain_envelope_stale_revision_and_invalid_replacement_are_atomic() {
    for stale in [false, true] {
        let mut inner = state();
        let mut edit = request(&inner, "track:0");
        let before = inner.project.clone();
        if stale {
            edit.expected_revision = inner.revision.saturating_add(1);
        } else {
            edit.replacement.time_range.end = edit.replacement.time_range.start;
        }
        assert!(replace_gain_automation_inner(&mut inner, edit).is_err());
        assert_eq!(inner.project, before);
        assert!(inner.undo.undo_entries().is_empty());
    }
}

#[test]
fn gain_envelope_preview_refusal_leaves_shown_document_revision_dirty_and_undo_unchanged() {
    let mut inner = state();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    let edit = request(&inner, "bus:b");
    let before_dsl = inner.dsl_source.clone();
    crate::commands::preview::begin_inner(
        &mut inner,
        "@bus b\n  gain_automation: linear ppq=4 [1:1 - 2:1] {1:1=0.75,2:1=0.5}\n".into(),
        "bus:b".into(),
        before_dsl,
    )
    .unwrap();
    let shown = inner.project.clone();
    let source = inner.dsl_source.clone();
    let revision = inner.revision;
    let dirty = inner.dirty;
    let history = inner.undo.undo_entries().len();
    assert!(replace_gain_automation_inner(&mut inner, edit).is_err());
    assert_eq!(inner.project, shown);
    assert_eq!(inner.dsl_source, source);
    assert_eq!(inner.revision, revision);
    assert_eq!(inner.dirty, dirty);
    assert_eq!(inner.undo.undo_entries().len(), history);
}

#[test]
fn typed_gain_editor_and_agent_subtree_share_segment_validation_and_roundtrip() {
    let mut inner = state();
    let original = inner.project.clone();
    let mut edit = request(&inner, "bus:b");
    edit.replacement.interpolation = ast::GainInterpolation::Segments;
    edit.replacement.value_mapping = Some(ast::AutomationValueMapping {
        input_quantization_steps: Some(4),
        scale: 2.0,
        ..Default::default()
    });
    edit.replacement.points[0].curve = ast::Curve::Exponential;
    edit.replacement.points[0].curvature = Some(-2.0);
    replace_gain_automation_inner(&mut inner, edit).unwrap();
    let fragment = oc_dsl::ops::extract(&inner.project, "bus:b")
        .unwrap()
        .dsl_text;
    let mut agent = original;
    oc_dsl::ops::replace(&mut agent, "bus:b", &fragment).unwrap();
    oc_dsl::validate(&agent).unwrap();
    assert_eq!(agent, inner.project);
    assert_eq!(oc_dsl::parse(&oc_dsl::serialize(&agent)).unwrap(), agent);
}
