//! Control-link command: learn one row per target, edit, remove, undo.

use oc_dsl::ast::{ControlLinkSource, ControlLinkTarget, LinkFormula};

use super::support::{make_track, test_state};
use crate::commands::control_links::{
    add_control_link_target_inner, remove_control_link_target_inner,
    update_control_link_target_inner,
};

fn cc(controller: u8) -> ControlLinkSource {
    ControlLinkSource::MidiCc {
        channel: 1,
        controller,
    }
}

fn target(track: &str, param: &str) -> ControlLinkTarget {
    ControlLinkTarget {
        track: track.into(),
        param: param.into(),
        formula: LinkFormula::Linear,
        smoothing_ms: 0,
    }
}

fn state_with_track() -> crate::state::AppStateInner {
    let mut inner = test_state();
    inner.project.tracks.push(make_track("Lead"));
    inner
}

#[test]
fn learn_writes_exactly_one_record_per_target() {
    let mut inner = state_with_track();
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "filter.cutoff")).unwrap();
    // The same target learned again keeps one row and its tuning.
    let mut again = target("Lead", "filter.cutoff");
    again.formula = LinkFormula::Exponential;
    again.smoothing_ms = 8;
    add_control_link_target_inner(&mut inner, cc(74), again).unwrap();
    let links = &inner.project.properties.control_links;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].name, "cc74");
    assert_eq!(links[0].targets.len(), 1);
    assert_eq!(links[0].targets[0].formula, LinkFormula::Linear);
    assert_eq!(links[0].targets[0].smoothing_ms, 0);
    assert_eq!(
        inner.undo.undo.len(),
        1,
        "re-learning a bound target is a no-op"
    );

    // A second target under the same control is one more row.
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "output.gain")).unwrap();
    assert_eq!(inner.project.properties.control_links[0].targets.len(), 2);
    // A different control is a different record.
    add_control_link_target_inner(&mut inner, cc(1), target("Lead", "output.gain")).unwrap();
    assert_eq!(inner.project.properties.control_links.len(), 2);
}

#[test]
fn learned_links_serialize_and_undo_as_one_entry() {
    let mut inner = state_with_track();
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "filter.cutoff")).unwrap();
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(inner.dsl_source.contains("@control_link cc74"));
    assert!(
        inner
            .dsl_source
            .contains("-> track: Lead param: filter.cutoff formula: linear")
    );
    // Undo removes the learned link and the DSL with it.
    inner.undo.undo(&mut inner.project).unwrap();
    assert!(inner.project.properties.control_links.is_empty());
}

#[test]
fn update_retunes_one_row_and_rejects_an_unknown_pair() {
    let mut inner = state_with_track();
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "filter.cutoff")).unwrap();
    update_control_link_target_inner(
        &mut inner,
        cc(74),
        "Lead",
        "filter.cutoff",
        LinkFormula::Logarithmic,
        25,
    )
    .unwrap();
    let row = &inner.project.properties.control_links[0].targets[0];
    assert_eq!(row.formula, LinkFormula::Logarithmic);
    assert_eq!(row.smoothing_ms, 25);

    let error = update_control_link_target_inner(
        &mut inner,
        cc(74),
        "Lead",
        "output.gain",
        LinkFormula::Linear,
        0,
    )
    .expect_err("unknown row");
    assert!(error.contains("output.gain"));

    // A macro source cannot take a raw-MIDI curve, on add or on update.
    add_control_link_target_inner(
        &mut inner,
        ControlLinkSource::Macro { index: 1 },
        target("Lead", "filter.cutoff"),
    )
    .unwrap();
    let error = update_control_link_target_inner(
        &mut inner,
        ControlLinkSource::Macro { index: 1 },
        "Lead",
        "filter.cutoff",
        LinkFormula::MidiUnit,
        0,
    )
    .expect_err("raw MIDI curve on a macro");
    assert!(error.contains("raw MIDI"));
}

#[test]
fn removing_the_last_row_removes_the_link() {
    let mut inner = state_with_track();
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "filter.cutoff")).unwrap();
    add_control_link_target_inner(&mut inner, cc(74), target("Lead", "output.gain")).unwrap();
    remove_control_link_target_inner(&mut inner, &cc(74), "Lead", "output.gain").unwrap();
    assert_eq!(inner.project.properties.control_links[0].targets.len(), 1);
    remove_control_link_target_inner(&mut inner, &cc(74), "Lead", "filter.cutoff").unwrap();
    assert!(inner.project.properties.control_links.is_empty());
    assert!(inner.dsl_source.find("@control_link").is_none());
}

#[test]
fn rejects_unknown_tracks_and_midi_formulas_on_a_macro() {
    let mut inner = state_with_track();
    let error =
        add_control_link_target_inner(&mut inner, cc(74), target("Nope", "cutoff")).unwrap_err();
    assert!(error.contains("Unknown track"));

    let mut raw_midi = target("Lead", "cutoff");
    raw_midi.formula = LinkFormula::MidiCentered;
    let error =
        add_control_link_target_inner(&mut inner, ControlLinkSource::Macro { index: 1 }, raw_midi)
            .unwrap_err();
    assert!(error.contains("raw MIDI"));
    assert!(inner.project.properties.control_links.is_empty());
}
