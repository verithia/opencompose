//! Supported normalized synth-lane tables. The stored number is independent of
//! the project syntax and application release versions.

use serde::Deserialize;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Automation value normalization written in the project.
pub enum AutomationNormalization {
    Legacy,
    Wide,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Why an automation normalization version is unsupported.
pub enum NormalizationVersionError {
    #[error("unsupported automation_normalization version {0}; supported versions are 1 and 2")]
    Unsupported(u8),
    #[error("invalid automation_normalization version '{0}'; expected 1 or 2")]
    Invalid(String),
}

impl AutomationNormalization {
    /// Normalize an optional serialized version, defaulting to legacy.
    pub fn from_optional(version: Option<u8>) -> Result<Self, NormalizationVersionError> {
        version.unwrap_or(1).try_into()
    }

    /// The serialized version number.
    pub const fn number(self) -> u8 {
        match self {
            Self::Legacy => 1,
            Self::Wide => crate::ast::WIDE_AUTOMATION_NORMALIZATION,
        }
    }
}

impl TryFrom<u8> for AutomationNormalization {
    type Error = NormalizationVersionError;

    fn try_from(version: u8) -> Result<Self, Self::Error> {
        match version {
            1 => Ok(Self::Legacy),
            crate::ast::WIDE_AUTOMATION_NORMALIZATION => Ok(Self::Wide),
            other => Err(NormalizationVersionError::Unsupported(other)),
        }
    }
}

impl FromStr for AutomationNormalization {
    type Err = NormalizationVersionError;

    fn from_str(written: &str) -> Result<Self, Self::Err> {
        match written {
            "1" => Ok(Self::Legacy),
            "2" => Ok(Self::Wide),
            other => match other.parse::<u8>() {
                Ok(version) if !matches!(version, 1 | 2) => {
                    Err(NormalizationVersionError::Unsupported(version))
                }
                _ => Err(NormalizationVersionError::Invalid(other.to_owned())),
            },
        }
    }
}

pub(crate) fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = Option::<u8>::deserialize(deserializer)?;
    AutomationNormalization::from_optional(version).map_err(serde::de::Error::custom)?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_normalization_checked_decoder_exhausts_the_stored_byte_domain() {
        assert_eq!(
            AutomationNormalization::from_optional(None),
            Ok(AutomationNormalization::Legacy)
        );
        for version in 0..=u8::MAX {
            assert_eq!(
                AutomationNormalization::try_from(version).is_ok(),
                matches!(version, 1 | 2)
            );
        }
    }

    #[test]
    fn automation_normalization_declarations_refuse_unknown_and_malformed_values() {
        for written in [
            "0",
            "3",
            "255",
            "256",
            "999999999999999999999999",
            "-1",
            "1.5",
            "two",
            "",
            "01",
            "2x",
        ] {
            assert!(
                crate::parse(&format!(
                    "@project\n  automation_normalization: {written}\n"
                ))
                .is_err(),
                "{written}"
            );
        }
    }

    #[test]
    fn automation_normalization_supported_values_preserve_text_and_json_identity() {
        for declared in [None, Some(1), Some(2)] {
            let source = declared.map_or_else(
                || "@project\n".to_owned(),
                |version| format!("@project\n  automation_normalization: {version}\n"),
            );
            let project = crate::parse(&source).unwrap();
            assert_eq!(project.properties.automation_normalization, declared);
            assert_eq!(
                project.properties.automation_normalization_version(),
                declared.unwrap_or(1)
            );
            assert_eq!(
                project.properties.uses_wide_automation_normalization(),
                declared == Some(2)
            );
            assert!(crate::validate(&project).is_ok());
            assert_eq!(crate::parse(&crate::serialize(&project)).unwrap(), project);
            let wire = serde_json::to_value(&project).unwrap();
            assert_eq!(
                serde_json::from_value::<crate::ast::Project>(wire).unwrap(),
                project
            );
        }
    }

    #[test]
    fn automation_normalization_json_and_direct_ast_refuse_unsupported_versions() {
        let project = crate::parse("@project\n").unwrap();
        for version in [0, 3, 255] {
            let mut invalid = project.clone();
            invalid.properties.automation_normalization = Some(version);
            assert!(!invalid.properties.uses_wide_automation_normalization());
            let errors = crate::validate(&invalid).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|error| error.to_string().contains("automation_normalization"))
            );
            let wire = serde_json::to_value(&invalid).unwrap();
            assert!(serde_json::from_value::<crate::ast::Project>(wire).is_err());
        }
        for malformed in [
            serde_json::json!(256),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("2"),
        ] {
            let mut wire = serde_json::to_value(&project).unwrap();
            wire["properties"]["automation_normalization"] = malformed;
            assert!(serde_json::from_value::<crate::ast::Project>(wire).is_err());
        }
        let mut wire = serde_json::to_value(&project).unwrap();
        wire["properties"]["automation_normalization"] = serde_json::Value::Null;
        assert_eq!(
            serde_json::from_value::<crate::ast::Project>(wire)
                .unwrap()
                .properties
                .automation_normalization,
            None
        );
    }
}
