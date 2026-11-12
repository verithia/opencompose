//! Program (patch) commands: native enumeration, selection, and the
//! state-injection fallback. The fake VST3 keeps this unconditional; a real
//! plugin can be exercised with `OCP_TEST_PLUGINS=1` outside this suite.

use crate::commands::tests::support::TempDir;
use crate::commands::{
    PLUGIN_STATE_INLINE_BYTES, PLUGIN_STATE_INLINE_CEILING, ProgramSelectRequest,
    apply_plugin_state_value, plugin_program_listing, read_plugin_state_value,
    select_plugin_program_request,
};
use crate::state::{AppState, UndoScope, UndoScopes};

const PROJECT: &str = r#"@ocp "0.2.0"
@project "program select"
  tempo: 120
@preset fake
  source: vst3
  plugin: "test.fake.one_bus_unity"
@track host | preset: fake, instrument: keys/kontakt
  @phrase line [1:1 - 2:1]
    C4 q
"#;

const CLAP_PROJECT: &str = r#"@ocp "0.2.0"
@project "state injection"
  tempo: 120
@preset synth
  source: clap
  plugin: "com.example.synth"
@track lead | preset: synth, instrument: synth/lead_square
  @phrase line [1:1 - 2:1]
    C4 q
"#;

fn state_with_project(source: &str) -> AppState {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(source).expect("test project parses");
        inner.dsl_source = oc_dsl::serialize(&inner.project);
    }
    state
}

fn state_with_fake_plugin() -> AppState {
    let state = state_with_project(PROJECT);
    let instance =
        oc_host::vst3_instance::test_support::fake_instance_for_id("test.fake.one_bus_unity", 512)
            .expect("in-process fake VST3");
    state.lock_inner().vst3_plugin_instances.insert(
        "fake".to_string(),
        std::sync::Arc::new(std::sync::Mutex::new(instance)),
    );
    state
}

fn render_peak(state: &AppState) -> f32 {
    oc_host::application_main::ApplicationMainExecutor::ensure_test_executor()
        .expect("test application executor");
    let snapshot = crate::commands::render::capture_offline_snapshot(
        state,
        None,
        &crate::commands::render::ExportOverrides::default(),
    )
    .expect("offline render snapshot");
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache.set_vst3_tail_probe(|plan| {
        if plan.path.to_str() == Some("test.fake.one_bus_unity") {
            Ok(0)
        } else {
            Err(oc_host::vst3_instance::Vst3OwnedError::InvalidRequest(
                "Unexpected program fixture plugin",
            ))
        }
    });
    let (audio, _cache) =
        crate::commands::render::render_offline_snapshot_using_cache(snapshot, cache)
            .expect("offline render");
    audio
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

fn preset_param(state: &AppState, preset: &str, key: &str) -> Option<String> {
    state
        .lock_inner()
        .project
        .presets
        .iter()
        .find(|candidate| candidate.name == preset)
        .and_then(|preset| {
            preset
                .params
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| format!("{value:?}"))
        })
}

#[test]
fn the_capability_table_is_data_the_agent_can_read() {
    let table = crate::commands::program_capability_table();
    let vst3 = table
        .iter()
        .find(|row| row.format == "vst3")
        .expect("vst3 row");
    assert_eq!(vst3.enumerate, "native");
    assert!(vst3.select.iter().any(|row| row == "native_program_data"));
    assert!(vst3.select.iter().any(|row| row == "state_injection"));

    let clap = table
        .iter()
        .find(|row| row.format == "clap")
        .expect("clap row");
    assert_eq!(clap.enumerate, "none");
    assert_eq!(clap.select, vec!["state_injection".to_string()]);
    for row in &table {
        assert!(!row.note.is_empty(), "{} has no note", row.format);
    }
}

#[test]
fn a_fake_plugin_lists_its_native_programs() {
    let state = state_with_fake_plugin();
    let listing = plugin_program_listing(&state, "fake").unwrap();

    assert_eq!(listing.format, "vst3");
    assert_eq!(listing.enumerate, "native");
    assert!(listing.loaded);
    assert_eq!(
        listing
            .programs
            .iter()
            .map(|program| program.name.as_str())
            .collect::<Vec<_>>(),
        ["Init", "Bright"]
    );
    assert!(
        listing
            .select
            .iter()
            .any(|mechanism| mechanism == "native_program_data")
    );
}

#[test]
fn selecting_a_program_writes_state_the_renderer_obeys() {
    let state = state_with_fake_plugin();

    let silent = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            index: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(silent.mechanism, "native_program_data");
    assert_eq!(silent.program.as_ref().unwrap().name, "Init");
    assert!(silent.state_bytes > 0);
    assert!(
        preset_param(&state, "fake", "vst3_state_hex").is_some(),
        "the selected program must be embedded in the project"
    );
    assert_eq!(render_peak(&state), 0.0, "program 0 is the silent program");

    let bright = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            name: Some("Bright".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(bright.program.as_ref().unwrap().name, "Bright");
    assert!(
        render_peak(&state) > 0.5,
        "the selected program must produce audible audio"
    );

    // Selection carries the same undo scope as the GUI's plugin-state save.
    let inner = state.lock_inner();
    let entry = inner.undo.undo.last().expect("selection is undoable");
    assert_eq!(entry.label, "plugin state");
    assert!(
        entry
            .scopes
            .intersects(UndoScopes::one(UndoScope::PluginState))
    );
}

#[test]
fn a_selected_program_can_be_written_to_a_sidecar() {
    let dir = TempDir::new("program-sidecar");
    std::fs::create_dir_all(dir.path()).unwrap();
    let path = dir.path().join("bright.state");
    let state = state_with_fake_plugin();

    let outcome = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            index: Some(1),
            mode: Some("file".to_string()),
            path: Some(path.display().to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(outcome.state_param, "vst3_state_file");
    assert!(path.exists(), "the sidecar must be written");
    assert!(preset_param(&state, "fake", "vst3_state_file").is_some());
    assert!(
        preset_param(&state, "fake", "vst3_state_hex").is_none(),
        "a sidecar selection must clear the embedded state"
    );
}

#[test]
fn an_unknown_program_is_refused_with_the_available_names() {
    let state = state_with_fake_plugin();
    let error = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            index: Some(7),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("out of range"), "{error}");
    assert!(error.contains("Init"), "{error}");

    let error = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            name: Some("Nope".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("no program named 'Nope'"), "{error}");
    assert!(
        error.contains("Init") && error.contains("Bright"),
        "a refused name must list the programs that were offered: {error}"
    );
}

#[test]
fn a_selection_without_an_address_is_refused() {
    let state = state_with_fake_plugin();
    let error =
        select_plugin_program_request(&state, "fake", ProgramSelectRequest::default()).unwrap_err();
    assert!(error.contains("index"), "{error}");
    assert!(error.contains("state_file"), "{error}");

    let error = select_plugin_program_request(
        &state,
        "fake",
        ProgramSelectRequest {
            index: Some(0),
            name: Some("Init".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("not both"), "{error}");
}

#[test]
fn a_format_without_a_native_list_explains_the_state_fallback() {
    let state = state_with_project(CLAP_PROJECT);
    let error = select_plugin_program_request(
        &state,
        "synth",
        ProgramSelectRequest {
            index: Some(0),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("no native program list"), "{error}");
    assert!(error.contains("state_file"), "{error}");

    let listing = plugin_program_listing(&state, "synth").unwrap();
    assert_eq!(listing.format, "clap");
    assert_eq!(listing.enumerate, "none");
    assert!(listing.programs.is_empty());
}

#[test]
fn a_program_can_be_selected_by_injecting_its_state_file() {
    let dir = TempDir::new("state-injection");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state_file = dir.path().join("patch.bin");
    std::fs::write(&state_file, b"opaque program blob").unwrap();
    let state = state_with_project(CLAP_PROJECT);

    let outcome = select_plugin_program_request(
        &state,
        "synth",
        ProgramSelectRequest {
            state_file: Some(state_file.display().to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(outcome.mechanism, "state_injection");
    assert_eq!(outcome.state_param, "clap_state_file");
    assert!(preset_param(&state, "synth", "clap_state_file").is_some());
    let inner = state.lock_inner();
    let entry = inner.undo.undo.last().expect("injection is undoable");
    assert!(
        entry
            .scopes
            .intersects(UndoScopes::one(UndoScope::PluginState))
    );
}

#[test]
fn state_injection_refuses_an_empty_or_missing_file() {
    let state = state_with_project(CLAP_PROJECT);
    let error = select_plugin_program_request(
        &state,
        "synth",
        ProgramSelectRequest {
            state_file: Some("/nonexistent/program.bin".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("failed to read state file"), "{error}");

    let dir = TempDir::new("empty-state");
    std::fs::create_dir_all(dir.path()).unwrap();
    let empty = dir.path().join("empty.bin");
    std::fs::write(&empty, b"").unwrap();
    let error = select_plugin_program_request(
        &state,
        "synth",
        ProgramSelectRequest {
            state_file: Some(empty.display().to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("empty"), "{error}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn a_state_value_round_trips_into_a_fresh_project() {
    let source = state_with_fake_plugin();
    select_plugin_program_request(
        &source,
        "fake",
        ProgramSelectRequest {
            name: Some("Bright".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let read = read_plugin_state_value(&source, "fake", None).unwrap();
    assert_eq!(read.format, "vst3");
    assert_eq!(read.param, "vst3_state_hex");
    assert_eq!(read.plugin, "test.fake.one_bus_unity");
    assert!(read.loaded, "the live fake answered the read");
    assert!(!read.truncated);
    assert_eq!(read.limit_bytes, PLUGIN_STATE_INLINE_BYTES);
    assert_eq!(read.sha256.len(), 64);
    assert!(read.bytes > 0);
    let state_hex = read.state_hex.clone().expect("a small state is inlined");
    assert_eq!(state_hex.len(), read.bytes * 2);

    // A project that never loaded the plugin must render the applied value.
    let target = state_with_project(PROJECT);
    let applied = apply_plugin_state_value(&target, "fake", &state_hex).unwrap();
    assert_eq!(applied.param, "vst3_state_hex");
    assert_eq!(applied.bytes, read.bytes);
    assert_eq!(applied.sha256, read.sha256);
    assert!(preset_param(&target, "fake", "vst3_state_hex").is_some());
    assert!(
        preset_param(&target, "fake", "vst3_state_file").is_none(),
        "an inline apply clears any sidecar"
    );
    assert!(
        render_peak(&target) > 0.5,
        "the applied state must be the state the renderer uses"
    );
    let inner = target.lock_inner();
    let entry = inner.undo.undo.last().expect("apply is undoable");
    assert_eq!(entry.label, "plugin state");
    assert!(
        entry
            .scopes
            .intersects(UndoScopes::one(UndoScope::PluginState))
    );
}

#[test]
fn a_state_over_the_inline_cap_is_truncated_without_a_fragment() {
    let state = state_with_fake_plugin();
    let read = read_plugin_state_value(&state, "fake", Some(1)).unwrap();
    assert!(read.truncated, "a 1-byte cap cannot hold an envelope");
    assert!(read.state_hex.is_none(), "no fragment may be returned");
    assert!(read.bytes > 1);
    assert_eq!(read.limit_bytes, 1);
    assert_eq!(read.sha256.len(), 64);
    assert!(read.note.contains("no fragment"), "{}", read.note);
    assert!(read.note.contains("max_bytes"), "{}", read.note);
}

#[test]
fn a_state_cap_over_the_ceiling_is_refused() {
    let state = state_with_fake_plugin();
    let error =
        read_plugin_state_value(&state, "fake", Some(PLUGIN_STATE_INLINE_CEILING + 1)).unwrap_err();
    assert!(error.contains("at most"), "{error}");
    assert!(error.contains("max_bytes"), "{error}");
    assert!(read_plugin_state_value(&state, "fake", Some(0)).is_err());
}

#[test]
fn a_state_from_another_plugin_is_refused_before_writing() {
    let state = state_with_project(PROJECT);
    let foreign = oc_host::encode_plugin_state(
        oc_host::PluginFormat::Vst3,
        "com.example.other",
        b"not this plugin",
    );
    let error = apply_plugin_state_value(&state, "fake", &hex(&foreign)).unwrap_err();
    assert!(
        error.contains("belongs to plugin 'com.example.other'"),
        "{error}"
    );
    assert!(error.contains("test.fake.one_bus_unity"), "{error}");
    assert!(
        preset_param(&state, "fake", "vst3_state_hex").is_none(),
        "a refused state must not touch the project"
    );
}

#[test]
fn a_tampered_state_is_refused_by_its_sha256() {
    let source = state_with_fake_plugin();
    let read = read_plugin_state_value(&source, "fake", None).unwrap();
    let mut tampered = read.state_hex.expect("a small state is inlined");
    let last = if tampered.ends_with('0') { '1' } else { '0' };
    tampered.pop();
    tampered.push(last);

    let target = state_with_project(PROJECT);
    let error = apply_plugin_state_value(&target, "fake", &tampered).unwrap_err();
    assert!(error.contains("integrity check"), "{error}");
    assert!(error.contains("sha256 mismatch"), "{error}");
    assert!(
        preset_param(&target, "fake", "vst3_state_hex").is_none(),
        "a tampered state must not touch the project"
    );
}

#[test]
fn an_empty_or_malformed_state_value_is_refused() {
    let state = state_with_project(PROJECT);
    let error = apply_plugin_state_value(&state, "fake", "").unwrap_err();
    assert!(error.contains("empty"), "{error}");
    let error = apply_plugin_state_value(&state, "fake", "abc").unwrap_err();
    assert!(error.contains("odd number of digits"), "{error}");
    let error = apply_plugin_state_value(&state, "fake", "zz").unwrap_err();
    assert!(error.contains("invalid state hex"), "{error}");
}
