//! Latency probe for one preset click. Ignored: run it with
//! `cargo test -p oc-gui --lib -- preset_load_probe --ignored --nocapture`.
//!
//! Shape A is the old one: the command body runs on the calling thread, which is
//! the webview thread. Shape B runs the same body on the blocking pool and
//! reports what the caller pays for the handoff. `OCP_PROBE_HEAVY=1` adds eleven
//! hosted-instrument presets, which is what makes a snapshot megabyte-sized.

use std::time::Instant;

use crate::commands::synth::set_preset_params_blocking;
use crate::state::AppState;

use super::support::*;

const DEFAULT_PROJECT: &str = "../../../../benchmarks/session-20tracks.ocp";

/// The whole-patch batch the editor sends: every modulation reset plus the patch.
fn factory_batch() -> Vec<(String, Option<String>)> {
    let mut batch: Vec<(String, Option<String>)> = vec![("fx.count".into(), Some("0".into()))];
    for macro_index in 1..=8 {
        batch.push((format!("macro.{macro_index}"), Some("0".into())));
    }
    for step in 1..=16 {
        batch.push((format!("step.{step}"), Some("0".into())));
    }
    for route in 1..=64 {
        batch.push((format!("mod.{route}.source"), Some("0".into())));
        batch.push((format!("mod.{route}.destination"), Some("0".into())));
        batch.push((format!("mod.{route}.amount"), Some("0".into())));
    }
    for (key, value) in [
        ("synth_version", "2"),
        ("osc.a.waveform", "4"),
        ("osc.a.wavetable_position", "0.58"),
        ("osc.a.level", "0.58"),
        ("osc.b.waveform", "0"),
        ("osc.b.level", "0.32"),
        ("osc.b.semitones", "12"),
        ("noise.level", "0.035"),
        ("unison.detune", "14"),
        ("filter.cutoff", "4200"),
        ("filter.resonance", "0.2"),
        ("filter.env.amount", "1.4"),
        ("amp.attack", "0.72"),
        ("amp.decay", "1.4"),
        ("amp.sustain", "0.78"),
        ("amp.release", "3.2"),
        ("voice.mono", "0"),
        ("lfo.sync", "1"),
        ("lfo.sync_div", "16"),
        ("mod.1.source", "0"),
        ("mod.1.destination", "4"),
        ("mod.1.amount", "0.2"),
    ] {
        batch.push((key.into(), Some(value.into())));
    }
    batch
}

/// The project a preset click runs against, plus hosted presets on demand.
fn with_hosted_presets(mut project: oc_dsl::ast::Project) -> oc_dsl::ast::Project {
    project.presets.push(oc_dsl::ast::PresetDef {
        name: "lead".into(),
        source: Some("builtin".into()),
        plugin: Some("builtin.open_synth".into()),
        program: None,
        patch: Some("Init".into()),
        file: None,
        extends: None,
        root_note: None,
        params: vec![],
        effects: vec![],
    });
    if std::env::var("OCP_PROBE_HEAVY").is_err() {
        return project;
    }
    for index in 0..11 {
        let name = format!("hosted_{index}");
        project.presets.push(oc_dsl::ast::PresetDef {
            name: name.clone(),
            source: Some("vst3".into()),
            plugin: Some("vst3:com.example.instrument".into()),
            program: None,
            patch: None,
            file: None,
            extends: None,
            root_note: None,
            params: (0..2623)
                .map(|param| {
                    (
                        format!("hosted.param{param}"),
                        oc_dsl::ast::ParamValue::Number(param as f64 / 2623.0),
                    )
                })
                .collect(),
            effects: vec![],
        });
        if let Some(track) = project.tracks.get_mut(index * 2) {
            track.preset = Some(name);
        }
    }
    project
}

#[test]
#[ignore = "latency probe: run with --ignored --nocapture"]
fn probe_preset_click_latency() {
    let path = std::env::var("OCP_PROBE_PROJECT").unwrap_or_else(|_| DEFAULT_PROJECT.into());
    let source = std::fs::read_to_string(&path).expect("probe project");
    let dir = TempDir::new("preset-load-probe");
    // Leaked so the blocking task can borrow it; the probe runs once.
    let state: &'static AppState = Box::leak(Box::new(state_with_project(&source, dir.path())));
    {
        let mut inner = state.lock_inner();
        inner.project = with_hosted_presets(inner.project.clone());
        inner.dsl_source = oc_dsl::serialize(&inner.project);
    }

    let batch = factory_batch();
    println!(
        "params_per_click={} tracks={}",
        batch.len(),
        state.lock_inner().project.tracks.len()
    );

    // Shape A: the body runs on this thread, as the old command did.
    for run in 1..=3 {
        let ticket = state.patch_load.begin();
        let start = Instant::now();
        let snapshot = set_preset_params_blocking(
            state,
            "lead".into(),
            batch.clone(),
            true,
            Vec::new(),
            Some("Glass Pad".to_string()),
            Some(ticket),
        )
        .expect("patch load");
        let body_ms = start.elapsed().as_secs_f64() * 1000.0;
        // A sync command also encodes the reply on this thread.
        let encode = Instant::now();
        let json_bytes = serde_json::to_vec(&snapshot).unwrap().len();
        let reply_ms = encode.elapsed().as_secs_f64() * 1000.0;
        println!(
            "before run {run}: ui_thread_blocked={body_ms:.1} reply_encode={reply_ms:.1} \
             ui_thread_total={:.1} reply_bytes={json_bytes}",
            body_ms + reply_ms
        );
    }

    // Shape B: the body runs on the blocking pool; this thread only waits.
    for run in 1..=3 {
        let ticket = state.patch_load.begin();
        let start = Instant::now();
        let batch = batch.clone();
        let handoff = tauri::async_runtime::spawn_blocking(move || {
            set_preset_params_blocking(
                state,
                "lead".into(),
                batch,
                true,
                Vec::new(),
                Some("Glass Pad".to_string()),
                Some(ticket),
            )
        });
        let enqueue_ms = start.elapsed().as_secs_f64() * 1000.0;
        let snapshot = tauri::async_runtime::block_on(handoff).unwrap().unwrap();
        let load_ms = start.elapsed().as_secs_f64() * 1000.0 - enqueue_ms;
        let encode = Instant::now();
        let json_bytes = serde_json::to_vec(&snapshot).unwrap().len();
        let reply_ms = encode.elapsed().as_secs_f64() * 1000.0;
        println!(
            "after  run {run}: ui_thread_enqueue={enqueue_ms:.3} load_off_thread={load_ms:.1} \
             reply_encode_off_thread={reply_ms:.1} reply_bytes={json_bytes}"
        );
    }
}
