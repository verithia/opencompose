//! `@audio_clip` syntax, round-trip and validation.

use oc_dsl::ast::{Project, StretchMode};
use oc_dsl::{parse, serialize};

const SOURCE: &str = r#"@ocp "0.2.0"
@project "Audio"
  tempo: 120

@audio_clip intro [1:1 - 5:1] | track: tape
  file: "media/intro.wav"
  offset: 250ms
  gain: -3dB
  fade_in: 5ms
  fade_out: 1.5s

@audio_clip outro [9:1 - 10:1] | track: tape
  file: "media/outro.flac"

@track tape | instrument: audio/clip
  mix: vol=-6dB
"#;

fn round_trip(project: &Project) -> Project {
    parse(&serialize(project)).unwrap()
}

#[test]
fn audio_clip_round_trips_through_parse_and_format() {
    let project = parse(SOURCE).unwrap();
    assert_eq!(project.audio_clip_count(), 2);
    let intro = project.audio_clip(0).unwrap();
    assert_eq!(intro.name, "intro");
    assert_eq!(intro.track, "tape");
    assert_eq!(intro.file, "media/intro.wav");
    assert_eq!(intro.offset_seconds, 0.25);
    assert_eq!(intro.gain_db, -3.0);
    assert_eq!(intro.fade_in_seconds, 0.005);
    assert_eq!(intro.fade_out_seconds, 1.5);
    assert_eq!(intro.time_range.start, oc_dsl::ast::BarBeat::new(1, 1, 0));
    assert_eq!(intro.time_range.end, oc_dsl::ast::BarBeat::new(5, 1, 0));

    let formatted = serialize(&project);
    assert!(formatted.contains("@audio_clip intro [1:1 - 5:1] | track: tape"));
    assert!(formatted.contains("  file: \"media/intro.wav\""));
    assert!(formatted.contains("  offset: 0.25s"));
    assert!(formatted.contains("  gain: -3dB"));
    assert!(formatted.contains("  fade_in: 0.005s"));
    assert!(formatted.contains("  fade_out: 1.5s"));
    assert_eq!(round_trip(&project), project);
    assert_eq!(serialize(&round_trip(&project)), formatted);
}

#[test]
fn projects_without_audio_clips_format_unchanged() {
    let source = "@ocp \"0.2.0\"\n@project \"Plain\"\n  tempo: 90\n\n@track keys | instrument: piano/acoustic_grand\n";
    let project = parse(source).unwrap();
    let formatted = serialize(&project);
    assert!(!formatted.contains("audio_clip"));
    assert_eq!(round_trip(&project), project);
}

#[test]
fn audio_clips_never_change_existing_track_output() {
    let with_clip = parse(SOURCE).unwrap();
    let mut without_clip = with_clip.clone();
    without_clip.clear_audio_clips();
    let without_text = serialize(&without_clip);
    assert!(!without_text.contains("audio_clip"));
    assert_eq!(round_trip(&without_clip), without_clip);
}

#[test]
fn stretch_defaults_to_bypass_and_is_omitted() {
    let project = parse(SOURCE).unwrap();
    for clip in project.audio_clips() {
        assert_eq!(clip.stretch, 1.0);
        assert_eq!(clip.stretch_mode, StretchMode::Musical);
        assert!(!clip.tempo_follow);
    }
    let formatted = serialize(&project);
    assert!(!formatted.contains("stretch:"), "{formatted}");
    assert!(!formatted.contains("mode:"), "{formatted}");
    assert!(!formatted.contains("tempo_follow"), "{formatted}");
    assert_eq!(round_trip(&project), project);
}

#[test]
fn stretch_parameters_round_trip() {
    let source = r#"@ocp "0.2.0"
@project "Stretch"
  tempo: 120

@audio_clip fast [1:1 - 5:1] | track: tape
  file: "media/intro.wav"
  stretch: 1.25
  mode: percussive

@audio_clip follow [9:1 - 10:1] | track: tape
  file: "media/outro.flac"
  tempo_follow: true
  mode: off

@track tape | instrument: audio/clip
"#;
    let project = parse(source).unwrap();
    let fast = project.audio_clip(0).unwrap();
    assert_eq!(fast.stretch, 1.25);
    assert_eq!(fast.stretch_mode, StretchMode::Percussive);
    assert!(!fast.tempo_follow);
    let follow = project.audio_clip(1).unwrap();
    assert_eq!(follow.stretch, 1.0);
    assert_eq!(follow.stretch_mode, StretchMode::Off);
    assert!(follow.tempo_follow);

    let formatted = serialize(&project);
    assert!(formatted.contains("  stretch: 1.25"), "{formatted}");
    assert!(formatted.contains("  mode: percussive"), "{formatted}");
    assert!(formatted.contains("  mode: off"), "{formatted}");
    assert!(formatted.contains("  tempo_follow: true"), "{formatted}");
    assert_eq!(round_trip(&project), project);
    assert_eq!(serialize(&round_trip(&project)), formatted);
}

#[test]
fn take_fields_round_trip_and_are_omitted_when_unset() {
    let project = parse(SOURCE).unwrap();
    let formatted = serialize(&project);
    assert!(!formatted.contains("take_group:"), "{formatted}");
    assert!(!formatted.contains("take:"), "{formatted}");
    assert!(!formatted.contains("muted:"), "{formatted}");

    let source = r#"@ocp "0.2.0"
@project "Takes"
  tempo: 120

@audio_clip take2 [1:1 - 3:1] | track: tape
  file: "media/take-vox-2.wav"
  take_group: "vox take 42"
  take: 2
  muted: true

@track tape | instrument: audio/clip
"#;
    let project = parse(source).unwrap();
    let clip = project.audio_clip(0).unwrap();
    assert_eq!(clip.take_group.as_deref(), Some("vox take 42"));
    assert_eq!(clip.take, Some(2));
    assert!(clip.muted);
    let formatted = serialize(&project);
    assert!(
        formatted.contains("  take_group: \"vox take 42\""),
        "{formatted}"
    );
    assert!(formatted.contains("  take: 2"), "{formatted}");
    assert!(formatted.contains("  muted: true"), "{formatted}");
    assert_eq!(round_trip(&project), project);
    assert_eq!(serialize(&round_trip(&project)), formatted);
}

#[test]
fn take_fields_are_json_compatible_in_both_directions() {
    let project = parse(SOURCE).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    assert!(!json.contains("take_group"), "{json}");
    assert!(!json.contains("\"take\""), "{json}");
    assert!(!json.contains("\"muted\""), "{json}");
    // A document written before the fields existed loads with defaults.
    let old: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(old, project);

    let mut with_takes = project.clone();
    with_takes.audio_clip_mut(0).unwrap().take_group = Some("vox take 7".into());
    with_takes.audio_clip_mut(0).unwrap().take = Some(1);
    with_takes.audio_clip_mut(0).unwrap().muted = true;
    let json = serde_json::to_string(&with_takes).unwrap();
    assert!(json.contains("\"take_group\":\"vox take 7\""), "{json}");
    let restored: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, with_takes);
}

#[test]
fn validation_rejects_non_positive_or_non_finite_stretch() {
    for value in [0.0, -1.5, f64::NAN, f64::INFINITY] {
        let mut project = parse(SOURCE).unwrap();
        project.audio_clip_mut(0).unwrap().stretch = value;
        let errors = oc_dsl::validate(&project).unwrap_err();
        let messages = errors.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(
            messages.iter().any(|message| message.contains("stretch")),
            "{value}: {messages:?}"
        );
    }
}

#[test]
fn validation_rejects_unknown_track_and_duplicate_names() {
    let mut project = parse(SOURCE).unwrap();
    project.audio_clip_mut(0).unwrap().track = "missing".into();
    project.audio_clip_mut(1).unwrap().name = "intro".into();
    let errors = oc_dsl::validate(&project).unwrap_err();
    let messages = errors.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(messages.iter().any(|message| message.contains("missing")));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("defined multiple times"))
    );
}

#[test]
fn validation_rejects_invalid_media_parameters() {
    let mut project = parse(SOURCE).unwrap();
    project.audio_clip_mut(0).unwrap().file = String::new();
    project.audio_clip_mut(0).unwrap().offset_seconds = -1.0;
    project.audio_clip_mut(0).unwrap().fade_in_seconds = -0.5;
    project.audio_clip_mut(1).unwrap().file = "/absolute/path.wav".into();
    project.audio_clip_mut(1).unwrap().gain_db = f64::NAN;
    project.audio_clip_mut(1).unwrap().time_range.end =
        project.audio_clip(1).unwrap().time_range.start;
    let errors = oc_dsl::validate(&project).unwrap_err();
    let messages = errors.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("empty path"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("must be relative"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("end must be after start"))
    );
    assert!(
        messages
            .iter()
            .filter(|message| message.contains("audio clip 'intro'"))
            .count()
            >= 2
    );
}
