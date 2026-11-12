//! End-to-end LV2 host tests against an in-tree fixture bundle.

use std::path::{Path, PathBuf};
use std::process::Command;

use oc_host::PluginFormat;
use oc_host::lv2::{Lv2Instance, metadata};

const GAIN_URI: &str = "http://opencompose.local/lv2/fixture/gain";
const TONE_URI: &str = "http://opencompose.local/lv2/fixture/tone";
const MONO_URI: &str = "http://opencompose.local/lv2/fixture/mono";
const LATENCY_URI: &str = "http://opencompose.local/lv2/fixture/latency";
const WIDE_URI: &str = "http://opencompose.local/lv2/fixture/wide";

struct FixtureBundle {
    root: PathBuf,
}

impl FixtureBundle {
    fn new(name: &str, manifest: &str) -> Self {
        Self::from_library(name, manifest, &fixture_library())
    }

    fn from_library(name: &str, manifest: &str, dylib: &Path) -> Self {
        let root = oc_audio::private_fs::test_root().join(format!(
            "ocp-lv2-bundle-{}-{}-{name}",
            std::process::id(),
            unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let binary_name = dylib.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::copy(dylib, root.join(&binary_name)).unwrap();
        std::fs::write(
            root.join("manifest.ttl"),
            manifest.replace("{{BINARY}}", &binary_name),
        )
        .unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for FixtureBundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn load_error(bundle: &Path, uri: &str) -> String {
    match Lv2Instance::load(bundle, uri, 48_000, 128) {
        Ok(_) => panic!("expected LV2 load to fail"),
        Err(error) => error,
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn fixture_library() -> PathBuf {
    if let Some(path) = std::env::var_os("OCP_LV2_FIXTURE") {
        return PathBuf::from(path);
    }
    oc_host::test_support::lv2_module_fixture()
        .expect("native LV2 fixture built for this host target")
        .to_path_buf()
}

fn manifest(plugin_declaration: &str) -> String {
    format!(
        r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix units: <http://lv2plug.in/ns/ext/units#> .
@prefix pg: <http://lv2plug.in/ns/ext/port-groups#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
{plugin_declaration}
"#
    )
}

const GAIN_PORTS: &str = r#"
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ], [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 3 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 4 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:minimum 0.0 ; lv2:maximum 4.0 ; lv2:default 1.0
    ]"#;

const TONE_PORTS: &str = r#"
    lv2:port [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 0 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ], [
        a lv2:AtomPort, lv2:InputPort ; lv2:index 2 ; lv2:symbol "midi_in" ; lv2:name "MIDI In" ;
        atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent
    ]"#;

fn gain_bundle(name: &str) -> FixtureBundle {
    FixtureBundle::new(
        name,
        &manifest(&format!(
            r#"
<{GAIN_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Gain" ;
    doap:maintainer [ foaf:name "OpenCompose Tests" ] ;
    {GAIN_PORTS} .
"#
        ))
        .replace(
            "@prefix midi:",
            "@prefix foaf: <http://xmlns.com/foaf/0.1/> .\n@prefix midi:",
        ),
    )
}

fn tone_bundle(name: &str) -> FixtureBundle {
    FixtureBundle::new(
        name,
        &manifest(&format!(
            r#"
<{TONE_URI}> a lv2:Plugin, lv2:InstrumentPlugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Tone" ;
    {TONE_PORTS} .
"#
        )),
    )
}

#[test]
fn discovers_a_bundle_with_two_plugins() {
    let bundle = FixtureBundle::new(
        "two",
        &manifest(&format!(
            r#"
<{GAIN_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Gain" ;
    {GAIN_PORTS} .
<{TONE_URI}> a lv2:Plugin, lv2:InstrumentPlugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Tone" ;
    {TONE_PORTS} .
"#
        )),
    );
    let plugins = metadata::scan_bundle(bundle.path()).unwrap();
    assert_eq!(plugins.len(), 2);
    let gain = plugins
        .iter()
        .find(|plugin| plugin.uri == GAIN_URI)
        .unwrap();
    assert_eq!(gain.name, "Fixture Gain");
    assert_eq!(gain.audio_inputs(), 2);
    assert_eq!(gain.audio_outputs(), 2);
    assert!(!gain.is_instrument);
    let info = gain.info();
    assert_eq!(info.format, PluginFormat::Lv2);
    assert_eq!(info.path, bundle.path().to_string_lossy());
    let tone = plugins
        .iter()
        .find(|plugin| plugin.uri == TONE_URI)
        .unwrap();
    assert!(tone.is_instrument);
    assert_eq!(tone.audio_inputs(), 0);
    assert!(tone.midi_input_port().is_some());
    assert_eq!(tone.info().category, oc_host::PluginCategory::Instrument);
}

#[test]
fn gain_effect_produces_exact_samples_and_exposes_one_parameter() {
    let bundle = gain_bundle("gain");
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    let params = instance.params();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].symbol, "gain");
    assert_eq!(params[0].min, 0.0);
    assert_eq!(params[0].max, 4.0);
    assert_eq!(params[0].default, 1.0);
    assert!(instance.set_param("Gain", 0.5));
    assert_eq!(instance.get_param("gain"), Some(0.5));

    let mut buffer = vec![0.25, -0.5, 1.0, -1.0];
    instance.process_interleaved(&mut buffer, 48_000);
    assert_eq!(buffer, vec![0.125, -0.25, 0.5, -0.5]);
}

#[test]
fn mono_effect_is_duplicated_to_both_output_channels() {
    let bundle = FixtureBundle::new(
        "mono",
        &manifest(&format!(
            r#"
<{MONO_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Mono" ;
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in" ; lv2:name "In"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out" ; lv2:name "Out"
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 2 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:minimum 0.0 ; lv2:maximum 4.0 ; lv2:default 1.0
    ] .
"#
        )),
    );
    let mut instance = Lv2Instance::load(bundle.path(), MONO_URI, 48_000, 128).unwrap();
    instance.set_param("gain", 2.0);
    let mut buffer = vec![0.25, 0.0, -0.5, 0.0];
    instance.process_interleaved(&mut buffer, 48_000);
    assert_eq!(buffer, vec![0.5, 0.5, -1.0, -1.0]);
}

#[test]
fn instrument_receives_midi_at_a_block_offset() {
    let bundle = tone_bundle("tone");
    let mut instance = Lv2Instance::load(bundle.path(), TONE_URI, 48_000, 128).unwrap();
    assert!(instance.is_instrument());
    instance.note_on_channel(0, 69, 127.0, 32);
    let output = instance.process_block_with_transport(64, None).to_vec();
    assert_eq!(output.len(), 128);
    for frame in 0..32 {
        assert_eq!(output[frame * 2], 0.0, "frame {frame} must precede note-on");
        assert_eq!(output[frame * 2 + 1], 0.0);
    }
    let step = std::f64::consts::TAU * 440.0 / 48_000.0;
    for frame in 32..64 {
        let expected = ((frame - 32) as f64 * step).sin() * 0.25;
        assert!(
            (f64::from(output[frame * 2]) - expected).abs() < 1e-6,
            "frame {frame}: {} != {expected}",
            output[frame * 2]
        );
        assert_eq!(output[frame * 2], output[frame * 2 + 1]);
    }
    instance.note_off_channel(0, 69, 0);
    let silence = instance.process_block_with_transport(64, None).to_vec();
    assert!(silence.iter().all(|sample| *sample == 0.0));
}

#[test]
fn state_round_trip_restores_the_control_value() {
    let bundle = gain_bundle("state");
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    assert!(instance.set_param("gain", 0.25));
    let state = instance.state_bytes().unwrap();
    assert!(state.starts_with(b"OCLV2ST1"));

    let mut restored = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    restored.set_state_bytes(&state).unwrap();
    let mut buffer = vec![1.0, 1.0];
    restored.process_interleaved(&mut buffer, 48_000);
    assert_eq!(buffer, vec![0.25, 0.25]);
}

#[test]
fn latency_designation_is_reported_after_activation() {
    let bundle = FixtureBundle::new(
        "latency",
        &manifest(&format!(
            r#"
<{LATENCY_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Latency" ;
    {GAIN_PORTS} , [
        a lv2:ControlPort, lv2:OutputPort ; lv2:index 5 ; lv2:symbol "latency" ;
        lv2:name "Latency" ; lv2:designation lv2:latency
    ] .
"#
        )),
    );
    let mut instance = Lv2Instance::load(bundle.path(), LATENCY_URI, 48_000, 128).unwrap();
    assert_eq!(instance.latency_samples(), 0);
    let mut buffer = vec![0.0, 0.0];
    instance.process_interleaved(&mut buffer, 48_000);
    assert_eq!(instance.latency_samples(), 64);
}

#[test]
fn unsupported_required_feature_is_rejected_before_instantiation() {
    let bundle = FixtureBundle::new(
        "worker",
        &manifest(&format!(
            r#"
<{GAIN_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Worker" ;
    lv2:requiredFeature <http://lv2plug.in/ns/ext/worker#schedule> ;
    {GAIN_PORTS} .
"#
        )),
    );
    let error = load_error(bundle.path(), GAIN_URI);
    assert!(error.contains("worker#schedule"), "{error}");
    assert!(error.contains("unsupported feature"), "{error}");
}

#[test]
fn more_than_two_audio_outputs_is_rejected_cleanly() {
    let bundle = FixtureBundle::new(
        "wide",
        &manifest(&format!(
            r#"
<{WIDE_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Wide" ;
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l"
    ], [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 1 ; lv2:symbol "in_r"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out1"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 3 ; lv2:symbol "out2"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 4 ; lv2:symbol "out3"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 5 ; lv2:symbol "out4"
    ] .
"#
        )),
    );
    let error = load_error(bundle.path(), WIDE_URI);
    assert!(error.contains("at most stereo"), "{error}");
}

#[test]
fn malformed_manifest_is_a_clean_error_without_a_panic() {
    let bundle = FixtureBundle::new("broken", "<urn:plugin> a lv2:Plugin");
    let error = metadata::scan_bundle(bundle.path()).unwrap_err();
    assert!(error.contains("malformed LV2 manifest"), "{error}");
    let error = load_error(bundle.path(), GAIN_URI);
    assert!(error.contains("malformed LV2 manifest"), "{error}");
}

#[test]
fn discovery_walks_a_bundle_root() {
    let bundle = gain_bundle("discover");
    let root = bundle.path().parent().unwrap().to_path_buf();
    let bundles = metadata::discover_bundles_in(std::slice::from_ref(&root));
    assert!(bundles.iter().any(|candidate| candidate == bundle.path()));
    let found = metadata::find_in_paths(GAIN_URI, &[root]).unwrap();
    assert_eq!(found.uri, GAIN_URI);
    let missing = metadata::find_in_paths(
        "urn:no-such-plugin",
        &[bundle.path().parent().unwrap().to_path_buf()],
    );
    assert!(missing.is_none());
}

#[test]
fn atom_buffer_stays_writable_for_an_empty_midi_port() {
    // The host must present a valid empty sequence even when no event is queued.
    let bundle = tone_bundle("empty");
    let mut instance = Lv2Instance::load(bundle.path(), TONE_URI, 48_000, 128).unwrap();
    let output = instance.process_block_with_transport(16, None).to_vec();
    assert_eq!(output.len(), 32);
    assert!(output.iter().all(|sample| *sample == 0.0));
    assert!(instance.output_midi_events().is_empty());
}

/// A stereo gain with 21 control inputs: one plain gain, one switch, one integer,
/// one enumeration, one `lv2:enabled` bypass, one custom unit, one host-managed
/// sample-rate port (which must not reach the panel) and 13 plain `units:hz` knobs.
fn panel_ports() -> String {
    let mut ports = String::from(
        r#"
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ], [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 3 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ], [
        a lv2:ControlPort, lv2:OutputPort ; lv2:index 5 ; lv2:symbol "latency" ;
        lv2:name "Latency" ; lv2:designation lv2:latency
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 4 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:minimum 0.0 ; lv2:maximum 1.0 ; lv2:default 0.5 ;
        units:unit units:db ;
        pg:group [ a pg:Group ; lv2:name "Output" ]
    ]"#,
    );
    for step in 0..20 {
        let index = 6 + step;
        let group = if step % 2 == 0 { "Filter" } else { "Output" };
        let (minimum, maximum, default) = match step {
            1 => (0.0, 15.0, 4.0),
            _ => (0.0, 10.0, step as f32 / 2.0),
        };
        let mut extra = String::new();
        match step {
            0 => extra.push_str("lv2:portProperty lv2:toggled ; "),
            1 => extra.push_str("lv2:portProperty lv2:integer ; "),
            2 => extra.push_str(
                r#"lv2:portProperty lv2:enumeration ;
        lv2:scalePoint [ rdfs:label "Low" ; rdf:value 0 ],
            [ rdfs:label "Mid" ; rdf:value 1 ],
            [ rdfs:label "High" ; rdf:value 2 ] ; "#,
            ),
            3 => extra.push_str("lv2:designation lv2:enabled ; "),
            5 => extra.push_str("lv2:designation lv2:sampleRate ; "),
            _ => {}
        }
        let unit = match step {
            4 => "units:unit [ a units:Unit ; units:symbol \"%\" ]".to_string(),
            0..=3 => "units:unit units:db".to_string(),
            _ => "units:unit units:hz".to_string(),
        };
        ports.push_str(&format!(
            r#", [
        a lv2:ControlPort, lv2:InputPort ; lv2:index {index} ;
        lv2:symbol "knob_{step}" ; lv2:name "Knob {step}" ;
        lv2:minimum {minimum} ; lv2:maximum {maximum} ; lv2:default {default} ;
        {extra}{unit} ;
        pg:group [ a pg:Group ; lv2:name "{group}" ]
    ]"#
        ));
    }
    ports
}

#[test]
fn a_twenty_parameter_bundle_reports_a_panel_surface() {
    let bundle = FixtureBundle::new(
        "panel",
        &manifest(&format!(
            r#"
<{GAIN_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Panel" ;
    {} .
"#,
            panel_ports()
        )),
    );
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    let surface = instance.parameter_surface();
    let params = surface.parameters();
    assert_eq!(params.len(), 20, "{params:#?}");

    let gain = &params[0];
    assert_eq!(gain.name, "Gain");
    assert_eq!(gain.index, 4);
    assert_eq!(gain.unit, "dB");
    assert_eq!(gain.group.as_deref(), Some("Output"));
    assert_eq!(gain.default, Some(0.5));
    assert!((gain.current - 0.5).abs() < 1e-6);
    assert!((gain.normalized - 0.5).abs() < 1e-6);
    assert!(gain.automatable);
    assert!(!gain.by_passing);

    let switch = &params[1];
    assert_eq!(switch.name, "Knob 0");
    assert_eq!(switch.steps, Some(2));
    assert_eq!(switch.group.as_deref(), Some("Filter"));

    let integer = &params[2];
    assert_eq!(integer.steps, Some(16));
    assert_eq!(integer.min, 0.0);
    assert_eq!(integer.max, 15.0);
    assert!((integer.normalized - 4.0 / 15.0).abs() < 1e-6);

    assert_eq!(params[3].steps, Some(3), "three scale points");
    let custom_unit = params
        .iter()
        .find(|param| param.name == "Knob 4")
        .expect("custom unit port");
    assert_eq!(custom_unit.unit, "%", "inline unit symbol");
    assert_eq!(custom_unit.steps, None);
    let standard_unit = params
        .iter()
        .find(|param| param.name == "Knob 6")
        .expect("standard unit port");
    assert_eq!(standard_unit.unit, "Hz", "standard unit import");

    // The host-managed sample-rate port never reaches the panel.
    assert!(
        !params.iter().any(|param| param.name == "Knob 5"),
        "{:?}",
        params.iter().map(|param| &param.name).collect::<Vec<_>>()
    );

    // `lv2:enabled` is the bundle's own bypass switch.
    let enabled = params
        .iter()
        .position(|param| param.name == "Knob 3")
        .expect("enabled port is a parameter");
    assert!(!params[enabled].by_passing);
    assert!(instance.set_param_by_name("Knob 3", 0.0));
    let bypassed = instance.parameter_surface();
    assert!(bypassed.parameters()[enabled].by_passing);
}

#[test]
fn a_bundle_with_no_control_ports_is_not_an_empty_panel() {
    let bundle = FixtureBundle::new(
        "no-controls",
        &manifest(&format!(
            r#"
<{GAIN_URI}> a lv2:Plugin ;
    lv2:binary <{{{{BINARY}}}}> ;
    doap:name "Fixture Silent" ;
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] .
"#
        )),
    );
    let instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    assert_eq!(
        instance.parameter_surface(),
        oc_host::params::ParamSurface::Unavailable {
            issue: oc_host::params::ParamSurfaceIssue::PluginReportsNoParameters
        }
    );
}

#[path = "lv2/authored_parameters.rs"]
mod authored_parameters;

#[test]
fn atom_ports_have_native_alignment_capacity_and_complete_midi_roundtrip_without_heap_work() {
    const URI: &str = "http://opencompose.local/lv2/fixture/atom-echo";
    let bundle = FixtureBundle::new(
        "atom-écho-声-🎹",
        &manifest(&format!(
            r#"
<{URI}> a lv2:Plugin, lv2:InstrumentPlugin ; doap:name "Atom Contract" ; lv2:binary <{{{{BINARY}}}}> ;
    lv2:port [ a lv2:AudioPort, lv2:OutputPort ; lv2:index 0 ; lv2:symbol "left" ; lv2:name "Left" ],
    [ a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "right" ; lv2:name "Right" ],
    [ a lv2:AtomPort, lv2:InputPort ; lv2:index 2 ; lv2:symbol "midi_in" ; lv2:name "MIDI In" ; atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent ],
    [ a lv2:AtomPort, lv2:OutputPort ; lv2:index 3 ; lv2:symbol "midi_out" ; lv2:name "MIDI Out" ; atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent ],
    [ a lv2:ControlPort, lv2:InputPort ; lv2:index 4 ; lv2:symbol "mode" ; lv2:name "Mode" ; lv2:minimum 0 ; lv2:maximum 3 ; lv2:default 0 ] .
"#
        )),
    );
    let mut instance = Lv2Instance::load(bundle.path(), URI, 48_000, 128).unwrap();
    for frames in [128, 17, 128] {
        let operations = oc_test_support::operations(|| {
            instance.note_on_channel(0, 60, 100.0 / 127.0, 0);
            instance.note_off_channel(0, 60, frames - 1);
            let output = instance.process_block_with_transport(frames, None);
            assert!(output.iter().all(|sample| *sample == 1.0));
        });
        assert_eq!(operations, 0, "first/warm native block {frames}");
        let events = instance.output_midi_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, 0);
        assert_eq!(events[1].0, frames - 1);
        assert_eq!(events[0].1, vec![0x90, 60, 100]);
        assert_eq!(events[1].1[0] & 0xF0, 0x80);
        assert_eq!(events[1].1[1], 60);
    }
    for mode in [1, 2, 3, 0] {
        assert!(instance.set_param("mode", f64::from(mode)));
        assert_eq!(
            oc_test_support::operations(|| {
                assert!(
                    instance
                        .process_block_with_transport(17, None)
                        .iter()
                        .all(|sample| *sample == (mode + 1) as f32)
                );
            }),
            0
        );
        let events = instance.output_midi_events();
        if mode == 1 {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].0, 16);
            assert_eq!(events[0].1.len(), 8160);
            assert_eq!(events[0].1[0], 0xF0);
            assert_eq!(events[0].1[8159], 0xF7);
            assert!(events[0].1[1..8159].iter().all(|byte| *byte == 0x55));
        } else {
            assert!(events.is_empty(), "malformed or empty mode {mode}");
        }
    }
}

#[test]
fn fixture_bundles_use_managed_root_and_drop_preserves_shared_or_external_library() {
    let root = oc_audio::private_fs::test_root();
    let library = fixture_library();
    if let Some(external) = std::env::var_os("OCP_LV2_FIXTURE") {
        assert_eq!(library, PathBuf::from(external));
    } else {
        assert_eq!(
            library,
            oc_host::test_support::lv2_module_fixture()
                .expect("native LV2 fixture built for this host target")
        );
    }
    let source_bytes = std::fs::read(&library).unwrap();
    let path = {
        let bundle = gain_bundle("managed-lifetime");
        assert!(bundle.path().starts_with(&root));
        let bundled_library = bundle.path().join(library.file_name().unwrap());
        assert_ne!(
            bundled_library, library,
            "bundle owns a separate library copy"
        );
        assert_eq!(std::fs::read(bundled_library).unwrap(), source_bytes);
        assert!(bundle.path().join("manifest.ttl").is_file());
        bundle.path().to_path_buf()
    };
    assert!(
        !path.exists(),
        "an unloaded bundle is removed by its local owner"
    );
    assert!(
        library.is_file(),
        "the shared or external source library remains untouched"
    );
    assert_eq!(std::fs::read(&library).unwrap(), source_bytes);
}

#[test]
fn fixture_constructor_failure_is_removed_at_process_exit() {
    const CHILD: &str = "OCP_LV2_CONSTRUCTOR_CLEANUP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = oc_audio::private_fs::test_root();
        let missing = root.join("missing-library.so");
        let result = std::panic::catch_unwind(|| {
            FixtureBundle::from_library("constructor-failure", "unused", &missing)
        });
        assert!(
            result.is_err(),
            "the actual constructor copy fails before Self exists"
        );
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_some(),
            "failed construction leaves scratch for the managed exit owner"
        );
        return;
    }
    let parent =
        oc_audio::private_fs::test_root().join(format!("lv2-child-exit-{}", unique_suffix()));
    std::fs::create_dir(&parent).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "plugins::lv2::fixture_constructor_failure_is_removed_at_process_exit",
        ])
        .env(CHILD, "1")
        .env("TMPDIR", &parent)
        .env("TEMP", &parent)
        .env("TMP", &parent)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "cleanup child failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "the intended child test ran"
    );
    assert_eq!(
        std::fs::read_dir(&parent).unwrap().count(),
        0,
        "normal child exit removes partially constructed fixture scratch"
    );
    std::fs::remove_dir(parent).unwrap();
}
