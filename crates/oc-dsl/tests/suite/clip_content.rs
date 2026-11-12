//! Slip and loop fields: the `offset` on a clip, the round trip, and the
//! refusals a project cannot express.
//!
//! Both fields are absent by default, so a project that uses neither keeps its
//! text byte for byte. A project that uses them round-trips through parse,
//! serialize and parse with an equal AST.

use oc_dsl::ast::{BarBeat, Clip, TimeRange};
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
"#;

const SLIPPED: &str = r#"@ocp "0.2.0"

@project "Slipped"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [2:1 - 6:1] offset: 1.5q
"#;

#[test]
fn a_clip_without_slip_formats_without_the_field() {
    let project = parse(PLAIN).expect("plain fixture parses");
    let clip = project.tracks[0].pattern_clip(0).unwrap();
    assert_eq!(clip.offset_quarters, 0.0);
    let formatted = serialize(&project);
    assert_eq!(formatted, PLAIN, "a project without slip keeps its text");
    assert_eq!(parse(&formatted).expect("reparse"), project);
}

#[test]
fn slip_parses_round_trips_and_keeps_its_text() {
    let project = parse(SLIPPED).expect("slipped fixture parses");
    let clip = project.tracks[0].pattern_clip(0).unwrap();
    assert_eq!(clip.pattern, "riff");
    assert!((clip.offset_quarters - 1.5).abs() < 1e-9);
    assert_eq!(clip.time_range.start, BarBeat::new(2, 1, 0));
    assert_eq!(clip.time_range.end, BarBeat::new(6, 1, 0));

    let formatted = serialize(&project);
    assert!(
        formatted.contains("  clip: riff [2:1 - 6:1] offset: 1.5q\n"),
        "{formatted}"
    );
    let reopened = parse(&formatted).expect("serialized slip re-parses");
    assert_eq!(reopened, project);
    assert_eq!(serialize(&reopened), formatted);
}

#[test]
fn a_fractional_slip_survives_the_round_trip() {
    let source = PLAIN.replace(
        "clip: riff [2:1 - 6:1]",
        "clip: riff [2:1 - 6:1] offset: 0.25q",
    );
    let project = parse(&source).expect("quarter-beat slip parses");
    assert!((project.tracks[0].pattern_clip(0).unwrap().offset_quarters - 0.25).abs() < 1e-9);
    assert_eq!(parse(&serialize(&project)).expect("reparse"), project);
}

#[test]
fn a_negative_slip_is_refused_by_validation() {
    let mut project = parse(PLAIN).expect("plain fixture parses");
    *project.tracks[0].pattern_clip_mut(0).unwrap() = Clip {
        pattern: "riff".to_string(),
        time_range: TimeRange {
            start: BarBeat::new(2, 1, 0),
            end: BarBeat::new(6, 1, 0),
        },
        offset_quarters: -1.0,
        loop_quarters: 0.0,
        id: None,
    };
    let errors = oc_dsl::validate(&project).expect_err("a negative slip must fail");
    assert!(
        errors
            .iter()
            .any(|error| format!("{error}").contains("offset")),
        "{errors:?}"
    );
}

#[test]
fn pattern_content_is_the_written_span() {
    let project = parse(SLIPPED).expect("slipped fixture parses");
    let pattern = &project.patterns[0];
    // Four quarter notes starting at beat 0 span four quarters.
    assert!((pattern.content_quarters() - 4.0).abs() < 1e-9);
}

#[test]
fn a_declared_pattern_length_round_trips_and_bounds_it() {
    let source = PLAIN.replace(
        "@pattern riff | instrument: piano/acoustic_grand",
        "@pattern riff | instrument: piano/acoustic_grand, bars: 2",
    );
    let project = parse(&source).expect("bars parses");
    assert_eq!(project.patterns[0].bars, Some(2.0));
    assert!((project.patterns[0].length_quarters() - 8.0).abs() < 1e-9);

    let formatted = serialize(&project);
    assert!(formatted.contains("bars: 2"), "{formatted}");
    assert_eq!(parse(&formatted).expect("reparse"), project);
}

#[test]
fn a_pattern_without_bars_occupies_its_last_written_bar() {
    let project = parse(PLAIN).expect("plain fixture parses");
    // Four quarters are exactly one bar.
    assert_eq!(project.patterns[0].length_quarters(), 4.0);

    let mut empty = project.clone();
    empty.patterns[0].events.clear();
    assert_eq!(
        empty.patterns[0].length_quarters(),
        4.0,
        "an empty pattern still has one bar of room"
    );
}

#[test]
fn a_pattern_length_of_zero_is_refused() {
    let source = PLAIN.replace(
        "@pattern riff | instrument: piano/acoustic_grand",
        "@pattern riff | instrument: piano/acoustic_grand, bars: 0",
    );
    let error = parse(&source)
        .expect_err("zero bars must be refused")
        .to_string();
    assert!(error.contains("above zero"), "{error}");
}

#[test]
fn a_audio_clip_loop_round_trips() {
    let source = r#"@ocp "0.2.0"

@project "Looped"
  tempo: 120
  time_signature: 4/4

@audio_clip hit [1:1 - 5:1] | track: tape
  file: "media/hit.wav"
  offset: 0.5s
  loop: 1.5s

@track tape | instrument: audio/clip
"#;
    let project = parse(source).expect("looped audio clip parses");
    let clip = project.audio_clip(0).unwrap();
    assert!((clip.offset_seconds - 0.5).abs() < 1e-9);
    assert!((clip.loop_seconds - 1.5).abs() < 1e-9);
    let formatted = serialize(&project);
    assert!(formatted.contains("  loop: 1.5s\n"), "{formatted}");
    let reopened = parse(&formatted).expect("serialized loop re-parses");
    assert_eq!(reopened, project);
    assert_eq!(serialize(&reopened), formatted);
}

#[test]
fn a_pattern_clip_loop_round_trips() {
    let source = r#"@ocp "0.2.0"

@project "Looped pattern"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [1:1 - 2:1] offset: 0.5q loop: 1q
"#;
    let project = parse(source).expect("looped pattern clip parses");
    let clip = project.tracks[0].pattern_clip(0).unwrap();
    assert!((clip.offset_quarters - 0.5).abs() < 1e-9);
    assert!((clip.loop_quarters - 1.0).abs() < 1e-9);
    let formatted = serialize(&project);
    assert!(
        formatted.contains("  clip: riff [1:1 - 2:1] offset: 0.5q loop: 1q\n"),
        "{formatted}"
    );
    let reopened = parse(&formatted).expect("serialized pattern loop re-parses");
    assert_eq!(reopened, project);
}

#[test]
fn a_loop_that_spans_the_whole_clip_is_refused() {
    let source = r#"@ocp "0.2.0"

@project "Too long"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [1:1 - 2:1] loop: 4q
"#;
    let project = parse(source).expect("the fixture parses");
    let errors = oc_dsl::validate(&project).expect_err("a full-length loop must fail");
    assert!(
        errors
            .iter()
            .any(|error| format!("{error}").contains("loop")),
        "{errors:?}"
    );
}
