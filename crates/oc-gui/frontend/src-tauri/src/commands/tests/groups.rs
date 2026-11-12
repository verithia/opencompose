//! Clip groups: the command surface, the undo entry each command writes, the
//! pruning rule when a member disappears, and the bar-1 refusal.

use oc_dsl::ast;

use super::support::test_state;
use crate::commands::arrangement::remove_clip_inner;
use crate::commands::groups::{ClipRef, apply_group, apply_move_clip_group, apply_ungroup};
use crate::commands::shared::resync_with_undo;
use crate::state::{UndoEdit, UndoScope};

const SONG: &str = r#"@ocp "0.2.0"

@project "groups"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: synth/pad, preset: tone
  clip: riff [1:1 - 3:1]
  clip: riff [5:1 - 7:1]

@track bass | instrument: synth/pad, preset: tone
  clip: riff [9:1 - 11:1]
"#;

fn project() -> ast::Project {
    let project = oc_dsl::parse(SONG).expect("fixture parses");
    oc_dsl::validate(&project).expect("fixture validates");
    project
}

fn clip(track: usize, clip: usize) -> ClipRef {
    ClipRef {
        track_index: track,
        clip_index: clip,
    }
}

fn starts(project: &ast::Project) -> Vec<(u32, u32, u32)> {
    project
        .tracks
        .iter()
        .flat_map(|track| track.pattern_clips())
        .map(|clip| {
            let start = &clip.time_range.start;
            (start.bar, start.beat, start.subdivision)
        })
        .collect()
}

fn push_undo(inner: &mut crate::state::AppStateInner, old: ast::Project, label: &str) {
    resync_with_undo(inner, old, UndoEdit::new(label, UndoScope::Project)).expect("resync");
}

#[test]
fn grouping_two_clips_writes_one_group_and_survives_save_load() {
    let mut inner = test_state();
    inner.project = project();
    let old = inner.project.clone();
    let id = apply_group(&mut inner.project, &[clip(0, 0), clip(0, 1)]).expect("two clips group");
    push_undo(&mut inner, old, "group clips");

    assert_eq!(id, "g1");
    assert_eq!(inner.project.groups[0].members, vec!["c1", "c2"]);
    assert_eq!(inner.undo.undo.len(), 1, "grouping is one undo entry");
    assert!(
        inner.dsl_source.contains("@group g1 {"),
        "{}",
        inner.dsl_source
    );
    assert!(inner.dsl_source.contains("clip: riff [1:1 - 3:1] id: c1"));

    let reopened = oc_dsl::parse(&inner.dsl_source).expect("saved text parses");
    assert_eq!(reopened, inner.project, "the group survives save and load");

    inner.undo.undo(&mut inner.project).unwrap();
    assert!(inner.project.groups.is_empty(), "undo removes the group");
    assert!(
        inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .id
            .is_none()
    );
}

#[test]
fn ungrouping_restores_the_plain_text_and_is_one_undo_entry() {
    let mut inner = test_state();
    inner.project = project();
    // The command path materializes instrument defaults before it writes text.
    oc_dsl::instruments::apply_defaults(&mut inner.project);
    let plain = oc_dsl::serialize(&inner.project);

    let old = inner.project.clone();
    apply_group(&mut inner.project, &[clip(0, 0), clip(0, 1)]).expect("two clips group");
    push_undo(&mut inner, old, "group clips");

    let old = inner.project.clone();
    let removed = apply_ungroup(&mut inner.project, &[clip(0, 1)]).expect("the group breaks");
    push_undo(&mut inner, old, "ungroup clips");

    assert_eq!(removed, vec!["g1".to_string()]);
    assert_eq!(inner.undo.undo.len(), 2, "ungroup is one undo entry");
    assert!(inner.project.groups.is_empty());
    assert_eq!(
        inner.dsl_source, plain,
        "ungrouping clears the identifiers, so the plain bytes come back"
    );
    assert!(!inner.dsl_source.contains("@group"), "{}", inner.dsl_source);

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.groups.len(), 1, "undo brings the group back");
}

#[test]
fn a_removed_member_drops_out_without_breaking_the_other_group() {
    let mut inner = test_state();
    inner.project = project();
    let old = inner.project.clone();
    // A three-clip group spanning both tracks.
    apply_group(&mut inner.project, &[clip(0, 0), clip(0, 1), clip(1, 0)])
        .expect("three clips group");
    push_undo(&mut inner, old, "group clips");
    assert_eq!(inner.project.groups[0].members, vec!["c1", "c2", "c3"]);

    // Delete the middle member through the command the timeline calls.
    remove_clip_inner(&mut inner, 0, 1).expect("the member is removed");

    assert_eq!(
        inner.project.groups[0].members,
        vec!["c1", "c3"],
        "the surviving members keep their group"
    );
    let text = oc_dsl::serialize(&inner.project);
    assert!(text.contains("@group g1 {"), "{text}");
    assert!(!text.contains("c2"), "{text}");

    // One member left: the group dissolves and the identifier goes with it.
    remove_clip_inner(&mut inner, 1, 0).expect("the last member is removed");
    assert!(inner.project.groups.is_empty());
    assert!(!oc_dsl::serialize(&inner.project).contains("@group"));
}

#[test]
fn dragging_a_member_moves_the_whole_group_by_one_delta() {
    let mut inner = test_state();
    inner.project = project();
    let old = inner.project.clone();
    apply_group(&mut inner.project, &[clip(0, 0), clip(0, 1)]).expect("two clips group");
    push_undo(&mut inner, old, "group clips");

    // Drag the first clip from bar 1 to bar 2: +4 quarters for both.
    let old = inner.project.clone();
    apply_move_clip_group(&mut inner.project, 0, 0, ast::BarBeat::new(2, 1, 0))
        .expect("the group moves");
    push_undo(&mut inner, old, "clip move");

    assert_eq!(
        starts(&inner.project),
        vec![(2, 1, 0), (6, 1, 0), (9, 1, 0)],
        "both members moved by the same delta; the ungrouped clip stayed"
    );
    assert_eq!(inner.project.groups[0].members, vec!["c1", "c2"]);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        starts(&inner.project),
        vec![(1, 1, 0), (5, 1, 0), (9, 1, 0)]
    );
}

#[test]
fn a_group_keeps_each_member_on_its_own_track() {
    let mut inner = test_state();
    inner.project = project();
    let old = inner.project.clone();
    apply_group(&mut inner.project, &[clip(0, 0), clip(1, 0)]).expect("two clips group");
    push_undo(&mut inner, old, "group clips");

    apply_move_clip_group(&mut inner.project, 0, 0, ast::BarBeat::new(3, 1, 0))
        .expect("the group moves");
    assert_eq!(
        starts(&inner.project),
        vec![(3, 1, 0), (5, 1, 0), (11, 1, 0)]
    );
    assert_eq!(
        inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .time_range
            .start
            .bar,
        3
    );
    assert_eq!(
        inner.project.tracks[1]
            .pattern_clip(0)
            .unwrap()
            .time_range
            .start
            .bar,
        11
    );
}

#[test]
fn a_move_that_would_push_a_member_before_bar_1_is_refused_with_a_reason() {
    let mut inner = test_state();
    inner.project = project();
    let old = inner.project.clone();
    apply_group(&mut inner.project, &[clip(0, 0), clip(0, 1)]).expect("two clips group");
    push_undo(&mut inner, old, "group clips");
    let before = oc_dsl::serialize(&inner.project);
    let entries = inner.undo.undo.len();

    // Dragging the bar-5 clip back to bar 4 would put the bar-1 member at bar 0.
    let error = apply_move_clip_group(&mut inner.project, 0, 1, ast::BarBeat::new(4, 1, 0))
        .expect_err("a member before bar 1 must be refused");
    assert!(error.contains("before bar 1"), "{error}");
    assert!(
        error.contains("riff"),
        "the reason names the member: {error}"
    );
    assert_eq!(
        oc_dsl::serialize(&inner.project),
        before,
        "a refused move leaves the project alone"
    );
    assert_eq!(inner.undo.undo.len(), entries, "no undo entry is written");
}

#[test]
fn moving_a_member_without_a_group_is_refused() {
    let mut inner = test_state();
    inner.project = project();
    let error = apply_move_clip_group(&mut inner.project, 0, 0, ast::BarBeat::new(2, 1, 0))
        .expect_err("an ungrouped clip has no group to move");
    assert!(error.contains("not in a group"), "{error}");
}

#[test]
fn grouping_fewer_than_two_clips_is_refused_and_leaves_no_group() {
    let mut inner = test_state();
    inner.project = project();
    let error = apply_group(&mut inner.project, &[clip(0, 0)]).expect_err("one clip cannot group");
    assert!(error.contains("at least 2 clips"), "{error}");
    assert!(inner.project.groups.is_empty());
    assert!(
        inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .id
            .is_none()
    );
}
