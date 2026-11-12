//! Control-link record: formulas, legacy one-target compatibility, round-trip.

use oc_dsl::ast::{
    ControlLink, ControlLinkSource, ControlLinkTarget, LinkFormula, LinkFormulaInput,
};
use oc_dsl::{parse, serialize, validator};

fn target(track: &str, param: &str, formula: LinkFormula, smoothing_ms: u32) -> ControlLinkTarget {
    ControlLinkTarget {
        track: track.into(),
        param: param.into(),
        formula,
        smoothing_ms,
    }
}

#[test]
fn formula_maps_endpoints_and_midpoint() {
    // Normalized curves: probe 0, 0.5 and 1.
    let normalized = [
        (LinkFormula::Linear, 0.0, 0.5, 1.0),
        (LinkFormula::Inverted, 1.0, 0.5, 0.0),
        (LinkFormula::Exponential, 0.0, 0.25, 1.0),
        (
            LinkFormula::Logarithmic,
            0.0,
            std::f64::consts::FRAC_1_SQRT_2,
            1.0,
        ),
    ];
    for (formula, low, middle, high) in normalized {
        assert!((formula.apply(0.0) - low).abs() < 1e-12, "{formula} at 0");
        assert!(
            (formula.apply(0.5) - middle).abs() < 1e-12,
            "{formula} at midpoint"
        );
        assert!((formula.apply(1.0) - high).abs() < 1e-12, "{formula} at 1");
    }
    // MIDI curves read the raw 0..127 value: probe 0, 64 and 127.
    for (formula, low, middle, high) in [
        (LinkFormula::MidiUnit, 0.0, 64.0 / 127.0, 1.0),
        (LinkFormula::MidiCentered, 0.0, 1.0, 0.0),
    ] {
        assert!((formula.apply(0.0) - low).abs() < 1e-12, "{formula} at 0");
        assert!(
            (formula.apply(64.0) - middle).abs() < 1e-12,
            "{formula} at 64"
        );
        assert!(
            (formula.apply(127.0) - high).abs() < 1e-12,
            "{formula} at 127"
        );
    }
    // Out-of-range and non-finite input never leaves the normalized domain.
    for formula in LinkFormula::ALL {
        for probe in [-5.0, 500.0] {
            let value = formula.apply(probe);
            assert!((0.0..=1.0).contains(&value), "{formula} at {probe}");
        }
        assert_eq!(formula.apply(f64::NAN), 0.0);
    }
}

#[test]
fn formulas_declare_their_input_domain() {
    assert_eq!(LinkFormula::Linear.input(), LinkFormulaInput::Normalized);
    assert_eq!(LinkFormula::Inverted.input(), LinkFormulaInput::Normalized);
    assert_eq!(
        LinkFormula::Exponential.input(),
        LinkFormulaInput::Normalized
    );
    assert_eq!(
        LinkFormula::Logarithmic.input(),
        LinkFormulaInput::Normalized
    );
    assert_eq!(LinkFormula::MidiUnit.input(), LinkFormulaInput::Midi7Bit);
    assert_eq!(
        LinkFormula::MidiCentered.input(),
        LinkFormulaInput::Midi7Bit
    );
}

#[test]
fn parses_fan_out_and_round_trips() {
    let source = r#"@project "Links"
  tempo: 120
  time_signature: 4/4

@track Lead | instrument: synth/lead

@control_link drive
  from: cc(74)
  -> track: Lead param: filter.cutoff formula: midi_unit smoothing: 8ms
  -> track: Lead param: "Filter Cutoff" formula: midi_centered
"#;
    let project = parse(source).expect("fan-out parses");
    assert_eq!(project.properties.control_links.len(), 1);
    let link = &project.properties.control_links[0];
    assert_eq!(link.name, "drive");
    assert_eq!(
        link.source,
        ControlLinkSource::MidiCc {
            channel: 1,
            controller: 74
        }
    );
    assert_eq!(
        link.targets,
        vec![
            target("Lead", "filter.cutoff", LinkFormula::MidiUnit, 8),
            target("Lead", "Filter Cutoff", LinkFormula::MidiCentered, 0),
        ]
    );

    let text = serialize(&project);
    let reparsed = parse(&text).expect("serialized fan-out parses");
    assert_eq!(
        reparsed.properties.control_links,
        project.properties.control_links
    );
    assert_eq!(reparsed, project);
}

#[test]
fn legacy_single_target_spelling_is_one_entry_fan_out() {
    let source = r#"@project "Legacy"
  tempo: 120
  time_signature: 4/4

@track Lead | instrument: synth/lead

@control_link drive
  from: macro(3)
  target: track: Lead param: cutoff formula: exponential smoothing: 12ms
"#;
    let project = parse(source).expect("legacy spelling parses");
    let link = &project.properties.control_links[0];
    assert_eq!(link.targets.len(), 1);
    assert_eq!(
        link.targets[0],
        target("Lead", "cutoff", LinkFormula::Exponential, 12)
    );
    assert_eq!(link.source, ControlLinkSource::Macro { index: 3 });
}

#[test]
fn legacy_json_target_loads_as_one_entry_fan_out() {
    let json = r#"{
        "name": "drive",
        "source": {"kind": "midi_cc", "channel": 2, "controller": 74},
        "target": {"track": "Lead", "param": "cutoff", "formula": "linear"}
    }"#;
    let link: ControlLink = serde_json::from_str(json).expect("legacy JSON loads");
    assert_eq!(
        link.source,
        ControlLinkSource::MidiCc {
            channel: 2,
            controller: 74
        }
    );
    assert_eq!(
        link.targets,
        vec![target("Lead", "cutoff", LinkFormula::Linear, 0)]
    );
}

#[test]
fn validator_rejects_midi_formula_on_a_macro_and_duplicate_rows() {
    let mut project = parse(
        r#"@project "Invalid"
  tempo: 120
  time_signature: 4/4

@track Lead | instrument: synth/lead

@control_link drive
  from: macro(1)
  -> track: Lead param: cutoff formula: midi_unit
  -> track: Lead param: cutoff formula: linear
"#,
    )
    .expect("parses; the validator owns the semantic rules");
    let errors = validator::validate(&project).expect_err("MIDI formula on a macro is refused");
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("raw MIDI"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("bound twice"))
    );

    project.properties.control_links[0].targets.truncate(1);
    project.properties.control_links[0].targets[0].formula = LinkFormula::Linear;
    validator::validate(&project).expect("one normalized row is valid");
}

#[test]
fn learning_a_target_keeps_one_row_per_target() {
    let mut link = ControlLink {
        name: "drive".into(),
        source: ControlLinkSource::MidiCc {
            channel: 1,
            controller: 74,
        },
        targets: vec![target("Lead", "cutoff", LinkFormula::Linear, 0)],
    };
    // Learning the same target again keeps its row and its tuning.
    assert!(!link.learn_target(target("Lead", "cutoff", LinkFormula::Exponential, 20)));
    assert_eq!(link.targets.len(), 1);
    assert_eq!(link.targets[0].formula, LinkFormula::Linear);
    assert_eq!(link.targets[0].smoothing_ms, 0);
    // A second target is one new row.
    assert!(link.learn_target(target("Lead", "volume", LinkFormula::MidiUnit, 0)));
    assert_eq!(link.targets.len(), 2);
}
