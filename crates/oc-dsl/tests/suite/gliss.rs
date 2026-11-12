//! The `gliss` note mark: every spelling parses, `fmt` keeps it, and a mark
//! that cannot name its destination is refused where the model reads it.

const SOURCE: &str = r#"@project "Gliss"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@track lead | preset: tone, instrument: synth/lead_saw
  @phrase slide [1:1 - 2:1]
    C4 q mf gliss
    G4 q mf gliss=D5
    A4 q mf gliss=120ms
    E4 q mf gliss=run
    B4 q mf gliss=run_diatonic
    C5 q mf
"#;

fn parse(source: &str) -> oc_dsl::ast::Project {
    oc_dsl::parse(source).unwrap_or_else(|error| panic!("{error}\n---\n{source}"))
}

fn modifiers(source: &str, index: usize) -> Vec<(String, oc_dsl::ast::ParamValue)> {
    parse(source).tracks[0].phrases[0].events[index]
        .modifiers
        .clone()
}

#[test]
fn every_gliss_form_parses_and_survives_fmt_byte_for_byte() {
    let project = parse(SOURCE);
    let text = oc_dsl::serialize(&project);
    assert_eq!(text, SOURCE, "fmt must keep the mark as it was written");
    assert_eq!(parse(&text), project);

    use oc_dsl::ast::ParamValue;
    assert_eq!(
        modifiers(SOURCE, 0),
        vec![("gliss".into(), ParamValue::String("next".into()))]
    );
    assert_eq!(
        modifiers(SOURCE, 1),
        vec![("gliss".into(), ParamValue::String("D5".into()))]
    );
    assert_eq!(
        modifiers(SOURCE, 2),
        vec![("gliss".into(), ParamValue::Time(0.12))]
    );
    assert_eq!(
        modifiers(SOURCE, 3),
        vec![("gliss".into(), ParamValue::String("run".into()))]
    );
    assert_eq!(
        modifiers(SOURCE, 4),
        vec![("gliss".into(), ParamValue::String("run_diatonic".into()))]
    );
    assert_eq!(modifiers(SOURCE, 5), Vec::<(String, ParamValue)>::new());
    assert_eq!(
        oc_dsl::gliss::form(&modifiers(SOURCE, 1)).unwrap(),
        Some(oc_dsl::gliss::Gliss::To(
            oc_dsl::ast::Pitch::from_text("D5").unwrap()
        ))
    );
}

#[test]
fn a_mark_without_a_following_note_is_refused_by_name() {
    let dangling = "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss\n";
    let error = oc_dsl::parse(dangling).unwrap_err().to_string();
    assert!(
        error.contains("gliss at @0: no note follows it in this phrase"),
        "{error}"
    );
    assert!(error.contains("gliss=G4"), "{error}");

    let chord_next = "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss\n    [E4 G4] q mf\n";
    let error = oc_dsl::parse(chord_next).unwrap_err().to_string();
    assert!(
        error.contains("the next note is a chord (E4 G4)"),
        "{error}"
    );
    assert!(error.contains("gliss=G4"), "{error}");

    // A rest does not sound, so it is not a destination; the written pitch after
    // it is. A rest as the marker's own line keeps the mark readable.
    let rest_between = "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss\n    rest q\n    G4 q mf\n";
    assert_eq!(
        oc_dsl::gliss::successor(&parse(rest_between).tracks[0].phrases[0].events, 0),
        oc_dsl::gliss::Successor::One(oc_dsl::ast::Pitch::from_text("G4").unwrap())
    );
}

#[test]
fn a_mark_that_shares_its_onset_is_refused_by_name() {
    for (source, needle) in [
        (
            "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    [C4 E4] q mf gliss\n    G4 q mf\n",
            "gliss needs a line of its own: a chord member sounds with the other pitches",
        ),
        (
            "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss, E4 q mf\n    G4 q mf\n",
            "gliss needs a line of its own: a comma-list member sounds with every other member",
        ),
    ] {
        let error = oc_dsl::parse(source).unwrap_err().to_string();
        assert!(error.contains(needle), "{error}");
    }
}

#[test]
fn a_mark_cannot_share_the_note_with_another_pitch_control() {
    for (extra, needle) in [
        ("bend=50ct", "cannot share the note with bend="),
        (
            "vibrato={rate: 5Hz, depth: 20ct}",
            "cannot share the note with vibrato=",
        ),
        (
            "expression={pressure: 50%}",
            "cannot share the note with expression=",
        ),
        (
            "pitch_curve=\"0:0,1:100\"",
            "cannot share the note with pitch_curve=",
        ),
    ] {
        let source = format!(
            "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss {extra}\n    G4 q mf\n"
        );
        let error = oc_dsl::parse(&source).unwrap_err().to_string();
        assert!(error.contains(needle), "{extra}: {error}");
    }
}

#[test]
fn an_unknown_value_names_every_form() {
    for (value, needle) in [
        ("foo", "gliss=foo is not a glissando form"),
        ("G", "gliss=G is not a glissando form"),
        ("5s", "gliss=5000ms is out of range"),
        ("0ms", "gliss=0ms is out of range"),
    ] {
        let source = format!(
            "@project\n@track lead | instrument: synth/lead_saw\n  @phrase one [1:1 - 2:1]\n    C4 q mf gliss={value}\n    G4 q mf\n"
        );
        let error = oc_dsl::parse(&source).unwrap_err().to_string();
        assert!(error.contains(needle), "{value}: {error}");
        assert!(error.contains("gliss=run"), "{value}: {error}");
    }
}

#[test]
fn a_pattern_refuses_a_dangling_mark_and_keeps_a_good_one() {
    let dangling =
        "@project\n@pattern riff | instrument: synth/lead_saw\n  events:\n    C4 q mf gliss\n";
    let error = oc_dsl::parse(dangling).unwrap_err().to_string();
    assert!(
        error.contains("no note follows it in this pattern"),
        "{error}"
    );

    let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@pattern riff | instrument: synth/lead_saw\n  events:\n    C4 q mf gliss\n    G4 q mf\n";
    let project = parse(source);
    assert_eq!(oc_dsl::serialize(&project), source);
    assert_eq!(project.patterns[0].events.len(), 2);
}

#[test]
fn the_mark_is_exported_as_data_not_dropped() {
    let project = parse(SOURCE);
    let json = serde_json::to_string(&project).unwrap();
    assert!(json.contains("\"gliss\""), "the key is missing from export");
    for value in [
        "\"next\"",
        "\"D5\"",
        "\"run\"",
        "\"run_diatonic\"",
        "\"Time\":0.12",
    ] {
        assert!(json.contains(value), "{value} missing from export");
    }
}
