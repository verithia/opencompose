//! Timeline markers: the named points the ruler draws and the user edits.
//!
//! A marker is an annotation, so an edit never has to satisfy an arrangement
//! rule. The list stays sorted by position, which keeps the written order and
//! the DSL block order the same after every move.

use oc_dsl::ast::{self, Marker};
use tauri::State;

use crate::state::{AppState, UndoEdit, UndoScope};

use super::sections::position_text;
use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};

fn names(project: &ast::Project) -> String {
    project
        .markers
        .iter()
        .map(|marker| format!("'{}'", marker.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("marker name cannot be empty; name the landmark, e.g. 'verse'".into());
    }
    Ok(name)
}

fn check_position(position: &ast::BarBeat, name: &str) -> Result<(), String> {
    if position.bar == 0 || position.beat == 0 {
        return Err(format!(
            "marker '{name}' is at {}; bars and beats are 1-based",
            position_text(position)
        ));
    }
    if position.subdivision >= 4 {
        return Err(format!(
            "marker '{name}' is at {}; a beat holds subdivisions 0-3",
            position_text(position)
        ));
    }
    Ok(())
}

fn find(project: &ast::Project, name: &str) -> Result<usize, String> {
    match project
        .markers
        .iter()
        .position(|marker| marker.name == name)
    {
        Some(index) => Ok(index),
        None if project.markers.is_empty() => Err(format!(
            "marker '{name}' not found; the project has no markers yet, add one with markers_add"
        )),
        None => Err(format!(
            "marker '{name}' not found; the project has {}",
            names(project)
        )),
    }
}

/// Index the marker belongs at: markers read in timeline order.
fn insertion_index(project: &ast::Project, position: &ast::BarBeat) -> usize {
    let order = |candidate: &ast::BarBeat| (candidate.bar, candidate.beat, candidate.subdivision);
    let wanted = order(position);
    project
        .markers
        .iter()
        .position(|marker| order(&marker.position) > wanted)
        .unwrap_or(project.markers.len())
}

fn validate(project: &ast::Project, touched: &str) -> Result<(), String> {
    oc_dsl::validate(project).map_err(|errors| {
        let text = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        format!("marker '{touched}' was not saved: {text}")
    })
}

/// Add one marker at its timeline position. Returns the index it took.
pub(crate) fn apply_add_marker(
    project: &mut ast::Project,
    name: &str,
    position: ast::BarBeat,
) -> Result<usize, String> {
    let name = check_name(name)?;
    if project.markers.iter().any(|marker| marker.name == name) {
        return Err(format!(
            "marker '{name}' already exists; rename it with markers_set or pick another name \
             (the project has {})",
            names(project)
        ));
    }
    check_position(&position, name)?;
    let mut candidate = project.clone();
    let index = insertion_index(&candidate, &position);
    candidate.markers.insert(
        index,
        Marker {
            name: name.to_string(),
            position,
        },
    );
    validate(&candidate, name)?;
    *project = candidate;
    Ok(index)
}

/// Move one marker, keeping the list in written order. Returns whether
/// anything changed, so an unchanged call pushes no undo entry.
pub(crate) fn apply_move_marker(
    project: &mut ast::Project,
    name: &str,
    position: ast::BarBeat,
) -> Result<bool, String> {
    let index = find(project, name)?;
    check_position(&position, name)?;
    if project.markers[index].position == position {
        return Ok(false);
    }
    let mut candidate = project.clone();
    let marker = candidate.markers.remove(index);
    let index = insertion_index(&candidate, &position);
    candidate.markers.insert(
        index,
        Marker {
            name: marker.name,
            position,
        },
    );
    validate(&candidate, name)?;
    *project = candidate;
    Ok(true)
}

/// Rename one marker. Returns whether anything changed.
pub(crate) fn apply_rename_marker(
    project: &mut ast::Project,
    name: &str,
    new_name: &str,
) -> Result<bool, String> {
    let index = find(project, name)?;
    let new_name = check_name(new_name)?;
    if new_name == name {
        return Ok(false);
    }
    if project.markers.iter().any(|marker| marker.name == new_name) {
        return Err(format!(
            "cannot rename marker '{name}' to '{new_name}': a marker named '{new_name}' already \
             exists (the project has {})",
            names(project)
        ));
    }
    let mut candidate = project.clone();
    candidate.markers[index].name = new_name.to_string();
    validate(&candidate, new_name)?;
    *project = candidate;
    Ok(true)
}

/// Remove one marker by name.
pub(crate) fn apply_remove_marker(project: &mut ast::Project, name: &str) -> Result<(), String> {
    let index = find(project, name)?;
    project.markers.remove(index);
    Ok(())
}

/// Add a project marker.
#[tauri::command]
pub fn add_marker(
    name: String,
    position: ast::BarBeat,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_add_marker(&mut inner.project, &name, position)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add marker", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Move a project marker.
#[tauri::command]
pub fn move_marker(
    name: String,
    position: ast::BarBeat,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    if apply_move_marker(&mut inner.project, &name, position)? {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("move marker", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Rename a project marker.
#[tauri::command]
pub fn rename_marker(
    name: String,
    new_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    if apply_rename_marker(&mut inner.project, &name, &new_name)? {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("rename marker", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Remove a project marker.
#[tauri::command]
pub fn remove_marker(name: String, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_remove_marker(&mut inner.project, &name)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("delete marker", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
