//! AU and LV2 plugin state: capture, embed, reopen, and drop on replacement.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oc_dsl::ast;

#[cfg(target_os = "macos")]
use crate::commands::plugin_host::apply_plugin_state_value;
use crate::commands::plugin_host::{
    load_plugin_for_preset_blocking, read_plugin_state_value, save_plugin_state_embedded_inner,
    save_plugin_state_file_inner,
};
use crate::commands::project::replace_project;

use super::support::{TempDir, default_project, make_track};

const LV2_GAIN_URI: &str = "http://opencompose.local/lv2/fixture/gain";
const LV2_MONO_URI: &str = "http://opencompose.local/lv2/fixture/mono";
#[cfg(target_os = "macos")]
const AU_PLUGIN: &str = "Apple: DLSMusicDevice";

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

const MONO_PORTS: &str = r#"
    lv2:port [
        a lv2:AudioPort, lv2:InputPort ; lv2:index 0 ; lv2:symbol "in" ; lv2:name "In"
    ], [
        a lv2:AudioPort, lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out" ; lv2:name "Out"
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 2 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:minimum 0.0 ; lv2:maximum 4.0 ; lv2:default 1.0
    ]"#;

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// A one-plugin LV2 bundle that reuses the fixture binary's descriptor.
struct FixtureBundle {
    root: PathBuf,
}

impl FixtureBundle {
    fn new(name: &str, uri: &str, ports: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ocp-gui-lv2-state-{}-{}-{name}",
            std::process::id(),
            unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let library = super::support::lv2_fixture_library();
        let binary = library.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::copy(&library, root.join(&binary)).unwrap();
        let manifest = format!(
            r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix pg: <http://lv2plug.in/ns/ext/port-groups#> .

<{uri}> a lv2:Plugin ;
    lv2:binary <{binary}> ;
    doap:name "Fixture State" ;
    {ports} .
"#
        );
        std::fs::write(root.join("manifest.ttl"), manifest).unwrap();
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

fn preset(name: &str, source: &str, plugin: &str) -> ast::PresetDef {
    ast::PresetDef {
        name: name.to_string(),
        source: Some(source.to_string()),
        plugin: Some(plugin.to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: Vec::new(),
        effects: Vec::new(),
        extends: None,
    }
}

fn lv2_state(uri: &str, bundle: &Path) -> Arc<crate::state::AppState> {
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = vec![preset("Panel", "lv2", uri)];
        project.tracks = vec![{
            let mut track = make_track("Keys");
            track.preset = Some("Panel".to_string());
            track
        }];
        inner.project = project;
        inner.scanned_plugins.push(oc_host::PluginInfo {
            id: uri.to_string(),
            name: "Fixture State".to_string(),
            vendor: "OpenCompose Tests".to_string(),
            format: oc_host::PluginFormat::Lv2,
            path: bundle.display().to_string(),
            category: oc_host::PluginCategory::Effect,
            num_params: 1,
            version: None,
            architectures: vec![std::env::consts::ARCH.to_string()],
            state_abi: None,
        });
    }
    state
}

/// The project as the DSL round trip would reopen it, with no live instances.
fn reopen(project: &ast::Project, info: &[oc_host::PluginInfo]) -> Arc<crate::state::AppState> {
    let dsl = oc_dsl::serialize(project);
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(&dsl).expect("the serialized project parses");
        inner.dsl_source = dsl;
        inner.scanned_plugins = info.to_vec();
    }
    state
}

fn state_file(state: &crate::state::AppState, preset_name: &str, key: &str) -> Option<String> {
    let inner = state.lock_inner();
    let preset = inner
        .project
        .presets
        .iter()
        .find(|p| p.name == preset_name)?;
    for (name, value) in &preset.params {
        if name == key
            && let ast::ParamValue::String(value) = value
        {
            return Some(value.clone());
        }
    }
    None
}

#[test]
fn an_lv2_preset_captures_its_state_to_a_file_and_reloads_it() {
    let bundle = FixtureBundle::new("gain", LV2_GAIN_URI, GAIN_PORTS);
    let state = lv2_state(LV2_GAIN_URI, bundle.path());
    let directory = TempDir::new("lv2-state-file");
    std::fs::create_dir_all(directory.path()).unwrap();
    let path = directory.path().join("panel.lv2state");
    let path_text = path.to_string_lossy().into_owned();

    save_plugin_state_file_inner(&state, "Panel", &path_text).expect("capture the fixture state");

    let encoded = std::fs::read(&path).expect("the capture wrote a file");
    let context =
        oc_host::PluginStateContext::new(oc_host::PluginFormat::Lv2, Some(LV2_GAIN_URI), "Panel");
    let bytes = oc_host::decode_plugin_state(&encoded, &context)
        .expect("the file carries the fixture's LV2 envelope");
    assert!(!bytes.is_empty(), "the fixture reports a state chunk");
    assert_eq!(
        state_file(&state, "Panel", "lv2_state_file").as_deref(),
        Some(path_text.as_str())
    );

    load_plugin_for_preset_blocking(&state, "Panel").expect("the captured state applies on load");
    assert!(
        state
            .lock_inner()
            .lv2_plugin_instances
            .contains_key("Panel")
    );
    let readout = read_plugin_state_value(&state, "Panel", None).expect("read the live state");
    assert_eq!(readout.format, "lv2");
    assert!(readout.loaded, "the live unit answers, not a temporary one");
}

#[test]
fn an_lv2_live_edit_survives_an_embed_and_reopen() {
    let bundle = FixtureBundle::new("gain-reopen", LV2_GAIN_URI, GAIN_PORTS);
    let state = lv2_state(LV2_GAIN_URI, bundle.path());
    load_plugin_for_preset_blocking(&state, "Panel").expect("load the fixture");
    let mut buffer = vec![1.0f32; 2 * 64];
    {
        let mut inner = state.lock_inner();
        let instance = inner
            .lv2_plugin_instances
            .get_mut("Panel")
            .expect("the loaded unit");
        assert!(instance.set_param("gain", 0.25));
        instance.process_interleaved(&mut buffer, 48_000);
    }
    assert!(
        (buffer[0] - 0.25).abs() < 1e-6,
        "the live edit reaches the plugin: {}",
        buffer[0]
    );

    save_plugin_state_embedded_inner(&state, "Panel").expect("embed the live state");
    let (project, info) = {
        let inner = state.lock_inner();
        (inner.project.clone(), inner.scanned_plugins.clone())
    };
    assert!(state_file(&state, "Panel", "lv2_state_hex").is_some());
    assert!(state_file(&state, "Panel", "lv2_state_file").is_none());

    let reopened = reopen(&project, &info);
    load_plugin_for_preset_blocking(&reopened, "Panel").expect("reopen with the embedded state");
    let mut buffer = vec![1.0f32; 2 * 64];
    reopened
        .lock_inner()
        .lv2_plugin_instances
        .get_mut("Panel")
        .expect("the reopened unit")
        .process_interleaved(&mut buffer, 48_000);
    assert!(
        (buffer[0] - 0.25).abs() < 1e-6,
        "reopened gain is {}",
        buffer[0]
    );
}

#[test]
fn an_lv2_preset_without_a_state_interface_reports_a_readable_error() {
    let bundle = FixtureBundle::new("mono", LV2_MONO_URI, MONO_PORTS);
    let state = lv2_state(LV2_MONO_URI, bundle.path());
    let directory = TempDir::new("lv2-no-state");
    std::fs::create_dir_all(directory.path()).unwrap();
    let path = directory.path().join("mono.lv2state");
    let path_text = path.to_string_lossy().into_owned();

    let error = save_plugin_state_file_inner(&state, "Panel", &path_text)
        .expect_err("a unit without state:interface cannot be captured");
    assert!(
        error.contains("does not implement state:interface"),
        "{error}"
    );
    assert!(!path.exists(), "a refused capture writes no file");
}

#[test]
fn a_builtin_preset_names_the_formats_that_carry_state() {
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = vec![preset("Synth", "builtin", "builtin.open_synth")];
        inner.project = project;
    }

    let error =
        read_plugin_state_value(&state, "Synth", None).expect_err("a built-in has no state");
    assert!(error.contains("AU") && error.contains("LV2"), "{error}");
}

#[test]
fn project_replacement_drops_the_lv2_unit_and_the_next_lookup_reloads() {
    let bundle = FixtureBundle::new("gain-replace", LV2_GAIN_URI, GAIN_PORTS);
    let state = lv2_state(LV2_GAIN_URI, bundle.path());
    load_plugin_for_preset_blocking(&state, "Panel").expect("load the fixture");
    assert!(
        state
            .lock_inner()
            .lv2_plugin_instances
            .contains_key("Panel")
    );

    let fresh = oc_dsl::parse(&format!(
        "@project \"Fresh\"\n@preset Panel\n  source: lv2\n  plugin: \"{LV2_GAIN_URI}\"\n"
    ))
    .unwrap();
    {
        let mut inner = state.lock_inner();
        replace_project(&mut inner, fresh, Some("/tmp/Fresh.ocp".into())).unwrap();
        assert!(inner.plugin_instances.is_empty());
        assert!(inner.vst3_plugin_instances.is_empty());
        assert!(inner.vst2_plugin_instances.is_empty());
        assert!(
            inner.lv2_plugin_instances.is_empty(),
            "the replaced project must drop its LV2 unit"
        );
        #[cfg(target_os = "macos")]
        assert!(inner.au_plugin_instances.is_empty());
    }

    load_plugin_for_preset_blocking(&state, "Panel").expect("the next lookup instantiates a unit");
    assert!(
        state
            .lock_inner()
            .lv2_plugin_instances
            .contains_key("Panel")
    );
}

#[cfg(target_os = "macos")]
fn au_state() -> Arc<crate::state::AppState> {
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = vec![preset("Au Keys", "au", AU_PLUGIN)];
        project.tracks = vec![{
            let mut track = make_track("Keys");
            track.preset = Some("Au Keys".to_string());
            track
        }];
        inner.project = project;
    }
    state
}

/// A system AudioUnit is instantiated only when the operator opts in.
#[cfg(target_os = "macos")]
fn system_au_available() -> bool {
    if std::env::var_os("OCP_TEST_PLUGINS").is_none() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host the system AudioUnit");
        return false;
    }
    if oc_host::au_instance::resolve_component(AU_PLUGIN).is_err() {
        eprintln!("skipping: '{AU_PLUGIN}' is not registered");
        return false;
    }
    true
}

#[cfg(target_os = "macos")]
#[test]
fn an_au_preset_captures_its_state_to_a_file_and_reloads_it() {
    if !system_au_available() {
        return;
    }
    let state = au_state();
    let directory = TempDir::new("au-state-file");
    std::fs::create_dir_all(directory.path()).unwrap();
    let path = directory.path().join("keys.aupreset");
    let path_text = path.to_string_lossy().into_owned();

    save_plugin_state_file_inner(&state, "Au Keys", &path_text).expect("capture the unit state");

    let encoded = std::fs::read(&path).expect("the capture wrote a file");
    let context =
        oc_host::PluginStateContext::new(oc_host::PluginFormat::Au, Some(AU_PLUGIN), "Au Keys");
    let bytes = oc_host::decode_plugin_state(&encoded, &context)
        .expect("the file carries the unit's AU envelope");
    assert!(
        bytes.starts_with(b"bplist"),
        "ClassInfo is a binary property list, got {:?}",
        &bytes[..bytes.len().min(8)]
    );
    assert_eq!(
        state_file(&state, "Au Keys", "au_state_file").as_deref(),
        Some(path_text.as_str())
    );

    // Read it back through the same command surface and apply it again.
    let readout = read_plugin_state_value(&state, "Au Keys", None).expect("read the state");
    assert_eq!(readout.format, "au");
    let hex = readout.state_hex.expect("the state fits the inline cap");
    let applied = apply_plugin_state_value(&state, "Au Keys", &hex).expect("apply the state");
    assert_eq!(applied.format, "au");
    assert_eq!(applied.param, "au_state_hex");

    state.lock_inner().au_plugin_instances.clear();
    load_plugin_for_preset_blocking(&state, "Au Keys").expect("the captured state applies on load");
    assert!(
        state
            .lock_inner()
            .au_plugin_instances
            .contains_key("Au Keys")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn project_replacement_drops_the_au_unit() {
    if !system_au_available() {
        return;
    }
    let state = au_state();
    load_plugin_for_preset_blocking(&state, "Au Keys").expect("load the system unit");
    assert!(
        state
            .lock_inner()
            .au_plugin_instances
            .contains_key("Au Keys")
    );

    let fresh = oc_dsl::parse("@project \"Fresh\"\n@preset \"Au Keys\"\n  source: au\n  plugin: \"Apple: DLSMusicDevice\"\n").unwrap();
    {
        let mut inner = state.lock_inner();
        replace_project(&mut inner, fresh, None).unwrap();
        assert!(
            inner.au_plugin_instances.is_empty(),
            "the replaced project must drop its AU unit"
        );
        assert!(inner.lv2_plugin_instances.is_empty());
    }
}
