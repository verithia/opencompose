//! Typed edit descriptions paired from forward and inverse patches.
//!
//! Each case builds a small `.ocp` pair, diffs it, inverts the patch, and
//! asserts the facts the undo UI and the agent panel render.

use oc_dsl::ast::{ProjectProperties, TimeSignature};
use oc_dsl::change::{
    self, BeatSpan, Change, ChangeSet, MAX_CHANGES, MAX_FACT_TEXT_CHARS, MixParam, Owner,
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
    let set = change::describe_patches(&forward, &inverse);
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
fn removing_several_tracks_resolves_each_name_from_the_inverse_set() {
    let set = describe_sources(
        "@project\n@track a | instrument: piano/acoustic_grand\n@track b | instrument: piano/acoustic_grand\n@track c | instrument: piano/acoustic_grand\n",
        "@project\n@track b | instrument: piano/acoustic_grand\n",
    );
    assert_eq!(set.more, 0);
    assert_eq!(
        set.changes,
        vec![
            Change::TrackRemoved { track: "c".into() },
            Change::TrackRemoved { track: "a".into() },
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
        changes: vec![PatchOp::SetProperties(properties)],
    };
    let inverse = ProjectPatch {
        label: String::new(),
        changes: Vec::new(),
    };
    let set = change::describe_patches(&forward, &inverse);
    assert_eq!(only(&set), Change::ProjectSettings);

    let mut removal = ProjectPatch {
        label: String::new(),
        changes: vec![PatchOp::RemovePreset("gone".into())],
    };
    removal.label = "missing inverse".into();
    let mutations = diff::mutations(&removal, &inverse);
    assert!(mutations.is_empty());
    assert!(change::describe(&mutations).is_empty());
}

#[test]
fn stored_compressed_patches_still_describe_the_edit() {
    let (old, new) = projects("@project\n  tempo: 120\n", "@project\n  tempo: 132\n");
    let forward = diff::diff(&old, &new).unwrap().unwrap();
    let inverse = diff::invert_patch(&old, &new, &forward);
    let reopen = |patch: &ProjectPatch| {
        diff::compressed::decode_patch(&diff::compressed::encode_patch(patch).unwrap()).unwrap()
    };
    let set = change::describe_patches(&reopen(&forward), &reopen(&inverse));
    assert_eq!(
        only(&set),
        Change::Tempo {
            from: 120.0,
            to: 132.0
        }
    );
}

#[test]
fn describe_with_properties_uses_the_caller_meter_for_spans() {
    let (old, new) = projects(
        "@project\n  time_signature: 3/4\n@section verse [1:1 - 2:1]\n",
        "@project\n  time_signature: 3/4\n@section verse [1:1 - 4:1]\n",
    );
    let forward = diff::diff(&old, &new).unwrap().unwrap();
    let inverse = diff::invert_patch(&old, &new, &forward);
    let set = change::describe_patches_with_properties(&forward, &inverse, &new.properties);
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
