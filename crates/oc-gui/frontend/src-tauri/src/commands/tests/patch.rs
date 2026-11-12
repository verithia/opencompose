use oc_dsl::ast::ParamValue;
use oc_engine::preset::{PresetLibrary, SaveOutcome};

use crate::commands::patch::*;
use crate::commands::project::{duplicate_track_in_project, duplicate_track_inner};
use crate::commands::synth::{
    import_external_preset_at, load_synth_patch_into_project, randomize_preset_in_project,
};

use super::support::*;

const LIFECYCLE: &str = r#"@ocp "0.2.0"
@project "Lifecycle"
  tempo: 120
  time_signature: 4/4
@preset source
  source: builtin
  plugin: "builtin.open_synth"
  patch: "Glass Pad"
  params: { osc.a.level: 0.5, filter.cutoff: 4200 }
  effects:
    - chorus: rate=0.8, depth=4, mix=30%
@preset child
  source: builtin
  plugin: "builtin.open_synth"
  extends: source
@pattern riff | instrument: synth/lead_square, preset: source
  events:
    C4 q mf
@track lead | preset: source, instrument: synth/lead_square
  @phrase lead [1:1 - 2:1]
    C4 q mf
"#;

fn project() -> oc_dsl::ast::Project {
    oc_dsl::parse(LIFECYCLE).expect("lifecycle fixture parses")
}

fn params(project: &oc_dsl::ast::Project, name: &str) -> Vec<(String, ParamValue)> {
    project
        .presets
        .iter()
        .find(|preset| preset.name == name)
        .expect("preset exists")
        .params
        .clone()
}

#[test]
fn duplicate_is_a_real_copy_and_editing_it_leaves_the_original_alone() {
    let mut project = project();
    duplicate_preset_in_project(&mut project, "source", "source copy", true).unwrap();

    // The track and pattern moved to the copy; the original keeps its values.
    assert!(project.presets.iter().any(|p| p.name == "source copy"));
    assert!(project.presets.iter().any(|p| p.name == "source"));
    assert_eq!(project.tracks[0].preset.as_deref(), Some("source copy"));
    assert_eq!(project.patterns[0].preset.as_deref(), Some("source copy"));

    // Editing the copy must not move the original, key by key.
    let original = params(&project, "source");
    let copy = params(&project, "source copy");
    assert_eq!(original, copy);
    let index = project
        .presets
        .iter()
        .position(|preset| preset.name == "source copy")
        .unwrap();
    project.presets[index].params[0].1 = ParamValue::Number(0.9);
    project.presets[index].effects.push(oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Delay,
        params: vec![],
        bypass: false,
        sidechain_source: None,
    });
    assert_eq!(params(&project, "source"), original);
    assert_ne!(params(&project, "source copy"), original);
    assert_eq!(
        project
            .presets
            .iter()
            .find(|p| p.name == "source")
            .unwrap()
            .effects
            .len(),
        1
    );
}

#[test]
fn duplicating_a_track_gives_the_copy_its_own_preset() {
    let mut project = project();
    let index = duplicate_track_in_project(&mut project, 0, "lead 2").unwrap();

    // The source keeps its preset; the copy gets a new name with the same values.
    assert_eq!(project.tracks[0].preset.as_deref(), Some("source"));
    assert_eq!(project.tracks[index].preset.as_deref(), Some("source copy"));
    assert_eq!(params(&project, "source"), params(&project, "source copy"));
    assert_eq!(project.tracks[index].name, "lead 2");
    assert_eq!(
        project.tracks[index].instrument,
        project.tracks[0].instrument
    );

    // Editing the copy's preset is inaudible on the original track.
    let original = params(&project, "source");
    let copy = project
        .presets
        .iter()
        .position(|preset| preset.name == "source copy")
        .unwrap();
    project.presets[copy].params[0].1 = ParamValue::Number(0.11);
    assert_eq!(params(&project, "source"), original);
    // The pattern that played the original still does.
    assert_eq!(project.patterns[0].preset.as_deref(), Some("source"));
}

#[test]
fn duplicating_twice_keeps_preset_names_unique() {
    let mut project = project();
    duplicate_track_in_project(&mut project, 0, "lead 2").unwrap();
    duplicate_track_in_project(&mut project, 0, "lead 3").unwrap();

    let names: Vec<&str> = project
        .presets
        .iter()
        .map(|preset| preset.name.as_str())
        .collect();
    assert_eq!(names, ["source", "child", "source copy", "source copy 2"]);
    assert_eq!(project.tracks[1].preset.as_deref(), Some("source copy"));
    assert_eq!(project.tracks[2].preset.as_deref(), Some("source copy 2"));
}

#[test]
fn a_duplicated_factory_patch_carries_its_values_and_leaves_the_bank_alone() {
    let mut project = project();
    // `source` was loaded from the bundled factory bank: the project block
    // holds the materialized values and `patch` only names their origin.
    assert_eq!(project.presets[0].patch.as_deref(), Some("Glass Pad"));
    let original = params(&project, "source");
    duplicate_track_in_project(&mut project, 0, "lead 2").unwrap();

    let copy = project
        .presets
        .iter()
        .position(|preset| preset.name == "source copy")
        .unwrap();
    assert_eq!(project.presets[copy].patch.as_deref(), Some("Glass Pad"));
    assert_eq!(project.presets[copy].params, original);
    assert_eq!(project.presets[copy].effects, project.presets[0].effects);

    project.presets[copy].params[0].1 = ParamValue::Number(0.23);
    project.presets[copy].patch = Some("Edited".into());
    assert_eq!(params(&project, "source"), original);
    assert_eq!(project.presets[0].patch.as_deref(), Some("Glass Pad"));
}

#[test]
fn a_track_without_a_preset_duplicates_without_one() {
    let mut project = project();
    project.tracks[0].preset = None;
    let presets = project.presets.len();
    let index = duplicate_track_in_project(&mut project, 0, "lead 2").unwrap();

    assert_eq!(project.tracks[index].preset, None);
    assert_eq!(project.presets.len(), presets);
    // Saving it as a new preset has nothing to fork and says so.
    let error = save_track_preset_as_new_in_project(&mut project, index).unwrap_err();
    assert!(error.contains("no preset"), "{error}");
}

#[test]
fn duplicate_track_is_one_undo_entry() {
    let mut inner = test_state();
    inner.project = project();
    let before = inner.project.clone();
    let snapshot = duplicate_track_inner(&mut inner, 0, "lead 2").unwrap();

    assert_eq!(snapshot.project.tracks.len(), 2);
    assert_eq!(inner.undo.undo.len(), 1, "track and preset copy, one entry");
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(
        inner.project.tracks[1].preset.as_deref(),
        Some("source copy")
    );
}

#[test]
fn save_as_new_preset_moves_only_that_track() {
    let mut project = project();
    let name = save_track_preset_as_new_in_project(&mut project, 0).unwrap();

    assert_eq!(name, "source copy");
    assert_eq!(project.tracks[0].preset.as_deref(), Some("source copy"));
    assert_eq!(params(&project, "source"), params(&project, "source copy"));
    assert!(project.presets.iter().any(|preset| preset.name == "source"));
    // Only the track moved; the pattern still plays the shared original.
    assert_eq!(project.patterns[0].preset.as_deref(), Some("source"));

    // Saving again forks the copy under a fresh name instead of colliding.
    let again = save_track_preset_as_new_in_project(&mut project, 0).unwrap();
    assert_eq!(again, "source copy 2");
    assert_eq!(project.tracks[0].preset.as_deref(), Some("source copy 2"));
    assert_eq!(
        params(&project, "source copy"),
        params(&project, "source copy 2")
    );
}

#[test]
fn a_loaded_patch_keeps_its_name_and_provenance_through_an_ocp_round_trip() {
    let mut project = project();
    load_synth_patch_into_project(
        &mut project,
        "source",
        Some("Warm Pad"),
        vec![("filter.cutoff".into(), Some("4200".into()))],
        vec![oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Delay,
            params: vec![("time".into(), ParamValue::Time(0.25))],
            bypass: false,
            sidechain_source: None,
        }],
    )
    .unwrap();

    let text = oc_dsl::serialize(&project);
    let reloaded = oc_dsl::parse(&text).expect("serialized project parses");
    let preset = reloaded
        .presets
        .iter()
        .find(|preset| preset.name == "source")
        .unwrap();
    assert_eq!(preset.patch.as_deref(), Some("Warm Pad"));
    assert_eq!(preset.effects.len(), 1);
    assert_eq!(oc_dsl::serialize(&reloaded), text);
}

#[test]
fn duplicate_can_keep_original_assignments_when_asked() {
    let mut project = project();
    duplicate_preset_in_project(&mut project, "source", "variant", false).unwrap();
    assert_eq!(project.tracks[0].preset.as_deref(), Some("source"));
    assert_eq!(project.patterns[0].preset.as_deref(), Some("source"));
    assert!(
        project
            .presets
            .iter()
            .any(|preset| preset.name == "variant")
    );
}

#[test]
fn duplicate_and_rename_reject_a_taken_or_empty_name() {
    let mut project = project();
    assert!(duplicate_preset_in_project(&mut project, "source", "child", true).is_err());
    assert!(duplicate_preset_in_project(&mut project, "source", "  ", true).is_err());
    assert!(duplicate_preset_in_project(&mut project, "missing", "fresh", true).is_err());
    assert!(rename_preset_in_project(&mut project, "source", "child").is_err());
}

#[test]
fn rename_rewrites_track_pattern_and_extends_references() {
    let mut project = project();
    rename_preset_in_project(&mut project, "source", "warm pad").unwrap();
    assert!(project.presets.iter().all(|preset| preset.name != "source"));
    assert_eq!(project.tracks[0].preset.as_deref(), Some("warm pad"));
    assert_eq!(project.patterns[0].preset.as_deref(), Some("warm pad"));
    assert_eq!(
        project
            .presets
            .iter()
            .find(|preset| preset.name == "child")
            .unwrap()
            .extends
            .as_deref(),
        Some("warm pad")
    );
}

#[test]
fn load_replaces_params_effects_and_provenance() {
    let mut project = project();
    let effects = vec![oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Delay,
        params: vec![("time".into(), ParamValue::Time(0.25))],
        bypass: false,
        sidechain_source: None,
    }];
    load_synth_patch_into_project(
        &mut project,
        "source",
        Some("Neon Bass"),
        vec![
            ("osc.a.level".into(), Some("0.7".into())),
            ("filter.cutoff".into(), Some("740".into())),
        ],
        effects.clone(),
    )
    .unwrap();
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "source")
        .unwrap();
    assert_eq!(preset.patch.as_deref(), Some("Neon Bass"));
    assert_eq!(preset.effects, effects);
    assert_eq!(preset.params.len(), 2);
}

#[test]
fn loading_a_then_b_then_a_reproduces_the_same_parameter_set() {
    let mut project = project();
    let a = vec![
        ("osc.a.level".into(), Some("0.7".into())),
        ("filter.cutoff".into(), Some("740".into())),
        ("amp.attack".into(), Some("0.004".into())),
    ];
    let b = vec![
        ("osc.a.level".into(), Some("0.4".into())),
        ("amp.release".into(), Some("3.2".into())),
        ("env3.decay".into(), Some("0.9".into())),
        ("sub.level".into(), Some("0.5".into())),
    ];
    let snapshot = |project: &oc_dsl::ast::Project| {
        let mut preset = project
            .presets
            .iter()
            .find(|preset| preset.name == "source")
            .unwrap()
            .clone();
        preset.params.sort_by(|left, right| left.0.cmp(&right.0));
        preset
    };

    load_synth_patch_into_project(&mut project, "source", Some("A"), a.clone(), vec![]).unwrap();
    let first = snapshot(&project);
    load_synth_patch_into_project(&mut project, "source", Some("B"), b, vec![]).unwrap();
    let second = snapshot(&project);
    assert_ne!(first.params, second.params);
    // B declared nothing from A, so A's keys are gone rather than left behind.
    assert!(
        second.params.iter().all(|(key, _)| key != "filter.cutoff"),
        "load B kept A's filter cutoff"
    );

    load_synth_patch_into_project(&mut project, "source", Some("A"), a, vec![]).unwrap();
    assert_eq!(snapshot(&project), first);
}

#[test]
fn loading_a_full_patch_clears_parameters_it_does_not_declare() {
    let mut project = project();
    let full = vec![
        ("osc.a.level".into(), Some("0.66".into())),
        ("amp.attack".into(), Some("0.01".into())),
        ("amp.decay".into(), Some("0.35".into())),
        ("amp.sustain".into(), Some("0.92".into())),
        ("amp.release".into(), Some("0.25".into())),
        ("env3.attack".into(), Some("0.5".into())),
        ("env3.decay".into(), Some("0.5".into())),
        ("env3.sustain".into(), Some("0.5".into())),
        ("env3.release".into(), Some("0.5".into())),
        ("osc.b.level".into(), Some("0.2".into())),
        ("osc.b.pan".into(), Some("-0.25".into())),
        ("filter.wet".into(), Some("0.8".into())),
    ];
    load_synth_patch_into_project(&mut project, "source", Some("Bank"), full, vec![]).unwrap();
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "source")
        .unwrap();
    for key in [
        "amp.attack",
        "amp.decay",
        "amp.sustain",
        "amp.release",
        "env3.attack",
        "env3.decay",
        "env3.sustain",
        "env3.release",
        "osc.b.level",
        "osc.b.pan",
        "filter.wet",
    ] {
        assert!(
            preset.params.iter().any(|(name, _)| name == key),
            "missing {key}"
        );
    }
}

#[test]
fn library_round_trip_uses_a_temporary_directory() {
    let temp = TempDir::new("patch-library");
    let library = PresetLibrary::at(temp.path());
    let project = project();

    for (preset, name) in [("source", "source"), ("child", "Library child")] {
        let entry = sound_preset_from_project(&project, preset, name, Some("pad"), vec![]).unwrap();
        save_patch_to_library_with(&library, &entry, false).unwrap();
    }

    let scan = library.scan();
    assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    assert_eq!(scan.presets.len(), 2);
    let source = scan
        .presets
        .iter()
        .find(|preset| preset.name == "source")
        .unwrap();
    assert_eq!(source.params, params(&project, "source"));
    assert_eq!(source.patch.as_deref(), Some("Glass Pad"));
    assert_eq!(source.effects, project.presets[0].effects);
    let child = scan
        .presets
        .iter()
        .find(|preset| preset.name == "Library child")
        .unwrap();
    let resolved = oc_dsl::preset::resolve_preset(&project.presets, "child").unwrap();
    assert_eq!(child.params, resolved.params);
    assert_eq!(child.effects, resolved.effects);
}

#[test]
fn library_patch_reuses_its_inherited_wavetable_from_another_project() {
    use oc_synth::synth::{OpenSynth, load_project_patch};

    let source = TempDir::new("library-patch-source");
    let receiver = TempDir::new("library-patch-receiver");
    let library_dir = TempDir::new("library-patch-store");
    let library = PresetLibrary::at(library_dir.path());
    let table = "table.wav";
    for (dir, harmonic) in [(source.path(), 1.0), (receiver.path(), 3.0)] {
        std::fs::create_dir_all(dir).unwrap();
        let mut writer = hound::WavWriter::create(
            dir.join(table),
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for frame in 0..2048 {
            let phase = std::f32::consts::TAU * frame as f32 / 2048.0;
            writer.write_sample((phase * harmonic).sin() * 0.8).unwrap();
        }
        writer.finalize().unwrap();
    }
    let project = oc_dsl::parse(
        "@project\n@preset parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {wavetable.file: \"table.wav\", osc.a.waveform: 4}\n@preset child\n  extends: parent\n  params: {output.gain: 0.17}\n",
    ).unwrap();
    let entry = sound_preset_from_project(&project, "child", "Saved child", None, vec![]).unwrap();
    let original = entry.params.clone();
    let render = |params, root| {
        let patch = load_project_patch(params, Some(root)).unwrap();
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        let mut output = vec![0.0; 4096];
        synth.render_interleaved(&mut output, 48_000);
        output
    };
    let reference = render(&original, source.path());
    assert_ne!(reference, render(&original, receiver.path()));
    let mut legacy = entry.clone();
    legacy.name = "Legacy child".into();
    library.save(&legacy).unwrap();
    library
        .save_project_patch(&entry, Some(source.path()), false)
        .unwrap();
    std::fs::remove_file(source.path().join(table)).unwrap();
    let scan = library.scan();
    assert_eq!(scan.presets.len(), 2);
    assert_eq!(scan.warnings.len(), 1);
    assert!(scan.warnings[0].contains("Legacy child"));
    let saved = scan
        .presets
        .into_iter()
        .find(|preset| preset.name == "Saved child")
        .unwrap();
    let state = saved
        .params
        .iter()
        .find_map(|(key, value)| match (key.as_str(), value) {
            ("open_synth_state_file", ParamValue::String(path)) => Some(std::path::Path::new(path)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        state.parent(),
        Some(library_dir.path().join("assets").as_path())
    );
    let mut receiving_project = oc_dsl::parse(
        "@project\n@preset target\n  source: builtin\n  plugin: \"builtin.open_synth\"\n",
    )
    .unwrap();
    let params = saved
        .params
        .into_iter()
        .map(|(name, value)| {
            let text = match value {
                ParamValue::String(text) => text,
                ParamValue::Number(number) => number.to_string(),
                other => panic!("unexpected fixture value {other:?}"),
            };
            (name, Some(text))
        })
        .collect();
    load_synth_patch_into_project(
        &mut receiving_project,
        "target",
        Some(&saved.name),
        params,
        saved.effects,
    )
    .unwrap();
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&receiving_project)).unwrap();
    assert_eq!(
        render(&reopened.presets[0].params, receiver.path()),
        reference
    );
    let restored = oc_dsl::preset::resolve_preset(&project.presets, "child").unwrap();
    assert_eq!(
        restored.params, original,
        "library save only changes the snapshot"
    );
}

#[test]
fn a_second_save_without_overwrite_is_refused_but_with_it_replaces() {
    let temp = TempDir::new("patch-library-conflict");
    let library = PresetLibrary::at(temp.path());
    let project = project();
    let entry = sound_preset_from_project(&project, "source", "source", None, vec![]).unwrap();
    assert_eq!(
        save_patch_to_library_with(&library, &entry, false).unwrap(),
        SaveOutcome::Created
    );
    let error = save_patch_to_library_with(&library, &entry, false).unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(
        save_patch_to_library_with(&library, &entry, true).unwrap(),
        SaveOutcome::Replaced
    );
    assert_eq!(library.scan().presets.len(), 1);
}

#[test]
fn a_corrupt_library_file_does_not_hide_the_rest() {
    let temp = TempDir::new("patch-library-corrupt");
    let library = PresetLibrary::at(temp.path());
    let project = project();
    let entry = sound_preset_from_project(&project, "source", "source", None, vec![]).unwrap();
    save_patch_to_library_with(&library, &entry, false).unwrap();
    std::fs::write(temp.path().join("broken.ocp-preset"), "not json").unwrap();
    let scan = library.scan();
    assert_eq!(scan.presets.len(), 1);
    assert_eq!(scan.warnings.len(), 1);
    assert!(scan.warnings[0].contains("broken.ocp-preset"));
}

#[test]
fn an_unsupported_or_corrupt_file_reports_its_name_and_reason() {
    let temp = TempDir::new("external-import");
    std::fs::create_dir_all(temp.path()).unwrap();

    let text = temp.path().join("patch.txt");
    std::fs::write(&text, b"whatever").unwrap();
    let error = import_external_preset_at(&text).unwrap_err();
    assert!(error.contains("patch.txt"), "{error}");
    assert!(error.contains(".SerumPreset"), "{error}");

    let broken = temp.path().join("broken.SerumPreset");
    std::fs::write(&broken, b"not a preset").unwrap();
    let error = import_external_preset_at(&broken).unwrap_err();
    assert!(error.starts_with("broken.SerumPreset: "), "{error}");
    assert!(error.contains("XferJson"), "{error}");
}

const RANDOMIZED: &str = r#"@ocp "0.2.0"
@project "Random"
  tempo: 120
@preset source
  source: builtin
  plugin: "builtin.open_synth"
  params: { osc.a.level: 0.5, filter.cutoff: 4200, output.gain: 0.42 }
@track lead | preset: source, instrument: synth/lead_square
  @phrase lead [1:1 - 2:1]
    C4 q mf
"#;

#[test]
fn randomize_is_seeded_and_keeps_the_output_gain() {
    let mut first_project = oc_dsl::parse(RANDOMIZED).expect("randomize fixture parses");
    randomize_preset_in_project(&mut first_project, "source", 7).unwrap();
    let first = params(&first_project, "source");
    let gain = first
        .iter()
        .find(|(key, _)| key == "output.gain")
        .and_then(|(_, value)| value.as_f64());
    assert_eq!(gain, Some(0.42), "output gain is not a variation axis");

    let mut second_project = oc_dsl::parse(RANDOMIZED).expect("randomize fixture parses");
    randomize_preset_in_project(&mut second_project, "source", 7).unwrap();
    assert_eq!(
        params(&second_project, "source"),
        first,
        "same seed, same patch"
    );

    let mut third_project = oc_dsl::parse(RANDOMIZED).expect("randomize fixture parses");
    randomize_preset_in_project(&mut third_project, "source", 8).unwrap();
    assert_ne!(
        params(&third_project, "source"),
        first,
        "a new seed moves the patch"
    );
}

#[test]
fn randomize_materialises_inherited_parameters_and_label() {
    let mut project = project();
    randomize_preset_in_project(&mut project, "child", 3).unwrap();
    let child = project
        .presets
        .iter()
        .find(|preset| preset.name == "child")
        .unwrap();
    assert!(child.extends.is_none());
    assert!(child.params.iter().any(|(key, _)| key == "osc.a.level"));
    assert_eq!(child.patch.as_deref(), Some("Random 3"));
}
