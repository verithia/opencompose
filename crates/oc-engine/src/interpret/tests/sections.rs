//! Section sound flags apply a region articulation to note onsets.

use super::super::*;

fn notes(events: &[MidiEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect()
}

fn phrase_project(flag: &str, event: &str) -> Project {
    let flags = if flag.is_empty() {
        String::new()
    } else {
        format!("  flags: {flag}\n")
    };
    oc_dsl::parse(&format!(
        "@project\n  tempo: 120\n@section verse [1:1 - 2:1]\n{flags}@preset strings\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{keyswitch_duration_ms: 10, articulation_pizzicato_keyswitch: \"C0\", articulation_staccato_keyswitch: \"D0\"}}\n@track strings | preset: strings, instrument: strings/violin\n  @phrase line [1:1 - 2:1]\n    {event}\n"
    ))
    .unwrap()
}

#[test]
fn a_section_flag_presses_the_regions_keyswitch() {
    let flagged = phrase_project("pizzicato", "C4 q mf");
    let plain = phrase_project("", "C4 q mf");
    assert!(notes(&interpret(&flagged, &InterpretSettings::default())).contains(&12));
    assert!(!notes(&interpret(&plain, &InterpretSettings::default())).contains(&12));
}

#[test]
fn a_notes_own_articulation_wins_over_the_section_flag() {
    let project = phrase_project("pizzicato", "C4 q mf articulation=staccato");
    let notes = notes(&interpret(&project, &InterpretSettings::default()));
    assert!(
        notes.contains(&14),
        "the note's staccato keyswitch: {notes:?}"
    );
    assert!(
        !notes.contains(&12),
        "the section flag is overridden: {notes:?}"
    );
}

#[test]
fn a_section_flag_reaches_a_placed_pattern_note() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@section verse [1:1 - 2:1]\n  flags: pizzicato\n@preset strings\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {keyswitch_duration_ms: 10, articulation_pizzicato_keyswitch: \"C0\"}\n@pattern riff | instrument: strings/violin, preset: strings\n  events:\n    C4 q mf\n@track strings | preset: strings, instrument: strings/violin\n  clip: riff [1:1 - 2:1]\n",
    )
    .unwrap();
    assert!(notes(&interpret(&project, &InterpretSettings::default())).contains(&12));
}

#[test]
fn a_section_flag_round_trips_and_repeats_with_its_section() {
    let project = oc_dsl::parse(
        "@project
  tempo: 120
@section verse [1:1 - 2:1]
  flags: con_sord
  repeat: 2
@track strings | instrument: strings/violin
  @phrase line [1:1 - 2:1]
    C4 q mf
",
    )
    .unwrap();
    let text = oc_dsl::serialize(&project);
    assert!(text.contains("flags: muted"), "{text}");
    let reparsed = oc_dsl::parse(&text).unwrap();
    assert_eq!(reparsed.sections[0].flags, Some(Articulation::Muted));

    let expanded = oc_dsl::arrangement::expand(&project).unwrap();
    assert_eq!(expanded.sections.len(), 2);
    assert!(
        expanded
            .sections
            .iter()
            .all(|section| section.flags == Some(Articulation::Muted))
    );
}

#[test]
fn a_section_rejects_a_second_flags_line() {
    let error = oc_dsl::parse(
        "@project
@section verse [1:1 - 2:1]
  flags: pizzicato
  flags: arco
@track strings | instrument: strings/violin
",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("one `flags:` line"), "{error}");
}

/// A flag belongs to the section that carries it: a note written before the
/// boundary keeps the earlier sound, so a seek or a pickup cannot inherit it.
#[test]
fn a_flag_begins_at_its_own_section_boundary() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n\
         @section intro [1:1 - 2:1]\n\
         @section verse [2:1 - 3:1]\n  flags: pizzicato\n\
         @preset strings\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  \
         params: {keyswitch_duration_ms: 10, articulation_pizzicato_keyswitch: \"C0\"}\n\
         @track strings | preset: strings, instrument: strings/violin\n  \
         @phrase line [1:1 - 3:1]\n    C4 q mf\n    @4 C5 q mf\n",
    )
    .unwrap();
    let notes = notes(&interpret(&project, &InterpretSettings::default()));
    let first = notes
        .iter()
        .position(|note| *note == 60)
        .expect("the intro note");
    let switch = notes
        .iter()
        .position(|note| *note == 12)
        .expect("the keyswitch");
    let later = notes
        .iter()
        .position(|note| *note == 72)
        .expect("the verse note");
    assert!(
        first < switch && switch < later,
        "the keyswitch landed at {switch}, outside {first}..{later}: {notes:?}"
    );
}
