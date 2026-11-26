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
            full_program: None,
            trim_preset: None,
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

#[test]
fn worker_normalization_matches_authoritative_parameters_and_exact_pcm() {
    let project = oc_dsl::parse(r#"@project
  tempo: 120
@preset measured
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 0, osc.b.level: 0, noise.level: 0, unison.voices: 1, output.gain: 0.217381, amp.release: 0.037s}
@track lead | preset: measured, instrument: synth/keys
"#).unwrap();
    let preset = &project.presets[0];
    let track = &project.tracks[0];
    assert!(crate::loudness::can_prepare_in_parallel(preset));
    let untrimmed = crate::preset::builtin_track_params_untrimmed(&project, Some(preset), track);
    let events = events();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let rendered = render_builtin_tracks_parallel(
        vec![BuiltinTrackJob {
            index: 0,
            plugin_id: oc_synth::synth::PLUGIN_ID.into(),
            preset_name: preset.name.clone(),
            params: untrimmed,
            trim_preset: Some(preset.clone()),
            full_program: None,
            events: events.clone(),
            performance: None,
        }],
        BuiltinRenderContext {
            total_samples: 1600,
            start_sample: 0,
            sample_rate: 8000,
            tempo: 120.0,
            resource_root: None,
            clock: &clock,
            cancel: None,
            worker_options: crate::render_parallel::RenderWorkerOptions::default(),
        },
    )
    .unwrap();
    // The worker performs the first calibration; the ordinary parameter owner
    // then retrieves exactly the same trim for the reference rendering path.
    let authoritative = crate::preset::builtin_track_params_at(&project, Some(preset), track, None);
    let refs: Vec<_> = events.iter().collect();
    let (reference, latency) = render_builtin_project_with_latency(
        oc_synth::synth::PLUGIN_ID,
        Some(&authoritative),
        &refs,
        1600,
        0,
        8000,
        120.0,
        None,
        Some(&clock),
        None,
    )
    .unwrap();
    assert_eq!(rendered[0].audio, reference);
    assert_eq!(rendered[0].latency, latency);
    assert!(reference.iter().any(|sample| *sample != 0.0));
    assert!(
        authoritative
            .iter()
            .any(|(key, _)| key == crate::loudness::TRIM_PARAM)
    );
}

#[test]
fn unsafe_normalization_cannot_enter_a_builtin_worker() {
    let mut project = oc_dsl::parse(SOURCE).unwrap();
    project.presets[0].plugin = Some(oc_synth::synth::PLUGIN_ID.into());
    project.presets[0].params.push((
        "open_synth_state_file".into(),
        ParamValue::String("unreadable.json".into()),
    ));
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let result = render_builtin_tracks_parallel(
        vec![BuiltinTrackJob {
            index: 0,
            plugin_id: oc_synth::synth::PLUGIN_ID.into(),
            preset_name: "unsafe".into(),
            params: Vec::new(),
            trim_preset: Some(project.presets[0].clone()),
            full_program: None,
            events: Vec::new(),
            performance: None,
        }],
        BuiltinRenderContext {
            total_samples: 1600,
            start_sample: 0,
            sample_rate: 8000,
            tempo: 120.0,
            resource_root: None,
            clock: &clock,
            cancel: None,
            worker_options: crate::render_parallel::RenderWorkerOptions::default(),
        },
    );
    assert!(result.unwrap_err().contains("requires serial preparation"));
}
