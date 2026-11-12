//! Effect-chain reordering: order, bounds, refusal, undo, cache invalidation,
//! and the DSL round trip.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast;
use oc_dsl::ast::EffectType;

use crate::commands::fx::{move_effect_inner, set_effect_param_inner};
use crate::commands::project::{replace_project, save_file_inner};
use crate::state::AppStateInner;

use super::support::{TempDir, state_with_project, test_state};

const CHAIN: &str = r#"@ocp "0.2.0"

@project "reorder"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
  effects:
    - gain: factor=0.5
    - reverb: mix=0.2
    - limiter

@bus space
  routes: [master]
  effects:
    - gain: factor=0.8
    - delay: time=0.25

@master
  effects:
    - gain: factor=0.9
    - limiter

@track piano | preset: lead, instrument: synth/lead_square
  effects:
    - gain: factor=0.5
    - reverb: mix=0.2
    - limiter
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;

fn state_with_chain() -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(CHAIN).unwrap();
    oc_dsl::validate(&inner.project).unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

fn types(effects: &[ast::Effect]) -> Vec<EffectType> {
    effects
        .iter()
        .map(|effect| effect.effect_type.clone())
        .collect()
}

fn track_types(project: &ast::Project) -> Vec<EffectType> {
    types(&project.tracks[0].effects)
}

fn bus_types(project: &ast::Project) -> Vec<EffectType> {
    types(&project.buses[0].effects)
}

fn master_types(project: &ast::Project) -> Vec<EffectType> {
    types(&project.master.as_ref().unwrap().effects)
}

fn preset_types(project: &ast::Project) -> Vec<EffectType> {
    types(&project.presets[0].effects)
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
fn moves_one_effect_and_undo_restores_the_order() {
    let mut inner = state_with_chain();
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Gain, EffectType::Reverb, EffectType::Limiter]
    );

    move_effect_inner(&mut inner, "track:0", 0, 2).unwrap();
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Reverb, EffectType::Limiter, EffectType::Gain],
        "move down moves the effect after its neighbours"
    );
    assert_eq!(inner.undo.undo.len(), 1, "one move, one undo entry");
    assert_eq!(inner.undo.undo[0].label, "move effect");

    move_effect_inner(&mut inner, "track:0", 2, 1).unwrap();
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Reverb, EffectType::Gain, EffectType::Limiter],
        "move up moves the effect before its neighbour"
    );

    inner.undo.undo(&mut inner.project).unwrap();
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Gain, EffectType::Reverb, EffectType::Limiter],
        "undo restores the chain order"
    );
}

#[test]
fn moves_effects_on_bus_master_and_preset_chains() {
    let mut inner = state_with_chain();

    move_effect_inner(&mut inner, "bus:space", 1, 0).unwrap();
    assert_eq!(
        bus_types(&inner.project),
        vec![EffectType::Delay, EffectType::Gain]
    );

    move_effect_inner(&mut inner, "master", 0, 1).unwrap();
    assert_eq!(
        master_types(&inner.project),
        vec![EffectType::Limiter, EffectType::Gain]
    );

    move_effect_inner(&mut inner, "preset:lead", 1, 0).unwrap();
    assert_eq!(
        preset_types(&inner.project),
        vec![EffectType::Reverb, EffectType::Gain, EffectType::Limiter]
    );

    assert_eq!(inner.undo.undo.len(), 3, "one entry per chain");
}

#[test]
fn refuses_moves_past_the_bounds_with_a_sentence() {
    let mut inner = state_with_chain();

    let error = move_effect_inner(&mut inner, "track:0", 0, 3).unwrap_err();
    assert_eq!(
        error,
        "Cannot move effect 0 to position 3: track:0 has 3 effect(s)"
    );
    let error = move_effect_inner(&mut inner, "track:0", 3, 0).unwrap_err();
    assert_eq!(
        error,
        "Effect 3 is out of range for track:0: the chain has 3 effect(s)"
    );
    assert!(inner.undo.undo.is_empty(), "a refused move adds no history");
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Gain, EffectType::Reverb, EffectType::Limiter]
    );
}

#[test]
fn moving_an_effect_to_its_own_index_is_a_no_op() {
    let mut inner = state_with_chain();

    move_effect_inner(&mut inner, "track:0", 1, 1).unwrap();
    assert!(inner.undo.undo.is_empty(), "a no-op adds no undo entry");
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Gain, EffectType::Reverb, EffectType::Limiter]
    );
}

#[test]
fn refuses_scopes_without_a_reorderable_chain() {
    let mut inner = state_with_chain();

    let error = move_effect_inner(&mut inner, "pattern:riff", 0, 1).unwrap_err();
    assert_eq!(
        error,
        "Effects can be reordered only in a track, bus, master, or preset chain; \
         'pattern:riff' is not one of them"
    );
    let error = move_effect_inner(&mut inner, "preset:missing", 0, 1).unwrap_err();
    assert_eq!(error, "Preset 'missing' not found");
    let error = move_effect_inner(&mut inner, "bus:missing", 0, 1).unwrap_err();
    assert_eq!(error, "Bus 'missing' not found");
    assert!(inner.undo.undo.is_empty());
}

/// An FX-chain edit reaches the live graph as a chain patch. Instruments are
/// not rebuilt, and the edit is not a structural change, so no project load is
/// queued and the cached render is dropped.
#[test]
fn a_chain_edit_patches_the_live_graph_instead_of_reloading_it() {
    use std::sync::atomic::Ordering;
    let mut inner = state_with_chain();
    let rate = inner.audio.audio_io.sample_rate();
    inner.audio.sample_rate = rate;
    inner.audio.render_cache = Some(render_cache(&inner.project));
    // The project is already loaded into the live thread.
    inner.audio.render_dirty = false;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(rate));
    let playback = std::sync::Arc::clone(&inner.audio.render_thread.as_ref().unwrap().playback);

    set_effect_param_inner(&mut inner, "track:0", 1, "mix", "60%").unwrap();

    assert_eq!(
        playback.chain_requests.load(Ordering::Acquire),
        1,
        "a track chain edit must hand the graph a chain document"
    );
    assert_eq!(
        playback.project_loads.load(Ordering::Acquire),
        0,
        "a track chain edit must not reload the project"
    );
    assert!(
        !inner.audio.render_dirty,
        "a parameter edit must not schedule a reload"
    );
    assert!(
        inner.audio.render_cache.as_ref().unwrap().is_dirty(),
        "a parameter edit must invalidate the rendered audio"
    );
}

#[test]
fn reorder_round_trips_through_the_dsl() {
    let mut inner = state_with_chain();
    move_effect_inner(&mut inner, "track:0", 2, 0).unwrap();

    // The saved text is what `resync_with_undo` serialised for the new order.
    let reparsed = oc_dsl::parse(&inner.dsl_source).unwrap();
    assert_eq!(
        track_types(&reparsed),
        vec![EffectType::Limiter, EffectType::Gain, EffectType::Reverb]
    );
    assert_eq!(
        bus_types(&reparsed),
        vec![EffectType::Gain, EffectType::Delay]
    );
    assert_eq!(
        preset_types(&reparsed),
        vec![EffectType::Gain, EffectType::Reverb, EffectType::Limiter]
    );
    assert_eq!(reparsed, inner.project, "the snapshot survives the trip");
}

#[test]
fn reorder_survives_a_save_and_reopen() {
    let dir = TempDir::new("movefx");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(CHAIN, dir.path());
    {
        let mut inner = state.lock_inner();
        move_effect_inner(&mut inner, "track:0", 2, 0).unwrap();
    }

    let saved_path = dir.path().join("reordered.ocp");
    save_file_inner(Some(saved_path.display().to_string()), &state).unwrap();
    let text = std::fs::read_to_string(&saved_path).unwrap();
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        track_types(&reopened),
        vec![EffectType::Limiter, EffectType::Gain, EffectType::Reverb],
        "the file keeps the moved order"
    );

    {
        let mut inner = state.lock_inner();
        replace_project(&mut inner, reopened, Some(saved_path)).unwrap();
    }
    let inner = state.lock_inner();
    assert_eq!(
        track_types(&inner.project),
        vec![EffectType::Limiter, EffectType::Gain, EffectType::Reverb],
        "the reopened project keeps the moved order"
    );
}

#[test]
fn reorder_invalidates_the_rendered_audio() {
    let mut inner = state_with_chain();
    inner.audio.render_cache = Some(render_cache(&inner.project));

    move_effect_inner(&mut inner, "track:0", 0, 1).unwrap();
    assert!(
        inner.audio.render_cache.as_ref().unwrap().is_dirty(),
        "a reorder must invalidate the rendered audio"
    );

    // A move to the same position must not throw the render away.
    let mut inner = state_with_chain();
    inner.audio.render_cache = Some(render_cache(&inner.project));
    move_effect_inner(&mut inner, "track:0", 1, 1).unwrap();
    assert!(
        !inner.audio.render_cache.as_ref().unwrap().is_dirty(),
        "a no-op move keeps the rendered audio"
    );
}
