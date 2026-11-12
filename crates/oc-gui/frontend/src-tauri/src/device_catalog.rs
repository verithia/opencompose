//! Bounded agent access to the same instrument schema used by the editor.
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    #[schemars(description = "builtin.open_synth; omit for mixer effects")]
    instrument: Option<String>,
    #[schemars(description = "Project parameter key prefix, e.g. osc.a. or filter.")]
    prefix: Option<String>,
    #[schemars(description = "Case-insensitive schema text filter")]
    query: Option<String>,
    #[schemars(description = "Exact group id from returned groups")]
    group: Option<String>,
    #[schemars(description = "Page offset, default 0")]
    offset: Option<usize>,
    #[schemars(description = "Entries per page, default 24, maximum 64")]
    limit: Option<usize>,
}

impl ToolArguments for Query {
    const ID: ToolId = ToolId::DevicesCatalog;
    const FIELD_ORDER: &'static [&'static str] =
        &["instrument", "prefix", "query", "group", "offset", "limit"];
}

#[cfg(test)]
pub(crate) fn catalog(args: &Value) -> Result<Value, String> {
    let request = Query::decode(args).map_err(|error| error.to_string())?;
    catalog_request(&request)
}

pub(crate) fn catalog_request(q: &Query) -> Result<Value, String> {
    use oc_engine::dsp::schema::synth;
    let Some(instrument) = q.instrument.as_deref() else {
        if q.prefix.is_some()
            || q.query.is_some()
            || q.group.is_some()
            || q.offset.is_some()
            || q.limit.is_some()
        {
            return Err(
                "catalog filters need an instrument: call {\"instrument\":\"builtin.open_synth\",\"prefix\":\"filter\"} for synth parameters, or {} with no arguments for mixer devices"
                    .into(),
            );
        }
        let mut value = oc_engine::dsp::schema::catalog_json();
        // UI/schema IDs use hyphens; effect entries use canonical underscore keywords.
        // Keep both so callers never have to guess which namespace a tool accepts.
        for device in value["devices"]
            .as_array_mut()
            .expect("device catalog array")
        {
            let id = device["id"].as_str().expect("device schema id").to_owned();
            let primary = json!(oc_device_schema::primary_params(&id));
            device["dsl_id"] = json!(id.replace('-', "_"));
            // The ten to twenty controls that carry the device; a model that
            // reads every parameter wastes context on names it never sets.
            device["primary"] = primary;
        }
        return Ok(value);
    };
    if instrument != synth::SYNTH_PLUGIN_ID {
        return Err(
            "Supported instrument: builtin.open_synth; omit arguments for mixer devices".into(),
        );
    }
    let limit = q.limit.unwrap_or(24);
    if !(1..=64).contains(&limit) {
        return Err("limit must be between 1 and 64".into());
    }
    for filter in [&q.prefix, &q.query, &q.group].into_iter().flatten() {
        if filter.len() > 128 {
            return Err("Catalog filters must be at most 128 bytes".into());
        }
    }
    let catalog = synth::synth_catalog();
    if let Some(group) = &q.group
        && !catalog.groups.iter().any(|g| g.id == group)
    {
        return Err(format!(
            "Unknown group '{group}'; query instrument without group to list available groups"
        ));
    }
    let prefix = q.prefix.as_deref().unwrap_or("");
    let query = q.query.as_deref().unwrap_or("").to_lowercase();
    let matches = |group: &str, value: &Value| {
        q.group.as_deref().is_none_or(|g| g == group)
            && (query.is_empty() || value.to_string().to_lowercase().contains(&query) || {
                let mut ids = std::collections::BTreeSet::new();
                collect(value, &mut ids);
                ids.iter().filter_map(|id| synth::find_enum(id)).any(|e| {
                    serde_json::to_string(e)
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&query)
                })
            })
    };
    // The controls that carry the sound come first, so a model that reads one
    // page starts with the ten to twenty parameters that matter.
    let mut ordered: Vec<&synth::SynthParam> = catalog.params.iter().collect();
    if prefix.is_empty() && query.is_empty() && q.group.is_none() {
        ordered.sort_by_key(|param| !catalog.primary.contains(&param.canonical()));
    }
    let mut entries = Vec::new();
    for param in ordered {
        let value = json!({"type":"parameter", "schema":param});
        if param
            .ids
            .iter()
            .chain(param.aliases.iter())
            .any(|id| id.starts_with(prefix))
            && matches(param.group, &value)
        {
            entries.push(value);
        }
    }
    for family in catalog.families {
        let key_matches = family.prefix.starts_with(prefix)
            || (family.first..=family.last).any(|index| {
                family.fields.iter().any(|field| {
                    let key = if field.suffix.is_empty() {
                        format!("{}{index}", family.prefix)
                    } else {
                        format!("{}{index}.{}", family.prefix, field.suffix)
                    };
                    key.starts_with(prefix)
                })
            });
        let value = json!({"type":"family", "schema":family});
        if key_matches && matches(family.group, &value) {
            entries.push(value);
        }
    }
    for effect in catalog.fx_types {
        let value = json!({"type":"synth_fx", "schema":effect});
        let key_matches = prefix.is_empty()
            || "fx.".starts_with(prefix)
            || catalog.families.iter().filter(|f| f.id == "fx").any(|f| {
                (f.first..=f.last).any(|slot| {
                    format!("fx.{slot}.type").starts_with(prefix)
                        || effect
                            .params
                            .iter()
                            .any(|p| format!("fx.{slot}.{}", p.key).starts_with(prefix))
                })
            });
        if key_matches && matches("fx", &value) {
            entries.push(value);
        }
    }
    let total = entries.len();
    let offset = q.offset.unwrap_or(0).min(total);
    let end = offset.saturating_add(limit).min(total);
    let page = &entries[offset..end];
    let mut enum_ids = std::collections::BTreeSet::new();
    fn collect(value: &Value, ids: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if (key == "enumId" || key == "enum_id")
                        && let Some(id) = value.as_str()
                    {
                        ids.insert(id.to_owned());
                    }
                    collect(value, ids);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, ids);
                }
            }
            _ => {}
        }
    }
    for entry in page {
        collect(entry, &mut enum_ids);
    }
    let enums: Vec<_> = enum_ids
        .iter()
        .filter_map(|id| synth::find_enum(id))
        .collect();
    Ok(
        json!({"instrument":instrument, "version":catalog.version, "groups":catalog.groups,
        "primary":catalog.primary,
        "entries":page, "enums":enums, "total":total, "offset":offset, "limit":limit,
        "next_offset": if end < total { Some(end) } else { None },
        "usage":"Concrete parameter ids/aliases and indexed family fields are project keys; use presets_set_params to apply. Status distinguishes working, partial, noOp and unreachable controls. Defaults are schema defaults, not current preset values."}),
    )
}

pub(crate) fn is_open_synth_preset(project: &oc_dsl::ast::Project, name: &str) -> bool {
    let mut current = name;
    for _ in 0..=project.presets.len() {
        let Some(preset) = project.presets.iter().find(|p| p.name == current) else {
            return false;
        };
        if let Some(plugin) = preset.plugin.as_deref() {
            return plugin == "builtin.open_synth";
        }
        let Some(parent) = preset.extends.as_deref() else {
            return false;
        };
        current = parent;
    }
    false
}

pub(crate) fn builtin_parameter_hint(project: &oc_dsl::ast::Project, name: &str) -> Option<String> {
    if !is_open_synth_preset(project, name) {
        return None;
    }
    Some(format!(
        "Preset '{name}' uses built-in OpenSynth, not a loadable external plugin. Use devices_catalog with instrument: builtin.open_synth and optional prefix (e.g. osc.a.) for parameter ranges, enums and defaults; use project_extract to inspect current preset values."
    ))
}

/// Validate described controls before mutation; opaque state/assets and open
/// extension fields keep their existing loader-specific validation.
pub(crate) fn validate_synth_value(
    key: &str,
    value: &oc_dsl::ast::ParamValue,
) -> Result<(), String> {
    use oc_dsl::ast::ParamValue as V;
    use oc_engine::dsp::schema::synth::{self, SynthUnit as U, SynthValueKind as K};
    let (unit, kind, min, max, enum_id) = if let Some(p) = synth::find_param(key) {
        (p.unit, p.kind, p.min, p.max, p.enum_id)
    } else if let Some((_, p)) = synth::find_family_field(key) {
        (p.unit, p.kind, p.min, p.max, p.enum_id)
    } else {
        return Ok(());
    };
    let expected = if matches!(unit, U::Percent | U::Fraction) {
        format!("unitless linear value in {min}..{max} (not dB or a percent suffix)")
    } else {
        format!("{:?} value in {min}..{max}", unit)
    };
    let error = || {
        format!(
            "Invalid OpenSynth parameter '{key}': expected {expected}; received {value:?}. Use devices_catalog with instrument builtin.open_synth and prefix '{key}' for the authoritative schema."
        )
    };
    match kind {
        K::Metadata => return Ok(()),
        K::String => {
            return if matches!(value, V::String(_)) {
                Ok(())
            } else {
                Err(error())
            };
        }
        K::Array => {
            return if matches!(value, V::Array(v) if v.iter().all(|n| n.is_finite())) {
                Ok(())
            } else {
                Err(error())
            };
        }
        _ => {}
    }
    // Synth FX type names are explicitly accepted by the loader, unlike
    // arbitrary enum labels on ordinary numeric controls.
    if key.starts_with("fx.")
        && key.ends_with(".type")
        && let V::String(label) = value
    {
        return if synth::synth_catalog()
            .fx_types
            .iter()
            .any(|fx| fx.id == label)
        {
            Ok(())
        } else {
            Err(error())
        };
    }
    let number = match value {
        V::Number(n) => *n,
        V::Db(n) if unit == U::Db => *n,
        V::Frequency(n) if unit == U::Hz => *n,
        V::Cents(n) if unit == U::Cents => *n,
        V::Time(n) if unit == U::Seconds => *n,
        V::Bool(flag) if kind == K::Bool => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        // Generic preset parsing retains % numerically, rather than dividing
        // by 100. Reject it instead of silently changing a normalized control.
        _ => return Err(error()),
    };
    if !number.is_finite() || number < min || number > max {
        return Err(error());
    }
    if kind == K::Bool && number != 0.0 && number != 1.0 {
        return Err(error());
    }
    if kind == K::Enum {
        if number.fract() != 0.0 {
            return Err(error());
        }
        if let Some(options) = enum_id.and_then(synth::find_enum)
            && !options.options.iter().any(|o| o.id as f64 == number)
        {
            return Err(error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synth_value_validation_rejects_wrong_units_ranges_and_nonfinite() {
        use oc_dsl::ast::ParamValue as V;
        for value in [
            V::Db(-3.0),
            V::Number(-3.0),
            V::Number(1.1),
            V::Number(f64::NAN),
            V::Percentage(85.0),
        ] {
            let error = validate_synth_value("output.gain", &value).unwrap_err();
            assert!(error.contains("unitless linear value in 0..1"));
            assert!(error.contains("devices_catalog"));
        }
        for (key, value) in [
            ("output.gain", V::Number(0.7)),
            ("amp.attack", V::Time(0.015)),
            ("filter.cutoff", V::Frequency(9500.0)),
            ("osc.a.detune", V::Cents(24.0)),
            ("osc.a.waveform", V::Number(1.0)),
        ] {
            validate_synth_value(key, &value).unwrap();
        }
        assert!(validate_synth_value("osc.a.waveform", &V::Number(1.5)).is_err());
        assert!(validate_synth_value("mod.1.source", &V::Number(10000.0)).is_err());
    }

    #[test]
    fn synth_value_validation_batch_is_atomic_and_preserves_external_plugins() {
        use crate::commands::apply_preset_params;
        let mut project = oc_dsl::parse("@project\n@preset base\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.3}\n@preset child\n  extends: base\n@preset external\n  source: vst3\n  plugin: \"test\"\n").unwrap();
        let before = project.clone();
        let error = apply_preset_params(
            &mut project,
            "child",
            vec![
                ("osc.a.unison".into(), Some("7".into())),
                ("output.gain".into(), Some("-3dB".into())),
            ],
        )
        .unwrap_err();
        assert!(error.contains("unitless linear"));
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        apply_preset_params(
            &mut project,
            "child",
            vec![("output.gain".into(), Some("0.7".into()))],
        )
        .unwrap();
        apply_preset_params(
            &mut project,
            "external",
            vec![("output.gain".into(), Some("-3dB".into()))],
        )
        .unwrap();
        apply_preset_params(&mut project, "child", vec![("output.gain".into(), None)]).unwrap();
    }

    #[test]
    fn synth_catalog_builtin_error_hint_resolves_inherited_presets() {
        let project = oc_dsl::parse("@project\n@preset base\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@preset child\n  extends: base\n").unwrap();
        let hint = builtin_parameter_hint(&project, "child").unwrap();
        assert!(hint.contains("devices_catalog"));
        assert!(builtin_parameter_hint(&project, "missing").is_none());
    }

    #[test]
    fn the_first_synth_page_starts_with_the_primary_controls() {
        let value = catalog(&json!({"instrument": "builtin.open_synth"})).unwrap();
        let primary: Vec<&str> = value["primary"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        assert!(primary.len() <= 24);
        let first = value["entries"][0]["schema"]["ids"][0].as_str().unwrap();
        assert!(
            primary.contains(&first),
            "first entry {first} is not primary"
        );
    }

    #[test]
    fn synth_catalog_preserves_effects_default() {
        let value = catalog(&json!({})).unwrap();
        let mut original_shape = value.clone();
        for device in original_shape["devices"].as_array_mut().unwrap() {
            let object = device.as_object_mut().unwrap();
            object.remove("dsl_id");
            object.remove("primary");
        }
        assert_eq!(original_shape, oc_engine::dsp::schema::catalog_json());
        // The primary set rides beside the parameters, so a model can start
        // with the controls that carry the device.
        let devices = value["devices"].as_array().unwrap();
        let reverb = devices.iter().find(|d| d["id"] == "reverb").unwrap();
        assert!(
            reverb["primary"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "decay")
        );
        assert!(catalog(&json!({"prefix":"osc."})).is_err());
    }
    #[test]
    fn mixer_catalog_dsl_ids_parse_to_the_same_device() {
        let value = catalog(&json!({})).unwrap();
        for device in value["devices"].as_array().unwrap() {
            let keyword = device["dsl_id"].as_str().unwrap();
            let source = format!(
                "@project\n@track test | instrument: piano/acoustic_grand\n  effects:\n    - {keyword}\n"
            );
            let project = oc_dsl::parse(&source).unwrap_or_else(|e| panic!("{keyword}: {e}"));
            assert_eq!(
                project.tracks[0].effects[0].effect_type.device_id(),
                device["id"].as_str()
            );
        }
        let de_esser = value["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == "de-esser")
            .unwrap();
        assert_eq!(de_esser["dsl_id"], "de_esser");
    }

    #[test]
    fn synth_catalog_oscillator_schema_is_authoritative_and_enums_present() {
        let value =
            catalog(&json!({"instrument":"builtin.open_synth","prefix":"osc.a.waveform"})).unwrap();
        let entries = value["entries"].as_array().unwrap();
        assert!(!entries.is_empty());
        let expected = oc_engine::dsp::schema::synth::find_param("osc.a.waveform").unwrap();
        assert_eq!(
            entries[0]["schema"],
            serde_json::to_value(expected).unwrap()
        );
        assert!(!value["enums"].as_array().unwrap().is_empty());
    }
    #[test]
    fn synth_catalog_families_and_paging_are_bounded() {
        let value =
            catalog(&json!({"instrument":"builtin.open_synth","prefix":"mod.1.source"})).unwrap();
        assert_eq!(value["entries"][0]["type"], "family");
        assert!(!value["enums"].as_array().unwrap().is_empty());
        let first = catalog(&json!({"instrument":"builtin.open_synth","limit":1})).unwrap();
        let second =
            catalog(&json!({"instrument":"builtin.open_synth","limit":1,"offset":1})).unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 1);
        assert_eq!(first["next_offset"], 1);
        assert_ne!(first["entries"], second["entries"]);
        assert!(catalog(&json!({"instrument":"builtin.open_synth","limit":65})).is_err());
        assert!(catalog(&json!({"instrument":"other"})).is_err());
        let fx = catalog(&json!({"instrument":"builtin.open_synth","prefix":"fx.16."})).unwrap();
        assert!(
            fx["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "synth_fx")
        );
        let saw = catalog(
            &json!({"instrument":"builtin.open_synth","query":"saw","prefix":"osc.a.waveform"}),
        )
        .unwrap();
        assert!(!saw["entries"].as_array().unwrap().is_empty());
    }
}
