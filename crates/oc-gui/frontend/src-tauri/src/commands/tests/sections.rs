//! Arrangement sections: the agent surface's add/set/remove on project state,
//! the refusals a model can hit, and the rendered length a repeat plan declares.

use oc_dsl::ast;

use super::support::test_state;
use crate::commands::sections::{
    apply_add_section, apply_remove_section, apply_set_section, parse_position, position_text,
    summary,
};
use crate::commands::shared::resync_with_undo;
use crate::state::{UndoEdit, UndoScope};

const ARRANGE: &str = r#"@ocp "0.2.0"

@project "arrangement"
  tempo: 100
  time_signature: 4/4

@track pad | instrument: synth/pad
  @phrase held [1:1 - 21:1]
    C4 w

@section intro [1:1 - 9:1]
  label: "Intro"

@section chorus [17:1 - 21:1]
  label: "Chorus"
"#;

fn project(source: &str) -> ast::Project {
    let project = oc_dsl::parse(source).expect("fixture parses");
    oc_dsl::validate(&project).expect("fixture validates");
    project
}

fn section_names(project: &ast::Project) -> Vec<&str> {
    project
        .sections
        .iter()
        .map(|section| section.name.as_str())
        .collect()
}

#[test]
fn sections_add_inserts_at_the_written_position() {
    let mut project = project(ARRANGE);

    let index = apply_add_section(
        &mut project,
        "verse",
        parse_position("9:1").unwrap(),
        parse_position("17:1").unwrap(),
        None,
        Some("Verse".into()),
    )
    .unwrap();

    assert_eq!(index, 1);
    assert_eq!(section_names(&project), ["intro", "verse", "chorus"]);
    assert_eq!(project.sections[1].label.as_deref(), Some("Verse"));
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened, project, "the new section survives save/load");
}

#[test]
fn sections_set_moves_resizes_renames_and_sets_the_repeat() {
    let mut project = project(ARRANGE);

    let changed = apply_set_section(
        &mut project,
        "chorus",
        Some("hook".into()),
        Some(parse_position("17:1").unwrap()),
        Some(parse_position("25:1").unwrap()),
        Some(2),
        Some("Hook".into()),
    )
    .unwrap();
    assert!(changed);

    let hook = &project.sections[1];
    assert_eq!(hook.name, "hook");
    assert_eq!(position_text(&hook.time_range.end), "25:1");
    assert_eq!(hook.repeat, Some(2));
    assert_eq!(hook.label.as_deref(), Some("Hook"));

    // repeat 1 and an empty label clear the fields rather than writing them.
    apply_set_section(
        &mut project,
        "hook",
        None,
        None,
        None,
        Some(1),
        Some(String::new()),
    )
    .unwrap();
    let hook = &project.sections[1];
    assert_eq!(hook.repeat, None);
    assert_eq!(hook.label, None);
}

#[test]
fn an_unchanged_section_edit_reports_no_change_and_pushes_no_undo() {
    let mut inner = test_state();
    inner.project = project(ARRANGE);
    let revision = inner.revision;
    let before = inner.project.clone();

    assert!(
        !apply_set_section(&mut inner.project, "intro", None, None, None, Some(1), None).unwrap(),
        "a call that names no change is not an edit"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.revision, revision);

    let old_project = inner.project.clone();
    apply_add_section(
        &mut inner.project,
        "verse",
        parse_position("9:1").unwrap(),
        parse_position("17:1").unwrap(),
        None,
        None,
    )
    .unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add section", UndoScope::Project),
    )
    .expect("resync");
    assert_eq!(inner.undo.undo.len(), 1, "one section, one undo entry");
    assert!(inner.revision > revision, "the edit bumps the revision");
    assert!(inner.dsl_source.contains("@section verse"));
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(section_names(&inner.project), ["intro", "chorus"]);
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(section_names(&inner.project), ["intro", "verse", "chorus"]);
}

#[test]
fn sections_remove_drops_only_the_named_section() {
    let mut project = project(ARRANGE);
    apply_remove_section(&mut project, "intro").unwrap();
    assert_eq!(section_names(&project), ["chorus"]);
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened, project);
    assert_eq!(
        apply_remove_section(&mut project, "intro").unwrap_err(),
        "section 'intro' not found; the project has 'chorus'"
    );
}

#[test]
fn sections_refuse_a_duplicate_name() {
    let mut project = project(ARRANGE);
    let error = apply_add_section(
        &mut project,
        "intro",
        parse_position("9:1").unwrap(),
        parse_position("17:1").unwrap(),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "section 'intro' already exists; use sections_set to change it, or pick another name \
         (the project has 'intro', 'chorus')"
    );
    assert_eq!(section_names(&project), ["intro", "chorus"]);
}

#[test]
fn sections_refuse_an_overlapping_range() {
    let mut project = project(ARRANGE);
    let error = apply_add_section(
        &mut project,
        "verse",
        parse_position("17:1").unwrap(),
        parse_position("25:1").unwrap(),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "section 'verse' [17:1 - 25:1] overlaps section 'chorus' [17:1 - 21:1]; \
         sections must not overlap"
    );
    assert_eq!(section_names(&project), ["intro", "chorus"]);
}

#[test]
fn sections_refuse_a_span_that_precedes_the_written_order() {
    let mut project = project(ARRANGE);
    // A project that arrives out of written order is refused rather than kept.
    project.sections.swap(0, 1);
    let error =
        apply_set_section(&mut project, "chorus", None, None, None, Some(2), None).unwrap_err();
    assert!(
        error.contains("section 'intro' starts at 1:1, before section 'chorus' starts at 17:1"),
        "{error}"
    );
    assert!(error.contains("written order"), "{error}");
}

#[test]
fn sections_refuse_a_range_that_ends_before_it_starts() {
    let mut project = project(ARRANGE);
    let error = apply_set_section(
        &mut project,
        "chorus",
        None,
        None,
        Some(parse_position("17:1").unwrap()),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "section 'chorus' range [17:1 - 17:1] ends before it starts"
    );
    assert_eq!(position_text(&project.sections[1].time_range.end), "21:1");
}

#[test]
fn sections_refuse_repeat_zero() {
    let mut project = project(ARRANGE);
    let error =
        apply_set_section(&mut project, "chorus", None, None, None, Some(0), None).unwrap_err();
    assert_eq!(
        error,
        "section 'chorus' repeat cannot be 0; a section plays at least once (omit repeat or use 1)"
    );
    let error = apply_add_section(
        &mut project,
        "verse",
        parse_position("9:1").unwrap(),
        parse_position("17:1").unwrap(),
        Some(0),
        None,
    )
    .unwrap_err();
    assert!(
        error.contains("section 'verse' repeat cannot be 0"),
        "{error}"
    );
}

#[test]
fn sections_refuse_renaming_to_an_existing_name() {
    let mut project = project(ARRANGE);
    let error = apply_set_section(
        &mut project,
        "chorus",
        Some("intro".into()),
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "cannot rename section 'chorus' to 'intro': a section named 'intro' already exists \
         (the project has 'intro', 'chorus')"
    );
    assert_eq!(section_names(&project), ["intro", "chorus"]);
}

#[test]
fn sections_refuse_a_span_past_the_project_extent() {
    let mut project = project(ARRANGE);
    let error = apply_add_section(
        &mut project,
        "outro",
        parse_position("21:1").unwrap(),
        parse_position("2000:1").unwrap(),
        None,
        None,
    )
    .unwrap_err();
    assert!(
        error.starts_with("section 'outro' leaves the rendered arrangement ending at "),
        "{error}"
    );
    assert!(
        error.ends_with("past the 600-second project extent the desktop renderer supports"),
        "{error}"
    );
    assert!(error.contains("4778.4 s"), "{error}");
    assert_eq!(section_names(&project), ["intro", "chorus"]);
}

#[test]
fn sections_refuse_a_position_outside_its_bar() {
    let mut project = project(ARRANGE);
    let error = apply_set_section(
        &mut project,
        "chorus",
        None,
        Some(parse_position("17:5").unwrap()),
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(
        error.starts_with("section 'chorus' start 17:5: "),
        "{error}"
    );
    assert!(error.contains("inside its notated bar"), "{error}");
}

#[test]
fn parse_position_accepts_the_dsl_spellings() {
    assert_eq!(parse_position("9").unwrap(), ast::BarBeat::new(9, 1, 0));
    assert_eq!(parse_position("9:2").unwrap(), ast::BarBeat::new(9, 2, 0));
    assert_eq!(parse_position("9:2:3").unwrap(), ast::BarBeat::new(9, 2, 3));
    assert_eq!(position_text(&ast::BarBeat::new(9, 1, 0)), "9:1");
    assert_eq!(position_text(&ast::BarBeat::new(9, 4, 2)), "9:4:2");
    assert_eq!(
        parse_position("0:1").unwrap_err(),
        "position '0:1' uses 0; bars and beats are 1-based"
    );
    assert_eq!(
        parse_position("9:1:4").unwrap_err(),
        "position '9:1:4' has subdivision 4; a beat holds subdivisions 0-3"
    );
    assert_eq!(
        parse_position("9:x").unwrap_err(),
        "position '9:x' has a non-numeric part 'x'; use bar:beat:subdivision, e.g. 9:1"
    );
}

/// The capability the tools exist for: "repeat the verse twice, so the whole
/// thing ends at bar 32" is a repeat plan, and the rendered length follows it.
#[test]
fn a_repeat_plan_sets_the_rendered_end() {
    let mut project = project(ARRANGE);
    apply_add_section(
        &mut project,
        "verse",
        parse_position("9:1").unwrap(),
        parse_position("17:1").unwrap(),
        None,
        None,
    )
    .unwrap();

    let plan = summary(&project, 7).unwrap();
    assert_eq!(plan["count"], 3);
    assert_eq!(plan["rendered_bars"], 20.0);
    assert_eq!(plan["rendered_end"], "21:1");
    assert_eq!(plan["rendered_seconds"], 48.0);
    assert_eq!(plan["sections"][1]["bars"], 8.0);
    assert_eq!(plan["sections"][1]["repeat"], 1);

    // "repeat the verse twice" and "make the chorus last 8 bars": both repeats.
    apply_set_section(&mut project, "verse", None, None, None, Some(2), None).unwrap();
    apply_set_section(&mut project, "chorus", None, None, None, Some(2), None).unwrap();

    let plan = summary(&project, 8).unwrap();
    assert_eq!(plan["sections"][1]["repeat"], 2);
    assert_eq!(plan["sections"][2]["repeat"], 2);
    assert_eq!(
        plan["rendered_bars"], 32.0,
        "8 bars of intro + 16 of verse + 8 of chorus"
    );
    assert_eq!(
        plan["rendered_end"], "33:1",
        "end-exclusive, as the DSL writes it"
    );
    assert_eq!(plan["rendered_seconds"], 76.8);
    assert_eq!(
        plan["revision"], 8,
        "the summary carries the project revision"
    );
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened, project, "the repeats survive save/load");
}

/// A repeat that holds a tempo change reports the length it plays; the written
/// map read at a rendered quarter would report 32 s for this arrangement.
#[test]
fn a_repeat_holding_a_tempo_change_reports_the_played_seconds() {
    let project = project(
        r#"@ocp "0.2.0"

@project "repeat tempo"
  tempo: 120
  time_signature: 4/4
  tempo_map:
    3:1 = 240
    7:1 = 60

@track pad | instrument: synth/pad
  @phrase held [1:1 - 2:1]
    C4 w

@section verse [1:1 - 5:1]
  repeat: 2

@section outro [5:1 - 9:1]
"#,
    );

    let plan = summary(&project, 1).unwrap();
    assert_eq!(plan["rendered_end"], "13:1");
    assert_eq!(plan["rendered_bars"], 12.0);
    assert_eq!(plan["rendered_seconds"], 20.0);
    assert_eq!(plan["sections"][0]["bars"], 4.0);
}

/// Every refusal reaches the model as `ok: false`, and the ones the section
/// manual explains carry its id next to the message.
#[test]
fn section_refusals_reach_the_agent_as_a_skill_hint() {
    let mut project = project(ARRANGE);
    let error = apply_add_section(
        &mut project,
        "verse",
        parse_position("17:1").unwrap(),
        parse_position("25:1").unwrap(),
        None,
        None,
    )
    .unwrap_err();
    let payload = crate::ai::skill_hints::refusal(&error);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"], error.as_str());
    assert_eq!(payload["skill"], "dsl-subtree-paths");
    assert_eq!(
        payload["hint"],
        "read_skill('dsl-subtree-paths') teaches this"
    );

    let error = apply_set_section(
        &mut project,
        "chorus",
        Some("intro".into()),
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    let payload = crate::ai::skill_hints::refusal(&error);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["skill"], "dsl-subtree-paths");
}
