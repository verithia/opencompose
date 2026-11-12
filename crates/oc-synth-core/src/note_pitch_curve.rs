//! Explicit piecewise-linear, note-relative pitch curve (quarters:cents).
//! Bounded to two octaves; independent from legacy scalar-expression limits.
use oc_dsl::ast::ParamValue;

/// Largest note-relative pitch offset.
pub const MAX_SEMITONES: f64 = 24.0;

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

/// Hermite uses a forward slope at the first point, centered slopes at interior
/// points and a zero final tangent.
pub fn interpolation(modifiers: &[(String, ParamValue)]) -> Result<Option<f64>, String> {
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

/// Parse `quarters:cents` points, bounded to two octaves.
pub fn parse(
    modifiers: &[(String, ParamValue)],
    duration: f64,
) -> Result<Option<Vec<(f64, f64)>>, String> {
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
            return Err(
                "pitch_curve requires ordered in-note times and finite +/-2400 cents".into(),
            );
        }
        points.push((t, v));
        if points.len() > 128 {
            return Err("pitch_curve exceeds128points".into());
        }
    }
    if points.first().map(|p| p.0) != Some(0.0) {
        return Err("pitch_curve must start at zero".into());
    }
    Ok(Some(points))
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
        for bad in ["0:NaN", "0:2401", "0:0,0:1", "1:0", "0:0,2:0"] {
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
