//! One parameter surface for every hosted format, including formats without an
//! editor.
//!
//! Each host converts what its plugin reports into [`ParamDescriptor`]s and hands
//! them to [`surface`], so a format that lists parameters and a format that
//! cannot are the same type for the panel. An empty list never reaches the UI:
//! it is reported as [`ParamSurfaceIssue::PluginReportsNoParameters`].
//!
//! `by_passing` is only exact where the format names a bypass control (LV2
//! `lv2:enabled`). VST2 and AU carry no such flag, so it follows the plugin's own
//! naming convention: a control whose name says "bypass" and whose value is on.

use serde::{Deserialize, Serialize};

/// One parameter as the generic panel renders it.
///
/// `current`, `min`, `max` and `default` are in the plugin's own units;
/// `normalized` is the 0..1 domain the automation lane stores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDescriptor {
    /// Position in the plugin's own parameter list; the panel's order.
    pub index: u32,
    /// Name the plugin reports; also the lane target.
    pub name: String,
    /// The plugin's unit label ("dB", "Hz", "%"), empty when it reports none.
    pub unit: String,
    pub min: f64,
    pub max: f64,
    pub current: f64,
    /// The plugin's own formatted value at read time ("1.20 kHz", "-12.3 dB").
    ///
    /// VST2 is the only format that produces this text itself; the panel shows
    /// its own numeric form once the value has been moved.
    pub display: String,
    /// `current` on the 0..1 domain.
    pub normalized: f64,
    /// The plugin's own default; `None` when the format reports none.
    pub default: Option<f64>,
    /// Step count for a switch, integer or enumerated control.
    pub steps: Option<u32>,
    /// The plugin accepts automation for this parameter.
    pub automatable: bool,
    /// A bypass switch that is currently on.
    pub by_passing: bool,
    /// Group the plugin assigns; the panel's section title.
    pub group: Option<String>,
}

impl ParamDescriptor {
    /// Value of `normalized` for a control whose plugin reports no range.
    pub fn decode(&self, normalized: f64) -> f64 {
        denormalize(normalized, self.min, self.max)
    }

    /// The 0..1 value of `value` in the plugin's units.
    pub fn encode(&self, value: f64) -> f64 {
        normalize(value, self.min, self.max)
    }
}

/// Why a plugin cannot offer a parameter surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamSurfaceIssue {
    /// The format has no host-readable parameter list.
    FormatHasNoParameters { format: String },
    /// The format has its own editor, so the generated panel is not used.
    EditorFormat { format: String },
    /// The plugin loaded but reports no parameters.
    PluginReportsNoParameters,
    /// The instance could not be loaded or queried.
    QueryFailed { message: String },
}

/// What a format can tell the panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamSurface {
    Parameters { parameters: Vec<ParamDescriptor> },
    Unavailable { issue: ParamSurfaceIssue },
}

impl ParamSurface {
    /// The descriptors, or an empty slice when the surface is unavailable.
    pub fn parameters(&self) -> &[ParamDescriptor] {
        match self {
            Self::Parameters { parameters } => parameters,
            Self::Unavailable { .. } => &[],
        }
    }
}

/// Build a surface from descriptors; no parameters is a typed unavailability.
pub fn surface(descriptors: Vec<ParamDescriptor>) -> ParamSurface {
    if descriptors.is_empty() {
        ParamSurface::Unavailable {
            issue: ParamSurfaceIssue::PluginReportsNoParameters,
        }
    } else {
        ParamSurface::Parameters {
            parameters: descriptors,
        }
    }
}

/// A format without a parameter list at all.
pub fn unavailable_for_format(format: &str) -> ParamSurface {
    ParamSurface::Unavailable {
        issue: ParamSurfaceIssue::FormatHasNoParameters {
            format: format.to_owned(),
        },
    }
}

/// A format the generated panel does not serve because it has its own editor.
pub fn unavailable_editor_format(format: &str) -> ParamSurface {
    ParamSurface::Unavailable {
        issue: ParamSurfaceIssue::EditorFormat {
            format: format.to_owned(),
        },
    }
}

/// A failed instantiation or query, with the message the panel shows.
pub fn query_failed(message: impl Into<String>) -> ParamSurface {
    ParamSurface::Unavailable {
        issue: ParamSurfaceIssue::QueryFailed {
            message: message.into(),
        },
    }
}

/// Map a value in the plugin's units onto 0..1.
pub fn normalize(value: f64, min: f64, max: f64) -> f64 {
    let span = max - min;
    if !span.is_finite() || span.abs() <= f64::EPSILON {
        return 0.0;
    }
    ((value - min) / span).clamp(0.0, 1.0)
}

/// Map a 0..1 value back onto the plugin's units.
pub fn denormalize(normalized: f64, min: f64, max: f64) -> f64 {
    let span = max - min;
    if !span.is_finite() || span.abs() <= f64::EPSILON {
        return min;
    }
    min + normalized.clamp(0.0, 1.0) * span
}

/// A value plus its unit, for formats that report a unit but no display text.
pub fn display_text(value: f64, unit: &str) -> String {
    let unit = unit.trim();
    if unit.is_empty() {
        format!("{value:.3}")
    } else {
        format!("{value:.3} {unit}")
    }
}

/// A control whose value is a bypass switch that is currently on.
pub fn names_bypass(name: &str) -> bool {
    name.to_ascii_lowercase().contains("bypass")
}

/// Step count for an integer control spanning `min..=max`.
pub fn integer_steps(min: f64, max: f64) -> Option<u32> {
    let span = max - min;
    if !span.is_finite() || !(1.0..=1024.0).contains(&span) {
        return None;
    }
    Some(span.round() as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(index: u32, name: &str, min: f64, max: f64, current: f64) -> ParamDescriptor {
        ParamDescriptor {
            index,
            name: name.to_owned(),
            unit: "dB".into(),
            min,
            max,
            current,
            display: display_text(current, "dB"),
            normalized: normalize(current, min, max),
            default: Some(min),
            steps: None,
            automatable: true,
            by_passing: false,
            group: None,
        }
    }

    #[test]
    fn a_twenty_parameter_surface_keeps_the_plugin_order() {
        let descriptors: Vec<ParamDescriptor> = (0..20)
            .map(|index| {
                descriptor(
                    index,
                    &format!("Param {index}"),
                    0.0,
                    10.0,
                    f64::from(index) / 2.0,
                )
            })
            .collect();
        let surface = surface(descriptors);
        let ParamSurface::Parameters { parameters } = &surface else {
            panic!("expected parameters");
        };
        assert_eq!(parameters.len(), 20);
        assert_eq!(parameters[19].name, "Param 19");
        assert_eq!(parameters[19].index, 19);
        assert!((parameters[4].normalized - 0.2).abs() < 1e-9);
        assert_eq!(parameters[4].decode(0.5), 5.0);
        assert_eq!(parameters[4].encode(7.5), 0.75);
    }

    #[test]
    fn an_empty_parameter_list_is_a_typed_unavailability() {
        let surface = surface(Vec::new());
        assert_eq!(
            surface,
            ParamSurface::Unavailable {
                issue: ParamSurfaceIssue::PluginReportsNoParameters
            }
        );
        assert!(surface.parameters().is_empty());
    }

    #[test]
    fn a_format_without_a_parameter_list_names_itself() {
        let surface = unavailable_for_format("soundfont");
        assert_eq!(
            surface,
            ParamSurface::Unavailable {
                issue: ParamSurfaceIssue::FormatHasNoParameters {
                    format: "soundfont".into()
                }
            }
        );
    }

    #[test]
    fn a_zero_width_range_stays_at_its_lowest_value() {
        assert_eq!(normalize(3.0, 2.0, 2.0), 0.0);
        assert_eq!(denormalize(0.7, 2.0, 2.0), 2.0);
        assert_eq!(normalize(-4.0, 0.0, 1.0), 0.0);
        assert_eq!(denormalize(4.0, 0.0, 1.0), 1.0);
    }

    #[test]
    fn step_counts_come_from_the_reported_range() {
        assert_eq!(integer_steps(0.0, 1.0), Some(2));
        assert_eq!(integer_steps(0.0, 15.0), Some(16));
        assert_eq!(integer_steps(0.0, 0.5), None);
        assert_eq!(integer_steps(0.0, 44100.0), None);
    }

    #[test]
    fn the_wire_shape_is_stable_for_the_panel() {
        let surface = surface(vec![ParamDescriptor {
            steps: Some(2),
            group: Some("Filter".into()),
            ..descriptor(0, "Bypass", 0.0, 1.0, 1.0)
        }]);
        let json = serde_json::to_string(&surface).expect("serialize");
        assert!(json.contains("\"kind\":\"parameters\""), "{json}");
        assert!(json.contains("\"steps\":2"), "{json}");
        assert!(json.contains("\"group\":\"Filter\""), "{json}");
        let unavailable = serde_json::to_string(&unavailable_for_format("au")).expect("serialize");
        assert!(
            unavailable.contains("\"kind\":\"format_has_no_parameters\""),
            "{unavailable}"
        );
        let failed = serde_json::to_string(&query_failed("no component")).expect("serialize");
        assert!(failed.contains("\"kind\":\"query_failed\""), "{failed}");
    }
}
