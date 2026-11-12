//! Hairpins: `cresc` / `dim` parse, survive `fmt`, and expand to track gain.

const SWELL: &str = r#"@ocp "0.2.0"
@project "swell"
  tempo: 120
  time_signature: 4/4

@track strings | instrument: strings/violin
  @phrase swell [1:1 - 3:1]
    @0 C4 w pp
    @2.0 cresc
    @2 E4 w
    @4 G4 w f
"#;

fn swell() -> oc_dsl::ast::Project {
    let project = oc_dsl::parse(SWELL).expect("the hairpin fixture parses");
    oc_dsl::validate(&project).expect("the hairpin fixture validates");
    project
}

#[test]
fn both_marks_parse_at_their_own_position() {
    let source = SWELL.replace("@2.0 cresc", "@2.0 cresc\n    @3.0 dim");
    let project = oc_dsl::parse(&source).expect("both marks parse");
    let hairpins = &project.tracks[0].phrases[0].hairpins;
    assert_eq!(hairpins.len(), 2);
    assert_eq!(hairpins[0].beat, 2.0);
    assert_eq!(hairpins[0].kind, oc_dsl::ast::HairpinKind::Crescendo);
    assert_eq!(hairpins[1].beat, 3.0);
    assert_eq!(hairpins[1].kind, oc_dsl::ast::HairpinKind::Diminuendo);
    // A mark takes no time: the notes keep their sequential positions.
    let starts: Vec<f64> = project.tracks[0].phrases[0]
        .events
        .iter()
        .map(|event| event.start_beat)
        .collect();
    assert_eq!(starts, vec![0.0, 2.0, 4.0]);
}

#[test]
fn a_hairpin_survives_fmt_and_shows_its_expansion() {
    let project = swell();
    let text = oc_dsl::serialize(&project);
    assert!(text.contains("@2 cresc"), "{text}");
    assert!(
        text.contains("// gain_automation: linear [1:3 - 2:1] {1:3=1, 1:4:3=8.46281}"),
        "the formatted text names the clip it will write:\n{text}"
    );
    let reopened = oc_dsl::parse(&text).expect("formatted hairpin project re-parses");
    assert_eq!(project, reopened);
    assert_eq!(oc_dsl::serialize(&reopened), text);
}

#[test]
fn the_expansion_is_a_gain_ramp_from_pp_to_f_on_the_track() {
    let project = swell();
    let expanded = oc_dsl::dynamics::expand(&project).expect("the fixture expands");
    let clips = &expanded.tracks[0].gain_automation;
    assert_eq!(clips.len(), 1);
    assert_eq!(
        clips[0].interpolation,
        oc_dsl::ast::GainInterpolation::Linear
    );
    assert_eq!(clips[0].ppq, 4);
    assert_eq!(
        clips[0].time_range.start,
        oc_dsl::ast::BarBeat::new(1, 3, 0)
    );
    assert_eq!(clips[0].time_range.end, oc_dsl::ast::BarBeat::new(2, 1, 0));
    assert_eq!(
        clips[0].points.last().unwrap().position,
        oc_dsl::ast::BarBeat::new(1, 4, 3),
        "the target lands one subdivision tick inside the span"
    );
    let values: Vec<f64> = clips[0]
        .points
        .iter()
        .map(|point| point.value.as_f64().expect("numeric gain"))
        .collect();
    // pp (33) to f (96) under the sampler's square law is +18.55 dB.
    assert_eq!(values.first().copied(), Some(1.0));
    let total_db = 20.0 * values.last().unwrap().log10();
    let expected = 40.0 * (96.0f64 / 33.0).log10();
    assert!(
        (total_db - expected).abs() < 1e-3,
        "the ramp reaches the target level: {total_db} dB, expected {expected}"
    );
    // The note inside the span takes the start level, so the ramp is the only
    // thing moving; the note that carries the target dynamic keeps it. The
    // levels reach the interpreter, not the AST, so expanding stays repeatable.
    let project = swell();
    let phrase = &project.tracks[0].phrases[0];
    let levels: Vec<(usize, u8)> = oc_dsl::dynamics::expand_phrase(phrase, &project.properties)
        .expect("the phrase expands")
        .into_iter()
        .flat_map(|expansion| expansion.levels)
        .collect();
    assert_eq!(levels, vec![(1, 33)]);
    let velocities: Vec<Option<u8>> = expanded.tracks[0].phrases[0]
        .events
        .iter()
        .map(|event| event.velocity)
        .collect();
    assert_eq!(velocities, vec![None, None, None]);
    assert_eq!(
        expanded.tracks[0].phrases[0].events[2].dynamics,
        Some(oc_dsl::ast::Dynamics::F)
    );
}

#[test]
fn expanding_twice_is_expanding_once() {
    let project = swell();
    let once = oc_dsl::dynamics::expand(&project).expect("the fixture expands");
    let twice = oc_dsl::dynamics::expand(&once).expect("the expansion is repeatable");
    assert_eq!(*once, *twice);
    assert_eq!(twice.tracks[0].gain_automation.len(), 1);
    // The generated clip is valid state on its own: a project that carries it
    // validates without an overlap complaint.
    oc_dsl::validate(&once).expect("the expanded project validates");
}

#[test]
fn dim_writes_a_falling_ramp_under_the_same_rule() {
    let source = SWELL
        .replace("@2.0 cresc", "@2.0 dim")
        .replace("    @0 C4 w pp", "    @0 C4 w f")
        .replace("    @4 G4 w f", "    @4 G4 w pp");
    let project = oc_dsl::parse(&source).expect("the dim fixture parses");
    oc_dsl::validate(&project).expect("the dim fixture validates");
    let expanded = oc_dsl::dynamics::expand(&project).expect("the dim fixture expands");
    let values: Vec<f64> = expanded.tracks[0].gain_automation[0]
        .points
        .iter()
        .map(|point| point.value.as_f64().unwrap())
        .collect();
    assert_eq!(values.first().copied(), Some(1.0));
    assert!(values.last().copied().unwrap() < 0.2, "{values:?}");
    let total_db = 20.0 * values.last().unwrap().log10();
    let expected = -40.0 * (96.0f64 / 33.0).log10();
    assert!(
        (total_db - expected).abs() < 1e-3,
        "the ramp falls {expected} dB: {total_db}"
    );
}

#[test]
fn a_mark_with_no_following_notes_is_refused_by_name() {
    let source = r#"@project
  tempo: 120
@track strings | instrument: strings/violin
  @phrase swell [1:1 - 3:1]
    C4 w mf
    @4.0 cresc
"#;
    let project = oc_dsl::parse(source).expect("the fixture parses");
    let errors = oc_dsl::validate(&project).expect_err("a mark past the notes cannot expand");
    let message = errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("cresc at beat 4 has no following notes"),
        "{message}"
    );
}

#[test]
fn a_mark_without_a_position_or_over_hits_is_refused_by_name() {
    let missing = oc_dsl::parse(
        "@project\n@track t | instrument: strings/violin\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n    cresc\n",
    )
    .expect_err("a hairpin needs a position");
    assert!(
        missing
            .to_string()
            .contains("hairpin needs a position: write `@2.0 cresc` or `@2.0 dim`"),
        "{missing}"
    );

    let hits = oc_dsl::parse(
        "@project\n@track d | instrument: drums/acoustic_kit\n  @pattern beat [1:1 - 2:1]\n    @0 cresc\n    kick 1:0 f\n",
    )
    .expect_err("drum hits cannot swell");
    assert!(
        hits.to_string().contains("a hairpin needs sustained notes"),
        "{hits}"
    );
}

#[test]
fn export_shows_the_expanded_clip_on_the_track() {
    let project = swell();
    let expanded = oc_dsl::dynamics::expand(&project).expect("the fixture expands");
    let json = serde_json::to_value(&expanded).unwrap();
    let clips = json["tracks"][0]["gain_automation"]
        .as_array()
        .expect("the export carries the generated clip");
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0]["interpolation"], "Linear");
    assert_eq!(clips[0]["points"][0]["value"]["Number"], 1.0);
    // The mark itself survives, so the export is not lossy in either direction.
    assert_eq!(
        json["tracks"][0]["phrases"][0]["hairpins"][0]["kind"],
        "crescendo"
    );
    let restored: oc_dsl::ast::Project = serde_json::from_value(json).unwrap();
    assert_eq!(restored, *expanded);
}

// ---- Pattern hairpins -------------------------------------------------------

const PATTERN_SWELL: &str = r#"@ocp "0.2.0"
@project "pattern swell"
  tempo: 120
  time_signature: 4/4

@pattern line | instrument: strings/violin
  events:
    @0 C4 w pp
    @2.0 cresc
    @2 E4 w
    @4 G4 w f

@track strings | instrument: strings/violin
  clip: line [1:1 - 3:1]
"#;

fn pattern_swell() -> oc_dsl::ast::Project {
    let project = oc_dsl::parse(PATTERN_SWELL).expect("the pattern swell parses");
    oc_dsl::validate(&project).expect("the pattern swell validates");
    project
}

#[test]
fn pattern_hairpins_parse_and_survive_fmt() {
    let project = pattern_swell();
    let pattern = &project.patterns[0];
    assert_eq!(pattern.hairpins.len(), 1);
    assert_eq!(pattern.hairpins[0].beat, 2.0);
    assert_eq!(
        pattern.hairpins[0].kind,
        oc_dsl::ast::HairpinKind::Crescendo
    );
    // A mark takes no time: the notes keep the positions they were written at.
    let starts: Vec<f64> = pattern
        .events
        .iter()
        .map(|event| event.start_beat)
        .collect();
    assert_eq!(starts, vec![0.0, 2.0, 4.0]);

    let text = oc_dsl::serialize(&project);
    assert!(text.contains("    @2 cresc"), "{text}");
    let reopened = oc_dsl::parse(&text).expect("the formatted pattern re-parses");
    assert_eq!(project, reopened);
    assert_eq!(oc_dsl::serialize(&reopened), text);
}

#[test]
fn a_pattern_mark_expands_once_per_placement() {
    let project = pattern_swell();
    let expanded = oc_dsl::dynamics::expand(&project).expect("the placement expands");
    let clips = &expanded.tracks[0].gain_automation;
    assert_eq!(clips.len(), 1);
    assert_eq!(
        clips[0].time_range.start,
        oc_dsl::ast::BarBeat::new(1, 3, 0),
        "the mark sits two beats into the clip"
    );
    assert_eq!(clips[0].time_range.end, oc_dsl::ast::BarBeat::new(2, 1, 0));
    let values: Vec<f64> = clips[0]
        .points
        .iter()
        .map(|point| point.value.as_f64().expect("numeric gain"))
        .collect();
    assert_eq!(values.first().copied(), Some(1.0));
    let total_db = 20.0 * values.last().unwrap().log10();
    let expected = 40.0 * (96.0f64 / 33.0).log10();
    assert!(
        (total_db - expected).abs() < 1e-3,
        "the ramp reaches f from pp: {total_db} dB, expected {expected}"
    );

    let levels: Vec<(usize, u8)> = oc_dsl::dynamics::expand_pattern_with(
        &project.patterns[0],
        &oc_dsl::timing::ProjectClock::new(&project.properties).unwrap(),
        0.0,
        None,
        Some(0.0),
    )
    .expect("the content expands")
    .into_iter()
    .flat_map(|expansion| expansion.levels)
    .collect();
    assert_eq!(levels, vec![(1, 33)], "the note inside the span takes pp");

    // A second placement is a second ramp, at that placement's position.
    let mut twice = pattern_swell();
    twice.tracks[0].push_pattern_clip(oc_dsl::ast::Clip {
        pattern: "line".into(),
        time_range: oc_dsl::ast::TimeRange {
            start: oc_dsl::ast::BarBeat::new(3, 1, 0),
            end: oc_dsl::ast::BarBeat::new(5, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    oc_dsl::validate(&twice).expect("two placements validate");
    let expanded = oc_dsl::dynamics::expand(&twice).expect("both placements expand");
    let starts: Vec<_> = expanded.tracks[0]
        .gain_automation
        .iter()
        .map(|clip| clip.time_range.start)
        .collect();
    assert_eq!(
        starts,
        vec![
            oc_dsl::ast::BarBeat::new(1, 3, 0),
            oc_dsl::ast::BarBeat::new(3, 3, 0),
        ]
    );
}

#[test]
fn a_pattern_without_hairpins_serializes_as_before() {
    let source = "@project\n  tempo: 120\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n  hits:\n    kick 1:0 mf\n";
    let project = oc_dsl::parse(source).expect("the fixture parses");
    let text = oc_dsl::serialize(&project);
    assert!(!text.contains("cresc") && !text.contains("dim"), "{text}");
    assert_eq!(oc_dsl::serialize(&oc_dsl::parse(&text).unwrap()), text);
}

#[test]
fn a_loop_seam_cuts_each_pattern_ramp_at_its_own_repetition() {
    let source = r#"@ocp "0.2.0"
@project "looped swell"
  tempo: 120
  time_signature: 4/4

@pattern line | instrument: strings/violin
  events:
    @0 C4 w pp
    @2.0 cresc
    @2 E4 w
    @4 G4 w
    @6 B4 w f

@track strings | instrument: strings/violin
  clip: line [1:1 - 3:1] offset: 2q loop: 4q
"#;
    let project = oc_dsl::parse(source).expect("the looped swell parses");
    let expanded = oc_dsl::dynamics::expand(&project).expect("both repetitions expand");
    let spans: Vec<_> = expanded.tracks[0]
        .gain_automation
        .iter()
        .map(|clip| (clip.time_range.start, clip.time_range.end))
        .collect();
    assert_eq!(
        spans,
        vec![
            // The first repetition runs from the clip head to the loop seam.
            (
                oc_dsl::ast::BarBeat::new(1, 1, 0),
                oc_dsl::ast::BarBeat::new(2, 1, 0)
            ),
            // The second runs from the seam to the clip end.
            (
                oc_dsl::ast::BarBeat::new(2, 1, 0),
                oc_dsl::ast::BarBeat::new(3, 1, 0)
            ),
        ]
    );
}
