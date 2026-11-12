//! The effect bypass field: one command for every device, one undo entry, and
//! no second spelling inside the parameter list.

use oc_dsl::ast;

use crate::commands::fx::{
    set_effect_bypass_inner, set_effect_param_inner, set_effect_params_inner,
};
use crate::state::AppStateInner;

use super::support::test_state;

const CHAIN: &str = r#"@ocp "0.2.0"

@project "bypass"
  tempo: 120

@track lead | instrument: synth/lead_square
  effects:
    - compression: threshold=-20dB, ratio=4:1
    - delay: mix=20%
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;

fn chain_state() -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(CHAIN).unwrap();
    oc_dsl::validate(&inner.project).unwrap();
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

#[test]
fn toggles_the_field_on_and_off_and_undo_restores_it() {
    let mut inner = chain_state();
    assert!(!inner.project.tracks[0].effects[0].bypass);

    set_effect_bypass_inner(&mut inner, "track:0", 0, true).unwrap();
    assert!(inner.project.tracks[0].effects[0].bypass);
    assert!(
        inner
            .dsl_source
            .contains("- compression: threshold=-20dB, ratio=4:1, bypass=true"),
        "the field reaches the DSL text:\n{}",
        inner.dsl_source
    );
    assert_eq!(inner.undo.undo[0].label, "bypass effect");

    set_effect_bypass_inner(&mut inner, "track:0", 0, false).unwrap();
    assert!(!inner.project.tracks[0].effects[0].bypass);
    assert!(!inner.dsl_source.contains("bypass=true"));

    inner.undo.undo(&mut inner.project).unwrap();
    assert!(
        inner.project.tracks[0].effects[0].bypass,
        "undoing the off step restores the on step"
    );
}

#[test]
fn a_missing_effect_index_refuses_without_an_edit() {
    let mut inner = chain_state();
    let before = inner.project.clone();

    let error = set_effect_bypass_inner(&mut inner, "track:0", 9, true).unwrap_err();
    assert_eq!(error, "Effect index 9 out of range");
    assert!(inner.undo.undo.is_empty(), "a refusal adds no undo entry");
    assert_eq!(inner.project, before, "a refusal leaves the project alone");
}

#[test]
fn a_bypass_parameter_is_refused_so_the_field_has_one_meaning() {
    let mut inner = chain_state();
    let before = inner.project.clone();

    let error = set_effect_param_inner(&mut inner, "track:0", 0, "bypass", "true").unwrap_err();
    assert!(error.contains("effect field"), "{error}");
    assert!(error.contains("set_effect_bypass"), "{error}");
    assert!(inner.undo.undo.is_empty(), "a refusal adds no undo entry");
    assert_eq!(inner.project, before, "a refusal leaves the project alone");
}

#[test]
fn a_batch_that_names_bypass_is_refused_atomically() {
    let mut inner = chain_state();
    let before = inner.project.clone();

    let error = set_effect_params_inner(
        &mut inner,
        "track:0",
        0,
        &[
            ("threshold".to_string(), "-30dB".to_string()),
            ("bypass".to_string(), "true".to_string()),
        ],
    )
    .unwrap_err();
    assert!(error.contains("effect field"), "{error}");
    assert_eq!(
        inner.project, before,
        "the valid half of a refused batch must not land"
    );
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn the_parameter_name_is_reserved_on_a_plugin_too() {
    let mut inner = chain_state();
    inner.project.tracks[0].effects.push(ast::Effect {
        effect_type: ast::EffectType::Plugin("test.fake.effect".into()),
        params: vec![],
        bypass: false,
        sidechain_source: None,
    });

    let error = set_effect_param_inner(&mut inner, "track:0", 2, "bypass", "true").unwrap_err();
    assert!(error.contains("effect field"), "{error}");
    assert!(
        inner.project.tracks[0].effects[2].params.is_empty(),
        "a plugin keeps no bypass parameter either"
    );
}
