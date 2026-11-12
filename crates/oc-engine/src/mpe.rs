//! MPE project settings and their mapping to OpenSynth patch params.
//!
//! Zones live in the project document so offline render and streaming playback
//! resolve the same channel layout. Live input settings are applied separately.

use oc_dsl::ast::{ParamValue, Project};

/// Patch params shared by every OpenSynth instance built from a project.
pub fn patch_params(project: &Project) -> Vec<(String, ParamValue)> {
    let Some(config) = project.properties.mpe else {
        return Vec::new();
    };
    let mut params = Vec::with_capacity(5);
    if let Some(range) = config.lower {
        params.push((
            "mpe.lower_first".to_string(),
            ParamValue::Number(range.first as f64),
        ));
        params.push((
            "mpe.lower_last".to_string(),
            ParamValue::Number(range.last as f64),
        ));
    }
    if let Some(range) = config.upper {
        params.push((
            "mpe.upper_first".to_string(),
            ParamValue::Number(range.first as f64),
        ));
        params.push((
            "mpe.upper_last".to_string(),
            ParamValue::Number(range.last as f64),
        ));
    }
    params.push((
        "mpe.bend_range".to_string(),
        ParamValue::Number(config.bend_range_semitones()),
    ));
    params
}
