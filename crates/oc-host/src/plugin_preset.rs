//! Third-party plugin preset import (`*.ffp`).
//!
//! Two containers exist: binary (`FQ2p`/`FQ3p`/`FPRr`/`FL2p`/`FPMb`/`FPSr`:
//! 4-byte magic, u32 LE version, u32 LE parameter count, then count LE f32
//! plain values by consecutive VST3 id) and ASCII (`FC3p`/`FQ4p`:
//! `[Preset]`/`[Parameters]` with `name=value` lines).
//!
//! The values are *plain*, not normalised: frequencies are log2(Hz), enums are
//! indices, gains are dB, and several controls use the plug-in's own 0..1 curve
//! domain. Conversion to the normalised VST3 domain therefore goes through the
//! plug-in itself ([`Vst3Instance::set_param_plain_by_name`]).
//!
//! Product labels in the signatures identify which hosted plugin's parameter
//! set a file targets. They are compatibility labels, not implementation
//! provenance. Custom files from another plugin version are not covered.

use serde::Serialize;
use std::collections::HashMap;

use crate::vst3_instance::{Vst3Instance, Vst3ParamDetail};

/// `log2(10 Hz)`, the lowest documented EQ/crossover frequency.
const LOG2_10_HZ: f64 = std::f64::consts::LOG2_10;
/// `log2(30 kHz)`, the highest documented EQ/crossover frequency.
const LOG2_30_KHZ: f64 = 14.872_674_880_270_605;

const BINARY_HEADER_LEN: usize = 12;

struct BinarySignature {
    magic: &'static [u8; 4],
    version: u32,
    plugin: &'static str,
    /// True when the documented payload stores frequencies as log2(Hz).
    log2_frequencies: bool,
}

/// Magic and version pairs used by supported preset files.
const BINARY_SIGNATURES: &[BinarySignature] = &[
    BinarySignature {
        magic: b"FQ2p",
        version: 2,
        plugin: "Pro-Q 2",
        log2_frequencies: true,
    },
    BinarySignature {
        magic: b"FQ3p",
        version: 4,
        plugin: "Pro-Q 3",
        log2_frequencies: true,
    },
    BinarySignature {
        magic: b"FPRr",
        version: 4,
        plugin: "Pro-R",
        log2_frequencies: false,
    },
    BinarySignature {
        magic: b"FL2p",
        version: 2,
        plugin: "Pro-L 2",
        log2_frequencies: false,
    },
    BinarySignature {
        magic: b"FPMb",
        version: 3,
        plugin: "Pro-MB",
        log2_frequencies: true,
    },
    BinarySignature {
        magic: b"FPSr",
        version: 2,
        plugin: "Pro-DS",
        log2_frequencies: false,
    },
];

struct TextSignature {
    signature: &'static str,
    version: u32,
    plugin: &'static str,
    log2_frequencies: bool,
}

/// Text signatures used by supported preset files.
const TEXT_SIGNATURES: &[TextSignature] = &[
    TextSignature {
        signature: "FC3p",
        version: 2,
        plugin: "Pro-C 3",
        log2_frequencies: true,
    },
    TextSignature {
        signature: "FQ4p",
        version: 4,
        plugin: "Pro-Q 4",
        log2_frequencies: true,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Plugin preset container format.
pub enum PresetFormat {
    Binary,
    Text,
}

impl PresetFormat {
    /// Canonical format name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Text => "text",
        }
    }
}

/// One parsed plain value, keyed by VST3 id (binary) or title (text).
#[derive(Debug, Clone, PartialEq)]
pub enum PresetValue {
    Indexed { index: u32, plain: f64 },
    Named { name: String, plain: f64 },
}

impl PresetValue {
    /// The stored plain value, regardless of keying.
    pub fn plain(&self) -> f64 {
        match self {
            Self::Indexed { plain, .. } | Self::Named { plain, .. } => *plain,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// A parsed plugin preset with keyed plain values.
pub struct PluginPreset {
    pub format: PresetFormat,
    pub signature: String,
    pub version: u32,
    /// Compatibility label for the signature (for example `Pro-Q 2`).
    pub plugin: String,
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub values: Vec<PresetValue>,
}

impl PluginPreset {
    fn log2_frequencies(&self) -> bool {
        if self.format == PresetFormat::Binary {
            BINARY_SIGNATURES
                .iter()
                .find(|spec| spec.signature_matches(&self.signature))
                .is_some_and(|spec| spec.log2_frequencies)
        } else {
            TEXT_SIGNATURES
                .iter()
                .find(|spec| spec.signature == self.signature)
                .is_some_and(|spec| spec.log2_frequencies)
        }
    }
}

impl BinarySignature {
    fn signature_matches(&self, signature: &str) -> bool {
        self.magic == signature.as_bytes()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
/// Why a plugin preset could not be parsed or mapped.
pub enum PresetError {
    #[error(".ffp is empty")]
    Empty,
    #[error(".ffp header is truncated: {0} bytes")]
    TruncatedHeader(usize),
    #[error(".ffp has unknown 4-byte magic {0:?}")]
    UnknownMagic(String),
    #[error(".ffp version {version} is not documented for signature {signature}")]
    UnsupportedVersion { signature: String, version: u32 },
    #[error(".ffp declares {declared} parameters but the file has {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error(".ffp payload is truncated: {declared} parameters declared, {actual} present")]
    TruncatedPayload { declared: usize, actual: usize },
    #[error(".ffp payload length {0} is not a multiple of 4")]
    UnalignedPayload(usize),
    #[error(".ffp payload is empty")]
    EmptyPayload,
    #[error(".ffp value {index} is not finite: {value}")]
    NonFiniteValue { index: usize, value: f64 },
    #[error(".ffp is not valid UTF-8 text")]
    TextNotUtf8,
    #[error("text .ffp is missing the [{0}] section")]
    MissingSection(&'static str),
    #[error("text .ffp is missing the {0} key")]
    MissingKey(&'static str),
    #[error("text .ffp is missing Signature")]
    MissingSignature,
    #[error("text .ffp line {line} is not `name = value`: {text}")]
    MalformedLine { line: usize, text: String },
    #[error("text .ffp parameter name is empty at line {line}")]
    EmptyName { line: usize },
    #[error("text .ffp value for {name:?} is not a number: {value:?}")]
    NonNumericValue { name: String, value: String },
    #[error("text .ffp Signature {0:?} is not documented")]
    UnknownSignature(String),
    #[error("{label:?} stores a stepped index but value {value} is not an integer")]
    NonIntegralStepped { label: String, value: f64 },
    #[error(
        "{label:?} value {value} is outside the documented log2(Hz) plain range; .ffp stores plain values, not normalised ones"
    )]
    NormalizedValue { label: String, value: f64 },
}

/// Parse either `.ffp` container. The container is detected from the first
/// non-whitespace bytes (`[Preset]`/`[Parameters]` vs. a documented magic).
pub fn parse_preset(bytes: &[u8]) -> Result<PluginPreset, PresetError> {
    if bytes.is_empty() {
        return Err(PresetError::Empty);
    }
    let bytes = strip_bom(bytes);
    let first = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(0);
    if bytes[first..].starts_with(b"[Preset]") || bytes[first..].starts_with(b"[Parameters]") {
        parse_text(bytes)
    } else {
        parse_binary(bytes)
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

fn parse_binary(bytes: &[u8]) -> Result<PluginPreset, PresetError> {
    if bytes.len() < BINARY_HEADER_LEN {
        return Err(PresetError::TruncatedHeader(bytes.len()));
    }
    let magic: [u8; 4] = bytes[0..4].try_into().expect("4-byte slice");
    let spec = BINARY_SIGNATURES
        .iter()
        .find(|spec| *spec.magic == magic)
        .ok_or_else(|| PresetError::UnknownMagic(String::from_utf8_lossy(&magic).into_owned()))?;
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("u32 slice"));
    if version != spec.version {
        return Err(PresetError::UnsupportedVersion {
            signature: String::from_utf8_lossy(&magic).into_owned(),
            version,
        });
    }
    // Vendor files store a parameter count, not a byte length:
    // `(file_size - 12) / 4 == count`.
    let declared = u32::from_le_bytes(bytes[8..12].try_into().expect("u32 slice")) as usize;
    let payload = &bytes[BINARY_HEADER_LEN..];
    if !payload.len().is_multiple_of(4) {
        return Err(PresetError::UnalignedPayload(payload.len()));
    }
    let actual = payload.len() / 4;
    if actual < declared {
        return Err(PresetError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(PresetError::LengthMismatch { declared, actual });
    }
    if declared == 0 {
        return Err(PresetError::EmptyPayload);
    }

    let mut values = Vec::with_capacity(declared);
    for (index, chunk) in payload.chunks_exact(4).enumerate() {
        let value = f32::from_le_bytes(chunk.try_into().expect("4-byte chunk")) as f64;
        if !value.is_finite() {
            return Err(PresetError::NonFiniteValue { index, value });
        }
        values.push(PresetValue::Indexed {
            index: index as u32,
            plain: value,
        });
    }

    Ok(PluginPreset {
        format: PresetFormat::Binary,
        signature: String::from_utf8_lossy(&magic).into_owned(),
        version,
        plugin: spec.plugin.to_string(),
        name: None,
        author: None,
        description: None,
        values,
    })
}

fn parse_text(bytes: &[u8]) -> Result<PluginPreset, PresetError> {
    let text = std::str::from_utf8(bytes).map_err(|_| PresetError::TextNotUtf8)?;
    let mut section = "";
    let mut seen_preset = false;
    let mut seen_parameters = false;
    let mut signature: Option<String> = None;
    let mut version: Option<u32> = None;
    let mut name = None;
    let mut author = None;
    let mut description = None;
    let mut values = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(PresetError::MalformedLine {
                    line: line_number,
                    text: line.to_string(),
                });
            }
            section = match &line[1..line.len() - 1] {
                "Preset" => {
                    seen_preset = true;
                    "Preset"
                }
                "Parameters" => {
                    seen_parameters = true;
                    "Parameters"
                }
                _ => "Other",
            };
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            return Err(PresetError::MalformedLine {
                line: line_number,
                text: line.to_string(),
            });
        };
        let key = key.trim();
        let value = strip_quotes(raw_value.trim());
        match section {
            "Preset" => match key {
                "Signature" => signature = Some(value.to_string()),
                "Version" => {
                    version = Some(value.parse().map_err(|_| PresetError::NonNumericValue {
                        name: key.to_string(),
                        value: value.to_string(),
                    })?)
                }
                "Name" => name = Some(value.to_string()),
                "Author" => author = Some(value.to_string()),
                "Description" => description = Some(value.to_string()),
                _ => {}
            },
            "Parameters" => {
                if key.is_empty() {
                    return Err(PresetError::EmptyName { line: line_number });
                }
                let plain: f64 = value.parse().map_err(|_| PresetError::NonNumericValue {
                    name: key.to_string(),
                    value: value.to_string(),
                })?;
                if !plain.is_finite() {
                    return Err(PresetError::NonFiniteValue {
                        index: values.len(),
                        value: plain,
                    });
                }
                values.push(PresetValue::Named {
                    name: key.to_string(),
                    plain,
                });
            }
            _ => {}
        }
    }

    if !seen_preset {
        return Err(PresetError::MissingSection("Preset"));
    }
    if !seen_parameters {
        return Err(PresetError::MissingSection("Parameters"));
    }
    let signature = signature.ok_or(PresetError::MissingSignature)?;
    let spec = TEXT_SIGNATURES
        .iter()
        .find(|spec| spec.signature == signature)
        .ok_or_else(|| PresetError::UnknownSignature(signature.clone()))?;
    let version = version.ok_or(PresetError::MissingKey("Version"))?;
    if version != spec.version {
        return Err(PresetError::UnsupportedVersion { signature, version });
    }

    Ok(PluginPreset {
        format: PresetFormat::Text,
        signature: spec.signature.to_string(),
        version,
        plugin: spec.plugin.to_string(),
        name,
        author,
        description,
        values,
    })
}

fn strip_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

/// A scanned plug-in parameter the import can target.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetTarget {
    pub id: u32,
    pub name: String,
    pub stepped: bool,
}

impl PresetTarget {
    /// Build a target from a scanned VST3 parameter.
    pub fn from_detail(detail: &Vst3ParamDetail) -> Self {
        Self {
            id: detail.id,
            name: detail.name.clone(),
            stepped: detail.step_count > 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// One preset value matched to a plugin parameter.
pub struct MappedParam {
    pub id: u32,
    pub name: String,
    pub plain: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
/// Matched and unmatched preset values.
pub struct PresetMapping {
    pub mapped: Vec<MappedParam>,
    /// File entries without a matching scanned parameter; never silently dropped.
    pub unmapped: Vec<String>,
}

/// Match file entries to scanned parameters by VST3 id (binary) or title (text).
///
/// Values are validated against the documented plain domains: stepped controls
/// must carry an integer index, and log2(Hz) frequency controls must fall in
/// the documented 10 Hz..30 kHz range. A normalised payload fails both checks,
/// which is intentional: `.ffp` values are plain, never 0..1-normalised.
pub fn map_preset(
    preset: &PluginPreset,
    targets: &[PresetTarget],
) -> Result<PresetMapping, PresetError> {
    let by_id: HashMap<u32, &PresetTarget> =
        targets.iter().map(|target| (target.id, target)).collect();
    let by_name: HashMap<String, &PresetTarget> = targets
        .iter()
        .map(|target| (normalize_key(&target.name), target))
        .collect();
    let mut mapping = PresetMapping::default();
    for value in &preset.values {
        let (label, target) = match value {
            PresetValue::Indexed { index, .. } => (format!("#{index}"), by_id.get(index).copied()),
            PresetValue::Named { name, .. } => {
                (name.clone(), by_name.get(&normalize_key(name)).copied())
            }
        };
        let Some(target) = target else {
            mapping.unmapped.push(label);
            continue;
        };
        validate_plain(preset, target, &label, value.plain())?;
        mapping.mapped.push(MappedParam {
            id: target.id,
            name: target.name.clone(),
            plain: value.plain(),
        });
    }
    Ok(mapping)
}

fn validate_plain(
    preset: &PluginPreset,
    target: &PresetTarget,
    label: &str,
    plain: f64,
) -> Result<(), PresetError> {
    if target.stepped && plain.fract() != 0.0 {
        return Err(PresetError::NonIntegralStepped {
            label: label.to_string(),
            value: plain,
        });
    }
    if preset.log2_frequencies() && is_frequency_like(&target.name) && !is_log2_hz(plain) {
        return Err(PresetError::NormalizedValue {
            label: label.to_string(),
            value: plain,
        });
    }
    Ok(())
}

fn is_frequency_like(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("frequency") || name.contains("crossover")
}

fn is_log2_hz(plain: f64) -> bool {
    // The stored values are f32, so the documented f64 bounds need a tolerance.
    (LOG2_10_HZ - 1e-4..=LOG2_30_KHZ + 1e-3).contains(&plain)
}

fn normalize_key(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
/// A parameter whose readback differs from the preset.
pub struct ParamMismatch {
    pub name: String,
    pub expected_plain: f64,
    pub readback_plain: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
/// Summary of mapping, application and readback.
pub struct PresetImportReport {
    pub signature: String,
    pub version: u32,
    pub format: &'static str,
    pub preset_name: Option<String>,
    pub parsed: usize,
    pub mapped: usize,
    pub applied: usize,
    pub unmapped: Vec<String>,
    /// Parameters the plug-in refused to write.
    pub rejected: Vec<String>,
    /// Parameters whose plain readback does not match the file value.
    pub mismatched: Vec<ParamMismatch>,
}

/// Map, apply and read back a preset on a live VST3 instance.
pub fn apply_to_instance(
    instance: &mut Vst3Instance,
    preset: &PluginPreset,
) -> Result<PresetImportReport, PresetError> {
    let details = instance.get_param_details();
    let targets: Vec<PresetTarget> = details.iter().map(PresetTarget::from_detail).collect();
    let mapping = map_preset(preset, &targets)?;

    let mut report = PresetImportReport {
        signature: preset.signature.clone(),
        version: preset.version,
        format: preset.format.as_str(),
        preset_name: preset.name.clone(),
        parsed: preset.values.len(),
        mapped: mapping.mapped.len(),
        applied: 0,
        unmapped: mapping.unmapped,
        rejected: Vec::new(),
        mismatched: Vec::new(),
    };

    for param in &mapping.mapped {
        if instance.set_param_plain_by_name(&param.name, param.plain) {
            report.applied += 1;
        } else {
            report.rejected.push(param.name.clone());
        }
    }

    for param in &mapping.mapped {
        let readback = instance.get_param_plain_by_name(&param.name);
        let matches = readback
            .is_some_and(|value| (value - param.plain).abs() <= 1e-4 * param.plain.abs().max(1.0));
        if !matches {
            report.mismatched.push(ParamMismatch {
                name: param.name.clone(),
                expected_plain: param.plain,
                readback_plain: readback,
            });
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINARY_EQ_LOW_CUT: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-lowcut.ffp");
    const BINARY_EQ_TRUNCATED: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-truncated.ffp");
    const BINARY_EQ_BAD_MAGIC: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-bad-magic.ffp");
    const BINARY_EQ_LENGTH_MISMATCH: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-length-mismatch.ffp");
    const BINARY_EQ_NORMALISED: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-normalised.ffp");
    const BINARY_EQ_BAD_VERSION: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/binary-eq-version-3.ffp");
    const TEXT_COMP_CLEAN: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/text-comp-clean.ffp");
    const TEXT_COMP_VOCAL: &[u8] =
        include_bytes!("../tests/fixtures/plugin-preset/text-comp-vocal.ffp");

    fn eq_targets() -> Vec<PresetTarget> {
        (0..189)
            .map(|id| PresetTarget {
                id,
                name: format!("Param {id}"),
                stepped: false,
            })
            .collect()
    }

    #[test]
    fn parses_binary_low_cut_fixture() {
        let preset = parse_preset(BINARY_EQ_LOW_CUT).expect("valid fixture");
        assert_eq!(preset.format, PresetFormat::Binary);
        assert_eq!(preset.signature, "FQ2p");
        assert_eq!(preset.version, 2);
        assert_eq!(preset.plugin, "Pro-Q 2");
        assert_eq!(preset.values.len(), 189);
        let band1: Vec<f64> = preset.values[0..7].iter().map(PresetValue::plain).collect();
        assert_eq!(band1[0], 1.0, "Band 1 State = Enabled");
        assert!((band1[1] - 50f64.log2()).abs() < 1e-6, "50 Hz as log2");
        assert_eq!(band1[2], 0.0, "default gain");
        assert_eq!(band1[3], 0.5, "Q plain 0.5, displayed as 1.0");
        assert_eq!(band1[4], 2.0, "Shape = Low Cut");
        assert_eq!(band1[5], 6.0, "Slope = 48 dB/oct");
        assert_eq!(band1[6], 2.0, "Stereo placement");
    }

    #[test]
    fn parses_text_clean_fixture() {
        let preset = parse_preset(TEXT_COMP_CLEAN).expect("valid fixture");
        assert_eq!(preset.format, PresetFormat::Text);
        assert_eq!(preset.signature, "FC3p");
        assert_eq!(preset.version, 2);
        assert_eq!(preset.plugin, "Pro-C 3");
        assert_eq!(preset.name.as_deref(), Some("Clean"));
        assert_eq!(preset.author.as_deref(), Some("OpenCompose"));
        assert_eq!(
            preset.description.as_deref(),
            Some("Clean, transparent compression")
        );
        let values: Vec<(&str, f64)> = preset
            .values
            .iter()
            .map(|value| match value {
                PresetValue::Named { name, plain } => (name.as_str(), *plain),
                PresetValue::Indexed { .. } => panic!("text fixture has indexed value"),
            })
            .collect();
        assert_eq!(
            values,
            vec![
                ("Style", 0.0),
                ("Threshold", -16.0),
                ("Ratio", 0.560_000_002_384_186),
                ("Knee", 7.345_943_450_927_73),
                ("Attack", 0.142_277_777_194_977),
                ("Release", 0.277_943_134_307_861),
                ("Auto Gain", 1.0),
                ("Knee Display Enabled", 0.0),
                ("Maximum Lookahead", 0.0),
            ]
        );
    }

    #[test]
    fn parses_text_vocal_fixture() {
        let preset = parse_preset(TEXT_COMP_VOCAL).expect("valid fixture");
        assert_eq!(preset.name.as_deref(), Some("Vocal"));
        assert_eq!(preset.values.len(), 7);
        assert_eq!(preset.values[0].plain(), 10.0, "Style");
        assert_eq!(preset.values[1].plain(), -14.0, "Threshold");
    }

    #[test]
    fn rejects_truncated_header() {
        let error = parse_preset(&BINARY_EQ_LOW_CUT[0..8]).unwrap_err();
        assert_eq!(error, PresetError::TruncatedHeader(8));
    }

    #[test]
    fn rejects_truncated_payload() {
        let error = parse_preset(BINARY_EQ_TRUNCATED).unwrap_err();
        assert!(matches!(error, PresetError::TruncatedPayload { .. }));
    }

    #[test]
    fn rejects_wrong_magic() {
        let error = parse_preset(BINARY_EQ_BAD_MAGIC).unwrap_err();
        assert_eq!(error, PresetError::UnknownMagic("XXXX".into()));
    }

    #[test]
    fn rejects_length_mismatch() {
        let error = parse_preset(BINARY_EQ_LENGTH_MISMATCH).unwrap_err();
        assert_eq!(
            error,
            PresetError::LengthMismatch {
                declared: 189,
                actual: 190
            }
        );
    }

    #[test]
    fn rejects_unsupported_binary_version() {
        let error = parse_preset(BINARY_EQ_BAD_VERSION).unwrap_err();
        assert_eq!(
            error,
            PresetError::UnsupportedVersion {
                signature: "FQ2p".into(),
                version: 3,
            }
        );
    }

    #[test]
    fn rejects_unaligned_payload() {
        let mut bytes = BINARY_EQ_LOW_CUT[..14].to_vec();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        // Header declares 2 parameters, so the 2-byte payload is unaligned.
        let error = parse_preset(&bytes).unwrap_err();
        assert_eq!(error, PresetError::UnalignedPayload(2));
    }

    #[test]
    fn rejects_empty_payload() {
        let mut bytes = BINARY_EQ_LOW_CUT[..12].to_vec();
        bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_preset(&bytes).unwrap_err(), PresetError::EmptyPayload);
    }

    #[test]
    fn rejects_non_finite_values() {
        let mut bytes = BINARY_EQ_LOW_CUT.to_vec();
        bytes[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
        let error = parse_preset(&bytes).unwrap_err();
        assert!(matches!(error, PresetError::NonFiniteValue { .. }));
    }

    #[test]
    fn accepts_documented_payload_counts_for_every_binary_signature() {
        for spec in BINARY_SIGNATURES {
            // Documented payload counts per binary signature.
            let counts = match spec.plugin {
                "Pro-Q 2" => 189usize,
                "Pro-Q 3" => 334,
                "Pro-R" => 85,
                "Pro-L 2" => 31,
                "Pro-MB" => 150,
                "Pro-DS" => 20,
                _ => unreachable!(),
            };
            let mut bytes = Vec::with_capacity(12 + counts * 4);
            bytes.extend_from_slice(spec.magic);
            bytes.extend_from_slice(&spec.version.to_le_bytes());
            bytes.extend_from_slice(&(counts as u32).to_le_bytes());
            for _ in 0..counts {
                bytes.extend_from_slice(&0f32.to_le_bytes());
            }
            let preset = parse_preset(&bytes).expect("documented count parses");
            assert_eq!(preset.values.len(), counts, "{} payload", spec.plugin);
        }
    }

    #[test]
    fn rejects_unknown_text_signature() {
        let text = b"[Preset]\nSignature=FC9p\nVersion=2\n[Parameters]\nGain=0\n";
        let error = parse_preset(text).unwrap_err();
        assert_eq!(error, PresetError::UnknownSignature("FC9p".into()));
    }

    #[test]
    fn rejects_unsupported_text_version() {
        let text = b"[Preset]\nSignature=FC3p\nVersion=3\n[Parameters]\nGain=0\n";
        let error = parse_preset(text).unwrap_err();
        assert_eq!(
            error,
            PresetError::UnsupportedVersion {
                signature: "FC3p".into(),
                version: 3,
            }
        );
    }

    #[test]
    fn rejects_missing_parameters_section() {
        let text = b"[Preset]\nSignature=FC3p\nVersion=2\n";
        let error = parse_preset(text).unwrap_err();
        assert_eq!(error, PresetError::MissingSection("Parameters"));
    }

    #[test]
    fn rejects_malformed_and_non_numeric_lines() {
        let malformed = b"[Preset]\nSignature=FC3p\nVersion=2\n[Parameters]\nGain\n";
        assert!(matches!(
            parse_preset(malformed).unwrap_err(),
            PresetError::MalformedLine { .. }
        ));
        let non_numeric = b"[Preset]\nSignature=FC3p\nVersion=2\n[Parameters]\nGain=loud\n";
        assert!(matches!(
            parse_preset(non_numeric).unwrap_err(),
            PresetError::NonNumericValue { .. }
        ));
    }

    #[test]
    fn maps_binary_values_by_vst3_id() {
        let preset = parse_preset(BINARY_EQ_LOW_CUT).unwrap();
        let mapping = map_preset(&preset, &eq_targets()).unwrap();
        assert_eq!(mapping.mapped.len(), 189);
        assert!(mapping.unmapped.is_empty());
        assert_eq!(mapping.mapped[0].id, 0);
        assert_eq!(mapping.mapped[1].id, 1);
        assert!((mapping.mapped[1].plain - 50f64.log2()).abs() < 1e-6);
    }

    #[test]
    fn reports_unmapped_binary_entries() {
        let preset = parse_preset(BINARY_EQ_LOW_CUT).unwrap();
        let targets: Vec<PresetTarget> = eq_targets().into_iter().take(7).collect();
        let mapping = map_preset(&preset, &targets).unwrap();
        assert_eq!(mapping.mapped.len(), 7);
        assert_eq!(mapping.unmapped.len(), 182);
        assert!(mapping.unmapped.contains(&"#7".to_string()));
    }

    fn comp_targets() -> Vec<PresetTarget> {
        // Titles as the vendor preset stores them.
        [
            (0, "Style", true),
            (1, "Threshold", false),
            (4, "Ratio", false),
            (5, "Knee", false),
            (7, "Attack", false),
            (8, "Release", false),
            (19, "Auto Gain", true),
            (95, "Maximum Lookahead", true),
            (98, "Knee Display Enabled", true),
        ]
        .into_iter()
        .map(|(id, name, stepped)| PresetTarget {
            id,
            name: name.to_string(),
            stepped,
        })
        .collect()
    }

    #[test]
    fn maps_text_values_by_parameter_title() {
        let preset = parse_preset(TEXT_COMP_CLEAN).unwrap();
        let mapping = map_preset(&preset, &comp_targets()).unwrap();
        let mapped: Vec<(&str, f64)> = mapping
            .mapped
            .iter()
            .map(|param| (param.name.as_str(), param.plain))
            .collect();
        assert_eq!(
            mapped,
            vec![
                ("Style", 0.0),
                ("Threshold", -16.0),
                ("Ratio", 0.560_000_002_384_186),
                ("Knee", 7.345_943_450_927_73),
                ("Attack", 0.142_277_777_194_977),
                ("Release", 0.277_943_134_307_861),
                ("Auto Gain", 1.0),
                ("Knee Display Enabled", 0.0),
                ("Maximum Lookahead", 0.0),
            ]
        );
        assert!(mapping.unmapped.is_empty());
    }

    #[test]
    fn reports_unmapped_text_entries() {
        let preset = parse_preset(TEXT_COMP_CLEAN).unwrap();
        // Simulate an inventory without the UI-only key: it must be reported.
        let targets: Vec<PresetTarget> = comp_targets()
            .into_iter()
            .filter(|target| target.name != "Knee Display Enabled")
            .collect();
        let mapping = map_preset(&preset, &targets).unwrap();
        assert_eq!(mapping.mapped.len(), 8);
        assert_eq!(mapping.unmapped, vec!["Knee Display Enabled".to_string()]);
    }

    #[test]
    fn maps_text_parameter_names() {
        // Titles and ids as the vendor preset stores them.
        let targets: Vec<PresetTarget> = [
            (0, "Band 1 Used", true),
            (1, "Band 1 Enabled", true),
            (2, "Band 1 Frequency", false),
            (3, "Band 1 Gain", false),
            (4, "Band 1 Q", false),
            (12, "Band 1 Threshold", false),
        ]
        .into_iter()
        .map(|(id, name, stepped)| PresetTarget {
            id,
            name: name.to_string(),
            stepped,
        })
        .collect();
        let text = b"[Preset]\nSignature=FQ4p\nVersion=4\nName=Basic\n[Parameters]\nBand 1 Used=1\nBand 1 Frequency=9.965784\nBand 1 Gain=-3.5\nBand 1 Dynamic Range=0\n";
        let preset = parse_preset(text).unwrap();
        let mapping = map_preset(&preset, &targets).unwrap();
        let mapped: Vec<(u32, &str, f64)> = mapping
            .mapped
            .iter()
            .map(|param| (param.id, param.name.as_str(), param.plain))
            .collect();
        assert_eq!(
            mapped,
            vec![
                (0, "Band 1 Used", 1.0),
                (2, "Band 1 Frequency", 9.965784),
                (3, "Band 1 Gain", -3.5),
            ]
        );
        assert_eq!(mapping.unmapped, vec!["Band 1 Dynamic Range".to_string()]);
    }

    #[test]
    fn rejects_normalised_frequency_payload() {
        let preset = parse_preset(BINARY_EQ_NORMALISED).unwrap();
        let mut targets = eq_targets();
        targets[1].name = "Band 1 Frequency".into();
        let error = map_preset(&preset, &targets).unwrap_err();
        assert!(matches!(
            error,
            PresetError::NormalizedValue { ref label, value }
                if label == "#1" && (value - 0.201_024).abs() < 1e-6
        ));
    }

    #[test]
    fn rejects_fractional_stepped_value() {
        let preset = PluginPreset {
            format: PresetFormat::Text,
            signature: "FC3p".into(),
            version: 2,
            plugin: "Pro-C 3".into(),
            name: None,
            author: None,
            description: None,
            values: vec![PresetValue::Named {
                name: "Auto Gain".into(),
                plain: 0.5,
            }],
        };
        let targets = vec![PresetTarget {
            id: 19,
            name: "Auto Gain".into(),
            stepped: true,
        }];
        assert_eq!(
            map_preset(&preset, &targets).unwrap_err(),
            PresetError::NonIntegralStepped {
                label: "Auto Gain".into(),
                value: 0.5,
            }
        );
    }
}
