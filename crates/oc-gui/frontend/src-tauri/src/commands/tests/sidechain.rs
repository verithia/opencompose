//! Sidechain keys: the validator owns which key an owner may read, so a bus key
//! on a track chain is refused with the validator's own sentence.

use crate::commands::fx::set_effect_sidechain_inner;
use crate::state::AppStateInner;

use super::support::test_state;

const CHAIN: &str = r#"@ocp "0.2.0"

@project "sidechain"
  tempo: 120

@bus reverb
  routes: [master]
  effects:
    - dynamic_eq

@bus plate
  routes: [master]
  effects:
    - compression: ratio=4:1

@master
  effects:
    - limiter: ceiling=-1dB

@track lead | instrument: synth/lead_square
  effects:
    - dynamic_eq
  @phrase p [1:1 - 2:1]
    C4 w mf

@track drum | instrument: drums/acoustic_kit
"#;

fn chain_state() -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(CHAIN).unwrap();
    oc_dsl::validate(&inner.project).unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

/// The document both the save path and `ocp validate` see.
fn document(inner: &AppStateInner) -> String {
    oc_dsl::serialize(&inner.project)
}

#[test]
fn a_bus_key_on_a_track_chain_returns_the_validator_sentence() {
    let mut inner = chain_state();
    let before = inner.project.clone();
    let revision = inner.revision;

    let error = set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("reverb".to_string()))
        .unwrap_err();

    assert!(
        error.contains("Sidechain key 'reverb' in track 'lead' is a bus"),
        "the refusal is the validator's own sentence: {error}"
    );
    assert_eq!(inner.project, before, "a refusal changes nothing");
    assert_eq!(inner.revision, revision, "a refusal publishes no revision");
    assert!(inner.undo.undo.is_empty(), "a refusal adds no undo entry");
    assert!(!inner.dirty, "a refusal keeps the document clean");
}

#[test]
fn a_track_key_on_a_track_chain_is_accepted() {
    let mut inner = chain_state();

    set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();

    assert_eq!(
        inner.project.tracks[0].effects[0].sidechain_source,
        Some("drum".to_string())
    );
    assert_eq!(inner.undo.undo[0].label, "effect sidechain");
    assert!(
        document(&inner).contains("sidechain=drum"),
        "the key reaches the saved document:\n{}",
        document(&inner)
    );
}

#[test]
fn a_track_key_on_a_bus_chain_is_accepted() {
    let mut inner = chain_state();

    set_effect_sidechain_inner(&mut inner, "bus:reverb", 0, Some("drum".to_string())).unwrap();

    assert_eq!(
        inner.project.buses[0].effects[0].sidechain_source,
        Some("drum".to_string())
    );
}

#[test]
fn a_bus_key_on_a_bus_chain_is_accepted() {
    let mut inner = chain_state();

    set_effect_sidechain_inner(&mut inner, "bus:reverb", 0, Some("plate".to_string())).unwrap();

    assert_eq!(
        inner.project.buses[0].effects[0].sidechain_source,
        Some("plate".to_string())
    );
}

#[test]
fn a_bus_key_on_the_master_chain_is_accepted() {
    let mut inner = chain_state();

    set_effect_sidechain_inner(&mut inner, "master", 0, Some("plate".to_string())).unwrap();

    assert_eq!(
        inner.project.master.as_ref().unwrap().effects[0].sidechain_source,
        Some("plate".to_string())
    );
}

#[test]
fn clearing_the_key_is_accepted() {
    let mut inner = chain_state();
    set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();

    set_effect_sidechain_inner(&mut inner, "track:0", 0, None).unwrap();

    assert_eq!(inner.project.tracks[0].effects[0].sidechain_source, None);
    assert!(
        !document(&inner).contains("sidechain="),
        "a cleared key leaves no field behind:\n{}",
        document(&inner)
    );
}

#[test]
fn an_unknown_key_returns_the_validator_sentence() {
    let mut inner = chain_state();
    let before = inner.project.clone();

    let error =
        set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("nope".to_string())).unwrap_err();

    assert!(
        error.contains("Unknown sidechain key 'nope' (in track 'lead')"),
        "{error}"
    );
    assert_eq!(inner.project, before);
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn setting_the_same_key_again_adds_no_undo_entry() {
    let mut inner = chain_state();
    set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();
    let entries = inner.undo.undo.len();

    set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();

    assert_eq!(inner.undo.undo.len(), entries);
}

/// Every accepted combination must survive a save and re-open: the documents
/// this writes are the ones `ocp validate` is run against.
#[test]
fn accepted_keys_write_documents_that_validate() {
    let dir = std::env::temp_dir().join("sidechain-validate-fixtures");
    std::fs::create_dir_all(&dir).unwrap();

    let mut cases: Vec<(&str, &str, Option<&str>, AppStateInner)> = Vec::new();
    {
        let mut inner = chain_state();
        set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();
        cases.push(("track-key-on-track", "track:0", Some("drum"), inner));
    }
    {
        let mut inner = chain_state();
        set_effect_sidechain_inner(&mut inner, "bus:reverb", 0, Some("drum".to_string())).unwrap();
        cases.push(("track-key-on-bus", "bus:reverb", Some("drum"), inner));
    }
    {
        let mut inner = chain_state();
        set_effect_sidechain_inner(&mut inner, "bus:reverb", 0, Some("plate".to_string())).unwrap();
        cases.push(("bus-key-on-bus", "bus:reverb", Some("plate"), inner));
    }
    {
        let mut inner = chain_state();
        set_effect_sidechain_inner(&mut inner, "track:0", 0, Some("drum".to_string())).unwrap();
        set_effect_sidechain_inner(&mut inner, "track:0", 0, None).unwrap();
        cases.push(("cleared", "track:0", None, inner));
    }

    for (name, target, expected, inner) in cases {
        let text = document(&inner);
        let parsed = oc_dsl::parse(&text).expect("the saved document parses");
        oc_dsl::validate(&parsed).expect("the saved document validates");
        let effect = match target {
            "bus:reverb" => &parsed.buses[0].effects[0],
            _ => &parsed.tracks[0].effects[0],
        };
        assert_eq!(effect.sidechain_source.as_deref(), expected, "{name}");
        std::fs::write(dir.join(format!("{name}.ocp")), &text).unwrap();
    }
}
