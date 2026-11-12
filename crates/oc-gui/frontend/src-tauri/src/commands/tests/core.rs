use std::path::{Path, PathBuf};

use oc_dsl::ast::{self, *};

use crate::commands::arrangement::*;
use crate::commands::notes::*;
use crate::commands::plugin_catalog::*;
use crate::commands::plugin_host::*;
use crate::commands::project::*;
use crate::commands::render::*;
use crate::commands::shared::*;
use crate::commands::synth::*;
use crate::state::{UndoEdit, UndoScope};

use super::support::*;

#[test]
fn rest_events_reach_the_gui_as_pitch_placeholders() {
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Rests"
  tempo: 120
  time_signature: 4/4
@track lead | instrument: synth/lead_square
  @phrase lead_p [1:1:0 - 3:1:0]
    C3 q mf
    Eb3 q mf
    G3 q mf
    Bb3 q mf
    C4 h mf
    rest q
    Bb3 q mf
"#,
    )
    .unwrap();
    let phrase = &project.tracks[0].phrases[0];
    // The rest is a real event in the timeline payload, so consumers must skip it.
    assert_eq!(phrase.events.len(), 7);
    let midis: Vec<u8> = phrase
        .events
        .iter()
        .map(|event| event.pitch.to_midi())
        .collect();
    assert_eq!(midis, vec![48, 51, 55, 58, 60, 12, 58]);
    assert!(
        phrase.events[5]
            .modifiers
            .iter()
            .any(|(key, _)| key == "rest")
    );
}

#[test]
fn pasted_block_names_stay_unique() {
    let names = ["intro_pad", "intro_pad 2"];
    assert_eq!(
        unique_block_name("intro_pad", names.iter().copied()),
        "intro_pad 3"
    );
    assert_eq!(unique_block_name("bass", names.iter().copied()), "bass");
}

#[test]
fn editor_duration_preserves_long_and_triplet_lengths() {
    for beats in [
        0.125,
        0.25,
        1.0 / 3.0,
        1.0 / 6.0,
        1.5,
        3.0,
        5.25,
        9.0,
        32.0,
        8.0 + 1.0 / 3.0,
    ] {
        let duration = editor_duration(beats).unwrap();
        assert!((duration.to_beats() - beats).abs() < 1e-8);
        let mut project = oc_dsl::parse("@project\n@track keys | instrument: piano/acoustic_grand\n  @phrase melody [1:1:0 - 33:1:0]\n    C4 q\n").unwrap();
        project.tracks[0].phrases[0].events[0].duration = duration;
        let source = oc_dsl::serialize(&project);
        let parsed = oc_dsl::parse(&source).unwrap();
        assert!(
            (parsed.tracks[0].phrases[0].events[0].duration.to_beats() - beats).abs() < 1e-8,
            "{source}"
        );
    }
    for beats in [0.0, -1.0, f64::NAN, f64::INFINITY, 1025.0, 0.1] {
        assert!(editor_duration(beats).is_err());
    }
}
#[test]
fn factory_patch_rejects_unknown_synth_keys_by_name() {
    let mut project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.5}\n",
        )
        .unwrap();
    let original = project.clone();
    let error = replace_synth_patch(
        &mut project,
        "p",
        vec![
            ("osc.bogus".into(), Some("1".into())),
            ("filter.cutoff".into(), Some("740".into())),
            ("mod.1.source".into(), Some("0".into())),
        ],
    )
    .expect_err("unknown factory keys must be rejected");
    assert!(error.contains("osc.bogus"), "{error}");
    assert!(!error.contains("filter.cutoff"), "{error}");
    assert_eq!(project, original);
}

#[test]
fn factory_patch_accepts_file_and_array_keys() {
    let params = vec![
        ("wavetable.file".to_string(), None),
        ("open_synth_state_file".to_string(), None),
        (
            "serum_file".to_string(),
            Some("/tmp/x.SerumPreset".to_string()),
        ),
        ("lfo2.shape.x".to_string(), Some("[0,0.5,1]".to_string())),
        ("filter.cutoff".to_string(), Some("740".to_string())),
    ];
    assert!(unknown_synth_params(&params).is_empty());
}

fn macro_learn_project() -> ast::Project {
    oc_dsl::parse(
        "@project \"Macro learn\"\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {filter.cutoff: 800, filter.resonance: 0.2}\n",
    )
    .unwrap()
}

fn resolved_number(project: &ast::Project, preset_name: &str, key: &str) -> Option<f64> {
    let resolved = oc_dsl::preset::resolve(project).unwrap();
    resolved
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)?
        .params
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_f64())
}

#[test]
fn macro_learn_writes_the_expected_route_triple() {
    let mut project = macro_learn_project();
    let result = macro_learn_inner(&mut project, "p", 2, "filter.cutoff", None).unwrap();
    assert_eq!(result.status, MacroLearnStatus::Bound);
    assert_eq!(result.slot, Some(1));
    assert_eq!(result.destination, Some(0));
    assert_eq!(result.source, Some(4), "Macro 2 is source id 4");
    // The triple is what the matrix and the engine both read back.
    assert_eq!(resolved_number(&project, "p", "mod.1.source"), Some(4.0));
    assert_eq!(
        resolved_number(&project, "p", "mod.1.destination"),
        Some(0.0)
    );
    assert_eq!(resolved_number(&project, "p", "mod.1.amount"), Some(0.5));
}

#[test]
fn macro_learn_clicking_the_same_control_unbinds_it() {
    let mut project = macro_learn_project();
    macro_learn_inner(&mut project, "p", 2, "filter.cutoff", None).unwrap();
    let result = macro_learn_inner(&mut project, "p", 2, "filter.cutoff", None).unwrap();
    assert_eq!(result.status, MacroLearnStatus::Unbound);
    assert_eq!(result.slot, Some(1));
    assert_eq!(resolved_number(&project, "p", "mod.1.amount"), Some(0.0));
    // A third click learns it again into the freed slot.
    let again = macro_learn_inner(&mut project, "p", 2, "filter.cutoff", None).unwrap();
    assert_eq!(again.status, MacroLearnStatus::Bound);
    assert_eq!(again.slot, Some(1));
}

#[test]
fn macro_learn_refuses_a_control_without_a_destination() {
    let mut project = macro_learn_project();
    let before = project.clone();
    let result = macro_learn_inner(&mut project, "p", 1, "unison.width", None).unwrap();
    assert_eq!(result.status, MacroLearnStatus::Unsupported);
    assert_eq!(result.destination, None);
    assert_eq!(result.slot, None);
    assert_eq!(project, before, "an unsupported learn must not write");
}

#[test]
fn macro_learn_reports_a_full_matrix_without_writing() {
    let mut project = macro_learn_project();
    let fill: Vec<(String, Option<String>)> = (1..=64)
        .map(|slot| (format!("mod.{slot}.amount"), Some("0.5".to_string())))
        .collect();
    apply_preset_params(&mut project, "p", fill).unwrap();
    let before = project.clone();
    let result = macro_learn_inner(&mut project, "p", 3, "filter.cutoff", None).unwrap();
    assert_eq!(result.status, MacroLearnStatus::Full);
    assert_eq!(result.slot, None);
    assert_eq!(project, before);
}

/// The factory list lives in the editor, so every key it can send must be
/// one the synth understands or LOAD is rejected as a whole.
#[test]
fn factory_patch_editor_keys_are_all_known() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src/components/OpenSynthEditor.tsx"
    ));
    let start = source
        .find("const FACTORY_PATCHES")
        .expect("factory patch table");
    let end = source[start..]
        .find("\n};")
        .map(|offset| start + offset)
        .expect("factory patch table end");
    let mut keys = std::collections::BTreeSet::new();
    let mut rest = &source[start..end];
    while let Some(open) = rest.find("[\"") {
        let after = &rest[open + 2..];
        let close = after.find('"').expect("factory key");
        keys.insert(after[..close].to_string());
        rest = &after[close + 1..];
    }
    // Macro, step and matrix resets are generated by factoryParams, not
    // spelled out in the table, so mirror those families here.
    for macro_index in 1..=8 {
        keys.insert(format!("macro.{macro_index}"));
    }
    for step in 1..=16 {
        keys.insert(format!("step.{step}"));
    }
    for route in 1..=64 {
        for field in ["source", "destination", "amount"] {
            keys.insert(format!("mod.{route}.{field}"));
        }
    }
    // factoryParams also clears the rack before applying a patch.
    keys.insert("fx.count".to_string());
    assert!(keys.len() > 300, "only found {} factory keys", keys.len());
    for required in [
        "synth_version",
        "filter.cutoff",
        "mod.64.amount",
        "step.16",
        "macro.8",
        "wavetable.file",
    ] {
        assert!(keys.contains(required), "missing {required}");
    }
    let params: Vec<(String, Option<String>)> = keys
        .into_iter()
        .map(|key| (key, Some("1".to_string())))
        .collect();
    let unknown = unknown_synth_params(&params);
    assert!(unknown.is_empty(), "editor sends unknown keys: {unknown:?}");
    let uncatalogued: Vec<&String> = params
        .iter()
        .map(|(key, _)| key)
        .filter(|key| !oc_engine::dsp::schema::synth::is_known_param(key))
        .collect();
    assert!(
        uncatalogued.is_empty(),
        "editor sends keys missing from the synth catalog: {uncatalogued:?}"
    );
}

/// The authored bank lives in the frontend data file. Every key it names must
/// be one the synth resolves and every `fx.N.type` must name a real module,
/// because LOAD writes the whole batch or nothing.
#[test]
fn factory_bank_preset_keys_and_fx_labels_are_known() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src/data/factoryPresets.ts"
    ));
    let mut keys = std::collections::BTreeSet::new();
    let mut labels = 0usize;
    for line in source.lines() {
        let mut rest = line;
        while let Some(open) = rest.find("[\"") {
            let after = &rest[open + 2..];
            let Some(key_end) = after.find('"') else {
                break;
            };
            let key = &after[..key_end];
            let tail = after[key_end + 1..].trim_start_matches([',', ' ']);
            keys.insert(key.to_string());
            if let Some(value_text) = tail.strip_prefix('"')
                && let Some(value_end) = value_text.find('"')
            {
                let value = &value_text[..value_end];
                if key.starts_with("fx.") && key.ends_with(".type") {
                    assert!(
                        oc_synth::synth::SynthFxType::from_label(value).is_some(),
                        "{key}: unknown FX module {value}"
                    );
                    labels += 1;
                }
            }
            rest = tail;
        }
    }
    assert!(keys.len() > 100, "only found {} preset keys", keys.len());
    assert!(labels >= 8, "only found {labels} FX module labels");
    for required in [
        "output.gain",
        "amp.attack",
        "osc.a.warp_mode",
        "filter2.enabled",
        "mod.1.destination",
        "fx.count",
    ] {
        assert!(keys.contains(required), "missing {required}");
    }
    let params: Vec<(String, Option<String>)> = keys
        .into_iter()
        .map(|key| (key, Some("1".to_string())))
        .collect();
    let unknown = unknown_synth_params(&params);
    assert!(
        unknown.is_empty(),
        "preset bank sends unknown keys: {unknown:?}"
    );
    let uncatalogued: Vec<&String> = params
        .iter()
        .map(|(key, _)| key)
        .filter(|key| !oc_engine::dsp::schema::synth::is_known_param(key))
        .collect();
    assert!(
        uncatalogued.is_empty(),
        "preset bank sends keys missing from the synth catalog: {uncatalogued:?}"
    );
}

#[test]
fn factory_load_marks_dirty_and_pushes_one_undo_entry() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../examples/opensynth_showcase.ocp"
    )))
    .unwrap();
    let old_project = inner.project.clone();
    replace_synth_patch(
        &mut inner.project,
        "neon_bass",
        vec![
            ("osc.a.level".into(), Some("0.58".into())),
            ("filter.cutoff".into(), Some("4200".into())),
            ("wavetable.file".into(), None),
        ],
    )
    .unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("load neon_bass", UndoScope::Preset),
    )
    .expect("resync");

    assert!(inner.dirty);
    assert_eq!(inner.undo.undo.len(), 1, "one LOAD is one undo entry");
    assert!(
        inner.undo.undo[0].label.contains("neon_bass"),
        "unexpected label {}",
        inner.undo.undo[0].label
    );
    // The replacement drops the previous patch, not just the edited keys.
    let preset = snapshot(&inner)
        .project
        .presets
        .iter()
        .find(|preset| preset.name == "neon_bass")
        .unwrap()
        .params
        .clone();
    assert_eq!(preset.len(), 2);
    assert_eq!(
        preset
            .iter()
            .find(|(key, _)| key == "osc.a.level")
            .unwrap()
            .1,
        ast::ParamValue::Number(0.58)
    );
    assert_eq!(
        preset
            .iter()
            .find(|(key, _)| key == "filter.cutoff")
            .unwrap()
            .1,
        ast::ParamValue::Number(4200.0)
    );
}

#[test]
fn factory_patch_load_is_idempotent() {
    let mut project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.5}\n",
        )
        .unwrap();
    let params = vec![
        ("filter.cutoff".to_string(), Some("740".to_string())),
        ("osc.a.level".to_string(), Some("0.74".to_string())),
        ("wavetable.file".to_string(), None),
    ];
    replace_synth_patch(&mut project, "p", params.clone()).unwrap();
    let once = project.presets[0].params.clone();
    replace_synth_patch(&mut project, "p", params).unwrap();
    assert_eq!(project.presets[0].params, once);
    assert_eq!(once.len(), 2);
}

#[test]
fn factory_patch_detaches_inherited_state_without_losing_effects() {
    let mut project = oc_dsl::parse("@project\n@preset parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.5}\n  effects:\n    - gain: factor=0.5\n@preset child\n  extends: parent\n  params: {osc.a.level: 0.3}\n").unwrap();
    project.presets[0].params.push((
        "open_synth_state_file".into(),
        oc_dsl::ast::ParamValue::String("old-patch.json".into()),
    ));
    let original = project.clone();
    let old_effects = oc_dsl::preset::resolve(&project).unwrap().presets[1]
        .effects
        .clone();
    assert!(
        replace_synth_patch(&mut project, "child", vec![("".into(), Some("1".into()))]).is_err()
    );
    assert_eq!(project, original);
    replace_synth_patch(
        &mut project,
        "child",
        vec![("output.gain".into(), Some("0.2".into()))],
    )
    .unwrap();
    assert_eq!(project.presets[0], original.presets[0]);
    assert_eq!(project.presets[1].extends, None);
    assert_eq!(project.presets[1].effects, old_effects);
    assert_eq!(project.presets[1].params.len(), 1);
    let resolved = oc_dsl::preset::resolve(&project).unwrap();
    assert_eq!(resolved.presets[1].params, project.presets[1].params);
    assert_eq!(resolved.presets[1].effects, old_effects);
}

#[test]
fn project_replacement_clears_old_edit_transactions() {
    let mut inner = test_state();
    let history_owner = inner
        .undo
        .begin_owned_gesture(crate::state::UndoScope::Automation)
        .unwrap();
    inner.plugin_automation_gestures.insert(
        "p".into(),
        crate::state::PluginAutomationGesture {
            history_owner,
            owner: None,
            old_project: inner.project.clone(),
            track_index: 0,
            lane_param: "cutoff".into(),
        },
    );
    inner.dirty = true;
    let project = oc_dsl::parse("@project \"Fresh\"\n").unwrap();
    replace_project(&mut inner, project.clone(), Some("/tmp/Fresh.ocp".into())).unwrap();
    assert!(inner.plugin_automation_gestures.is_empty());
    assert!(inner.plugin_instances.is_empty());
    assert!(inner.vst3_plugin_instances.is_empty());
    assert!(inner.vst2_plugin_instances.is_empty());
    assert!(inner.lv2_plugin_instances.is_empty());
    #[cfg(target_os = "macos")]
    assert!(inner.au_plugin_instances.is_empty());
    assert!(inner.open_clap_editors.is_empty());
    assert_eq!(inner.dsl_source, oc_dsl::serialize(&project));
    assert_eq!(inner.file_path, Some(PathBuf::from("/tmp/Fresh.ocp")));
    assert!(!inner.dirty);
    assert!(inner.undo.undo.is_empty());
    replace_project(&mut inner, default_project(), None).unwrap();
    assert!(inner.file_path.is_none());
}

#[test]
fn first_press_does_not_scan_plugins_for_a_plugin_free_project() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
            "@project \"Plain\"\n@preset keys\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.5}\n",
        )
        .unwrap();
    let warnings = collect_plugin_render_warnings(&inner.project);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(ensure_clap_instances_loaded(&mut inner).is_empty());
    assert!(
        inner.scanned_plugins.is_empty(),
        "the first press must not scan the plugin inventory"
    );
}

#[test]
fn plugin_projects_request_background_plugin_warm_up() {
    let plain = oc_dsl::parse("@project \"Plain\"\n").unwrap();
    assert!(plugin_lookup_requests(&plain).is_empty());
    assert!(!project_uses_plugin_effects(&plain));
    assert!(!project_needs_plugin_inventory(&plain));

    let vst3 = oc_dsl::parse(
        "@project \"Vst\"\n@preset bass\n  source: vst3\n  plugin: \"vst3:halcyon_2\"\n",
    )
    .unwrap();
    assert_eq!(
        plugin_lookup_requests(&vst3),
        vec![("vst3:halcyon_2".to_string(), oc_host::PluginFormat::Vst3)]
    );
    // A single VST3 id resolves per bundle; the full inventory is not needed.
    assert!(!project_needs_plugin_inventory(&vst3));

    let clap = oc_dsl::parse(
        "@project \"Clap\"\n@preset lead\n  source: clap\n  plugin: \"com.example.synth\"\n",
    )
    .unwrap();
    assert!(project_needs_plugin_inventory(&clap));
}

#[test]
#[ignore = "requires OCP_TEST_CLAP pointing to local Dexed; no editor/audio device opened"]
fn project_replacement_does_not_reuse_same_named_clap_preset() {
    use oc_host::clap_instance::ClapInstance;
    let path = std::env::var("OCP_TEST_CLAP").expect("OCP_TEST_CLAP");
    let mut inner = test_state();
    inner.plugin_instances.insert(
        "p".into(),
        ClapInstance::load(
            std::path::Path::new(&path),
            "com.digital-suburban.dexed",
            48000,
            256,
        )
        .unwrap(),
    );
    let project = oc_dsl::parse(
        "@project\n@preset p\n  source: clap\n  plugin: \"test.missing.different-plugin\"\n",
    )
    .unwrap();
    // Before replacement this name collision silently skips loading altogether.
    inner.project = project.clone();
    assert!(ensure_clap_instances_loaded(&mut inner).is_empty());
    assert!(inner.plugin_instances.contains_key("p"));
    replace_project(&mut inner, project, None).unwrap();
    assert!(!inner.plugin_instances.contains_key("p"));
    let warnings = ensure_clap_instances_loaded(&mut inner);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("test.missing.different-plugin"));
    assert!(!inner.plugin_instances.contains_key("p"));
}

#[test]
fn performance_edit_is_atomic_stale_checked_and_undoable() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse("@project\n@track t | instrument: piano/acoustic_grand\n  performance: { @0.125 ch2 note_on 60 90; @1.125 ch2 note_off 60 37; }\n").unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    let original = inner.project.clone();
    let expected = original.tracks[0].performance.as_ref().unwrap().events[1].clone();
    let mut replacement = expected.clone();
    replacement.quarter = 0.0;
    assert!(
        apply_performance_event_edit(&mut inner, 0, "t", 1, expected.clone(), replacement.clone())
            .is_err()
    );
    assert_eq!(inner.dsl_source, oc_dsl::serialize(&original));
    assert!(inner.undo.undo.is_empty());
    replacement.quarter = 1.25;
    replacement.message = oc_dsl::performance::ChannelCommand::NoteOff {
        note: 60,
        velocity: 73,
    };
    assert!(
        apply_performance_event_edit(
            &mut inner,
            0,
            "renamed",
            1,
            expected.clone(),
            replacement.clone()
        )
        .is_err()
    );
    apply_performance_event_edit(&mut inner, 0, "t", 1, expected.clone(), replacement.clone())
        .unwrap();
    assert_eq!(inner.undo.undo.len(), 1);
    let edited = inner.project.clone();
    assert!(apply_performance_event_edit(&mut inner, 0, "t", 1, expected, replacement).is_err());
    assert_eq!(
        oc_dsl::serialize(&inner.project),
        oc_dsl::serialize(&edited)
    );
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        oc_dsl::serialize(&inner.project),
        oc_dsl::serialize(&original)
    );
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(
        oc_dsl::serialize(&inner.project),
        oc_dsl::serialize(&edited)
    );
}

#[test]
fn plugin_instantiation_runs_without_the_app_state_lock() {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(
                "@project\n@preset imported\n  source: vst3\n  plugin: \"vst3:halcyon_2\"\n  params: { serum_file: \"/tmp/x.SerumPreset\" }\n",
            )
            .unwrap();
    }

    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let worker_state = Arc::clone(&state);
    let worker_started = Arc::clone(&started);
    let worker_release = Arc::clone(&release);
    let worker = std::thread::spawn(move || {
        load_plugin_for_preset_with(&worker_state, "imported", |_plan| {
            worker_started.wait();
            worker_release.wait();
            Err("probe: plugin loading is stubbed".to_string())
        })
    });

    started.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    let lock_free = loop {
        match state.inner.try_lock() {
            Ok(_guard) => break true,
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => break false,
        }
    };
    release.wait();
    let result = worker.join().unwrap();

    assert!(
        lock_free,
        "plugin instantiation held the app-state lock, so editor commands would freeze"
    );
    assert!(result.is_err());
}

#[test]
#[ignore = "instantiates a hosted VST3 plugin; run manually with OCP_TEST_PLUGINS=1 and a project with vst3 presets"]
fn measure_preset_plugin_load_latency() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    if std::env::var("OCP_TEST_PLUGINS").ok().as_deref() != Some("1") {
        eprintln!("skipped: set OCP_TEST_PLUGINS=1 to instantiate the plugin");
        return;
    }
    let path = std::env::var("OCP_PRESET_PROJECT")
        .map(PathBuf::from)
        .expect("set OCP_PRESET_PROJECT to a project with vst3 presets");
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let names: Vec<String> = project
        .presets
        .iter()
        .filter(|preset| preset.source.as_deref() == Some("vst3"))
        .map(|preset| preset.name.clone())
        .collect();
    assert!(names.len() >= 2, "project needs at least two vst3 presets");

    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        inner.project = project;
        inner.file_path = Some(path);
    }

    let plan = plugin_load_plan(&state, &names[0]).unwrap();
    let info = plan
        .plugin_info
        .clone()
        .or_else(|| oc_host::find_plugin_info(&plan.plugin_id, plan.format))
        .expect("plugin info");
    let load_start = Instant::now();
    let mut instance =
        oc_host::vst3_instance::Vst3Instance::load(Path::new(&info.path), plan.sample_rate, 512)
            .unwrap();
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    let apply_start = Instant::now();
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Vst3,
        Some(info.id.as_str()),
        &plan.preset_name,
    );
    let _ = oc_engine::render_cache::apply_vst3_preset_state_at(
        &mut instance,
        &plan.preset.params,
        &context,
        None,
    );
    let apply_ms = apply_start.elapsed().as_secs_f64() * 1000.0;

    let params_start = Instant::now();
    let params = instance.get_params();
    let params_ms = params_start.elapsed().as_secs_f64() * 1000.0;

    let by_name_start = Instant::now();
    for param in &params {
        let _ = instance.get_param_by_name(&param.name);
    }
    let by_name_ms = by_name_start.elapsed().as_secs_f64() * 1000.0;

    let values_start = Instant::now();
    let values = instance.get_params_with_values();
    let values_ms = values_start.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "BREAKDOWN load_ms={load_ms:.1} apply_state_ms={apply_ms:.1} get_params_ms={params_ms:.1} \
             by_name_ms={by_name_ms:.1} single_pass_ms={values_ms:.1} params={}/{}",
        params.len(),
        values.len()
    );
    drop(instance);

    let running = Arc::new(AtomicBool::new(true));
    let monitor_state = Arc::clone(&state);
    let monitor_running = Arc::clone(&running);
    let monitor = std::thread::spawn(move || {
        let mut blocked_since: Option<Instant> = None;
        let mut worst = Duration::ZERO;
        while monitor_running.load(Ordering::Relaxed) {
            match monitor_state.inner.try_lock() {
                Ok(_) => {
                    if let Some(start) = blocked_since.take() {
                        worst = worst.max(start.elapsed());
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    blocked_since.get_or_insert_with(Instant::now);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {}
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        worst
    });

    for name in [&names[0], &names[1], &names[0]] {
        let started = Instant::now();
        let params = load_plugin_for_preset_blocking(&state, name).unwrap();
        eprintln!(
            "LOAD preset={name} total_ms={:.1} params={}",
            started.elapsed().as_secs_f64() * 1000.0,
            params.len()
        );
    }
    running.store(false, Ordering::Relaxed);
    eprintln!(
        "LOCK max_blocked_ms={:.1}",
        monitor.join().unwrap().as_secs_f64() * 1000.0
    );
}

#[test]
fn snapshot_reports_the_arrangement_duration_before_playback() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
            "@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  @phrase held [1:1 - 3:1]\n    C4 w\n",
        )
        .unwrap();
    assert_eq!(snapshot(&inner).duration_seconds, 4.0);
    inner.project.sections.push(ast::Section {
        name: "body".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(5, 1, 0),
        },
        repeat: Some(2),
        label: None,
        flags: None,
    });
    assert_eq!(snapshot(&inner).duration_seconds, 16.0);
}

#[test]
fn range_stem_paths_are_unique_and_sanitized() {
    let output = std::path::Path::new("/tmp/mix.wav");
    let path = stem_path(output, 3, "Bass / Sub");
    assert_eq!(
        path.display().to_string(),
        "/tmp/mix.track-03-Bass___Sub.wav"
    );
}

#[test]
fn time_signature_updates_base_meter_and_bar_one_map_point() {
    let mut project = default_project();
    project.properties.time_sig_map = vec![
        ast::TimeSigPoint {
            position: ast::BarBeat::new(1, 1, 0),
            numerator: 4,
            denominator: 4,
        },
        ast::TimeSigPoint {
            position: ast::BarBeat::new(5, 1, 0),
            numerator: 3,
            denominator: 4,
        },
    ];
    set_project_time_signature(&mut project, 7, 8).unwrap();
    assert_eq!(project.properties.time_signature.numerator, 7);
    assert_eq!(project.properties.time_signature.denominator, 8);
    assert_eq!(project.properties.time_sig_map[0].numerator, 7);
    assert_eq!(project.properties.time_sig_map[0].denominator, 8);
    assert_eq!(project.properties.time_sig_map[1].numerator, 3);
    assert_eq!(project.properties.time_sig_map[1].denominator, 4);
}

#[test]
fn time_signature_change_remaps_content_to_the_same_quarters() {
    let mut project = oc_dsl::parse(
            "@project\n  time_signature: 4/4\n@track keys | instrument: piano/acoustic_grand\n  @phrase main [1:1:0 - 4:4:0]\n    C4 q\n",
        )
        .unwrap();
    project.sections.push(ast::Section {
        name: "intro".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(9, 1, 0),
        },
        repeat: None,
        label: None,
        flags: None,
    });
    project.tracks[0].push_pattern_clip(ast::Clip {
        pattern: "main".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(5, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    let old_properties = project.properties.clone();

    set_project_time_signature(&mut project, 3, 8).unwrap();
    remap_content_positions(&mut project, &old_properties).unwrap();

    let phrase = project.tracks[0].phrases[0].time_range.unwrap();
    assert_eq!(phrase.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(phrase.end, ast::BarBeat::new(11, 1, 0));
    let clip = project.tracks[0].pattern_clip(0).unwrap().time_range;
    assert_eq!(clip.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(clip.end, ast::BarBeat::new(11, 3, 0));
    let section = project.sections[0].time_range;
    assert_eq!(section.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(section.end, ast::BarBeat::new(22, 2, 0));

    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let quarter_of = |range: ast::TimeRange| {
        clock.quarter_notes_at(&range.start).unwrap()..=clock.quarter_notes_at(&range.end).unwrap()
    };
    assert_eq!(quarter_of(phrase), 0.0..=15.0);
    assert_eq!(quarter_of(clip), 0.0..=16.0);
    assert_eq!(quarter_of(section), 0.0..=32.0);
}

#[test]
fn time_signature_rejects_invalid_meters_without_mutating() {
    let mut project = default_project();
    for (numerator, denominator) in [(0, 4), (33, 4), (4, 3), (4, 0)] {
        let before = project.clone();
        assert!(set_project_time_signature(&mut project, numerator, denominator).is_err());
        assert_eq!(project, before);
    }
    for (numerator, denominator) in [(1, 1), (12, 8), (32, 64)] {
        set_project_time_signature(&mut project, numerator, denominator).unwrap();
        assert_eq!(project.properties.time_signature.numerator, numerator);
        assert_eq!(project.properties.time_signature.denominator, denominator);
    }
}

#[test]
fn tempo_map_replaces_and_survives_dsl_roundtrip() {
    let mut project = default_project();
    let points = vec![
        ast::TempoPoint {
            position: ast::BarBeat::new(1, 1, 0),
            offset_quarters: 0.0,
            bpm: 90.0,
        },
        ast::TempoPoint {
            position: ast::BarBeat::new(5, 1, 0),
            offset_quarters: 0.0,
            bpm: 140.0,
        },
    ];
    apply_tempo_map(&mut project, points.clone()).unwrap();
    assert_eq!(project.properties.tempo_map, points);

    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened.properties.tempo_map, points);

    // The empty map is the single-tempo project again.
    apply_tempo_map(&mut project, vec![]).unwrap();
    assert!(project.properties.tempo_map.is_empty());
    assert_eq!(
        project.properties.tempo,
        ast::ProjectProperties::default().tempo
    );
}

#[test]
fn tempo_map_rejects_invalid_values_and_duplicate_positions() {
    let point = ast::TempoPoint {
        position: ast::BarBeat::new(3, 1, 0),
        offset_quarters: 0.0,
        bpm: 120.0,
    };
    let mut project = default_project();
    for bpm in [0.0, 1_000.0, f64::NAN] {
        let before = project.clone();
        let invalid = vec![ast::TempoPoint { bpm, ..point }];
        assert!(apply_tempo_map(&mut project, invalid).is_err());
        assert_eq!(
            project, before,
            "a rejected map must not mutate the project"
        );
    }
    assert!(apply_tempo_map(&mut project, vec![point, point]).is_err());
    assert!(project.properties.tempo_map.is_empty());
}

#[test]
fn tempo_map_command_is_one_dirty_undo_entry() {
    let mut inner = test_state();
    let before = inner.project.clone();
    let points = vec![ast::TempoPoint {
        position: ast::BarBeat::new(2, 1, 0),
        offset_quarters: 0.0,
        bpm: 140.0,
    }];
    let saved = set_tempo_map_inner(&mut inner, points.clone()).unwrap();
    assert!(saved.dirty);
    assert_eq!(inner.undo.undo.len(), 1, "one map edit, one undo entry");
    assert_eq!(inner.project.properties.tempo_map, points);
    assert_eq!(inner.dsl_source, oc_dsl::serialize(&inner.project));

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
}

#[test]
fn tempo_map_command_does_not_dirty_on_rejection() {
    let mut inner = test_state();
    let point = ast::TempoPoint {
        position: ast::BarBeat::new(2, 1, 0),
        offset_quarters: 0.0,
        bpm: 140.0,
    };
    set_tempo_map_inner(&mut inner, vec![point]).unwrap();
    assert_eq!(inner.undo.undo.len(), 1);
    let before = inner.project.clone();
    inner.dirty = false;

    assert!(set_tempo_map_inner(&mut inner, vec![ast::TempoPoint { bpm: 0.0, ..point }]).is_err());
    assert!(!inner.dirty);
    assert_eq!(inner.undo.undo.len(), 1, "a rejected map adds no history");
    assert_eq!(inner.project, before);
}

#[test]
fn time_sig_map_remaps_content_and_survives_dsl_roundtrip() {
    let mut project = oc_dsl::parse(
        "@project\n  time_signature: 4/4\n@track keys | instrument: piano/acoustic_grand\n  @phrase main [1:1:0 - 5:1:0]\n    C4 q\n",
    )
    .unwrap();
    project.tracks[0].push_pattern_clip(ast::Clip {
        pattern: "main".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(5, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    let old_properties = project.properties.clone();

    apply_time_sig_map(
        &mut project,
        vec![ast::TimeSigPoint {
            position: ast::BarBeat::new(3, 1, 0),
            numerator: 3,
            denominator: 4,
        }],
    )
    .unwrap();
    remap_content_positions(&mut project, &old_properties).unwrap();

    // Written content keeps its absolute quarter; bar labels move to the map.
    let phrase = project.tracks[0].phrases[0].time_range.unwrap();
    assert_eq!(phrase.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(phrase.end, ast::BarBeat::new(5, 3, 0));
    let clip = project.tracks[0].pattern_clip(0).unwrap().time_range;
    assert_eq!(clip.end, ast::BarBeat::new(5, 3, 0));

    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    assert_eq!(clock.quarter_notes_at(&phrase.end).unwrap(), 16.0);
    assert_eq!(
        clock.quarter_notes_at(&ast::BarBeat::new(4, 1, 0)).unwrap(),
        11.0
    );

    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(
        reopened.properties.time_sig_map,
        project.properties.time_sig_map
    );

    // The empty map is the project meter again.
    let before = project.properties.clone();
    apply_time_sig_map(&mut project, vec![]).unwrap();
    remap_content_positions(&mut project, &before).unwrap();
    assert!(project.properties.time_sig_map.is_empty());
    assert_eq!(
        project.tracks[0].phrases[0].time_range.unwrap().end,
        ast::BarBeat::new(5, 1, 0)
    );
}

#[test]
fn time_sig_map_rejects_non_bar_starts_and_duplicate_bars() {
    let mut project = default_project();
    let valid = ast::TimeSigPoint {
        position: ast::BarBeat::new(3, 1, 0),
        numerator: 3,
        denominator: 4,
    };
    for bad in [
        ast::TimeSigPoint {
            position: ast::BarBeat::new(3, 2, 0),
            ..valid
        },
        ast::TimeSigPoint {
            position: ast::BarBeat::new(3, 1, 2),
            ..valid
        },
        ast::TimeSigPoint {
            position: ast::BarBeat::new(0, 1, 0),
            ..valid
        },
        ast::TimeSigPoint {
            numerator: 33,
            ..valid
        },
        ast::TimeSigPoint {
            denominator: 5,
            ..valid
        },
    ] {
        let before = project.clone();
        assert!(apply_time_sig_map(&mut project, vec![bad]).is_err());
        assert_eq!(
            project, before,
            "a rejected map must not mutate the project"
        );
    }
    assert!(apply_time_sig_map(&mut project, vec![valid, valid]).is_err());
    assert!(project.properties.time_sig_map.is_empty());
}

#[test]
fn time_sig_map_command_is_one_dirty_undo_entry_and_remaps() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
        "@project\n  time_signature: 4/4\n@track keys | instrument: piano/acoustic_grand\n  @phrase main [1:1:0 - 5:1:0]\n    C4 q\n",
    )
    .unwrap();
    let before = inner.project.clone();
    let saved = set_time_sig_map_inner(
        &mut inner,
        vec![ast::TimeSigPoint {
            position: ast::BarBeat::new(3, 1, 0),
            numerator: 3,
            denominator: 4,
        }],
    )
    .unwrap();
    assert!(saved.dirty);
    assert_eq!(inner.undo.undo.len(), 1, "one map edit, one undo entry");
    assert_eq!(
        inner.project.tracks[0].phrases[0].time_range.unwrap().end,
        ast::BarBeat::new(5, 3, 0)
    );

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
}

fn audio_clip_project() -> crate::state::AppStateInner {
    let mut inner = test_state();
    inner.project.tracks.push(make_track("tape"));
    inner
        .project
        .add_audio_clip(ast::AudioClip {
            name: "hit".into(),
            track: "tape".into(),
            file: "media/hit.wav".into(),
            time_range: ast::TimeRange {
                start: ast::BarBeat::new(1, 1, 0),
                end: ast::BarBeat::new(3, 1, 0),
            },
            placement: Default::default(),
            offset_seconds: 0.0,
            gain_db: -3.0,
            fade_in_seconds: 0.1,
            fade_out_seconds: 0.2,
            invert: false,
            crossfade_seconds: 0.0,
            stretch: 1.0,
            stretch_mode: ast::StretchMode::Musical,
            tempo_follow: false,
            pitch: 0.0,
            formant: 0.0,
            bypass_processing: false,
            preserve_formants: false,
            warp_markers: Vec::new(),
            loop_seconds: 0.0,
            take_group: None,
            take: None,
            muted: false,
            gain_envelope: Vec::new(),
        })
        .expect("the fixture clip lands on its track");
    inner
}

#[test]
fn audio_clip_move_keeps_duration_and_can_change_track() {
    let mut inner = audio_clip_project();
    inner.project.tracks.push(make_track("tape2"));
    move_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(2, 3, 0),
        Some("tape2".into()),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.track, "tape2");
    assert_eq!(clip.time_range.start, ast::BarBeat::new(2, 3, 0));
    // Two bars stay two bars.
    assert_eq!(clip.time_range.end, ast::BarBeat::new(4, 3, 0));
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_trim_shifts_the_media_offset() {
    let mut inner = audio_clip_project();
    // At 120 BPM, two beats is one second.
    trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(1, 3, 0),
        ast::BarBeat::new(3, 1, 0),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!((clip.offset_seconds - 1.0).abs() < 1e-9);
    assert_eq!(clip.time_range.start, ast::BarBeat::new(1, 3, 0));
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(clip.gain_db, -3.0);
    assert_eq!(clip.fade_in_seconds, 0.1);
}

#[test]
fn audio_clip_trim_remaps_the_gain_envelope() {
    let mut inner = audio_clip_project();
    inner.project.audio_clip_mut(0).unwrap().gain_envelope = vec![
        ast::GainEnvelopePoint {
            position: 0.0,
            gain_db: 0.0,
        },
        ast::GainEnvelopePoint {
            position: 0.5,
            gain_db: -6.0,
        },
        ast::GainEnvelopePoint {
            position: 1.0,
            gain_db: 0.0,
        },
    ];
    trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(2, 1, 0),
        ast::BarBeat::new(3, 1, 0),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.gain_envelope.len(), 2);
    assert_eq!(clip.gain_envelope[0].position, 0.0);
    assert!((clip.gain_envelope[0].gain_db + 6.0).abs() < 1e-9);
    assert_eq!(clip.gain_envelope[1].position, 1.0);
    assert!(clip.gain_envelope[1].gain_db.abs() < 1e-9);
}

#[test]
fn audio_clip_trim_rejects_an_empty_range() {
    let mut inner = audio_clip_project();
    let error = trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(2, 1, 0),
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("after its start"), "{error}");
    assert_eq!(inner.project.audio_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_split_makes_two_clips_with_continuous_offsets() {
    let mut inner = audio_clip_project();
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(2, 1, 0)).unwrap();
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].time_range.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(clips[0].time_range.end, ast::BarBeat::new(2, 1, 0));
    assert_eq!(clips[1].time_range.start, ast::BarBeat::new(2, 1, 0));
    assert_eq!(clips[1].time_range.end, ast::BarBeat::new(3, 1, 0));
    assert!((clips[0].offset_seconds - 0.0).abs() < 1e-9);
    assert!((clips[1].offset_seconds - 2.0).abs() < 1e-9);
    // Interior fades disappear; the outer ones stay.
    assert_eq!(clips[0].fade_in_seconds, 0.1);
    assert_eq!(clips[0].fade_out_seconds, 0.0);
    assert_eq!(clips[1].fade_in_seconds, 0.0);
    assert_eq!(clips[1].fade_out_seconds, 0.2);
    assert_eq!(clips[1].gain_db, -3.0);
    assert_ne!(clips[0].name, clips[1].name);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_split_remaps_the_gain_envelope() {
    let mut inner = audio_clip_project();
    inner.project.audio_clip_mut(0).unwrap().gain_envelope = vec![
        ast::GainEnvelopePoint {
            position: 0.0,
            gain_db: 0.0,
        },
        ast::GainEnvelopePoint {
            position: 0.25,
            gain_db: -6.0,
        },
        ast::GainEnvelopePoint {
            position: 0.75,
            gain_db: 3.0,
        },
        ast::GainEnvelopePoint {
            position: 1.0,
            gain_db: 0.0,
        },
    ];
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(2, 1, 0)).unwrap();
    let clips = inner.project.audio_clips();
    assert_eq!(clips[0].gain_envelope.len(), 3);
    assert_eq!(clips[0].gain_envelope[0].position, 0.0);
    assert!((clips[0].gain_envelope[1].position - 0.5).abs() < 1e-9);
    assert!((clips[0].gain_envelope[1].gain_db + 6.0).abs() < 1e-9);
    assert_eq!(clips[0].gain_envelope[2].position, 1.0);
    assert!((clips[0].gain_envelope[2].gain_db + 1.5).abs() < 1e-9);
    assert_eq!(clips[1].gain_envelope.len(), 3);
    assert_eq!(clips[1].gain_envelope[0].position, 0.0);
    assert!((clips[1].gain_envelope[0].gain_db + 1.5).abs() < 1e-9);
    assert!((clips[1].gain_envelope[1].position - 0.5).abs() < 1e-9);
    assert!((clips[1].gain_envelope[1].gain_db - 3.0).abs() < 1e-9);
    assert_eq!(clips[1].gain_envelope[2].position, 1.0);
}

#[test]
fn audio_clip_split_outside_the_range_is_rejected() {
    let mut inner = audio_clip_project();
    let error = split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(3, 1, 0)).unwrap_err();
    assert!(error.contains("inside the audio clip"), "{error}");
    assert_eq!(inner.project.audio_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), 0);
}

/// A clip with real media, a zero envelope and its own loop window.
///
/// The loop rules decode the file, so the fixture writes one; at 120 BPM a bar
/// is two seconds and a beat is half a second.
fn audio_clip_loop_project(media_seconds: f64) -> (TempDir, crate::state::AppStateInner) {
    let dir = TempDir::new("clip-loop");
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    write_slip_media(&dir.path().join("media/hit.wav"), media_seconds);
    let mut inner = audio_clip_project();
    inner.file_path = Some(dir.path().join("project.ocp"));
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.gain_db = 0.0;
    clip.fade_in_seconds = 0.0;
    clip.fade_out_seconds = 0.0;
    (dir, inner)
}

fn set_clip(inner: &mut crate::state::AppStateInner, start: ast::BarBeat, end: ast::BarBeat) {
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.time_range = ast::TimeRange { start, end };
}

#[test]
fn audio_clip_split_keeps_the_loop_on_both_halves() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 2.0;
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(5, 1, 0)).unwrap();
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 2);
    // Both halves repeat the same two seconds and start a whole loop in.
    assert!((clips[0].offset_seconds - 0.0).abs() < 1e-9);
    assert!((clips[0].loop_seconds - 2.0).abs() < 1e-9);
    assert!((clips[1].offset_seconds - 0.0).abs() < 1e-9);
    assert!((clips[1].loop_seconds - 2.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_split_wraps_the_loop_position() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 2.0;
    // Nine seconds in: the right half resumes one second into the two-second loop.
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(5, 3, 0)).unwrap();
    let clips = inner.project.audio_clips();
    assert!((clips[1].offset_seconds - 1.0).abs() < 1e-9);
    assert!((clips[1].loop_seconds - 2.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_split_clears_a_loop_the_half_cannot_repeat() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    // Eleven seconds of an eight-second loop; split nine seconds in leaves two.
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(6, 3, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 8.0;
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(5, 3, 0)).unwrap();
    let clips = inner.project.audio_clips();
    // The left half still crosses the seam and keeps the loop.
    assert!((clips[0].loop_seconds - 8.0).abs() < 1e-9);
    // The right half repeats nowhere inside its two seconds: one pass of the
    // same media, which is what the engine accepts.
    assert_eq!(clips[1].loop_seconds, 0.0);
    assert!((clips[1].offset_seconds - 1.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_split_clears_the_left_loop_it_cannot_repeat() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 4.0;
    // One second in: the left half holds no seam, so it plays one pass.
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(1, 3, 0)).unwrap();
    let clips = inner.project.audio_clips();
    assert_eq!(clips[0].loop_seconds, 0.0);
    assert!((clips[0].offset_seconds - 0.0).abs() < 1e-9);
    // The right half resumes one second into the four-second loop.
    assert!((clips[1].offset_seconds - 1.0).abs() < 1e-9);
    assert!((clips[1].loop_seconds - 4.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_split_refuses_a_seam_the_right_half_cannot_repeat() {
    let (_dir, mut inner) = audio_clip_loop_project(4.0);
    // Five seconds of a four-second loop, cut three seconds in: the right half
    // is two seconds long but the next seam is one second away.
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(3, 3, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 4.0;
    let error = split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(2, 3, 0)).unwrap_err();
    assert_eq!(error, SPLIT_AUDIO_LOOP_SEAM);
    assert_eq!(inner.project.audio_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_split_refuses_a_loop_past_the_media() {
    let (_dir, mut inner) = audio_clip_loop_project(6.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    {
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.offset_seconds = 2.0;
        clip.loop_seconds = 4.0;
    }
    // The seam lands one second into the loop, so the right half would repeat
    // two seconds the file does not have.
    let error = split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(5, 3, 0)).unwrap_err();
    assert_eq!(error, SPLIT_AUDIO_LOOP_PAST_MEDIA);
    assert_eq!(inner.project.audio_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_split_clamps_fades_to_each_half() {
    let mut inner = audio_clip_project();
    {
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.fade_in_seconds = 3.0;
        clip.fade_out_seconds = 3.0;
    }
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(2, 1, 0)).unwrap();
    let clips = inner.project.audio_clips();
    // The seam removes the interior ramps; the outer ones stop at the half.
    assert!((clips[0].fade_in_seconds - 2.0).abs() < 1e-9);
    assert_eq!(clips[0].fade_out_seconds, 0.0);
    assert_eq!(clips[1].fade_in_seconds, 0.0);
    assert!((clips[1].fade_out_seconds - 2.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

/// Mean |x| of bars 5-8 of the mix, in i16 units, from a full offline render.
fn rendered_right_half_level(inner: &crate::state::AppStateInner) -> f64 {
    const RATE: u32 = 48_000;
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    let (audio, _cache) = oc_engine::render::render_full_for_export(
        &inner.project,
        Path::new(""),
        RATE,
        0.0,
        &mut std::collections::HashMap::new(),
        root,
        16.0,
        0.0,
    )
    .expect("the fixture renders");
    let start = 8 * RATE as usize * 2;
    let end = (16 * RATE as usize * 2).min(audio.len());
    let sum: f64 = audio[start..end]
        .iter()
        .map(|sample| f64::from(sample.abs()) * 32_767.0)
        .sum();
    sum / (end - start) as f64
}

/// A looped clip cut mid-loop leaves a right half that still sounds.
#[test]
fn audio_clip_split_keeps_the_right_half_audible() {
    let (_dir, mut inner) = audio_clip_loop_project(4.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 4.0;
    let before = rendered_right_half_level(&inner);
    split_audio_clip_inner(&mut inner, 0, ast::BarBeat::new(5, 1, 0)).unwrap();
    let after = rendered_right_half_level(&inner);
    assert!(
        before > 1000.0,
        "the fixture's right half is loud: {before}"
    );
    // Four seconds of media in a four-second loop: both halves repeat it whole.
    assert!(
        (after - before).abs() <= before * 0.01,
        "right half {after} against the unsplit {before}"
    );
}

#[test]
fn audio_clip_trim_wraps_the_loop_position() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 2.0;
    // Five seconds into the clip is one second into its two-second loop.
    trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(3, 3, 0),
        ast::BarBeat::new(9, 1, 0),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!((clip.offset_seconds - 1.0).abs() < 1e-9);
    assert!((clip.loop_seconds - 2.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_trim_clears_a_loop_the_clip_cannot_fit() {
    let (_dir, mut inner) = audio_clip_loop_project(8.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 2.0;
    // A second and a half repeats nowhere inside a two-second loop.
    trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(1, 4, 0),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.loop_seconds, 0.0);
    assert!((clip.offset_seconds - 0.0).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_trim_refuses_a_loop_past_the_media() {
    let (_dir, mut inner) = audio_clip_loop_project(4.0);
    set_clip(
        &mut inner,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(9, 1, 0),
    );
    inner.project.audio_clip_mut(0).unwrap().loop_seconds = 4.0;
    // Three seconds into a four-second single-loop file: the window would need
    // three more seconds of media that do not exist.
    let error = trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(2, 3, 0),
        ast::BarBeat::new(9, 1, 0),
    )
    .unwrap_err();
    assert_eq!(error, LOOP_PAST_MEDIA);
    assert_eq!(inner.project.audio_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_trim_clamps_fades_to_the_new_length() {
    let mut inner = audio_clip_project();
    {
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.fade_in_seconds = 3.0;
        clip.fade_out_seconds = 3.0;
    }
    trim_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(1, 4, 0),
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!((clip.fade_in_seconds - 1.5).abs() < 1e-9);
    assert!((clip.fade_out_seconds - 1.5).abs() < 1e-9);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_remove_is_one_undo_step() {
    let mut inner = audio_clip_project();
    remove_audio_clip_inner(&mut inner, 0).unwrap();
    assert!(inner.project.audio_clips().is_empty());
    assert_eq!(inner.undo.undo.len(), 1);
}

/// A saved project whose clip media exists: envelope edits require the file.
fn audio_clip_project_with_media() -> (TempDir, crate::state::AppStateInner) {
    let dir = TempDir::new("clip-envelope");
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/hit.wav"), b"RIFF").unwrap();
    let mut inner = audio_clip_project();
    inner.file_path = Some(dir.path().join("project.ocp"));
    (dir, inner)
}

/// A whole drag is one history entry: the timeline opens a gesture around it.
#[test]
fn audio_clip_envelope_drag_is_one_undo_entry() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    inner.undo.begin_gesture(UndoScope::Project);
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-4.0), None, None).unwrap();
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-5.0), None, None).unwrap();
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-6.0), None, None).unwrap();
    inner.undo.end_gesture();
    assert_eq!(inner.undo.undo.len(), 1, "a drag is one history entry");
    assert_eq!(inner.project.audio_clip(0).unwrap().gain_db, -6.0);

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().gain_db, -3.0);
}

#[test]
fn audio_clip_gain_is_one_undo_step_and_undo_restores() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let before = inner.project.clone();
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-6.0), None, None).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().gain_db, -6.0);
    assert!(
        inner.dsl_source.contains("gain: -6dB"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "clip gain");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
}

#[test]
fn audio_clip_envelope_guard_rejects_stale_revisions_and_agent_transactions() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let revision = inner.revision;
    inner.bump_revision();
    assert!(
        guard_gui_clip_edit(&inner, Some(revision))
            .unwrap_err()
            .contains("stale revision")
    );
    assert!(guard_gui_clip_edit(&inner, Some(inner.revision)).is_ok());
    inner
        .begin_transaction("edit".into(), "agent".into())
        .unwrap();
    assert!(
        guard_gui_clip_edit(&inner, Some(inner.revision))
            .unwrap_err()
            .contains("active agent transaction")
    );
}

#[test]
fn audio_clip_gain_clamps_to_the_stated_range() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    set_audio_clip_envelope_inner(&mut inner, 0, Some(999.0), None, None).unwrap();
    assert_eq!(
        inner.project.audio_clip(0).unwrap().gain_db,
        CLIP_GAIN_MAX_DB
    );
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-999.0), None, None).unwrap();
    assert_eq!(
        inner.project.audio_clip(0).unwrap().gain_db,
        CLIP_GAIN_MIN_DB
    );

    let error =
        set_audio_clip_envelope_inner(&mut inner, 0, Some(f64::INFINITY), None, None).unwrap_err();
    assert!(error.contains("finite"), "{error}");
}

#[test]
fn audio_clip_fade_shorter_than_20ms_is_refused() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let before = inner.project.clone();
    let error = set_audio_clip_envelope_inner(&mut inner, 0, None, Some(0.010), None).unwrap_err();
    assert_eq!(error, FADE_TOO_SHORT);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);

    // Exactly 20 ms is the floor, and zero clears the fade.
    set_audio_clip_envelope_inner(&mut inner, 0, None, Some(0.020), None).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().fade_in_seconds, 0.02);
    set_audio_clip_envelope_inner(&mut inner, 0, None, Some(0.0), None).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().fade_in_seconds, 0.0);
}

#[test]
fn audio_clip_fade_past_the_clip_length_clamps_at_the_length() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    // Two bars of 4/4 at 120 BPM is four seconds.
    set_audio_clip_envelope_inner(&mut inner, 0, None, Some(10.0), None).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().fade_in_seconds, 4.0);
}

#[test]
fn audio_clip_envelope_touches_only_the_named_field() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    set_audio_clip_envelope_inner(&mut inner, 0, None, None, Some(0.5)).unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.fade_out_seconds, 0.5);
    assert_eq!(clip.gain_db, -3.0);
    assert_eq!(clip.fade_in_seconds, 0.1);
    assert_eq!(inner.undo.undo[0].label, "fade out");

    set_audio_clip_envelope_inner(&mut inner, 0, None, Some(1.0), None).unwrap();
    assert_eq!(inner.undo.undo[1].label, "fade in");
}

#[test]
fn audio_clip_envelope_writes_nothing_when_no_value_changes() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    set_audio_clip_envelope_inner(&mut inner, 0, None, None, None).unwrap();
    set_audio_clip_envelope_inner(&mut inner, 0, Some(-3.0), None, None).unwrap();
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_without_media_is_refused() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    inner.project.audio_clip_mut(0).unwrap().file = "media/gone.wav".into();
    let before = inner.project.clone();
    let error = set_audio_clip_envelope_inner(&mut inner, 0, Some(-6.0), None, None).unwrap_err();
    assert_eq!(error, CLIP_HAS_NO_MEDIA);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);

    let (_dir, mut inner) = audio_clip_project_with_media();
    inner.project.audio_clip_mut(0).unwrap().file = String::new();
    let error = set_audio_clip_envelope_inner(&mut inner, 0, None, Some(0.5), None).unwrap_err();
    assert_eq!(error, CLIP_HAS_NO_MEDIA);
}

#[test]
fn audio_clip_envelope_rejects_an_out_of_range_index() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let error = set_audio_clip_envelope_inner(&mut inner, 9, Some(0.0), None, None).unwrap_err();
    assert!(error.contains("out of range"), "{error}");
    assert_eq!(inner.undo.undo.len(), 0);
}

fn point(position: f64, gain_db: f64) -> oc_dsl::ast::GainEnvelopePoint {
    oc_dsl::ast::GainEnvelopePoint { position, gain_db }
}

#[test]
fn audio_clip_envelope_full_writes_crossfade_and_points() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let points = vec![point(0.0, 0.0), point(0.5, -80.0), point(1.0, 0.0)];
    set_audio_clip_envelope_full(&mut inner, 0, None, None, None, Some(0.25), Some(points))
        .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.crossfade_seconds, 0.25);
    assert_eq!(clip.gain_envelope.len(), 3);
    assert_eq!(clip.gain_envelope[1].gain_db, CLIP_GAIN_MIN_DB);
    assert!(
        inner
            .dsl_source
            .contains("gain_envelope: 0:0dB, 0.5:-60dB, 1:0dB"),
        "{}",
        inner.dsl_source
    );
    assert!(
        inner.dsl_source.contains("crossfade: 0.25s"),
        "{}",
        inner.dsl_source
    );
    // Two fields at once is still the generic label.
    assert_eq!(inner.undo.undo[0].label, "clip envelope");

    let (_dir, mut inner) = audio_clip_project_with_media();
    set_audio_clip_envelope_full(&mut inner, 0, None, None, None, Some(0.1), None).unwrap();
    assert_eq!(inner.undo.undo[0].label, "crossfade");

    let (_dir, mut inner) = audio_clip_project_with_media();
    set_audio_clip_envelope_full(
        &mut inner,
        0,
        None,
        None,
        None,
        None,
        Some(vec![point(0.0, 0.0), point(1.0, -6.0)]),
    )
    .unwrap();
    assert_eq!(inner.undo.undo[0].label, "gain envelope");

    // An empty list clears the envelope again.
    set_audio_clip_envelope_full(&mut inner, 0, None, None, None, None, Some(Vec::new())).unwrap();
    assert!(
        inner
            .project
            .audio_clip(0)
            .unwrap()
            .gain_envelope
            .is_empty()
    );
}

#[test]
fn audio_clip_envelope_full_rejects_a_bad_envelope_position() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let before = inner.project.clone();
    let error = set_audio_clip_envelope_full(
        &mut inner,
        0,
        None,
        None,
        None,
        None,
        Some(vec![point(0.0, 0.0), point(0.5, -6.0)]),
    )
    .unwrap_err();
    assert!(error.contains("gain_envelope"), "{error}");
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);

    let error = set_audio_clip_envelope_full(
        &mut inner,
        0,
        None,
        None,
        None,
        None,
        Some(vec![point(f64::NAN, 0.0), point(1.0, 0.0)]),
    )
    .unwrap_err();
    assert!(error.contains("finite"), "{error}");
    assert_eq!(inner.project, before);
}

#[test]
fn audio_clip_crossfade_shorter_than_20ms_is_refused() {
    let (_dir, mut inner) = audio_clip_project_with_media();
    let error = set_audio_clip_envelope_full(&mut inner, 0, None, None, None, Some(0.010), None)
        .unwrap_err();
    assert_eq!(error, FADE_TOO_SHORT);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn multi_block_remove_is_one_undo_step_and_restores_everything() {
    let mut inner = test_state();
    let mut keys = make_track("keys");
    keys.phrases.push(ast::Phrase {
        name: "main".into(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        }),
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        channel: None,
    });
    keys.phrases.push(ast::Phrase {
        name: "intro".into(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(5, 1, 0),
            end: ast::BarBeat::new(6, 1, 0),
        }),
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        channel: None,
    });
    for (start, end) in [(1u32, 3u32), (5, 7), (9, 11)] {
        keys.push_pattern_clip(ast::Clip {
            pattern: "melody".into(),
            time_range: ast::TimeRange {
                start: ast::BarBeat::new(start, 1, 0),
                end: ast::BarBeat::new(end, 1, 0),
            },
            offset_quarters: 0.0,
            loop_quarters: 0.0,
            id: None,
        });
    }
    let mut drums = make_track("drums");
    drums.patterns.push(ast::Pattern {
        name: "beat".into(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(2, 1, 0),
        }),
        repeat: 1,
        hits: vec![],
    });
    inner.project.tracks.push(keys);
    inner.project.tracks.push(drums);
    inner.project.patterns.push(ast::PatternDef {
        name: "melody".into(),
        instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
        preset: None,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        hits: vec![],
        color: None,
    });

    let blocks = vec![
        BlockSelection {
            kind: "clip".into(),
            track_index: 0,
            index: 0,
        },
        BlockSelection {
            kind: "clip".into(),
            track_index: 0,
            index: 2,
        },
        BlockSelection {
            kind: "phrase".into(),
            track_index: 0,
            index: 0,
        },
        BlockSelection {
            kind: "pattern".into(),
            track_index: 1,
            index: 0,
        },
    ];
    remove_blocks_inner(&mut inner, &blocks).unwrap();

    // Removing 0 and 2 must leave the middle clip, not shift it away.
    assert_eq!(
        inner.project.tracks[0].pattern_clip_count(),
        1,
        "only the untouched clip stays"
    );
    assert_eq!(
        inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .time_range
            .start,
        ast::BarBeat::new(5, 1, 0)
    );
    assert_eq!(
        inner.project.tracks[0].phrases.len(),
        1,
        "only the picked phrase goes"
    );
    assert_eq!(inner.project.tracks[0].phrases[0].name, "intro");
    assert!(inner.project.tracks[1].patterns.is_empty());
    assert_eq!(inner.undo.undo.len(), 1, "one gesture, one undo entry");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.tracks[0].pattern_clip_count(), 3);
    assert_eq!(inner.project.tracks[0].phrases.len(), 2);
    assert_eq!(inner.project.tracks[1].patterns.len(), 1);
}

#[test]
fn multi_block_remove_rejects_an_out_of_range_block_without_mutating() {
    let mut inner = test_state();
    let mut track = make_track("keys");
    track.push_pattern_clip(ast::Clip {
        pattern: "melody".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    inner.project.tracks.push(track);
    let before = inner.project.clone();
    let blocks = vec![
        BlockSelection {
            kind: "clip".into(),
            track_index: 0,
            index: 0,
        },
        BlockSelection {
            kind: "phrase".into(),
            track_index: 0,
            index: 3,
        },
    ];
    assert!(remove_blocks_inner(&mut inner, &blocks).is_err());
    assert_eq!(inner.project, before);
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn audio_clip_move_rejects_an_unknown_track() {
    let mut inner = audio_clip_project();
    let error = move_audio_clip_inner(
        &mut inner,
        0,
        ast::BarBeat::new(1, 1, 0),
        Some("missing".into()),
    )
    .unwrap_err();
    assert!(error.contains("missing"), "{error}");
    assert_eq!(inner.project.audio_clip(0).unwrap().track, "tape");
}

#[test]
fn test_plugin_render_warnings_for_missing_plugin_effects() {
    let mut project = default_project();
    project.master = Some(ast::Master {
        gain_automation: Vec::new(),
        automation: Vec::new(),
        effects: vec![ast::Effect {
            effect_type: ast::EffectType::Plugin("org.example.fx".to_string()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        }],
        loudness_target: None,
        volume_db: 0.0,
        input_routes: vec![],
    });

    let warnings = collect_plugin_render_warnings(&project);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not found as a renderable CLAP, VST2 or VST3 plugin"));
    assert!(warnings[0].contains("org.example.fx"));
}

#[test]
fn test_plugin_render_warnings_for_missing_vst3_preset_plugin() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "VST Synth".to_string(),
        source: Some("vst3".to_string()),
        plugin: Some("vst3:example".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    let mut track = make_track("Lead");
    track.preset = Some("VST Synth".to_string());
    project.tracks.push(track);

    let warnings = collect_plugin_render_warnings(&project);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not found as a renderable VST3 plugin"));
    assert!(warnings[0].contains("VST Synth"));
    assert!(warnings[0].contains("track 'Lead'"));
}

#[test]
fn plugin_automation_points_are_normalized_sorted_and_replaced() {
    let mut project = default_project();
    project.tracks.push(make_track("Lead"));
    upsert_plugin_automation_point(
        &mut project,
        0,
        "pluginparam_4375746f6666",
        ast::BarBeat::new(2, 1, 0),
        0.8,
    )
    .unwrap();
    upsert_plugin_automation_point(
        &mut project,
        0,
        "pluginparam_4375746f6666",
        ast::BarBeat::new(1, 3, 0),
        2.0,
    )
    .unwrap();
    upsert_plugin_automation_point(
        &mut project,
        0,
        "pluginparam_4375746f6666",
        ast::BarBeat::new(2, 1, 0),
        0.25,
    )
    .unwrap();

    let lane = &project.tracks[0].automation_lane(0).unwrap();
    assert_eq!(lane.param, "pluginparam_4375746f6666");
    assert_eq!(plugin_automation_lane_name("Cutoff"), lane.param);
    assert_eq!(lane.points.len(), 2);
    assert_eq!(lane.points[0].position, ast::BarBeat::new(1, 3, 0));
    assert!(matches!(lane.points[0].value, ast::ParamValue::Number(1.0)));
    assert!(matches!(
        lane.points[1].value,
        ast::ParamValue::Number(0.25)
    ));
}

#[test]
fn compatibility_report_rejects_version_architecture_and_state_abi_mismatch() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Qualified Synth".into(),
        source: Some("clap".into()),
        plugin: Some("org.example.synth".into()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![
            (
                "plugin_version".into(),
                ast::ParamValue::String("2.0".into()),
            ),
            (
                "plugin_architecture".into(),
                ast::ParamValue::String("x86_64".into()),
            ),
            (
                "plugin_state_abi".into(),
                ast::ParamValue::String("old-state".into()),
            ),
        ],
        effects: vec![],
        extends: None,
    });
    let plugins = vec![oc_host::PluginInfo {
        id: "org.example.synth".into(),
        name: "Example Synth".into(),
        vendor: "Example".into(),
        format: oc_host::PluginFormat::Clap,
        path: "/plugins/synth.clap".into(),
        category: oc_host::PluginCategory::Instrument,
        num_params: 0,
        version: Some("3.0".into()),
        architectures: vec!["aarch64".into()],
        state_abi: Some("new-state".into()),
    }];

    let report = build_project_compatibility_report(&project, None, &plugins);
    let kinds: Vec<_> = report
        .issues
        .iter()
        .map(|issue| issue.kind.as_str())
        .collect();
    assert!(kinds.contains(&"plugin_version_mismatch"));
    assert!(kinds.contains(&"plugin_architecture_mismatch"));
    assert!(kinds.contains(&"plugin_state_abi_mismatch"));
}

// ── Parsing helpers ─────────────────────────────────────────────────

#[test]
fn test_parse_note_name() {
    assert_eq!(parse_note_name("C").unwrap(), NoteName::C);
    assert_eq!(parse_note_name("G").unwrap(), NoteName::G);
    assert!(parse_note_name("X").is_err());
    assert!(parse_note_name("").is_err());
}

#[test]
fn test_parse_accidental() {
    assert_eq!(parse_accidental("").unwrap(), Accidental::Natural);
    assert_eq!(parse_accidental("#").unwrap(), Accidental::Sharp);
    assert_eq!(parse_accidental("b").unwrap(), Accidental::Flat);
    assert_eq!(parse_accidental("##").unwrap(), Accidental::DoubleSharp);
    assert_eq!(parse_accidental("bb").unwrap(), Accidental::DoubleFlat);
    assert!(parse_accidental("x").is_err());
}

#[test]
fn test_parse_base_duration() {
    assert_eq!(parse_base_duration("q").unwrap(), BaseDuration::Quarter);
    assert_eq!(parse_base_duration("h").unwrap(), BaseDuration::Half);
    assert_eq!(parse_base_duration("w").unwrap(), BaseDuration::Whole);
    assert_eq!(parse_base_duration("8th").unwrap(), BaseDuration::Eighth);
    assert_eq!(
        parse_base_duration("16th").unwrap(),
        BaseDuration::Sixteenth
    );
    assert!(parse_base_duration("xyz").is_err());
}

#[test]
fn test_parse_target() {
    match parse_target("master").unwrap() {
        Target::Master => {}
        _ => panic!("Expected Master"),
    }
    match parse_target("track:0").unwrap() {
        Target::Track(0) => {}
        _ => panic!("Expected Track(0)"),
    }
    match parse_target("track:42").unwrap() {
        Target::Track(42) => {}
        _ => panic!("Expected Track(42)"),
    }
    match parse_target("bus:reverb_bus").unwrap() {
        Target::Bus(name) => assert_eq!(name, "reverb_bus"),
        _ => panic!("Expected Bus"),
    }
    assert!(parse_target("invalid").is_err());
    assert!(parse_target("track:abc").is_err());
}

#[test]
fn test_parse_effect_type() {
    assert_eq!(parse_effect_type("eq").unwrap(), ast::EffectType::Eq);
    assert_eq!(
        parse_effect_type("reverb").unwrap(),
        ast::EffectType::Reverb
    );
    assert_eq!(
        parse_effect_type("compressor").unwrap(),
        ast::EffectType::Compression
    );
    assert_eq!(
        parse_effect_type("de_esser").unwrap(),
        ast::EffectType::DeEsser
    );
    assert_eq!(
        parse_effect_type("multiband").unwrap(),
        ast::EffectType::Multiband
    );
    assert_eq!(
        parse_effect_type("leveler").unwrap(),
        ast::EffectType::Leveler
    );
    assert_eq!(parse_effect_type("opto").unwrap(), ast::EffectType::Opto);
    assert_eq!(parse_effect_type("tuner").unwrap(), ast::EffectType::Tuner);
    assert_eq!(parse_effect_type("fet").unwrap(), ast::EffectType::Fet);
    assert_eq!(
        parse_effect_type("plugin:com.surge.synth").unwrap(),
        ast::EffectType::Plugin("com.surge.synth".to_string())
    );
    assert!(parse_effect_type("unknown_effect").is_err());
}

#[test]
fn test_parse_dynamics() {
    assert_eq!(parse_dynamics("pp").unwrap(), Dynamics::PP);
    assert_eq!(parse_dynamics("MF").unwrap(), Dynamics::MF);
    assert_eq!(parse_dynamics("fff").unwrap(), Dynamics::FFF);
    assert!(parse_dynamics("invalid").is_err());
}

// ── Effect param value parsing ──────────────────────────────────────

#[test]
fn test_param_value_parsing_number() {
    let result = parse_param_value("3.5").unwrap();
    assert!(matches!(result, ast::ParamValue::Number(n) if (n - 3.5).abs() < 1e-10));
}

#[test]
fn test_param_value_parsing_bool() {
    assert!(matches!(
        parse_param_value("true").unwrap(),
        ast::ParamValue::Bool(true)
    ));
    assert!(matches!(
        parse_param_value("false").unwrap(),
        ast::ParamValue::Bool(false)
    ));
}

#[test]
fn test_param_value_parsing_db() {
    let result = parse_param_value("-6.0dB").unwrap();
    assert!(matches!(result, ast::ParamValue::Db(n) if (n - (-6.0)).abs() < 1e-10));
}

#[test]
fn test_param_value_parsing_percentage() {
    let result = parse_param_value("75%").unwrap();
    assert!(matches!(result, ast::ParamValue::Percentage(n) if (n - 75.0).abs() < 1e-10));
}

#[test]
fn test_param_value_parsing_frequency_time_and_cents() {
    let hz = parse_param_value("440Hz").unwrap();
    assert!(matches!(hz, ast::ParamValue::Frequency(n) if (n - 440.0).abs() < 1e-10));

    let khz = parse_param_value("1.5kHz").unwrap();
    assert!(matches!(khz, ast::ParamValue::Frequency(n) if (n - 1500.0).abs() < 1e-10));

    let ms = parse_param_value("250ms").unwrap();
    assert!(matches!(ms, ast::ParamValue::Time(n) if (n - 0.25).abs() < 1e-10));

    let seconds = parse_param_value("1.25s").unwrap();
    assert!(matches!(seconds, ast::ParamValue::Time(n) if (n - 1.25).abs() < 1e-10));

    let cents = parse_param_value("30ct").unwrap();
    assert!(matches!(cents, ast::ParamValue::Cents(n) if (n - 30.0).abs() < 1e-10));

    let text = parse_param_value("drums").unwrap();
    assert!(matches!(text, ast::ParamValue::String(s) if s == "drums"));
}

#[test]
fn test_apply_preset_params_rejects_invalid_batch_without_partial_mutation() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Piano 1".to_string(),
        source: Some("soundfont".to_string()),
        plugin: None,
        program: Some(0),
        patch: None,
        file: None,
        root_note: None,
        params: vec![(
            "existing".to_string(),
            ast::ParamValue::String("keep".to_string()),
        )],
        effects: vec![],
        extends: None,
    });

    let result = apply_preset_params(
        &mut project,
        "Piano 1",
        vec![
            (
                "scala_file".to_string(),
                Some("/tmp/tuning.scl".to_string()),
            ),
            ("   ".to_string(), Some("bad".to_string())),
        ],
    );

    assert!(result.is_err());
    assert_eq!(project.presets[0].params.len(), 1);
    assert_eq!(project.presets[0].params[0].0, "existing");
    assert!(matches!(
        &project.presets[0].params[0].1,
        ast::ParamValue::String(value) if value == "keep"
    ));
}

#[test]
fn test_apply_preset_params_updates_existing_param() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Piano 1".to_string(),
        source: Some("soundfont".to_string()),
        plugin: None,
        program: Some(0),
        patch: None,
        file: None,
        root_note: None,
        params: vec![(
            "cc_64_param".to_string(),
            ast::ParamValue::String("Sustain".to_string()),
        )],
        effects: vec![],
        extends: None,
    });

    apply_preset_params(
        &mut project,
        "Piano 1",
        vec![("cc_64_param".to_string(), Some("Damper".to_string()))],
    )
    .unwrap();

    assert_eq!(project.presets[0].params.len(), 1);
    assert_eq!(project.presets[0].params[0].0, "cc_64_param");
    assert!(matches!(
        &project.presets[0].params[0].1,
        ast::ParamValue::String(value) if value == "Damper"
    ));
}

#[test]
fn test_apply_preset_params_removes_param_with_none() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Piano 1".to_string(),
        source: Some("soundfont".to_string()),
        plugin: None,
        program: Some(0),
        patch: None,
        file: None,
        root_note: None,
        params: vec![
            (
                "scala_file".to_string(),
                ast::ParamValue::String("/tmp/tuning.scl".to_string()),
            ),
            (
                "scala_root_note".to_string(),
                ast::ParamValue::String("C4".to_string()),
            ),
        ],
        effects: vec![],
        extends: None,
    });

    apply_preset_params(
        &mut project,
        "Piano 1",
        vec![("scala_file".to_string(), None)],
    )
    .unwrap();

    assert_eq!(project.presets[0].params.len(), 1);
    assert_eq!(project.presets[0].params[0].0, "scala_root_note");
}

#[test]
fn test_apply_preset_params_adds_multiple_params_atomically() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Piano 1".to_string(),
        source: Some("soundfont".to_string()),
        plugin: None,
        program: Some(0),
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });

    apply_preset_params(
        &mut project,
        "Piano 1",
        vec![
            (
                "scala_file".to_string(),
                Some("/tmp/tuning.scl".to_string()),
            ),
            ("scala_root_note".to_string(), Some("C4".to_string())),
            (
                "articulation_legato_release_sustain_aware".to_string(),
                Some("true".to_string()),
            ),
        ],
    )
    .unwrap();

    assert_eq!(project.presets[0].params.len(), 3);
    assert!(matches!(
        &project.presets[0].params[0].1,
        ast::ParamValue::String(value) if value == "/tmp/tuning.scl"
    ));
    assert!(matches!(
        &project.presets[0].params[1].1,
        ast::ParamValue::String(value) if value == "C4"
    ));
    assert!(matches!(
        project.presets[0].params[2].1,
        ast::ParamValue::Bool(true)
    ));
}

#[test]
fn test_write_plugin_state_file_updates_preset_path() {
    let mut inner = test_state();
    inner.project.presets.push(ast::PresetDef {
        name: "ClapSynth".to_string(),
        source: Some("clap".to_string()),
        plugin: Some("vendor.clap".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    let path = std::env::temp_dir().join(format!(
        "opencompose-test-state-{}.clapstate",
        std::process::id()
    ));
    let path_string = path.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&path);

    write_plugin_state_file_and_update_preset(
        &mut inner,
        "ClapSynth",
        &path_string,
        &CapturedPluginStateFormat::Clap,
        "vendor.clap",
        vec![1, 2, 3, 4],
    )
    .unwrap();

    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Clap,
        Some("vendor.clap"),
        "ClapSynth",
    );
    assert_eq!(
        oc_host::decode_plugin_state(&std::fs::read(&path).unwrap(), &context).unwrap(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        &inner.project.presets[0].params[0].1,
        ast::ParamValue::String(value) if value == &path_string
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_write_plugin_state_file_failure_does_not_mutate_preset() {
    let mut inner = test_state();
    inner.project.presets.push(ast::PresetDef {
        name: "ClapSynth".to_string(),
        source: Some("clap".to_string()),
        plugin: Some("vendor.clap".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    let dir_path = std::env::temp_dir().to_string_lossy().to_string();

    let result = write_plugin_state_file_and_update_preset(
        &mut inner,
        "ClapSynth",
        &dir_path,
        &CapturedPluginStateFormat::Clap,
        "vendor.clap",
        vec![1, 2, 3, 4],
    );

    assert!(result.is_err());
    assert!(inner.project.presets[0].params.is_empty());
}

#[test]
fn test_embed_plugin_state_updates_preset_hex_and_clears_file() {
    let mut inner = test_state();
    inner.project.presets.push(ast::PresetDef {
        name: "ClapSynth".to_string(),
        source: Some("clap".to_string()),
        plugin: Some("vendor.clap".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![
            (
                "clap_state_file".to_string(),
                ast::ParamValue::String("/tmp/old.clapstate".to_string()),
            ),
            ("gain".to_string(), ast::ParamValue::Number(0.5)),
        ],
        effects: vec![],
        extends: None,
    });

    embed_plugin_state_and_update_preset(
        &mut inner,
        "ClapSynth",
        &CapturedPluginStateFormat::Clap,
        "vendor.clap",
        vec![0, 1, 0xab, 0xff],
    )
    .unwrap();

    assert!(
        inner.project.presets[0]
            .params
            .iter()
            .all(|(name, _)| name != "clap_state_file")
    );
    let hex = match inner.project.presets[0]
        .params
        .iter()
        .find(|(name, _)| name == "clap_state_hex")
        .map(|(_, value)| value)
    {
        Some(ast::ParamValue::String(value)) => value,
        other => panic!("missing clap_state_hex: {other:?}"),
    };
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Clap,
        Some("vendor.clap"),
        "ClapSynth",
    );
    assert_eq!(
        oc_host::decode_plugin_state(&decode_state_hex(hex), &context).unwrap(),
        vec![0, 1, 0xab, 0xff]
    );
    assert!(matches!(
        inner.project.presets[0]
            .params
            .iter()
            .find(|(name, _)| name == "gain")
            .map(|(_, value)| value),
        Some(ast::ParamValue::Number(0.5))
    ));
}

#[test]
fn test_encode_hex_lowercase() {
    assert_eq!(encode_hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
}

#[test]
fn test_embed_vst3_state_updates_vst3_params() {
    let mut inner = test_state();
    inner.project.presets.push(ast::PresetDef {
        name: "VstSynth".to_string(),
        source: Some("vst3".to_string()),
        plugin: Some("vendor.vst3".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![
            (
                "vst3_state_file".to_string(),
                ast::ParamValue::String("/tmp/old.vstpreset".to_string()),
            ),
            (
                "clap_state_hex".to_string(),
                ast::ParamValue::String("keep".to_string()),
            ),
        ],
        effects: vec![],
        extends: None,
    });

    embed_plugin_state_and_update_preset(
        &mut inner,
        "VstSynth",
        &CapturedPluginStateFormat::Vst3,
        "vendor.vst3",
        vec![0xde, 0xad, 0xbe, 0xef],
    )
    .unwrap();

    assert!(
        inner.project.presets[0]
            .params
            .iter()
            .all(|(name, _)| name != "vst3_state_file")
    );
    let hex = match inner.project.presets[0]
        .params
        .iter()
        .find(|(name, _)| name == "vst3_state_hex")
        .map(|(_, value)| value)
    {
        Some(ast::ParamValue::String(value)) => value,
        other => panic!("missing vst3_state_hex: {other:?}"),
    };
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Vst3,
        Some("vendor.vst3"),
        "VstSynth",
    );
    assert_eq!(
        oc_host::decode_plugin_state(&decode_state_hex(hex), &context).unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
    assert!(matches!(
        inner.project.presets[0]
            .params
            .iter()
            .find(|(name, _)| name == "clap_state_hex")
            .map(|(_, value)| value),
        Some(ast::ParamValue::String(value)) if value == "keep"
    ));
}

fn synth_automation_project() -> ast::Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Automation"
  tempo: 120
  time_signature: 4/4

@preset Lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {filter.cutoff: 12000}

@track Lead | instrument: synth/lead_saw, preset: Lead
  @phrase riff [1:1:0 - 3:1:0]
    C3 w mf
"#,
    )
    .unwrap()
}

/// Literal produced by `utils/automationTarget.ts` for `filter.cutoff`; the one
/// encoder is shared with the hosted-plugin path, not duplicated for synths.
#[test]
fn synth_automation_lane_name_matches_the_frontend_encoder() {
    assert_eq!(
        plugin_automation_lane_name("filter.cutoff"),
        "pluginparam_66696c7465722e6375746f6666"
    );
}

#[test]
fn synth_automation_gesture_writes_one_lane_as_one_undo_entry() {
    let mut inner = test_state();
    inner.project = synth_automation_project();
    inner.dsl_source = oc_dsl::serialize(&inner.project);

    begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").unwrap();
    // Several drag samples, as a real pointer drag produces.
    update_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.cutoff",
        0.5,
        Some(200.0),
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap();
    update_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.cutoff",
        0.75,
        Some(2000.0),
        ast::BarBeat::new(1, 3, 0),
    )
    .unwrap();
    update_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.cutoff",
        0.9,
        Some(8000.0),
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap();
    end_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff").unwrap();

    let lane = &inner.project.tracks[0].automation_lane(0).unwrap();
    assert_eq!(lane.param, "pluginparam_66696c7465722e6375746f6666");
    assert_eq!(lane.points.len(), 3);
    assert!(matches!(
        lane.points[0].value,
        ast::ParamValue::Number(value) if (value - 0.5).abs() < 1e-9
    ));
    assert!(matches!(
        lane.points[2].value,
        ast::ParamValue::Number(value) if (value - 0.9).abs() < 1e-9
    ));
    // The drag also wrote the preset value, so playback and display agree.
    let preset = inner
        .project
        .presets
        .iter()
        .find(|preset| preset.name == "Lead")
        .unwrap();
    assert!(preset.params.iter().any(|(key, value)| {
        key == "filter.cutoff"
            && matches!(value, ast::ParamValue::Number(number) if (number - 8000.0).abs() < 1e-9)
    }));

    assert_eq!(inner.undo.undo.len(), 1, "one drag is one undo entry");
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("one undo entry");
    assert!(project.tracks[0].automation_count() == 0);
}

#[test]
fn removing_synth_automation_is_one_undo_entry() {
    let mut inner = test_state();
    // Start from an authored lane so this checks removal alone; the removal's
    // own single command is the only entry involved.
    inner.project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Automation"
  tempo: 120
  time_signature: 4/4

@preset Lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {filter.cutoff: 8000}

@track Lead | instrument: synth/lead_saw, preset: Lead
  automation: pluginparam_66696c7465722e6375746f6666 { 1:1:0=0.867353 }
  @phrase riff [1:2:0 - 2:1:0]
    C3 h mf
"#,
    )
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);

    remove_synth_param_automation_inner(&mut inner, 0, "filter.cutoff").unwrap();
    assert!(inner.project.tracks[0].automation_count() == 0);
    assert_eq!(inner.undo.undo.len(), 1);

    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("one undo entry");
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().param,
        "pluginparam_66696c7465722e6375746f6666"
    );
}

#[test]
fn synth_automation_rejects_missing_targets_and_bad_values() {
    let mut inner = test_state();
    inner.project = synth_automation_project();

    assert!(begin_synth_param_gesture_inner(&mut inner, "Lead", 9, "filter.cutoff").is_err());
    assert!(begin_synth_param_gesture_inner(&mut inner, "Missing", 0, "filter.cutoff").is_err());
    begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").unwrap();
    assert!(
        begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.cutoff").is_err(),
        "one gesture per parameter at a time"
    );
    assert!(
        update_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "filter.cutoff",
            f64::NAN,
            None,
            ast::BarBeat::new(1, 1, 0),
        )
        .is_err()
    );
    assert!(
        update_synth_param_gesture_inner(
            &mut inner,
            "Lead",
            "filter.cutoff",
            0.5,
            None,
            ast::BarBeat::new(0, 1, 0),
        )
        .is_err()
    );
    end_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff").unwrap();
    assert!(end_synth_param_gesture_inner(&mut inner, "Lead", "filter.cutoff").is_err());
    assert!(remove_synth_param_automation_inner(&mut inner, 0, "filter.cutoff").is_err());
}

#[test]
fn first_synth_automation_write_migrates_legacy_lanes_and_stamps_the_table() {
    let mut inner = test_state();
    // A lane written by the old editor: 1.0 selected filter model 2.
    inner.project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Automation"
  tempo: 120
  time_signature: 4/4

@preset Lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {filter.model: 2}

@track Lead | instrument: synth/lead_saw, preset: Lead
  automation: pluginparam_66696c7465722e6d6f64656c { 1:1:0=1 }
  automation: pluginparam_66696c7465722e6375746f6666 { 1:1:0=200Hz }
"#,
    )
    .unwrap();
    assert!(
        !inner
            .project
            .properties
            .uses_wide_automation_normalization()
    );

    let before_begin = inner.project.clone();
    begin_synth_param_gesture_inner(&mut inner, "Lead", 0, "filter.model").unwrap();
    assert_eq!(
        inner.project, before_begin,
        "begin alone must not migrate the document"
    );
    // New points use the widened positions; the migrated point keeps model 2.
    update_synth_param_gesture_inner(
        &mut inner,
        "Lead",
        "filter.model",
        3.0 / 11.0,
        None,
        ast::BarBeat::new(1, 2, 0),
    )
    .unwrap();

    assert_eq!(
        inner.project.properties.automation_normalization,
        Some(ast::WIDE_AUTOMATION_NORMALIZATION)
    );
    let migrated = inner.project.tracks[0].automation_lane(0).unwrap().points[0]
        .value
        .as_f64()
        .unwrap();
    assert!(
        (migrated - 2.0 / 11.0).abs() < 1e-12,
        "migrated to {migrated}"
    );
    // A point written in the patch's own unit is table independent.
    assert!(
        matches!(
            inner.project.tracks[0].automation_lane(1).unwrap().points[0].value,
            ast::ParamValue::Frequency(value) if value == 200.0
        ),
        "a physical point keeps its unit"
    );

    end_synth_param_gesture_inner(&mut inner, "Lead", "filter.model").unwrap();

    let text = oc_dsl::serialize(&inner.project);
    assert!(text.contains("automation_normalization: 2"), "{text}");
    let reparsed = oc_dsl::parse(&text).unwrap();
    assert_eq!(oc_dsl::serialize(&reparsed), text);
    assert_eq!(
        reparsed.properties.automation_normalization,
        Some(ast::WIDE_AUTOMATION_NORMALIZATION)
    );
}

fn decode_state_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect()
}

// ── Slip: move content under a fixed clip ─────────────────────────────────────

/// Six seconds of a stereo ramp: frame `f` reads `f × 1e-6`, so a rendered
/// sample names the source frame it came from.
fn write_slip_media(path: &Path, seconds: f64) {
    const RATE: u32 = 48_000;
    let frames = (f64::from(RATE) * seconds).round() as usize;
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..frames {
        let value = frame as f32 * 1e-6;
        writer.write_sample(value).unwrap();
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

/// Two bars of a six-second file: two seconds of media are hidden at the tail.
fn audio_clip_slip_project() -> (TempDir, crate::state::AppStateInner) {
    let dir = TempDir::new("clip-slip");
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    write_slip_media(&dir.path().join("media/hit.wav"), 6.0);
    let mut inner = audio_clip_project();
    inner.file_path = Some(dir.path().join("project.ocp"));
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.gain_db = 0.0;
    clip.fade_in_seconds = 0.0;
    clip.fade_out_seconds = 0.0;
    (dir, inner)
}

#[test]
fn audio_clip_playback_preflight_rejects_content_changed_inside_a_transaction() {
    let (_dir, mut inner) = audio_clip_slip_project();
    inner
        .begin_transaction("edit".into(), "test".into())
        .unwrap();
    let prepared = ClipPlaybackEdit::capture(&inner)
        .prepare(
            0,
            AudioClipPlayback {
                pitch: Some(2.0),
                ..Default::default()
            },
        )
        .unwrap();
    let revision = inner.revision;
    inner.project.audio_clip_mut(0).unwrap().gain_db = -4.0;
    inner.bump_revision();
    assert_eq!(inner.revision, revision);
    let error = prepared.commit(&mut inner).unwrap_err();
    assert!(error.contains("project changed"));
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.gain_db, -4.0);
    assert_eq!(clip.pitch, 0.0);
}

#[test]
fn audio_clip_playback_edits_persist_and_invalid_combinations_leave_no_undo() {
    use oc_dsl::ast::StretchMode;

    let (dir, mut inner) = audio_clip_slip_project();
    let original = inner.project.audio_clip(0).unwrap().clone();
    let error = set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            pitch: Some(7.0),
            stretch_mode: Some(StretchMode::Off),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("pitch needs a stretch mode"));
    assert_eq!(inner.project.audio_clip(0), Some(&original));
    assert!(inner.undo.undo.is_empty());

    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            stretch: Some(0.5),
            pitch: Some(7.0),
            ..Default::default()
        },
    )
    .unwrap();
    let saved = oc_dsl::parse(&inner.dsl_source).unwrap();
    let clip = saved.audio_clip(0).unwrap();
    assert_eq!(clip.stretch, 0.5);
    assert_eq!(clip.pitch, 7.0);
    assert_eq!(clip.time_range, original.time_range);
    assert_eq!(clip.file, original.file);
    assert_eq!(inner.undo.undo.len(), 1);

    let mut buffer = vec![0.0_f32; 4 * 48_000 * 2];
    oc_engine::audio_clip::mix_into_track(&mut buffer, &saved, clip, 48_000, Some(dir.path()))
        .unwrap();
    assert!(buffer[2 * 48_000 * 2].abs() > 1e-3);
    assert_eq!(
        buffer[7 * 48_000],
        0.0,
        "the faster media ends inside the fixed range"
    );

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.audio_clip(0), Some(&original));
}

#[test]
fn audio_clip_playback_fit_source_clears_trim_and_loop_and_plays_the_tail() {
    const RATE: u32 = 48_000;
    for loop_seconds in [0.0, 1.25] {
        let (dir, mut inner) = audio_clip_slip_project();
        let mut writer = hound::WavWriter::create(
            dir.path().join("media/hit.wav"),
            hound::WavSpec {
                channels: 2,
                sample_rate: RATE,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for frame in 0..6 * RATE {
            let amplitude = if frame < 3 * RATE { 0.08 } else { 0.35 };
            let phase = 2.0 * std::f64::consts::PI * 440.0 * f64::from(frame) / f64::from(RATE);
            let value = (amplitude * phase.sin()) as f32;
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.offset_seconds = 1.0;
        clip.loop_seconds = loop_seconds;
        clip.stretch_mode = ast::StretchMode::Varispeed;
        let original = clip.clone();
        let mut expected = inner.project.clone();
        let clip = expected.audio_clip_mut(0).unwrap();
        clip.tempo_follow = true;
        clip.offset_seconds = 0.0;
        clip.loop_seconds = 0.0;
        let render = |project: &ast::Project| {
            let mut pcm = vec![0.0_f32; 4 * RATE as usize * 2];
            oc_engine::audio_clip::mix_into_track(
                &mut pcm,
                project,
                project.audio_clip(0).unwrap(),
                RATE,
                Some(dir.path()),
            )
            .unwrap();
            pcm
        };
        set_audio_clip_playback_inner(
            &mut inner,
            0,
            AudioClipPlayback {
                tempo_follow: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let reopened = oc_dsl::parse(&inner.dsl_source).unwrap();
        let clip = reopened.audio_clip(0).unwrap();
        assert_eq!(clip.offset_seconds, 0.0);
        assert_eq!(clip.loop_seconds, 0.0);
        assert!(clip.tempo_follow);
        assert_eq!(clip.time_range, original.time_range);
        let actual = render(&reopened);
        assert_eq!(actual, render(&expected));
        let tail = &actual[(13 * RATE as usize / 4) * 2..(15 * RATE as usize / 4) * 2];
        let power = tail
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            / tail.len() as f64;
        assert!(
            power.sqrt() > 0.2,
            "the fitted source tail must remain audible"
        );
        assert_eq!(inner.undo.undo.len(), 1);
        inner.undo.undo(&mut inner.project).unwrap();
        assert_eq!(inner.project.audio_clip(0), Some(&original));
    }
}

#[test]
fn audio_clip_playback_unrelated_edit_preserves_existing_fit_offsets() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.tempo_follow = true;
    clip.offset_seconds = 1.0;
    clip.loop_seconds = 0.5;
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            bypass_processing: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!(clip.tempo_follow);
    assert!(clip.bypass_processing);
    assert_eq!(clip.offset_seconds, 1.0);
    assert_eq!(clip.loop_seconds, 0.5);
}

#[test]
fn audio_clip_playback_fit_refuses_conflicts_without_writing_an_edit() {
    let (_dir, mut inner) = audio_clip_slip_project();
    inner.project.audio_clip_mut(0).unwrap().offset_seconds = 1.0;
    let original = inner.project.audio_clip(0).unwrap().clone();
    for playback in [
        AudioClipPlayback {
            tempo_follow: Some(true),
            stretch: Some(0.5),
            ..Default::default()
        },
        AudioClipPlayback {
            tempo_follow: Some(true),
            stretch_mode: Some(ast::StretchMode::Off),
            ..Default::default()
        },
        AudioClipPlayback {
            tempo_follow: Some(true),
            warp_markers: Some(vec![ast::AudioWarpMarker {
                source_seconds: 1.0,
                target_seconds: 1.5,
            }]),
            ..Default::default()
        },
    ] {
        let error = set_audio_clip_playback_inner(&mut inner, 0, playback).unwrap_err();
        assert!(
            error.contains("fit") || error.contains("Fit") || error.contains("tempo"),
            "{error}"
        );
        assert_eq!(inner.project.audio_clip(0), Some(&original));
        assert!(inner.undo.undo.is_empty());
    }
}

#[test]
fn audio_clip_playback_fit_can_resize_the_destination_in_the_same_edit() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.offset_seconds = 1.0;
    clip.loop_seconds = 0.5;
    let end = ast::BarBeat::new(4, 1, 0);
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            tempo_follow: Some(true),
            stretch_to: Some(end),
            ..Default::default()
        },
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!(clip.tempo_follow);
    assert_eq!(clip.time_range.end, end);
    assert_eq!(clip.offset_seconds, 0.0);
    assert_eq!(clip.loop_seconds, 0.0);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_playback_bypass_round_trips_and_restores_the_processed_audio() {
    let (dir, mut inner) = audio_clip_slip_project();
    let render = |project: &ast::Project| {
        let mut audio = vec![0.0; 4 * 48_000 * 2];
        oc_engine::audio_clip::mix_into_track(
            &mut audio,
            project,
            project.audio_clip(0).unwrap(),
            48_000,
            Some(dir.path()),
        )
        .unwrap();
        audio
    };
    let raw = render(&inner.project);
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            pitch: Some(3.0),
            formant: Some(1.0),
            preserve_formants: Some(true),
            warp_markers: Some(vec![ast::AudioWarpMarker {
                source_seconds: 1.0,
                target_seconds: 1.5,
            }]),
            ..Default::default()
        },
    )
    .unwrap();
    let configured = inner.project.audio_clip(0).unwrap().clone();
    let processed = render(&inner.project);
    assert_ne!(processed, raw);

    for (bypassed, expected) in [(true, &raw), (false, &processed)] {
        set_audio_clip_playback_inner(
            &mut inner,
            0,
            AudioClipPlayback {
                bypass_processing: Some(bypassed),
                ..Default::default()
            },
        )
        .unwrap();
        let restored = oc_dsl::parse(&inner.dsl_source).unwrap();
        let mut clip = restored.audio_clip(0).unwrap().clone();
        assert_eq!(clip.bypass_processing, bypassed);
        clip.bypass_processing = false;
        assert_eq!(clip, configured);
        assert_eq!(&render(&restored), expected);
    }
}

#[test]
fn audio_clip_playback_varispeed_applies_one_resize_and_persists() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let end = ast::BarBeat::new(2, 1, 0);
    let revision = inner.revision;
    for playback in [
        AudioClipPlayback {
            stretch: Some(0.5),
            stretch_to: Some(end),
            ..Default::default()
        },
        AudioClipPlayback {
            pitch: Some(1.0),
            stretch_mode: Some(ast::StretchMode::Varispeed),
            ..Default::default()
        },
    ] {
        assert!(set_audio_clip_playback_inner(&mut inner, 0, playback).is_err());
        assert_eq!(inner.revision, revision);
    }
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            stretch_to: Some(end),
            stretch_mode: Some(ast::StretchMode::Varispeed),
            ..Default::default()
        },
    )
    .unwrap();
    let reopened = oc_dsl::parse(&inner.dsl_source).unwrap();
    let clip = reopened.audio_clip(0).unwrap();
    assert_eq!(clip.stretch_mode, ast::StretchMode::Varispeed);
    assert_eq!(clip.time_range.end, end);
    assert_eq!(clip.stretch, 0.5);
    assert_eq!(clip.pitch, 0.0);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn audio_clip_playback_warp_scales_with_stretch_and_round_trips() {
    let (dir, mut inner) = audio_clip_slip_project();
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            formant: Some(2.0),
            preserve_formants: Some(true),
            warp_markers: Some(vec![ast::AudioWarpMarker {
                source_seconds: 1.0,
                target_seconds: 1.5,
            }]),
            ..Default::default()
        },
    )
    .unwrap();
    let root = Some(dir.path());
    let original = inner.project.audio_clip(0).unwrap().clone();
    let before = oc_engine::audio_clip::content_seconds(&inner.project, &original, root).unwrap();
    let end = ast::BarBeat::new(2, 1, 0);
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            stretch_to: Some(end),
            ..Default::default()
        },
    )
    .unwrap();
    let saved = oc_dsl::parse(&inner.dsl_source).unwrap();
    let clip = saved.audio_clip(0).unwrap();
    assert_eq!(clip.time_range.end, end);
    assert_eq!(clip.warp_markers[0].target_seconds, 0.75);
    assert_eq!(clip.formant, 2.0);
    assert!(clip.preserve_formants);
    let after = oc_engine::audio_clip::content_seconds(&saved, clip, root).unwrap();
    assert!((after - before / 2.0).abs() < 0.0001);
    let revision = inner.revision;
    assert!(
        set_audio_clip_playback_inner(
            &mut inner,
            0,
            AudioClipPlayback {
                warp_markers: Some(vec![ast::AudioWarpMarker {
                    source_seconds: 7.0,
                    target_seconds: 1.0
                }]),
                ..Default::default()
            },
        )
        .is_err()
    );
    assert_eq!(inner.revision, revision);
}

#[test]
fn audio_clip_playback_split_and_trim_freeze_tempo_follow_without_retiming() {
    let (dir, mut inner) = audio_clip_slip_project();
    inner.project.audio_clip_mut(0).unwrap().tempo_follow = true;
    let mut before = vec![0.0_f32; 4 * 48_000 * 2];
    oc_engine::audio_clip::mix_into_track(
        &mut before,
        &inner.project,
        inner.project.audio_clip(0).unwrap(),
        48_000,
        Some(dir.path()),
    )
    .unwrap();
    let at = ast::BarBeat::new(2, 1, 0);
    split_audio_clip_inner(&mut inner, 0, at).unwrap();
    let mut after = vec![0.0_f32; before.len()];
    for clip in inner.project.audio_clips() {
        assert!(!clip.tempo_follow);
        assert!((clip.stretch - 2.0 / 3.0).abs() < 1e-9);
        oc_engine::audio_clip::mix_into_track(
            &mut after,
            &inner.project,
            clip,
            48_000,
            Some(dir.path()),
        )
        .unwrap();
    }
    assert_eq!(before, after);

    let (_dir, mut trimmed) = audio_clip_slip_project();
    trimmed.project.audio_clip_mut(0).unwrap().tempo_follow = true;
    trim_audio_clip_inner(&mut trimmed, 0, at, ast::BarBeat::new(3, 1, 0)).unwrap();
    let clip = trimmed.project.audio_clip(0).unwrap();
    assert!(!clip.tempo_follow);
    assert_eq!(clip.offset_seconds, 2.0);
    assert!((clip.stretch - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn audio_clip_slip_writes_the_offset_and_keeps_the_range() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let range = inner.project.audio_clip(0).unwrap().time_range;
    set_audio_clip_offset_inner(&mut inner, 0, 1.5).unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert!((clip.offset_seconds - 1.5).abs() < 1e-9);
    assert_eq!(clip.time_range, range, "slip never moves the clip");
    assert!(
        inner.dsl_source.contains("offset: 1.5s"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "clip slip");
}

#[test]
fn audio_clip_slip_survives_save_and_reload() {
    let (_dir, mut inner) = audio_clip_slip_project();
    set_audio_clip_offset_inner(&mut inner, 0, 1.5).unwrap();
    let saved = inner.dsl_source.clone();
    let reloaded = oc_dsl::parse(&saved).expect("the saved project re-parses");
    assert_eq!(
        oc_dsl::serialize(&reloaded),
        saved,
        "a slipped clip keeps its text"
    );
    assert!((reloaded.audio_clip(0).unwrap().offset_seconds - 1.5).abs() < 1e-9);
}

#[test]
fn audio_clip_slip_is_one_undo_entry_per_drag() {
    let (_dir, mut inner) = audio_clip_slip_project();
    inner.undo.begin_gesture(UndoScope::Project);
    set_audio_clip_offset_inner(&mut inner, 0, 0.5).unwrap();
    set_audio_clip_offset_inner(&mut inner, 0, 1.0).unwrap();
    set_audio_clip_offset_inner(&mut inner, 0, 1.5).unwrap();
    inner.undo.end_gesture();
    assert_eq!(inner.undo.undo.len(), 1, "a drag is one history entry");
    assert!((inner.project.audio_clip(0).unwrap().offset_seconds - 1.5).abs() < 1e-9);

    inner.undo.undo(&mut inner.project).unwrap();
    assert!((inner.project.audio_clip(0).unwrap().offset_seconds).abs() < 1e-9);
}

#[test]
fn audio_clip_slip_clamps_at_the_hidden_tail() {
    let (_dir, mut inner) = audio_clip_slip_project();
    // Four seconds of clip over six seconds of media leaves two to slip.
    set_audio_clip_offset_inner(&mut inner, 0, 99.0).unwrap();
    assert!((inner.project.audio_clip(0).unwrap().offset_seconds - 2.0).abs() < 1e-9);
}

#[test]
fn audio_clip_slip_refuses_a_whole_content_clip() {
    let (_dir, mut inner) = audio_clip_slip_project();
    // Eight seconds of clip over six seconds of media: nothing is hidden.
    inner.project.audio_clip_mut(0).unwrap().time_range.end = ast::BarBeat::new(5, 1, 0);
    let before = inner.project.clone();
    let error = set_audio_clip_offset_inner(&mut inner, 0, 1.0).unwrap_err();
    assert_eq!(error, SLIP_NOTHING_TO_SLIP);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn a_slipped_audio_clip_renders_from_the_offset() {
    let (dir, mut inner) = audio_clip_slip_project();
    set_audio_clip_offset_inner(&mut inner, 0, 0.5).unwrap();
    let mut buffer = vec![0.0_f32; 4 * 48_000 * 2];
    oc_engine::audio_clip::mix_into_track(
        &mut buffer,
        &inner.project,
        inner.project.audio_clip(0).unwrap(),
        48_000,
        Some(dir.path()),
    )
    .unwrap();
    // The ramp value names its source frame, so the head of the render is the
    // half-second the slip hid. Decoding may land a frame either side.
    let expected = (0.5 * 48_000.0) as f32 * 1e-6;
    assert!((buffer[0] - expected).abs() < 4e-6, "{}", buffer[0]);
    assert!((buffer[2] - expected).abs() < 4e-6, "{}", buffer[2]);
}

#[test]
fn audio_clip_slip_refuses_a_non_finite_offset() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let error = set_audio_clip_offset_inner(&mut inner, 0, f64::NAN).unwrap_err();
    assert!(error.contains("zero or longer"), "{error}");
}

// ── Slip on a pattern clip ───────────────────────────────────────────────────

/// A four-beat pattern in a two-beat clip: two beats of content are hidden.
fn pattern_clip_slip_project() -> crate::state::AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
        r#"@project "Pattern slip"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [2:1 - 2:3]
"#,
    )
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

#[test]
fn pattern_clip_slip_writes_the_offset_and_keeps_the_range() {
    let mut inner = pattern_clip_slip_project();
    let range = inner.project.tracks[0].pattern_clip(0).unwrap().time_range;
    set_pattern_clip_offset_inner(&mut inner, 0, 0, 1.5).unwrap();
    let clip = &inner.project.tracks[0].pattern_clip(0).unwrap();
    assert!((clip.offset_quarters - 1.5).abs() < 1e-9);
    assert_eq!(clip.time_range, range, "slip never moves the clip");
    assert!(
        inner
            .dsl_source
            .contains("clip: riff [2:1 - 2:3] offset: 1.5q"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "clip slip");

    inner.undo.undo(&mut inner.project).unwrap();
    assert!(
        (inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .offset_quarters)
            .abs()
            < 1e-9
    );
}

#[test]
fn pattern_clip_slip_is_one_undo_entry_per_drag() {
    let mut inner = pattern_clip_slip_project();
    inner.undo.begin_gesture(UndoScope::Project);
    set_pattern_clip_offset_inner(&mut inner, 0, 0, 0.5).unwrap();
    set_pattern_clip_offset_inner(&mut inner, 0, 0, 1.0).unwrap();
    set_pattern_clip_offset_inner(&mut inner, 0, 0, 1.5).unwrap();
    inner.undo.end_gesture();
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(
        (inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .offset_quarters
            - 1.5)
            .abs()
            < 1e-9
    );
}

#[test]
fn pattern_clip_slip_refuses_a_whole_content_clip() {
    let mut inner = pattern_clip_slip_project();
    // Four beats of clip over four beats of pattern: nothing is hidden.
    inner.project.tracks[0]
        .pattern_clip_mut(0)
        .unwrap()
        .time_range
        .end = ast::BarBeat::new(3, 1, 0);
    let before = inner.project.clone();
    let error = set_pattern_clip_offset_inner(&mut inner, 0, 0, 1.0).unwrap_err();
    assert_eq!(error, SLIP_NOTHING_TO_SLIP_PATTERN);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn pattern_clip_slip_clamps_at_the_content_end() {
    let mut inner = pattern_clip_slip_project();
    set_pattern_clip_offset_inner(&mut inner, 0, 0, 99.0).unwrap();
    assert!(
        (inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .offset_quarters
            - 2.0)
            .abs()
            < 1e-9
    );
}

// ── Clip loop points ─────────────────────────────────────────────────────────

#[test]
fn audio_clip_loop_sets_once_and_clears() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let before = inner.project.clone();
    set_audio_clip_loop_inner(&mut inner, 0, Some(1.5)).unwrap();
    assert!((inner.project.audio_clip(0).unwrap().loop_seconds - 1.5).abs() < 1e-9);
    assert!(
        inner.dsl_source.contains("loop: 1.5s"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "clip loop");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);

    set_audio_clip_loop_inner(&mut inner, 0, Some(1.5)).unwrap();
    set_audio_clip_loop_inner(&mut inner, 0, None).unwrap();
    assert_eq!(inner.project.audio_clip(0).unwrap().loop_seconds, 0.0);
    assert!(!inner.dsl_source.contains("loop:"), "{}", inner.dsl_source);
}

#[test]
fn audio_clip_loop_is_one_undo_entry_per_gesture() {
    let (_dir, mut inner) = audio_clip_slip_project();
    inner.undo.begin_gesture(UndoScope::Project);
    set_audio_clip_loop_inner(&mut inner, 0, Some(0.5)).unwrap();
    set_audio_clip_loop_inner(&mut inner, 0, Some(1.0)).unwrap();
    set_audio_clip_loop_inner(&mut inner, 0, Some(1.5)).unwrap();
    inner.undo.end_gesture();
    assert_eq!(inner.undo.undo.len(), 1, "a drag is one history entry");
    assert!((inner.project.audio_clip(0).unwrap().loop_seconds - 1.5).abs() < 1e-9);
}

#[test]
fn audio_clip_loop_refuses_media_shorter_than_100ms() {
    let dir = TempDir::new("clip-short-loop");
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    write_slip_media(&dir.path().join("media/hit.wav"), 0.05);
    let mut inner = audio_clip_project();
    inner.file_path = Some(dir.path().join("project.ocp"));
    let before = inner.project.clone();
    let error = set_audio_clip_loop_inner(&mut inner, 0, Some(0.04)).unwrap_err();
    assert_eq!(error, LOOP_TOO_SHORT_MEDIA);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn audio_clip_loop_refuses_a_loop_as_long_as_the_clip() {
    let (_dir, mut inner) = audio_clip_slip_project();
    let error = set_audio_clip_loop_inner(&mut inner, 0, Some(4.0)).unwrap_err();
    assert_eq!(error, LOOP_NOT_SHORTER_THAN_CLIP);
    let error = set_audio_clip_loop_inner(&mut inner, 0, Some(0.05)).unwrap_err();
    assert_eq!(error, LOOP_TOO_SHORT);
}

#[test]
fn audio_clip_loop_refuses_a_region_that_runs_past_the_media() {
    let (_dir, mut inner) = audio_clip_slip_project();
    // Five seconds into a six-second file leaves room for a one-second loop.
    inner.project.audio_clip_mut(0).unwrap().offset_seconds = 5.0;
    let error = set_audio_clip_loop_inner(&mut inner, 0, Some(2.0)).unwrap_err();
    assert_eq!(error, LOOP_PAST_MEDIA);
    set_audio_clip_loop_inner(&mut inner, 0, Some(1.0)).unwrap();
    assert!((inner.project.audio_clip(0).unwrap().loop_seconds - 1.0).abs() < 1e-9);
}

#[test]
fn pattern_clip_loop_sets_once_and_clears() {
    let mut inner = pattern_clip_slip_project();
    set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(1.5)).unwrap();
    assert!(
        (inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .loop_quarters
            - 1.5)
            .abs()
            < 1e-9
    );
    assert!(
        inner
            .dsl_source
            .contains("clip: riff [2:1 - 2:3] loop: 1.5q"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo[0].label, "clip loop");

    set_pattern_clip_loop_inner(&mut inner, 0, 0, None).unwrap();
    assert_eq!(
        inner.project.tracks[0]
            .pattern_clip(0)
            .unwrap()
            .loop_quarters,
        0.0
    );
    assert!(!inner.dsl_source.contains("loop:"), "{}", inner.dsl_source);
}

#[test]
fn pattern_clip_loop_preserves_the_declared_silent_tail_when_split_at_a_seam() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
        r#"@project "Six-quarter cycle"
  tempo: 120
  time_signature: 4/4

@pattern low_tom | instrument: drums/acoustic_kit, bars: 1.5
  hits:
    tom_low 1:2 mf
    tom_low 4:2 mf
    tom_low 6:2 mf

@track drums | instrument: drums/acoustic_kit
  clip: low_tom [1:1 - 7:1]
"#,
    )
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    assert_eq!(inner.project.patterns[0].content_quarters(), 5.5);
    assert_eq!(
        set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(6.25)).unwrap_err(),
        LOOP_PAST_CONTENT
    );

    set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(6.0)).unwrap();
    assert!(
        inner.dsl_source.contains("loop: 6q"),
        "{}",
        inner.dsl_source
    );
    split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(2, 3, 0)).unwrap();

    let mut clips = inner.project.tracks[0].pattern_clips();
    let left = clips.next().unwrap();
    let right = clips.next().unwrap();
    assert_eq!(left.loop_quarters, 0.0, "the first six quarters play once");
    assert_eq!(right.offset_quarters, 0.0);
    assert_eq!(right.loop_quarters, 6.0);
}

#[test]
fn pattern_clip_loop_refuses_a_loop_past_the_content_or_the_clip() {
    let mut inner = pattern_clip_slip_project();
    let error = set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(2.0)).unwrap_err();
    assert_eq!(error, LOOP_NOT_SHORTER_THAN_CLIP);
    let error = set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(0.1)).unwrap_err();
    assert_eq!(error, LOOP_TOO_SHORT_PATTERN);

    // Content is four beats: a three-beat offset leaves one beat to repeat.
    inner.project.tracks[0]
        .pattern_clip_mut(0)
        .unwrap()
        .offset_quarters = 3.0;
    let error = set_pattern_clip_loop_inner(&mut inner, 0, 0, Some(1.5)).unwrap_err();
    assert_eq!(error, LOOP_PAST_CONTENT);
}

// ── Splitting a pattern clip ────────────────────────────────────────────────

/// An eight-quarter clip over `notes` quarters of pattern content, with the
/// given slip and loop.
fn pattern_clip_window_project(
    notes: usize,
    offset: f64,
    loop_quarters: f64,
) -> crate::state::AppStateInner {
    let mut events = String::new();
    for beat in 0..notes {
        if beat == 0 {
            events.push_str("    C4 q mf\n");
        } else {
            events.push_str(&format!("    @{beat} C4 q mf\n"));
        }
    }
    let dsl = format!(
        r#"@project "Pattern window"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
{events}
@track keys | instrument: piano/acoustic_grand
  clip: riff [1:1 - 3:1]
"#
    );
    let mut inner = test_state();
    inner.project = oc_dsl::parse(&dsl).unwrap();
    let clip = inner.project.tracks[0].pattern_clip_mut(0).unwrap();
    clip.offset_quarters = offset;
    clip.loop_quarters = loop_quarters;
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

fn clip_ranges(inner: &crate::state::AppStateInner) -> Vec<ast::TimeRange> {
    inner.project.tracks[0]
        .pattern_clips()
        .map(|clip| clip.time_range)
        .collect()
}

#[test]
fn split_pattern_clip_keeps_the_slip_and_loop_in_both_halves() {
    let mut inner = pattern_clip_window_project(4, 1.0, 2.0);
    let before = inner.project.clone();

    split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(2, 1, 0)).unwrap();

    assert_eq!(
        clip_ranges(&inner),
        vec![
            ast::TimeRange {
                start: ast::BarBeat::new(1, 1, 0),
                end: ast::BarBeat::new(2, 1, 0),
            },
            ast::TimeRange {
                start: ast::BarBeat::new(2, 1, 0),
                end: ast::BarBeat::new(3, 1, 0),
            },
        ]
    );
    let clips: Vec<_> = inner.project.tracks[0].pattern_clips().cloned().collect();
    // Four quarters of loop fit in each half, so both keep repeating.
    for clip in &clips {
        assert!((clip.offset_quarters - 1.0).abs() < 1e-9, "{clip:?}");
        assert!((clip.loop_quarters - 2.0).abs() < 1e-9, "{clip:?}");
    }
    // Quarter 4 is two whole loop periods in, so the right half resumes on the
    // same content position instead of restarting at the pattern head.
    assert!((clips[1].offset_quarters - 1.0).abs() < 1e-9);
    assert!(
        inner.dsl_source.contains("offset: 1q loop: 2q"),
        "{}",
        inner.dsl_source
    );

    // One history entry restores the clip exactly.
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "clip split");
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, before);
}

#[test]
fn split_pattern_clip_advances_the_right_offset_by_the_elapsed_window() {
    let mut inner = pattern_clip_window_project(4, 1.5, 0.0);

    // Two and a quarter quarters in, the right half starts 3.75 quarters into
    // the content instead of at its head.
    split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(1, 3, 1)).unwrap();

    let clips: Vec<_> = inner.project.tracks[0].pattern_clips().cloned().collect();
    assert!((clips[0].offset_quarters - 1.5).abs() < 1e-9);
    assert!((clips[0].loop_quarters).abs() < 1e-9);
    assert!((clips[1].offset_quarters - 3.75).abs() < 1e-9);
    assert!((clips[1].loop_quarters).abs() < 1e-9);
}

#[test]
fn split_pattern_clip_refuses_a_right_half_past_the_content() {
    let mut inner = pattern_clip_window_project(4, 1.5, 0.0);
    let before = inner.project.clone();

    // Four quarters in, a single pass would start 5.5 quarters into four
    // quarters of content, which renders as silence.
    let error = split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(2, 1, 0)).unwrap_err();

    assert_eq!(error, SPLIT_PAST_CONTENT);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn split_pattern_clip_refuses_a_seam_the_right_half_cannot_repeat() {
    // One quarter of slip and a six-quarter loop over eight quarters of
    // content: four quarters in, the next seam falls inside a right half that
    // is too short to hold a loop of its own.
    let mut inner = pattern_clip_window_project(8, 1.0, 6.0);
    let before = inner.project.clone();

    let error = split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(2, 1, 0)).unwrap_err();

    assert_eq!(error, SPLIT_LOOP_SEAM);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn split_pattern_clip_refuses_to_shift_a_repeating_source_window() {
    let mut inner = pattern_clip_window_project(8, 1.0, 2.0);
    let before = inner.project.clone();

    // The remaining seven quarters can hold a loop, but the right half
    // needs one quarter of the old cycle before restarting at its old offset.
    let error = split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(1, 2, 0)).unwrap_err();

    assert_eq!(error, SPLIT_LOOP_SEAM);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn split_pattern_clip_refuses_a_loop_window_past_the_content() {
    let mut inner = pattern_clip_window_project(8, 1.0, 6.0);
    let before = inner.project.clone();

    // 1.5 quarters in, the right half resumes at content 2.5 and its six-quarter
    // loop would run to 8.5 of eight quarters.
    let error = split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(1, 2, 2)).unwrap_err();

    assert_eq!(error, SPLIT_LOOP_CONTENT);
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn split_pattern_clip_refuses_a_point_outside_the_clip() {
    let mut inner = pattern_clip_window_project(4, 1.0, 0.0);
    let before = inner.project.clone();

    let error = split_clip_inner(&mut inner, 0, 0, ast::BarBeat::new(1, 1, 0)).unwrap_err();

    assert_eq!(error, SPLIT_INSIDE_CLIP);
    assert_eq!(inner.project, before);
}

#[test]
fn add_clip_carries_a_playback_window() {
    let mut inner = pattern_clip_slip_project();
    add_clip_inner(
        &mut inner,
        0,
        "riff",
        ast::BarBeat::new(3, 1, 0),
        ast::BarBeat::new(4, 1, 0),
        Some(1.5),
        None,
    )
    .unwrap();

    let clip = inner.project.tracks[0].pattern_clip(1).unwrap();
    assert!((clip.offset_quarters - 1.5).abs() < 1e-9);
    assert!((clip.loop_quarters).abs() < 1e-9);
    assert!(
        inner.dsl_source.contains("offset: 1.5q"),
        "{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "add clip");
}

#[test]
fn add_clip_without_a_window_stays_a_single_pass() {
    let mut inner = pattern_clip_slip_project();
    add_clip_inner(
        &mut inner,
        0,
        "riff",
        ast::BarBeat::new(3, 1, 0),
        ast::BarBeat::new(4, 1, 0),
        None,
        None,
    )
    .unwrap();

    let clip = inner.project.tracks[0].pattern_clip(1).unwrap();
    assert!((clip.offset_quarters).abs() < 1e-9);
    assert!((clip.loop_quarters).abs() < 1e-9);
}

#[test]
fn add_clip_refuses_a_loop_as_long_as_the_clip() {
    let mut inner = pattern_clip_slip_project();
    let before = inner.project.clone();

    let error = add_clip_inner(
        &mut inner,
        0,
        "riff",
        ast::BarBeat::new(3, 1, 0),
        ast::BarBeat::new(4, 1, 0),
        None,
        Some(4.0),
    )
    .unwrap_err();

    assert!(error.contains("Invalid clip playback window"), "{error}");
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

// ── Clip placement and the performance timeline ──────────────────────────────
//
// A `performance:` track plays raw channel commands, so the validator refuses
// any note-derived content on it. Placing a clip there must not publish a
// document that `ocp validate` and `ocp render` both reject.

/// A `performance:` track beside two normal ones, with a pattern to place.
fn performance_clip_project() -> crate::state::AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
        r#"@project "Performance clip"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track perf | instrument: piano/acoustic_grand
  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 0; }

@track keys | instrument: piano/acoustic_grand

@track pads | instrument: piano/acoustic_grand
"#,
    )
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

/// The document the old drop published: a clip on a performance track.
fn clip_on_a_performance_track_project(notes: &str) -> crate::state::AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(&format!(
        r#"@project "Performance clip"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track perf | instrument: piano/acoustic_grand
  performance: {{ @0 ch0 note_on 60 90; @1 ch0 note_off 60 0; }}
{notes}  clip: riff [3:1 - 4:1]

@track keys | instrument: piano/acoustic_grand
"#
    ))
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

#[test]
fn adding_a_pattern_clip_to_a_performance_track_is_refused() {
    let mut inner = performance_clip_project();
    let before = inner.project.clone();
    let dirty = inner.dirty;
    let revision = inner.revision;

    let error = add_clip_inner(
        &mut inner,
        0,
        "riff",
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(2, 1, 0),
        None,
        None,
    )
    .unwrap_err();

    assert!(
        error.contains("performance commands cannot coexist with note-derived content"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.dirty, dirty);
    assert_eq!(inner.revision, revision);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn pasting_a_slipped_clip_onto_a_performance_track_is_refused() {
    let mut inner = performance_clip_project();
    let before = inner.project.clone();

    // A copy and a duplicate carry the source window, so this is the paste path.
    let error = add_clip_inner(
        &mut inner,
        0,
        "riff",
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(2, 1, 0),
        Some(0.5),
        Some(1.0),
    )
    .unwrap_err();

    assert!(
        error.contains("performance commands cannot coexist with note-derived content"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn moving_a_pattern_clip_onto_a_performance_track_is_refused() {
    let mut inner = performance_clip_project();
    add_clip_inner(
        &mut inner,
        1,
        "riff",
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(2, 1, 0),
        None,
        None,
    )
    .unwrap();
    let before = inner.project.clone();
    let undo = inner.undo.undo.len();

    let error =
        move_clip_to_track_inner(&mut inner, 1, 0, 0, ast::BarBeat::new(3, 1, 0)).unwrap_err();

    assert!(
        error.contains("performance commands cannot coexist with note-derived content"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), undo);
}

#[test]
fn moving_a_pattern_clip_between_normal_tracks_is_one_entry() {
    let mut inner = performance_clip_project();
    add_clip_inner(
        &mut inner,
        1,
        "riff",
        ast::BarBeat::new(1, 1, 0),
        ast::BarBeat::new(2, 1, 0),
        None,
        None,
    )
    .unwrap();
    let undo = inner.undo.undo.len();

    move_clip_to_track_inner(&mut inner, 1, 0, 2, ast::BarBeat::new(3, 1, 0)).unwrap();

    assert_eq!(inner.project.tracks[1].pattern_clip_count(), 0);
    assert_eq!(inner.project.tracks[2].pattern_clip_count(), 1);
    assert_eq!(inner.undo.undo.len(), undo + 1);
    assert_eq!(inner.undo.undo[undo].label, "clip move");
}

#[test]
fn moving_a_clip_off_a_performance_track_is_refused_while_it_keeps_notes() {
    // A phrase stays behind when the clip leaves, so the destination is still
    // refused: the move would publish a document the validator rejects.
    let mut inner =
        clip_on_a_performance_track_project("  @phrase held [1:1 - 2:1]\n    @0.0 C4 q mf\n");
    let before = inner.project.clone();

    let error =
        move_clip_to_track_inner(&mut inner, 0, 0, 1, ast::BarBeat::new(3, 1, 0)).unwrap_err();

    assert!(
        error.contains("performance commands cannot coexist with note-derived content"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn moving_the_last_clip_off_a_performance_track_repairs_the_document() {
    // The clip is the only note-derived content, so the move is valid and the
    // published file renders again.
    let mut inner = clip_on_a_performance_track_project("");

    move_clip_to_track_inner(&mut inner, 0, 0, 1, ast::BarBeat::new(3, 1, 0)).unwrap();

    assert_eq!(inner.project.tracks[0].pattern_clip_count(), 0);
    assert_eq!(inner.project.tracks[1].pattern_clip_count(), 1);
    assert!(oc_dsl::validate(&inner.project).is_ok());
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn moving_a_phrase_onto_a_performance_track_is_refused() {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(
        r#"@project "Performance phrase"
  tempo: 120
  time_signature: 4/4

@track perf | instrument: piano/acoustic_grand
  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 0; }

@track keys | instrument: piano/acoustic_grand
  @phrase run [1:1 - 2:1]
    @0.0 C4 q mf
"#,
    )
    .unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    let before = inner.project.clone();

    let error = move_phrase_inner(&mut inner, 1, 0, 0, ast::BarBeat::new(3, 1, 0)).unwrap_err();

    assert!(
        error.contains("performance commands cannot coexist with note-derived content"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo.len(), 0);

    // The same move inside a normal track is still published as one entry.
    move_phrase_inner(&mut inner, 1, 0, 1, ast::BarBeat::new(3, 1, 0)).unwrap();
    assert_eq!(inner.project.tracks[1].phrases.len(), 1);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn the_agent_edit_path_refuses_a_clip_on_a_performance_track() {
    use crate::commands::ai::{apply_edit_to_project, validate_edit_in_project};

    let mut inner = performance_clip_project();
    let track_dsl = oc_dsl::ops::extract(&inner.project, "track:perf")
        .expect("the performance track extracts")
        .dsl_text;
    let edit = format!("{track_dsl}  clip: riff [1:1 - 2:1]\n");

    let check = validate_edit_in_project(&inner, edit.clone(), "track:perf".into());
    assert!(!check.valid);
    assert!(
        check
            .errors
            .iter()
            .any(|error| error.contains("performance commands cannot coexist")),
        "{:?}",
        check.errors
    );

    let expected = inner.dsl_source.clone();
    let error = apply_edit_to_project(&mut inner, edit, "track:perf".into(), expected).unwrap_err();
    assert!(
        error.contains("performance commands cannot coexist"),
        "{error}"
    );
    assert_eq!(inner.project.tracks[0].pattern_clip_count(), 0);
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn a_looped_audio_clip_renders_the_repetition_sample_for_sample() {
    let (dir, mut inner) = audio_clip_slip_project();
    set_audio_clip_loop_inner(&mut inner, 0, Some(1.0)).unwrap();
    let mut buffer = vec![0.0_f32; 4 * 48_000 * 2];
    oc_engine::audio_clip::mix_into_track(
        &mut buffer,
        &inner.project,
        inner.project.audio_clip(0).unwrap(),
        48_000,
        Some(dir.path()),
    )
    .unwrap();
    // The ramp names its source frame: one loop later the same frame plays.
    let loop_frames = 48_000;
    assert!(buffer[10 * 2].abs() > 0.0, "{}", buffer[10 * 2]);
    for frame in [10usize, 24_000, 40_000] {
        assert_eq!(
            buffer[frame * 2],
            buffer[(frame + loop_frames) * 2],
            "frame {frame}"
        );
    }
}

#[test]
fn adding_a_plugin_track_is_one_undo_entry_and_one_rebuild() {
    let dir = crate::commands::tests::support::TempDir::new("add-instrument-track");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state =
        crate::commands::tests::support::state_with_project("@project\n  tempo: 120\n", dir.path());
    let mut inner = state.lock_inner();
    let before = inner.revision;
    let snapshot = add_instrument_track_inner(
        &mut inner,
        "Hosted Instrument".into(),
        "vst3".into(),
        Some("plugin-id".into()),
        None,
        "Hosted Instrument".into(),
        "keys".into(),
        "piano".into(),
    )
    .expect("the preset and its track are added");

    assert_eq!(snapshot.project.presets.len(), 1);
    assert_eq!(snapshot.project.presets[0].name, "Hosted Instrument");
    assert_eq!(
        snapshot.project.presets[0].plugin.as_deref(),
        Some("plugin-id")
    );
    assert_eq!(snapshot.project.tracks.len(), 1);
    assert_eq!(
        snapshot.project.tracks[0].preset.as_deref(),
        Some("Hosted Instrument")
    );
    // One document revision, so one graph rebuild and one undo step.
    assert!(inner.revision > before, "the edit publishes a revision");
    let entries = inner.undo.history();
    assert_eq!(
        entries.len(),
        1,
        "the preset and the track are one history entry"
    );
    assert_eq!(entries[0].name, "add instrument track");
}
