//! Declarative parameter catalog for the built-in OpenSynth instrument.
//!
//! The engine's `SynthPatch::set_param` is the ABI authority; this table is the
//! single description the GUI, randomiser and future macro-learn read. Every
//! entry names concrete project keys, so a consistency test can assert the
//! engine accepts each one (see `crates/oc-synth-core/tests/synth_catalog.rs`).
//!
//! Enum option ids are the numeric project ABI (`mod.*.source`, `filter.model`,
//! `osc.*.warp_mode`, `fx.N.type`, ...), not Rust variant order. Keep the order
//! in each table identical to the engine's `from_value` mapping.

use serde::Serialize;

/// Project spelling of the built-in instrument plugin.
pub const SYNTH_PLUGIN_ID: &str = "builtin.open_synth";

/// Catalog schema revision; bump when fields change shape.
pub const SYNTH_CATALOG_VERSION: u32 = 4;

pub use crate::units::Unit as SynthUnit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// How a normalized 0..1 position maps onto an OpenSynth parameter's range.
pub enum SynthCurve {
    Linear,
    Log,
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// What kind of value a parameter holds, which decides how it is applied.
pub enum SynthValueKind {
    Continuous,
    Bool,
    Enum,
    /// Path or free text; applied by the loader, not `set_param`.
    String,
    /// Numeric array; applied by the loader via `is_array_param`.
    Array,
    /// Written by the serialiser, accepted but not a control.
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// The control the patch editor draws for a parameter.
pub enum SynthControl {
    Slider,
    Select,
    Toggle,
    None,
}

/// How far the engine actually acts on a parameter or enum option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportStatus {
    /// Fully implemented and reachable through project parameters.
    Working,
    /// Accepted and partly implemented; see the entry note.
    Partial,
    /// Accepted but produces no audible or measurable change.
    NoOp,
    /// Typed in the engine but unreachable from project parameters.
    Unmapped,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One OpenSynth parameter: its names, its range, its control and how far the
/// engine acts on it.
pub struct SynthParam {
    /// Concrete project keys sharing one description. The first is canonical.
    pub ids: &'static [&'static str],
    /// i18n key (`synth.param.*`) resolved by the frontend dictionaries.
    pub label: &'static str,
    pub group: &'static str,
    pub unit: SynthUnit,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
    pub curve: SynthCurve,
    /// Host-automation law, independent of the UI display curve.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization: Option<SynthDspLaw>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization_floor: Option<f64>,
    /// Exact affine coefficient when subtracting physical endpoints would
    /// change an established floating-point automation law.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization_span: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_runtime_bounds: Option<SynthSampleRateBounds>,
    pub kind: SynthValueKind,
    pub control: SynthControl,
    pub status: SupportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    pub aliases: &'static [&'static str],
}

/// Versioned host-automation law. UI display curves are a separate contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SynthDspLaw {
    /// Physical unit fractions retain their bits and reject out-of-domain inverses.
    Identity,
    Linear,
    Square,
    Cubic,
    Log10,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// How a synth parameter maps between normalized and physical values.
pub struct SynthDspNormalization {
    pub domain: SynthNumericDomain,
    pub law: SynthDspLaw,
    /// Positive automation floor when physical storage also admits zero.
    pub floor: Option<f64>,
    pub span: Option<f64>,
}

impl SynthDspNormalization {
    /// Whether the law and domain describe a usable mapping.
    pub fn is_valid(self) -> bool {
        if !self.domain.is_valid() {
            return false;
        }
        match self.law {
            SynthDspLaw::Identity => {
                self.floor.is_none()
                    && self.span.is_none()
                    && self.domain.min == 0.0
                    && self.domain.max == 1.0
            }
            SynthDspLaw::Linear => {
                self.floor.is_none()
                    && self.span.is_none_or(|span| {
                        let scale = self.domain.max.abs().max(self.domain.min.abs()).max(1.0);
                        span.is_finite()
                            && span > 0.0
                            && ((self.domain.min + span) - self.domain.max).abs()
                                <= 4.0 * f64::EPSILON * scale
                    })
            }
            SynthDspLaw::Square | SynthDspLaw::Cubic => {
                self.floor.is_none()
                    && self.span.is_none()
                    && self.domain.min == 0.0
                    && self.domain.max > 0.0
            }
            SynthDspLaw::Log10 | SynthDspLaw::Log => {
                let floor = self.floor.unwrap_or(self.domain.min);
                self.span.is_none()
                    && floor.is_finite()
                    && floor > 0.0
                    && floor >= self.domain.min
                    && floor < self.domain.max
            }
        }
    }

    /// Exact physical interval reachable by the host's normalized positions.
    /// A physical zero-rate value can be valid without being in this interval.
    pub fn normalization_domain(self) -> Option<(f64, f64)> {
        self.is_valid()
            .then(|| (self.floor.unwrap_or(self.domain.min), self.domain.max))
    }

    /// Map a normalized position to physical units, clamping to 0..1.
    pub fn denormalize(self, normalized: f64) -> Option<f64> {
        if !self.is_valid() || !normalized.is_finite() {
            return None;
        }
        let value = normalized.clamp(0.0, 1.0);
        match self.law {
            SynthDspLaw::Identity => Some(value),
            SynthDspLaw::Linear => match self.span {
                Some(span) => Some(self.domain.min + value * span),
                // Zero-origin physical controls historically multiply directly;
                // preserve signed zero as well as the established operation order.
                None if self.domain.min == 0.0 => Some(value * self.domain.max),
                None => self.domain.denormalize_linear(value),
            },
            SynthDspLaw::Square => Some(value.powi(2) * self.domain.max),
            SynthDspLaw::Cubic => Some(value.powi(3) * self.domain.max),
            SynthDspLaw::Log10 | SynthDspLaw::Log => {
                let floor = self.floor.unwrap_or(self.domain.min);
                Some(floor * (self.domain.max / floor).powf(value))
            }
        }
    }

    /// Map a physical value to a normalized position, or `None` outside the law.
    pub fn normalize(self, physical: f64) -> Option<f64> {
        if !self.is_valid() || !physical.is_finite() {
            return None;
        }
        let position = match self.law {
            SynthDspLaw::Identity => {
                return (self.domain.min..=self.domain.max)
                    .contains(&physical)
                    .then_some(physical);
            }
            SynthDspLaw::Linear => match self.span {
                Some(span) => (physical - self.domain.min) / span,
                None => return self.domain.normalize_linear(physical),
            },
            // Squared inverses reject negative physical times;
            // the cubic inverse saturates them at zero. Keep that distinction.
            SynthDspLaw::Square => (physical / self.domain.max).sqrt(),
            SynthDspLaw::Cubic => (physical / self.domain.max).cbrt(),
            SynthDspLaw::Log10 => {
                let floor = self.floor.unwrap_or(self.domain.min);
                (physical / floor).log10() / (self.domain.max / floor).log10()
            }
            SynthDspLaw::Log => {
                let floor = self.floor.unwrap_or(self.domain.min);
                (physical / floor).ln() / (self.domain.max / floor).ln()
            }
        };
        position.is_finite().then(|| position.clamp(0.0, 1.0))
    }
}

/// Physical numeric bounds, defaults and linear DSP normalization shared by
/// the catalog and parameter setters. Curved DSP laws remain explicit callers;
/// a UI display curve does not silently change host automation semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Inclusive physical range and default for one synth control.
pub struct SynthNumericDomain {
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

impl SynthNumericDomain {
    /// Whether the bounds are finite and contain the default.
    pub fn is_valid(self) -> bool {
        self.min.is_finite()
            && self.max.is_finite()
            && self.default.is_finite()
            && self.min <= self.max
            && (self.max - self.min).is_finite()
            && self.default >= self.min
            && self.default <= self.max
    }

    /// Clamp a physical value into the range; `None` when the domain is invalid.
    pub fn clamp(self, physical: f64) -> Option<f64> {
        (self.is_valid() && physical.is_finite()).then(|| physical.clamp(self.min, self.max))
    }

    /// Map 0..1 onto the range with saturating ends.
    pub fn denormalize_linear(self, normalized: f64) -> Option<f64> {
        if !self.is_valid() || !normalized.is_finite() {
            return None;
        }
        if normalized <= 0.0 {
            return Some(self.min);
        }
        if normalized >= 1.0 {
            return Some(self.max);
        }
        Some(self.min + normalized * (self.max - self.min))
    }

    /// Map a physical value onto 0..1, saturating outside the range.
    pub fn normalize_linear(self, physical: f64) -> Option<f64> {
        if !self.is_valid() || !physical.is_finite() {
            return None;
        }
        if self.max == self.min {
            return Some(0.0);
        }
        // Clamp physical values before subtracting, avoiding overflow at finite
        // outliers while preserving saturating host-automation behavior.
        Some(
            ((physical.clamp(self.min, self.max) - self.min) / (self.max - self.min))
                .clamp(0.0, 1.0),
        )
    }
}

/// Runtime cutoff bounds after modulation, independent of authored Hz limits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthSampleRateBounds {
    pub min: f64,
    pub max_sample_rate_ratio: f64,
}

impl SynthSampleRateBounds {
    /// Clamp to the minimum and to a fraction of the sample rate.
    pub fn clamp(self, value: f64, sample_rate: f64) -> f64 {
        value.clamp(self.min, sample_rate * self.max_sample_rate_ratio)
    }
}

/// Physical unit-fraction controls share these inclusive bounds.
/// Nonzero factory values remain declared by their individual descriptors.
pub const UNIT_FRACTION: SynthNumericDomain = SynthNumericDomain {
    min: 0.0,
    max: 1.0,
    default: 0.0,
};

/// Levels and envelope amplitudes use the shared unit-fraction domain.
pub const AMPLITUDE_FRACTION: SynthNumericDomain = UNIT_FRACTION;

/// Signed modulation fractions and shaping controls share this physical domain.
pub const BIPOLAR_MODULATION: SynthNumericDomain = SynthNumericDomain {
    min: -1.0,
    max: 1.0,
    default: 0.0,
};
/// Envelope-to-filter depth is stored in octaves, rather than a fraction.
pub const FILTER_ENVELOPE_DEPTH: SynthNumericDomain = SynthNumericDomain {
    min: -8.0,
    max: 8.0,
    default: 0.0,
};

/// The established pulse-width interpolation coefficient. It differs by one
/// rounding unit from subtracting the stored 0.05 and 0.95 endpoints.
pub const OSC_PULSE_WIDTH_SPAN: f64 = 0.9;

/// Authored filter cutoff range.
pub const FILTER_CUTOFF_HZ: SynthNumericDomain = SynthNumericDomain {
    min: 20.0,
    max: 20_000.0,
    default: 12_000.0,
};
/// Free-running LFO rate range.
pub const LFO_RATE_HZ: SynthNumericDomain = SynthNumericDomain {
    min: 0.0,
    max: 100.0,
    default: 5.2,
};
/// Step-modulator rate range.
pub const STEP_RATE_HZ: SynthNumericDomain = SynthNumericDomain {
    min: 0.01,
    max: 30.0,
    default: 4.0,
};
/// Lowest non-zero LFO rate used by automation.
pub const LFO_AUTOMATION_FLOOR_HZ: f64 = 0.01;
/// Runtime cutoff bounds after modulation.
pub const FILTER_RUNTIME_BOUNDS: SynthSampleRateBounds = SynthSampleRateBounds {
    min: 8.0,
    max_sample_rate_ratio: 0.45,
};

/// Physical time range shared by route storage, automation and DSP slew clamps.
pub const MOD_ROUTE_SLEW: SynthNumericDomain = SynthNumericDomain {
    min: 0.0,
    max: 0.5,
    default: 0.0,
};

/// Concert-pitch reference range.
pub const TUNING_REFERENCE_HZ: SynthNumericDomain = SynthNumericDomain {
    min: 400.0,
    max: 480.0,
    default: 440.0,
};
/// Transpose range in semitones.
pub const TUNING_TRANSPOSE: SynthNumericDomain = SynthNumericDomain {
    min: -24.0,
    max: 24.0,
    default: 0.0,
};
/// Fine-tune range in cents.
pub const TUNING_FINE: SynthNumericDomain = SynthNumericDomain {
    min: -100.0,
    max: 100.0,
    default: 0.0,
};

impl SynthParam {
    /// The first and canonical spelling of the parameter's key.
    pub const fn canonical(&self) -> &'static str {
        self.ids[0]
    }

    const fn alias(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    const fn note(mut self, note: &'static str) -> Self {
        self.note = Some(note);
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One field of a family: the suffix after the family prefix, plus its range
/// and the control that edits it.
pub struct FamilyField {
    /// Suffix after the index, without the leading dot; empty for scalar slots.
    pub suffix: &'static str,
    pub label: &'static str,
    pub unit: SynthUnit,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
    pub curve: SynthCurve,
    /// Host-automation law, independent of the UI display curve.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization: Option<SynthDspLaw>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization_floor: Option<f64>,
    /// Exact affine coefficient when subtracting physical endpoints would
    /// change an established floating-point automation law.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_normalization_span: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsp_runtime_bounds: Option<SynthSampleRateBounds>,
    pub kind: SynthValueKind,
    pub control: SynthControl,
    pub status: SupportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_id: Option<&'static str>,
}

/// An indexed parameter family such as `mod.<slot>.<field>`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// A numbered parameter family, such as one oscillator, one LFO or one envelope.
pub struct SynthFamily {
    pub id: &'static str,
    pub group: &'static str,
    /// Key prefix before the index, including the separator.
    pub prefix: &'static str,
    pub first: usize,
    pub last: usize,
    pub fields: &'static [FamilyField],
    /// The engine keeps unknown fields for module compatibility (FX racks).
    pub open_fields: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One option of an enumerated parameter.
pub struct SynthEnumOption {
    /// Numeric project id.
    pub id: usize,
    /// Technical label, kept in English in every locale (PWM, FM, RM, ...).
    pub label: &'static str,
    pub status: SupportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// A named enumeration that parameters refer to through `enum_id`.
pub struct SynthEnum {
    pub id: &'static str,
    pub label: &'static str,
    pub options: &'static [SynthEnumOption],
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One control of a synth FX module.
pub struct FxParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: SynthUnit,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
    pub kind: SynthValueKind,
    pub control: SynthControl,
    pub options: &'static [SynthEnumOption],
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One synth FX module type, with the parameter palette it is built from.
pub struct FxTypeSpec {
    /// Project spelling written to `fx.N.type`.
    pub id: &'static str,
    /// Technical display name.
    pub label: &'static str,
    /// Numeric `fx.N.type` ABI id.
    pub index: usize,
    pub status: SupportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    /// Controllable parameters; empty when the type has no editor controls.
    pub params: &'static [FxParamSpec],
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One group in the patch editor.
pub struct SynthGroup {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// A control that can be learned onto a mod-matrix destination.
pub struct ModLearnTarget {
    /// Project parameter key the control writes, such as `filter.cutoff`.
    pub control: &'static str,
    /// Numeric `mod.N.destination` id the route should target.
    pub destination: usize,
}

impl ModLearnTarget {
    const fn new(control: &'static str, destination: usize) -> Self {
        Self {
            control,
            destination,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// The whole OpenSynth parameter catalog, as serialised for the patch editor.
pub struct SynthCatalog {
    pub version: u32,
    pub units: &'static [crate::units::UnitDefinition],
    pub plugin_id: &'static str,
    pub groups: &'static [SynthGroup],
    pub params: &'static [SynthParam],
    /// The controls that carry the sound: oscillators, filter, amp envelope and
    /// the first mod slot. A model starts here instead of reading every page.
    pub primary: &'static [&'static str],
    pub families: &'static [SynthFamily],
    pub mod_sources: SynthEnum,
    pub mod_destinations: SynthEnum,
    pub mod_aux: SynthEnum,
    pub filter_models: SynthEnum,
    pub filter_modes: SynthEnum,
    pub filter_routings: SynthEnum,
    pub warp_modes: SynthEnum,
    pub waveforms: SynthEnum,
    pub sub_shapes: SynthEnum,
    pub lfo_shapes: SynthEnum,
    pub quality_modes: SynthEnum,
    pub fx_param_kinds: SynthEnum,
    pub fx_types: &'static [FxTypeSpec],
    /// Default control → destination pairs for macro learn and reverse lookup.
    pub mod_learn_targets: &'static [ModLearnTarget],
    /// Engine-typed modulation destinations with no numeric project id yet.
    pub unmapped_mod_destinations: &'static [SynthEnumOption],
}

const SLIDER: SynthControl = SynthControl::Slider;
const SELECT: SynthControl = SynthControl::Select;
const TOGGLE: SynthControl = SynthControl::Toggle;
const NONE: SynthControl = SynthControl::None;
const CONT: SynthValueKind = SynthValueKind::Continuous;
const BOOL: SynthValueKind = SynthValueKind::Bool;
const ENUM: SynthValueKind = SynthValueKind::Enum;
const STRING: SynthValueKind = SynthValueKind::String;
const ARRAY: SynthValueKind = SynthValueKind::Array;
const META: SynthValueKind = SynthValueKind::Metadata;
const LIN: SynthCurve = SynthCurve::Linear;
const LOG: SynthCurve = SynthCurve::Log;
const TIME: SynthCurve = SynthCurve::Time;
const WORK: SupportStatus = SupportStatus::Working;
const PART: SupportStatus = SupportStatus::Partial;

const fn family(
    id: &'static str,
    group: &'static str,
    prefix: &'static str,
    first: usize,
    last: usize,
    fields: &'static [FamilyField],
    open_fields: bool,
) -> SynthFamily {
    SynthFamily {
        id,
        group,
        prefix,
        first,
        last,
        fields,
        open_fields,
    }
}

const fn opt(id: usize, label: &'static str, status: SupportStatus) -> SynthEnumOption {
    SynthEnumOption {
        id,
        label,
        status,
        note: None,
    }
}

const fn opt_note(
    id: usize,
    label: &'static str,
    status: SupportStatus,
    note: &'static str,
) -> SynthEnumOption {
    SynthEnumOption {
        id,
        label,
        status,
        note: Some(note),
    }
}

const fn fx_choice(
    key: &'static str,
    label: &'static str,
    default: f64,
    options: &'static [SynthEnumOption],
) -> FxParamSpec {
    FxParamSpec {
        key,
        label,
        unit: SynthUnit::None,
        min: 0.0,
        max: (options.len() - 1) as f64,
        step: 1.0,
        default,
        kind: SynthValueKind::Enum,
        control: SynthControl::Select,
        options,
    }
}

const fn fx_type(
    id: &'static str,
    label: &'static str,
    index: usize,
    status: SupportStatus,
    params: &'static [FxParamSpec],
) -> FxTypeSpec {
    FxTypeSpec {
        id,
        label,
        index,
        status,
        note: None,
        params,
    }
}

const GROUPS: &[SynthGroup] = &[
    SynthGroup {
        id: "osc",
        label: "synth.group.osc",
    },
    SynthGroup {
        id: "sub",
        label: "synth.group.sub",
    },
    SynthGroup {
        id: "amp",
        label: "synth.group.amp",
    },
    SynthGroup {
        id: "filter",
        label: "synth.group.filter",
    },
    SynthGroup {
        id: "env",
        label: "synth.group.env",
    },
    SynthGroup {
        id: "lfo",
        label: "synth.group.lfo",
    },
    SynthGroup {
        id: "mod",
        label: "synth.group.mod",
    },
    SynthGroup {
        id: "fx",
        label: "synth.group.fx",
    },
    SynthGroup {
        id: "fxmod",
        label: "synth.group.fxmod",
    },
    SynthGroup {
        id: "voice",
        label: "synth.group.voice",
    },
    SynthGroup {
        id: "step",
        label: "synth.group.step",
    },
    SynthGroup {
        id: "file",
        label: "synth.group.file",
    },
];

const PARAMS: &[SynthParam] = &[
    // ---- Oscillators -----------------------------------------------------
    SynthParam {
        ids: &["osc.a.waveform"],
        label: "synth.param.waveform",
        group: "osc",
        unit: SynthUnit::None,
        min: 0.0,
        max: 6.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("waveform"),
        note: None,
        aliases: &[],
    }
    .alias(&["osc.waveform", "waveform"]),
    SynthParam {
        ids: &["osc.b.waveform"],
        label: "synth.param.waveform",
        group: "osc",
        unit: SynthUnit::None,
        min: 0.0,
        max: 6.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("waveform"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.c.waveform"],
        label: "synth.param.waveform",
        group: "osc",
        unit: SynthUnit::None,
        min: 0.0,
        max: 6.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("waveform"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.level"],
        label: "synth.param.level",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: 0.78,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.b.level"],
        label: "synth.param.level",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.c.level"],
        label: "synth.param.level",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "osc.a.pulse_width",
            "osc.b.pulse_width",
            "osc.c.pulse_width",
        ],
        label: "synth.param.pulse_width",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: 0.05,
        max: 0.95,
        step: 0.01,
        default: 0.5,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: Some(OSC_PULSE_WIDTH_SPAN),
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "osc.a.wavetable_position",
            "osc.b.wavetable_position",
            "osc.c.wavetable_position",
        ],
        label: "synth.param.wavetable_position",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.warp_mode", "osc.b.warp_mode", "osc.c.warp_mode"],
        label: "synth.param.warp_mode",
        group: "osc",
        unit: SynthUnit::None,
        min: 0.0,
        max: 69.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("warpMode"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "osc.a.warp_amount",
            "osc.b.warp_amount",
            "osc.c.warp_amount",
        ],
        label: "synth.param.warp_amount",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.warp_var", "osc.b.warp_var", "osc.c.warp_var"],
        label: "synth.param.warp_var",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.warp2_mode", "osc.b.warp2_mode", "osc.c.warp2_mode"],
        label: "synth.param.warp2_mode",
        group: "osc",
        unit: SynthUnit::None,
        min: 0.0,
        max: 69.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("warpMode"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "osc.a.warp2_amount",
            "osc.b.warp2_amount",
            "osc.c.warp2_amount",
        ],
        label: "synth.param.warp2_amount",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.warp2_var", "osc.b.warp2_var", "osc.c.warp2_var"],
        label: "synth.param.warp2_var",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.pan", "osc.b.pan", "osc.c.pan"],
        label: "synth.param.pan",
        group: "osc",
        unit: SynthUnit::None,
        min: -1.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.semitones"],
        label: "synth.param.semitones",
        group: "osc",
        unit: SynthUnit::Semitones,
        min: -48.0,
        max: 48.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.b.semitones", "osc.c.semitones"],
        label: "synth.param.semitones",
        group: "osc",
        unit: SynthUnit::Semitones,
        min: -24.0,
        max: 24.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.unison", "unison.voices"],
        label: "synth.param.unison",
        group: "osc",
        unit: SynthUnit::None,
        min: 1.0,
        max: 16.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.b.unison", "osc.c.unison"],
        label: "synth.param.unison",
        group: "osc",
        unit: SynthUnit::None,
        min: 1.0,
        max: 16.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.detune", "unison.detune", "detune"],
        label: "synth.param.detune",
        group: "osc",
        unit: SynthUnit::Cents,
        min: 0.0,
        max: 100.0,
        step: 0.1,
        default: 7.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.b.detune", "osc.c.detune"],
        label: "synth.param.detune",
        group: "osc",
        unit: SynthUnit::Cents,
        min: 0.0,
        max: 100.0,
        step: 0.1,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.a.width", "unison.width", "stereo_width"],
        label: "synth.param.width",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: 0.55,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.b.width", "osc.c.width"],
        label: "synth.param.width",
        group: "osc",
        unit: SynthUnit::Fraction,
        min: UNIT_FRACTION.min,
        max: UNIT_FRACTION.max,
        step: 0.01,
        default: UNIT_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Sub, noise, ring ------------------------------------------------
    SynthParam {
        ids: &["sub.level"],
        label: "synth.param.sub_level",
        group: "sub",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["sub.semitones"],
        label: "synth.param.semitones",
        group: "sub",
        unit: SynthUnit::Semitones,
        min: -48.0,
        max: 48.0,
        step: 1.0,
        default: -12.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["sub.fine"],
        label: "synth.param.fine",
        group: "sub",
        unit: SynthUnit::Cents,
        min: -100.0,
        max: 100.0,
        step: 0.1,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["sub.fine_cents"]),
    SynthParam {
        ids: &["sub.pan"],
        label: "synth.param.pan",
        group: "sub",
        unit: SynthUnit::None,
        min: -1.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["sub.waveform"],
        label: "synth.param.sub_shape",
        group: "sub",
        unit: SynthUnit::None,
        min: 0.0,
        max: 5.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("subShape"),
        note: None,
        aliases: &[],
    }
    .alias(&["sub.shape"]),
    SynthParam {
        ids: &["sub.initial_phase"],
        label: "synth.param.initial_phase",
        group: "sub",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["sub.contiguous_phase"],
        label: "synth.param.contiguous_phase",
        group: "sub",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["ring.level"],
        label: "synth.param.ring",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["noise.level"],
        label: "synth.param.noise",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["output.gain"],
        label: "synth.param.gain",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: 0.22,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["gain"]),
    SynthParam {
        ids: &["velocity.to_amp"],
        label: "synth.param.velocity",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: 1.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Envelopes -------------------------------------------------------
    SynthParam {
        ids: &["amp.attack"],
        label: "synth.param.attack",
        group: "amp",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.008,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["attack"]),
    SynthParam {
        ids: &["amp.decay"],
        label: "synth.param.decay",
        group: "amp",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.16,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["decay"]),
    SynthParam {
        ids: &["amp.sustain"],
        label: "synth.param.sustain",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: 0.72,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["sustain"]),
    SynthParam {
        ids: &["amp.release"],
        label: "synth.param.release",
        group: "amp",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 20.0,
        step: 0.001,
        default: 0.28,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["release"]),
    SynthParam {
        ids: &["amp.curve"],
        label: "synth.param.curve",
        group: "amp",
        unit: SynthUnit::None,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["amp.damping"],
        label: "synth.param.damping",
        group: "amp",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter_env.attack"],
        label: "synth.param.attack",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.01,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter_env.decay"],
        label: "synth.param.decay",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.2,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter_env.sustain"],
        label: "synth.param.sustain",
        group: "env",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: AMPLITUDE_FRACTION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter_env.release"],
        label: "synth.param.release",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 20.0,
        step: 0.001,
        default: 0.25,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter_env.curve"],
        label: "synth.param.curve",
        group: "env",
        unit: SynthUnit::None,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["env3.attack", "env4.attack"],
        label: "synth.param.attack",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.008,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["env3.decay", "env4.decay"],
        label: "synth.param.decay",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 10.0,
        step: 0.001,
        default: 0.16,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["env3.sustain", "env4.sustain"],
        label: "synth.param.sustain",
        group: "env",
        unit: SynthUnit::Fraction,
        min: AMPLITUDE_FRACTION.min,
        max: AMPLITUDE_FRACTION.max,
        step: 0.01,
        default: 0.72,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Identity),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["env3.release", "env4.release"],
        label: "synth.param.release",
        group: "env",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 20.0,
        step: 0.001,
        default: 0.28,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["env3.curve", "env4.curve"],
        label: "synth.param.curve",
        group: "env",
        unit: SynthUnit::None,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Filters ---------------------------------------------------------
    SynthParam {
        ids: &["filter.cutoff"],
        label: "synth.param.cutoff",
        group: "filter",
        unit: SynthUnit::Hz,
        min: FILTER_CUTOFF_HZ.min,
        max: FILTER_CUTOFF_HZ.max,
        step: 1.0,
        default: FILTER_CUTOFF_HZ.default,
        curve: LOG,
        dsp_normalization: Some(SynthDspLaw::Log10),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: Some(FILTER_RUNTIME_BOUNDS),
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["cutoff"]),
    SynthParam {
        ids: &["filter.resonance"],
        label: "synth.param.resonance",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.15,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["resonance"]),
    SynthParam {
        ids: &["filter.drive"],
        label: "synth.param.drive",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.wet"],
        label: "synth.param.wet",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.mode"],
        label: "synth.param.mode",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 3.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("filterMode"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.model"],
        label: "synth.param.model",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 11.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("filterModel"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.keytrack"],
        label: "synth.param.keytrack",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.25,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.keytrack.origin"],
        label: "synth.param.keytrack_origin",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 127.0,
        step: 1.0,
        default: 60.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.env.amount"],
        label: "synth.param.env_amount",
        group: "filter",
        unit: SynthUnit::Octaves,
        min: FILTER_ENVELOPE_DEPTH.min,
        max: FILTER_ENVELOPE_DEPTH.max,
        step: 0.1,
        default: FILTER_ENVELOPE_DEPTH.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.enabled"],
        label: "synth.param.enabled",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.cutoff"],
        label: "synth.param.cutoff",
        group: "filter",
        unit: SynthUnit::Hz,
        min: FILTER_CUTOFF_HZ.min,
        max: FILTER_CUTOFF_HZ.max,
        step: 1.0,
        default: FILTER_CUTOFF_HZ.default,
        curve: LOG,
        dsp_normalization: Some(SynthDspLaw::Log10),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: Some(FILTER_RUNTIME_BOUNDS),
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.resonance"],
        label: "synth.param.resonance",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.15,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.drive"],
        label: "synth.param.drive",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.wet"],
        label: "synth.param.wet",
        group: "filter",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.mode"],
        label: "synth.param.mode",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 3.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("filterMode"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter2.model"],
        label: "synth.param.model",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 11.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("filterModel"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["filter.routing"],
        label: "synth.param.routing",
        group: "filter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("filterRouting"),
        note: None,
        aliases: &[],
    },
    // ---- LFO 1 -----------------------------------------------------------
    SynthParam {
        ids: &["lfo.rate"],
        label: "synth.param.rate",
        group: "lfo",
        unit: SynthUnit::Hz,
        min: LFO_RATE_HZ.min,
        max: LFO_RATE_HZ.max,
        step: 0.01,
        default: LFO_RATE_HZ.default,
        curve: LOG,
        dsp_normalization: Some(SynthDspLaw::Log10),
        dsp_normalization_floor: Some(LFO_AUTOMATION_FLOOR_HZ),
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.pitch"],
        label: "synth.param.pitch",
        group: "lfo",
        unit: SynthUnit::Cents,
        min: 0.0,
        max: 200.0,
        step: 0.1,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.waveform"],
        label: "synth.param.waveform",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 6.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: PART,
        enum_id: Some("waveform"),
        note: None,
        aliases: &[],
    }
    .note("Retired selector; folds into the LFO shape and is kept for old documents."),
    SynthParam {
        ids: &["lfo.shape"],
        label: "synth.param.shape",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 4.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("lfoShape"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.shape.points"],
        label: "synth.param.shape_points",
        group: "lfo",
        unit: SynthUnit::None,
        min: 2.0,
        max: 480.0,
        step: 1.0,
        default: 3.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.retrigger"],
        label: "synth.param.retrigger",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.sync"],
        label: "synth.param.sync",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.division"],
        label: "synth.param.division",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 5.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.sync_div"],
        label: "synth.param.sync_div",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 42.0,
        step: 1.0,
        default: 22.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.smooth"],
        label: "synth.param.smooth",
        group: "lfo",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.rise"],
        label: "synth.param.rise",
        group: "lfo",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 5.0,
        step: 0.01,
        default: 0.0,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.delay"],
        label: "synth.param.delay",
        group: "lfo",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 5.0,
        step: 0.01,
        default: 0.0,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.bend"],
        label: "synth.param.bend",
        group: "lfo",
        unit: SynthUnit::None,
        min: -1.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.phase"],
        label: "synth.param.phase",
        group: "lfo",
        unit: SynthUnit::Degrees,
        min: 0.0,
        max: 360.0,
        step: 0.1,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.rate_x10"],
        label: "synth.param.rate_x10",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["lfo.shape.x", "lfo.shape.y", "lfo.shape.curve"],
        label: "synth.param.shape",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ARRAY,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- LFO 2..10 -------------------------------------------------------
    SynthParam {
        ids: &[
            "lfo2.rate",
            "lfo3.rate",
            "lfo4.rate",
            "lfo5.rate",
            "lfo6.rate",
            "lfo7.rate",
            "lfo8.rate",
            "lfo9.rate",
            "lfo10.rate",
        ],
        label: "synth.param.rate",
        group: "lfo",
        unit: SynthUnit::Hz,
        min: LFO_RATE_HZ.min,
        max: LFO_RATE_HZ.max,
        step: 0.01,
        default: LFO_RATE_HZ.default,
        curve: LOG,
        dsp_normalization: Some(SynthDspLaw::Log10),
        dsp_normalization_floor: Some(LFO_AUTOMATION_FLOOR_HZ),
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.waveform",
            "lfo3.waveform",
            "lfo4.waveform",
            "lfo5.waveform",
            "lfo6.waveform",
            "lfo7.waveform",
            "lfo8.waveform",
            "lfo9.waveform",
            "lfo10.waveform",
        ],
        label: "synth.param.waveform",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 6.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: PART,
        enum_id: Some("waveform"),
        note: None,
        aliases: &[],
    }
    .note("Retired selector; folds into the LFO shape and is kept for old documents."),
    SynthParam {
        ids: &[
            "lfo2.shape",
            "lfo3.shape",
            "lfo4.shape",
            "lfo5.shape",
            "lfo6.shape",
            "lfo7.shape",
            "lfo8.shape",
            "lfo9.shape",
            "lfo10.shape",
        ],
        label: "synth.param.shape",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 4.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("lfoShape"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.shape.points",
            "lfo3.shape.points",
            "lfo4.shape.points",
            "lfo5.shape.points",
            "lfo6.shape.points",
            "lfo7.shape.points",
            "lfo8.shape.points",
            "lfo9.shape.points",
            "lfo10.shape.points",
        ],
        label: "synth.param.shape_points",
        group: "lfo",
        unit: SynthUnit::None,
        min: 2.0,
        max: 480.0,
        step: 1.0,
        default: 3.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.retrigger",
            "lfo3.retrigger",
            "lfo4.retrigger",
            "lfo5.retrigger",
            "lfo6.retrigger",
            "lfo7.retrigger",
            "lfo8.retrigger",
            "lfo9.retrigger",
            "lfo10.retrigger",
        ],
        label: "synth.param.retrigger",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.sync",
            "lfo3.sync",
            "lfo4.sync",
            "lfo5.sync",
            "lfo6.sync",
            "lfo7.sync",
            "lfo8.sync",
            "lfo9.sync",
            "lfo10.sync",
        ],
        label: "synth.param.sync",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.division",
            "lfo3.division",
            "lfo4.division",
            "lfo5.division",
            "lfo6.division",
            "lfo7.division",
            "lfo8.division",
            "lfo9.division",
            "lfo10.division",
        ],
        label: "synth.param.division",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 5.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.sync_div",
            "lfo3.sync_div",
            "lfo4.sync_div",
            "lfo5.sync_div",
            "lfo6.sync_div",
            "lfo7.sync_div",
            "lfo8.sync_div",
            "lfo9.sync_div",
            "lfo10.sync_div",
        ],
        label: "synth.param.sync_div",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 42.0,
        step: 1.0,
        default: 22.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.smooth",
            "lfo3.smooth",
            "lfo4.smooth",
            "lfo5.smooth",
            "lfo6.smooth",
            "lfo7.smooth",
            "lfo8.smooth",
            "lfo9.smooth",
            "lfo10.smooth",
        ],
        label: "synth.param.smooth",
        group: "lfo",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.rise",
            "lfo3.rise",
            "lfo4.rise",
            "lfo5.rise",
            "lfo6.rise",
            "lfo7.rise",
            "lfo8.rise",
            "lfo9.rise",
            "lfo10.rise",
        ],
        label: "synth.param.rise",
        group: "lfo",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 5.0,
        step: 0.01,
        default: 0.0,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.delay",
            "lfo3.delay",
            "lfo4.delay",
            "lfo5.delay",
            "lfo6.delay",
            "lfo7.delay",
            "lfo8.delay",
            "lfo9.delay",
            "lfo10.delay",
        ],
        label: "synth.param.delay",
        group: "lfo",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 5.0,
        step: 0.01,
        default: 0.0,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.bend",
            "lfo3.bend",
            "lfo4.bend",
            "lfo5.bend",
            "lfo6.bend",
            "lfo7.bend",
            "lfo8.bend",
            "lfo9.bend",
            "lfo10.bend",
        ],
        label: "synth.param.bend",
        group: "lfo",
        unit: SynthUnit::None,
        min: -1.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.phase",
            "lfo3.phase",
            "lfo4.phase",
            "lfo5.phase",
            "lfo6.phase",
            "lfo7.phase",
            "lfo8.phase",
            "lfo9.phase",
            "lfo10.phase",
        ],
        label: "synth.param.phase",
        group: "lfo",
        unit: SynthUnit::Degrees,
        min: 0.0,
        max: 360.0,
        step: 0.1,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &[
            "lfo2.rate_x10",
            "lfo3.rate_x10",
            "lfo4.rate_x10",
            "lfo5.rate_x10",
            "lfo6.rate_x10",
            "lfo7.rate_x10",
            "lfo8.rate_x10",
            "lfo9.rate_x10",
            "lfo10.rate_x10",
        ],
        label: "synth.param.rate_x10",
        group: "lfo",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Step sequencer --------------------------------------------------
    SynthParam {
        ids: &["step.rate"],
        label: "synth.param.step_rate",
        group: "step",
        unit: SynthUnit::Hz,
        min: STEP_RATE_HZ.min,
        max: STEP_RATE_HZ.max,
        step: 0.01,
        default: STEP_RATE_HZ.default,
        curve: LOG,
        dsp_normalization: Some(SynthDspLaw::Log),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["step.sync"],
        label: "synth.param.step_sync",
        group: "step",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["step.division"],
        label: "synth.param.step_division",
        group: "step",
        unit: SynthUnit::None,
        min: 0.0,
        max: 5.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["step.smoothing"],
        label: "synth.param.step_smoothing",
        group: "step",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Voice, tuning, quality, MPE ------------------------------------
    SynthParam {
        ids: &["voice.mono"],
        label: "synth.param.mono",
        group: "voice",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["voice.polyphony"],
        label: "synth.param.polyphony",
        group: "voice",
        unit: SynthUnit::None,
        min: 1.0,
        max: 32.0,
        step: 1.0,
        default: 16.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["voice.legato"],
        label: "synth.param.legato",
        group: "voice",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["voice.glide"],
        label: "synth.param.glide",
        group: "voice",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 4.0,
        step: 0.001,
        default: 0.08,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Cubic),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["quality.oversampling"],
        label: "synth.param.oversampling",
        group: "voice",
        unit: SynthUnit::None,
        min: 0.0,
        max: 2.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("qualityMode"),
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.start_phase"],
        label: "synth.param.start_phase",
        group: "voice",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["osc.random_phase"],
        label: "synth.param.random_phase",
        group: "voice",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["tuning.reference_hz"],
        label: "synth.param.reference_hz",
        group: "voice",
        unit: SynthUnit::Hz,
        min: TUNING_REFERENCE_HZ.min,
        max: TUNING_REFERENCE_HZ.max,
        step: 0.1,
        default: TUNING_REFERENCE_HZ.default,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["tuning.transpose"],
        label: "synth.param.transpose",
        group: "voice",
        unit: SynthUnit::Semitones,
        min: TUNING_TRANSPOSE.min,
        max: TUNING_TRANSPOSE.max,
        step: 1.0,
        default: TUNING_TRANSPOSE.default,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["tuning.fine"],
        label: "synth.param.fine",
        group: "voice",
        unit: SynthUnit::Cents,
        min: TUNING_FINE.min,
        max: TUNING_FINE.max,
        step: 0.1,
        default: TUNING_FINE.default,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["mpe.enabled"],
        label: "synth.param.mpe_enabled",
        group: "voice",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["mpe.bend_range"],
        label: "synth.param.bend_range",
        group: "voice",
        unit: SynthUnit::Semitones,
        min: 0.0,
        max: 96.0,
        step: 1.0,
        default: 48.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["mpe.lower_first"],
        label: "synth.param.mpe_lower_first",
        group: "voice",
        unit: SynthUnit::None,
        min: 2.0,
        max: 16.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["mpe_first_channel"]),
    SynthParam {
        ids: &["mpe.lower_last"],
        label: "synth.param.mpe_lower_last",
        group: "voice",
        unit: SynthUnit::None,
        min: 2.0,
        max: 16.0,
        step: 1.0,
        default: 16.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    }
    .alias(&["mpe_last_channel"]),
    SynthParam {
        ids: &["mpe.upper_first"],
        label: "synth.param.mpe_upper_first",
        group: "voice",
        unit: SynthUnit::None,
        min: 1.0,
        max: 15.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["mpe.upper_last"],
        label: "synth.param.mpe_upper_last",
        group: "voice",
        unit: SynthUnit::None,
        min: 1.0,
        max: 15.0,
        step: 1.0,
        default: 15.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    // ---- Rack gate, metadata and loader-only keys ------------------------
    SynthParam {
        ids: &["fx.count"],
        label: "synth.param.fx_count",
        group: "fx",
        unit: SynthUnit::None,
        min: 0.0,
        max: 16.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["synth_version"],
        label: "synth.param.version",
        group: "voice",
        unit: SynthUnit::None,
        min: 0.0,
        max: 2.0,
        step: 1.0,
        default: 2.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: META,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["wavetable.file"],
        label: "synth.param.file",
        group: "file",
        unit: SynthUnit::None,
        min: 0.0,
        max: 0.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: STRING,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["open_synth_state_file"],
        label: "synth.param.file",
        group: "file",
        unit: SynthUnit::None,
        min: 0.0,
        max: 0.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: STRING,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["serum_file"],
        label: "synth.param.file",
        group: "file",
        unit: SynthUnit::None,
        min: 0.0,
        max: 0.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: STRING,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
    SynthParam {
        ids: &["serum_root"],
        label: "synth.param.file",
        group: "file",
        unit: SynthUnit::None,
        min: 0.0,
        max: 0.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: STRING,
        control: NONE,
        status: WORK,
        enum_id: None,
        note: None,
        aliases: &[],
    },
];

const MOD_FIELDS: &[FamilyField] = &[
    FamilyField {
        suffix: "source",
        label: "synth.param.mod_source",
        unit: SynthUnit::None,
        min: 0.0,
        max: 28.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("modSource"),
    },
    FamilyField {
        suffix: "destination",
        label: "synth.param.mod_destination",
        unit: SynthUnit::None,
        min: 0.0,
        max: 34.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("modDestination"),
    },
    FamilyField {
        suffix: "amount",
        label: "synth.param.mod_amount",
        unit: SynthUnit::Fraction,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "curve",
        label: "synth.param.mod_curve",
        unit: SynthUnit::None,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "slew_up",
        label: "synth.param.mod_slew_up",
        unit: SynthUnit::Seconds,
        min: MOD_ROUTE_SLEW.min,
        max: MOD_ROUTE_SLEW.max,
        step: 0.001,
        default: MOD_ROUTE_SLEW.default,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "slew_down",
        label: "synth.param.mod_slew_down",
        unit: SynthUnit::Seconds,
        min: MOD_ROUTE_SLEW.min,
        max: MOD_ROUTE_SLEW.max,
        step: 0.001,
        default: MOD_ROUTE_SLEW.default,
        curve: TIME,
        dsp_normalization: Some(SynthDspLaw::Square),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "bipolar",
        label: "synth.param.mod_bipolar",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "aux",
        label: "synth.param.mod_aux",
        unit: SynthUnit::None,
        min: 0.0,
        max: 29.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("modAux"),
    },
    FamilyField {
        suffix: "aux_invert",
        label: "synth.param.mod_aux_invert",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
    },
];

const FXMOD_FIELDS: &[FamilyField] = &[
    FamilyField {
        suffix: "source",
        label: "synth.param.fxmod_source",
        unit: SynthUnit::None,
        min: 0.0,
        max: 28.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("modSource"),
    },
    FamilyField {
        suffix: "fx_index",
        label: "synth.param.fxmod_fx_index",
        unit: SynthUnit::None,
        min: 0.0,
        max: 15.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SELECT,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "parameter",
        label: "synth.param.fxmod_parameter",
        unit: SynthUnit::None,
        min: 0.0,
        max: 26.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: Some("fxParam"),
    },
    FamilyField {
        suffix: "amount",
        label: "synth.param.fxmod_amount",
        unit: SynthUnit::Fraction,
        min: BIPOLAR_MODULATION.min,
        max: BIPOLAR_MODULATION.max,
        step: 0.01,
        default: BIPOLAR_MODULATION.default,
        curve: LIN,
        dsp_normalization: Some(SynthDspLaw::Linear),
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: CONT,
        control: SLIDER,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "bipolar",
        label: "synth.param.fxmod_bipolar",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
    },
];

const FX_RACK_FIELDS: &[FamilyField] = &[
    FamilyField {
        suffix: "type",
        label: "synth.param.fx_type",
        unit: SynthUnit::None,
        min: 0.0,
        max: 18.0,
        step: 1.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ENUM,
        control: SELECT,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "enabled",
        label: "synth.param.fx_enabled",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 1.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: BOOL,
        control: TOGGLE,
        status: WORK,
        enum_id: None,
    },
];

const MACRO_FIELD: &[FamilyField] = &[FamilyField {
    suffix: "",
    label: "synth.param.macro",
    unit: SynthUnit::Fraction,
    min: 0.0,
    max: 1.0,
    step: 0.01,
    default: 0.0,
    curve: LIN,
    dsp_normalization: None,
    dsp_normalization_floor: None,
    dsp_normalization_span: None,
    dsp_runtime_bounds: None,
    kind: CONT,
    control: SLIDER,
    status: WORK,
    enum_id: None,
}];

const STEP_FIELD: &[FamilyField] = &[FamilyField {
    suffix: "",
    label: "synth.param.step_value",
    unit: SynthUnit::Fraction,
    min: -1.0,
    max: 1.0,
    step: 0.01,
    default: 0.0,
    curve: LIN,
    dsp_normalization: Some(SynthDspLaw::Linear),
    dsp_normalization_floor: None,
    dsp_normalization_span: None,
    dsp_runtime_bounds: None,
    kind: CONT,
    control: SLIDER,
    status: WORK,
    enum_id: None,
}];

const CURVE_FIELD: &[FamilyField] = &[FamilyField {
    suffix: "",
    label: "synth.param.lfo_curve",
    unit: SynthUnit::Fraction,
    min: -1.0,
    max: 1.0,
    step: 0.01,
    default: 0.0,
    curve: LIN,
    dsp_normalization: None,
    dsp_normalization_floor: None,
    dsp_normalization_span: None,
    dsp_runtime_bounds: None,
    kind: CONT,
    control: NONE,
    status: WORK,
    enum_id: None,
}];

const SHAPE_ARRAY_FIELDS: &[FamilyField] = &[
    FamilyField {
        suffix: "shape.x",
        label: "synth.param.shape",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ARRAY,
        control: NONE,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "shape.y",
        label: "synth.param.shape",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ARRAY,
        control: NONE,
        status: WORK,
        enum_id: None,
    },
    FamilyField {
        suffix: "shape.curve",
        label: "synth.param.shape",
        unit: SynthUnit::None,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        default: 0.0,
        curve: LIN,
        dsp_normalization: None,
        dsp_normalization_floor: None,
        dsp_normalization_span: None,
        dsp_runtime_bounds: None,
        kind: ARRAY,
        control: NONE,
        status: WORK,
        enum_id: None,
    },
];

const FAMILIES: &[SynthFamily] = &[
    family("macro", "mod", "macro.", 1, 8, MACRO_FIELD, false),
    family("step", "step", "step.", 1, 16, STEP_FIELD, false),
    family("mod", "mod", "mod.", 1, 64, MOD_FIELDS, false),
    family("fxmod", "fxmod", "fxmod.", 1, 64, FXMOD_FIELDS, false),
    family("fx", "fx", "fx.", 1, 16, FX_RACK_FIELDS, true),
    family("lfo_curve", "lfo", "lfo.curve.", 0, 255, CURVE_FIELD, false),
    family("lfo_shape", "lfo", "lfo", 2, 10, SHAPE_ARRAY_FIELDS, false),
];

const MOD_SOURCES: SynthEnum = SynthEnum {
    id: "modSource",
    label: "Modulation source",
    options: &[
        opt(0, "LFO 1", WORK),
        opt(1, "Filter Env", WORK),
        opt(2, "Velocity", WORK),
        opt(3, "Macro 1", WORK),
        opt(4, "Macro 2", WORK),
        opt(5, "Macro 3", WORK),
        opt(6, "Macro 4", WORK),
        opt(7, "Keytrack", WORK),
        opt(8, "Aftertouch", WORK),
        opt(9, "Mod Wheel", WORK),
        opt(10, "Step Seq", WORK),
        opt(11, "LFO 2", WORK),
        opt(12, "LFO 3", WORK),
        opt(13, "LFO 4", WORK),
        opt(14, "LFO 5", WORK),
        opt(15, "LFO 6", WORK),
        opt(16, "LFO 7", WORK),
        opt(17, "LFO 8", WORK),
        opt(18, "LFO 9", WORK),
        opt(19, "LFO 10", WORK),
        opt(20, "Env 3", WORK),
        opt(21, "Env 4", WORK),
        opt(22, "Macro 5", WORK),
        opt(23, "Macro 6", WORK),
        opt(24, "Macro 7", WORK),
        opt(25, "Macro 8", WORK),
        opt(26, "Amp Env", WORK),
        opt(27, "Pitch Bend", WORK),
        opt(28, "Timbre", WORK),
    ],
};

/// `mod.N.aux`: 0 disables the auxiliary source; 1..=29 carry the same source
/// ids as [`MOD_SOURCES`] shifted by one. Labels repeat the source vocabulary on
/// purpose so the select can name "None" without moving the source ABI.
const MOD_AUX: SynthEnum = SynthEnum {
    id: "modAux",
    label: "Auxiliary modulation source",
    options: &[
        opt(0, "None", WORK),
        opt(1, "LFO 1", WORK),
        opt(2, "Filter Env", WORK),
        opt(3, "Velocity", WORK),
        opt(4, "Macro 1", WORK),
        opt(5, "Macro 2", WORK),
        opt(6, "Macro 3", WORK),
        opt(7, "Macro 4", WORK),
        opt(8, "Keytrack", WORK),
        opt(9, "Aftertouch", WORK),
        opt(10, "Mod Wheel", WORK),
        opt(11, "Step Seq", WORK),
        opt(12, "LFO 2", WORK),
        opt(13, "LFO 3", WORK),
        opt(14, "LFO 4", WORK),
        opt(15, "LFO 5", WORK),
        opt(16, "LFO 6", WORK),
        opt(17, "LFO 7", WORK),
        opt(18, "LFO 8", WORK),
        opt(19, "LFO 9", WORK),
        opt(20, "LFO 10", WORK),
        opt(21, "Env 3", WORK),
        opt(22, "Env 4", WORK),
        opt(23, "Macro 5", WORK),
        opt(24, "Macro 6", WORK),
        opt(25, "Macro 7", WORK),
        opt(26, "Macro 8", WORK),
        opt(27, "Amp Env", WORK),
        opt(28, "Pitch Bend", WORK),
        opt(29, "Timbre", WORK),
    ],
};

/// Default control → destination pairs for macro learn and reverse lookup.
///
/// Only controls whose destination has a numeric project id are listed: a
/// learn must never write a route the project cannot reload. Controls absent
/// from this table are reported as unmodulatable instead of being guessed at.
const MOD_LEARN_TARGETS: &[ModLearnTarget] = &[
    ModLearnTarget::new("filter.cutoff", 0),
    ModLearnTarget::new("filter.resonance", 3),
    ModLearnTarget::new("filter.drive", 9),
    ModLearnTarget::new("filter2.cutoff", 12),
    ModLearnTarget::new("filter2.resonance", 13),
    ModLearnTarget::new("filter2.drive", 14),
    ModLearnTarget::new("osc.a.pulse_width", 5),
    ModLearnTarget::new("osc.a.wavetable_position", 7),
    ModLearnTarget::new("osc.a.warp_amount", 16),
    ModLearnTarget::new("osc.a.warp2_amount", 19),
    ModLearnTarget::new("osc.b.level", 2),
    ModLearnTarget::new("osc.b.semitones", 8),
    ModLearnTarget::new("osc.b.wavetable_position", 22),
    ModLearnTarget::new("osc.b.warp_amount", 17),
    ModLearnTarget::new("osc.b.warp2_amount", 20),
    ModLearnTarget::new("osc.c.level", 10),
    ModLearnTarget::new("osc.c.wavetable_position", 11),
    ModLearnTarget::new("osc.c.warp_amount", 18),
    ModLearnTarget::new("osc.c.warp2_amount", 21),
    ModLearnTarget::new("ring.level", 6),
];

/// Numeric ids follow `mod_destination_from_value`; ids stop at 34 because the
/// engine never maps the noise/sub/wet voice destinations.
const MOD_DESTINATIONS: SynthEnum = SynthEnum {
    id: "modDestination",
    label: "Modulation destination",
    options: &[
        opt(0, "Filter Cutoff", WORK),
        opt(1, "Pitch", WORK),
        opt(2, "Oscillator Mix", WORK),
        opt(3, "Resonance", WORK),
        opt(4, "Stereo Width", WORK),
        opt(5, "Pulse Width A", WORK),
        opt(6, "Ring Mod Level", WORK),
        opt(7, "Wavetable Position A", WORK),
        opt(8, "Oscillator B Tune", WORK),
        opt(9, "Filter Drive", WORK),
        opt(10, "Oscillator C Level", WORK),
        opt(11, "Wavetable Position C", WORK),
        opt(12, "Filter 2 Cutoff", WORK),
        opt(13, "Filter 2 Resonance", WORK),
        opt(14, "Filter 2 Drive", WORK),
        opt(15, "Voice Amp", WORK),
        opt(16, "Warp Amount A", WORK),
        opt(17, "Warp Amount B", WORK),
        opt(18, "Warp Amount C", WORK),
        opt(19, "Warp 2 Amount A", WORK),
        opt(20, "Warp 2 Amount B", WORK),
        opt(21, "Warp 2 Amount C", WORK),
        opt(22, "Wavetable Position B", WORK),
        opt(23, "Source Density A", WORK),
        opt(24, "Source Density B", WORK),
        opt(25, "Source Density C", WORK),
        opt(26, "Source Position A", WORK),
        opt(27, "Source Position B", WORK),
        opt(28, "Source Position C", WORK),
        opt(29, "Source Morph A", WORK),
        opt(30, "Source Morph B", WORK),
        opt(31, "Source Morph C", WORK),
        opt(32, "Source Bend A", WORK),
        opt(33, "Source Bend B", WORK),
        opt(34, "Source Bend C", WORK),
    ],
};

const UNMAPPED_MOD_DESTINATIONS: &[SynthEnumOption] = &[
    opt_note(
        40,
        "Noise Level",
        SupportStatus::Unmapped,
        "Typed in the engine, but mod_destination_from_value has no numeric id: project routes cannot reach it yet.",
    ),
    opt_note(
        41,
        "Sub Level",
        SupportStatus::Unmapped,
        "Typed in the engine, but mod_destination_from_value has no numeric id: project routes cannot reach it yet.",
    ),
    opt_note(
        42,
        "Filter Wet",
        SupportStatus::Unmapped,
        "Typed in the engine, but mod_destination_from_value has no numeric id: project routes cannot reach it yet.",
    ),
    opt_note(
        43,
        "Filter 2 Wet",
        SupportStatus::Unmapped,
        "Typed in the engine, but mod_destination_from_value has no numeric id: project routes cannot reach it yet.",
    ),
];

const FILTER_MODELS: SynthEnum = SynthEnum {
    id: "filterModel",
    label: "Filter model",
    options: &[
        opt(0, "SVF", WORK),
        opt(1, "Ladder", WORK),
        opt(2, "Dirty Ladder", WORK),
        opt(3, "Ladder 3-pole", WORK),
        opt(4, "Ladder 1-pole", WORK),
        opt(5, "SVF 1-pole", WORK),
        opt(6, "SVF 3-pole", WORK),
        opt(7, "SVF 4-pole", WORK),
        opt(8, "Moog 6 dB", WORK),
        opt(9, "Moog 12 dB", WORK),
        opt(10, "Moog 18 dB", WORK),
        opt(11, "Moog 24 dB", WORK),
    ],
};

const FILTER_MODES: SynthEnum = SynthEnum {
    id: "filterMode",
    label: "Filter mode",
    options: &[
        opt(0, "Low-pass", WORK),
        opt(1, "Band-pass", WORK),
        opt(2, "High-pass", WORK),
        opt(3, "Notch", WORK),
    ],
};

const FILTER_ROUTINGS: SynthEnum = SynthEnum {
    id: "filterRouting",
    label: "Filter routing",
    options: &[opt(0, "Serial", WORK), opt(1, "Parallel", WORK)],
};

const WARP_MODES: SynthEnum = SynthEnum {
    id: "warpMode",
    label: "Warp mode",
    options: &[
        opt(0, "Off", WORK),
        opt(1, "Sync", WORK),
        opt(2, "Bend +", WORK),
        opt(3, "Bend -", WORK),
        opt(4, "Bend +/-", WORK),
        opt(5, "PWM", WORK),
        opt(6, "ASYM +", WORK),
        opt(7, "ASYM -", WORK),
        opt(8, "ASYM +/-", WORK),
        opt(9, "Flip", WORK),
        opt(10, "DLM", WORK),
        opt(11, "Remap 1", WORK),
        opt(12, "Remap 2", WORK),
        opt(13, "Remap 3", WORK),
        opt(14, "Remap 4", WORK),
        opt(15, "Quantize", WORK),
        opt(16, "Even/Odd", WORK),
        opt_note(
            17,
            "LPF",
            SupportStatus::NoOp,
            "Coefficient law unresolved; behaves as Off.",
        ),
        opt_note(
            18,
            "HPF",
            SupportStatus::NoOp,
            "Coefficient law unresolved; behaves as Off.",
        ),
        opt(19, "Tube", WORK),
        opt(20, "Soft Clip", WORK),
        opt(21, "Hard Clip", WORK),
        opt(22, "Diode 1", WORK),
        opt(23, "Diode 2", WORK),
        opt(24, "Lin Fold", WORK),
        opt(25, "Sin Fold", WORK),
        opt(26, "Zero Square", WORK),
        opt(27, "ASYM", WORK),
        opt(28, "Rectify", WORK),
        opt(29, "Sine Shaper", WORK),
        opt(30, "Stomp Box", WORK),
        opt(31, "Tape Sat", WORK),
        opt(32, "Soft Sat", WORK),
        opt_note(
            33,
            "FM OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            34,
            "FM OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            35,
            "FM Noise",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            36,
            "FM Sub",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            37,
            "FM Filt1",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            38,
            "FM Filt2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            39,
            "FMX OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            40,
            "FMX OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            41,
            "FMX Noise",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            42,
            "FMX Sub",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            43,
            "FMX Filt1",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            44,
            "FMX Filt2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            45,
            "FMP OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            46,
            "FMP OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            47,
            "FMP Noise",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            48,
            "FMP Sub",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            49,
            "FMP Filt1",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            50,
            "FMP Filt2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            51,
            "PD OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            52,
            "PD OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            53,
            "PD Noise",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            54,
            "PD Sub",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            55,
            "PD Filt1",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            56,
            "PD Filt2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            57,
            "Self PD",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            58,
            "AM OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            59,
            "AM OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            60,
            "AM Noise",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            61,
            "AM Sub",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            62,
            "AM Filt1",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            63,
            "AM Filt2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            64,
            "RM OSC",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt_note(
            65,
            "RM OSC2",
            SupportStatus::NoOp,
            "Selectable; the readout is unchanged.",
        ),
        opt(66, "RM Noise", WORK),
        opt(67, "RM Sub", WORK),
        opt(68, "RM Filt1", WORK),
        opt(69, "RM Filt2", WORK),
    ],
};

const WAVEFORMS: SynthEnum = SynthEnum {
    id: "waveform",
    label: "Oscillator waveform",
    options: &[
        opt(0, "Sine", WORK),
        opt(1, "Saw", WORK),
        opt(2, "Square", WORK),
        opt(3, "Triangle", WORK),
        opt(4, "Wavetable", WORK),
        opt_note(
            5,
            "Granular",
            PART,
            "Needs a granular source patch; a bare waveform id plays nothing useful.",
        ),
        opt_note(
            6,
            "Spectral",
            PART,
            "Needs a spectral source patch; a bare waveform id plays nothing useful.",
        ),
    ],
};

const SUB_SHAPES: SynthEnum = SynthEnum {
    id: "subShape",
    label: "Sub shape",
    options: &[
        opt(0, "Sine", WORK),
        opt(1, "Round Rect", WORK),
        opt(2, "Triangle", WORK),
        opt(3, "Saw", WORK),
        opt(4, "Square", WORK),
        opt(5, "Pulse", WORK),
    ],
};

const LFO_SHAPES: SynthEnum = SynthEnum {
    id: "lfoShape",
    label: "LFO shape",
    options: &[
        opt(0, "Curve", WORK),
        opt(1, "Path", WORK),
        opt(2, "Chaos A", WORK),
        opt(3, "Chaos B", WORK),
        opt(4, "Random & Hold", WORK),
    ],
};

const QUALITY_MODES: SynthEnum = SynthEnum {
    id: "qualityMode",
    label: "Oscillator quality",
    options: &[
        opt(0, "Eco · 1×", WORK),
        opt(1, "Studio · 2×", WORK),
        opt(2, "Ultra · 4×", WORK),
    ],
};

const FX_PARAM_KINDS: SynthEnum = SynthEnum {
    id: "fxParam",
    label: "FX modulation target",
    options: &[
        opt(0, "Mix", WORK),
        opt(1, "Feedback", WORK),
        opt(2, "Depth", WORK),
        opt(3, "Low Gain", WORK),
        opt(4, "High Gain", WORK),
        opt(5, "Drive", WORK),
        opt(6, "Low Freq", WORK),
        opt(7, "High Freq", WORK),
        opt(8, "Output Level", WORK),
        opt(9, "Bandwidth", WORK),
        opt(10, "Cutoff", WORK),
        opt(11, "Tone", WORK),
        opt(12, "Rate", WORK),
        opt(13, "Shift", WORK),
        opt(14, "Range", WORK),
        opt(15, "Width", WORK),
        opt(16, "High-pass", WORK),
        opt(17, "Low-pass", WORK),
        opt(18, "Balance", WORK),
        opt(19, "Reso 1", WORK),
        opt(20, "Reso 2", WORK),
        opt(21, "Time", WORK),
        opt(22, "Makeup", WORK),
        opt(23, "Resonance", WORK),
        opt(24, "Output Mix", WORK),
        opt(25, "Output Width", WORK),
        opt(26, "Delay Time", WORK),
    ],
};

const FX_FILTER_MODES: &[SynthEnumOption] = &[
    opt(0, "Low-pass", WORK),
    opt(1, "High-pass", WORK),
    opt(2, "Band-pass", WORK),
];

const DRIVE_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "drive",
        label: "synth.param.drive",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.35,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "tone",
        label: "synth.param.tone",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.55,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level",
        label: "synth.param.level",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 0.8,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const COMPRESSOR_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "threshold",
        label: "synth.param.threshold",
        unit: SynthUnit::Db,
        min: -60.0,
        max: 0.0,
        step: 0.5,
        default: -18.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "ratio",
        label: "synth.param.ratio",
        unit: SynthUnit::Ratio,
        min: 1.0,
        max: 20.0,
        step: 0.1,
        default: 4.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "attack",
        label: "synth.param.attack",
        unit: SynthUnit::Ms,
        min: 0.1,
        max: 200.0,
        step: 0.1,
        default: 10.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "release",
        label: "synth.param.release",
        unit: SynthUnit::Ms,
        min: 5.0,
        max: 2000.0,
        step: 1.0,
        default: 150.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "makeup",
        label: "synth.param.makeup",
        unit: SynthUnit::Db,
        min: 0.0,
        max: 24.0,
        step: 0.1,
        default: 0.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const EQ3_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "low",
        label: "synth.param.low",
        unit: SynthUnit::Db,
        min: -18.0,
        max: 18.0,
        step: 0.1,
        default: 0.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mid",
        label: "synth.param.mid",
        unit: SynthUnit::Db,
        min: -18.0,
        max: 18.0,
        step: 0.1,
        default: 0.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "high",
        label: "synth.param.high",
        unit: SynthUnit::Db,
        min: -18.0,
        max: 18.0,
        step: 0.1,
        default: 0.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "low_freq",
        label: "synth.param.low_freq",
        unit: SynthUnit::Hz,
        min: 20.0,
        max: 1000.0,
        step: 1.0,
        default: 250.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "high_freq",
        label: "synth.param.high_freq",
        unit: SynthUnit::Hz,
        min: 500.0,
        max: 20000.0,
        step: 10.0,
        default: 4000.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const DELAY_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "time",
        label: "synth.param.time",
        unit: SynthUnit::Seconds,
        min: 0.01,
        max: 1.2,
        step: 0.001,
        default: 0.25,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "feedback",
        label: "synth.param.feedback",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.95,
        step: 0.01,
        default: 0.35,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "tone",
        label: "synth.param.tone",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.55,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.3,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const REVERB_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "size",
        label: "synth.param.size",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.5,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "damp",
        label: "synth.param.damp",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.95,
        step: 0.01,
        default: 0.4,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "predelay",
        label: "synth.param.predelay",
        unit: SynthUnit::Seconds,
        min: 0.0,
        max: 0.2,
        step: 0.001,
        default: 0.02,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "width",
        label: "synth.param.width",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.8,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.25,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const CHORUS_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "rate",
        label: "synth.param.rate",
        unit: SynthUnit::Hz,
        min: 0.05,
        max: 8.0,
        step: 0.01,
        default: 0.6,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "depth",
        label: "synth.param.depth",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.35,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "feedback",
        label: "synth.param.feedback",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.9,
        step: 0.01,
        default: 0.1,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.35,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const DISTORTION_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "drive",
        label: "synth.param.drive",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.4,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "tone",
        label: "synth.param.tone",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.55,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level",
        label: "synth.param.level",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 0.7,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const PHASER_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "rate",
        label: "synth.param.rate",
        unit: SynthUnit::Hz,
        min: 0.05,
        max: 8.0,
        step: 0.01,
        default: 0.5,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "depth",
        label: "synth.param.depth",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.7,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "feedback",
        label: "synth.param.feedback",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.85,
        step: 0.01,
        default: 0.35,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.5,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const FLANGER_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "rate",
        label: "synth.param.rate",
        unit: SynthUnit::Hz,
        min: 0.05,
        max: 8.0,
        step: 0.01,
        default: 0.25,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "depth",
        label: "synth.param.depth",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.7,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "feedback",
        label: "synth.param.feedback",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.85,
        step: 0.01,
        default: 0.5,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.5,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const FILTER_FX_PARAMS: &[FxParamSpec] = &[
    FxParamSpec {
        key: "cutoff",
        label: "synth.param.cutoff",
        unit: SynthUnit::Hz,
        min: 20.0,
        max: 20000.0,
        step: 1.0,
        default: 2000.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "resonance",
        label: "synth.param.resonance",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 0.95,
        step: 0.01,
        default: 0.2,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    fx_choice("mode", "synth.param.mode", 0.0, FX_FILTER_MODES),
    FxParamSpec {
        key: "mix",
        label: "synth.param.mix",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: SLIDER,
        options: &[],
    },
    FxParamSpec {
        key: "level_out",
        label: "synth.param.level_out",
        unit: SynthUnit::Fraction,
        min: 0.0,
        max: 2.0,
        step: 0.01,
        default: 1.0,
        kind: CONT,
        control: NONE,
        options: &[],
    },
];

const FX_TYPES: &[FxTypeSpec] = &[
    fx_type("drive", "Drive", 0, WORK, DRIVE_PARAMS),
    fx_type("compressor", "Compressor", 1, WORK, COMPRESSOR_PARAMS),
    fx_type("eq3", "EQ3", 2, WORK, EQ3_PARAMS),
    fx_type("delay", "Delay", 3, WORK, DELAY_PARAMS),
    fx_type("reverb", "Reverb", 4, WORK, REVERB_PARAMS),
    fx_type("chorus", "Chorus", 5, WORK, CHORUS_PARAMS),
    fx_type("distortion", "Distortion", 6, WORK, DISTORTION_PARAMS),
    fx_type("phaser", "Phaser", 7, WORK, PHASER_PARAMS),
    fx_type("flanger", "Flanger", 8, WORK, FLANGER_PARAMS),
    fx_type("filter", "Filter", 9, WORK, FILTER_FX_PARAMS),
    FxTypeSpec {
        id: "utils",
        label: "Utils",
        index: 10,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "split",
        label: "Split",
        index: 11,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "split3",
        label: "Split 3",
        index: 12,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "split_ms",
        label: "Split MS",
        index: 13,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "bode",
        label: "Bode",
        index: 14,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "parallel",
        label: "Parallel Rack",
        index: 15,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "hyperd",
        label: "Hyper D",
        index: 16,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "conv",
        label: "Conv",
        index: 17,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
    FxTypeSpec {
        id: "multiband",
        label: "Multiband",
        index: 18,
        status: PART,
        note: Some("Rack-ready; no dedicated editor controls yet."),
        params: &[],
    },
];

static SYNTH_CATALOG: SynthCatalog = SynthCatalog {
    primary: &[
        "osc.a.waveform",
        "osc.a.level",
        "osc.a.warp_mode",
        "osc.b.waveform",
        "osc.b.level",
        "osc.c.waveform",
        "osc.c.level",
        "filter.cutoff",
        "filter.resonance",
        "filter.mode",
        "filter.env.amount",
        "amp.attack",
        "amp.decay",
        "amp.sustain",
        "amp.release",
        "mod.1.source",
        "mod.1.destination",
        "mod.1.amount",
        "output.gain",
        "voice.glide",
    ],
    version: SYNTH_CATALOG_VERSION,
    units: crate::units::DEFINITIONS,
    plugin_id: SYNTH_PLUGIN_ID,
    groups: GROUPS,
    params: PARAMS,
    families: FAMILIES,
    mod_sources: MOD_SOURCES,
    mod_destinations: MOD_DESTINATIONS,
    mod_aux: MOD_AUX,
    filter_models: FILTER_MODELS,
    filter_modes: FILTER_MODES,
    filter_routings: FILTER_ROUTINGS,
    warp_modes: WARP_MODES,
    waveforms: WAVEFORMS,
    sub_shapes: SUB_SHAPES,
    lfo_shapes: LFO_SHAPES,
    quality_modes: QUALITY_MODES,
    fx_param_kinds: FX_PARAM_KINDS,
    fx_types: FX_TYPES,
    mod_learn_targets: MOD_LEARN_TARGETS,
    unmapped_mod_destinations: UNMAPPED_MOD_DESTINATIONS,
};

/// The one parameter catalog; callers borrow it, tests pin its shape.
pub fn synth_catalog() -> &'static SynthCatalog {
    &SYNTH_CATALOG
}

/// Direct key lookup, including aliases.
pub fn find_param(key: &str) -> Option<&'static SynthParam> {
    SYNTH_CATALOG
        .params
        .iter()
        .find(|param| param.ids.contains(&key) || param.aliases.contains(&key))
}

/// Family lookup for indexed keys such as `mod.3.amount`.
pub fn find_family(key: &str) -> Option<&'static SynthFamily> {
    SYNTH_CATALOG
        .families
        .iter()
        .find(|family| family_matches(family, key))
}

/// Field description for an indexed key, when the family declares it.
pub fn find_family_field(key: &str) -> Option<(&'static SynthFamily, &'static FamilyField)> {
    let family = find_family(key)?;
    let rest = key.strip_prefix(family.prefix)?;
    let suffix = if family.fields.len() == 1 && family.fields[0].suffix.is_empty() {
        ""
    } else {
        rest.split_once('.').map(|(_, suffix)| suffix)?
    };
    family
        .fields
        .iter()
        .find(|field| field.suffix == suffix)
        .map(|field| (family, field))
}

/// Numeric bounds for a direct key, alias or indexed family field. Lookup is
/// allocation-free and leaves the supplied key unchanged.
pub fn numeric_domain(key: &str) -> Option<SynthNumericDomain> {
    let (min, max, default, kind) = find_param(key)
        .map(|param| (param.min, param.max, param.default, param.kind))
        .or_else(|| {
            find_family_field(key)
                .map(|(_, field)| (field.min, field.max, field.default, field.kind))
        })?;
    if !matches!(
        kind,
        SynthValueKind::Continuous | SynthValueKind::Enum | SynthValueKind::Bool
    ) {
        return None;
    }
    let domain = SynthNumericDomain { min, max, default };
    domain.is_valid().then_some(domain)
}

/// Borrow the physical owner and its explicit DSP law; no key rewriting,
/// allocation or runtime initialization is needed for automation updates.
pub fn dsp_normalization(key: &str) -> Option<SynthDspNormalization> {
    let law = find_param(key)
        .and_then(|param| param.dsp_normalization)
        .or_else(|| find_family_field(key).and_then(|(_, field)| field.dsp_normalization))?;
    let floor = find_param(key)
        .and_then(|param| param.dsp_normalization_floor)
        .or_else(|| find_family_field(key).and_then(|(_, field)| field.dsp_normalization_floor));
    let span = find_param(key)
        .and_then(|param| param.dsp_normalization_span)
        .or_else(|| find_family_field(key).and_then(|(_, field)| field.dsp_normalization_span));
    let mapping = SynthDspNormalization {
        domain: numeric_domain(key)?,
        law,
        floor,
        span,
    };
    mapping.is_valid().then_some(mapping)
}

/// Whether the catalog describes `key`, either directly or through a family.
pub fn is_known_param(key: &str) -> bool {
    find_param(key).is_some() || find_family(key).is_some()
}

/// Looks up an enumeration by the id a parameter's `enum_id` names.
pub fn find_enum(id: &str) -> Option<&'static SynthEnum> {
    let catalog = synth_catalog();
    [
        &catalog.mod_sources,
        &catalog.mod_destinations,
        &catalog.mod_aux,
        &catalog.filter_models,
        &catalog.filter_modes,
        &catalog.filter_routings,
        &catalog.warp_modes,
        &catalog.waveforms,
        &catalog.sub_shapes,
        &catalog.lfo_shapes,
        &catalog.quality_modes,
        &catalog.fx_param_kinds,
    ]
    .into_iter()
    .find(|spec| spec.id == id)
}

/// Numeric destination a learn should target for `control`, if it has one.
pub fn learn_destination(control: &str) -> Option<usize> {
    SYNTH_CATALOG
        .mod_learn_targets
        .iter()
        .find(|target| target.control == control)
        .map(|target| target.destination)
}

fn family_matches(family: &SynthFamily, key: &str) -> bool {
    let Some(rest) = key.strip_prefix(family.prefix) else {
        return false;
    };
    if family.fields.len() == 1 && family.fields[0].suffix.is_empty() {
        return rest
            .parse::<usize>()
            .is_ok_and(|index| (family.first..=family.last).contains(&index));
    }
    let Some((index, suffix)) = rest.split_once('.') else {
        return false;
    };
    let Ok(index) = index.parse::<usize>() else {
        return false;
    };
    if !(family.first..=family.last).contains(&index) {
        return false;
    }
    family.open_fields || family.fields.iter().any(|field| field.suffix == suffix)
}

/// Every concrete key the catalog names. Open FX fields are not enumerated.
pub fn parameter_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for param in SYNTH_CATALOG.params {
        keys.extend(param.ids.iter().map(|key| (*key).to_string()));
        keys.extend(param.aliases.iter().map(|key| (*key).to_string()));
    }
    for family in SYNTH_CATALOG.families {
        for index in family.first..=family.last {
            if family.fields.len() == 1 && family.fields[0].suffix.is_empty() {
                keys.push(format!("{}{index}", family.prefix));
            } else {
                for field in family.fields {
                    keys.push(format!("{}{index}.{}", family.prefix, field.suffix));
                }
            }
        }
    }
    keys
}

const TS_HEADER: &str = r#"// @generated by `cargo run -p oc-device-schema --bin gen-synth-catalog`.
// Source of truth: crates/oc-device-schema/src/synth.rs. Do not edit by hand.

import type { UnitId, UnitDefinition } from "./units.gen";
export type SynthUnit = UnitId;
export type SynthCurve = "linear" | "log" | "time";
export type SynthValueKind = "continuous" | "bool" | "enum" | "string" | "array" | "metadata";
export type SynthControl = "slider" | "select" | "toggle" | "none";
export type SupportStatus = "working" | "partial" | "noOp" | "unmapped";

export type SynthDspLaw = 'identity' | 'linear' | 'square' | 'cubic' | 'log10' | 'log';

export interface SynthSampleRateBounds { min: number; maxSampleRateRatio: number; }

export interface SynthParamSpec {
  ids: string[];
  label: string;
  group: string;
  unit: SynthUnit;
  min: number;
  max: number;
  step: number;
  default: number;
  curve: SynthCurve;
  dspNormalization?: SynthDspLaw;
  dspNormalizationFloor?: number;
  dspNormalizationSpan?: number;
  dspRuntimeBounds?: SynthSampleRateBounds;
  kind: SynthValueKind;
  control: SynthControl;
  status: SupportStatus;
  enumId?: string;
  note?: string;
  aliases: string[];
}

export interface FamilyField {
  suffix: string;
  label: string;
  unit: SynthUnit;
  min: number;
  max: number;
  step: number;
  default: number;
  curve: SynthCurve;
  dspNormalization?: SynthDspLaw;
  dspNormalizationFloor?: number;
  dspNormalizationSpan?: number;
  dspRuntimeBounds?: SynthSampleRateBounds;
  kind: SynthValueKind;
  control: SynthControl;
  status: SupportStatus;
  enumId?: string;
}

export interface SynthFamily {
  id: string;
  group: string;
  prefix: string;
  first: number;
  last: number;
  fields: FamilyField[];
  openFields: boolean;
}

export interface SynthEnumOption {
  id: number;
  label: string;
  status: SupportStatus;
  note?: string;
}

export interface SynthEnum {
  id: string;
  label: string;
  options: SynthEnumOption[];
}

export interface ModLearnTarget {
  control: string;
  destination: number;
}

export interface FxParamSpec {
  key: string;
  label: string;
  unit: SynthUnit;
  min: number;
  max: number;
  step: number;
  default: number;
  kind: SynthValueKind;
  control: SynthControl;
  options: SynthEnumOption[];
}

export interface FxTypeSpec {
  id: string;
  label: string;
  index: number;
  status: SupportStatus;
  note?: string;
  params: FxParamSpec[];
}

export interface SynthGroup {
  id: string;
  label: string;
}

export interface SynthCatalog {
  version: number;
  units: UnitDefinition[];
  pluginId: string;
  groups: SynthGroup[];
  params: SynthParamSpec[];
  /** The controls that carry the sound; read these before paging the rest. */
  primary: string[];
  families: SynthFamily[];
  modSources: SynthEnum;
  modDestinations: SynthEnum;
  modAux: SynthEnum;
  filterModels: SynthEnum;
  filterModes: SynthEnum;
  filterRoutings: SynthEnum;
  warpModes: SynthEnum;
  waveforms: SynthEnum;
  subShapes: SynthEnum;
  lfoShapes: SynthEnum;
  qualityModes: SynthEnum;
  fxParamKinds: SynthEnum;
  fxTypes: FxTypeSpec[];
  modLearnTargets: ModLearnTarget[];
  unmappedModDestinations: SynthEnumOption[];
}

export const SYNTH_CATALOG: SynthCatalog = "#;

/// Render the checked-in TypeScript module. `gen-synth-catalog` writes it and
/// the freshness test compares it, so the frontend never hand-maintains a list.
pub fn render_typescript() -> String {
    let json = serde_json::to_string_pretty(&SYNTH_CATALOG)
        .expect("the synth catalog is static serializable data");
    format!("{TS_HEADER}{json};\n")
}

#[cfg(test)]
mod primary_tests {
    use super::*;

    #[test]
    fn every_primary_key_resolves() {
        for key in SYNTH_CATALOG.primary {
            assert!(
                find_param(key).is_some() || find_family(key).is_some(),
                "primary key '{key}' is not a parameter or a family"
            );
        }
    }

    #[test]
    fn the_primary_set_stays_small() {
        assert!(SYNTH_CATALOG.primary.len() <= 24);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn oscillator_fraction_schema_all_twenty_keys_use_existing_identity_authority() {
        use super::*;
        let mut count = 0;
        for oscillator in ['a', 'b', 'c'] {
            for field in [
                "wavetable_position",
                "warp_amount",
                "warp_var",
                "warp2_amount",
                "warp2_var",
                "width",
            ] {
                let key = format!("osc.{oscillator}.{field}");
                let owner = dsp_normalization(&key).unwrap();
                assert_eq!(owner.law, SynthDspLaw::Identity, "{key}");
                assert_eq!(
                    (owner.domain.min, owner.domain.max),
                    (UNIT_FRACTION.min, UNIT_FRACTION.max)
                );
                assert_eq!(
                    owner.domain.default,
                    if key == "osc.a.width" {
                        0.55
                    } else {
                        UNIT_FRACTION.default
                    }
                );
                let encoded = serde_json::to_value(find_param(&key).unwrap()).unwrap();
                assert_eq!(encoded["dspNormalization"], "identity");
                count += 1;
            }
        }
        for key in ["unison.width", "stereo_width"] {
            assert_eq!(dsp_normalization(key), dsp_normalization("osc.a.width"));
            count += 1;
        }
        assert_eq!(count, 20);
        assert_eq!(AMPLITUDE_FRACTION, UNIT_FRACTION);
        assert_eq!(SYNTH_CATALOG_VERSION, 4);
    }

    #[test]
    fn signed_modulation_domains_are_shared_and_keep_fraction_and_octave_units_distinct() {
        use super::*;
        for key in [
            "amp.curve",
            "filter_env.curve",
            "env3.curve",
            "env4.curve",
            "mod.1.amount",
            "mod.64.curve",
            "fxmod.64.amount",
        ] {
            let owner = dsp_normalization(key).unwrap();
            assert_eq!(owner.domain, BIPOLAR_MODULATION, "{key}");
            assert_eq!(owner.law, SynthDspLaw::Linear, "{key}");
            assert_eq!(owner.denormalize(0.5), Some(0.0), "{key}");
        }
        let octaves = dsp_normalization("filter.env.amount").unwrap();
        assert_eq!(octaves.domain, FILTER_ENVELOPE_DEPTH);
        assert_eq!(octaves.normalization_domain(), Some((-8.0, 8.0)));
        assert_eq!(octaves.denormalize(0.0), Some(-8.0));
        assert_eq!(octaves.denormalize(1.0), Some(8.0));
        assert!(dsp_normalization("lfo.pitch").is_none());
        assert!(dsp_normalization("lfo.curve.255").is_none());
    }

    #[test]
    fn lfo_step_affine_controls_declare_existing_domains_and_signed_zero_laws() {
        use super::*;
        for number in 1..=10 {
            let prefix = if number == 1 {
                "lfo".to_string()
            } else {
                format!("lfo{number}")
            };
            for (field, min, max) in [
                ("phase", 0.0, 360.0),
                ("bend", -1.0, 1.0),
                ("smooth", 0.0, 1.0),
            ] {
                let key = format!("{prefix}.{field}");
                let owner = dsp_normalization(&key).unwrap();
                assert_eq!(owner.law, SynthDspLaw::Linear, "{key}");
                assert_eq!(owner.normalization_domain(), Some((min, max)), "{key}");
                assert_eq!(owner.domain.default, 0.0, "{key}");
                assert_eq!(owner.span, None, "{key}");
            }
        }
        for slot in 1..=16 {
            let owner = dsp_normalization(&format!("step.{slot}")).unwrap();
            assert_eq!(owner.normalization_domain(), Some((-1.0, 1.0)));
            assert_eq!(owner.denormalize(0.5), Some(0.0));
        }
        for key in ["lfo.phase", "lfo.smooth", "step.smoothing"] {
            let owner = dsp_normalization(key).unwrap();
            assert_eq!(
                owner.denormalize(-0.0).unwrap().to_bits(),
                (-0.0_f64).to_bits(),
                "{key}"
            );
        }
        assert!(dsp_normalization("lfo.pitch").is_none());
        assert!(dsp_normalization("sub.pan").is_none());
    }

    #[test]
    fn oscillator_affine_span_preserves_pulse_arithmetic_without_changing_bounds() {
        use super::*;
        let pulse = dsp_normalization("osc.a.pulse_width").unwrap();
        assert_eq!(pulse.law, SynthDspLaw::Linear);
        assert_eq!(pulse.span, Some(0.9));
        assert_eq!(pulse.domain.min, 0.05);
        assert_eq!(pulse.domain.max, 0.95);
        assert_eq!(pulse.normalization_domain(), Some((0.05, 0.95)));
        assert_ne!(pulse.domain.max - pulse.domain.min, 0.9);
        for value in [0.0_f64, 0.125, 0.5, 0.875, 1.0] {
            assert_eq!(
                pulse.denormalize(value).unwrap().to_bits(),
                (0.05 + value * 0.9).to_bits()
            );
        }
        assert_eq!(pulse.denormalize(0.5), Some(0.5));
        assert_eq!(pulse.normalize(0.95), Some((0.95 - 0.05) / 0.9));
        let encoded = serde_json::to_value(find_param("osc.a.pulse_width").unwrap()).unwrap();
        assert_eq!(encoded["dspNormalization"], "linear");
        assert_eq!(encoded["dspNormalizationSpan"], 0.9);
        assert!(
            serde_json::to_value(find_param("osc.a.pan").unwrap())
                .unwrap()
                .get("dspNormalizationSpan")
                .is_none()
        );
        for span in [0.0, -1.0, 0.8, 1.0, f64::NAN, f64::INFINITY] {
            let invalid = SynthDspNormalization {
                span: Some(span),
                ..pulse
            };
            assert!(!invalid.is_valid());
            assert_eq!(invalid.denormalize(0.5), None);
            assert_eq!(invalid.normalize(0.5), None);
        }
        let logarithmic = dsp_normalization("lfo.rate").unwrap();
        assert!(
            !SynthDspNormalization {
                span: Some(0.9),
                ..logarithmic
            }
            .is_valid()
        );
        // These need an explicit migration before full-domain automation.
        assert!(dsp_normalization("osc.a.semitones").is_none());
        assert!(dsp_normalization("sub.pan").is_none());
    }

    #[test]
    fn log_metadata_declares_positive_automation_floors_and_runtime_context() {
        use super::*;
        let lfo = dsp_normalization("lfo.rate").unwrap();
        assert_eq!(lfo.domain, LFO_RATE_HZ);
        assert_eq!(lfo.domain.min, 0.0);
        assert_eq!(
            lfo.normalization_domain(),
            Some((LFO_AUTOMATION_FLOOR_HZ, 100.0))
        );
        assert_eq!(lfo.denormalize(0.0), Some(0.01));
        assert_eq!(lfo.normalize(0.0), None);
        assert_eq!(lfo.normalize(-1.0), None);
        assert_eq!(
            lfo.normalize(f64::MAX),
            None,
            "legacy division overflow is not a representable inverse"
        );
        assert_eq!(lfo.normalize(0.001), Some(0.0));
        let encoded = serde_json::to_value(find_param("lfo.rate").unwrap()).unwrap();
        assert_eq!(encoded["dspNormalization"], "log10");
        assert_eq!(encoded["dspNormalizationFloor"], 0.01);
        assert_eq!(encoded["min"], 0.0);
        let cutoff = dsp_normalization("filter.cutoff").unwrap();
        assert_eq!(cutoff.normalization_domain(), Some((20.0, 20_000.0)));
        assert_eq!(
            find_param("filter.cutoff").unwrap().dsp_runtime_bounds,
            Some(FILTER_RUNTIME_BOUNDS)
        );
        let power = serde_json::to_value(find_param("amp.attack").unwrap()).unwrap();
        assert_eq!(power["dspNormalization"], "cubic");
        assert!(power.get("dspNormalizationFloor").is_none());
        for floor in [0.0, -1.0, f64::NAN, 100.0, 101.0] {
            let invalid = SynthDspNormalization {
                floor: Some(floor),
                ..lfo
            };
            assert!(!invalid.is_valid());
            assert_eq!(invalid.normalization_domain(), None);
            assert_eq!(invalid.denormalize(0.5), None);
        }
        for rate in [8_000.0, 44_100.0, 48_000.0] {
            for value in [-100.0_f64, 0.0, 20.0, 20_000.0, 100_000.0] {
                assert_eq!(
                    FILTER_RUNTIME_BOUNDS.clamp(value, rate),
                    value.clamp(8.0, rate * 0.45)
                );
            }
        }
        // Stored/automated cutoff never acquires a device-rate-dependent range.
        assert_eq!(cutoff.domain.max, 20_000.0);
        assert_eq!(cutoff.denormalize(1.0), Some(20_000.0));
    }

    #[test]
    fn dsp_time_metadata_is_explicit_valid_and_distinct_from_display_curves() {
        let mut count = 0;
        for key in super::parameter_keys() {
            let Some(mapping) = super::dsp_normalization(&key) else {
                continue;
            };
            if !matches!(
                mapping.law,
                super::SynthDspLaw::Square | super::SynthDspLaw::Cubic
            ) {
                continue;
            }
            count += 1;
            assert!(mapping.is_valid(), "{key}");
            assert_eq!(mapping.domain.min, 0.0, "instant time remains valid: {key}");
            assert_eq!(mapping.denormalize(0.0), Some(0.0), "{key}");
            assert_eq!(mapping.denormalize(1.0), Some(mapping.domain.max), "{key}");
            assert_eq!(mapping.normalize(f64::NAN), None, "{key}");
            assert_eq!(mapping.denormalize(f64::INFINITY), None, "{key}");
            assert_eq!(
                mapping.normalize(-1.0),
                match mapping.law {
                    super::SynthDspLaw::Square => None,
                    _ => Some(0.0),
                },
                "{key}"
            );
        }
        assert!(
            count >= 161,
            "all power-time controls and route slots: {count}"
        );
        let attack = super::find_param("amp.attack").unwrap();
        assert_eq!(attack.curve, super::SynthCurve::Time);
        assert_eq!(attack.dsp_normalization, Some(super::SynthDspLaw::Cubic));
        let rise = super::find_param("lfo.rise").unwrap();
        assert_eq!(rise.curve, super::SynthCurve::Time);
        assert_eq!(rise.dsp_normalization, Some(super::SynthDspLaw::Square));
        assert_eq!(
            super::dsp_normalization("lfo.rate").unwrap().law,
            super::SynthDspLaw::Log10
        );
        let invalid = super::SynthDspNormalization {
            domain: super::TUNING_REFERENCE_HZ,
            law: super::SynthDspLaw::Square,
            floor: None,
            span: None,
        };
        assert!(!invalid.is_valid());
        assert_eq!(invalid.denormalize(0.5), None);
    }

    #[test]
    fn every_numeric_catalog_domain_is_valid_and_saturates_finite_values() {
        for key in super::parameter_keys() {
            let kind = super::find_param(&key)
                .map(|p| p.kind)
                .or_else(|| super::find_family_field(&key).map(|(_, f)| f.kind))
                .unwrap();
            let domain = super::numeric_domain(&key);
            if matches!(
                kind,
                super::SynthValueKind::Continuous
                    | super::SynthValueKind::Enum
                    | super::SynthValueKind::Bool
            ) {
                let domain = domain.unwrap_or_else(|| panic!("invalid numeric domain: {key}"));
                assert_eq!(domain.denormalize_linear(0.0), Some(domain.min), "{key}");
                assert_eq!(domain.denormalize_linear(1.0), Some(domain.max), "{key}");
                assert_eq!(domain.clamp(f64::MIN), Some(domain.min), "{key}");
                assert_eq!(domain.clamp(f64::MAX), Some(domain.max), "{key}");
                assert_eq!(domain.clamp(f64::NAN), None, "{key}");
                let position = domain.normalize_linear(domain.default).unwrap();
                let round = domain.denormalize_linear(position).unwrap();
                assert!(
                    (round - domain.default).abs() <= 1e-10 * domain.default.abs().max(1.0),
                    "{key}"
                );
            } else {
                assert!(domain.is_none(), "nonnumeric domain: {key}");
            }
        }
    }

    #[test]
    fn malformed_numeric_domains_reject_conversions_without_panicking() {
        use super::SynthNumericDomain as Domain;
        for domain in [
            Domain {
                min: 1.0,
                max: 0.0,
                default: 0.0,
            },
            Domain {
                min: 0.0,
                max: 1.0,
                default: 2.0,
            },
            Domain {
                min: f64::NAN,
                max: 1.0,
                default: 0.0,
            },
            Domain {
                min: 0.0,
                max: f64::INFINITY,
                default: 0.0,
            },
            Domain {
                min: -f64::MAX,
                max: f64::MAX,
                default: 0.0,
            },
        ] {
            assert!(!domain.is_valid());
            assert_eq!(domain.clamp(0.5), None);
            assert_eq!(domain.normalize_linear(0.5), None);
            assert_eq!(domain.denormalize_linear(0.5), None);
        }
        assert!(super::numeric_domain("not.a.parameter").is_none());
    }

    use super::*;

    fn all_enums(catalog: &SynthCatalog) -> Vec<&SynthEnum> {
        vec![
            &catalog.mod_sources,
            &catalog.mod_destinations,
            &catalog.mod_aux,
            &catalog.filter_models,
            &catalog.filter_modes,
            &catalog.filter_routings,
            &catalog.warp_modes,
            &catalog.waveforms,
            &catalog.sub_shapes,
            &catalog.lfo_shapes,
            &catalog.quality_modes,
            &catalog.fx_param_kinds,
        ]
    }

    #[test]
    fn identity_amplitude_law_preserves_bits_and_rejects_unreachable_inverses() {
        use super::*;
        for key in [
            "osc.a.level",
            "osc.b.level",
            "osc.c.level",
            "ring.level",
            "sub.level",
            "noise.level",
            "output.gain",
            "gain",
            "amp.sustain",
            "sustain",
            "filter_env.sustain",
            "env3.sustain",
            "env4.sustain",
            "amp.damping",
            "velocity.to_amp",
        ] {
            let owner = dsp_normalization(key).unwrap();
            assert_eq!(owner.law, SynthDspLaw::Identity, "{key}");
            assert_eq!(owner.normalization_domain(), Some((0.0, 1.0)));
            for value in [-0.0_f64, 0.0, f64::MIN_POSITIVE, 0.22, 0.72, 1.0] {
                assert_eq!(
                    owner.denormalize(value).unwrap().to_bits(),
                    value.to_bits(),
                    "forward {key}"
                );
                assert_eq!(
                    owner.normalize(value).unwrap().to_bits(),
                    value.to_bits(),
                    "inverse {key}"
                );
            }
            for value in [
                f64::MIN,
                -f64::MIN_POSITIVE,
                1.0000000000000002,
                f64::MAX,
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ] {
                assert_eq!(owner.normalize(value), None, "{key}");
            }
            assert_eq!(owner.denormalize(-1.0), Some(0.0));
            assert_eq!(owner.denormalize(2.0), Some(1.0));
            assert_eq!(owner.denormalize(f64::NAN), None);
            assert_eq!(owner.denormalize(f64::INFINITY), None);
        }
        let owner = dsp_normalization("output.gain").unwrap();
        for invalid in [
            SynthDspNormalization {
                floor: Some(0.01),
                ..owner
            },
            SynthDspNormalization {
                span: Some(1.0),
                ..owner
            },
            SynthDspNormalization {
                domain: BIPOLAR_MODULATION,
                ..owner
            },
        ] {
            assert!(!invalid.is_valid());
            assert_eq!(invalid.denormalize(0.5), None);
            assert_eq!(invalid.normalize(0.5), None);
        }
        let encoded = serde_json::to_value(find_param("output.gain").unwrap()).unwrap();
        assert_eq!(encoded["dspNormalization"], "identity");
        assert_eq!(SYNTH_CATALOG_VERSION, 4);
        let typescript = render_typescript();
        assert!(typescript.contains("export type SynthDspLaw = 'identity' | 'linear' | 'square' | 'cubic' | 'log10' | 'log';"));
        // Existing law tags and descriptor fields retain their wire spelling.
        assert_eq!(serde_json::to_value(SynthDspLaw::Linear).unwrap(), "linear");
        assert_eq!(serde_json::to_value(SynthDspLaw::Cubic).unwrap(), "cubic");
        assert_eq!(serde_json::to_value(SynthDspLaw::Log10).unwrap(), "log10");
    }

    #[test]
    fn catalog_shape_is_pinned() {
        let catalog = synth_catalog();
        assert_eq!(catalog.version, 4);
        assert_eq!(catalog.params.len(), 123);
        for (key, default) in [
            ("osc.a.waveform", 1.0),
            ("osc.b.waveform", 2.0),
            ("osc.c.waveform", 0.0),
            ("osc.a.level", 0.78),
            ("osc.b.level", 0.0),
            ("osc.c.level", 0.0),
        ] {
            let param = find_param(key).unwrap();
            assert_eq!(param.ids, &[key]);
            assert_eq!(param.default, default, "{key}");
        }
        assert_eq!(parameter_keys().len(), 1517);
        assert_eq!(catalog.families.len(), 7);
        assert_eq!(catalog.groups.len(), 12);
        assert_eq!(catalog.mod_sources.options.len(), 29);
        assert_eq!(catalog.mod_destinations.options.len(), 35);
        assert_eq!(catalog.mod_aux.options.len(), 30);
        assert_eq!(catalog.mod_learn_targets.len(), 20);
        assert_eq!(catalog.unmapped_mod_destinations.len(), 4);
        assert_eq!(catalog.filter_models.options.len(), 12);
        assert_eq!(catalog.filter_modes.options.len(), 4);
        assert_eq!(catalog.warp_modes.options.len(), 70);
        assert_eq!(catalog.waveforms.options.len(), 7);
        assert_eq!(catalog.sub_shapes.options.len(), 6);
        assert_eq!(catalog.lfo_shapes.options.len(), 5);
        assert_eq!(catalog.fx_types.len(), 19);
        assert_eq!(
            catalog
                .warp_modes
                .options
                .iter()
                .filter(|option| option.status == SupportStatus::NoOp)
                .count(),
            35
        );
        assert_eq!(
            catalog
                .warp_modes
                .options
                .iter()
                .filter(|option| option.status == SupportStatus::Working)
                .count(),
            35
        );
        assert_eq!(
            catalog
                .fx_types
                .iter()
                .filter(|fx| fx.status == SupportStatus::Working)
                .count(),
            10
        );
    }

    #[test]
    fn parameter_keys_are_unique() {
        let keys = parameter_keys();
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(keys.len(), unique.len(), "duplicate catalog keys");
    }

    #[test]
    fn enum_option_ids_are_contiguous() {
        for spec in all_enums(synth_catalog()) {
            for (index, option) in spec.options.iter().enumerate() {
                assert_eq!(option.id, index, "{} option order", spec.id);
            }
        }
        for fx in synth_catalog().fx_types {
            for param in fx.params {
                for (index, option) in param.options.iter().enumerate() {
                    assert_eq!(option.id, index, "{}.{} option order", fx.id, param.key);
                }
            }
        }
    }

    #[test]
    fn non_working_entries_explain_themselves() {
        let catalog = synth_catalog();
        for param in catalog.params {
            if param.status != SupportStatus::Working {
                assert!(
                    param.note.is_some(),
                    "{} needs a note for {:?}",
                    param.canonical(),
                    param.status
                );
            }
        }
        for spec in all_enums(catalog) {
            for option in spec.options {
                if option.status != SupportStatus::Working {
                    assert!(
                        option.note.is_some(),
                        "{} {} needs a note for {:?}",
                        spec.id,
                        option.label,
                        option.status
                    );
                }
            }
        }
    }

    #[test]
    fn select_params_name_a_matching_enum() {
        let catalog = synth_catalog();
        for param in catalog.params {
            if param.control != SynthControl::Select {
                continue;
            }
            // Discrete numeric selects (unison voices, polyphony) have no enum.
            let Some(enum_id) = param.enum_id else {
                assert_eq!(param.step, 1.0, "{}", param.canonical());
                continue;
            };
            assert_eq!(param.kind, SynthValueKind::Enum, "{}", param.canonical());
            let spec = find_enum(enum_id).expect("enum id resolves");
            assert_eq!(param.min, 0.0, "{}", param.canonical());
            assert_eq!(
                param.max,
                (spec.options.len() - 1) as f64,
                "{}",
                param.canonical()
            );
            assert_eq!(param.step, 1.0, "{}", param.canonical());
        }
        for family in catalog.families {
            for field in family.fields {
                if field.control != SynthControl::Select || field.enum_id.is_none() {
                    continue;
                }
                let spec = find_enum(field.enum_id.unwrap()).expect("enum id resolves");
                assert_eq!(
                    field.max,
                    (spec.options.len() - 1) as f64,
                    "{}.{}",
                    family.id,
                    field.suffix
                );
            }
        }
    }

    #[test]
    fn learn_targets_name_real_controls_and_destinations() {
        let catalog = synth_catalog();
        let destinations: std::collections::BTreeSet<_> = catalog
            .mod_destinations
            .options
            .iter()
            .map(|option| option.id)
            .collect();
        let mut controls = std::collections::BTreeSet::new();
        for target in catalog.mod_learn_targets {
            assert!(
                is_known_param(target.control),
                "{} is not a catalog parameter",
                target.control
            );
            assert!(
                controls.insert(target.control),
                "{} is listed twice",
                target.control
            );
            assert!(
                destinations.contains(&target.destination),
                "{} targets unknown destination {}",
                target.control,
                target.destination
            );
        }
        assert_eq!(learn_destination("filter.cutoff"), Some(0));
        assert_eq!(learn_destination("unison.width"), None);
    }

    #[test]
    fn aux_sources_match_the_source_enum() {
        let catalog = synth_catalog();
        assert_eq!(catalog.mod_aux.options[0].label, "None");
        for (source, aux) in catalog
            .mod_sources
            .options
            .iter()
            .zip(&catalog.mod_aux.options[1..])
        {
            assert_eq!(aux.label, source.label);
            assert_eq!(aux.id, source.id + 1);
        }
    }

    #[test]
    fn every_catalog_key_resolves_through_the_catalog() {
        for key in parameter_keys() {
            assert!(is_known_param(&key), "{key} is named but unknown");
        }
        assert!(!is_known_param("osc.zzz"));
        assert!(!is_known_param("mod.0.source"));
        assert!(!is_known_param("mod.65.amount"));
        assert!(!is_known_param("fx.17.any_field"));
    }

    #[test]
    fn generated_typescript_is_current() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../oc-gui/frontend/src/data/synthCatalog.gen.ts"
        );
        let on_disk = std::fs::read_to_string(path).expect("generated synth catalog exists");
        assert_eq!(
            on_disk,
            render_typescript(),
            "run `cargo run -p oc-device-schema --bin gen-synth-catalog`"
        );
    }
}
