//! Project-parameter ABI for the expanded synth FX rack.

use oc_dsl::ast::ParamValue;
use oc_synth::synth::{SynthFxType, SynthPatch};

fn number(value: f64) -> ParamValue {
    ParamValue::Number(value)
}

fn text(value: &str) -> ParamValue {
    ParamValue::String(value.into())
}

#[test]
fn new_effects_round_trip_through_project_params() {
    let params = vec![
        ("fx.count".to_string(), number(4.0)),
        ("fx.1.type".to_string(), text("distortion")),
        ("fx.1.drive".to_string(), number(0.75)),
        ("fx.1.tone".to_string(), number(0.4)),
        ("fx.1.level".to_string(), number(0.6)),
        ("fx.1.mix".to_string(), number(0.9)),
        ("fx.2.type".to_string(), number(7.0)),
        ("fx.2.rate".to_string(), number(1.5)),
        ("fx.2.depth".to_string(), number(0.8)),
        ("fx.2.feedback".to_string(), number(0.4)),
        ("fx.2.mix".to_string(), number(0.5)),
        ("fx.3.type".to_string(), text("flanger")),
        ("fx.3.rate".to_string(), number(0.2)),
        ("fx.3.depth".to_string(), number(0.6)),
        ("fx.3.feedback".to_string(), number(0.7)),
        ("fx.3.mix".to_string(), number(0.45)),
        ("fx.4.type".to_string(), text("filter")),
        ("fx.4.cutoff".to_string(), number(8_000.0)),
        ("fx.4.resonance".to_string(), number(0.5)),
        ("fx.4.mode".to_string(), number(2.0)),
        ("fx.4.mix".to_string(), number(1.0)),
    ];
    let mut patch = SynthPatch::default();
    patch.apply_params(Some(&params));

    assert_eq!(patch.fx.len(), 4);
    assert_eq!(patch.fx[0].fx_type, SynthFxType::Distortion);
    assert_eq!(patch.fx[0].param("drive"), Some(0.75));
    assert_eq!(patch.fx[0].param("mix"), Some(0.9));
    assert_eq!(patch.fx[1].fx_type, SynthFxType::Phaser);
    assert_eq!(patch.fx[1].param("rate"), Some(1.5));
    assert_eq!(patch.fx[1].param("feedback"), Some(0.4));
    assert_eq!(patch.fx[2].fx_type, SynthFxType::Flanger);
    assert_eq!(patch.fx[2].param("depth"), Some(0.6));
    assert_eq!(patch.fx[2].param("mix"), Some(0.45));
    assert_eq!(patch.fx[3].fx_type, SynthFxType::Filter);
    assert_eq!(patch.fx[3].param("cutoff"), Some(8_000.0));
    assert_eq!(patch.fx[3].param("mode"), Some(2.0));

    let json = serde_json::to_vec(&patch).expect("serialize patch");
    let restored: SynthPatch = serde_json::from_slice(&json).expect("deserialize patch");
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
}
