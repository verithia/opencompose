//! Arrangement sections: the named spans and repeats that define the rendered timeline.
//!
//! A section list is only valid when the spans are in written order and disjoint,
//! and when the expanded arrangement fits the desktop render budget, so every
//! edit builds a candidate, checks it and only then replaces the project.

use oc_dsl::ast::{self, Section};
use oc_dsl::timing::ProjectClock;
use serde_json::{Value, json};
use tauri::State;

use crate::project_extent::RENDER_BUDGET_SECONDS;
use crate::state::{AppState, UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};

/// The one line the arrangement summary rounds lengths to, in bars and seconds.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// `bar:beat:subdivision` as the DSL writes it; beat defaults to 1 and
/// subdivision to 0, so `9` and `9:1` both mean the first beat of bar 9.
pub(crate) fn parse_position(text: &str) -> Result<ast::BarBeat, String> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(format!(
            "position '{text}' must be bar, bar:beat or bar:beat:subdivision, e.g. 9:1"
        ));
    }
    let mut numbers = [1_u32, 1_u32, 0_u32];
    for (index, part) in parts.iter().enumerate() {
        numbers[index] = part.trim().parse::<u32>().map_err(|_| {
            format!(
                "position '{text}' has a non-numeric part '{part}'; use bar:beat:subdivision, e.g. 9:1"
            )
        })?;
    }
    if numbers[0] == 0 || numbers[1] == 0 {
        return Err(format!(
            "position '{text}' uses 0; bars and beats are 1-based"
        ));
    }
    if numbers[2] >= 4 {
        return Err(format!(
            "position '{text}' has subdivision {}; a beat holds subdivisions 0-3",
            numbers[2]
        ));
    }
    Ok(ast::BarBeat::new(numbers[0], numbers[1], numbers[2]))
}

/// The position as the DSL writes it, without a trailing `:0` subdivision.
pub(crate) fn position_text(position: &ast::BarBeat) -> String {
    if position.subdivision == 0 {
        format!("{}:{}", position.bar, position.beat)
    } else {
        format!(
            "{}:{}:{}",
            position.bar, position.beat, position.subdivision
        )
    }
}

fn names(project: &ast::Project) -> String {
    project
        .sections
        .iter()
        .map(|section| format!("'{}'", section.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn find(project: &ast::Project, name: &str) -> Result<usize, String> {
    match project
        .sections
        .iter()
        .position(|section| section.name == name)
    {
        Some(index) => Ok(index),
        None if project.sections.is_empty() => Err(format!(
            "section '{name}' not found; the project has no sections yet, add one with sections_add"
        )),
        None => Err(format!(
            "section '{name}' not found; the project has {}",
            names(project)
        )),
    }
}

fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("section name cannot be empty; name the part, e.g. 'verse'".into());
    }
    Ok(name)
}

/// A section span in quarter notes, with the text a refusal quotes back.
struct Span {
    name: String,
    start: f64,
    end: f64,
    repeat: u32,
    start_text: String,
    end_text: String,
}

fn spans(project: &ast::Project) -> Result<Vec<Span>, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let mut spans = Vec::with_capacity(project.sections.len());
    for section in &project.sections {
        let start_text = position_text(&section.time_range.start);
        let end_text = position_text(&section.time_range.end);
        let start = clock
            .quarter_notes_at(&section.time_range.start)
            .map_err(|error| format!("section '{}' start {start_text}: {error}", section.name))?;
        let end = clock
            .quarter_notes_at(&section.time_range.end)
            .map_err(|error| format!("section '{}' end {end_text}: {error}", section.name))?;
        if end <= start {
            return Err(format!(
                "section '{}' range [{start_text} - {end_text}] ends before it starts",
                section.name
            ));
        }
        spans.push(Span {
            name: section.name.clone(),
            start,
            end,
            repeat: section.repeat.unwrap_or(1),
            start_text,
            end_text,
        });
    }
    Ok(spans)
}

/// Sections play in written order and never overlap: a later span may only
/// start where the previous one ended or after it.
fn check_order(spans: &[Span]) -> Result<(), String> {
    for pair in spans.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        if current.start >= previous.end - 1e-9 {
            continue;
        }
        if current.start < previous.start - 1e-9 {
            return Err(format!(
                "section '{}' starts at {}, before section '{}' starts at {}; sections play in written order and must not overlap",
                current.name, current.start_text, previous.name, previous.start_text
            ));
        }
        return Err(format!(
            "section '{}' [{} - {}] overlaps section '{}' [{} - {}]; sections must not overlap",
            current.name,
            current.start_text,
            current.end_text,
            previous.name,
            previous.start_text,
            previous.end_text
        ));
    }
    Ok(())
}

/// The rendered end is the anchor plus every play, so it is checked before any
/// expansion: an absurd repeat is refused instead of allocated.
fn check_extent(project: &ast::Project, spans: &[Span], touched: &str) -> Result<(), String> {
    let Some(anchor) = spans.first() else {
        return Ok(());
    };
    let clock = ProjectClock::new(&project.properties)?;
    let rendered = anchor.start
        + spans
            .iter()
            .map(|span| (span.end - span.start) * f64::from(span.repeat))
            .sum::<f64>();
    let seconds = oc_dsl::arrangement::rendered_clock(project)?.seconds_at_quarter(rendered)?;
    if seconds.is_finite() && seconds <= RENDER_BUDGET_SECONDS {
        return Ok(());
    }
    let end_text = clock
        .bar_beat_at_quarter(rendered)
        .map(|position| position_text(&position))
        .unwrap_or_else(|_| format!("{rendered:.1} quarters"));
    Err(format!(
        "section '{touched}' leaves the rendered arrangement ending at {end_text} ({seconds:.1} s), \
         past the {:.0}-second project extent the desktop renderer supports",
        RENDER_BUDGET_SECONDS
    ))
}

fn validate(project: &ast::Project, touched: &str) -> Result<(), String> {
    let spans = spans(project)?;
    check_order(&spans)?;
    check_extent(project, &spans, touched)?;
    oc_dsl::validate(project).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn normalized_repeat(name: &str, repeat: Option<u32>) -> Result<Option<u32>, String> {
    match repeat {
        Some(0) => Err(format!(
            "section '{name}' repeat cannot be 0; a section plays at least once (omit repeat or use 1)"
        )),
        Some(1) | None => Ok(None),
        Some(repeat) => Ok(Some(repeat)),
    }
}

fn normalized_label(label: Option<String>) -> Option<String> {
    label
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
}

fn insert_at_time(project: &mut ast::Project, section: Section) -> Result<usize, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let start = clock.quarter_notes_at(&section.time_range.start)?;
    let index = project
        .sections
        .iter()
        .position(|existing| {
            clock
                .quarter_notes_at(&existing.time_range.start)
                .is_ok_and(|quarter| quarter > start)
        })
        .unwrap_or(project.sections.len());
    project.sections.insert(index, section);
    Ok(index)
}

/// Add one section at its written position. Returns the index it took.
pub(crate) fn apply_add_section(
    project: &mut ast::Project,
    name: &str,
    start: ast::BarBeat,
    end: ast::BarBeat,
    repeat: Option<u32>,
    label: Option<String>,
) -> Result<usize, String> {
    let name = check_name(name)?;
    if project.sections.iter().any(|section| section.name == name) {
        return Err(format!(
            "section '{name}' already exists; use sections_set to change it, or pick another name (the project has {})",
            names(project)
        ));
    }
    let repeat = normalized_repeat(name, repeat)?;
    let mut candidate = project.clone();
    let index = insert_at_time(
        &mut candidate,
        Section {
            name: name.to_string(),
            time_range: ast::TimeRange { start, end },
            repeat,
            label: normalized_label(label),
            flags: None,
        },
    )?;
    validate(&candidate, name)?;
    *project = candidate;
    Ok(index)
}

/// Move, resize, rename, relabel or repeat an existing section. Omitted fields
/// keep their value; `repeat: 1` and an empty label clear theirs. Returns
/// whether anything changed, so an unchanged call pushes no undo entry.
pub(crate) fn apply_set_section(
    project: &mut ast::Project,
    current: &str,
    new_name: Option<String>,
    start: Option<ast::BarBeat>,
    end: Option<ast::BarBeat>,
    repeat: Option<u32>,
    label: Option<String>,
) -> Result<bool, String> {
    let index = find(project, current)?;
    let mut candidate = project.clone();
    let rename = match new_name {
        Some(name) => {
            let name = check_name(&name)?.to_string();
            if name != current
                && candidate
                    .sections
                    .iter()
                    .any(|section| section.name == name)
            {
                return Err(format!(
                    "cannot rename section '{current}' to '{name}': a section named '{name}' already exists (the project has {})",
                    names(&candidate)
                ));
            }
            Some(name)
        }
        None => None,
    };
    let section = &mut candidate.sections[index];
    if let Some(name) = rename {
        section.name = name;
    }
    if let Some(start) = start {
        section.time_range.start = start;
    }
    if let Some(end) = end {
        section.time_range.end = end;
    }
    if let Some(repeat) = repeat {
        section.repeat = normalized_repeat(current, Some(repeat))?;
    }
    if let Some(label) = label {
        section.label = normalized_label(Some(label));
    }
    let touched = candidate.sections[index].name.clone();
    validate(&candidate, &touched)?;
    let changed = project.sections != candidate.sections;
    *project = candidate;
    Ok(changed)
}

/// Remove one section by name.
pub(crate) fn apply_remove_section(project: &mut ast::Project, name: &str) -> Result<(), String> {
    let index = find(project, name)?;
    project.sections.remove(index);
    Ok(())
}

/// The arrangement summary every section tool returns: the written spans, their
/// repeats, and where the expanded arrangement ends.
pub(crate) fn summary(project: &ast::Project, revision: u64) -> Result<Value, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let mut rows = Vec::with_capacity(project.sections.len());
    for (index, section) in project.sections.iter().enumerate() {
        let start = clock.quarter_notes_at(&section.time_range.start)?;
        let end = clock.quarter_notes_at(&section.time_range.end)?;
        let meter = clock.meter_at_bar(section.time_range.start.bar)?;
        let bar_quarters = f64::from(meter.numerator) * 4.0 / f64::from(meter.denominator);
        rows.push(json!({
            "index": index,
            "name": section.name,
            "start": position_text(&section.time_range.start),
            "end": position_text(&section.time_range.end),
            "quarters": round3(end - start),
            "bars": round3((end - start) / bar_quarters),
            "repeat": section.repeat.unwrap_or(1),
            "label": section.label,
        }));
    }
    let (rendered_end, rendered_quarters, rendered_seconds) =
        match oc_dsl::arrangement::rendered_end_quarter(project)? {
            Some(quarter) => (
                clock
                    .bar_beat_at_quarter(quarter)
                    .map(|position| position_text(&position))
                    .unwrap_or_else(|_| format!("{quarter:.3} quarters")),
                round3(quarter),
                round3(oc_dsl::arrangement::rendered_clock(project)?.seconds_at_quarter(quarter)?),
            ),
            None => ("0:1".to_string(), 0.0, 0.0),
        };
    Ok(json!({
        "count": rows.len(),
        "sections": rows,
        "rendered_end": rendered_end,
        "rendered_quarters": rendered_quarters,
        "rendered_bars": round3(rendered_quarters / 4.0),
        "rendered_seconds": rendered_seconds,
        "revision": revision,
    }))
}

/// Add a section.
#[tauri::command]
pub fn add_section(
    name: String,
    start: ast::BarBeat,
    end: ast::BarBeat,
    repeat: Option<u32>,
    label: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_add_section(&mut inner.project, &name, start, end, repeat, label)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add section", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Update a section.
pub fn set_section(
    name: String,
    new_name: Option<String>,
    start: Option<ast::BarBeat>,
    end: Option<ast::BarBeat>,
    repeat: Option<u32>,
    label: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    if apply_set_section(
        &mut inner.project,
        &name,
        new_name,
        start,
        end,
        repeat,
        label,
    )? {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("section edit", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Remove a section.
#[tauri::command]
pub fn remove_section(name: String, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_remove_section(&mut inner.project, &name)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove section", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
