//! Attribution is appended at the owning check, never recovered from error text.
use std::collections::HashMap;

use crate::{
    diagnostics::{DiagnosticSubject, ValidationDiagnostic},
    error::ValidationError,
    source_map::SourceSubject,
};

#[derive(Default)]
pub(super) struct Emissions {
    locations: Option<Vec<(usize, SourceSubject)>>,
}
impl Emissions {
    pub(super) fn enabled() -> Self {
        Self {
            locations: Some(Vec::new()),
        }
    }
    pub(super) fn push(
        &mut self,
        errors: &mut Vec<ValidationError>,
        error: ValidationError,
        subject: SourceSubject,
    ) {
        // The private validator only appends errors. One attribution per selected
        // emission bounds this sidecar by the number of actual errors.
        if let Some(locations) = &mut self.locations {
            locations.push((errors.len(), subject));
        }
        errors.push(error);
    }
    /// Attribute errors appended by one check to its owning source block.
    /// An inner check's more specific subject wins over an outer range.
    pub(super) fn attribute(&mut self, start: usize, end: usize, subject: SourceSubject) {
        if let Some(locations) = &mut self.locations {
            for index in start..end {
                if !locations.iter().any(|(at, _)| *at == index) {
                    locations.push((index, subject));
                }
            }
        }
    }

    pub(super) fn finish(self, errors: Vec<ValidationError>) -> Vec<ValidationDiagnostic> {
        let locations: HashMap<usize, SourceSubject> =
            self.locations.unwrap_or_default().into_iter().collect();
        errors
            .into_iter()
            .enumerate()
            .map(|(index, error)| {
                let subject = locations.get(&index).copied().map_or(
                    DiagnosticSubject::UnknownSubject,
                    DiagnosticSubject::Located,
                );
                ValidationDiagnostic { error, subject }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagnosticSubject;
    use crate::source_map::{SourceOwner, SourcePart};

    #[test]
    fn a_specific_subject_is_not_overwritten_by_an_enclosing_range() {
        let mut emissions = Emissions::enabled();
        let mut errors = Vec::new();
        let specific = SourceSubject {
            owner: SourceOwner::Track(0),
            part: SourcePart::Whole,
        };
        emissions.push(
            &mut errors,
            ValidationError::OutOfRange {
                field: "track".into(),
                value: 0.0,
                min: 1.0,
                max: 2.0,
            },
            specific,
        );
        emissions.attribute(
            0,
            1,
            SourceSubject {
                owner: SourceOwner::Project,
                part: SourcePart::Whole,
            },
        );
        let diagnostics = emissions.finish(errors);
        assert_eq!(diagnostics[0].subject, DiagnosticSubject::Located(specific));
    }
}
