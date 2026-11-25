//! Explicit analytic gain clips; legacy interpolation remains a separate ABI.
use crate::ast::{
    AutomationLane, GainAutomation, GainInterpolation, LaneLfo, LaneMode, ParamValue,
};
use crate::automation::{LaneCurve, authored_mapped_bounds, validate_mapped_bounds};
use crate::timing::ProjectClock;

/// Prepare the opt-in segment representation without changing its gain owner.
/// Numeric anchors are receiver inputs; an optional transfer produces gain.
pub fn compile_segments(
    clip: &GainAutomation,
    clock: &ProjectClock,
    sample_rate: u32,
) -> Result<LaneCurve, String> {
    if clip.interpolation != GainInterpolation::Segments {
        return Err("analytic gain preparation requires interpolation=Segments".into());
    }
    if clip.ppq == 0 || clip.ppq > 96_000 || clip.points.is_empty() {
        return Err(
            "analytic gain clips require a bounded positive PPQ and numeric anchors".into(),
        );
    }
    for point in &clip.points {
        let ParamValue::Number(value) = point.value else {
            return Err("gain anchors must be numeric".into());
        };
        if !value.is_finite() {
            return Err("gain anchors must be finite".into());
        }
        if let Some(tension) = point.tension {
            if point.curve != crate::ast::Curve::Smooth
                || !tension.is_finite()
                || !(0.0..=1.0).contains(&tension)
            {
                return Err("gain tension requires a smooth curve and a value in 0..1".into());
            }
        }
        if clip.value_mapping.is_none() {
            validate_mapped_bounds("factor", Some("gain"), Some((value, value)))?;
        }
    }
    let lane = AutomationLane {
        playlist_lane: None,
        param: "gain".into(),
        time_range: Some(clip.time_range),
        ppq: clip.ppq,
        muted: false,
        smoothing_ms: 0.0,
        mode: LaneMode::Envelope,
        lfo: LaneLfo::default(),
        points: clip.points.clone(),
        value_mapping: clip.value_mapping,
        preserve_discontinuities: clip.preserve_discontinuities,
    };
    if lane.value_mapping.is_some() {
        validate_mapped_bounds("factor", Some("gain"), Some(authored_mapped_bounds(&lane)?))?;
    }
    LaneCurve::new(&lane, clock, sample_rate, |value| {
        value.as_f64().ok_or("gain anchors must be numeric".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project(mode: &str, extra: &str) -> crate::ast::Project {
        crate::parse(&format!("@project\n  tempo: 120\n@bus b\n  gain_automation: {mode} ppq=48 [1:1 - 1:3] {extra} {{1:1=0 curve=exponential curvature=-2, 1:2=1, 1:2=0.25, 2:1=0.5}}\n")).unwrap()
    }
    #[test]
    fn gain_segments_roundtrip_map_coincident_knots_and_allow_crop_brackets() {
        let p = project(
            "segments",
            "discontinuities mapping(exponential, curvature=2, scale=2, input_quantization_steps=128)",
        );
        crate::validate(&p).unwrap();
        let text = crate::serialize(&p);
        assert_eq!(crate::parse(&text).unwrap(), p);
        assert_eq!(
            serde_json::from_str::<crate::ast::Project>(&serde_json::to_string(&p).unwrap())
                .unwrap(),
            p
        );
        let clock = ProjectClock::new(&p.properties).unwrap();
        let curve = compile_segments(&p.buses[0].gain_automation[0], &clock, 100).unwrap();
        assert!(curve.envelope_value(49.0).unwrap() > curve.envelope_value(50.0).unwrap());
        assert_eq!(curve.envelope_value(100.0), None);
    }
    #[test]
    fn legacy_gain_modes_require_explicit_opt_in_and_keep_old_wire() {
        for mode in ["linear", "step"] {
            let old = format!(
                "@project\n@bus b\n  gain_automation: {mode} [1:1 - 2:1] {{1:1=1, 2:1=0.5}}\n"
            );
            let p = crate::parse(&old).unwrap();
            crate::validate(&p).unwrap();
            let serialized = crate::serialize(&p);
            assert!(
                !serialized.contains("mapping(")
                    && !serialized.contains("discontinuities")
                    && !serialized.contains("segments")
            );
            assert_eq!(crate::parse(&serialized).unwrap(), p);
            let bad = old.replace(" {", " mapping(linear, scale=2) {");
            assert!(crate::validate(&crate::parse(&bad).unwrap()).is_err());
        }
    }
    #[test]
    fn gain_mapping_rejects_bad_bounds_and_quantization_before_admission() {
        for mapping in [
            "mapping(linear, scale=100)",
            "mapping(linear, input_quantization_steps=0)",
            "mapping(linear, input_quantization_steps=1000001)",
        ] {
            let p = project("segments", &format!("discontinuities {mapping}"));
            assert!(crate::validate(&p).is_err(), "{mapping}");
        }
    }
}
