use crate::commands::plugin_host::load_plugin_for_preset_blocking;
use crate::state::AppState;

use super::support::TempDir;

#[test]
fn plugin_panel_restores_state_relative_to_its_saved_project() {
    let dir = TempDir::new("relative-plugin-state");
    let states = dir.path().join("states");
    std::fs::create_dir_all(&states).unwrap();
    let mut chunk = b"OCF2".to_vec();
    chunk.extend_from_slice(&0.75f32.to_le_bytes());
    chunk.extend_from_slice(&0.0f32.to_le_bytes());
    std::fs::write(states.join("saved.bank"), chunk).unwrap();
    let project = oc_dsl::parse(
        r#"@ocp "0.6.0"
@project "Saved state"
  tempo: 120
  time_signature: 4/4
@preset saved
  source: vst2
  plugin: "test.fixture.vst2.gain"
  params: {vst2_state_file: "states/saved.bank"}
"#,
    )
    .unwrap();
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = project;
        inner.file_path = Some(dir.path().join("Saved.ocp"));
    }
    let parameters = load_plugin_for_preset_blocking(&state, "saved")
        .expect("the plugin panel uses the document's directory for state");
    let gain = parameters
        .iter()
        .find(|parameter| parameter.name == "Gain")
        .unwrap();
    assert_eq!(gain.current, 0.75);
    assert_eq!(
        state.lock_inner().project.presets[0].params[0].1,
        oc_dsl::ast::ParamValue::String("states/saved.bank".into())
    );
}
