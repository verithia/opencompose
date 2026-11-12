//! Effect mutations on a preset chain: `preset:name` resolves through the same
//! chain as a track, bus, or master, and every preset edit undoes, resyncs and
//! invalidates the rendered audio.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast;
use oc_dsl::ast::{EffectType, ParamValue};

use crate::commands::fx::{
    add_effect_inner, remove_effect_inner, set_effect_bypass_inner, set_effect_param_inner,
    set_effect_params_inner,
};
use crate::commands::project::{replace_project, save_file_inner};
use crate::state::AppStateInner;

use super::support::{TempDir, state_with_project, test_state};

/// A preset chain that ends in a plugin effect, so the chain covers both a
/// built-in and a hosted entry.
const PRESET_CHAIN: &str = r#"@ocp "0.2.0"

@project "preset chain"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
  effects:
    - gain: factor=0.5
    - reverb: mix=0.2
    - plugin:builtin.test_tone

@track piano | preset: lead, instrument: synth/lead_square
  effects:
    - gain: factor=0.5
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;

fn state_with_preset_chain() -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(PRESET_CHAIN).unwrap();
    oc_dsl::validate(&inner.project).unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

fn names(effects: &[ast::Effect]) -> Vec<String> {
    effects
        .iter()
        .map(|effect| match &effect.effect_type {
            EffectType::Plugin(id) => format!("plugin:{id}"),
            other => other.lane_name().to_string(),
        })
        .collect()
}

fn preset_names(project: &ast::Project) -> Vec<String> {
    names(&project.presets[0].effects)
}

fn render_cache(project: &ast::Project) -> oc_engine::render_cache::RenderCache {
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache
        .render_full(
            project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(!cache.is_dirty(), "the fixture must render clean");
    cache
}

#[test]
fn adds_an_effect_to_a_preset_chain_and_undo_restores_it() {
    let mut inner = state_with_preset_chain();

    add_effect_inner(&mut inner, "preset:lead", "delay").unwrap();
    assert_eq!(
        preset_names(&inner.project),
        ["gain", "reverb", "plugin:builtin.test_tone", "delay"],
        "the effect lands at the end of the preset chain"
    );
    assert_eq!(inner.undo.undo.len(), 1, "one add, one undo entry");
    assert_eq!(inner.undo.undo[0].label, "add effect");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        preset_names(&inner.project),
        ["gain", "reverb", "plugin:builtin.test_tone"],
        "undo removes the added effect"
    );
}

#[test]
fn removes_an_effect_from_a_preset_chain_and_undo_restores_it() {
    let mut inner = state_with_preset_chain();

    remove_effect_inner(&mut inner, "preset:lead", 0).unwrap();
    assert_eq!(
        preset_names(&inner.project),
        ["reverb", "plugin:builtin.test_tone"]
    );
    assert_eq!(inner.undo.undo[0].label, "remove effect");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        preset_names(&inner.project),
        ["gain", "reverb", "plugin:builtin.test_tone"]
    );
}

#[test]
fn sets_a_parameter_on_a_preset_effect_and_undo_restores_it() {
    let mut inner = state_with_preset_chain();

    set_effect_param_inner(&mut inner, "preset:lead", 1, "mix", "50%").unwrap();
    assert_eq!(
        inner.project.presets[0].effects[1].params,
        vec![("mix".to_string(), ParamValue::Percentage(50.0))]
    );
    assert_eq!(inner.undo.undo[0].label, "effect parameter");

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        inner.project.presets[0].effects[1].params,
        vec![("mix".to_string(), ParamValue::Number(0.2))]
    );

    // The batch form lands as one undo entry for the whole gesture.
    set_effect_params_inner(
        &mut inner,
        "preset:lead",
        1,
        &[
            ("mix".to_string(), "25%".to_string()),
            ("decay".to_string(), "2s".to_string()),
        ],
    )
    .unwrap();
    assert_eq!(
        inner.project.presets[0].effects[1].params,
        vec![
            ("mix".to_string(), ParamValue::Percentage(25.0)),
            ("decay".to_string(), ParamValue::Time(2.0)),
        ]
    );
    assert_eq!(inner.undo.undo.len(), 1, "one gesture, one undo entry");
    assert_eq!(inner.undo.undo[0].label, "effect parameters");
}

#[test]
fn toggles_bypass_on_a_preset_effect_and_undo_restores_it() {
    let mut inner = state_with_preset_chain();
    let bypassed = |inner: &AppStateInner| inner.project.presets[0].effects[2].bypass;

    assert!(!bypassed(&inner));

    set_effect_bypass_inner(&mut inner, "preset:lead", 2, true).unwrap();
    assert!(bypassed(&inner));
    assert_eq!(inner.undo.undo[0].label, "bypass effect");
    assert!(
        inner.dsl_source.contains("bypass=true"),
        "the field reaches the DSL text:\n{}",
        inner.dsl_source
    );
    assert!(
        inner.project.presets[0].effects[2]
            .params
            .iter()
            .all(|(name, _)| name != "bypass"),
        "the field never becomes a parameter"
    );

    inner.undo.undo(&mut inner.project).unwrap();
    assert!(!bypassed(&inner));
}

#[test]
fn a_preset_mutation_invalidates_the_rendered_audio() {
    let mut inner = state_with_preset_chain();
    // The renderer refuses an external effect it cannot load, so drop the
    // plugin before asking for a clean render.
    inner.project.presets[0].effects.pop();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner.audio.render_cache = Some(render_cache(&inner.project));

    add_effect_inner(&mut inner, "preset:lead", "delay").unwrap();
    assert!(
        inner.audio.render_cache.as_ref().unwrap().is_dirty(),
        "an added preset effect must invalidate the rendered audio"
    );
}

#[test]
fn refuses_a_missing_preset_without_an_edit() {
    let mut inner = state_with_preset_chain();
    let before = inner.project.clone();

    for error in [
        add_effect_inner(&mut inner, "preset:missing", "delay").unwrap_err(),
        remove_effect_inner(&mut inner, "preset:missing", 0).unwrap_err(),
        set_effect_param_inner(&mut inner, "preset:missing", 0, "mix", "50%").unwrap_err(),
        set_effect_param_inner(&mut inner, "preset:missing", 0, "bypass", "true").unwrap_err(),
    ] {
        assert_eq!(error, "Preset 'missing' not found");
    }

    assert!(inner.undo.undo.is_empty(), "a refusal adds no undo entry");
    assert_eq!(inner.project, before, "a refusal leaves the project alone");
}

#[test]
fn refuses_an_index_past_the_chain_without_an_edit() {
    let mut inner = state_with_preset_chain();
    let before = inner.project.clone();

    for error in [
        remove_effect_inner(&mut inner, "preset:lead", 3).unwrap_err(),
        set_effect_param_inner(&mut inner, "preset:lead", 3, "mix", "50%").unwrap_err(),
        set_effect_params_inner(
            &mut inner,
            "preset:lead",
            3,
            &[("mix".to_string(), "50%".to_string())],
        )
        .unwrap_err(),
    ] {
        assert_eq!(error, "Effect index 3 out of range");
    }
    assert_eq!(
        remove_effect_inner(&mut inner, "track:0", 1).unwrap_err(),
        "Effect index 1 out of range",
        "a track target refuses the same way"
    );

    assert!(inner.undo.undo.is_empty(), "a refusal adds no undo entry");
    assert_eq!(inner.project, before, "a refusal leaves the project alone");
}

#[test]
fn refuses_scopes_without_a_mutable_chain() {
    let mut inner = state_with_preset_chain();
    let before = inner.project.clone();
    let sentence = "Effects can be reordered only in a track, bus, master, or preset chain; \
                    'pattern:riff' is not one of them";

    assert_eq!(
        add_effect_inner(&mut inner, "pattern:riff", "delay").unwrap_err(),
        sentence
    );
    assert_eq!(
        remove_effect_inner(&mut inner, "pattern:riff", 0).unwrap_err(),
        sentence
    );
    assert_eq!(
        set_effect_param_inner(&mut inner, "pattern:riff", 0, "mix", "50%").unwrap_err(),
        sentence
    );

    assert!(inner.undo.undo.is_empty());
    assert_eq!(inner.project, before);
}

#[test]
fn adds_an_effect_to_a_project_without_a_master_section() {
    let mut inner = test_state();
    assert!(inner.project.master.is_none());

    add_effect_inner(&mut inner, "master", "limiter").unwrap();
    assert_eq!(
        names(&inner.project.master.as_ref().unwrap().effects),
        ["limiter"],
        "the first master effect creates the master chain"
    );

    let mut inner = test_state();
    assert!(add_effect_inner(&mut inner, "pattern:riff", "delay").is_err());
    assert!(
        inner.project.master.is_none(),
        "a refused scope does not create a master chain"
    );
}

#[test]
fn a_preset_chain_round_trips_through_save_and_reopen() {
    let dir = TempDir::new("presetfx");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(PRESET_CHAIN, dir.path());
    {
        let mut inner = state.lock_inner();
        add_effect_inner(&mut inner, "preset:lead", "delay").unwrap();
        remove_effect_inner(&mut inner, "preset:lead", 0).unwrap();
    }

    let saved_path = dir.path().join("preset.ocp");
    save_file_inner(Some(saved_path.display().to_string()), &state).unwrap();
    let text = std::fs::read_to_string(&saved_path).unwrap();
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        preset_names(&reopened),
        ["reverb", "plugin:builtin.test_tone", "delay"],
        "the file keeps the added and removed effects"
    );

    {
        let mut inner = state.lock_inner();
        replace_project(&mut inner, reopened, Some(saved_path)).unwrap();
    }
    let inner = state.lock_inner();
    assert_eq!(
        preset_names(&inner.project),
        ["reverb", "plugin:builtin.test_tone", "delay"],
        "the reopened project keeps the preset chain"
    );
}
