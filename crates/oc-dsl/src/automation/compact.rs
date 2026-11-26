//! Compact analytic segments and recipient-domain value transfers.
use crate::ast::{AutomationValueMapping, Curve, ValueMappingCurve};
use oc_device_schema::automation_curves::{MAX_EXPONENT_ARGUMENT, valid_curvature};

/// Normalized exponential with the exact linear limit at zero curvature.
/// Unlike a power curve this permits unequal, finite endpoint slopes.
pub fn exponential_shape(curvature: f64, position: f64) -> f64 {
    if curvature == 0.0 || position == 0.0 || position == 1.0 {
        return position;
    }
    if curvature.abs() < f64::MIN_POSITIVE {
        let argument = curvature * position;
        let relative = if argument == 0.0 {
            1.0
        } else {
            argument.exp_m1() / argument
        };
        return position * relative * (curvature / curvature.exp_m1());
    }
    (curvature * position).exp_m1() / curvature.exp_m1()
}

/// The existing smooth tension and the signed exponential coefficient are independent.
pub fn segment_shape(curve: Curve, tension: f64, curvature: f64, position: f64) -> f64 {
    if curve == Curve::Exponential {
        exponential_shape(curvature, position)
    } else {
        super::shape(curve, tension, position)
    }
}

/// A bounded automation value transfer ready for sample-by-sample evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreparedMapping {
    quantization_steps: Option<f64>,
    curvature: f64,
    inverse_denominator: f64,
    scale: f64,
    offset: f64,
}
impl PreparedMapping {
    /// Prepare a transfer for the finite interval from `low` to `high`.
    pub fn new(mapping: AutomationValueMapping, low: f64, high: f64) -> Result<Self, String> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err("automation mapping requires ordered finite input bounds".into());
        }
        if !mapping.scale.is_finite() || !mapping.offset.is_finite() {
            return Err("automation mapping scale and offset must be finite".into());
        }
        let quantization_steps = mapping.input_quantization_steps.map(f64::from);
        if mapping.input_quantization_steps.is_some_and(|steps| {
            !(oc_device_schema::automation_curves::MIN_INPUT_QUANTIZATION_STEPS
                ..=oc_device_schema::automation_curves::MAX_INPUT_QUANTIZATION_STEPS)
                .contains(&steps)
        }) {
            return Err(
                "automation input quantization steps exceed the supported positive range".into(),
            );
        }
        let quantize = |value: f64| -> Result<f64, String> {
            match quantization_steps {
                Some(steps) if !(value * steps).is_finite() => {
                    Err("automation quantization input must remain finite".into())
                }
                Some(steps) => Ok((value * steps).floor() / steps),
                None => Ok(value),
            }
        };
        let (low, high) = (quantize(low)?, quantize(high)?);
        let curvature = mapping.curvature.unwrap_or(0.0);
        if !valid_curvature(curvature)
            || (mapping.curve == ValueMappingCurve::Linear && mapping.curvature.is_some())
        {
            return Err(
                "automation mapping curvature requires exponential and the supported finite range"
                    .into(),
            );
        }
        if mapping.curve == ValueMappingCurve::Exponential
            && [low, high]
                .into_iter()
                .any(|v| !v.is_finite() || (v * curvature).abs() > MAX_EXPONENT_ARGUMENT)
        {
            return Err("automation mapping exponent exceeds the prepared finite range".into());
        }
        let prepared = Self {
            quantization_steps,
            curvature: if mapping.curve == ValueMappingCurve::Exponential {
                curvature
            } else {
                0.0
            },
            inverse_denominator: if curvature == 0.0 {
                1.0
            } else {
                1.0 / curvature.exp_m1()
            },
            scale: mapping.scale,
            offset: mapping.offset,
        };
        if ![
            prepared.apply_shaped_input(low),
            prepared.apply_shaped_input(high),
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err("automation mapping output must be finite".into());
        }
        Ok(prepared)
    }
    /// Transfer one value from the prepared input interval.
    pub fn apply(self, value: f64) -> f64 {
        let value = self
            .quantization_steps
            .map_or(value, |steps| (value * steps).floor() / steps);
        self.apply_shaped_input(value)
    }
    fn apply_shaped_input(self, value: f64) -> f64 {
        let shaped = if self.curvature == 0.0 || value == 0.0 || value == 1.0 {
            value
        } else {
            if self.inverse_denominator.is_finite() {
                (self.curvature * value).exp_m1() * self.inverse_denominator
            } else {
                exponential_shape(self.curvature, value)
            }
        };
        self.offset + self.scale * shaped
    }
}

/// Apply a validated generic transfer to one recipient-domain value.
pub fn map_value(mapping: AutomationValueMapping, value: f64) -> Result<f64, String> {
    Ok(PreparedMapping::new(mapping, value, value)?.apply(value))
}

/// Conservative mapped bounds from every authored numeric anchor, before clock
/// rounding or coincident-point replacement. Admission must hold at every rate.
pub fn authored_mapped_bounds(lane: &crate::ast::AutomationLane) -> Result<(f64, f64), String> {
    let mapping = lane
        .value_mapping
        .ok_or("automation lane has no value mapping")?;
    let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in &lane.points {
        let crate::ast::ParamValue::Number(value) = point.value else {
            return Err("mapped automation currently requires numeric anchors".into());
        };
        if !value.is_finite() {
            return Err("mapped automation requires finite anchors".into());
        }
        low = low.min(value);
        high = high.max(value);
    }
    if low > high {
        return Err("mapped automation requires finite anchors".into());
    }
    if lane.is_lfo() {
        super::validate_lane_lfo(&lane.lfo)?;
        // A reduced-depth waveform is inside its authored range. Preserve that
        // range too: duplicate-knot replacement can change the actual midpoint.
        let amount = lane.lfo.amount.max(1.0);
        let center = 0.5 * low + 0.5 * high;
        low = center + (low - center) * amount;
        high = center + (high - center) * amount;
    }
    let prepared = PreparedMapping::new(mapping, low, high)?;
    let a = prepared.apply(low);
    let b = prepared.apply(high);
    Ok((a.min(b), a.max(b)))
}

/// A built-in discrete or unknown parameter cannot silently ignore an authored
/// interpolation shape. Hosted plugins retain their normalized continuous path.
pub fn validate_encoded_parameter_curve(
    lane: &crate::ast::AutomationLane,
    builtin_synth: bool,
) -> Result<(), String> {
    if !builtin_synth
        || !lane
            .points
            .iter()
            .any(|p| !matches!(p.curve, Curve::Linear | Curve::Stairs) || p.curvature.is_some())
    {
        return Ok(());
    }
    let Some(name) = super::decode_plugin_automation_param(&lane.param) else {
        return Ok(());
    };
    use oc_device_schema::synth::{SynthValueKind, find_family_field, find_param};
    let kind = find_param(&name)
        .map(|p| p.kind)
        .or_else(|| find_family_field(&name).map(|(_, p)| p.kind));
    if kind != Some(SynthValueKind::Continuous) {
        return Err(format!(
            "built-in parameter '{name}' does not support continuous automation curves"
        ));
    }
    Ok(())
}

/// Resolve only source identity for compact-curve validation. This does not
/// change instrument preparation or the historical normalization contract.
pub fn track_uses_builtin_synth(
    project: &crate::ast::Project,
    track: &crate::ast::Track,
) -> Result<bool, String> {
    let plugin = match track.preset.as_deref() {
        Some(name) => crate::preset::resolve_preset(&project.presets, name)?.plugin,
        None => crate::instruments::track_instrument(project, track)
            .and_then(crate::instruments::default_patch_plugin)
            .map(str::to_string),
    };
    Ok(plugin.as_deref() == Some(oc_device_schema::synth::SYNTH_PLUGIN_ID))
}

/// Check a mapped numeric range against its authoritative recipient owner.
/// `None` identifies a hosted effect's normalized domain, never an unknown builtin.
/// This preparation-time rule is shared by project admission and the audio graph.
pub fn validate_mapped_bounds(
    parameter: &str,
    device_id: Option<&str>,
    bounds: Option<(f64, f64)>,
) -> Result<(), String> {
    use oc_device_schema::effect_slot::SlotControl;
    let (minimum, maximum) = match device_id {
        _ if parameter == SlotControl::WET_ID => (SlotControl::WET_MIN, SlotControl::WET_MAX),
        Some(id) => {
            let device = oc_device_schema::find_device(id)
                .ok_or_else(|| format!("unknown mapped automation device '{id}'"))?;
            let spec = device
                .param(parameter)
                .ok_or_else(|| format!("effect '{id}' has no parameter '{parameter}'"))?;
            (spec.min, spec.max)
        }
        None => (0.0, 1.0),
    };
    let Some((low, high)) = bounds else {
        return Ok(());
    };
    if !low.is_finite() || !high.is_finite() || low > high || low < minimum || high > maximum {
        return Err(format!(
            "mapped automation '{parameter}' output {low}..{high} exceeds recipient range {minimum}..{maximum}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast::*, automation::LaneCurve, timing::ProjectClock};
    fn lane() -> AutomationLane {
        AutomationLane {
            param: "gain.factor".into(),
            points: vec![
                AutomationPoint {
                    position: BarBeat::new(1, 1, 0),
                    value: ParamValue::Number(0.2),
                    curve: Curve::Exponential,
                    curvature: Some(-4.0),
                    tension: None,
                },
                AutomationPoint {
                    position: BarBeat::new(2, 1, 0),
                    value: ParamValue::Number(0.8),
                    curve: Curve::Linear,
                    curvature: None,
                    tension: None,
                },
            ],
            ..Default::default()
        }
    }
    fn clock() -> ProjectClock {
        ProjectClock::new(&crate::parse("@project\n  tempo: 120\n").unwrap().properties).unwrap()
    }
    #[test]
    fn input_quantization_precedes_the_value_transfer_and_is_bounded() {
        let mapping = AutomationValueMapping {
            input_quantization_steps: Some(4),
            curve: ValueMappingCurve::Exponential,
            curvature: Some(2.0),
            scale: 2.0,
            offset: 1.0,
        };
        for value in [-0.26, -0.01, 0.0, 0.249999, 0.25, 0.99, 1.0] {
            let quantized = (value * 4.0_f64).floor() / 4.0;
            assert_eq!(
                map_value(mapping, value).unwrap(),
                1.0 + 2.0 * exponential_shape(2.0, quantized)
            );
        }
        let source = "@project\n@bus b\n  effects:\n    - gain: factor=1\n  automation: gain.factor mapping(exponential, curvature=2, input_quantization_steps=128) {1:1=0, 2:1=1}\n";
        let project = crate::parse(source).unwrap();
        crate::validate(&project).unwrap();
        assert_eq!(crate::parse(&crate::serialize(&project)).unwrap(), project);
        let mut invalid = mapping;
        invalid.input_quantization_steps = Some(0);
        assert!(map_value(invalid, 0.5).is_err());
        invalid.input_quantization_steps = Some(1_000_001);
        assert!(map_value(invalid, 0.5).is_err());
        let overflow = AutomationValueMapping {
            input_quantization_steps: Some(4),
            ..Default::default()
        };
        assert!(map_value(overflow, f64::MAX).is_err());
    }

    #[test]
    fn compact_exponential_crop_and_mirrored_halves_match_closed_forms() {
        for k in [-64.0, -4.0, -1e-9, 0.0, 1e-9, 4.0, 64.0] {
            assert_eq!(exponential_shape(k, 0.0), 0.0);
            assert_eq!(exponential_shape(k, 1.0), 1.0);
            for i in 0..=100 {
                let u = i as f64 / 100.0;
                let (a, b) = (0.17, 0.83);
                let low = exponential_shape(k, a);
                let high = exponential_shape(k, b);
                let projected = low + (high - low) * exponential_shape(k * (b - a), u);
                assert!((projected - exponential_shape(k, a + (b - a) * u)).abs() < 3e-14);
                let mirrored = if u < 0.5 {
                    0.5 * exponential_shape(k, 2.0 * u)
                } else {
                    1.0 - 0.5 * exponential_shape(k, 2.0 - 2.0 * u)
                };
                let halves = if u < 0.5 {
                    0.5 * exponential_shape(k, 2.0 * u)
                } else {
                    0.5 + 0.5 * exponential_shape(-k, 2.0 * u - 1.0)
                };
                assert!((mirrored - halves).abs() < 3e-14);
            }
        }
    }
    #[test]
    fn mapping_follows_recipient_conversion_and_interpolation_not_anchor_mapping() {
        let mut l = lane();
        l.value_mapping = Some(AutomationValueMapping {
            curve: ValueMappingCurve::Exponential,
            curvature: Some(2.0),
            scale: -2.0,
            offset: 3.0,
            input_quantization_steps: None,
        });
        let c = LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap() * 2.0)).unwrap();
        for i in 0..=200 {
            let raw = 0.4 + 1.2 * exponential_shape(-4.0, i as f64 / 200.0);
            let expected = 3.0 - 2.0 * exponential_shape(2.0, raw);
            assert!((c.envelope_value(i as f64).unwrap() - expected).abs() < 2e-14);
        }
        let expected = [
            map_value(l.value_mapping.unwrap(), 0.4).unwrap(),
            map_value(l.value_mapping.unwrap(), 1.6).unwrap(),
        ];
        assert_eq!(
            c.value_bounds(),
            Some((expected[0].min(expected[1]), expected[0].max(expected[1])))
        );
    }
    #[test]
    fn mapped_lfo_depth_extrema_and_preparation_refusals_are_explicit() {
        let mut l = lane();
        l.mode = LaneMode::Lfo;
        l.lfo.amount = 4.0;
        l.value_mapping = Some(AutomationValueMapping {
            scale: -1.0,
            offset: 1.0,
            ..Default::default()
        });
        let c = LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap())).unwrap();
        let (low, high) = c.value_bounds().unwrap();
        assert!((low + 0.7).abs() < 1e-12);
        assert!((high - 1.7).abs() < 1e-12);
        for n in 0..100 {
            let v = c.lfo_value(n as f64 / 100.0).unwrap();
            assert!(v >= low - 1e-12 && v <= high + 1e-12);
        }
        l.value_mapping = Some(AutomationValueMapping {
            curve: ValueMappingCurve::Exponential,
            curvature: Some(64.0),
            ..Default::default()
        });
        assert!(LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap())).is_err());
        l = lane();
        l.points[0].curvature = Some(f64::NAN);
        assert!(LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap())).is_err());
        l = lane();
        l.value_mapping = Some(AutomationValueMapping::default());
        l.points[0].value = ParamValue::Percentage(0.2);
        assert!(LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap())).is_err());
    }
    #[test]
    fn a_mapped_wet_lane_checks_the_resulting_fraction() {
        let source = "@project\n@bus out\n  effects:\n    - gain: factor=1\n  automation: fx1.slot/wet mapping(linear, scale=0.01) {1:1=0, 2:1=100}\n";
        let project = crate::parse(source).unwrap();
        assert!(crate::validate(&project).is_ok());
        let invalid = crate::parse(&source.replace("scale=0.01", "scale=0.02")).unwrap();
        let errors = crate::validate(&invalid).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("exceeds recipient range"))
        );
    }

    #[test]
    fn discontinuities_opt_in_preserves_left_limit_and_last_write_without_new_lanes() {
        let source = "@project\n  tempo: 120\n@bus jump\n  effects:\n    - gain: factor=1\n  automation: gain.factor discontinuities [ppq=96000] {1:1=0, 1:3=1, 1:3:1=0, 2:1=0}\n";
        let project = crate::parse(source).unwrap();
        crate::validate(&project).unwrap();
        let lane = &project.buses[0].automation[0];
        assert!(lane.preserve_discontinuities);
        let c = LaneCurve::new(lane, &clock(), 100, |p| Ok(p.as_f64().unwrap())).unwrap();
        assert_eq!(c.points().len(), 4);
        assert!((c.envelope_value(50.0).unwrap() - 0.5).abs() < 1e-14);
        assert!((c.envelope_value(99.0).unwrap() - 0.99).abs() < 1e-14);
        assert_eq!(c.envelope_value(100.0), Some(0.0));
        assert_eq!(c.envelope_value(1000.0), Some(0.0));
        let mut old = lane.clone();
        old.preserve_discontinuities = false;
        let old = LaneCurve::new(&old, &clock(), 100, |p| Ok(p.as_f64().unwrap())).unwrap();
        assert_eq!(old.points().len(), 3);
        assert_eq!(old.envelope_value(50.0), Some(0.0));
        let text = crate::serialize(&project);
        assert_eq!(crate::parse(&text).unwrap(), project);
        let mut repeating = lane.clone();
        repeating.mode = LaneMode::Lfo;
        repeating.points[2].position = repeating.points[1].position;
        let repeating =
            LaneCurve::new(&repeating, &clock(), 100, |p| Ok(p.as_f64().unwrap())).unwrap();
        assert!((repeating.lfo_value(0.25).unwrap() - 0.5).abs() < 1e-14);
        assert_eq!(repeating.lfo_value(0.5), Some(0.0));
    }
    #[test]
    fn subnormal_curvature_has_finite_normalized_and_prepared_limits() {
        for k in [f64::from_bits(1), -f64::from_bits(1), 1e-310, -1e-310] {
            assert_eq!(exponential_shape(k, 0.5), 0.5);
            let mut l = lane();
            l.points[0].curvature = Some(k);
            l.value_mapping = Some(AutomationValueMapping {
                curve: ValueMappingCurve::Exponential,
                curvature: Some(k),
                ..Default::default()
            });
            let curve = LaneCurve::new(&l, &clock(), 100, |p| Ok(p.as_f64().unwrap())).unwrap();
            for sample in [0.0, 50.0, 100.0, 199.0] {
                assert!(curve.envelope_value(sample).unwrap().is_finite());
            }
        }
    }
    #[test]
    fn mapped_admission_cannot_hide_invalid_anchors_in_a_sample_clock_collision() {
        let maximum = oc_device_schema::find_device("gain")
            .unwrap()
            .param("factor")
            .unwrap()
            .max;
        let source = format!(
            "@project\n  tempo: 156\n@bus b\n  effects:\n    - gain: factor=1\n  automation: gain.factor [ppq=96000] mapping(linear, scale=2) {{1:1:0={maximum}, 1:1:2=0}}\n"
        );
        let project = crate::parse(&source).unwrap();
        let lane = &project.buses[0].automation[0];
        let clock = ProjectClock::new(&project.properties).unwrap();
        let bounds = |rate| {
            LaneCurve::new(lane, &clock, rate, |v| Ok(v.as_f64().unwrap()))
                .unwrap()
                .value_bounds()
        };
        assert_eq!(bounds(48_000), Some((0.0, 0.0)));
        assert_eq!(bounds(192_000), Some((0.0, maximum * 2.0)));
        assert_eq!(authored_mapped_bounds(lane).unwrap(), (0.0, maximum * 2.0));
        assert!(
            crate::validate(&project)
                .unwrap_err()
                .iter()
                .any(|e| e.to_string().contains("recipient range"))
        );
    }

    #[test]
    fn builtin_discrete_curves_fail_closed_using_parameter_kind_metadata() {
        let source = "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track t | preset: p, instrument: synth/lead_saw\n  automation: PARAM {1:1=0.2 curve=exponential curvature=-4, 2:1=0.8}\n";
        let discrete = crate::parse(&source.replace(
            "PARAM",
            &super::super::encode_plugin_automation_param("filter.model"),
        ))
        .unwrap();
        assert!(
            crate::validate(&discrete)
                .unwrap_err()
                .iter()
                .any(|e| e.to_string().contains("continuous automation curves"))
        );
        let continuous = crate::parse(&source.replace(
            "PARAM",
            &super::super::encode_plugin_automation_param("filter.cutoff"),
        ))
        .unwrap();
        crate::validate(&continuous).unwrap();
        let external = source
            .replace("source: builtin", "source: vst3")
            .replace("builtin.open_synth", "test.plugin");
        crate::validate(
            &crate::parse(&external.replace(
                "PARAM",
                &super::super::encode_plugin_automation_param("filter.model"),
            ))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn project_admission_checks_mapped_recipient_bounds_and_lfo_depth() {
        let source = "@project\n@bus b\n  effects:\n    - gain: factor=1\n  automation: gain.factor mapping(linear, scale=1) {1:1=0, 2:1=1}\n";
        let max = oc_device_schema::find_device("gain")
            .unwrap()
            .param("factor")
            .unwrap()
            .max;
        let bad =
            crate::parse(&source.replace("scale=1", &format!("scale={}", max + 1.0))).unwrap();
        assert!(
            crate::validate(&bad)
                .unwrap_err()
                .iter()
                .any(|e| e.to_string().contains("recipient range"))
        );
        let good = crate::parse(&source.replace("scale=1", "scale=-1, offset=1")).unwrap();
        crate::validate(&good).unwrap();
        let mut depth = crate::parse(source).unwrap();
        depth.buses[0].automation[0].mode = LaneMode::Lfo;
        depth.buses[0].automation[0].lfo.amount = 4.0;
        assert!(
            crate::validate(&depth)
                .unwrap_err()
                .iter()
                .any(|e| e.to_string().contains("recipient range"))
        );
        let hosted = source
            .replace("gain: factor=1", "plugin:\"vst3.test.fake\"")
            .replace("gain.factor", "fx1.Cutoff")
            .replace("scale=1", "scale=2");
        assert!(
            crate::validate(&crate::parse(&hosted).unwrap())
                .unwrap_err()
                .iter()
                .any(|e| e.to_string().contains("recipient range"))
        );
        assert!(validate_mapped_bounds("factor", Some("unknown-owner"), Some((0.0, 1.0))).is_err());
    }

    #[test]
    fn project_admission_accepts_encoded_exponential_curves_but_not_control_curves() {
        let source = "@project\n@track lead | instrument: synth/lead_square\n  automation: pluginparam_66696c7465722e6375746f6666 discontinuities {1:1=0.2 curve=exponential curvature=-4, 1:3=0.8, 1:3=0.1, 2:1=0.5}\n";
        let project = crate::parse(source).unwrap();
        crate::validate(&project).unwrap();
        crate::validate(&crate::parse(&crate::serialize(&project)).unwrap()).unwrap();
        assert!(
            crate::validate(
                &crate::parse(&source.replace("pluginparam_66696c7465722e6375746f6666", "volume"))
                    .unwrap()
            )
            .is_err()
        );
        let mut smoothed = project;
        smoothed.tracks[0]
            .automation_mut()
            .next()
            .unwrap()
            .smoothing_ms = 1.0;
        assert!(crate::validate(&smoothed).is_err());
    }

    #[test]
    fn compact_wire_roundtrips_and_legacy_output_is_unchanged() {
        let source = "@project\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor mapping(exponential, curvature=2, scale=-1, offset=1) {1:1=0 curve=exponential curvature=-4, 2:1=1}\n";
        let p = crate::parse(source).unwrap();
        crate::validate(&p).unwrap();
        let text = crate::serialize(&p);
        assert_eq!(crate::parse(&text).unwrap(), p);
        assert_eq!(
            serde_json::from_str::<Project>(&serde_json::to_string(&p).unwrap()).unwrap(),
            p
        );
        let old = source
            .replace(" mapping(exponential, curvature=2, scale=-1, offset=1)", "")
            .replace(" curve=exponential curvature=-4", "");
        let p = crate::parse(&old).unwrap();
        let text = crate::serialize(&p);
        assert!(!text.contains("mapping(") && !text.contains("curvature="));
        assert!(crate::parse(&source.replace("curvature=-4", "curvature=-65")).is_ok());
        assert!(
            crate::validate(
                &crate::parse(&source.replace("curvature=-4", "curvature=-65")).unwrap()
            )
            .is_err()
        );
    }
}
