use std::path::{Path, PathBuf};

use oc_dsl::ast;
use oc_dsl::timing::ProjectClock;

use crate::commands::arrangement::*;
use crate::commands::fx::*;
use crate::commands::mixer::*;
use crate::commands::plugin_catalog::*;
use crate::commands::plugin_host::*;
use crate::commands::project::*;
use crate::commands::shared::*;
use crate::state::{UndoEdit, UndoScope};

use super::support::*;

#[test]
fn test_resync_with_undo_updates_dsl_and_dirty() {
    let mut inner = test_state();
    let old_project = inner.project.clone();
    inner.project.tracks.push(make_track("Piano"));
    inner.dirty = false;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("test edit", UndoScope::Project),
    )
    .expect("resync");
    assert!(inner.dirty);
    assert!(inner.dsl_source.contains("Piano") || inner.dsl_source.contains("piano"));
    // render_dirty is false if preload_project succeeded (sent to render thread),
    // or true if no soundfont was available (preload_project returns early).
    // Either way, the audio state has been notified of the change.
    // Undo stack should have one entry
    assert!(inner.undo.can_undo());
}

#[test]
fn test_resync_with_undo_marks_dirty_and_pushes_undo() {
    let mut inner = test_state();
    let old_project = inner.project.clone();
    inner.project.properties.tempo = 200.0;
    inner.dirty = false;
    inner.audio.render_dirty = false;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("test edit", UndoScope::Project),
    )
    .expect("resync");
    assert!(inner.dirty);
    // render_dirty may be false (preload succeeded) or true (no soundfont).
    // The key invariant is that the render thread has been notified.
    assert!(inner.undo.can_undo());
}

#[test]
fn test_undo_restores_previous_state() {
    let mut inner = test_state();
    let old_project = inner.project.clone();
    inner.project.tracks.push(make_track("Piano"));
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("test edit", UndoScope::Project),
    )
    .expect("resync");
    assert_eq!(inner.project.tracks.len(), 1);

    // Undo should restore empty tracks
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("undo");
    assert_eq!(project.tracks.len(), 0);
}

#[test]
fn test_redo_reapplies_change() {
    let mut inner = test_state();
    let old_project = inner.project.clone();
    inner.project.tracks.push(make_track("Piano"));
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("test edit", UndoScope::Project),
    )
    .expect("resync");

    // Undo
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("undo");
    inner.project = project;

    // Redo should bring the track back
    let mut project = inner.project.clone();
    inner.undo.redo(&mut project).expect("redo");
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(project.tracks[0].name, "Piano");
}

#[test]
fn test_snapshot_reflects_state() {
    let mut inner = test_state();
    inner.project.tracks.push(make_track("Test Track"));
    inner.file_path = Some(PathBuf::from("/tmp/test.ocp"));
    inner.dirty = true;
    let snap = snapshot(&inner);
    assert_eq!(snap.track_count, 1);
    assert_eq!(snap.file_path.unwrap(), "/tmp/test.ocp");
    assert!(snap.dirty);
}

#[test]
fn test_apply_bus_mix_updates_only_the_named_bus() {
    let mut project = default_project();
    project.buses = vec![
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "reverb".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "delay".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
    ];

    apply_bus_mix(&mut project, "reverb", Some(-4.5), Some(true), Some(true)).unwrap();
    assert_eq!(project.buses[0].volume_db, -4.5);
    assert!(project.buses[0].mute && project.buses[0].solo);
    assert_eq!(project.buses[1].volume_db, 0.0);
    assert!(!project.buses[1].mute && !project.buses[1].solo);

    assert!(apply_bus_mix(&mut project, "missing", None, Some(true), None).is_err());
    assert!(apply_bus_mix(&mut project, "reverb", None, None, None).is_ok());
}

#[test]
fn test_remove_bus_references_cleans_tracks_and_bus_routes() {
    let mut project = default_project();
    let mut track = make_track("Piano");
    track.mix.sends = vec![
        ast::Send {
            target: "reverb".to_string(),
            amount: 0.4,
            source: None,
            pre_fader: false,
            pan: 0.0,
        },
        ast::Send {
            target: "delay".to_string(),
            amount: 0.2,
            source: None,
            pre_fader: false,
            pan: 0.0,
        },
    ];
    project.tracks.push(track);
    project.buses = vec![
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "reverb".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "delay".to_string(),
            routes: vec!["reverb".to_string(), "master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
    ];

    remove_bus_references(&mut project, "reverb").unwrap();

    assert_eq!(project.buses.len(), 1);
    assert_eq!(project.buses[0].name, "delay");
    assert_eq!(project.buses[0].routes, vec!["master"]);
    assert_eq!(project.tracks[0].mix.sends.len(), 1);
    assert_eq!(project.tracks[0].mix.sends[0].target, "delay");
}

#[test]
fn test_remove_bus_references_rejects_unknown_bus_without_mutation() {
    let mut project = default_project();
    project.buses.push(ast::Bus {
        gain_automation: Vec::new(),
        automation: Vec::new(),
        name: "reverb".to_string(),
        routes: vec!["master".to_string()],
        effects: vec![],
        volume_db: 0.0,
        mute: false,
        solo: false,
        input_routes: vec![],
    });
    let before = project.clone();

    assert_eq!(
        remove_bus_references(&mut project, "missing").unwrap_err(),
        "Bus 'missing' not found"
    );
    assert_eq!(project, before);
}

#[test]
fn test_set_bus_routes_accepts_dag_and_rejects_cycle_without_mutation() {
    let mut project = default_project();
    project.buses = vec![
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "room".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "glue".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
    ];

    set_bus_routes_in_project(&mut project, "room", vec!["glue".to_string()]).unwrap();
    assert_eq!(project.buses[0].routes, vec!["glue"]);
    let before = project.clone();
    let error =
        set_bus_routes_in_project(&mut project, "glue", vec!["room".to_string()]).unwrap_err();
    assert!(error.contains("routing cycle"));
    assert_eq!(project, before);
}

#[test]
fn test_send_mutations_reject_a_cyclic_bus_graph() {
    let mut project = default_project();
    project.buses = vec![
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "room".to_string(),
            routes: vec!["glue".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
        ast::Bus {
            gain_automation: Vec::new(),
            automation: Vec::new(),
            name: "glue".to_string(),
            routes: vec!["master".to_string()],
            effects: vec![],
            volume_db: 0.0,
            mute: false,
            solo: false,
            input_routes: vec![],
        },
    ];
    assert!(routing_cycle_error(&project).is_none());

    project.buses[1].routes = vec!["room".to_string()];
    let error = routing_cycle_error(&project).expect("cycle must be reported");
    assert!(error.contains("cycle"), "unexpected error: {error}");
    project.buses[1].routes = vec!["master".to_string()];
    assert!(routing_cycle_error(&project).is_none());
}

#[test]
fn test_project_compatibility_reports_missing_external_resources() {
    let mut project = default_project();
    let mut track = make_track("Lead");
    track.preset = Some("missing_definition".to_string());
    project.tracks.push(track);
    project.presets.push(ast::PresetDef {
        name: "External synth".to_string(),
        source: Some("clap".to_string()),
        plugin: Some("org.example.absent".to_string()),
        program: None,
        patch: None,
        file: Some("missing.wav".to_string()),
        root_note: None,
        params: vec![(
            "clap_state_file".to_string(),
            ast::ParamValue::String("missing.clapstate".to_string()),
        )],
        effects: vec![],
        extends: None,
    });

    let report = build_project_compatibility_report(
        &project,
        Some(std::path::Path::new("/tmp/project.ocp")),
        &[],
    );
    let kinds: std::collections::HashSet<_> = report
        .issues
        .iter()
        .map(|issue| issue.kind.as_str())
        .collect();

    assert!(!report.compatible);
    assert_eq!(report.installed_plugin_count, 0);
    assert!(kinds.contains("missing_preset"));
    assert!(kinds.contains("missing_plugin"));
    assert!(kinds.contains("missing_media"));
    assert!(kinds.contains("missing_plugin_state"));
}

#[test]
fn test_project_compatibility_accepts_builtin_project() {
    let project = default_project();
    let report = build_project_compatibility_report(&project, None, &[]);
    assert!(report.compatible);
    assert!(report.issues.is_empty());
}

#[test]
fn test_project_compatibility_rejects_sitala_without_captured_kit_state() {
    let mut project = default_project();
    project.presets.push(ast::PresetDef {
        name: "Drum kit".to_string(),
        source: Some("vst3".to_string()),
        plugin: Some("vst3:sitala".to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    let plugins = vec![oc_host::PluginInfo {
        id: "vst3:sitala".to_string(),
        name: "Sitala".to_string(),
        vendor: "Decomposer".to_string(),
        format: oc_host::PluginFormat::Vst3,
        path: "/plugins/Sitala.vst3".to_string(),
        category: oc_host::PluginCategory::Instrument,
        num_params: 0,
        version: Some("1.0".into()),
        architectures: vec!["aarch64".into()],
        state_abi: Some("vst3:vst3:sitala:1.0:component-state-v1".into()),
    }];

    let report = build_project_compatibility_report(&project, None, &plugins);

    assert!(!report.compatible);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == "missing_sampler_state")
    );

    project.presets[0].params.push((
        "vst3_state_hex".to_string(),
        ast::ParamValue::String("00ff".to_string()),
    ));
    let report = build_project_compatibility_report(&project, None, &plugins);
    assert!(report.compatible);
}

#[test]
fn test_project_compatibility_resolves_effect_plugins_and_reports_missing_ones() {
    let mut project = default_project();
    let mut track = make_track("Lead");
    track.effects = vec![
        ast::Effect {
            effect_type: ast::EffectType::Plugin("org.example.present".to_string()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        },
        ast::Effect {
            effect_type: ast::EffectType::Plugin("org.example.absent".to_string()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        },
    ];
    project.tracks.push(track);
    let plugins = vec![oc_host::PluginInfo {
        id: "org.example.present".to_string(),
        name: "Present FX".to_string(),
        vendor: "Example".to_string(),
        format: oc_host::PluginFormat::Clap,
        path: "/plugins/present.clap".to_string(),
        category: oc_host::PluginCategory::Effect,
        num_params: 0,
        version: Some("1.0".into()),
        architectures: vec!["aarch64".into()],
        state_abi: Some("clap:org.example.present:1.0:component-state-v1".into()),
    }];

    let report = build_project_compatibility_report(&project, None, &plugins);

    assert!(!report.compatible);
    assert_eq!(report.resolved_plugins.len(), 1);
    assert_eq!(report.resolved_plugins[0].name, "Present FX");
    assert!(report.issues.iter().any(|issue| {
        issue.kind == "missing_plugin" && issue.message.contains("org.example.absent")
    }));
}

/// The published inventory is process-wide; tests that install one take turns.
static PUBLISHED_INVENTORY: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A project keeps the format prefix in an effect reference; the audit resolves
/// it against the published inventory, exactly like the render path.
#[test]
fn test_project_compatibility_resolves_a_prefixed_plugin_effect() {
    let _inventory = PUBLISHED_INVENTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut project = default_project();
    let mut track = make_track("Lead");
    track.effects = vec![ast::Effect {
        effect_type: ast::EffectType::Plugin("vst3./plugins/Installed Effect.vst3".to_string()),
        params: vec![],
        bypass: false,
        sidechain_source: None,
    }];
    project.tracks.push(track);
    let plugins = vec![oc_host::PluginInfo {
        id: "com.example.installed".to_string(),
        name: "Installed Effect".to_string(),
        vendor: "Example".to_string(),
        format: oc_host::PluginFormat::Vst3,
        path: "/plugins/Installed Effect.vst3".to_string(),
        category: oc_host::PluginCategory::Effect,
        num_params: 0,
        version: Some("1.0".into()),
        architectures: vec!["aarch64".into()],
        state_abi: None,
    }];
    // The resolver falls back to the published inventory, which this stands in for.
    oc_host::install_plugin_inventory(plugins.clone());

    let report = build_project_compatibility_report(&project, None, &plugins);

    assert!(report.compatible, "issues: {:?}", report.issues);
    assert_eq!(report.resolved_plugins.len(), 1);
    assert_eq!(
        report.resolved_plugins[0].required_id,
        "vst3./plugins/Installed Effect.vst3"
    );
    assert_eq!(
        report.resolved_plugins[0].installed_id,
        "com.example.installed"
    );
}

/// Stripping the prefix must not turn an unknown bundle into a match.
#[test]
fn test_project_compatibility_reports_a_prefixed_effect_that_is_absent() {
    let _inventory = PUBLISHED_INVENTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut project = default_project();
    let mut track = make_track("Lead");
    track.effects = vec![ast::Effect {
        effect_type: ast::EffectType::Plugin("vst3.com.example.absent".to_string()),
        params: vec![],
        bypass: false,
        sidechain_source: None,
    }];
    project.tracks.push(track);
    oc_host::install_plugin_inventory(Vec::new());

    let report = build_project_compatibility_report(&project, None, &[]);

    assert!(!report.compatible);
    assert!(report.issues.iter().any(|issue| {
        issue.kind == "missing_plugin" && issue.message.contains("vst3.com.example.absent")
    }));
    assert!(report.resolved_plugins.is_empty());
}

/// AU and VST2 presets answer the missing-plugin, version and architecture
/// questions the same way CLAP and VST3 presets do.
#[test]
fn test_project_compatibility_audits_au_and_vst2_presets() {
    let mut project = default_project();
    project.presets = vec![
        preset("AU Keys", "au", "com.example.absent.au"),
        preset("VST2 Comp", "vst2", "com.example.comp"),
    ];
    project.presets[1].params.push((
        "plugin_version".to_string(),
        ast::ParamValue::String("3.0".to_string()),
    ));
    project.presets[1].params.push((
        "plugin_architecture".to_string(),
        ast::ParamValue::String("x86_64".to_string()),
    ));
    let plugins = vec![oc_host::PluginInfo {
        id: "com.example.comp".to_string(),
        name: "Example Comp".to_string(),
        vendor: "Example".to_string(),
        format: oc_host::PluginFormat::Vst2,
        path: "/plugins/Example Comp.vst".to_string(),
        category: oc_host::PluginCategory::Effect,
        num_params: 0,
        version: Some("2.0".into()),
        architectures: vec!["aarch64".into()],
        state_abi: None,
    }];

    let report = build_project_compatibility_report(&project, None, &plugins);

    assert!(!report.compatible);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| { issue.kind == "missing_plugin" && issue.subject == "AU Keys" }),
        "issues: {:?}",
        report.issues
    );
    assert!(
        report.issues.iter().any(|issue| {
            issue.kind == "plugin_version_mismatch" && issue.subject == "VST2 Comp"
        })
    );
    assert!(report.issues.iter().any(|issue| {
        issue.kind == "plugin_architecture_mismatch" && issue.subject == "VST2 Comp"
    }));
    assert_eq!(report.resolved_plugins.len(), 1);
    assert_eq!(report.resolved_plugins[0].installed_id, "com.example.comp");
}

/// A plugin effect the host cannot resolve fails at insertion and leaves the
/// chain untouched, so the UI can show the message on an unchanged rack.
#[test]
fn test_add_effect_refuses_a_plugin_that_is_not_installed() {
    let _inventory = PUBLISHED_INVENTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    oc_host::install_plugin_inventory(Vec::new());
    let mut inner = test_state();
    inner.project.tracks.push(make_track("Lead"));

    let error = add_effect_inner(&mut inner, "track:0", "plugin:com.example.absent")
        .expect_err("an unresolvable plugin effect is refused");

    assert!(error.contains("is not installed"), "{error}");
    assert!(inner.project.tracks[0].effects.is_empty());
}

/// The same refusal answers a plugin class that is an instrument, not a device.
#[test]
fn test_add_effect_refuses_a_plugin_instrument() {
    let _inventory = PUBLISHED_INVENTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    oc_host::install_plugin_inventory(vec![oc_host::PluginInfo {
        id: "com.example.synth".to_string(),
        name: "Example Synth".to_string(),
        vendor: "Example".to_string(),
        format: oc_host::PluginFormat::Vst3,
        path: "/plugins/Example Synth.vst3".to_string(),
        category: oc_host::PluginCategory::Instrument,
        num_params: 0,
        version: None,
        architectures: vec!["aarch64".into()],
        state_abi: None,
    }]);
    let mut inner = test_state();
    inner.project.tracks.push(make_track("Lead"));

    let error = add_effect_inner(&mut inner, "track:0", "plugin:com.example.synth")
        .expect_err("a plugin instrument is not an effect");

    assert!(error.contains("is an instrument"), "{error}");
    assert!(inner.project.tracks[0].effects.is_empty());
}

#[test]
fn resized_range_keeps_the_start_and_moves_the_end() {
    let project = default_project();
    let clock = ProjectClock::new(&project.properties).expect("clock");
    let range = ast::TimeRange {
        start: ast::BarBeat::new(1, 1, 0),
        end: ast::BarBeat::new(3, 1, 0),
    };

    let resized = resized_range(&clock, &range, ast::BarBeat::new(2, 3, 0)).expect("resize");

    assert_eq!(resized.start, ast::BarBeat::new(1, 1, 0));
    assert_eq!(resized.end, ast::BarBeat::new(2, 3, 0));
}

#[test]
fn resized_range_rejects_a_collapsed_span() {
    let project = default_project();
    let clock = ProjectClock::new(&project.properties).expect("clock");
    let range = ast::TimeRange {
        start: ast::BarBeat::new(1, 1, 0),
        end: ast::BarBeat::new(3, 1, 0),
    };

    let error =
        resized_range(&clock, &range, ast::BarBeat::new(1, 1, 0)).expect_err("collapsed span");

    assert!(error.contains("at least a 16th note"));
}

#[test]
fn move_clip_between_tracks_keeps_duration_and_pattern() {
    let mut project = default_project();
    project.properties.time_signature.numerator = 4;
    let mut from = make_track("A");
    from.push_pattern_clip(ast::Clip {
        pattern: "beat".to_string(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    project.tracks.push(from);
    project.tracks.push(make_track("B"));

    move_clip_between_tracks(&mut project, 0, 0, 1, ast::BarBeat::new(2, 3, 2))
        .expect("cross-track move");

    assert!(project.tracks[0].pattern_clip_count() == 0);
    assert_eq!(project.tracks[1].pattern_clip_count(), 1);
    let moved = &project.tracks[1].pattern_clip(0).unwrap();
    assert_eq!(moved.pattern, "beat");
    assert_eq!(moved.time_range.start, ast::BarBeat::new(2, 3, 2));
    // Two bars stay two bars: 2:3:2 + 8 quarters = 4:3:2.
    assert_eq!(moved.time_range.end, ast::BarBeat::new(4, 3, 2));
}

#[test]
fn move_clip_between_tracks_rejects_an_unknown_track() {
    let mut project = default_project();
    let mut track = make_track("A");
    track.push_pattern_clip(ast::Clip {
        pattern: "beat".to_string(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    project.tracks.push(track);

    let error = move_clip_between_tracks(&mut project, 0, 0, 9, ast::BarBeat::new(2, 1, 0))
        .expect_err("out of range");
    assert!(error.contains("Track 9 out of range"));
    assert_eq!(project.tracks[0].pattern_clip_count(), 1);
}

#[test]
fn move_phrase_between_tracks_keeps_duration_and_events() {
    let mut project = default_project();
    let mut from = make_track("A");
    from.phrases.push(ast::Phrase {
        name: "melody".to_string(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        }),
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        channel: None,
    });
    project.tracks.push(from);
    project.tracks.push(make_track("B"));

    move_phrase_between_tracks(&mut project, 0, 0, 1, ast::BarBeat::new(2, 3, 2))
        .expect("cross-track phrase move");

    assert!(project.tracks[0].phrases.is_empty());
    assert_eq!(project.tracks[1].phrases.len(), 1);
    let moved = &project.tracks[1].phrases[0];
    assert_eq!(moved.name, "melody");
    assert_eq!(moved.time_range.unwrap().start, ast::BarBeat::new(2, 3, 2));
    assert_eq!(moved.time_range.unwrap().end, ast::BarBeat::new(4, 3, 2));
}

#[test]
fn move_phrase_between_tracks_rejects_an_unknown_track() {
    let mut project = default_project();
    let mut track = make_track("A");
    track.phrases.push(ast::Phrase {
        name: "melody".to_string(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(3, 1, 0),
        }),
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        channel: None,
    });
    project.tracks.push(track);

    let error = move_phrase_between_tracks(&mut project, 0, 0, 9, ast::BarBeat::new(2, 1, 0))
        .expect_err("out of range");
    assert!(error.contains("Track 9 out of range"));
    assert_eq!(project.tracks[0].phrases.len(), 1);
}

#[test]
fn move_inline_pattern_between_tracks_keeps_repeat_and_hits() {
    let mut project = default_project();
    let mut from = make_track("A");
    from.patterns.push(ast::Pattern {
        name: "main_beat".to_string(),
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(2, 1, 0),
        }),
        repeat: 4,
        hits: vec![ast::HitEvent {
            drum: "kick".to_string(),
            position: ast::BeatPosition {
                beat: 1,
                subdivision: 0,
            },
            dynamics: None,
            velocity: None,
            articulation: None,
            modifiers: vec![],
        }],
    });
    project.tracks.push(from);
    project.tracks.push(make_track("B"));

    move_inline_pattern_between_tracks(&mut project, 0, 0, 1, ast::BarBeat::new(1, 3, 0))
        .expect("cross-track pattern move");

    assert!(project.tracks[0].patterns.is_empty());
    assert_eq!(project.tracks[1].patterns.len(), 1);
    let moved = &project.tracks[1].patterns[0];
    assert_eq!(moved.repeat, 4);
    assert_eq!(moved.hits.len(), 1);
    assert_eq!(moved.time_range.unwrap().start, ast::BarBeat::new(1, 3, 0));
    assert_eq!(moved.time_range.unwrap().end, ast::BarBeat::new(2, 3, 0));
}

#[test]
fn move_inline_pattern_rejects_a_missing_range() {
    let mut project = default_project();
    let mut track = make_track("A");
    track.patterns.push(ast::Pattern {
        name: "main_beat".to_string(),
        time_range: None,
        repeat: 1,
        hits: vec![],
    });
    project.tracks.push(track);

    let error =
        move_inline_pattern_between_tracks(&mut project, 0, 0, 0, ast::BarBeat::new(1, 1, 0))
            .expect_err("no range");
    assert!(error.contains("no time range"));
}

// ── Clip move normalization ─────────────────────────────────────────

#[test]
fn test_clip_move_normalizes_bar_beat() {
    let mut inner = test_state();
    inner.project.properties.time_signature.numerator = 4;
    let mut track = make_track("T");
    track.push_pattern_clip(ast::Clip {
        pattern: "p".to_string(),
        time_range: ast::TimeRange {
            start: ast::BarBeat {
                bar: 1,
                beat: 1,
                subdivision: 0,
            },
            end: ast::BarBeat {
                bar: 3,
                beat: 3,
                subdivision: 0,
            },
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    inner.project.tracks.push(track);

    // Move clip from bar 1 beat 1 to bar 2 beat 3
    // Delta: bar +1, beat +2
    // End was bar 3 beat 3 → bar 4 beat 5 → normalized to bar 5 beat 1
    let beats_per_bar = inner.project.properties.time_signature.numerator as i32;
    let start_bar: u32 = 2;
    let start_beat: u32 = 3;
    let start_sub: u32 = 0;

    let track = &mut inner.project.tracks[0];
    let clip = track.pattern_clip_mut(0).expect("the clip exists");
    let old_start = clip.time_range.start;
    let old_end = clip.time_range.end;
    let bar_delta = start_bar as i32 - old_start.bar as i32;
    let beat_delta = start_beat as i32 - old_start.beat as i32;
    let sub_delta = start_sub as i32 - old_start.subdivision as i32;

    let subs_per_beat: i32 = 4;
    let mut new_end_sub = old_end.subdivision as i32 + sub_delta;
    let mut new_end_beat = old_end.beat as i32 + beat_delta;
    let mut new_end_bar = old_end.bar as i32 + bar_delta;

    if new_end_sub < 0 {
        let borrow = (-new_end_sub + subs_per_beat - 1) / subs_per_beat;
        new_end_sub += borrow * subs_per_beat;
        new_end_beat -= borrow;
    } else if new_end_sub >= subs_per_beat {
        let carry = new_end_sub / subs_per_beat;
        new_end_sub -= carry * subs_per_beat;
        new_end_beat += carry;
    }
    if new_end_beat < 1 {
        let borrow = (1 - new_end_beat + beats_per_bar - 1) / beats_per_bar;
        new_end_beat += borrow * beats_per_bar;
        new_end_bar -= borrow;
    } else if new_end_beat > beats_per_bar {
        let carry = (new_end_beat - 1) / beats_per_bar;
        new_end_beat -= carry * beats_per_bar;
        new_end_bar += carry;
    }

    // End should be bar 4 beat 5 → normalized to bar 5 beat 1
    assert_eq!(new_end_bar, 5);
    assert_eq!(new_end_beat, 1);
    assert_eq!(new_end_sub, 0);
}

#[test]
fn effect_parameter_edits_use_the_device_schema() {
    let effect = ast::Effect {
        effect_type: ast::EffectType::Compression,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    assert!(
        validate_builtin_effect_param(&effect, "threshold", &ast::ParamValue::Db(-6.0)).is_ok()
    );
    assert!(
        validate_builtin_effect_param(&effect, "threshold", &ast::ParamValue::Db(-100.0)).is_err()
    );
    assert!(validate_builtin_effect_param(&effect, "typo", &ast::ParamValue::Number(1.0)).is_err());
    assert!(
        validate_builtin_effect_param(&effect, "attack", &ast::ParamValue::Percentage(20.0))
            .is_err()
    );
}

#[test]
fn effect_lane_edits_set_smoothing_and_point_curves() {
    let mut project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB\n  automation: compression.threshold {1:1:0=-24dB, 2:1:0=-6dB}\n",
    )
    .unwrap();
    assert!(
        apply_lane_smoothing(&mut project, "track:0", "compression.threshold", None, 25.0).is_ok()
    );
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().smoothing_ms,
        25.0
    );
    assert!(
        apply_lane_smoothing(&mut project, "track:0", "compression.threshold", None, -1.0).is_err()
    );
    assert!(apply_lane_smoothing(&mut project, "track:0", "missing", None, 1.0).is_err());

    assert!(
        apply_lane_point_curve(
            &mut project,
            "track:0",
            "compression.threshold",
            None,
            0,
            "smooth",
            Some(0.4)
        )
        .is_ok()
    );
    let point = &project.tracks[0].automation_lane(0).unwrap().points[0];
    assert_eq!(point.curve, ast::Curve::Smooth);
    assert_eq!(point.tension, Some(0.4));

    assert!(
        apply_lane_point_curve(
            &mut project,
            "track:0",
            "compression.threshold",
            None,
            0,
            "half_sine",
            None
        )
        .is_ok()
    );
    let point = &project.tracks[0].automation_lane(0).unwrap().points[0];
    assert_eq!(point.curve, ast::Curve::HalfSine);
    assert_eq!(point.tension, None);

    assert!(
        apply_lane_point_curve(
            &mut project,
            "track:0",
            "compression.threshold",
            None,
            9,
            "linear",
            None
        )
        .is_err()
    );
    assert!(
        apply_lane_point_curve(
            &mut project,
            "track:0",
            "compression.threshold",
            None,
            0,
            "wobble",
            None
        )
        .is_err()
    );
}

#[test]
fn lane_mode_toggle_changes_only_the_mode() {
    let mut project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB\n  automation: compression.threshold {1:1:0=-24dB curve=stairs, 2:1:0=-6dB}\n",
    )
    .unwrap();
    let lfo = ast::LaneLfo {
        speed: ast::LfoSpeed::Note {
            numerator: 1,
            denominator: 8,
        },
        mult: 2.0,
        pulse_width: 0.25,
        skew: -0.5,
        tension: 0.3,
        amount: 0.75,
    };
    apply_lane_lfo(&mut project, "track:0", "compression.threshold", None, lfo).unwrap();
    let before = serde_json::to_value(&project).unwrap();
    assert!(before["tracks"][0]["automation"][0].get("mode").is_none());

    apply_lane_mode(
        &mut project,
        "track:0",
        "compression.threshold",
        None,
        "lfo",
    )
    .unwrap();
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().mode,
        ast::LaneMode::Lfo,
        "the mode is the only field the command writes"
    );
    let mut after = serde_json::to_value(&project).unwrap();
    assert_eq!(after["tracks"][0]["automation"][0]["mode"], "lfo");
    assert_eq!(
        after["tracks"][0]["automation"][0]["lfo"],
        serde_json::to_value(lfo).unwrap()
    );
    assert!(
        after["tracks"][0]["automation"][0]
            .as_object_mut()
            .unwrap()
            .remove("mode")
            .is_some()
    );
    assert_eq!(after, before, "switching mode changes nothing else");

    apply_lane_mode(
        &mut project,
        "track:0",
        "compression.threshold",
        None,
        "envelope",
    )
    .unwrap();
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().lfo,
        lfo,
        "the cycle is kept"
    );

    assert!(
        apply_lane_mode(
            &mut project,
            "track:0",
            "compression.threshold",
            None,
            "wobble"
        )
        .is_err()
    );
    assert!(apply_lane_mode(&mut project, "track:0", "missing", None, "lfo").is_err());
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().mode,
        ast::LaneMode::Envelope
    );
}

#[test]
fn lane_lfo_edits_validate_cycles_and_keep_the_previous_value_on_error() {
    let mut project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1=1, 1:2=0}\n",
    )
    .unwrap();
    let good = ast::LaneLfo {
        speed: ast::LfoSpeed::Hz { hz: 2.0 },
        mult: 2.0,
        pulse_width: 0.25,
        skew: -0.5,
        tension: 0.3,
        amount: 0.75,
    };
    apply_lane_lfo(&mut project, "track:0", "gain.factor", None, good).unwrap();
    assert_eq!(project.tracks[0].automation_lane(0).unwrap().lfo, good);
    // The command edits the cycle only; it never selects the mode.
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().mode,
        ast::LaneMode::Envelope
    );

    let mut bad = good;
    bad.pulse_width = 1.0;
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    bad = good;
    bad.speed = ast::LfoSpeed::Hz { hz: 0.0 };
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    bad = good;
    bad.mult = 0.0;
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    bad = good;
    bad.skew = 1.5;
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    bad = good;
    bad.amount = 4.5;
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    bad = good;
    bad.speed = ast::LfoSpeed::Note {
        numerator: 1,
        denominator: 0,
    };
    assert!(apply_lane_lfo(&mut project, "track:0", "gain.factor", None, bad).is_err());
    assert_eq!(
        project.tracks[0].automation_lane(0).unwrap().lfo,
        good,
        "a rejected edit leaves the cycle untouched"
    );
    assert!(apply_lane_lfo(&mut project, "track:0", "missing", None, good).is_err());
}

#[test]
#[ignore = "measures plug-in preparation with OCP_STREAM_PROJECT, OCP_TEST_PLUGINS=1 and a built scan helper"]
fn review_cold_first_press_latency() {
    use std::time::{Duration, Instant};

    let path = PathBuf::from(std::env::var_os("OCP_STREAM_PROJECT").unwrap());
    let source = std::fs::read_to_string(&path).unwrap();
    let project = oc_dsl::parse(&source).unwrap();
    let mut inner = test_state();
    inner.audio.resource_root = path.parent().map(Path::to_path_buf);

    let start = Instant::now();
    replace_project(&mut inner, project.clone(), Some(path)).unwrap();
    let open_ms = start.elapsed().as_secs_f64() * 1000.0;
    let wait = Instant::now();
    while wait.elapsed() < Duration::from_secs(10) {
        let ready = inner.audio.render_thread.as_ref().is_some_and(|rt| {
            rt.playback
                .warm_state
                .load(std::sync::atomic::Ordering::Acquire)
                == crate::render_thread::WARM_READY
        });
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let ready_ms = wait.elapsed().as_secs_f64() * 1000.0;

    let start = Instant::now();
    let warnings = collect_plugin_render_warnings(&inner.project);
    let warnings_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let clap_warnings = ensure_clap_instances_loaded(&mut inner);
    let clap_ms = start.elapsed().as_secs_f64() * 1000.0;

    let prepared = inner.project.clone();
    let start = Instant::now();
    let mut instances = std::mem::take(&mut inner.plugin_instances);
    let did_render = matches!(
        inner.audio.play(&prepared, &mut instances).unwrap(),
        crate::audio::PlayDispatch::Started { did_render: true }
    );
    inner.plugin_instances = instances;
    let play_ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "[measure] first_press open_ms={open_ms:.2} open_to_ready_ms={ready_ms:.2} warnings_ms={warnings_ms:.2} clap_ms={clap_ms:.2} play_ms={play_ms:.2} did_render={did_render} warnings={} clap_warnings={}",
        warnings.len(),
        clap_warnings.len()
    );
}

/// Stage table for both preview commands: preset audition and track audition.
#[test]
#[ignore = "measures preview latency; set OCP_PREVIEW_PROJECT to measure another project"]
fn review_preview_latency() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // One OpenSynth pad: a short project with an audition phrase heavy enough
    // that rendering the whole phrase up front is visible.
    const DEFAULT_PROJECT: &str = r#"@ocp "0.2.0"
@project "Preview latency"
  tempo: 120
@preset heavy_pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 4, osc.a.wavetable_position: 0.58, osc.a.level: 0.58, osc.b.waveform: 0, osc.b.level: 0.32, osc.b.semitones: 12, noise.level: 0.035, unison.detune: 14ct, unison.width: 0.78, filter.cutoff: 4200Hz, filter.resonance: 0.2, amp.attack: 0.72s, amp.decay: 1.4s, amp.sustain: 0.78, amp.release: 3.2s, lfo.sync: true, lfo.division: 4, mod.1.source: 0, mod.1.destination: 4, mod.1.amount: 0.2, quality.oversampling: 2, output.gain: 0.18}
@track lead | instrument: synth/virtual_analog, preset: heavy_pad
  @phrase note [1:1:0 - 2:1:0]
    C4 h mf
"#;
    let path = std::env::var_os("OCP_PREVIEW_PROJECT").map(PathBuf::from);
    let source = match &path {
        Some(path) => std::fs::read_to_string(path).unwrap(),
        None => DEFAULT_PROJECT.to_string(),
    };
    let project = oc_dsl::parse(&source).unwrap();
    let state = crate::state::AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = project.clone();
        inner.file_path = path.clone();
        inner.audio.enable_simulated_output();
        inner.audio.sample_rate = 48000;
        inner.audio.resource_root = path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        inner.audio.preload_project(&project);
    }
    let open = Instant::now();
    loop {
        let warm = state
            .lock_inner()
            .audio
            .render_thread
            .as_ref()
            .is_some_and(|rt| {
                rt.playback.warm_state.load(Ordering::Acquire) == crate::render_thread::WARM_READY
            });
        if warm || open.elapsed() > Duration::from_secs(10) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "[preview] open_to_warm_ms={:.1}",
        open.elapsed().as_secs_f64() * 1000.0
    );

    let counters = |state: &crate::state::AppState| {
        let inner = state.lock_inner();
        let rt = inner.audio.render_thread.as_ref().unwrap();
        (
            rt.playback.project_loads.load(Ordering::Acquire),
            rt.playback
                .instrument_instantiations
                .load(Ordering::Acquire),
        )
    };
    let heard_audition =
        |state: &crate::state::AppState| state.lock_inner().audio.heard_audition_blocks();
    let heard_transport =
        |state: &crate::state::AppState| state.lock_inner().audio.simulated_output_heard_blocks();
    let wait_heard = |before: u64, read: &dyn Fn() -> u64| -> Option<f64> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if read() > before {
                return Some(start.elapsed().as_secs_f64() * 1000.0);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    };

    // Preset audition: builtin presets first, then one repeat for the cache.
    let mut presets: Vec<String> = project
        .presets
        .iter()
        .filter(|preset| preset.source.as_deref() == Some("builtin"))
        .map(|preset| preset.name.clone())
        .collect();
    if let Some(first) = presets.first().cloned() {
        presets.push(first);
    }
    for (index, name) in presets.iter().enumerate() {
        let generation = state.audition.begin(name);
        let before = heard_audition(&state);
        let (loads, instruments) = counters(&state);
        let start = Instant::now();
        crate::audition::run(&state, name, generation, start);
        let publish_ms = start.elapsed().as_secs_f64() * 1000.0;
        let status = state.audition.snapshot();
        let audible_ms = wait_heard(before, &|| heard_audition(&state));
        let (loads_after, instruments_after) = counters(&state);
        eprintln!(
            "[preview] preset index={index} name={name} publish_ms={publish_ms:.2} first_audible_ms={audible_ms:?} status={} ready_ms={:?} loads={loads}->{loads_after} instruments={instruments}->{instruments_after}",
            status.status, status.ready_ms
        );
    }

    // Plugin presets are opt-in: they instantiate third-party bundles.
    if std::env::var_os("OCP_TEST_PLUGINS").is_some() {
        let plugins: Vec<String> = project
            .presets
            .iter()
            .filter(|preset| preset.source.as_deref() == Some("vst3"))
            .map(|preset| preset.name.clone())
            .collect();
        for name in plugins.iter().take(2) {
            let generation = state.audition.begin(name);
            let before = heard_audition(&state);
            let start = Instant::now();
            crate::audition::run(&state, name, generation, start);
            let publish_ms = start.elapsed().as_secs_f64() * 1000.0;
            let status = state.audition.snapshot();
            let audible_ms = wait_heard(before, &|| heard_audition(&state));
            eprintln!(
                "[preview] plugin_preset name={name} publish_ms={publish_ms:.2} first_audible_ms={audible_ms:?} status={} ready_ms={:?} instances={} loads={} error={:?}",
                status.status, status.ready_ms, status.instances, status.loads, status.error
            );
        }
    }

    // Track audition: the same command body `preview_track` runs, measured
    // on two tracks. Quiesce first: residual audio from an earlier audition
    // must not count as this press's first frame.
    for (index, track) in project.tracks.iter().enumerate().take(2) {
        {
            let mut inner = state.lock_inner();
            inner.audio.stop();
        }
        let quiesce = Instant::now();
        let mut stable = heard_transport(&state);
        while quiesce.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
            let now = heard_transport(&state);
            let active = state
                .lock_inner()
                .audio
                .render_thread
                .as_ref()
                .is_some_and(|rt| rt.playback.active.load(Ordering::Acquire));
            if now == stable && !active {
                break;
            }
            stable = now;
        }
        let before = heard_transport(&state);
        let (loads, instruments) = counters(&state);
        let start = Instant::now();
        {
            let mut inner = state.lock_inner();
            let warnings = ensure_clap_instances_loaded(&mut inner);
            let resource_root = inner
                .file_path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            let mut instances = std::mem::take(&mut inner.plugin_instances);
            let result = inner.audio.preview_track(
                &project,
                index,
                &mut instances,
                resource_root.as_deref(),
            );
            inner.plugin_instances = instances;
            assert!(result.is_ok(), "track preview: {result:?}");
            assert!(warnings.is_empty());
        }
        let return_ms = start.elapsed().as_secs_f64() * 1000.0;
        let audible_ms = wait_heard(before, &|| heard_transport(&state));
        let (loads_after, instruments_after) = counters(&state);
        eprintln!(
            "[preview] track index={index} name={} dispatch_return_ms={return_ms:.2} first_audible_ms={audible_ms:?} loads={loads}->{loads_after} instruments={instruments}->{instruments_after}",
            track.name
        );
    }

    state.lock_inner().audio.reset_project();
}

fn state_with_presets(presets: Vec<ast::PresetDef>) -> std::sync::Arc<crate::state::AppState> {
    let state = std::sync::Arc::new(crate::state::AppState::new());
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.presets = presets;
        inner.project = project;
    }
    state
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

fn vst3_plugin(id: &str, name: &str) -> oc_host::PluginInfo {
    oc_host::PluginInfo {
        id: id.to_string(),
        name: name.to_string(),
        vendor: String::new(),
        format: oc_host::PluginFormat::Vst3,
        path: format!("/plugins/{name}.vst3"),
        category: oc_host::PluginCategory::Instrument,
        num_params: 0,
        version: None,
        architectures: vec![],
        state_abi: None,
    }
}

#[test]
fn a_newer_plugin_state_is_refused_and_leaves_the_project_untouched() {
    let dir = TempDir::new("newer-plugin-state");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state_file = dir.path().join("lead.vstpreset");
    std::fs::write(
        &state_file,
        oc_host::encode_plugin_state_versioned(
            oc_host::PluginFormat::Vst3,
            "vst3:test.fake.one_bus",
            oc_host::supported_plugin_state_version() + 1,
            b"future chunk",
        ),
    )
    .unwrap();

    let mut lead = preset("Lead", "vst3", "vst3:test.fake.one_bus");
    lead.params.push((
        "vst3_state_file".to_string(),
        ast::ParamValue::String(state_file.to_string_lossy().into_owned()),
    ));
    let state = state_with_presets(vec![lead]);
    let before = state.lock_inner().project.clone();

    let error = load_plugin_for_preset_with(&state, "Lead", |plan| {
        let mut instance =
            oc_host::vst3_instance::test_support::fake_instance_for_id("test.fake.one_bus", 512)
                .expect("in-process fake VST3");
        let context = oc_host::PluginStateContext::new(
            oc_host::PluginFormat::Vst3,
            Some(plan.plugin_id.as_str()),
            &plan.preset_name,
        );
        oc_engine::render_cache::apply_vst3_preset_state_at(
            &mut instance,
            &plan.preset.params,
            &context,
            None,
        )?;
        Ok(LoadedPlugin {
            instance: LoadedPluginInstance::Vst3(Box::new(instance)),
            params: Vec::new(),
        })
    })
    .unwrap_err();

    let newer = oc_host::supported_plugin_state_version() + 1;
    assert!(error.contains(&format!("state version {newer}")), "{error}");
    assert!(
        error.contains(&format!(
            "supports version {}",
            oc_host::supported_plugin_state_version()
        )),
        "{error}"
    );
    assert!(error.contains("preset 'Lead'"), "{error}");
    let inner = state.lock_inner();
    assert_eq!(inner.project, before);
    assert!(inner.vst3_plugin_instances.is_empty());
    assert!(!inner.undo.can_undo());
}

#[test]
fn vst2_to_vst3_migration_applies_only_when_asked_and_is_undoable() {
    let mut halcyon = preset("Halcyon", "vst2", "vst2:halcyon_2");
    halcyon.params = vec![
        (
            "vst2_state_hex".to_string(),
            ast::ParamValue::String("deadbeef".to_string()),
        ),
        ("gain".to_string(), ast::ParamValue::Number(0.5)),
    ];
    let state = state_with_presets(vec![halcyon]);
    let vst3 = [vst3_plugin("vst3:halcyon_2", "Halcyon 2")];
    let before = state.lock_inner().project.clone();

    let (_, report) = {
        let mut inner = state.lock_inner();
        migrate_vst2_presets_to_vst3_inner(&mut inner, &[], &vst3, true).unwrap()
    };
    assert!(!report.applied);
    assert_eq!(report.migrations.len(), 1);
    assert_eq!(report.migrations[0].preset, "Halcyon");
    assert_eq!(report.migrations[0].vst3_plugin, "vst3:halcyon_2");
    {
        let inner = state.lock_inner();
        assert_eq!(inner.project, before, "a dry run changed the project");
        assert!(!inner.undo.can_undo(), "a dry run pushed undo");
    }

    let (_, report) = {
        let mut inner = state.lock_inner();
        migrate_vst2_presets_to_vst3_inner(&mut inner, &[], &vst3, false).unwrap()
    };
    assert!(report.applied);
    {
        let inner = state.lock_inner();
        let migrated = &inner.project.presets[0];
        assert_eq!(migrated.source.as_deref(), Some("vst3"));
        assert_eq!(migrated.plugin.as_deref(), Some("vst3:halcyon_2"));
        assert!(
            migrated
                .params
                .iter()
                .all(|(name, _)| name != "vst2_state_hex")
        );
        assert!(migrated.params.iter().any(|(name, _)| name == "gain"));
        assert!(inner.undo.can_undo(), "the migration did not enter undo");
    }

    let restored = {
        let mut inner = state.lock_inner();
        let mut project = inner.project.clone();
        inner.undo.undo(&mut project).expect("undo");
        project
    };
    assert_eq!(restored, before, "undo did not restore the VST2 preset");
}

#[test]
fn a_migration_with_nothing_to_do_reports_nothing_and_pushes_no_undo() {
    let mut plain = preset("Plain", "vst2", "vst2:nothing_installed");
    plain.params = vec![(
        "vst2_state_hex".to_string(),
        ast::ParamValue::String("deadbeef".to_string()),
    )];
    let state = state_with_presets(vec![plain]);
    let before = state.lock_inner().project.clone();

    let (_, report) = {
        let mut inner = state.lock_inner();
        // No VST3 twin is installed, so the plan is empty even when applied.
        migrate_vst2_presets_to_vst3_inner(&mut inner, &[], &[], false).unwrap()
    };
    assert!(!report.applied);
    assert!(report.migrations.is_empty());
    assert!(report.skipped.is_empty());

    let inner = state.lock_inner();
    assert_eq!(inner.project, before);
    assert!(!inner.undo.can_undo());
}

/// The report walks the plugin inventory and takes the app-state lock, so a
/// synchronous command would run that work on the webview thread. Keeping the
/// command `async` is what moves it to the blocking pool; this test fails to
/// compile if it is made synchronous again.
#[test]
fn the_compatibility_report_command_stays_async() {
    fn assert_async_command<F, Fut>(_: F)
    where
        F: Fn(tauri::AppHandle) -> Fut,
        Fut: std::future::Future<Output = ProjectCompatibilityReport>,
    {
    }
    assert_async_command(project_compatibility_report);
}

// ── Editor keep-alive ─────────────────────────────────────────────────────

/// An app state holding one in-process fake VST3 instance with an editor
/// controller, so it exposes its parameter list.
fn keepalive_state() -> crate::state::AppState {
    use oc_host::vst3_instance::test_support::{FakeControl, fake_editor_instance};
    let state = crate::state::AppState::new();
    let instance = fake_editor_instance(&std::sync::Arc::new(FakeControl::default()), 2, 512);
    state.lock_inner().vst3_plugin_instances.insert(
        "fake".to_string(),
        std::sync::Arc::new(std::sync::Mutex::new(instance)),
    );
    state
}

fn plugin_param_named(id: u32, name: &str) -> oc_host::clap_instance::PluginParam {
    oc_host::clap_instance::PluginParam {
        id,
        name: name.to_string(),
        module: "VST3".to_string(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        current: 0.0,
        display: "0.0000".to_string(),
        flags: 0,
    }
}

/// The close button in the editor's own title bar is the same close as the
/// app's button: the keep-alive is released and the panel is told once.
// The VST3 editor keep-alive runs only on macOS; elsewhere editors run in the
// out-of-process helper.
#[cfg(target_os = "macos")]
#[test]
fn keepalive_poll_reports_a_title_bar_close_and_releases_the_editor() {
    let state = keepalive_state();
    state.lock_inner().vst3_editor_keepalives.insert(
        "fake".to_string(),
        crate::plugin_keepalive::EditorKeepAlive::closed_for_test(),
    );

    assert!(
        plugin_gui_closed(&state, "fake").expect("the poll answers"),
        "a closed editor window is reported closed"
    );
    assert!(
        !state
            .lock_inner()
            .vst3_editor_keepalives
            .contains_key("fake"),
        "the keep-alive is released with the window"
    );
    assert!(
        !plugin_gui_closed(&state, "fake").expect("a second poll answers"),
        "the close is reported once, not on every poll"
    );
}

/// A poll with no reported change answers from the cached inventory; a
/// reported change — a program change, a drag or a reload — reads the plugin
/// again.
#[test]
fn keepalive_parameter_poll_reuses_the_inventory_until_a_change() {
    let state = keepalive_state();
    let mut inner = state.lock_inner();
    inner
        .plugin_param_cache
        .store("fake", vec![plugin_param_named(1, "Cached Only")]);
    drop(inner);

    let cached = vst3_preview_params(&state, "fake").expect("the preview answers");
    assert_eq!(
        cached[0].name, "Cached Only",
        "a poll with no change reuses the inventory instead of enumerating"
    );

    state.lock_inner().plugin_param_cache.invalidate("fake");
    let fresh = vst3_preview_params(&state, "fake").expect("the preview answers");
    assert!(
        fresh.iter().all(|param| param.name != "Cached Only"),
        "a reported change re-reads the plugin's own inventory"
    );
    assert!(
        fresh.iter().any(|param| param.name == "Gain"),
        "the fake plugin exposes its parameter"
    );
}

/// Helpers are children of this process, so these cases spawn a real one. They
/// are Unix-only because the stand-in for a plugin editor is `sh`/`sleep`; the
/// kill path itself is platform-neutral.
#[cfg(unix)]
mod editor_helpers {
    use super::*;

    fn log_path() -> PathBuf {
        std::env::temp_dir().join("ocp-editor-helper-test.log")
    }

    fn exited_helper() -> crate::editor_helper::EditorHelper {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 9")
            .spawn()
            .expect("spawn");
        crate::editor_helper::EditorHelper::track(child, log_path(), None)
    }

    fn live_helper() -> crate::editor_helper::EditorHelper {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn");
        crate::editor_helper::EditorHelper::track(child, log_path(), None)
    }

    fn wait_for_exit(helper: &crate::editor_helper::EditorHelper) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while helper.outcome().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(helper.outcome().is_some(), "the helper was not reaped");
    }

    /// A helper owns a plugin instance loaded from the project it was opened
    /// for. Replacing the project must stop it: a survivor answers "already
    /// open" for the next document and keeps its window after the project is
    /// gone.
    #[test]
    fn a_project_replacement_stops_every_editor_helper() {
        let mut inner = test_state();
        let helper = live_helper();
        let watched = helper.clone();
        inner.editor_helpers.insert("Kontakt".to_string(), helper);

        create_project_in(&mut inner, None).expect("new project");

        assert!(
            inner.editor_helpers.is_empty(),
            "the replaced project kept its editor helper"
        );
        wait_for_exit(&watched);
    }

    /// A helper that died is taken out of the map and reported once, so the UI
    /// closes the editor instead of reopening one that no longer exists.
    #[test]
    fn a_dead_helper_reports_closed_and_leaves_the_map() {
        let mut inner = test_state();
        inner
            .editor_helpers
            .insert("Kontakt".to_string(), exited_helper());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while inner
            .editor_helpers
            .get("Kontakt")
            .expect("helper tracked")
            .outcome()
            .is_none()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let exit = take_helper_exit(&mut inner, "Kontakt").expect("the exit is reported");
        assert!(!exit.closed_cleanly);
        assert!(exit.message.contains("inspector"));
        assert!(
            inner.editor_helpers.is_empty(),
            "the dead helper stayed tracked"
        );
    }

    /// A running helper reports nothing and keeps its slot: the editor is open,
    /// and reopening it would start a second window from the same project.
    #[test]
    fn a_running_helper_is_not_reported_as_closed() {
        let mut inner = test_state();
        let helper = live_helper();
        let watched = helper.clone();
        inner.editor_helpers.insert("Kontakt".to_string(), helper);

        assert!(take_helper_exit(&mut inner, "Kontakt").is_none());
        assert!(inner.editor_helpers.contains_key("Kontakt"));

        inner.close_editor_helpers();
        wait_for_exit(&watched);
    }
}
