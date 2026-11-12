//! Timeline markers: the `@marker` form, the round trip, the refusals, and the
//! patch a marker edit produces for undo.
//!
//! Markers are annotations. They may sit past the last audio and they never
//! change what renders, so validation checks the position's spelling only.

const PLAIN: &str = r#"@project "Plain"
  tempo: 120
  time_signature: 4/4

@section verse [1:1 - 9:1]
  label: "Verse"

@track pad | instrument: synth/pad
  @phrase held [1:1 - 9:1]
    C4 w
"#;

const MARKED: &str = r#"@project "Marked"
  tempo: 120
  time_signature: 4/4

@section verse [1:1 - 9:1]
  label: "Verse"

@marker verse at 9:1

@marker "Verse 2" at 9:1:2

@track pad | instrument: synth/pad
  @phrase held [1:1 - 9:1]
    C4 w
"#;

#[test]
fn a_marker_project_round_trips_and_keeps_its_text() {
    let project = oc_dsl::parse(MARKED).expect("marked fixture parses");
    assert_eq!(project.markers.len(), 2);
    assert_eq!(project.markers[0].name, "verse");
    assert_eq!(
        project.markers[0].position,
        oc_dsl::ast::BarBeat::new(9, 1, 0)
    );
    assert_eq!(project.markers[1].name, "Verse 2");
    assert_eq!(
        project.markers[1].position,
        oc_dsl::ast::BarBeat::new(9, 1, 2)
    );

    let serialized = oc_dsl::serialize(&project);
    assert_eq!(
        serialized, MARKED,
        "markers did not keep their written form"
    );

    let reopened = oc_dsl::parse(&serialized).expect("serialized markers re-parse");
    assert_eq!(reopened, project, "AST differs after the round trip");
}

#[test]
fn a_project_without_markers_serialises_byte_identically() {
    let project = oc_dsl::parse(PLAIN).expect("plain fixture parses");
    assert!(project.markers.is_empty());
    let serialized = oc_dsl::serialize(&project);
    assert_eq!(
        serialized, PLAIN,
        "a project with no markers must write the same bytes as before"
    );
    assert!(!serialized.contains("@marker"), "{serialized}");
}

#[test]
fn an_invalid_marker_position_is_refused_with_a_sentence() {
    let zero_bar = oc_dsl::parse("@project\n  tempo: 120\n@marker bad at 0:1\n").expect("parses");
    let errors = oc_dsl::validate(&zero_bar).expect_err("bar 0 must be refused");
    let text = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        text.contains("marker 'bad'.position") && text.contains("bars and beats are 1-based"),
        "{text}"
    );

    let past_beat =
        oc_dsl::parse("@project\n  tempo: 120\n@marker bad at 1:1:5\n").expect("parses");
    let errors = oc_dsl::validate(&past_beat).expect_err("subdivision 5 must be refused");
    let text = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        text.contains("marker 'bad'.position") && text.contains("a beat holds subdivisions 0-3"),
        "{text}"
    );
}

#[test]
fn a_duplicate_marker_name_is_refused() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n@marker hook at 1:1\n@marker hook at 5:1\n")
            .expect("parses");
    let errors = oc_dsl::validate(&project).expect_err("duplicate names must be refused");
    let text = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(text.contains("already uses the name 'hook'"), "{text}");
}

#[test]
fn a_marker_past_the_project_end_is_allowed() {
    let source = "@project\n  tempo: 120\n@marker outro at 99:3\n@track pad | instrument: synth/pad\n  @phrase held [1:1 - 2:1]\n    C4 w\n";
    let project = oc_dsl::parse(source).expect("parses");
    oc_dsl::validate(&project).expect("a marker past the last audio is a legal annotation");
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).expect("re-parses");
    assert_eq!(
        reopened.markers[0].position,
        oc_dsl::ast::BarBeat::new(99, 3, 0)
    );
}

#[test]
fn a_marker_edit_patches_and_inverts_as_markers() {
    let mut project = oc_dsl::parse(PLAIN).expect("plain fixture parses");
    let before = project.clone();
    project.markers.push(oc_dsl::ast::Marker {
        name: "verse".into(),
        position: oc_dsl::ast::BarBeat::new(9, 1, 0),
    });

    let patch = oc_dsl::diff::diff(&before, &project)
        .expect("projects serialize")
        .expect("a marker edit is a change");
    assert!(matches!(
        patch.changes.as_slice(),
        [oc_dsl::diff::PatchOp::SetMarkers(_)]
    ));

    let mut applied = before.clone();
    oc_dsl::diff::apply_patch(&mut applied, &patch).unwrap();
    assert_eq!(applied, project, "the patch did not reproduce the edit");

    let inverse = oc_dsl::diff::invert_patch(&before, &project, &patch);
    oc_dsl::diff::apply_patch(&mut applied, &inverse).unwrap();
    assert_eq!(
        applied, before,
        "the inverse patch did not restore the marker list"
    );
}
