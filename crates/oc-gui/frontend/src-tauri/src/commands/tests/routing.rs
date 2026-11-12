//! Mixer routing edits: node creation, delete policy, edge re-pointing,
//! `.ocp` round-trip and render parity with a hand-written project.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast;

use super::support::test_state;
use crate::commands::mixer::{
    apply_add_bus, apply_add_control_route, apply_add_send, apply_remove_bus,
    apply_remove_control_route, apply_set_send_target, apply_set_track_output,
    apply_set_track_output_port, bus_usage,
};
use crate::commands::shared::{resync_mixer_with_undo, resync_with_undo};
use crate::state::{UndoEdit, UndoScope};

const BASE: &str = r#"@ocp "0.2.0"

@project "routing edits"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus space
  routes: [master]

@track a | preset: tone, instrument: synth/lead_square
  mix: send(space=50%)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#;

fn project(source: &str) -> ast::Project {
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn bus<'a>(project: &'a ast::Project, name: &str) -> &'a ast::Bus {
    project
        .buses
        .iter()
        .find(|bus| bus.name == name)
        .unwrap_or_else(|| panic!("missing bus '{name}'"))
}

fn render(project: &ast::Project) -> Vec<f32> {
    oc_dsl::validate(project).unwrap();
    oc_engine::render_cache::RenderCache::new()
        .render_full(
            project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap()
}

fn assert_same_render(label: &str, actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "{label}: lengths differ");
    let mut worst = 0.0_f64;
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        let error = (f64::from(*a) - f64::from(*b)).abs();
        if error > 1e-7 {
            panic!("{label}: sample {index}: {a} vs {b} (error {error})");
        }
        worst = worst.max(error);
    }
    assert!(worst <= 1e-7, "{label}: worst error {worst}");
}

#[test]
fn bus_usage_lists_track_outputs_sends_and_bus_routes() {
    let project = project(
        r#"@ocp "0.2.0"
@project "usage"
  tempo: 120
  time_signature: 4/4
@bus space
  routes: [master]
@bus glue
  routes: [space]
@track a | instrument: piano/acoustic_grand
  mix: output = space, send(space=50%)
@track b | instrument: piano/acoustic_grand
  mix: send(space=25%), send(glue=10%)
"#,
    );

    let usage = bus_usage(&project, "space").unwrap();
    assert_eq!(usage.track_outputs, vec![0]);
    assert_eq!(usage.sends, vec![0, 1]);
    assert_eq!(usage.bus_routes, vec!["glue".to_string()]);
    assert_eq!(usage.describe(), "1 track output, 2 sends, 1 bus route");

    let unused = bus_usage(&project, "glue").unwrap();
    assert_eq!(unused.track_outputs, Vec::<usize>::new());
    assert_eq!(unused.sends, vec![1]);
    assert_eq!(unused.bus_routes, Vec::<String>::new());

    assert_eq!(
        bus_usage(&project, "missing").unwrap_err(),
        "Bus 'missing' not found"
    );
}

#[test]
fn add_bus_is_one_undo_entry_and_round_trips_through_ocp() {
    let mut inner = test_state();
    inner.project = project(BASE);
    let old_project = inner.project.clone();

    let name = apply_add_bus(&mut inner.project, "glue", vec!["master".to_string()]).unwrap();
    assert_eq!(name, "glue");
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add bus", UndoScope::MixerRouting),
    )
    .expect("resync");

    assert_eq!(
        inner.undo.undo.len(),
        1,
        "one node addition, one undo entry"
    );
    assert_eq!(bus(&inner.project, "glue").routes, vec!["master"]);

    let reopened = oc_dsl::parse(&inner.dsl_source).unwrap();
    assert_eq!(reopened, inner.project, "the new bus survives save/load");

    inner.undo.undo(&mut inner.project).unwrap();
    assert!(inner.project.buses.iter().all(|bus| bus.name != "glue"));
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(bus(&inner.project, "glue").routes, vec!["master"]);
}

#[test]
fn deleting_a_referenced_bus_is_rejected_without_mutation() {
    let mut project = project(
        r#"@ocp "0.2.0"
@project "delete in use"
  tempo: 120
  time_signature: 4/4
@bus space
  routes: [master]
@bus glue
  routes: [space]
@track a | instrument: piano/acoustic_grand
  mix: output = space, send(space=50%)
"#,
    );
    let before = project.clone();

    let error = apply_remove_bus(&mut project, "space", false).unwrap_err();
    assert!(error.contains("in use"), "unexpected error: {error}");
    assert!(
        error.contains("1 track output, 1 send, 1 bus route"),
        "the error must name every reference: {error}"
    );
    assert_eq!(project, before, "a rejected delete mutates nothing");

    assert_eq!(
        apply_remove_bus(&mut project, "missing", true).unwrap_err(),
        "Bus 'missing' not found"
    );
}

#[test]
fn rerouted_delete_returns_outputs_to_master_and_drops_edges_into_the_bus() {
    let mut project = project(
        r#"@ocp "0.2.0"
@project "reroute"
  tempo: 120
  time_signature: 4/4
@bus space
  routes: [master]
@bus glue
  routes: [space]
@track a | instrument: piano/acoustic_grand
  mix: output = space, send(space=50%)
@track b | instrument: piano/acoustic_grand
  mix: send(space=25%), send(glue=10%)
"#,
    );

    apply_remove_bus(&mut project, "space", true).unwrap();

    assert!(project.buses.iter().all(|bus| bus.name != "space"));
    assert_eq!(
        project.tracks[0].mix.output, None,
        "main output returns to master"
    );
    assert!(
        project.tracks[0].mix.sends.is_empty(),
        "a send into the deleted bus is dropped, never left dangling"
    );
    assert_eq!(project.tracks[1].mix.sends.len(), 1);
    assert_eq!(project.tracks[1].mix.sends[0].target, "glue");
    assert_eq!(
        bus(&project, "glue").routes,
        vec!["master"],
        "a bus left with no route is written back to master"
    );

    // The explicit re-route is a complete project, so it serializes and reloads
    // without a dangling reference.
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened, project);
}

#[test]
fn set_send_target_moves_one_send_and_keeps_its_level_tap_and_pan() {
    let mut project = project(
        r#"@ocp "0.2.0"
@project "re-point"
  tempo: 120
  time_signature: 4/4
@bus space
  routes: [master]
@bus glue
  routes: [master]
@bus delay
  routes: [master]
@track a | instrument: piano/acoustic_grand
  mix: send(space=50%, pre, pan=-0.4), send(glue=20%)
"#,
    );

    // A second send from the same tap into the destination would double it.
    let error = apply_set_send_target(&mut project, 0, "space", "glue", None).unwrap_err();
    assert!(error.contains("already sends"), "unexpected error: {error}");
    assert_eq!(project.tracks[0].mix.sends[0].target, "space");

    assert!(apply_set_send_target(&mut project, 0, "space", "missing", None).is_err());
    assert!(apply_set_send_target(&mut project, 0, "missing", "glue", None).is_err());
    assert_eq!(
        project.tracks[0].mix.sends.len(),
        2,
        "failed moves mutate nothing"
    );

    apply_set_send_target(&mut project, 0, "space", "delay", None).unwrap();
    let moved = &project.tracks[0].mix.sends[0];
    assert_eq!(moved.target, "delay");
    assert!((moved.amount - 0.5).abs() < 1e-12, "level changed");
    assert!(moved.pre_fader, "tap changed");
    assert!((moved.pan + 0.4).abs() < 1e-12, "pan changed");
    assert_eq!(
        project.tracks[0].mix.sends[1].target, "glue",
        "the other send moved"
    );
}

#[test]
fn set_send_target_is_one_undo_entry_and_round_trips() {
    let mut inner = test_state();
    inner.project = project(
        r#"@ocp "0.2.0"
@project "re-point"
  tempo: 120
  time_signature: 4/4
@bus space
  routes: [master]
@bus glue
  routes: [master]
@track a | instrument: piano/acoustic_grand
  mix: send(space=50%, pre, pan=-0.4)
"#,
    );
    let old_project = inner.project.clone();
    apply_set_send_target(&mut inner.project, 0, "space", "glue", None).unwrap();
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("send routing", UndoScope::MixerRouting),
    )
    .expect("resync");

    assert_eq!(
        inner.undo.undo.len(),
        1,
        "one edge re-point, one undo entry"
    );
    let send = &inner.project.tracks[0].mix.sends[0];
    assert_eq!(send.target, "glue");
    assert!(send.pre_fader);
    assert!((send.pan + 0.4).abs() < 1e-12);

    let text = inner.dsl_source.clone();
    assert!(
        text.contains("send(glue=50%, pre, pan=-0.4)"),
        "serialized send lost its tap or pan:\n{text}"
    );
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(reopened, inner.project);

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.tracks[0].mix.sends[0].target, "space");
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(inner.project.tracks[0].mix.sends[0].target, "glue");
}

#[test]
fn output_port_edits_round_trip_and_stay_absent_at_port_zero() {
    let mut inner = test_state();
    inner.project = project(BASE);
    let old_project = inner.project.clone();

    apply_set_track_output_port(&mut inner.project, 0, 2).unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track output port", UndoScope::MixerRouting),
    )
    .expect("resync");
    assert_eq!(inner.project.tracks[0].output_port, Some(2));
    assert!(
        inner.dsl_source.contains("port: 2"),
        "output port was not written:\n{}",
        inner.dsl_source
    );

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project.tracks[0].output_port, None);

    apply_set_track_output_port(&mut inner.project, 0, 0).unwrap();
    assert_eq!(inner.project.tracks[0].output_port, None);
    assert!(apply_set_track_output_port(&mut inner.project, 0, 64).is_err());
}

#[test]
fn a_bus_addition_and_send_repoint_render_like_the_hand_written_project() {
    let mut inner = test_state();
    inner.project = project(BASE);
    let old_project = inner.project.clone();

    // The edit sequence the routing toolbar performs: add a node, move the
    // track's main output to it, re-point the existing send into it, then add a
    // pre-fader send at another level.
    apply_add_bus(&mut inner.project, "glue", vec!["master".to_string()]).unwrap();
    apply_set_track_output(&mut inner.project, 0, "glue").unwrap();
    apply_add_send(
        &mut inner.project,
        0,
        "space",
        0.25,
        Some(true),
        None,
        Some(-0.5),
    )
    .unwrap();
    apply_set_send_target(&mut inner.project, 0, "space", "glue", None).unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("routing edit", UndoScope::MixerRouting),
    )
    .expect("resync");

    let serialized = inner.dsl_source.clone();
    assert!(
        serialized.contains("output=glue"),
        "main output was not written:\n{serialized}"
    );
    assert!(
        serialized.contains("send(glue=25%, pre, pan=-0.5)"),
        "send re-point lost level, tap or pan:\n{serialized}"
    );

    let hand_written = project(
        r#"@ocp "0.2.0"

@project "hand written"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus space
  routes: [master]

@bus glue
  routes: [master]

@track a | preset: tone, instrument: synth/lead_square
  mix: output = glue, send(glue=25%, pre, pan=-0.5)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#,
    );
    let reopened = oc_dsl::parse(&serialized).unwrap();

    let edited_render = render(&inner.project);
    let reloaded_render = render(&reopened);
    let expected_render = render(&hand_written);
    assert_same_render("save/load", &reloaded_render, &edited_render);
    assert_same_render("hand-written routing", &expected_render, &edited_render);
}

#[test]
fn a_rerouted_delete_renders_like_the_hand_written_project() {
    let mut inner = test_state();
    inner.project = project(
        r#"@ocp "0.2.0"

@project "delete reroute"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus space
  routes: [master]

@bus glue
  routes: [space]

@track a | preset: tone, instrument: synth/lead_square
  mix: output = glue, send(space=50%)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#,
    );
    let old_project = inner.project.clone();
    apply_remove_bus(&mut inner.project, "space", true).unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove bus", UndoScope::MixerRouting),
    )
    .expect("resync");

    let serialized = inner.dsl_source.clone();
    assert!(
        serialized.contains("routes: [master]"),
        "the bus routed into the deleted bus must be written back to master:\n{serialized}"
    );
    assert!(
        !serialized.contains("send(space"),
        "a send into the deleted bus survived:\n{serialized}"
    );

    let hand_written = project(
        r#"@ocp "0.2.0"

@project "hand written"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus glue
  routes: [master]

@track a | preset: tone, instrument: synth/lead_square
  mix: output = glue
  @phrase a [1:1 - 2:1]
    C4 w mf
"#,
    );
    let reopened = oc_dsl::parse(&serialized).unwrap();
    assert_same_render("save/load", &render(&reopened), &render(&inner.project));
    assert_same_render(
        "hand-written reroute",
        &render(&hand_written),
        &render(&inner.project),
    );
}

const CONTROL_PROJECT: &str = r#"@project "control cable"
@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms
@track kick | instrument: drums/kick
@track bass | instrument: synth/bass
  effects:
    - compression: ratio=4:1
"#;

fn control_project(source: &str) -> ast::Project {
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

#[test]
fn a_control_route_command_connects_the_device_and_round_trips() {
    let mut project = control_project(CONTROL_PROJECT);
    apply_add_control_route(&mut project, "track:1", "compression", "kick_env").unwrap();
    let routes = &project.tracks[1].input_routes;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, ast::RouteKind::Control);
    assert_eq!(routes[0].source, "kick_env");
    assert_eq!(routes[0].target, "compression");
    oc_dsl::validate(&project).unwrap();

    let text = oc_dsl::serialize(&project);
    assert!(
        text.contains("route: control kick_env -> compression"),
        "{text}"
    );
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(project, reopened);

    apply_remove_control_route(&mut project, "track:1", "compression").unwrap();
    assert!(project.tracks[1].input_routes.is_empty());
    assert!(
        apply_remove_control_route(&mut project, "track:1", "compression").is_err(),
        "removing a missing control route is a readable error"
    );
}

#[test]
fn a_control_route_refuses_an_audio_only_device_and_leaves_the_project_alone() {
    let text = CONTROL_PROJECT.replace("- compression: ratio=4:1", "- eq: mid_freq=1kHz");
    let mut project = control_project(&text);
    let before = project.clone();
    let error = apply_add_control_route(&mut project, "track:1", "eq", "kick_env").unwrap_err();
    assert!(
        error.contains("does not accept a control signal"),
        "{error}"
    );
    assert!(error.contains("eq"), "{error}");
    assert_eq!(
        project, before,
        "a refused edit does not mutate the project"
    );
}

#[test]
fn a_control_route_refuses_its_own_feed_track_and_an_unknown_source() {
    let mut project = control_project(CONTROL_PROJECT);
    let error =
        apply_add_control_route(&mut project, "track:0", "compression", "kick_env").unwrap_err();
    assert!(error.contains("same track"), "{error}");
    let error =
        apply_add_control_route(&mut project, "track:1", "compression", "ghost").unwrap_err();
    assert!(error.contains("Unknown control source 'ghost'"), "{error}");
}

#[test]
fn a_second_control_route_to_the_same_device_replaces_the_first() {
    let mut project = control_project(
        &CONTROL_PROJECT.replace(
            "@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms",
            "@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms\n@control_source snare_env | from: track(kick), attack: 0ms, release: 50ms",
        ),
    );
    apply_add_control_route(&mut project, "track:1", "compression", "kick_env").unwrap();
    apply_add_control_route(&mut project, "track:1", "compression", "snare_env").unwrap();
    assert_eq!(project.tracks[1].input_routes.len(), 1);
    assert_eq!(project.tracks[1].input_routes[0].source, "snare_env");
}

#[test]
fn a_control_route_on_a_bus_uses_the_same_command() {
    let text = format!(
        "{CONTROL_PROJECT}\n@bus drums\n  routes: [master]\n  effects:\n    - compression: ratio=4:1\n"
    );
    let mut project = control_project(&text);
    apply_add_control_route(&mut project, "bus:drums", "compression", "kick_env").unwrap();
    assert_eq!(project.buses[0].input_routes.len(), 1);
    oc_dsl::validate(&project).unwrap();
}

#[test]
fn a_control_route_refuses_a_loop_between_two_tracks() {
    let text = r#"@project "control loop"
@control_source a_env | from: track(a), attack: 0ms, release: 100ms
@control_source b_env | from: track(b), attack: 0ms, release: 100ms
@track a | instrument: synth/lead
  effects:
    - compression: ratio=4:1
  route: control b_env -> compression
@track b | instrument: synth/bass
  effects:
    - compression: ratio=4:1
"#;
    let mut project = control_project(text);
    let error =
        apply_add_control_route(&mut project, "track:1", "compression", "a_env").unwrap_err();
    assert!(error.contains("close a loop"), "{error}");
    assert!(error.contains("a -> b"), "{error}");
}
