//! Parse and adapt diagnostics from the owning DSL implementation.

use crate::DiagnosticInfo;
use oc_dsl::{
    ast::Project,
    diagnostics::DiagnosticLocation,
    source_map::{DocumentParseError, SourceLimits, SourceMapError},
};
use serde::Serialize;
use tower_lsp::lsp_types::{DiagnosticSeverity, Position, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocationStatus {
    Located,
    UnknownSubject,
    UnmappedSubject,
    Document,
    NoSource,
    InvalidSpan,
}

pub(crate) enum CacheAdmission<'a> {
    Admitted {
        source: &'a str,
        project: Option<Box<Project>>,
    },
    Rejected,
}

pub(crate) struct DocumentAnalysis<'a> {
    pub cache: CacheAdmission<'a>,
    pub errors: Vec<DiagnosticInfo>,
}

#[cfg(test)]
pub fn parse_and_diagnose(source: &str) -> (Option<Project>, Vec<DiagnosticInfo>) {
    parse_with_limits(source, SourceLimits::default())
}

#[cfg(test)]
pub(crate) fn parse_with_limits(
    source: &str,
    limits: SourceLimits,
) -> (Option<Project>, Vec<DiagnosticInfo>) {
    let analysis = analyze_with_limits(source, limits);
    let project = match analysis.cache {
        CacheAdmission::Admitted { project, .. } => project.map(|project| *project),
        CacheAdmission::Rejected => None,
    };
    (project, analysis.errors)
}

pub(crate) fn analyze_with_limits(source: &str, limits: SourceLimits) -> DocumentAnalysis<'_> {
    match oc_dsl::parse_document(source, limits) {
        Ok(document) => {
            let diagnostics = document
                .validation_diagnostics()
                .into_iter()
                .map(|diagnostic| {
                    let (range, location) = match diagnostic.subject.resolve(&document) {
                        DiagnosticLocation::Span(span) => match document.utf16_range(span) {
                            Ok(range) => (
                                Range::new(
                                    Position::new(range.start.line, range.start.character),
                                    Position::new(range.end.line, range.end.character),
                                ),
                                LocationStatus::Located,
                            ),
                            Err(_) => (Range::default(), LocationStatus::InvalidSpan),
                        },
                        DiagnosticLocation::UnknownSubject => {
                            (Range::default(), LocationStatus::UnknownSubject)
                        }
                        DiagnosticLocation::Unmapped(_) => {
                            (Range::default(), LocationStatus::UnmappedSubject)
                        }
                        DiagnosticLocation::Document => {
                            (Range::default(), LocationStatus::Document)
                        }
                        DiagnosticLocation::NoSource => {
                            (Range::default(), LocationStatus::NoSource)
                        }
                    };
                    DiagnosticInfo {
                        range,
                        location,
                        message: format!(
                            "{}: {}",
                            crate::validate::validation_path(&diagnostic.error),
                            diagnostic.error
                        ),
                        severity: DiagnosticSeverity::ERROR,
                    }
                })
                .collect();
            DocumentAnalysis {
                cache: CacheAdmission::Admitted {
                    source,
                    project: Some(Box::new(document.into_project())),
                },
                errors: diagnostics,
            }
        }
        Err(error) => {
            let (range, location) = match &error {
                DocumentParseError::Parse(
                    oc_dsl::error::ParseError::Syntax { line, col, .. }
                    | oc_dsl::error::ParseError::UnexpectedToken { line, col, .. },
                ) => (
                    crate::position::parser_range(source, *line, *col),
                    LocationStatus::Located,
                ),
                DocumentParseError::Parse(_) => (Range::default(), LocationStatus::UnknownSubject),
                DocumentParseError::Source(_) => (Range::default(), LocationStatus::Document),
            };
            let cache = match &error {
                // This is the DSL owner's admission result, not a repeated byte check.
                DocumentParseError::Source(SourceMapError::SourceLimit) => CacheAdmission::Rejected,
                _ => CacheAdmission::Admitted {
                    source,
                    project: None,
                },
            };
            DocumentAnalysis {
                cache,
                errors: vec![DiagnosticInfo {
                    range,
                    location,
                    message: error.to_string(),
                    severity: DiagnosticSeverity::ERROR,
                }],
            }
        }
    }
}
