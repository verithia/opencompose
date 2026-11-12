//! Native and resource-resolution cache tests.

use super::super::*;

#[test]
fn source_midi_defaults_do_not_bake_in_graph_fader_or_pan() {
    let project = oc_dsl::parse("@project\n@track t | instrument: synth/lead_square\n  mix: vol=-9dB, pan=-0.5\n  @phrase p [1:1 - 2:1]\n    @0 C4 q v=90\n").unwrap();
    let source = super::note_source_project(&project);
    assert_eq!(project.tracks[0].mix.volume_db, -9.0);
    assert_ne!(project.tracks[0].mix.pan, 0.0);
    let events = crate::interpret::try_interpret(&source, &Default::default()).unwrap();
    for controller in [7, 10] {
        let values: Vec<_> = events
            .iter()
            .filter_map(|event| match event.event {
                crate::interpret::MidiMessage::ControlChange {
                    controller: cc,
                    value,
                    ..
                } if cc == controller => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![if controller == 7 { 100 } else { 64 }]);
    }
}

#[test]
fn performance_preflight_rejects_lossy_backends_and_shared_instances() {
    let mut project = oc_dsl::parse("@project\n@preset p\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track t | preset: p, instrument: synth/lead_square\n  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 37; }\n").unwrap();
    let instances = std::collections::HashMap::new();
    let error = super::prepare_performance_tracks(&project, 48000, &instances).unwrap_err();
    assert!(error.contains("MIDI1 CLAP"), "{error}");
    project.presets[0].source = Some("clap".into());
    project.presets[0].plugin = Some("org.example.instrument".into());
    let mut second = project.tracks[0].clone();
    second.name = "second".into();
    project.tracks.push(second);
    let error = super::prepare_performance_tracks(&project, 48000, &instances).unwrap_err();
    assert!(error.contains("cannot share"), "{error}");
    project.tracks.pop();
    let error = super::prepare_performance_tracks(&project, 48000, &instances).unwrap_err();
    assert!(error.contains("instance is missing"), "{error}");
    project.tracks[0].performance.as_mut().unwrap().events[0].channel = 16;
    let error = super::prepare_performance_tracks(&project, 48000, &instances).unwrap_err();
    assert!(error.contains("Invalid performance project"), "{error}");
}

#[test]
fn audio_region_only_project_refreshes_and_relocates() {
    let root = std::env::temp_dir().join(format!("pgen-region-cache-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut wav = hound::WavWriter::create(
        root.join("source.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..800 {
        wav.write_sample(0.1_f32).unwrap();
    }
    wav.finalize().unwrap();
    let mut document = serde_json::json!({"sample_rate":8000,"regions":[{"file":"source.wav",
            "region":{"timeline_start":80,"source_start":10,"length":400,"gain":1.0,"envelope":[]}}]});
    std::fs::write(root.join("regions.json"), document.to_string()).unwrap();
    let project = oc_dsl::parse(
        r#"@project "Regions"
  tempo: 120
  time_signature: 4/4
@preset audio
  source: builtin
  plugin: "builtin.audio_regions"
  params: {audio_region_state_file: "regions.json"}
@track audio | preset: audio
"#,
    )
    .unwrap();
    let mut cache = super::RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let sf = std::path::Path::new("unused.sf2");
    let mut plugins = std::collections::HashMap::new();
    let first = cache.render_full(&project, sf, 8000, &mut plugins).unwrap();
    assert!(first.iter().any(|v| v.abs() > 0.01));
    assert!(first[..160].iter().all(|v| *v == 0.0));
    let relocated = root.with_extension("moved");
    std::fs::rename(&root, &relocated).unwrap();
    cache.set_resource_root(Some(relocated.clone()));
    assert_eq!(
        first,
        cache.render_full(&project, sf, 8000, &mut plugins).unwrap()
    );
    document["regions"][0]["region"]["gain"] = serde_json::json!(0.0);
    std::fs::write(relocated.join("regions.json"), format!("{}\n ", document)).unwrap();
    assert!(cache.is_dirty());
    let silent = cache
        .render_incremental(&project, sf, 8000, &mut plugins)
        .unwrap();
    assert!(silent.iter().all(|v| *v == 0.0));
    document["regions"][0]["region"]["gain"] = serde_json::json!(1.0);
    let compound = serde_json::json!({"sample_rate":8000,"regions":[{
        "source_document":document,
        "region":{"timeline_start":0,"source_start":0,"length":480,"gain":1.0,"envelope":[]}
    }]});
    std::fs::write(relocated.join("regions.json"), compound.to_string()).unwrap();
    let nested = cache
        .render_incremental(&project, sf, 8000, &mut plugins)
        .unwrap();
    assert_eq!(nested, first);
    let mut changed = hound::WavWriter::create(
        relocated.join("source.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..801 {
        changed.write_sample(0.2_f32).unwrap();
    }
    changed.finalize().unwrap();
    assert!(cache.is_dirty());
    let updated = cache
        .render_incremental(&project, sf, 8000, &mut plugins)
        .unwrap();
    assert!(
        updated
            .iter()
            .zip(&nested)
            .all(|(a, b)| (*a - *b * 2.0).abs() < 1e-6)
    );
    std::fs::remove_dir_all(relocated).unwrap();
}

#[test]
fn builtin_sampler_track_renders_through_the_block_source_path() {
    let root = std::env::temp_dir().join(format!("ocp-sampler-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(root.join("tone.wav"), spec).unwrap();
    for frame in 0..8000 {
        let value = (std::f32::consts::TAU * 220.0 * frame as f32 / 8000.0).sin() * 0.5;
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
    let project = oc_dsl::parse(
        r#"@project "Sampler"
  tempo: 120
  time_signature: 4/4
@preset sampler
  source: builtin
  plugin: "builtin.sampler"
  params: { sampler.sample_file: "tone.wav", sampler.root_note: 60 }
@track sampler | preset: sampler
  @phrase p [1:1 - 1:2]
    @0 C4 q v=100
"#,
    )
    .unwrap();
    let mut cache = super::RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let sf = std::path::Path::new("unused.sf2");
    let mut plugins = std::collections::HashMap::new();
    let rendered = cache.render_full(&project, sf, 8000, &mut plugins).unwrap();
    let peak = rendered
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(peak > 0.01, "builtin.sampler rendered silence");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn relative_wavetable_replays_and_missing_table_rejects() {
    let root = std::env::temp_dir().join(format!("ocp-wavetable-replay-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("table.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&file, spec).unwrap();
    for n in 0..2048 {
        writer
            .write_sample((std::f32::consts::TAU * n as f32 / 2048.0).sin())
            .unwrap();
    }
    writer.finalize().unwrap();
    let mut project = oc_dsl::parse(include_str!(
        "../../../../../examples/opensynth_showcase.ocp"
    ))
    .unwrap();
    project.tracks.truncate(1);
    let name = project.tracks[0].preset.clone().unwrap();
    let preset = project.presets.iter_mut().find(|p| p.name == name).unwrap();
    preset.params = vec![
        (
            "wavetable.file".into(),
            ParamValue::String("table.wav".into()),
        ),
        ("osc.a.waveform".into(), ParamValue::Number(4.0)),
    ];
    let mut cache = super::RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let mut plugins = std::collections::HashMap::new();
    let first = cache
        .render_full(
            &project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut plugins,
        )
        .unwrap();
    assert!(first.iter().any(|x| x.abs() > 0.001));
    let prepared = crate::block_source::prepare_project(&project, Some(&root)).unwrap();
    let selected = prepared.presets.iter().find(|p| p.name == name).unwrap();
    let from_blocks = oc_synth::synth::load_project_patch(&selected.params, None).unwrap();
    let from_audition = oc_synth::synth::load_project_patch(
        &project
            .presets
            .iter()
            .find(|p| p.name == name)
            .unwrap()
            .params,
        Some(&root),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(from_blocks).unwrap(),
        serde_json::to_value(from_audition).unwrap()
    );
    assert_eq!(
        first,
        cache
            .render_full(
                &project,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut plugins
            )
            .unwrap()
    );
    std::fs::remove_file(&file).unwrap();
    assert!(cache.is_dirty());
    assert!(
        cache
            .render_incremental(
                &project,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut plugins
            )
            .is_err()
    );
    assert!(oc_synth::synth::load_project_patch(&selected.params, None).is_err());
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn native_document_project_relocates_and_rejects_missing_state() {
    use oc_synth::synth::{SynthPatch, Waveform, document::PatchDocument};
    let root = std::env::temp_dir().join(format!("pgen-native-state-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut document = PatchDocument {
        version: 1,
        name: "Custom".into(),
        author: "OpenCompose".into(),
        category: "test".into(),
        use_policy: "diagnostic_only".into(),
        patch: SynthPatch::default(),
    };
    document.patch.oscillator_a_waveform = Waveform::Wavetable;
    document.patch.custom_wavetable = Some(
        serde_json::from_value(serde_json::json!({
            "samples": (0..4096).map(|n| {
                let phase = std::f64::consts::TAU * (n % 2048) as f64 / 2048.0;
                0.7 * phase.sin() + 0.2 * (phase * if n < 2048 { 9.0 } else { 17.0 }).sin()
            }).collect::<Vec<_>>(),
            "frame_count": 2, "frame_size": 2048,
        }))
        .unwrap(),
    );
    document.patch.oscillator_a_wavetable_position = 0.43;
    std::fs::write(root.join("voice.json"), document.to_json().unwrap()).unwrap();
    let mut project = oc_dsl::parse(include_str!(
        "../../../../../examples/opensynth_showcase.ocp"
    ))
    .unwrap();
    project.tracks.truncate(1);
    let preset_name = project.tracks[0].preset.clone().unwrap();
    let preset = project
        .presets
        .iter_mut()
        .find(|p| p.name == preset_name)
        .unwrap();
    preset.params = vec![(
        "open_synth_state_file".into(),
        ParamValue::String("voice.json".into()),
    )];
    let render = |root: &std::path::Path| {
        let mut cache = super::RenderCache::new();
        cache.set_resource_root(Some(root.to_path_buf()));
        cache.render_full(
            &project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut std::collections::HashMap::new(),
        )
    };
    let first = render(&root).unwrap();
    assert!(first.iter().all(|x| x.is_finite()));
    assert!(first.iter().any(|x| x.abs() > 0.001));
    let relocated = root.with_extension("relocated");
    std::fs::rename(&root, &relocated).unwrap();
    assert_eq!(first, render(&relocated).unwrap());
    let mut cache = super::RenderCache::new();
    cache.set_resource_root(Some(relocated.clone()));
    let mut plugins = std::collections::HashMap::new();
    cache
        .render_full(
            &project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut plugins,
        )
        .unwrap();
    assert!(!cache.is_dirty());
    document.patch.gain = 0.0;
    document.name = "Silent updated native patch".into();
    std::fs::write(relocated.join("voice.json"), document.to_json().unwrap()).unwrap();
    assert!(cache.is_dirty());
    let updated = cache
        .render_incremental(
            &project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut plugins,
        )
        .unwrap();
    assert!(updated.iter().all(|x| *x == 0.0));
    std::fs::remove_file(relocated.join("voice.json")).unwrap();
    assert!(cache.is_dirty());
    assert!(
        cache
            .render_incremental(
                &project,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut plugins
            )
            .is_err()
    );
    assert!(render(&relocated).unwrap_err().contains("OpenSynth state"));
    std::fs::remove_dir(relocated).unwrap();
}

#[test]
fn native_synth_track_override_changes_audio_and_preserves_preset() {
    let mut project = oc_dsl::parse(include_str!(
        "../../../../../examples/opensynth_showcase.ocp"
    ))
    .unwrap();
    project.tracks.truncate(1);
    let render = |p: &oc_dsl::ast::Project| {
        super::RenderCache::new()
            .render_full(
                p,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new(),
            )
            .unwrap()
    };
    let original_presets = serde_json::to_value(&project.presets).unwrap();
    let original = render(&project);
    assert!(original.iter().any(|sample| sample.abs() > 0.001));
    project.tracks[0].engine_params = Some(vec![("output.gain".into(), ParamValue::Number(0.0))]);
    let muted = render(&project);
    assert!(muted.iter().all(|sample| *sample == 0.0));
    assert_eq!(
        original_presets,
        serde_json::to_value(&project.presets).unwrap()
    );
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(muted, render(&reopened));
}

#[test]
fn native_performance_bound_program_reopens_and_pan_reaches_audio() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { performance.program: 6 }\n@track t | preset: p, instrument: synth/lead_square\n  performance: { @0 ch0 program 6; @0 ch0 cc 10 0; @0 ch0 note_on 60 90; @1 ch0 note_off 60 0; }\n").unwrap();
    let render = |p: &oc_dsl::ast::Project| {
        super::RenderCache::new()
            .render_full(
                p,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new(),
            )
            .unwrap()
    };
    let samples = render(&project);
    assert!(samples.iter().step_by(2).any(|x| x.abs() > 1e-6));
    assert!(samples.iter().skip(1).step_by(2).all(|x| *x == 0.));
    assert_eq!(
        samples,
        render(&oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap())
    );
    let mut unbound = project.clone();
    unbound.presets[0].params.clear();
    assert!(
        super::prepare_performance_tracks(&unbound, 8000, &std::collections::HashMap::new())
            .is_err()
    );
}

#[test]
fn native_performance_send_binding_reopens_and_reaches_declared_bus() {
    let text = "@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { performance.send91.bus: \"room\", performance.send91.initial: 0 }\n@track t | preset: p, instrument: synth/lead_square\n  mix: send(room=50%)\n  performance: { @0 ch0 cc 91 0; @0 ch0 note_on 60 90; @1 ch0 cc 91 127; @2 ch0 note_off 60 0; }\n@bus room\n  routes: [master]\n";
    let project = oc_dsl::parse(text).unwrap();
    let render = |p: &oc_dsl::ast::Project| {
        super::RenderCache::new().render_full(
            p,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut std::collections::HashMap::new(),
        )
    };
    let audio = render(&project).unwrap();
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(audio, render(&reopened).unwrap());
    let dry = render(&oc_dsl::parse(&text.replace("@1 ch0 cc 91 127", "@1 ch0 cc 91 0")).unwrap())
        .unwrap();
    assert_eq!(&audio[..8000], &dry[..8000]);
    assert!(dry[8000..16000].iter().any(|v| v.abs() > 1e-5));
    for (actual, direct) in audio[8000..16000].iter().zip(&dry[8000..16000]) {
        assert!((actual - (direct + direct * 0.5)).abs() < 1e-7);
    }
    let wet_text = text.replace("  routes: [master]\n",
            "  routes: [master]\n  effects:\n    - reverb: decay=0.4s, predelay=10ms, damping=0.5, mix=100%\n");
    let wet_project = oc_dsl::parse(&wet_text).unwrap();
    let wet = render(&wet_project).unwrap();
    assert!(wet.iter().all(|v| v.is_finite()));
    assert_eq!(
        wet,
        render(&oc_dsl::parse(&oc_dsl::serialize(&wet_project)).unwrap()).unwrap()
    );
    let off = render(&oc_dsl::parse(&wet_text.replace("cc 91 127", "cc 91 0")).unwrap()).unwrap();
    assert_eq!(&wet[..8000], &off[..8000]);
    assert!(wet.iter().zip(&off).any(|(a, b)| (a - b).abs() > 1e-5));
    let invalid = [
        text.replace(
            "performance.send91.initial: 0",
            "performance.send91.initial: 0.5",
        ),
        text.replace("performance.send91.bus: \"room\", ", ""),
        text.replace("send(room=50%)", "send(missing=50%)"),
        text.replace("@1 ch0", "@1 ch1"),
        text.replace("cc 91 127", "cc 93 127"),
    ];
    for text in invalid {
        let p = oc_dsl::parse(&text).unwrap();
        assert!(render(&p).is_err());
    }
}

#[test]
fn native_synth_project_reopens_with_identical_audio() {
    let project = oc_dsl::parse(include_str!(
        "../../../../../examples/opensynth_showcase.ocp"
    ))
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    let restored = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    let render = |p: &oc_dsl::ast::Project| {
        super::RenderCache::new()
            .render_full(
                p,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new(),
            )
            .unwrap()
    };
    let a = render(&project);
    assert_eq!(a, render(&restored));
    assert!(a.iter().all(|x| x.is_finite()));
    assert!(a.iter().any(|x| x.abs() > 0.01));
}
