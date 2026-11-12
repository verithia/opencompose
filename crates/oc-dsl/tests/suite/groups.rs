//! Clip groups: the `@group` form, the round trip, the byte identity of a
//! project without groups, the pruning rule, and the refusals.
//!
//! A group names its members by clip identifier. Membership never changes what
//! renders; it binds the move and the selection of the clips it lists.

use oc_dsl::ast::{BarBeat, Clip, ClipGroup, TimeRange};
use oc_dsl::groups::{self, reconcile};
use oc_dsl::{parse, serialize};

const PLAIN: &str = r#"@ocp "0.2.0"

@project "Plain"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [2:1 - 6:1]
  clip: riff [6:1 - 10:1]
"#;

const GROUPED: &str = r#"@ocp "0.2.0"

@project "Grouped"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@group g1 {
  members: c1, c2
}

@track keys | instrument: piano/acoustic_grand
  clip: riff [2:1 - 6:1] id: c1
  clip: riff [6:1 - 10:1] id: c2
"#;

#[test]
fn a_grouped_project_round_trips_and_keeps_its_text() {
    let project = parse(GROUPED).expect("grouped fixture parses");
    assert_eq!(project.groups.len(), 1);
    assert_eq!(project.groups[0].id, "g1");
    assert_eq!(project.groups[0].members, vec!["c1", "c2"]);
    assert_eq!(
        project.tracks[0].pattern_clip(0).unwrap().id.as_deref(),
        Some("c1")
    );

    let formatted = serialize(&project);
    assert_eq!(
        formatted, GROUPED,
        "the group did not keep its written form"
    );

    let reopened = parse(&formatted).expect("serialized group re-parses");
    assert_eq!(reopened, project, "AST differs after the round trip");
    assert_eq!(serialize(&reopened), formatted, "a second pass is stable");
}

#[test]
fn a_project_without_groups_serialises_byte_identically() {
    let project = parse(PLAIN).expect("plain fixture parses");
    assert!(project.groups.is_empty());
    let formatted = serialize(&project);
    assert_eq!(
        formatted, PLAIN,
        "a project with no groups must write the same bytes as before"
    );
    assert!(!formatted.contains("@group"), "{formatted}");
    assert!(!formatted.contains(" id: "), "{formatted}");
}

#[test]
fn grouping_then_ungrouping_restores_the_plain_bytes() {
    let mut project = parse(PLAIN).expect("plain fixture parses");
    groups::group_clips(&mut project, &[(0, 0), (0, 1)]).expect("two clips group");
    let grouped = serialize(&project);
    assert!(grouped.contains("@group g1 {"), "{grouped}");
    assert!(grouped.contains("id: c1"), "{grouped}");

    let ids = project.groups[0].members.clone();
    groups::ungroup_clips(&mut project, &ids).expect("the group is removed");
    assert!(project.groups.is_empty());
    assert_eq!(
        serialize(&project),
        PLAIN,
        "ungrouping clears the identifiers, so the bytes come back"
    );
}

#[test]
fn grouping_assigns_identifiers_and_moving_a_member_keeps_membership() {
    let mut project = parse(PLAIN).expect("plain fixture parses");
    assert_eq!(groups::next_clip_id(&project), "c1");
    let id = groups::group_clips(&mut project, &[(0, 0), (0, 1)]).expect("two clips group");
    assert_eq!(id, "g1");
    assert_eq!(project.groups[0].members, vec!["c1", "c2"]);

    // A move changes the range only; membership is by identifier.
    let moved = Clip {
        pattern: "riff".into(),
        time_range: TimeRange {
            start: BarBeat::new(3, 1, 0),
            end: BarBeat::new(7, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: Some("c1".into()),
    };
    *project.tracks[0].pattern_clip_mut(0).unwrap() = moved;
    reconcile(&mut project);
    assert_eq!(project.groups[0].members, vec!["c1", "c2"]);
    let (track, index, clip) = groups::clip_by_id(&project, "c1").expect("the member resolves");
    assert_eq!((track, index), (0, 0));
    assert_eq!(clip.time_range.start, BarBeat::new(3, 1, 0));
}

#[test]
fn a_removed_member_drops_out_and_a_one_member_group_dissolves() {
    let mut project = parse(GROUPED).expect("grouped fixture parses");
    let count = project.tracks[0].pattern_clip_count();
    project.tracks[0].remove_pattern_clip(count - 1);
    reconcile(&mut project);
    assert!(
        project.groups.is_empty(),
        "a group below two members must not stay behind"
    );
    assert!(
        project.tracks[0].pattern_clip(0).unwrap().id.is_none(),
        "the identifier is cleared once no group names it"
    );
    let formatted = serialize(&project);
    assert!(!formatted.contains("@group"), "{formatted}");
    assert!(!formatted.contains("id:"), "{formatted}");
}

#[test]
fn a_three_member_group_keeps_the_survivors_when_one_is_removed() {
    let mut project = parse(GROUPED).expect("grouped fixture parses");
    project.tracks[0].push_pattern_clip(Clip {
        pattern: "riff".into(),
        time_range: TimeRange {
            start: BarBeat::new(10, 1, 0),
            end: BarBeat::new(14, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: Some("c3".into()),
    });
    project.groups[0].members.push("c3".into());
    project.tracks[0].remove_pattern_clip(1);
    reconcile(&mut project);
    assert_eq!(project.groups[0].members, vec!["c1", "c3"]);
    assert_eq!(
        project.tracks[0].pattern_clip(0).unwrap().id.as_deref(),
        Some("c1")
    );
    assert_eq!(
        project.tracks[0].pattern_clip(1).unwrap().id.as_deref(),
        Some("c3")
    );
}

#[test]
fn a_dangling_member_is_refused_with_a_sentence() {
    let source = GROUPED.replace("members: c1, c2", "members: c1, c9");
    let project = parse(&source).expect("the text parses");
    let errors = oc_dsl::validate(&project).expect_err("a member with no clip must be refused");
    let text = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(text.contains("c9"), "{text}");
    assert!(text.contains("names no clip"), "{text}");
}

#[test]
fn a_group_below_two_members_is_refused() {
    let source = GROUPED.replace("members: c1, c2", "members: c1");
    let project = parse(&source).expect("the text parses");
    let errors = oc_dsl::validate(&project).expect_err("one member must be refused");
    let text = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(text.contains("at least 2 clips"), "{text}");
}

#[test]
fn a_repeated_member_and_a_repeated_group_identifier_are_refused() {
    let repeated_member =
        parse(&GROUPED.replace("members: c1, c2", "members: c1, c1")).expect("the text parses");
    let text = oc_dsl::validate(&repeated_member)
        .expect_err("a repeated member must be refused")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(text.contains("listed twice"), "{text}");

    let mut project = parse(GROUPED).expect("grouped fixture parses");
    project.groups.push(ClipGroup {
        id: "g1".into(),
        members: vec!["c1".into(), "c2".into()],
    });
    let text = oc_dsl::validate(&project)
        .expect_err("a repeated group identifier must be refused")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        text.contains("two groups use the identifier 'g1'"),
        "{text}"
    );
}

#[test]
fn a_duplicate_clip_identifier_is_refused() {
    let mut project = parse(GROUPED).expect("grouped fixture parses");
    project.tracks[0].pattern_clip_mut(1).unwrap().id = Some("c1".into());
    let text = oc_dsl::validate(&project)
        .expect_err("two clips with one identifier must be refused")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(text.contains("two clips use the identifier 'c1'"), "{text}");
}

#[test]
fn grouping_a_clip_that_already_belongs_moves_it_to_the_new_group() {
    let mut project = parse(GROUPED).expect("grouped fixture parses");
    project.tracks[0].push_pattern_clip(Clip {
        pattern: "riff".into(),
        time_range: TimeRange {
            start: BarBeat::new(10, 1, 0),
            end: BarBeat::new(14, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    let id = groups::group_clips(&mut project, &[(0, 0), (0, 2)]).expect("a new group forms");
    assert_eq!(id, "g2");
    assert_eq!(project.groups.len(), 1, "g1 lost its last valid member");
    assert_eq!(project.groups[0].id, "g2");
    assert_eq!(project.groups[0].members, vec!["c1", "c3"]);
    assert!(
        project.tracks[0].pattern_clip(1).unwrap().id.is_none(),
        "the clip left behind loses its identifier"
    );
}

#[test]
fn a_group_of_fewer_than_two_clips_is_refused_by_the_editing_rule() {
    let mut project = parse(PLAIN).expect("plain fixture parses");
    let error = groups::group_clips(&mut project, &[(0, 0)]).expect_err("one clip cannot group");
    assert!(error.contains("at least 2 clips"), "{error}");
    assert!(project.groups.is_empty());
}
