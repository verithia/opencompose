//! Bridges the DSL's effect types and typed parameter values onto the pure
//! device schema in `oc-dsp`. Keeping this here lets `oc-dsp` stay free of any
//! dependency on the language crate.

use std::collections::HashSet;

use oc_dsl::ast::{EffectType, ParamValue, Project};
use oc_dsp::schema::{
    DeviceSchema, ParamKind, ParamSchema, ParamUnit, Shorthand, ShorthandKind, ShorthandSource,
    ShorthandTarget, find_device, shorthands_for,
};

/// Schema for an effect device, if the catalog declares one.
pub fn device_for_effect(effect: &EffectType) -> Option<&'static DeviceSchema> {
    effect.device_id().and_then(find_device)
}

/// Validate a static mixer effect value in its physical schema coordinates.
/// Typed percentages on fractional mixer controls use conventional /100 scaling,
/// matching the effect factory. Automation deliberately retains its historical
/// physical-value interpretation through `validate_param` instead.
pub fn validate_static_param(
    device: &DeviceSchema,
    id: &str,
    value: &ParamValue,
) -> Result<f64, String> {
    if device
        .param(id)
        .is_some_and(|param| param.unit == ParamUnit::Fraction)
        && let ParamValue::Percentage(percent) = value
    {
        return validate_param(device, id, &ParamValue::Number(percent / 100.0));
    }
    validate_param(device, id, value)
}

/// Resolve a named parameter of `device` and convert `value` into the device
/// unit, validating the range. Returns the numeric value in that unit.
pub fn validate_param(device: &DeviceSchema, id: &str, value: &ParamValue) -> Result<f64, String> {
    let param = device.param(id).ok_or_else(|| {
        let expected = device
            .params
            .iter()
            .map(|param| param.id)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "effect '{}' has no parameter '{id}'; expected one of: {expected}",
            device.id
        )
    })?;
    let number = match param.kind {
        ParamKind::Bool => bool_value(value),
        ParamKind::Enum => enum_value(param, value),
        ParamKind::Continuous => param_value_in_unit(param, value),
    }
    .ok_or_else(|| {
        let expected = if param.kind == ParamKind::Enum {
            param.options.join(", ")
        } else {
            param.unit.as_str().to_string()
        };
        format!(
            "effect '{}' parameter '{}' expects {}; got {value:?}",
            device.id, param.id, expected
        )
    })?;
    if !number.is_finite() || number < param.min || number > param.max {
        return Err(format!(
            "effect '{}' parameter '{}' = {number} is outside {}..{}",
            device.id, param.id, param.min, param.max
        ));
    }
    Ok(number)
}

/// A built-in effect after legacy shorthand expansion.
pub struct ResolvedEffect {
    pub effect_type: EffectType,
    pub params: Vec<(String, ParamValue)>,
    pub warnings: Vec<String>,
}

/// Expand the retired vocabulary onto current controls; explicit controls win.
/// An unrepresentable shorthand warns and yields no parameter.
pub fn resolve_effect(effect_type: &EffectType, params: &[(String, ParamValue)]) -> ResolvedEffect {
    let unchanged = || ResolvedEffect {
        effect_type: effect_type.clone(),
        params: params.to_vec(),
        warnings: Vec::new(),
    };
    let Some(source) = device_for_effect(effect_type) else {
        return unchanged();
    };
    let rules: Vec<&Shorthand> = shorthands_for(source.id).collect();
    if rules.is_empty() {
        return unchanged();
    }
    let legacy: HashSet<&str> = rules.iter().map(|rule| rule.name).collect();
    if !params
        .iter()
        .any(|(name, _)| legacy.contains(name.as_str()))
    {
        return unchanged();
    }
    let target_id = rules
        .iter()
        .find(|rule| params.iter().any(|(name, _)| name == rule.name))
        .map(|rule| rule.target_device)
        .unwrap_or(source.id);
    let Some(target) = find_device(target_id) else {
        return unchanged();
    };
    let Some(resolved_type) = effect_for_device(target.id) else {
        return unchanged();
    };

    let mut out: Vec<(String, ParamValue)> = Vec::new();
    let mut explicit: HashSet<String> = HashSet::new();
    let mut bands: Vec<String> = Vec::new();
    for (name, value) in params {
        if legacy.contains(name.as_str()) {
            continue;
        }
        if source.id == "eq"
            && let Some(target_name) = eq_explicit_target(name)
        {
            out.push((target_name.to_string(), value.clone()));
            explicit.insert(target_name.to_string());
            if let Some(index) = band_index(target_name)
                && !bands.iter().any(|band| band == index)
            {
                bands.push(index.to_string());
            }
            continue;
        }
        if target.param(name).is_some() {
            out.push((name.clone(), value.clone()));
            explicit.insert(name.clone());
            continue;
        }
        // Unknown names stay in place so the checked factory rejects them.
        out.push((name.clone(), value.clone()));
    }
    for index in &bands {
        let enabled = format!("band{index}_enabled");
        if explicit.insert(enabled.clone()) {
            out.push((enabled, ParamValue::Bool(true)));
        }
        if let Some(label) = band_type(index) {
            let type_id = format!("band{index}_type");
            if explicit.insert(type_id.clone()) {
                out.push((type_id, ParamValue::String(label.to_string())));
            }
        }
    }

    let mut warning_keys: HashSet<String> = HashSet::new();
    let mut warnings = Vec::new();
    for (name, value) in params {
        if !legacy.contains(name.as_str()) {
            continue;
        }
        let candidates: Vec<&Shorthand> = rules
            .iter()
            .copied()
            .filter(|rule| rule.name == name)
            .collect();
        let Some(rule) = candidates
            .into_iter()
            .find(|rule| shorthand_matches(rule, value))
        else {
            let key = format!("{}.{}.{}", source.id, name, display_value(value));
            if warning_keys.insert(key) {
                warnings.push(format!(
                    "{}: legacy shorthand '{}={}' cannot be represented as {}; ignored",
                    source.id,
                    name,
                    display_value(value),
                    target.id
                ));
            }
            continue;
        };
        let (gain, frequency) = shorthand_inputs(rule, value);
        for target_spec in rule.targets {
            if explicit.contains(target_spec.param) {
                continue;
            }
            if let Some(converted) = target_value(target, target_spec, gain, frequency) {
                out.push((target_spec.param.to_string(), converted));
                explicit.insert(target_spec.param.to_string());
            }
        }
        let label = match rule.value {
            ShorthandKind::Word(word) => format!("{name}={word}"),
            _ => name.clone(),
        };
        if warning_keys.insert(format!("{}.{}", source.id, label)) {
            warnings.push(format!(
                "{}: legacy shorthand '{}' {}",
                source.id, label, rule.note
            ));
        }
    }

    ResolvedEffect {
        effect_type: resolved_type,
        params: out,
        warnings,
    }
}

/// One deduplicated warning per distinct legacy shorthand in the project.
pub fn legacy_warnings(project: &Project) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    let effects = project
        .tracks
        .iter()
        .flat_map(|track| &track.effects)
        .chain(project.presets.iter().flat_map(|preset| &preset.effects))
        .chain(project.buses.iter().flat_map(|bus| &bus.effects))
        .chain(project.master.iter().flat_map(|master| &master.effects));
    for effect in effects {
        for warning in resolve_effect(&effect.effect_type, &effect.params).warnings {
            if seen.insert(warning.clone()) {
                warnings.push(warning);
            }
        }
    }
    warnings
}

fn effect_for_device(id: &str) -> Option<EffectType> {
    Some(match id {
        "gain" => EffectType::Gain,
        "eq" => EffectType::Eq,
        "compression" => EffectType::Compression,
        "de-esser" => EffectType::DeEsser,
        "breath-split" => EffectType::BreathSplit,
        "plosive" => EffectType::Plosive,
        "multiband" => EffectType::Multiband,
        "leveler" => EffectType::Leveler,
        "opto" => EffectType::Opto,
        "fet" => EffectType::Fet,
        "iron" => EffectType::Iron,
        "passive-eq" => EffectType::PassiveEq,
        "doubler" => EffectType::Doubler,
        "exciter" => EffectType::Exciter,
        "ducked-send" => EffectType::DuckedSend,
        "resonance" => EffectType::Resonance,
        "tuner" => EffectType::Tuner,
        "dynamic-eq" => EffectType::DynamicEq,
        "reverb" => EffectType::Reverb,
        "reverb-fdn" => EffectType::ReverbFdn,
        "delay" => EffectType::Delay,
        "chorus" => EffectType::Chorus,
        "saturator" => EffectType::Saturator,
        "limiter" => EffectType::Limiter,
        "distortion" => EffectType::Distortion,
        "flanger" => EffectType::Flanger,
        "phaser" => EffectType::Phaser,
        "gate" => EffectType::Gate,
        "stereo-width" => EffectType::StereoWidth,
        _ => return None,
    })
}

fn eq_explicit_target(name: &str) -> Option<&'static str> {
    Some(match name {
        "low_freq" => "band1_freq",
        "low_gain" => "band1_gain",
        "mid_freq" | "freq" => "band2_freq",
        "mid_gain" | "gain" => "band2_gain",
        "q" => "band2_q",
        "high_freq" => "band4_freq",
        "high_gain" => "band4_gain",
        _ => return None,
    })
}

fn band_index(param: &str) -> Option<&str> {
    let rest = param.strip_prefix("band")?;
    let (index, _) = rest.split_once('_')?;
    index.parse::<u8>().ok().map(|_| index)
}

fn band_type(index: &str) -> Option<&'static str> {
    match index {
        "1" => Some("low_shelf"),
        "2" => Some("bell"),
        "4" => Some("high_shelf"),
        _ => None,
    }
}

fn shorthand_matches(rule: &Shorthand, value: &ParamValue) -> bool {
    match rule.value {
        ShorthandKind::GainAtFrequency => gain_frequency(value).is_some(),
        ShorthandKind::Frequency => frequency_value(value).is_some(),
        ShorthandKind::Word(word) => {
            matches!(value, ParamValue::String(text) if text.trim().eq_ignore_ascii_case(word))
        }
    }
}

fn shorthand_inputs(rule: &Shorthand, value: &ParamValue) -> (Option<f64>, Option<f64>) {
    match rule.value {
        ShorthandKind::GainAtFrequency => {
            let (gain, frequency) = gain_frequency(value).unwrap_or((0.0, 0.0));
            (Some(gain), Some(frequency))
        }
        ShorthandKind::Frequency => (None, frequency_value(value)),
        ShorthandKind::Word(_) => (None, None),
    }
}

fn gain_frequency(value: &ParamValue) -> Option<(f64, f64)> {
    let ParamValue::String(raw) = value else {
        return None;
    };
    let (gain, frequency) = raw.split_once('@')?;
    Some((parse_db(gain)?, parse_hz(frequency)?))
}

fn frequency_value(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Frequency(hz) => (*hz > 0.0).then_some(*hz),
        ParamValue::Number(hz) => (*hz > 0.0).then_some(*hz),
        ParamValue::String(raw) => parse_hz(raw),
        _ => None,
    }
}

fn parse_db(text: &str) -> Option<f64> {
    let text = text.trim();
    let number = text.strip_suffix("dB").unwrap_or(text).trim();
    let value = number.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn parse_hz(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = text.strip_suffix("Hz").unwrap_or(text).trim();
    let (number, scale) = match text.strip_suffix('k') {
        Some(thousands) => (thousands, 1_000.0),
        None => (text, 1.0),
    };
    let value = number.trim().parse::<f64>().ok()? * scale;
    (value.is_finite() && value > 0.0).then_some(value)
}

fn target_value(
    device: &DeviceSchema,
    target: &ShorthandTarget,
    gain: Option<f64>,
    frequency: Option<f64>,
) -> Option<ParamValue> {
    let param = device.param(target.param)?;
    match (param.kind, target.source) {
        (ParamKind::Bool, ShorthandSource::Fixed(value)) => Some(ParamValue::Bool(value != 0.0)),
        (ParamKind::Enum, ShorthandSource::Label(label)) => {
            Some(ParamValue::String(label.to_string()))
        }
        (ParamKind::Continuous, ShorthandSource::Gain) => Some(ParamValue::Db(gain?)),
        (ParamKind::Continuous, ShorthandSource::Frequency) => {
            Some(ParamValue::Frequency(frequency?))
        }
        (ParamKind::Continuous, ShorthandSource::Percent(value)) => {
            Some(ParamValue::Percentage(value))
        }
        (ParamKind::Continuous, ShorthandSource::Seconds(value)) => Some(ParamValue::Time(value)),
        (ParamKind::Continuous, ShorthandSource::Fixed(value)) => match param.unit {
            ParamUnit::Hz => Some(ParamValue::Frequency(value)),
            ParamUnit::Db => Some(ParamValue::Db(value)),
            ParamUnit::Ms => Some(ParamValue::Time(value / 1_000.0)),
            ParamUnit::Percent => Some(ParamValue::Percentage(value)),
            ParamUnit::Seconds => Some(ParamValue::Time(value)),
            ParamUnit::Cents => Some(ParamValue::Cents(value)),
            ParamUnit::Fraction
            | ParamUnit::Ratio
            | ParamUnit::None
            | ParamUnit::Semitones
            | ParamUnit::Octaves
            | ParamUnit::Degrees => Some(ParamValue::Number(value)),
        },
        _ => None,
    }
}

fn display_value(value: &ParamValue) -> String {
    match value {
        ParamValue::String(text) => text.clone(),
        other => format!("{other:?}"),
    }
}

fn bool_value(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Bool(enabled) => Some(if *enabled { 1.0 } else { 0.0 }),
        ParamValue::Number(number) if *number == 0.0 || *number == 1.0 => Some(*number),
        _ => None,
    }
}

fn enum_value(param: &ParamSchema, value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Number(number) if number.is_finite() && number.fract() == 0.0 => Some(*number),
        ParamValue::String(text) => {
            let wanted = normalize_choice(text);
            param
                .options
                .iter()
                .position(|option| normalize_choice(option) == wanted)
                .map(|index| index as f64)
        }
        _ => None,
    }
}

fn normalize_choice(text: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_digit = false;
    for character in text.trim().to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if previous_was_digit && character.is_ascii_alphabetic() {
                normalized.push('_');
            }
            normalized.push(character);
            previous_was_digit = character.is_ascii_digit();
        } else {
            normalized.push('_');
            previous_was_digit = false;
        }
    }
    normalized
}

fn param_value_in_unit(param: &ParamSchema, value: &ParamValue) -> Option<f64> {
    match (param.unit, value) {
        (_, ParamValue::Number(number)) => Some(*number),
        (ParamUnit::Hz, ParamValue::Frequency(hz)) => Some(*hz),
        (ParamUnit::Db, ParamValue::Db(db)) => Some(*db),
        (ParamUnit::Ms, ParamValue::Time(seconds)) => Some(*seconds * 1000.0),
        (ParamUnit::Percent, ParamValue::Percentage(percent)) => Some(*percent),
        // Effect automation historically uses physical percentage coordinates.
        // Static inserts use validate_static_param and the factory conversion.
        (ParamUnit::Fraction, ParamValue::Percentage(physical)) => Some(*physical),
        (ParamUnit::Seconds, ParamValue::Time(seconds)) => Some(*seconds),
        (ParamUnit::Cents, ParamValue::Cents(cents)) => Some(*cents),
        (ParamUnit::Ratio, ParamValue::String(text)) => parse_ratio(text),
        _ => None,
    }
}

fn parse_ratio(text: &str) -> Option<f64> {
    let (numerator, denominator) = text.split_once(':')?;
    let numerator = numerator.trim().parse::<f64>().ok()?;
    let denominator = denominator.trim().parse::<f64>().ok()?;
    (numerator.is_finite() && denominator.is_finite() && numerator > 0.0 && denominator > 0.0)
        .then_some(numerator / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsp::schema::find_device;

    #[test]
    fn every_dsp_effect_has_a_schema_with_parameters() {
        for effect in [
            EffectType::Gain,
            EffectType::Eq,
            EffectType::Compression,
            EffectType::DeEsser,
            EffectType::BreathSplit,
            EffectType::Plosive,
            EffectType::Multiband,
            EffectType::Leveler,
            EffectType::Opto,
            EffectType::Fet,
            EffectType::Iron,
            EffectType::PassiveEq,
            EffectType::Doubler,
            EffectType::Exciter,
            EffectType::DuckedSend,
            EffectType::Resonance,
            EffectType::Tuner,
            EffectType::Limiter,
            EffectType::Reverb,
            EffectType::ReverbFdn,
            EffectType::Delay,
            EffectType::Chorus,
            EffectType::Saturator,
            EffectType::DynamicEq,
            EffectType::Distortion,
            EffectType::Flanger,
            EffectType::Phaser,
            EffectType::Gate,
            EffectType::StereoWidth,
        ] {
            let device = device_for_effect(&effect)
                .unwrap_or_else(|| panic!("{effect:?} has no device schema"));
            assert!(!device.params.is_empty(), "{effect:?} has no parameters");
        }
        assert!(device_for_effect(&EffectType::Plugin("x".into())).is_none());
    }

    #[test]
    fn every_device_id_round_trips_through_its_effect_type() {
        for device in oc_dsp::schema::DEVICES {
            let effect = effect_for_device(device.id)
                .unwrap_or_else(|| panic!("{} has no effect type", device.id));
            assert_eq!(
                device_for_effect(&effect).map(|schema| schema.id),
                Some(device.id)
            );
        }
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        let compression = find_device("compression").unwrap();
        assert!(validate_param(compression, "threshold", &ParamValue::Db(-6.0)).is_ok());
        assert!(validate_param(compression, "threshold", &ParamValue::Db(-100.0)).is_err());
        assert!(validate_param(compression, "attack", &ParamValue::Percentage(20.0)).is_err());
        let reverb = find_device("reverb").unwrap();
        assert!(validate_param(reverb, "mix", &ParamValue::Percentage(150.0)).is_err());
        assert!(validate_param(reverb, "mix", &ParamValue::Percentage(50.0)).is_ok());
        let ratio =
            validate_param(compression, "ratio", &ParamValue::String("2:1".into())).unwrap();
        assert!((ratio - 2.0).abs() < 1e-9);
        assert!(validate_param(compression, "ratio", &ParamValue::String("2:0".into())).is_err());
    }

    #[test]
    fn compression_controls_validate() {
        let compression = find_device("compression").unwrap();
        assert!(validate_param(compression, "range", &ParamValue::Db(6.0)).is_ok());
        assert!(validate_param(compression, "range", &ParamValue::Db(61.0)).is_err());
        assert!(validate_param(compression, "stereo_link", &ParamValue::Percentage(50.0)).is_ok());
        assert!(
            validate_param(compression, "stereo_link", &ParamValue::Percentage(150.0)).is_err()
        );
        assert!(validate_param(compression, "hold", &ParamValue::Time(0.5)).is_ok());
        assert!(validate_param(compression, "hold", &ParamValue::Time(1.0)).is_err());
        assert!(validate_param(compression, "ratio_plain", &ParamValue::Number(0.6)).is_ok());
        assert!(validate_param(compression, "ratio_plain", &ParamValue::Number(1.5)).is_err());
        assert!(validate_param(compression, "attack_plain", &ParamValue::Number(0.3)).is_ok());
        assert!(validate_param(compression, "release_plain", &ParamValue::Number(0.4)).is_ok());
        assert!(validate_param(compression, "knee", &ParamValue::Db(72.0)).is_ok());
        assert!(validate_param(compression, "knee", &ParamValue::Db(80.0)).is_err());
        assert_eq!(compression.param("knee").unwrap().max, 72.0);
        assert_eq!(compression.param("ratio").unwrap().max, 100.0);
    }

    #[test]
    fn unknown_parameters_are_rejected() {
        let compression = find_device("compression").unwrap();
        assert!(validate_param(compression, "typo", &ParamValue::Number(1.0)).is_err());
        let eq = find_device("eq").unwrap();
        assert!(eq.param("freq").is_some());
        assert!(validate_param(eq, "freq", &ParamValue::Frequency(1_000.0)).is_ok());
    }

    #[test]
    fn deesser_and_multiband_enum_parameters_validate() {
        let deesser = find_device("de-esser").unwrap();
        assert_eq!(
            validate_param(
                deesser,
                "band_processing",
                &ParamValue::String("Split Band".into())
            )
            .unwrap(),
            1.0
        );
        assert_eq!(
            validate_param(
                deesser,
                "band_processing",
                &ParamValue::String("wide_band".into())
            )
            .unwrap(),
            0.0
        );
        assert!(validate_param(deesser, "stereo_link", &ParamValue::Percentage(50.0)).is_ok());
        assert_eq!(
            validate_param(
                deesser,
                "threshold_mode",
                &ParamValue::String("Relative".into())
            )
            .unwrap(),
            1.0
        );
        assert_eq!(
            validate_param(deesser, "detector", &ParamValue::String("In-band".into())).unwrap(),
            1.0
        );
        assert_eq!(
            validate_param(
                deesser,
                "sibilance_band",
                &ParamValue::String("7 kHz".into())
            )
            .unwrap(),
            3.0
        );
        assert!(
            validate_param(
                deesser,
                "sibilance_band",
                &ParamValue::String("4 kHz".into())
            )
            .is_err()
        );
        assert!(validate_param(deesser, "relative_threshold", &ParamValue::Db(-60.0)).is_err());
        assert!(validate_param(deesser, "stereo_link", &ParamValue::Percentage(150.0)).is_err());

        let multiband = find_device("multiband").unwrap();
        assert_eq!(
            validate_param(
                multiband,
                "band_count",
                &ParamValue::String("4 bands".into())
            )
            .unwrap(),
            2.0
        );
        assert_eq!(
            validate_param(multiband, "band1_ratio", &ParamValue::String("4:1".into())).unwrap(),
            4.0
        );
        assert!(
            validate_param(multiband, "band1_ratio", &ParamValue::String("4:0".into())).is_err()
        );
        assert!(validate_param(multiband, "band1_threshold", &ParamValue::Db(-24.0)).is_ok());
        assert!(validate_param(multiband, "band1_threshold", &ParamValue::Db(6.0)).is_err());
        assert_eq!(
            validate_param(multiband, "slope", &ParamValue::String("48 dB/oct".into())).unwrap(),
            4.0
        );
        assert_eq!(
            validate_param(multiband, "slope", &ParamValue::String("48dB/oct".into())).unwrap(),
            4.0
        );
        assert_eq!(
            validate_param(
                multiband,
                "phase_mode",
                &ParamValue::String("Linear".into())
            )
            .unwrap(),
            2.0
        );
        assert_eq!(
            validate_param(multiband, "band2_range", &ParamValue::Db(-6.0)).unwrap(),
            -6.0
        );
        assert!(validate_param(multiband, "band2_range", &ParamValue::Db(-61.0)).is_err());
        assert_eq!(
            validate_param(multiband, "band2_enabled", &ParamValue::Bool(false)).unwrap(),
            0.0
        );
        assert_eq!(
            validate_param(multiband, "detector", &ParamValue::String("peak".into())).unwrap(),
            0.0
        );
        assert_eq!(
            validate_param(
                multiband,
                "detector",
                &ParamValue::String("Programme".into())
            )
            .unwrap(),
            1.0
        );
        assert!(
            validate_param(multiband, "detector", &ParamValue::String("rms".into())).is_err(),
            "an option outside the schema must be refused"
        );
    }

    #[test]
    fn boolean_and_enum_parameters_validate() {
        let device = find_device("dynamic-eq").unwrap();
        assert_eq!(
            validate_param(device, "band2_enabled", &ParamValue::Bool(true)).unwrap(),
            1.0
        );
        assert_eq!(
            validate_param(device, "band2_enabled", &ParamValue::Number(0.0)).unwrap(),
            0.0
        );
        assert!(validate_param(device, "band2_enabled", &ParamValue::Number(2.0)).is_err());

        assert_eq!(
            validate_param(device, "band2_type", &ParamValue::String("Low Cut".into())).unwrap(),
            3.0
        );
        assert_eq!(
            validate_param(
                device,
                "band2_type",
                &ParamValue::String("low_shelf".into())
            )
            .unwrap(),
            1.0
        );
        assert_eq!(
            validate_param(device, "band2_type", &ParamValue::Number(5.0)).unwrap(),
            5.0
        );
        assert!(
            validate_param(device, "band2_type", &ParamValue::String("wobble".into())).is_err()
        );
        assert!(validate_param(device, "band2_type", &ParamValue::Number(7.0)).is_err());

        // Every band shares the same surface, so ids cannot silently drift.
        for band in 1..=6 {
            for field in ["enabled", "type", "freq", "threshold", "release"] {
                let id = format!("band{band}_{field}");
                assert!(device.param(&id).is_some(), "missing schema id {id}");
            }
        }
    }
}
