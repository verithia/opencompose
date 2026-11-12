use super::*;

fn pattern_project() -> Project {
    oc_dsl::parse(
        r#"@project
@pattern riff | instrument: synth/lead
  events:
    @0 C4 q mf
@track lead | instrument: synth/lead
  clip: riff [1:1 - 2:1]
"#,
    )
    .unwrap()
}

#[test]
fn source_lane_planning_does_not_add_an_empty_base_beside_pattern_clips() {
    let project = pattern_project();
    let (lanes, targets) = split_source_lanes(&project).unwrap();
    assert_eq!(targets, vec![0]);
    assert_eq!(lanes.tracks.len(), 1);
    assert_eq!(lanes.tracks[0].pattern_clip_count(), 1);
}

#[test]
fn source_lane_planning_preserves_track_notes_alongside_a_pattern() {
    let mut project = pattern_project();
    let notes = oc_dsl::parse("@project\n@track notes | instrument: synth/lead\n  @phrase held [1:1 - 2:1]\n    E4 w mf\n").unwrap();
    project.tracks[0].phrases = notes.tracks[0].phrases.clone();
    let (lanes, targets) = split_source_lanes(&project).unwrap();
    assert_eq!(targets, vec![0, 0]);
    assert_eq!(
        lanes
            .tracks
            .iter()
            .map(|track| track.phrases.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(
        lanes
            .tracks
            .iter()
            .map(|track| track.pattern_clip_count())
            .sum::<usize>(),
        1
    );
}

#[test]
fn source_lane_planning_keeps_inline_patterns_and_empty_live_input_sources() {
    let mut project = pattern_project();
    let pattern = project.patterns[0].clone();
    project.tracks[0]
        .clips
        .set_pattern_clips(std::iter::empty());
    project.tracks[0].inline_patterns.push(pattern);
    let (lanes, targets) = split_source_lanes(&project).unwrap();
    assert_eq!(targets, vec![0]);
    assert_eq!(lanes.tracks[0].inline_patterns.len(), 1);
    project.tracks[0].inline_patterns.clear();
    let (lanes, targets) = split_source_lanes(&project).unwrap();
    assert_eq!(targets, vec![0]);
    assert_eq!(lanes.tracks.len(), 1);
}

#[test]
fn streaming_and_offline_share_the_exact_source_plan() {
    let mut project = pattern_project();
    let second = project.tracks[0].pattern_clip(0).unwrap().clone();
    project.tracks[0].push_pattern_clip(second);
    project.patterns[0].preset = Some("oc_default_pad".into());
    project.patterns[0].instrument = oc_dsl::ast::InstrumentPath::new("unknown", "unknown");
    let inline = project.patterns[0].clone();
    project.tracks[0].inline_patterns.push(inline);
    let inserts = oc_dsl::parse(
        "@project\n@track fx | instrument: synth/lead\n  effects:\n    - gain: factor=0.5\n",
    )
    .unwrap()
    .tracks
    .remove(0)
    .effects;
    project.patterns[0].effects = inserts.clone();
    project.tracks[0].effects = inserts;
    let expected = oc_engine::source_lanes::plan(&project).unwrap();
    let (actual, targets) = split_source_lanes(&project).unwrap();
    assert_eq!(actual, *expected.tracks());
    assert_eq!(targets, expected.targets);
    assert_eq!(actual.tracks.len(), 3);
    assert_eq!(actual.tracks[0].instrument, project.tracks[0].instrument);
    assert_eq!(actual.tracks[0].preset, project.tracks[0].preset);
}

#[test]
fn shared_output_planning_coalesces_compatible_placements_and_rejects_conflicting_inserts() {
    let mut project = pattern_project();
    project.tracks[0].output_pair = Some(0);
    let clip = project.tracks[0].pattern_clip(0).unwrap().clone();
    project.tracks[0].push_pattern_clip(clip);
    let (actual, targets) = split_source_lanes(&project).unwrap();
    let expected = oc_engine::source_lanes::plan(&project).unwrap();
    assert_eq!(actual, *expected.tracks());
    assert_eq!(targets, vec![0]);
    assert_eq!(actual.tracks[0].pattern_clip_count(), 2);
    let mut different = project.patterns[0].clone();
    different.name = "different".into();
    different.effects = oc_dsl::parse(
        "@project\n@track fx | instrument: synth/lead\n  effects:\n    - gain: factor=0.5\n",
    )
    .unwrap()
    .tracks
    .remove(0)
    .effects;
    project.patterns.push(different);
    project.tracks[0].pattern_clip_mut(1).unwrap().pattern = "different".into();
    let live = split_source_lanes(&project).unwrap_err();
    let offline = oc_engine::source_lanes::plan(&project).unwrap_err();
    assert_eq!(live, offline);
    assert!(live.contains("Unsupported source chain") && live.contains("separate presets"));
}
