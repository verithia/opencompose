//! Rendered section sound flags.
//!
//! A section's `flags:` names the articulation in force for notes that start
//! inside it. The table is built from the expanded arrangement, so a repeated
//! section carries its flag every time it plays.

use oc_dsl::ast::{Articulation, Project};
use oc_dsl::timing::ProjectClock;

#[derive(Debug, Clone, Copy)]
pub(super) struct SectionFlag {
    pub start: f64,
    pub end: f64,
    pub articulation: Articulation,
}

/// The flagged sections in rendered quarter notes.
pub(super) fn table(project: &Project, clock: &ProjectClock) -> Result<Vec<SectionFlag>, String> {
    let mut flags = Vec::new();
    for section in &project.sections {
        let Some(articulation) = section.flags else {
            continue;
        };
        let start = clock.quarter_notes_at(&section.time_range.start)?;
        let end = clock.quarter_notes_at(&section.time_range.end)?;
        flags.push(SectionFlag {
            start,
            end,
            articulation,
        });
    }
    Ok(flags)
}

/// The technique in force at a note onset, or `None` outside a flagged section.
pub(super) fn articulation_at(flags: &[SectionFlag], quarter: f64) -> Option<Articulation> {
    flags
        .iter()
        .rev()
        .find(|flag| quarter >= flag.start - 1e-9 && quarter < flag.end - 1e-9)
        .map(|flag| flag.articulation)
}
