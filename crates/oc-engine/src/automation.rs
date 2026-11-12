//! Lane-value conversion shared by the renderer and the editor read-out.
//!
//! A lane stores DSL values, but a device takes its own unit and a patch or
//! hosted plugin takes a normalized 0..1 position. Both the automated effect
//! wrapper and the GUI's `automation_value_at` command convert through these
//! functions, so a knob and the audio read the same number for the same point.

use oc_dsl::ast::{Effect, ParamValue, Project, Track};
use oc_dsp::schema::DeviceSchema;

use crate::dsp::{device_for_effect, validate_param};

/// How a patch lane's points map onto an instrument's normalized domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchNormalization {
    /// The track plays `builtin.open_synth`, so a unit is the patch unit.
    pub builtin: bool,
    /// The document predates the widened normalized table.
    pub recalibrate_legacy: bool,
}

/// The patch-lane normalization rule for one track.
pub fn patch_normalization(project: &Project, track: &Track) -> PatchNormalization {
    let builtin = track
        .preset
        .as_deref()
        .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
        .is_some_and(|preset| preset.plugin.as_deref() == Some("builtin.open_synth"));
    PatchNormalization {
        builtin,
        recalibrate_legacy: builtin && !project.properties.uses_wide_automation_normalization(),
    }
}

/// Convert one authored point into the unit the device receives.
///
/// A built-in device validates against its schema; a hosted plugin takes the
/// numeric value as its own normalized parameter.
pub fn point_value(
    parameter: &str,
    value: &ParamValue,
    device: Option<&DeviceSchema>,
) -> Result<f64, String> {
    match device {
        Some(device) => validate_param(device, parameter, value),
        None => plugin_value(parameter, value),
    }
}

/// Convert one effect-lane point into the unit the effect receives.
pub fn effect_point_value(
    effect: &Effect,
    parameter: &str,
    value: &ParamValue,
) -> Result<f64, String> {
    point_value(parameter, value, device_for_effect(&effect.effect_type))
}

/// Convert one patch or hosted-plugin lane point into normalized 0..1.
///
/// Returns `None` when the point carries no numeric value, which is the same
/// point the interpreter skips.
pub fn patch_point_value(
    normalization: PatchNormalization,
    param: &str,
    value: &ParamValue,
) -> Option<f64> {
    let normalized =
        crate::interpret::lane_param_value_to_normalized(param, value, normalization.builtin)?;
    if normalization.recalibrate_legacy && !crate::interpret::lane_param_value_is_physical(value) {
        return Some(
            oc_synth::synth::recalibrate_legacy_normalized_param(param, normalized)
                .unwrap_or(normalized),
        );
    }
    Some(normalized)
}

fn plugin_value(parameter: &str, value: &ParamValue) -> Result<f64, String> {
    match value {
        ParamValue::Bool(on) => Ok(if *on { 1.0 } else { 0.0 }),
        ParamValue::String(_) | ParamValue::Array(_) => Err(format!(
            "plugin effect parameter '{parameter}' needs a numeric automation value; got {value:?}"
        )),
        _ => value.as_f64().ok_or_else(|| {
            format!("plugin effect parameter '{parameter}' needs a numeric automation value")
        }),
    }
}

#[cfg(test)]
mod normalization_version_tests {
    use super::*;

    #[test]
    fn automation_normalization_supported_versions_preserve_alias_and_legacy_decoding() {
        let base = oc_dsl::parse(
            "@project\n@preset synth\n  plugin: \"builtin.open_synth\"\n@track lead | instrument: synth/lead_saw, preset: synth\n",
        )
        .unwrap();
        for (version, expected) in [(None, 4.0 / 6.0), (Some(1), 4.0 / 6.0), (Some(2), 1.0)] {
            let mut project = base.clone();
            project.properties.automation_normalization = version;
            let context = patch_normalization(&project, &project.tracks[0]);
            for alias in ["osc.waveform", "waveform", "osc.a.waveform"] {
                assert_eq!(
                    patch_point_value(context, alias, &ParamValue::Number(1.0)),
                    Some(expected)
                );
            }
            // Typed physical values bypass only the legacy normalized remapping.
            assert_eq!(
                patch_point_value(context, "osc.a.detune", &ParamValue::Cents(50.0)),
                Some(0.5)
            );
            assert!(crate::interpret::try_interpret(&project, &Default::default()).is_ok());
        }
    }

    #[cfg(feature = "plugin-host")]
    #[test]
    fn automation_normalization_unsupported_version_cannot_reuse_a_warm_range_cache() {
        let mut project = oc_dsl::parse(
            r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
@track keys | preset: tone, instrument: keys/piano
  @phrase held [1:1 - 2:1]
    C4 q mf
"#,
        )
        .unwrap();
        let path = std::path::Path::new("unused-version-check.sf2");
        let mut cache = crate::render_cache::RenderCache::new();
        let mut instances = Default::default();
        cache
            .render_incremental(&project, path, 48_000, &mut instances)
            .unwrap();
        assert!(cache.master_buffer().is_some());
        project.properties.automation_normalization = Some(255);
        let error = cache
            .render_range(
                &project,
                path,
                48_000,
                &mut instances,
                crate::render_cache::RangeRenderRequest {
                    start_sample: 0,
                    end_sample: 512,
                    tracks: None,
                    stems: false,
                },
            )
            .unwrap_err();
        assert!(error.contains("automation_normalization"), "{error}");
        assert!(
            cache.master_buffer().is_none(),
            "failed version must invalidate earlier audio"
        );
    }

    #[test]
    fn automation_normalization_unsupported_ast_is_refused_by_interpret_and_render_entries() {
        let base = oc_dsl::parse("@project\n").unwrap();
        for version in [0, 3, 255] {
            let mut project = base.clone();
            project.properties.automation_normalization = Some(version);
            let error = crate::interpret::try_interpret(&project, &Default::default()).unwrap_err();
            assert!(error.contains("automation_normalization"), "{error}");
            #[cfg(feature = "plugin-host")]
            {
                let error = crate::render::render_to_buffer_with_plugins(
                    &project,
                    std::path::Path::new("unused-version-check.sf2"),
                    48_000,
                    &mut Default::default(),
                )
                .unwrap_err();
                assert!(error.contains("automation_normalization"), "{error}");
                // The unhosted render facade must fail for the same reason, before
                // empty-project or missing-SoundFont handling can mask the version.
                let error = crate::render::render_to_buffer(
                    &project,
                    std::path::Path::new("unused-version-check.sf2"),
                    48_000,
                )
                .unwrap_err();
                assert!(error.contains("automation_normalization"), "{error}");
                let mut cache = crate::render_cache::RenderCache::new();
                let error = cache
                    .render_incremental(
                        &project,
                        std::path::Path::new("unused-version-check.sf2"),
                        48_000,
                        &mut Default::default(),
                    )
                    .unwrap_err();
                assert!(error.contains("automation_normalization"), "{error}");
                assert!(cache.master_buffer().is_none());
            }
        }
    }
}
