//! Typed edit descriptions paired from forward and inverse patches.
//!
//! Each case builds a small `.ocp` pair, diffs it, inverts the patch, and
//! asserts the facts the undo UI and the agent panel render.

use oc_dsl::ast::{KeyMode, ProjectProperties, TimeSignature};
use oc_dsl::change::{
    self, BeatSpan, Change, ChangeSet, KeyToken, MAX_CHANGES, MAX_FACT_TEXT_CHARS, MixParam, Owner,
};
use oc_dsl::diff::{self, PatchOp, ProjectPatch};
use oc_dsl::parse;

fn projects(old: &str, new: &str) -> (oc_dsl::ast::Project, oc_dsl::ast::Project) {
    (
        parse(old).expect("old fixture parses"),
        parse(new).expect("new fixture parses"),
    )
}

fn describe_pair(
    old: &oc_dsl::ast::Project,
    new: &oc_dsl::ast::Project,
) -> (ProjectPatch, ProjectPatch, ChangeSet) {
    let forward = diff::diff(old, new)
        .expect("projects serialize")
        .expect("fixture pair is an edit");
    let inverse = diff::invert_patch(old, new, &forward);
    let set = change::describe(&forward, &inverse, &new.properties);
    (forward, inverse, set)
}

fn describe_sources(old: &str, new: &str) -> ChangeSet {
    let (old, new) = projects(old, new);
    describe_pair(&old, &new).2
}

fn only(set: &ChangeSet) -> Change {
    assert_eq!(set.more, 0);
    assert_eq!(set.changes.len(), 1, "{:?}", set.changes);
    set.changes[0].clone()
}

const TRACK: &str = "@project\n@track lead | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 q mf\n";
const EMPTY: &str = "@project\n";

#[test]
fn tempo_change_is_a_typed_fact_and_uses_the_snake_case_tag() {
    let set = describe_sources("@project\n  tempo: 120\n", "@project\n  tempo: 132\n");
    assert_eq!(
        only(&set),
        Change::Tempo {
            from: 120.0,
            to: 132.0
        }
    );
    let value = serde_json::to_value(Change::Tempo {
        from: 120.0,
        to: 132.0,
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"kind": "tempo", "from": 120.0, "to": 132.0})
    );
}

#[test]
fn key_change_keeps_the_dsl_mode_wire_and_the_project_wire_apart() {
    let set = describe_sources("@project\n  key: C major\n", "@project\n  key: F# minor\n");
    let Change::Key {
        to: Some(token), ..
    } = only(&set)
    else {
        panic!("a key edit is one key fact");
    };
    assert_eq!(token.mode, KeyMode::Minor);
    assert_eq!(
        serde_json::to_value(Change::Key {
            from: Some(KeyToken {
                root: "C".into(),
                mode: KeyMode::Major,
            }),
            to: Some(KeyToken {
                root: "F#".into(),
                mode: KeyMode::Minor,
            }),
        })
        .unwrap(),
        serde_json::json!({
            "kind": "key",
            "from": {"root": "C", "mode": "major"},
            "to": {"root": "F#", "mode": "minor"},
        })
    );
    let project = parse("@project\n  key: C major\n").unwrap();
    let key = serde_json::to_value(project.properties.key.unwrap()).unwrap();
    assert_eq!(key["mode"], "Major");
}

#[test]
fn time_signature_change_carries_both_meters() {
    let set = describe_sources(
        "@project\n  time_signature: 4/4\n",
        "@project\n  time_signature: 3/4\n",
    );
    assert_eq!(
        only(&set),
        Change::TimeSignature {
            from: TimeSignature {
                numerator: 4,
                denominator: 4,
            },
            to: TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        }
    );
}

#[test]
fn project_rename_reports_the_old_and_new_names() {
    let set = describe_sources("@project \"Old name\"\n", "@project \"New name\"\n");
    assert_eq!(
        only(&set),
        Change::ProjectRenamed {
            from: Some("Old name".into()),
            to: Some("New name".into()),
        }
    );
}

#[test]
fn tracks_add_remove_rename_and_reorder_as_their_own_facts() {
    let added = describe_sources(EMPTY, TRACK);
    assert_eq!(
        only(&added),
        Change::TrackAdded {
            track: "lead".into()
        }
    );

    let removed = describe_sources(TRACK, EMPTY);
    assert_eq!(
        only(&removed),
        Change::TrackRemoved {
            track: "lead".into()
        }
    );

    let renamed = describe_sources(
        TRACK,
        "@project\n@track lead_two | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 q mf\n",
    );
    assert_eq!(
        only(&renamed),
        Change::TrackRenamed {
            from: "lead".into(),
            to: "lead_two".into(),
        }
    );

    let reordered = describe_sources(
        "@project\n@track a | instrument: piano/acoustic_grand\n@track b | instrument: piano/acoustic_grand\n",
        "@project\n@track b | instrument: piano/acoustic_grand\n@track a | instrument: piano/acoustic_grand\n",
    );
    assert_eq!(only(&reordered), Change::TracksReordered);
}

#[test]
fn presets_patterns_and_buses_report_a_rename_as_its_own_fact() {
    let preset_before =
        "@project\n@preset lead\n  source: builtin\n  plugin: \"builtin.open_synth\"\n";
    let preset_after =
        "@project\n@preset lead_two\n  source: builtin\n  plugin: \"builtin.open_synth\"\n";
    let renamed = describe_sources(preset_before, preset_after);
    assert_eq!(
        only(&renamed),
        Change::PresetRenamed {
            from: "lead".into(),
            to: "lead_two".into(),
        }
    );

    let pattern_before =
        "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n";
    let pattern_after =
        "@project\n@pattern riff_two | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n";
    let renamed = describe_sources(pattern_before, pattern_after);
    assert_eq!(
        only(&renamed),
        Change::PatternRenamed {
            from: "riff".into(),
            to: "riff_two".into(),
        }
    );

    let bus_before = "@project\n@bus space\n  routes: [master]\n";
    let bus_after = "@project\n@bus room\n  routes: [master]\n";
    let renamed = describe_sources(bus_before, bus_after);
    assert_eq!(
        only(&renamed),
        Change::BusRenamed {
            from: "space".into(),
            to: "room".into(),
        }
    );
    assert_eq!(
        serde_json::to_value(Change::PresetRenamed {
            from: "a".into(),
            to: "b".into(),
        })
        .unwrap(),
        serde_json::json!({"kind": "preset_renamed", "from": "a", "to": "b"})
    );
    assert_eq!(
        serde_json::to_value(Change::PatternRenamed {
            from: "a".into(),
            to: "b".into(),
        })
        .unwrap(),
        serde_json::json!({"kind": "pattern_renamed", "from": "a", "to": "b"})
    );
    assert_eq!(
        serde_json::to_value(Change::BusRenamed {
            from: "a".into(),
            to: "b".into(),
        })
        .unwrap(),
        serde_json::json!({"kind": "bus_renamed", "from": "a", "to": "b"})
    );
}

#[test]
fn an_in_place_declaration_rename_is_a_rename_not_a_change() {
    let (old, mut new) = projects(
        "@project\n@bus space\n  routes: [master]\n",
        "@project\n@bus space\n  routes: [master]\n",
    );
    new.buses[0].name = "room".into();
    let (_, _, set) = describe_pair(&old, &new);
    assert_eq!(
        only(&set),
        Change::BusRenamed {
            from: "space".into(),
            to: "room".into(),
        }
    );
}

#[test]
fn removing_several_tracks_resolves_each_name_from_the_inverse_set() {
    let set = describe_sources(
        "@project\n@track a | instrument: piano/acoustic_grand\n@track b | instrument: piano/acoustic_grand\n@track c | instrument: piano/acoustic_grand\n",
        "@project\n@track b | instrument: piano/acoustic_grand\n",
    );
    assert_eq!(set.more, 0);
    assert_eq!(
        set.changes,
        vec![
            Change::TrackRemoved { track: "a".into() },
            Change::TrackRemoved { track: "c".into() },
        ]
    );
}

#[test]
fn volume_and_mute_on_one_track_share_one_mix_fact() {
    let (old, mut new) = projects(TRACK, TRACK);
    new.tracks[0].mix.volume_db = -6.0;
    new.tracks[0].mix.mute = true;
    let (_, _, set) = describe_pair(&old, &new);
    assert_eq!(
        only(&set),
        Change::TrackMix {
            track: "lead".into(),
            params: vec![
                MixParam::Volume {
                    from: 0.0,
                    to: -6.0,
                },
                MixParam::Mute {
                    from: false,
                    to: true,
                },
            ],
        }
    );
}

#[test]
fn track_effect_chain_reports_added_removed_changed_and_bypassed() {
    let base = "@project\n@track lead | instrument: piano/acoustic_grand\n";
    let with_eq = "@project\n@track lead | instrument: piano/acoustic_grand\n  effects:\n    - eq: low_cut=200Hz\n";
    let with_other_eq = "@project\n@track lead | instrument: piano/acoustic_grand\n  effects:\n    - eq: low_cut=300Hz\n";
    let bypassed = "@project\n@track lead | instrument: piano/acoustic_grand\n  effects:\n    - eq: low_cut=200Hz, bypass=true\n";
    let owner = Owner::Track {
        name: "lead".into(),
    };

    assert_eq!(
        only(&describe_sources(base, with_eq)),
        Change::EffectAdded {
            owner: owner.clone(),
            effect: "eq".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(with_eq, base)),
        Change::EffectRemoved {
            owner: owner.clone(),
            effect: "eq".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(with_eq, with_other_eq)),
        Change::EffectChanged {
            owner: owner.clone(),
            effect: "eq".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(with_eq, bypassed)),
        Change::EffectBypassed {
            owner,
            effect: "eq".into(),
            bypassed: true,
        }
    );
}

#[test]
fn master_effect_changes_use_the_master_owner() {
    let plain = "@project\n@master\n  effects:\n    - limiter\n";
    let added = "@project\n@master\n  effects:\n    - limiter\n    - eq: low_cut=200Hz\n";
    let bypassed = "@project\n@master\n  effects:\n    - limiter: bypass=true\n";

    let facts = describe_sources(plain, added);
    assert!(
        facts.changes.contains(&Change::EffectAdded {
            owner: Owner::Master,
            effect: "eq".into(),
        }),
        "{:?}",
        facts.changes
    );
    assert_eq!(
        only(&describe_sources(plain, bypassed)),
        Change::EffectBypassed {
            owner: Owner::Master,
            effect: "limiter".into(),
            bypassed: true,
        }
    );
}

#[test]
fn notes_added_in_bars_five_to_eight_count_and_span_both_places() {
    let before = "@project\n@track lead | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 9:1]\n    C4 w mf\n";
    let after = "@project\n@track lead | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 9:1]\n    C4 w mf\n    @16 D4 w mf\n    @28 E4 w mf\n";
    let set = describe_sources(before, after);
    assert_eq!(
        only(&set),
        Change::NotesEdited {
            track: "lead".into(),
            span: Some(BeatSpan {
                start: 16.0,
                end: 32.0,
            }),
            added: 2,
            removed: 0,
            changed: 0,
        }
    );
}

#[test]
fn a_moved_clip_spans_its_old_and_new_place() {
    let before = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff [2:1 - 6:1]\n";
    let after = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff [6:1 - 10:1]\n";
    let set = describe_sources(before, after);
    assert_eq!(
        only(&set),
        Change::ClipsEdited {
            track: "keys".into(),
            span: Some(BeatSpan {
                start: 4.0,
                end: 36.0,
            }),
        }
    );
}

#[test]
fn a_pattern_change_reports_the_content_span() {
    let before = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff [2:1 - 6:1]\n";
    let after = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n    D4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff [2:1 - 6:1]\n";
    let set = describe_sources(before, after);
    assert_eq!(
        only(&set),
        Change::PatternChanged {
            pattern: "riff".into(),
            span: Some(BeatSpan {
                start: 0.0,
                end: 2.0,
            }),
        }
    );
}

#[test]
fn adding_a_bus_reports_the_declaration_name() {
    let set = describe_sources(EMPTY, "@project\n@bus space\n  routes: [master]\n");
    assert_eq!(
        only(&set),
        Change::BusAdded {
            bus: "space".into()
        }
    );
}

#[test]
fn a_section_length_change_spans_the_old_and_new_measures() {
    let set = describe_sources(
        "@project\n@section verse [1:1 - 5:1]\n",
        "@project\n@section verse [1:1 - 9:1]\n",
    );
    assert_eq!(
        only(&set),
        Change::SectionsEdited {
            span: Some(BeatSpan {
                start: 0.0,
                end: 32.0,
            }),
        }
    );
}

#[test]
fn markers_count_additions_removals_and_moves() {
    let set = describe_sources(
        "@project\n@marker a at 1:1\n@marker b at 2:1\n",
        "@project\n@marker a at 3:1\n@marker c at 4:1\n",
    );
    assert_eq!(
        only(&set),
        Change::MarkersEdited {
            added: 1,
            removed: 1,
            changed: 1,
        }
    );
}

#[test]
fn a_track_routing_field_never_leaves_the_change_silent() {
    let (old, mut new) = projects(
        "@project\n@track lead | instrument: piano/acoustic_grand\n",
        "@project\n@track lead | instrument: piano/acoustic_grand\n",
    );
    new.tracks[0].channel = Some(2);
    let (_, _, set) = describe_pair(&old, &new);
    assert_eq!(
        only(&set),
        Change::TrackRouting {
            track: "lead".into()
        }
    );
}

#[test]
fn more_than_the_bound_records_the_dropped_fact_count() {
    let mut source = String::from("@project\n  tempo: 120\n");
    for index in 0..12 {
        source.push_str(&format!(
            "@track t{index} | instrument: piano/acoustic_grand\n"
        ));
    }
    let old = parse(&source).expect("many-track fixture parses");
    let mut new = old.clone();
    new.properties.tempo = 132.0;
    for (index, track) in new.tracks.iter_mut().enumerate() {
        track.mix.volume_db = -(index as f64) - 1.0;
    }
    let (_, _, set) = describe_pair(&old, &new);
    assert_eq!(set.changes.len(), MAX_CHANGES);
    assert_eq!(set.more, 5);
    assert!(!set.is_empty());
}

#[test]
fn a_forward_operation_without_an_inverse_partner_does_not_panic() {
    let properties = ProjectProperties {
        tempo: 140.0,
        ..Default::default()
    };
    let forward = ProjectPatch {
        label: String::new(),
        changes: vec![PatchOp::SetProperties(properties.clone())],
    };
    let inverse = ProjectPatch {
        label: String::new(),
        changes: Vec::new(),
    };
    let set = change::describe(&forward, &inverse, &properties);
    assert_eq!(only(&set), Change::ProjectSettings);

    let removal = ProjectPatch {
        label: "missing inverse".into(),
        changes: vec![PatchOp::RemovePreset("gone".into())],
    };
    assert!(change::describe(&removal, &inverse, &properties).is_empty());
}

#[test]
fn stored_compressed_patches_still_describe_the_edit() {
    let (old, new) = projects("@project\n  tempo: 120\n", "@project\n  tempo: 132\n");
    let forward = diff::diff(&old, &new).unwrap().unwrap();
    let inverse = diff::invert_patch(&old, &new, &forward);
    let reopen = |patch: &ProjectPatch| {
        diff::compressed::decode_patch(&diff::compressed::encode_patch(patch).unwrap()).unwrap()
    };
    let set = change::describe(&reopen(&forward), &reopen(&inverse), &new.properties);
    assert_eq!(
        only(&set),
        Change::Tempo {
            from: 120.0,
            to: 132.0
        }
    );
}

#[test]
fn describe_uses_the_caller_meter_for_spans() {
    let (old, new) = projects(
        "@project\n  time_signature: 3/4\n@section verse [1:1 - 2:1]\n",
        "@project\n  time_signature: 3/4\n@section verse [1:1 - 4:1]\n",
    );
    let forward = diff::diff(&old, &new).unwrap().unwrap();
    let inverse = diff::invert_patch(&old, &new, &forward);
    let set = change::describe(&forward, &inverse, &new.properties);
    assert!(
        set.changes.iter().any(|change| matches!(
            change,
            Change::SectionsEdited {
                span: Some(BeatSpan {
                    start: 0.0,
                    end: 9.0
                })
            }
        )),
        "{:?}",
        set.changes
    );
}

#[test]
fn a_long_declaration_name_is_clipped_in_the_fact() {
    let long = "x".repeat(MAX_FACT_TEXT_CHARS + 50);
    let set = describe_sources("@project\n", &format!("@project \"{long}\"\n"));
    match only(&set) {
        Change::ProjectRenamed { to: Some(name), .. } => {
            assert_eq!(name.chars().count(), MAX_FACT_TEXT_CHARS);
            assert!(name.ends_with("..."), "{name}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn membership_edits_do_not_report_a_declaration_reorder() {
    let tracks = |order: &str| format!("@project\n{order}");
    let rename_middle = tracks("@track a\n@track b\n@track c\n");
    let renamed = tracks("@track a\n@track x\n@track c\n");
    assert_eq!(
        only(&describe_sources(&rename_middle, &renamed)),
        Change::TrackRenamed {
            from: "b".into(),
            to: "x".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(
            &tracks("@track a\n@track c\n"),
            &rename_middle
        )),
        Change::TrackAdded { track: "b".into() }
    );
    assert_eq!(
        only(&describe_sources(
            &rename_middle,
            &tracks("@track a\n@track c\n")
        )),
        Change::TrackRemoved { track: "b".into() }
    );
    let reversed = tracks("@track c\n@track b\n@track a\n");
    assert_eq!(
        only(&describe_sources(&rename_middle, &reversed)),
        Change::TracksReordered
    );
    assert_eq!(
        only(&describe_sources(&reversed, &rename_middle)),
        Change::TracksReordered
    );

    let buses = |order: &str| format!("@project\n{order}");
    let bus = |name: &str| format!("@bus {name}\n  routes: [master]\n");
    let bus_list = |names: &[&str]| buses(&names.iter().map(|name| bus(name)).collect::<String>());
    let abc = bus_list(&["a", "b", "c"]);
    let axc = bus_list(&["a", "x", "c"]);
    assert_eq!(
        only(&describe_sources(&abc, &axc)),
        Change::BusRenamed {
            from: "b".into(),
            to: "x".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(&bus_list(&["a", "c"]), &abc)),
        Change::BusAdded { bus: "b".into() }
    );
    assert_eq!(
        only(&describe_sources(&abc, &bus_list(&["a", "c"]))),
        Change::BusRemoved { bus: "b".into() }
    );
    let cba = bus_list(&["c", "b", "a"]);
    assert_eq!(only(&describe_sources(&abc, &cba)), Change::BusesReordered);
    assert_eq!(only(&describe_sources(&cba, &abc)), Change::BusesReordered);

    let presets = |order: &str| format!("@project\n{order}");
    let preset = |name: &str| {
        format!("@preset {name}\n  source: builtin\n  plugin: \"builtin.open_synth\"\n")
    };
    let preset_list =
        |names: &[&str]| presets(&names.iter().map(|name| preset(name)).collect::<String>());
    assert_eq!(
        only(&describe_sources(
            &preset_list(&["a", "b", "c"]),
            &preset_list(&["a", "x", "c"])
        )),
        Change::PresetRenamed {
            from: "b".into(),
            to: "x".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(
            &preset_list(&["a", "c"]),
            &preset_list(&["a", "b", "c"])
        )),
        Change::PresetAdded { preset: "b".into() }
    );

    let patterns = |order: &str| format!("@project\n{order}");
    let pattern = |name: &str| {
        format!("@pattern {name} | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n")
    };
    let pattern_list =
        |names: &[&str]| patterns(&names.iter().map(|name| pattern(name)).collect::<String>());
    assert_eq!(
        only(&describe_sources(
            &pattern_list(&["a", "b", "c"]),
            &pattern_list(&["a", "x", "c"])
        )),
        Change::PatternRenamed {
            from: "b".into(),
            to: "x".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(
            &pattern_list(&["a", "c"]),
            &pattern_list(&["a", "b", "c"])
        )),
        Change::PatternAdded {
            pattern: "b".into()
        }
    );
}

#[test]
fn identical_content_renames_pair_in_declaration_order() {
    let set = describe_sources(
        "@project\n@track a\n@track b\n",
        "@project\n@track x\n@track y\n",
    );
    assert_eq!(
        set.changes,
        vec![
            Change::TrackRenamed {
                from: "a".into(),
                to: "x".into(),
            },
            Change::TrackRenamed {
                from: "b".into(),
                to: "y".into(),
            },
        ]
    );
}

#[test]
fn a_removed_and_an_appended_bus_are_not_a_rename() {
    let buses = |names: &[(&str, &str)]| {
        let mut source = String::from("@project\n");
        for (name, route) in names {
            source.push_str(&format!("@bus {name}\n  routes: [{route}]\n"));
        }
        source
    };
    let before = buses(&[("verb", "master"), ("a", "master"), ("b", "master")]);
    let after = buses(&[("a", "master"), ("b", "master"), ("delay", "master")]);
    assert_eq!(
        describe_sources(&before, &after).changes,
        vec![
            Change::BusRemoved { bus: "verb".into() },
            Change::BusAdded {
                bus: "delay".into()
            },
        ]
    );
    let in_place = buses(&[("delay", "master"), ("a", "master"), ("b", "master")]);
    assert_eq!(
        only(&describe_sources(&before, &in_place)),
        Change::BusRenamed {
            from: "verb".into(),
            to: "delay".into(),
        }
    );
    assert_eq!(
        describe_sources(&after, &before).changes,
        vec![
            Change::BusRemoved {
                bus: "delay".into()
            },
            Change::BusAdded { bus: "verb".into() },
        ]
    );
}

#[test]
fn renaming_a_track_does_not_report_each_audio_clip_on_it() {
    let before = "@project\n@audio_clip intro [1:1 - 5:1] | track: tape\n  file: \"a.wav\"\n@audio_clip outro [9:1 - 10:1] | track: tape\n  file: \"b.wav\"\n@track tape\n";
    let after = "@project\n@audio_clip intro [1:1 - 5:1] | track: reel\n  file: \"a.wav\"\n@audio_clip outro [9:1 - 10:1] | track: reel\n  file: \"b.wav\"\n@track reel\n";
    assert_eq!(
        only(&describe_sources(before, after)),
        Change::TrackRenamed {
            from: "tape".into(),
            to: "reel".into(),
        }
    );
}

#[test]
fn renames_pair_inside_the_gap_between_the_same_survivors() {
    let tracks = |order: &str| format!("@project\n{order}");
    let old = tracks("@track a\n@track b\n@track c\n");
    let inserted = tracks("@track n\n@track a\n@track x\n@track c\n");
    assert_eq!(
        describe_sources(&old, &inserted).changes,
        vec![
            Change::TrackRenamed {
                from: "b".into(),
                to: "x".into(),
            },
            Change::TrackAdded { track: "n".into() },
        ]
    );
    assert_eq!(
        describe_sources(&inserted, &old).changes,
        vec![
            Change::TrackRenamed {
                from: "x".into(),
                to: "b".into(),
            },
            Change::TrackRemoved { track: "n".into() },
        ]
    );

    let tail = tracks("@track b\n@track y\n");
    assert_eq!(
        describe_sources(&old, &tail).changes,
        vec![
            Change::TrackRenamed {
                from: "c".into(),
                to: "y".into(),
            },
            Change::TrackRemoved { track: "a".into() },
        ]
    );
    assert_eq!(
        describe_sources(&tail, &old).changes,
        vec![
            Change::TrackRenamed {
                from: "y".into(),
                to: "c".into(),
            },
            Change::TrackAdded { track: "a".into() },
        ]
    );

    let buses = |order: &str| format!("@project\n{order}");
    let bus = |name: &str| format!("@bus {name}\n  routes: [master]\n");
    let bus_list = |names: &[&str]| buses(&names.iter().map(|name| bus(name)).collect::<String>());
    let abc = bus_list(&["a", "b", "c"]);
    let inserted = bus_list(&["n", "a", "x", "c"]);
    assert_eq!(
        describe_sources(&abc, &inserted).changes,
        vec![
            Change::BusRenamed {
                from: "b".into(),
                to: "x".into(),
            },
            Change::BusAdded { bus: "n".into() },
        ]
    );
    assert_eq!(
        describe_sources(&inserted, &abc).changes,
        vec![
            Change::BusRenamed {
                from: "x".into(),
                to: "b".into(),
            },
            Change::BusRemoved { bus: "n".into() },
        ]
    );
    let tail = bus_list(&["b", "y"]);
    assert_eq!(
        describe_sources(&abc, &tail).changes,
        vec![
            Change::BusRenamed {
                from: "c".into(),
                to: "y".into(),
            },
            Change::BusRemoved { bus: "a".into() },
        ]
    );
    assert_eq!(
        describe_sources(&tail, &abc).changes,
        vec![
            Change::BusRenamed {
                from: "y".into(),
                to: "c".into(),
            },
            Change::BusAdded { bus: "a".into() },
        ]
    );

    let presets = |order: &str| format!("@project\n{order}");
    let preset = |name: &str| {
        format!("@preset {name}\n  source: builtin\n  plugin: \"builtin.open_synth\"\n")
    };
    let preset_list =
        |names: &[&str]| presets(&names.iter().map(|name| preset(name)).collect::<String>());
    assert_eq!(
        describe_sources(
            &preset_list(&["a", "b", "c"]),
            &preset_list(&["n", "a", "x", "c"])
        )
        .changes,
        vec![
            Change::PresetRenamed {
                from: "b".into(),
                to: "x".into(),
            },
            Change::PresetAdded { preset: "n".into() },
        ]
    );
    assert_eq!(
        describe_sources(
            &preset_list(&["n", "a", "x", "c"]),
            &preset_list(&["a", "b", "c"])
        )
        .changes,
        vec![
            Change::PresetRenamed {
                from: "x".into(),
                to: "b".into(),
            },
            Change::PresetRemoved { preset: "n".into() },
        ]
    );

    let patterns = |order: &str| format!("@project\n{order}");
    let pattern = |name: &str| {
        format!("@pattern {name} | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n")
    };
    let pattern_list =
        |names: &[&str]| patterns(&names.iter().map(|name| pattern(name)).collect::<String>());
    assert_eq!(
        describe_sources(
            &pattern_list(&["a", "b", "c"]),
            &pattern_list(&["n", "a", "x", "c"])
        )
        .changes,
        vec![
            Change::PatternRenamed {
                from: "b".into(),
                to: "x".into(),
            },
            Change::PatternAdded {
                pattern: "n".into()
            },
        ]
    );
    assert_eq!(
        describe_sources(
            &pattern_list(&["n", "a", "x", "c"]),
            &pattern_list(&["a", "b", "c"])
        )
        .changes,
        vec![
            Change::PatternRenamed {
                from: "x".into(),
                to: "b".into(),
            },
            Change::PatternRemoved {
                pattern: "n".into()
            },
        ]
    );
}

#[test]
fn reference_updates_that_follow_a_rename_are_not_their_own_facts() {
    let bus_before = "@project\n@bus verb\n  routes: [master]\n@bus glue\n  routes: [verb]\n@track lead | instrument: piano/acoustic_grand\n  mix: send(verb=25%), output=verb\n";
    let bus_after = "@project\n@bus delay\n  routes: [master]\n@bus glue\n  routes: [delay]\n@track lead | instrument: piano/acoustic_grand\n  mix: send(delay=25%), output=delay\n";
    assert_eq!(
        only(&describe_sources(bus_before, bus_after)),
        Change::BusRenamed {
            from: "verb".into(),
            to: "delay".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(bus_after, bus_before)),
        Change::BusRenamed {
            from: "delay".into(),
            to: "verb".into(),
        }
    );

    let preset_before = "@project\n@preset lead\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track keys | instrument: piano/acoustic_grand, preset: lead\n";
    let preset_after = "@project\n@preset lead_two\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track keys | instrument: piano/acoustic_grand, preset: lead_two\n";
    assert_eq!(
        only(&describe_sources(preset_before, preset_after)),
        Change::PresetRenamed {
            from: "lead".into(),
            to: "lead_two".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(preset_after, preset_before)),
        Change::PresetRenamed {
            from: "lead_two".into(),
            to: "lead".into(),
        }
    );

    let pattern_before = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff [1:1 - 2:1]\n";
    let pattern_after = "@project\n@pattern riff_two | instrument: piano/acoustic_grand\n  events:\n    C4 q mf\n@track keys | instrument: piano/acoustic_grand\n  clip: riff_two [1:1 - 2:1]\n";
    assert_eq!(
        only(&describe_sources(pattern_before, pattern_after)),
        Change::PatternRenamed {
            from: "riff".into(),
            to: "riff_two".into(),
        }
    );
    assert_eq!(
        only(&describe_sources(pattern_after, pattern_before)),
        Change::PatternRenamed {
            from: "riff_two".into(),
            to: "riff".into(),
        }
    );
}
