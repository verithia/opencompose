//! Offline render integration for LV2 instruments and effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use oc_engine::render_cache::RenderCache;
use oc_host::lv2::Lv2Instance;

const TONE_URI: &str = "http://opencompose.local/lv2/fixture/tone";
const GAIN_URI: &str = "http://opencompose.local/lv2/fixture/gain";

fn fixture_library() -> PathBuf {
    if let Some(path) = std::env::var_os("OCP_LV2_FIXTURE") {
        return PathBuf::from(path);
    }
    oc_host::test_support::lv2_module_fixture()
        .expect("native LV2 fixture built for this host target")
        .to_path_buf()
}

fn prepare_bundles(parent: &Path, library: &Path) -> std::io::Result<PathBuf> {
    let staging = tempfile::Builder::new()
        .prefix("lv2-render-")
        .tempdir_in(parent)?;
    let binary = format!("fixture.{}", std::env::consts::DLL_EXTENSION);
    for (name, declaration) in [("tone.lv2", tone_manifest()), ("gain.lv2", gain_manifest())] {
        let bundle = staging.path().join(name);
        std::fs::create_dir(&bundle)?;
        std::fs::copy(library, bundle.join(&binary))?;
        std::fs::write(
            bundle.join("manifest.ttl"),
            declaration.replace("{{BINARY}}", &binary),
        )?;
    }
    // The process scratch-home guard owns successful bundles until test exit.
    // Every earlier error/unwind removes the unpublished staging directory.
    Ok(staging.keep())
}

fn bundle_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = prepare_bundles(&oc_audio::private_fs::test_root(), &fixture_library())
            .expect("prepare complete LV2 fixture bundles");
        oc_host::lv2::add_search_path(root.clone());
        root
    })
}

#[test]
fn failed_bundle_preparation_cleans_staging_and_allows_a_fresh_attempt() {
    let parent = tempfile::Builder::new()
        .prefix("lv2-stage-failure-")
        .tempdir_in(oc_audio::private_fs::test_root())
        .unwrap();
    let missing = parent.path().join("missing-native-library");
    for _ in 0..2 {
        assert!(prepare_bundles(parent.path(), &missing).is_err());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }
    let ready = prepare_bundles(parent.path(), &fixture_library()).unwrap();
    assert!(ready.join("tone.lv2/manifest.ttl").is_file());
    assert!(ready.join("gain.lv2/manifest.ttl").is_file());
    std::fs::remove_dir_all(ready).unwrap();
}

fn tone_manifest() -> &'static str {
    r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
<http://opencompose.local/lv2/fixture/tone> a lv2:Plugin, lv2:InstrumentPlugin ;
    lv2:binary <{{BINARY}}> ;
    doap:name "Fixture Tone" ;
    lv2:port [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 0 ; lv2:symbol "out_l"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out_r"
    ], [
        a lv2:AtomPort, lv2:InputPort ; lv2:index 2 ; lv2:symbol "midi_in" ;
        atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent
    ] .
"#
}

fn gain_manifest() -> &'static str {
    r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
<http://opencompose.local/lv2/fixture/gain> a lv2:Plugin ;
    lv2:binary <{{BINARY}}> ;
    doap:name "Fixture Gain" ;
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l"
    ], [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 1 ; lv2:symbol "in_r"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out_l"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 3 ; lv2:symbol "out_r"
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 4 ; lv2:symbol "gain" ;
        lv2:minimum 0.0 ; lv2:maximum 4.0 ; lv2:default 1.0
    ] .
"#
}

fn tone_project(effects: &str) -> oc_dsl::ast::Project {
    let source = format!(
        r#"@ocp "0.2.0"
@project "lv2 tone"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: lv2
  plugin: "{TONE_URI}"
@track lead | preset: tone, instrument: synth/lead
  @phrase main [1:1 - 1:2]
    C4 q mf
{effects}"#
    );
    oc_dsl::parse(&source).unwrap()
}

fn builtin_project(effects: &str) -> oc_dsl::ast::Project {
    let source = format!(
        r#"@ocp "0.2.0"
@project "lv2 effect"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 440, amplitude: 0.1}}
@track lead | preset: tone, instrument: synth/lead
  @phrase main [1:1 - 1:2]
    C4 q mf
{effects}"#
    );
    oc_dsl::parse(&source).unwrap()
}

fn render(project: &oc_dsl::ast::Project) -> Vec<f32> {
    RenderCache::new()
        .render_full(
            project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap()
}

#[test]
fn lv2_instrument_renders_the_same_track_buffer_as_a_direct_instance() {
    let bundles = bundle_root();
    let project = tone_project("");
    let mut cache = RenderCache::new();
    let audio = cache
        .render_full(
            &project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(
        audio.iter().any(|sample| sample.abs() > 1e-4),
        "the LV2 instrument must be audible"
    );
    let track = cache
        .get_track_buffer(0)
        .expect("rendered track buffer")
        .to_vec();

    let mut direct = Lv2Instance::load(&bundles.join("tone.lv2"), TONE_URI, 48_000, 512).unwrap();
    direct.note_on_channel(0, 60, 80.0 / 127.0, 0);
    let blocks = 187;
    let mut expected = Vec::with_capacity(blocks * 256);
    for _ in 0..blocks {
        expected.extend_from_slice(direct.process_block_with_transport(128, None));
    }
    assert!(track.len() >= expected.len());
    for (index, (rendered, expected)) in track.iter().zip(&expected).enumerate() {
        assert!(
            (rendered - expected).abs() < 1e-6,
            "sample {index}: {rendered} != {expected}"
        );
    }
}

#[test]
fn lv2_effect_is_exact_at_unity_and_silent_at_zero() {
    bundle_root();
    let dry = render(&builtin_project(""));
    let unity = render(&builtin_project(&format!(
        "  effects:\n    - plugin:\"lv2.{GAIN_URI}\": gain=1.0\n"
    )));
    assert_eq!(dry.len(), unity.len());
    if let Some((index, (a, b))) = dry
        .iter()
        .zip(&unity)
        .enumerate()
        .find(|(_, (a, b))| (*a - *b).abs() > 1e-6)
    {
        panic!("unity mismatch at {index}: dry {a} unity {b}");
    }

    let silent = render(&builtin_project(&format!(
        "  effects:\n    - plugin:\"lv2.{GAIN_URI}\": gain=0.0\n"
    )));
    assert!(
        silent.iter().all(|sample| sample.abs() < 1e-9),
        "a zero LV2 gain must silence the track"
    );
}
