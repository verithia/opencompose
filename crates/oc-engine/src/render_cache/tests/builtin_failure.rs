//! Unknown instruments fail through every built-in rendering entry point.
use super::super::*;

const UNKNOWN: &str = "builtin.missing_instrument";
const SOURCE: &str = r#"@project
  tempo: 120
@preset lead_sound
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2}
@track lead | preset: lead_sound, instrument: synth/lead_square
  @phrase line [1:1 - 2:1]
    C4 q
"#;

fn events() -> Vec<MidiEvent> {
    vec![
        MidiEvent {
            time_samples: 0,
            track_index: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        },
        MidiEvent {
            time_samples: 1000,
            track_index: 0,
            event: MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            },
        },
    ]
}

#[test]
fn unknown_builtin_errors_from_both_public_wrappers() {
    let events = events();
    let refs: Vec<_> = events.iter().collect();
    let error = render_track_via_builtin_pub(UNKNOWN, None, &refs, 1024, 48_000).unwrap_err();
    assert!(error.contains(UNKNOWN), "{error}");
    let error =
        render_track_via_builtin_with_tempo(UNKNOWN, None, &refs, 1024, 48_000, 137.0).unwrap_err();
    assert!(error.contains(UNKNOWN), "{error}");
}

#[test]
fn valid_builtin_wrappers_keep_project_render_audio_identical() {
    let events = events();
    let refs: Vec<_> = events.iter().collect();
    let buffer =
        render_track_via_builtin_pub("builtin.test_tone", None, &refs, 1024, 48_000).unwrap();
    let timed =
        render_track_via_builtin_with_tempo("builtin.test_tone", None, &refs, 1024, 48_000, 137.0)
            .unwrap();
    let project = render_builtin_project(
        "builtin.test_tone",
        None,
        &refs,
        1024,
        0,
        48_000,
        137.0,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(buffer.iter().any(|sample| sample.abs() > 1e-4));
    assert_eq!(buffer, timed);
    assert_eq!(buffer, project);
}

#[test]
fn unknown_builtin_preset_failure_does_not_publish_or_reuse_silent_cache_audio() {
    let project = oc_dsl::parse(SOURCE).unwrap();
    let mut broken = project.clone();
    broken.presets[0].plugin = Some(UNKNOWN.into());
    let mut cache = RenderCache::new();
    let mut plugins = HashMap::new();
    let original = cache
        .render_full(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    assert!(original.iter().any(|sample| sample.abs() > 1e-4));
    let error = cache
        .render_incremental(&broken, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap_err();
    assert!(
        error.contains(UNKNOWN) && error.contains("lead_sound"),
        "{error}"
    );
    let recovered = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    assert_eq!(original, recovered);
    let error = RenderCache::new()
        .render_full(&broken, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap_err();
    assert!(
        error.contains(UNKNOWN) && error.contains("lead_sound"),
        "{error}"
    );
}

#[test]
fn unknown_builtin_live_source_and_native_performance_name_the_failed_plugin() {
    let mut project = oc_dsl::parse(SOURCE).unwrap();
    project.presets[0].plugin = Some(UNKNOWN.into());
    let preset = &project.presets[0];
    let error = match crate::block_source::BlockSource::new(
        Some(preset),
        &preset.params,
        Path::new("unused.sf2"),
        48_000,
        120.0,
        None,
    ) {
        Err(error) => error,
        Ok(_) => panic!("unknown instrument must not create a live source"),
    };
    assert!(
        error.contains(UNKNOWN) && error.contains("lead_sound"),
        "{error}"
    );
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let error = render_builtin_tracks_parallel(
        vec![BuiltinTrackJob {
            index: 0,
            plugin_id: UNKNOWN.into(),
            preset_name: "lead_sound".into(),
            params: Vec::new(),
            events: Vec::new(),
            performance: Some(Vec::new()),
        }],
        BuiltinRenderContext {
            total_samples: 1024,
            start_sample: 0,
            sample_rate: 48_000,
            tempo: 120.0,
            resource_root: None,
            clock: &clock,
            cancel: None,
            worker_options: crate::render_parallel::RenderWorkerOptions::default(),
        },
    )
    .unwrap_err();
    assert!(
        error.contains(UNKNOWN) && error.contains("lead_sound"),
        "{error}"
    );
}
