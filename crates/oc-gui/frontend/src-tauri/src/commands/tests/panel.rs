//! Tests for the generated parameter panel and the gesture that drives it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oc_dsl::ast;
use oc_host::params::{ParamSurface, ParamSurfaceIssue};

use crate::commands::plugin_host::{
    begin_plugin_param_gesture_inner, end_plugin_param_gesture_inner,
    load_plugin_for_preset_blocking, plugin_panel, plugin_panel_with,
    update_plugin_param_gesture_inner,
};

use super::support::{default_project, make_track};

/// The fixture binary only exports the gain descriptor, so the panel bundle
/// reuses its URI with a 20-control-port manifest.
const PANEL_URI: &str = "http://opencompose.local/lv2/fixture/gain";

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

/// A plugin surface with `count` parameters, as a format would report it.
fn surface_of(count: u32) -> ParamSurface {
    let descriptors = (0..count)
        .map(|index| oc_host::params::ParamDescriptor {
            index,
            name: format!("Param {index}"),
            unit: "dB".to_string(),
            min: 0.0,
            max: 10.0,
            current: 2.5,
            display: "2.500 dB".to_string(),
            normalized: 0.25,
            default: Some(1.0),
            steps: None,
            automatable: true,
            by_passing: false,
            group: None,
        })
        .collect();
    oc_host::params::surface(descriptors)
}

fn state_with_presets(presets: Vec<ast::PresetDef>, tracks: usize) -> Arc<crate::state::AppState> {
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = presets;
        project.tracks = (0..tracks)
            .map(|index| {
                let mut track = make_track(&format!("Track {index}"));
                track.preset = Some("Knobs".to_string());
                track
            })
            .collect();
        inner.project = project;
    }
    state
}

#[test]
fn the_panel_types_each_format_instead_of_returning_an_empty_list() {
    let state = state_with_presets(
        vec![
            preset("Organ", "soundfont", ""),
            preset("Channel", "clap", "clap:channel"),
            preset("Knobs", "vst2", "vst2:knobs"),
        ],
        1,
    );

    let panel =
        plugin_panel_with(&state, "Organ", |_| panic!("a soundfont has no plugin")).unwrap();
    assert_eq!(panel.format, "soundfont");
    assert!(matches!(
        panel.surface,
        ParamSurface::Unavailable {
            issue: ParamSurfaceIssue::FormatHasNoParameters { .. }
        }
    ));

    let panel = plugin_panel_with(&state, "Channel", |_| {
        panic!("a CLAP preset keeps its editor")
    })
    .unwrap();
    assert_eq!(panel.format, "clap");
    assert!(matches!(
        panel.surface,
        ParamSurface::Unavailable {
            issue: ParamSurfaceIssue::EditorFormat { .. }
        }
    ));

    let panel = plugin_panel_with(&state, "Knobs", |_plan| Ok(surface_of(20))).unwrap();
    assert_eq!(panel.format, "vst2");
    assert_eq!(panel.surface.parameters().len(), 20);
    assert_eq!(panel.surface.parameters()[19].name, "Param 19");
}

#[test]
fn a_plugin_that_reports_no_parameters_is_a_typed_state() {
    let state = state_with_presets(vec![preset("Knobs", "vst2", "vst2:knobs")], 1);
    let panel = plugin_panel_with(&state, "Knobs", |_plan| {
        Ok(oc_host::params::surface(Vec::new()))
    })
    .unwrap();
    assert_eq!(
        panel.surface,
        ParamSurface::Unavailable {
            issue: ParamSurfaceIssue::PluginReportsNoParameters
        }
    );
}

#[test]
fn a_failed_enumeration_keeps_its_message_for_the_panel() {
    let state = state_with_presets(vec![preset("Knobs", "vst2", "vst2:knobs")], 1);
    let panel = plugin_panel_with(&state, "Knobs", |_plan| {
        Err("plugin refused to load".to_string())
    })
    .unwrap();
    assert_eq!(
        panel.surface,
        ParamSurface::Unavailable {
            issue: ParamSurfaceIssue::QueryFailed {
                message: "plugin refused to load".to_string()
            }
        }
    );
}

#[test]
fn an_unknown_preset_is_an_error_not_an_empty_panel() {
    let state = state_with_presets(vec![preset("Knobs", "vst2", "vst2:knobs")], 1);
    let error = plugin_panel_with(&state, "Missing", |_| Ok(surface_of(1))).unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

// ============================================================================
// LV2 end-to-end: a real bundle with 20 control ports
// ============================================================================

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn fixture_library(directory: &Path) -> PathBuf {
    let library = super::support::lv2_fixture_library();
    let output = directory.join(library.file_name().unwrap());
    std::fs::copy(&library, &output).unwrap();
    output
}

/// A gain bundle with 20 control inputs, one of which is the host's sample-rate port.
fn panel_bundle() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ocp-gui-lv2-panel-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let library = fixture_library(&root);
    let binary = library.file_name().unwrap().to_string_lossy().into_owned();
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
        pg:group [ a pg:Group ; lv2:name "Output" ]
    ]"#,
    );
    for step in 0..20 {
        let index = 6 + step;
        let designation = if step == 0 {
            "lv2:designation lv2:sampleRate ; "
        } else {
            ""
        };
        ports.push_str(&format!(
            r#", [
        a lv2:ControlPort, lv2:InputPort ; lv2:index {index} ;
        lv2:symbol "knob_{step}" ; lv2:name "Knob {step}" ;
        lv2:minimum 0.0 ; lv2:maximum 10.0 ; lv2:default 2.5 ;
        {designation}pg:group [ a pg:Group ; lv2:name "Filter" ]
    ]"#
        ));
    }
    let manifest = format!(
        r#"
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix pg: <http://lv2plug.in/ns/ext/port-groups#> .

<{PANEL_URI}> a lv2:Plugin ;
    lv2:binary <{binary}> ;
    doap:name "Fixture Panel" ;
    {ports} .
"#
    );
    std::fs::write(root.join("manifest.ttl"), manifest).unwrap();
    root
}

fn plugin_info(root: &Path) -> oc_host::PluginInfo {
    oc_host::PluginInfo {
        id: PANEL_URI.to_string(),
        name: "Fixture Panel".to_string(),
        vendor: "OpenCompose Tests".to_string(),
        format: oc_host::PluginFormat::Lv2,
        path: root.display().to_string(),
        category: oc_host::PluginCategory::Effect,
        num_params: 20,
        version: None,
        architectures: vec![std::env::consts::ARCH.to_string()],
        state_abi: None,
    }
}

fn lv2_state(root: &Path) -> Arc<crate::state::AppState> {
    let state = Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = vec![preset("Panel", "lv2", PANEL_URI)];
        project.tracks = vec![{
            let mut track = make_track("Keys");
            track.preset = Some("Panel".to_string());
            track
        }];
        inner.project = project;
        inner.scanned_plugins.push(plugin_info(root));
    }
    state
}

#[test]
fn a_twenty_parameter_lv2_panel_reads_the_live_values() {
    let root = panel_bundle();
    let state = lv2_state(&root);
    load_plugin_for_preset_blocking(&state, "Panel").expect("load the fixture bundle");

    let panel = plugin_panel(&state, "Panel").expect("panel");
    assert_eq!(panel.format, "lv2");
    let parameters = panel.surface.parameters();
    assert_eq!(parameters.len(), 20, "{parameters:#?}");
    assert_eq!(parameters[0].name, "Gain");
    assert_eq!(parameters[0].group.as_deref(), Some("Output"));
    assert!((parameters[0].current - 0.5).abs() < 1e-6);
    assert_eq!(parameters[1].group.as_deref(), Some("Filter"));
    assert!(
        !parameters.iter().any(|param| param.name == "Knob 0"),
        "the host-managed sample-rate port must not be a knob"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn one_lv2_drag_is_one_undo_entry_and_reaches_the_plugin() {
    let root = panel_bundle();
    let state = lv2_state(&root);
    load_plugin_for_preset_blocking(&state, "Panel").expect("load the fixture bundle");

    {
        let mut inner = state.lock_inner();
        begin_plugin_param_gesture_inner(&mut inner, "Panel", 0, "Knob 3").expect("begin");
        for normalized in [0.25, 0.5, 0.75] {
            update_plugin_param_gesture_inner(
                &mut inner,
                "Panel",
                "Knob 3",
                normalized,
                ast::BarBeat::new(2, 3, 0),
            )
            .expect("update");
        }
        end_plugin_param_gesture_inner(&mut inner, "Panel", "Knob 3").expect("end");

        // The plugin holds the value in its own units, not a shadow copy.
        let instance = inner.lv2_plugin_instances.get("Panel").expect("instance");
        let current = instance.get_param("Knob 3").expect("parameter");
        assert!(
            (current - 7.5).abs() < 1e-6,
            "expected 7.5 Hz, got {current}"
        );

        // One lane with the last value, at the gesture's position.
        let lanes: Vec<_> = inner.project.tracks[0].automation().collect();
        assert_eq!(lanes.len(), 1);
        assert_eq!(
            lanes[0].param,
            crate::commands::plugin_host::plugin_automation_lane_name("Knob 3")
        );
        assert_eq!(lanes[0].points.len(), 1);
        assert_eq!(lanes[0].points[0].position, ast::BarBeat::new(2, 3, 0));
        let ast::ParamValue::Number(stored) = lanes[0].points[0].value else {
            panic!("automation stores a number");
        };
        assert!(
            (stored - 0.75).abs() < 1e-9,
            "lane stores 0.75, got {stored}"
        );

        // The whole drag is one undo entry.
        assert_eq!(inner.undo.undo.len(), 1);
    }

    // Undo takes the lane away again, so the drag left exactly one entry.
    {
        let mut inner = state.lock_inner();
        let mut project = inner.project.clone();
        assert!(
            inner.undo.undo(&mut project).is_ok(),
            "the drag is undoable"
        );
        inner.project = project;
        assert!(inner.project.tracks[0].automation_count() == 0);
    }
    let _ = std::fs::remove_dir_all(&root);
}
