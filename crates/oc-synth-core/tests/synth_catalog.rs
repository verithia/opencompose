//! Consistency guards between the declarative synth catalog and the engine ABI.
//!
//! `SynthPatch::set_param` stays the implementation authority; these tests prove
//! the catalog describes it exactly, so a new engine key or enum id cannot land
//! without the catalog (and therefore the GUI) following.

use oc_dsp::schema::synth::{self, SynthValueKind};
use oc_synth_core::synth::SynthPatch;
use serde_json::Value;

fn as_json(patch: &SynthPatch) -> Value {
    serde_json::to_value(patch).expect("patch serializes")
}

fn pick_value(spec_min: f64, spec_max: f64) -> f64 {
    spec_min + (spec_max - spec_min) * 0.5
}

fn field_kind(key: &str) -> SynthValueKind {
    if let Some(param) = synth::find_param(key) {
        return param.kind;
    }
    if let Some((_, field)) = synth::find_family_field(key) {
        return field.kind;
    }
    panic!("{key} is named in the catalog but has no description");
}

#[test]
fn every_catalog_key_is_accepted_by_set_param() {
    let mut patch = SynthPatch::default();
    let loader_only = [
        "wavetable.file",
        "open_synth_state_file",
        "serum_file",
        "serum_root",
    ];
    for key in synth::parameter_keys() {
        let kind = field_kind(&key);
        match kind {
            SynthValueKind::String => {
                assert!(
                    loader_only.contains(&key.as_str()),
                    "{key} is a string parameter outside the loader whitelist"
                );
                assert!(
                    !patch.set_param(&key, 0.0),
                    "{key} must be applied by the loader, not set_param"
                );
            }
            SynthValueKind::Array => {
                assert!(
                    oc_synth_core::synth::is_array_param(&key),
                    "{key} is catalogued as an array but the loader rejects it"
                );
            }
            SynthValueKind::Bool => {
                assert!(patch.set_param(&key, 1.0), "set_param rejected {key}");
                assert!(patch.set_param(&key, 0.0), "set_param rejected {key}");
            }
            SynthValueKind::Enum => {
                let spec = synth::find_param(&key)
                    .map(|param| (param.min, param.max))
                    .or_else(|| {
                        synth::find_family_field(&key).map(|(_, field)| (field.min, field.max))
                    })
                    .expect("enum spec");
                assert!(patch.set_param(&key, spec.0), "set_param rejected {key}");
                assert!(patch.set_param(&key, spec.1), "set_param rejected {key}");
            }
            SynthValueKind::Continuous | SynthValueKind::Metadata => {
                let (min, max) = synth::find_param(&key)
                    .map(|param| (param.min, param.max))
                    .or_else(|| {
                        synth::find_family_field(&key).map(|(_, field)| (field.min, field.max))
                    })
                    .expect("numeric spec");
                assert!(
                    patch.set_param(&key, pick_value(min, max)),
                    "set_param rejected {key}"
                );
            }
        }
    }
}

fn distinct_field_values(
    key: &str,
    ids: impl IntoIterator<Item = usize>,
    read: impl Fn(&Value) -> Value,
) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        let mut patch = SynthPatch::default();
        assert!(
            patch.set_param(key, id as f64),
            "set_param rejected {key}={id}"
        );
        seen.insert(read(&as_json(&patch)).to_string());
    }
    seen.len()
}

fn assert_enum_ids(key: &str, spec: &synth::SynthEnum, read: impl Fn(&Value) -> Value) {
    let count = distinct_field_values(key, spec.options.iter().map(|option| option.id), read);
    assert_eq!(
        count,
        spec.options.len(),
        "{key} collapses catalog ids onto fewer engine values"
    );
}

#[test]
fn catalog_enums_match_the_engine_numeric_abi() {
    let catalog = synth::synth_catalog();
    assert_enum_ids("osc.a.waveform", &catalog.waveforms, |json| {
        json["oscillator_a_waveform"].clone()
    });
    assert_enum_ids("osc.a.warp_mode", &catalog.warp_modes, |json| {
        json["oscillator_a_warp_mode"].clone()
    });
    assert_enum_ids("filter.model", &catalog.filter_models, |json| {
        json["filter_model"].clone()
    });
    assert_enum_ids("filter.mode", &catalog.filter_modes, |json| {
        json["filter_mode"].clone()
    });
    assert_enum_ids("filter.routing", &catalog.filter_routings, |json| {
        json["filter_routing"].clone()
    });
    assert_enum_ids("lfo.shape", &catalog.lfo_shapes, |json| {
        json["lfo_shape_type"].clone()
    });
    assert_enum_ids("sub.waveform", &catalog.sub_shapes, |json| {
        json["sub_waveform"].clone()
    });
    assert_enum_ids("quality.oversampling", &catalog.quality_modes, |json| {
        json["oscillator_quality"].clone()
    });
    assert_enum_ids("mod.1.source", &catalog.mod_sources, |json| {
        json["mod_routes"][0]["source"].clone()
    });
    assert_enum_ids("mod.1.destination", &catalog.mod_destinations, |json| {
        json["mod_routes"][0]["destination"].clone()
    });
    assert_enum_ids("fxmod.1.parameter", &catalog.fx_param_kinds, |json| {
        json["fx_mod_routes"][0]["parameter"].clone()
    });
}

#[test]
fn fx_type_labels_match_their_numeric_ids() {
    for fx in synth::synth_catalog().fx_types {
        let resolved = oc_synth_core::synth::SynthFxType::from_label(fx.id)
            .unwrap_or_else(|| panic!("{} is not an engine FX label", fx.id));
        assert_eq!(resolved.index(), fx.index, "{} index", fx.id);
    }
}

/// The four enum-only destinations have no numeric id; ids 35..=39 fall back to
/// Filter Cutoff. Pin the gap so extending the engine forces a catalog update.
#[test]
fn destination_abi_stops_where_the_catalog_says() {
    let ids = synth::synth_catalog().mod_destinations.options.len();
    assert_eq!(ids, 35);
    for id in ids..40 {
        let mut patch = SynthPatch::default();
        patch.set_param("mod.1.destination", id as f64);
        let value = as_json(&patch)["mod_routes"][0]["destination"].clone();
        assert_eq!(
            value,
            Value::String("FilterCutoff".into()),
            "destination id {id} now resolves; extend the catalog"
        );
    }
}

fn enum_spec(key: &str) -> Option<(f64, f64)> {
    if let Some(param) = synth::find_param(key) {
        return (param.kind == SynthValueKind::Enum).then_some((param.min, param.max));
    }
    let (_, field) = synth::find_family_field(key)?;
    (field.kind == SynthValueKind::Enum).then_some((field.min, field.max))
}

/// The normalized domain must span the whole catalog enum, so a widened engine
/// enum cannot land without the normalization table following. Comparing the
/// serialized patches checks the real engine ABI, not just the declared range.
#[test]
fn every_catalog_enum_spans_the_normalized_domain() {
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for key in synth::parameter_keys() {
        let Some((min, max)) = enum_spec(&key) else {
            continue;
        };
        checked += 1;
        // Give the FX rack its maximum size so `fx.N.type` is a real write
        // rather than a no-op on an empty rack.
        let base = SynthPatch {
            fx: (0..16)
                .map(|_| {
                    oc_synth_core::synth::SynthFx::new(oc_synth_core::synth::SynthFxType::Drive)
                })
                .collect(),
            ..Default::default()
        };
        let mut physical = base.clone();
        assert!(
            physical.set_param(&key, min),
            "set_param rejected {key}={min}"
        );
        assert!(
            physical.set_param(&key, max),
            "set_param rejected {key}={max}"
        );
        let mut normalized = base;
        normalized.set_normalized_param(&key, 0.0);
        normalized.set_normalized_param(&key, 1.0);
        if as_json(&physical) != as_json(&normalized) {
            failures.push(format!(
                "{key}: normalized 0..1 does not span the catalog range {min}..{max}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "normalization table does not cover the engine enum domain:\n{}",
        failures.join("\n")
    );
    assert!(checked >= 100, "expected every enum key, checked {checked}");
}

/// Every dotted string literal inside `set_param` must be a catalogued key.
#[test]
fn set_param_literals_are_all_catalogued() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/synth.rs"));
    let start = source.find("pub fn set_param").expect("set_param");
    let end = source[start..]
        .find("pub fn set_normalized_param")
        .map(|offset| start + offset)
        .expect("set_normalized_param");
    let mut rest = &source[start..end];
    let mut checked = 0usize;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        let literal = &after[..close];
        rest = &after[close + 1..];
        if !literal.contains('.') || literal.ends_with('.') {
            continue;
        }
        // Field suffixes inside the indexed-family matchers, not full keys.
        if matches!(literal, "shape.points") {
            continue;
        }
        checked += 1;
        assert!(
            synth::is_known_param(literal),
            "set_param handles {literal}, which the catalog does not describe"
        );
    }
    assert!(checked > 100, "only scanned {checked} key literals");
}
