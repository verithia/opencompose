//! Typed diagnostic location authority, independent of display messages.
use crate::{
    error::ValidationError,
    source_map::{ByteSpan, ParsedDocument, SourceSubject},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Where a diagnostic points, before source resolution.
pub enum DiagnosticSubject {
    /// A legacy check has not emitted a typed subject. No owner is inferred.
    UnknownSubject,
    Located(SourceSubject),
    /// A genuinely document-wide constraint, rather than a missing mapping.
    Document,
    /// Programmatically constructed ASTs and binary snapshots have no text.
    NoSource,
    /// The check or parser has not supplied this subject's source location.
    Unmapped(SourceSubject),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Resolved diagnostic location.
pub enum DiagnosticLocation {
    UnknownSubject,
    Span(ByteSpan),
    Document,
    NoSource,
    Unmapped(SourceSubject),
}
impl DiagnosticSubject {
    /// Resolve a subject against a parsed document.
    pub fn resolve(self, document: &ParsedDocument) -> DiagnosticLocation {
        match self {
            Self::UnknownSubject => DiagnosticLocation::UnknownSubject,
            Self::Located(subject) => document.span(subject).map_or(
                DiagnosticLocation::Unmapped(subject),
                DiagnosticLocation::Span,
            ),
            Self::Document => DiagnosticLocation::Document,
            Self::NoSource => DiagnosticLocation::NoSource,
            Self::Unmapped(subject) => DiagnosticLocation::Unmapped(subject),
        }
    }
}
#[derive(Debug)]
/// A validation error with its typed subject.
pub struct ValidationDiagnostic {
    pub error: ValidationError,
    pub subject: DiagnosticSubject,
}
impl ValidationDiagnostic {
    /// The one-based line and UTF-16 column of this diagnostic, when mapped.
    pub fn locate(&self, document: &ParsedDocument) -> Option<(u32, u32)> {
        match self.subject.resolve(document) {
            DiagnosticLocation::Span(span) => document
                .utf16_range(span)
                .ok()
                .map(|range| (range.start.line + 1, range.start.character + 1)),
            _ => None,
        }
    }
}
