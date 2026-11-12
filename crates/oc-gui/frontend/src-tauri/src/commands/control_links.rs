//! One-to-many control links: learn, retune and remove target rows.
//!
//! A link is keyed by its control source; learning the same target twice keeps
//! one row for it (one record per target), and a link with no rows left is
//! removed so a project never carries an empty fan-out.

use tauri::State;

use oc_dsl::ast::{ControlLink, ControlLinkSource, ControlLinkTarget, LinkFormula};

use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};
use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

const MAX_SMOOTHING_MS: u32 = 60_000;

/// Add one target row to the link for `source`, creating the link on first use.
#[tauri::command]
pub fn add_control_link_target(
    source: ControlLinkSource,
    target: ControlLinkTarget,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    add_control_link_target_inner(&mut inner, source, target)?;
    Ok(snapshot(&inner))
}

pub(crate) fn add_control_link_target_inner(
    inner: &mut AppStateInner,
    source: ControlLinkSource,
    mut target: ControlLinkTarget,
) -> Result<(), String> {
    target.track = target.track.trim().to_string();
    target.param = target.param.trim().to_string();
    validate_target(inner, &source, &target)?;
    let old_project = inner.project.clone();
    let index = find_link(&inner.project.properties.control_links, &source)
        .unwrap_or_else(|| insert_link(inner, source.clone()));
    if !inner.project.properties.control_links[index].learn_target(target) {
        return Ok(());
    }
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("control link learn", UndoScope::Automation),
    )?;
    Ok(())
}

/// Retune one existing row; the target pair identifies the row.
#[tauri::command]
pub fn update_control_link_target(
    source: ControlLinkSource,
    track: String,
    param: String,
    formula: LinkFormula,
    smoothing_ms: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_control_link_target_inner(&mut inner, source, &track, &param, formula, smoothing_ms)?;
    Ok(snapshot(&inner))
}

pub(crate) fn update_control_link_target_inner(
    inner: &mut AppStateInner,
    source: ControlLinkSource,
    track: &str,
    param: &str,
    formula: LinkFormula,
    smoothing_ms: u32,
) -> Result<(), String> {
    if formula.input() == oc_dsl::ast::LinkFormulaInput::Midi7Bit && !source.is_midi() {
        return Err(format!(
            "'{formula}' reads raw MIDI values; the source is not a MIDI CC"
        ));
    }
    let index = find_link(&inner.project.properties.control_links, &source)
        .ok_or_else(|| "No control link for that source".to_string())?;
    let link = &inner.project.properties.control_links[index];
    let current = link
        .target(track, param)
        .ok_or_else(|| format!("No control link target '{track}:{param}'"))?;
    if current.formula == formula && current.smoothing_ms == smoothing_ms.min(MAX_SMOOTHING_MS) {
        return Ok(());
    }
    let old_project = inner.project.clone();
    let target = inner.project.properties.control_links[index]
        .targets
        .iter_mut()
        .find(|entry| entry.track == track && entry.param == param)
        .expect("target resolved above");
    target.formula = formula;
    target.smoothing_ms = smoothing_ms.min(MAX_SMOOTHING_MS);
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("control link formula", UndoScope::Automation),
    )?;
    Ok(())
}

/// Drop one row; an empty link is removed with its last row.
#[tauri::command]
pub fn remove_control_link_target(
    source: ControlLinkSource,
    track: String,
    param: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_control_link_target_inner(&mut inner, &source, &track, &param)?;
    Ok(snapshot(&inner))
}

pub(crate) fn remove_control_link_target_inner(
    inner: &mut AppStateInner,
    source: &ControlLinkSource,
    track: &str,
    param: &str,
) -> Result<(), String> {
    let index = find_link(&inner.project.properties.control_links, source)
        .ok_or_else(|| "No control link for that source".to_string())?;
    let link = &inner.project.properties.control_links[index];
    if link.target(track, param).is_none() {
        return Err(format!("No control link target '{track}:{param}'"));
    }
    let old_project = inner.project.clone();
    let links = &mut inner.project.properties.control_links;
    links[index]
        .targets
        .retain(|entry| entry.track != track || entry.param != param);
    if links[index].targets.is_empty() {
        links.remove(index);
    }
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("control link remove", UndoScope::Automation),
    )?;
    Ok(())
}

fn validate_target(
    inner: &AppStateInner,
    source: &ControlLinkSource,
    target: &ControlLinkTarget,
) -> Result<(), String> {
    if target.track.is_empty() {
        return Err("A control link target needs a track".into());
    }
    if target.param.is_empty() {
        return Err("A control link target needs a parameter".into());
    }
    if !inner
        .project
        .tracks
        .iter()
        .any(|track| track.name == target.track)
    {
        return Err(format!("Unknown track '{}'", target.track));
    }
    if target.formula.input() == oc_dsl::ast::LinkFormulaInput::Midi7Bit && !source.is_midi() {
        return Err(format!(
            "'{}' reads raw MIDI values; the source is not a MIDI CC",
            target.formula
        ));
    }
    if target.smoothing_ms > MAX_SMOOTHING_MS {
        return Err(format!(
            "Smoothing {}ms is longer than {}s",
            target.smoothing_ms,
            MAX_SMOOTHING_MS / 1000
        ));
    }
    Ok(())
}

fn find_link(links: &[ControlLink], source: &ControlLinkSource) -> Option<usize> {
    links.iter().position(|link| &link.source == source)
}

/// Create a link with a unique name derived from its source.
fn insert_link(inner: &mut AppStateInner, source: ControlLinkSource) -> usize {
    let base = match &source {
        ControlLinkSource::MidiCc {
            channel,
            controller,
        } => {
            if *channel == 1 {
                format!("cc{controller}")
            } else {
                format!("cc{controller}_ch{channel}")
            }
        }
        ControlLinkSource::Macro { index } => format!("macro{index}"),
        ControlLinkSource::Lane { track, .. } => format!("lane_{}", sanitize(track)),
    };
    let mut name = base.clone();
    let mut suffix = 2;
    while inner
        .project
        .properties
        .control_links
        .iter()
        .any(|link| link.name == name)
    {
        name = format!("{base}_{suffix}");
        suffix += 1;
    }
    let link = ControlLink {
        name,
        source,
        targets: Vec::new(),
    };
    inner.project.properties.control_links.push(link);
    inner.project.properties.control_links.len() - 1
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "link".into()
    } else {
        cleaned
    }
}
