//! Explicit point and stepped-ramp representations of note-relative pitch.
//! Uses the shared note-relative pitch domain, independent of scalar expressions.
pub use oc_device_schema::note_expression::{MAX_PITCH_CURVE_POINTS, PitchCurveMode};
use oc_dsl::ast::ParamValue;

/// Largest note-relative pitch offset.
pub const MAX_SEMITONES: f64 = oc_device_schema::synth::NATIVE_PITCH_OFFSET.max;

/// Explicit note-relative control horizon; does not extend the key-down time.
pub fn horizon(modifiers: &[(String, ParamValue)], duration: f64) -> Result<f64, String> {
    let entries: Vec<_> = modifiers
        .iter()
        .filter(|(k, _)| k == "pitch_curve_horizon")
        .collect();
    if entries.is_empty() {
        return Ok(duration);
    }
    if entries.len() != 1 || !modifiers.iter().any(|(k, _)| k == "pitch_curve") {
        return Err("pitch_curve_horizon requires exactly one pitch_curve".into());
    }
    match &entries[0].1 {
        ParamValue::Number(value) if value.is_finite() && *value >= duration && *value <= 256.0 => {
            Ok(*value)
        }
        _ => Err(
            "pitch_curve_horizon must be finite, at least note duration and at most256quarters"
                .into(),
        ),
    }
}

/// Read an explicit mode without changing the default continuous curve.
pub fn mode(modifiers: &[(String, ParamValue)]) -> Result<PitchCurveMode, String> {
    let mut modes = modifiers
        .iter()
        .filter(|(key, _)| key == "pitch_curve_mode");
    let Some((_, value)) = modes.next() else {
        return Ok(PitchCurveMode::default());
    };
    if modes.next().is_some() || !modifiers.iter().any(|(key, _)| key == "pitch_curve") {
        return Err("pitch_curve_mode requires exactly one mode and a pitch_curve".into());
    }
    let ParamValue::String(value) = value else {
        return Err("pitch_curve_mode requires linear or step".into());
    };
    let mode =
        PitchCurveMode::from_token(value).ok_or("pitch_curve_mode requires linear or step")?;
    if mode == PitchCurveMode::Step
        && modifiers
            .iter()
            .any(|(key, _)| key == "pitch_curve_tension")
    {
        return Err("step pitch_curve cannot combine with pitch_curve_tension".into());
    }
    Ok(mode)
}

/// Hermite uses a forward slope at the first point, centered slopes at interior
/// points and a zero final tangent.
pub fn interpolation(modifiers: &[(String, ParamValue)]) -> Result<Option<f64>, String> {
    mode(modifiers)?;
    let values: Vec<_> = modifiers
        .iter()
        .filter(|(k, _)| k == "pitch_curve_tension")
        .collect();
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 || !modifiers.iter().any(|(k, _)| k == "pitch_curve") {
        return Err("pitch_curve_tension requires exactly one pitch_curve".into());
    }
    let tension = match &values[0].1 {
        ParamValue::Number(v) => *v,
        _ => return Err("pitch_curve_tension must be numeric".into()),
    };
    if !tension.is_finite() || !(0.0..=1.0).contains(&tension) {
        return Err("pitch_curve_tension must be finite in0..1".into());
    }
    Ok(Some(tension))
}

/// Interpolate the curve at `t`, optionally with Hermite tension.
pub fn value_at(points: &[(f64, f64)], t: f64, tension: Option<f64>) -> f64 {
    let i = points.partition_point(|p| p.0 <= t).saturating_sub(1);
    let (a, y) = points[i];
    let Some(&(b, z)) = points.get(i + 1) else {
        return y;
    };
    let u = (t - a) / (b - a);
    let Some(tension) = tension else {
        return y + (z - y) * u;
    };
    let tangent = |j: usize| {
        if j + 1 == points.len() {
            0.0
        } else if j == 0 {
            (points[1].1 - points[0].1) / (points[1].0 - points[0].0)
        } else {
            (points[j + 1].1 - points[j - 1].1) / (points[j + 1].0 - points[j - 1].0)
        }
    };
    let u2 = u * u;
    let u3 = u2 * u;
    (2.0 * u3 - 3.0 * u2 + 1.0) * y
        + (u3 - 2.0 * u2 + u) * tangent(i) * (b - a) * tension
        + (-2.0 * u3 + 3.0 * u2) * z
        + (u3 - u2) * tangent(i + 1) * (b - a) * tension
}

/// Parse `quarters:cents` points inside the shared note-relative pitch domain.
pub fn parse(
    modifiers: &[(String, ParamValue)],
    duration: f64,
) -> Result<Option<Vec<(f64, f64)>>, String> {
    let mode = mode(modifiers)?;
    let duration = horizon(modifiers, duration)?;
    let mut entries = modifiers.iter().filter(|(k, _)| k == "pitch_curve");
    let Some((_, ParamValue::String(text))) = entries.next() else {
        if modifiers.iter().any(|(k, _)| k == "pitch_curve") {
            return Err("pitch_curve requires a quoted quarters:cents list".into());
        }
        return Ok(None);
    };
    if entries.next().is_some() {
        return Err("duplicate pitch_curve".into());
    }
    if let Some(arguments) = text
        .strip_prefix("steps(")
        .and_then(|text| text.strip_suffix(')'))
    {
        if mode != PitchCurveMode::Step {
            return Err("pitch_curve steps(...) requires pitch_curve_mode=step".into());
        }
        return parse_step_ramp(arguments, duration).map(Some);
    }
    let mut points = Vec::new();
    for part in text.split(',') {
        let (t, v) = part
            .split_once(':')
            .ok_or("pitch_curve expects quarters:cents")?;
        let t: f64 = t.trim().parse().map_err(|_| "invalid curve time")?;
        let v: f64 = v.trim().parse().map_err(|_| "invalid curve cents")?;
        if !t.is_finite()
            || !v.is_finite()
            || t < 0.0
            || t > duration
            || v.abs() > MAX_SEMITONES * 100.0
            || points.last().is_some_and(|(previous, _)| t <= *previous)
        {
            return Err(format!(
                "pitch_curve requires ordered in-note times and finite +/-{} cents",
                MAX_SEMITONES * 100.0
            ));
        }
        points.push((t, v));
        if points.len() > MAX_PITCH_CURVE_POINTS {
            return Err(format!(
                "pitch_curve exceeds {MAX_PITCH_CURVE_POINTS} points"
            ));
        }
    }
    if points.first().map(|p| p.0) != Some(0.0) {
        return Err("pitch_curve must start at zero".into());
    }
    Ok(Some(points))
}

/// Expand a uniformly clocked linear pitch command into the existing note events.
fn parse_step_ramp(arguments: &str, duration: f64) -> Result<Vec<(f64, f64)>, String> {
    let mut parts = arguments.split(',').map(str::trim);
    let [
        Some(clock),
        Some(count),
        Some(before),
        Some(final_value),
        None,
    ] = std::array::from_fn(|_| parts.next())
    else {
        return Err(
            "pitch_curve steps expects ticks per quarter, count, initial and final cents".into(),
        );
    };
    let clock: u32 = clock
        .parse()
        .map_err(|_| "invalid pitch_curve step clock")?;
    let count: usize = count
        .parse()
        .map_err(|_| "invalid pitch_curve step count")?;
    let before: f64 = before
        .parse()
        .map_err(|_| "invalid initial pitch_curve cents")?;
    let final_value: f64 = final_value
        .parse()
        .map_err(|_| "invalid final pitch_curve cents")?;
    let cents_range = -MAX_SEMITONES * 100.0..=MAX_SEMITONES * 100.0;
    if clock == 0
        || !(1..=MAX_PITCH_CURVE_POINTS).contains(&count)
        || (count - 1) as f64 / f64::from(clock) > duration
        || !cents_range.contains(&before)
        || !cents_range.contains(&final_value)
    {
        return Err("pitch_curve steps exceeds the note horizon, count or cents bounds".into());
    }
    let delta = final_value - before;
    Ok((0..count)
        .map(|i| {
            let quarter = i as f64 / f64::from(clock);
            let cents = if i + 1 == count {
                final_value
            } else {
                before + delta * (i + 1) as f64 / count as f64
            };
            (quarter, cents)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_and_invalid_curves() {
        let curve = |s: &str| vec![("pitch_curve".into(), ParamValue::String(s.into()))];
        assert_eq!(
            parse(&curve("0:0,0.5:100,1:200"), 1.0)
                .unwrap()
                .unwrap()
                .len(),
            3
        );
        assert!(parse(&curve("0:-1200,1:1400"), 1.0).is_ok());
        assert!(parse(&curve("0:0,1:-3700"), 1.0).is_ok());
        assert!(parse(&curve("0:-9600,1:9600"), 1.0).is_ok());
        for bad in ["0:NaN", "0:9601", "0:0,0:1", "1:0", "0:0,2:0"] {
            assert!(parse(&curve(bad), 1.0).is_err());
        }
    }

    #[test]
    fn hermite_geometry_uses_explicit_tension_and_endpoint_tangents() {
        let p = [(0.0, 0.0), (1.0, 100.0)];
        assert_eq!(value_at(&p, 0.5, None), 50.0);
        assert_eq!(value_at(&p, 0.5, Some(1.0)), 62.5);
        assert_eq!(value_at(&p, 2.0, Some(1.0)), 100.0);
        assert!(interpolation(&[("pitch_curve_tension".into(), ParamValue::Number(1.0))]).is_err());
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;

    fn modifiers(points: &str) -> Vec<(String, ParamValue)> {
        vec![
            ("pitch_curve".into(), ParamValue::String(points.into())),
            ("pitch_curve_mode".into(), ParamValue::String("step".into())),
        ]
    }

    #[test]
    fn step_ramp_matches_the_explicit_clock_and_cents_without_changing_bounds() {
        let compact = modifiers("steps(16, 5, 0, -1200)");
        let explicit = modifiers("0:-240,0.0625:-480,0.125:-720,0.1875:-960,0.25:-1200");
        assert_eq!(
            parse(&compact, 0.25).unwrap(),
            parse(&explicit, 0.25).unwrap()
        );
        assert_eq!(
            parse(&modifiers("steps(8,2,100,200)"), 0.125)
                .unwrap()
                .unwrap(),
            [(0.0, 150.0), (0.125, 200.0)]
        );
        assert!(
            parse(&compact[..1], 1.0).is_err(),
            "requires explicit step mode"
        );
        assert!(parse(&compact, 0.24).is_err(), "cannot extend the horizon");
        for invalid in [
            "steps(0,1,0,0)",
            "steps(1,0,0,0)",
            "steps(96000,1025,0,0)",
            "steps(1,1,NaN,0)",
            "steps(1,1,0,9601)",
            "steps(1,1,0,0,1)",
        ] {
            assert!(parse(&modifiers(invalid), 1.0).is_err(), "{invalid}");
        }
    }

    #[test]
    fn step_mode_is_explicit_bounded_and_refuses_conflicting_controls() {
        let valid = modifiers("0:0,0.25:1200,1:-1200");
        assert_eq!(mode(&valid).unwrap(), PitchCurveMode::Step);
        assert_eq!(interpolation(&valid).unwrap(), None);
        assert_eq!(mode(&[]).unwrap(), PitchCurveMode::Linear);
        let mut conflict = valid.clone();
        conflict.push(("pitch_curve_tension".into(), ParamValue::Number(1.0)));
        assert!(parse(&conflict, 1.0).is_err());
        assert!(interpolation(&conflict).is_err());
        let mut duplicate = valid.clone();
        duplicate.push(valid[1].clone());
        assert!(parse(&duplicate, 1.0).is_err());
        assert!(parse(&valid[1..], 1.0).is_err());
        for value in [ParamValue::Number(1.0), ParamValue::String("smooth".into())] {
            let mut invalid = valid.clone();
            invalid[1].1 = value;
            assert!(parse(&invalid, 1.0).is_err());
        }
        for points in ["0:0,0.2:NaN", "0:0,0.2:9601", "0:0,0:100", "0:0,2:100"] {
            assert!(parse(&modifiers(points), 1.0).is_err());
        }
        // Dense points remain inside the independent duration bound.
        let points = (0..MAX_PITCH_CURVE_POINTS)
            .map(|i| format!("{}:0", i as f64 / MAX_PITCH_CURVE_POINTS as f64))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse(&modifiers(&points), 1.0).unwrap().unwrap().len(),
            MAX_PITCH_CURVE_POINTS
        );
        assert!(parse(&modifiers(&(points + ",1:0")), 1.0).is_err());
    }
}
