//! Adapt the DSL owner's validation result for language-server consumers.

use oc_dsl::{ast::Project, error::ValidationError};
use serde::{Deserialize, Serialize};

/// The validator's result in the shape the language server publishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<ValidationMessage>,
    pub warnings: Vec<ValidationMessage>,
    pub hints: Vec<ValidationMessage>,
}

/// One diagnostic with the AST path it applies to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationMessage {
    /// AST owner when supplied by the validator; otherwise the entire project.
    pub path: String,
    pub message: String,
    pub suggestion: Option<String>,
}

/// Validate a project through the DSL owner and report each error at its AST
/// path. Warnings and hints stay empty until the validator supplies them.
pub fn validate_project(project: &Project) -> ValidationReport {
    let errors: Vec<_> = oc_dsl::validate(project)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|error| ValidationMessage {
            path: validation_path(&error),
            message: error.to_string(),
            suggestion: None,
        })
        .collect();
    ValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings: Vec::new(),
        hints: Vec::new(),
    }
}

pub(crate) fn validation_path(error: &ValidationError) -> String {
    match error {
        ValidationError::TimeRangeOutOfBounds { track, phrase } => {
            format!("track:{track}.phrase:{phrase}")
        }
        ValidationError::DuplicateTrack { name } => format!("track:{name}"),
        ValidationError::DuplicatePreset { name } => format!("preset:{name}"),
        ValidationError::UnknownBus { track, .. } => format!("track:{track}.mix"),
        ValidationError::UnknownPreset { owner, .. } => owner.clone(),
        ValidationError::UnknownRoute { bus, .. } => format!("bus:{bus}"),
        ValidationError::InvalidValue { field, .. } => field.clone(),
        _ => "project".into(),
    }
}
