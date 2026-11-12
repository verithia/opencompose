//! Render cache behaviour tests.

mod arrangement;
#[cfg(target_os = "macos")]
mod au;
mod builtin_failure;
mod clips;
mod drumkit;
mod mix;
mod mixer_routing;
mod mpe;
mod multiport;
mod native;
mod pattern_preset;
mod plugins;
mod preset_state;
mod range;
mod solo;
mod source_lanes;

use super::*;
use oc_dsl::ast::{ProjectProperties, TimeSignature};
use std::collections::HashMap;
use std::path::Path;

fn fixture_tail_cache() -> RenderCache {
    let mut cache = RenderCache::new();
    cache.set_vst3_tail_probe(|plan| {
        let id =
            plan.path
                .to_str()
                .ok_or(oc_host::vst3_instance::Vst3OwnedError::InvalidRequest(
                    "Invalid fixture path",
                ))?;
        if oc_host::vst3_instance::test_support::fake_plugin_bus_count(id).is_some() {
            // These routing fixtures declare no tail. Native preparation is
            // exercised by the actual-main host audit and CLI child tests.
            Ok(0)
        } else {
            super::vst3_tail::execute_probe(plan)
        }
    });
    cache
}

fn render_without_cache_bounds(project: &Project) -> Vec<f32> {
    fixture_tail_cache()
        .render_full(
            project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap()
}

#[test]
fn master_chain_latency_is_compensated_without_losing_the_program_end() {
    let source = r#"@ocp "0.2.0"
@project "master pdc"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.1, waveform: 1}
@track tone | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 2:1]
    rest q
    C4 q mf
    rest h
@master
  effects:
    - limiter: ceiling=-0.8dB
"#;
    let with_limiter = render_without_cache_bounds(&oc_dsl::parse(source).unwrap());
    let bypassed = render_without_cache_bounds(
        &oc_dsl::parse(&source.replace("@master\n  effects:\n    - limiter: ceiling=-0.8dB\n", ""))
            .unwrap(),
    );
    assert_eq!(with_limiter.len(), bypassed.len());
    // A below-ceiling saw is transparent once the lookahead is compensated.
    assert_eq!(with_limiter, bypassed);
    let onset = |samples: &[f32]| {
        samples
            .chunks_exact(2)
            .position(|frame| frame[0] != 0.0)
            .expect("note onset")
    };
    assert_eq!(onset(&with_limiter), 24_000);
    assert_eq!(onset(&bypassed), 24_000);
    println!(
        "master PDC click: onset {} frames, length {} frames (engaged) / {} frames (bypassed)",
        onset(&with_limiter),
        with_limiter.len() / 2,
        bypassed.len() / 2
    );
}

#[test]
fn pre_roll_accepts_only_finite_bounded_seconds() {
    let mut cache = RenderCache::new();
    assert!(cache.set_pre_roll_seconds(0.0).is_ok());
    assert!(cache.set_pre_roll_seconds(4.0).is_ok());
    assert!(cache.set_pre_roll_seconds(600.0).is_ok());
    assert!(cache.set_pre_roll_seconds(-0.1).is_err());
    assert!(cache.set_pre_roll_seconds(f64::NAN).is_err());
    assert!(cache.set_pre_roll_seconds(600.1).is_err());
}

#[derive(Debug, PartialEq)]
enum QueuedEvent {
    NoteOn(u8, u8, u8, i32),
    NoteOff(u8, u8, i32),
    ControlChange(u8, u8, u8, i32),
    ProgramChange(u8, u8, i32),
    PitchBend(u8, i16, i32),
    ChannelPressure(u8, u8, i32),
    PolyPressure(u8, u8, u8, i32),
    NoteExpression(u8, u8, String, f64, i32),
    PluginParam(String, f64, i32),
}

#[derive(Default)]
struct TestSink(Vec<QueuedEvent>);

fn empty_project() -> Project {
    Project {
        name: Some("Test".to_string()),
        ocp_version: Some("0.2.0".to_string()),
        properties: ProjectProperties::default(),
        tracks: vec![],
        patterns: vec![],
        presets: vec![],
        buses: vec![],
        master: None,
        sections: vec![],
        markers: Vec::new(),
        groups: Vec::new(),
    }
}

fn shared_plugin_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "shared outputs"
  tempo: 120
  time_signature: 4/4

@preset shared
  source: clap
  plugin: "org.example.multi"
  params: {plugin_output_pair: 1}

@track close | preset: shared, instrument: drums/acoustic_kit
  engine_params: {plugin_output_pair: 1}

@track room | preset: shared, instrument: drums/acoustic_kit
  engine_params: {plugin_output_pair: 3}
"#,
    )
    .expect("shared plugin project should parse")
}
