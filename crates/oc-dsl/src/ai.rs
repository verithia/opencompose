//! AI tool interface — builds context for the model, applies edits.
//!
//! The AI tool works on AST subtrees, not raw text. This module provides:
//!
//! 1. **Context building** — extract relevant subtrees as DSL text for the model prompt
//! 2. **Edit application** — parse model output, validate, splice into AST
//! 3. **Diff generation** — show what changed between old and new DSL
//! 4. **Validation** — check if the model's output is valid DSL
//!
//! The model never sees the full project at once. It gets targeted context
//! (the track being edited + project properties) and returns targeted edits.

use crate::ast::*;
use crate::ops;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ============================================================================
// AI Context
// ============================================================================

/// Context prepared for the AI model — everything it needs to make an edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiContext {
    /// System prompt with DSL syntax rules.
    pub system_prompt: String,
    /// The DSL text of the subtree(s) the model should work on.
    pub dsl_context: String,
    /// What the user is asking for.
    pub user_instruction: String,
    /// Which AST paths are included in the context.
    pub context_paths: Vec<String>,
    /// Which path the model should output (what to replace).
    pub edit_target: String,
}

/// Build context for the AI model to edit a specific part of the project.
pub fn build_context(
    project: &Project,
    target_path: &str,
    user_instruction: &str,
) -> Result<AiContext, String> {
    // Always include project properties for tempo/key/time_sig reference
    let properties_ctx = ops::extract(project, "project.properties")?;

    // Extract the target subtree
    let target_ctx = ops::extract(project, target_path)?;

    // If editing a track, also include presets it references
    let mut extra_context = String::new();
    if target_path.starts_with("track:") {
        // Find the track
        if let Ok((_, track)) = resolve_track_from_path(project, target_path) {
            // Include the preset definition if the track uses one
            if let Some(preset_name) = &track.preset
                && let Ok(preset_ctx) = ops::extract(project, &format!("preset:{}", preset_name))
            {
                extra_context.push_str(&preset_ctx.dsl_text);
                extra_context.push('\n');
            }
        }
    }

    let dsl_context = format!(
        "// Project properties\n{}\n\n// Target to edit\n{}\n{}",
        properties_ctx.dsl_text.trim(),
        target_ctx.dsl_text.trim(),
        if extra_context.is_empty() {
            String::new()
        } else {
            format!("\n// Related presets\n{}", extra_context.trim())
        }
    );

    let context_paths = vec!["project.properties".to_string(), target_path.to_string()];

    Ok(AiContext {
        system_prompt: system_prompt(),
        dsl_context,
        user_instruction: user_instruction.to_string(),
        context_paths,
        edit_target: target_path.to_string(),
    })
}

/// Build context for generating a new element (track, phrase, preset).
pub fn build_generate_context(
    project: &Project,
    what: &str, // "track", "phrase", "preset"
    user_instruction: &str,
) -> Result<AiContext, String> {
    let properties_ctx = ops::extract(project, "project.properties")?;

    // Include existing tracks as reference (names + instruments only)
    let mut track_summary = String::new();
    for (i, track) in project.tracks.iter().enumerate() {
        let sound = match &track.preset {
            Some(p) => p.clone(),
            None => track.instrument.as_path(),
        };
        track_summary.push_str(&format!("// Track {}: {} [{}]\n", i, track.name, sound));
    }

    let dsl_context = format!(
        "// Project properties\n{}\n\n// Existing tracks\n{}\n",
        properties_ctx.dsl_text.trim(),
        track_summary.trim(),
    );

    Ok(AiContext {
        system_prompt: system_prompt(),
        dsl_context,
        user_instruction: user_instruction.to_string(),
        context_paths: vec!["project.properties".to_string()],
        edit_target: what.to_string(),
    })
}

// ============================================================================
// Edit Application
// ============================================================================

/// The model's response — new DSL text for the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiEdit {
    /// The DSL text output by the model.
    pub new_dsl: String,
    /// Which path this replaces.
    pub target_path: String,
    /// Optional model explanation.
    pub explanation: Option<String>,
}

/// Result of validating the model's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    /// The parsed DSL (if valid).
    pub parsed_dsl: Option<String>,
}

/// Validate that the model's output is parseable DSL.
pub fn validate_edit(edit: &AiEdit, project: &Project) -> ValidationResult {
    validate_edit_in(edit, project, None)
}

/// Validate a replacement: it must parse into the addressed subtree, every local
/// media path the resulting document declares must exist under `root`, and it
/// must not add a semantic error the validator would make a render refuse.
pub fn validate_edit_in(edit: &AiEdit, project: &Project, root: Option<&Path>) -> ValidationResult {
    // Validate through the same path resolver and fragment parser as application.
    // A mix/phrase/effects subtree is not a top-level project block.
    let candidate = match candidate_project(edit, project) {
        Ok(candidate) => candidate,
        Err(e) => {
            return ValidationResult {
                valid: false,
                errors: vec![e.to_string()],
                parsed_dsl: None,
            };
        }
    };
    let document = candidate_document(&candidate);
    let mut errors: Vec<String> = root
        .map(|root| {
            crate::media::missing_diagnostics(&candidate, root)
                .into_iter()
                .map(|(error, subject)| locate_message(error, subject, document.as_ref()))
                .collect()
        })
        .unwrap_or_default();
    errors.extend(introduced_errors_located(project, &candidate));
    if errors.is_empty() {
        ValidationResult {
            valid: true,
            errors,
            parsed_dsl: Some(edit.new_dsl.clone()),
        }
    } else {
        ValidationResult {
            valid: false,
            errors,
            parsed_dsl: None,
        }
    }
}

/// The project with `edit` applied, before any semantic check. A caller that
/// needs a check above this crate reads the same merge the validator read.
pub fn candidate_project(edit: &AiEdit, project: &Project) -> Result<Project, String> {
    let mut candidate = project.clone();
    ops::replace(&mut candidate, &edit.target_path, &edit.new_dsl)?;
    Ok(candidate)
}

/// Apply a validated edit to the project. Returns affected track indices.
pub fn apply_edit(project: &mut Project, edit: &AiEdit) -> Result<ops::EditResult, String> {
    apply_edit_in(project, edit, None)
}

/// Apply an edit only when the resulting document parses, its declared media
/// exists and it adds no semantic error. The caller's project is untouched on
/// failure.
pub fn apply_edit_in(
    project: &mut Project,
    edit: &AiEdit,
    root: Option<&Path>,
) -> Result<ops::EditResult, String> {
    let mut candidate = project.clone();
    let result = ops::replace(&mut candidate, &edit.target_path, &edit.new_dsl)?;
    let mut errors = root
        .map(|root| crate::media::missing(&candidate, root))
        .unwrap_or_default();
    errors.extend(introduced_errors_located(project, &candidate));
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    *project = candidate;
    Ok(result)
}

/// Semantic errors the replacement adds, worded as the validator words them. An
/// error the project already had does not block an unrelated edit.
fn candidate_document(candidate: &Project) -> Option<crate::source_map::ParsedDocument> {
    crate::parse_document(&crate::serialize(candidate), Default::default()).ok()
}

fn locate_message(
    error: String,
    subject: crate::source_map::SourceSubject,
    document: Option<&crate::source_map::ParsedDocument>,
) -> String {
    if let Some(document) = document
        && let Some(span) = document.span(subject)
        && let Ok(range) = document.utf16_range(span)
    {
        return format!(
            "{}:{}: {error}",
            range.start.line + 1,
            range.start.character + 1
        );
    }
    error
}

fn introduced_diagnostics(
    base: &Project,
    candidate: &Project,
) -> Vec<crate::diagnostics::ValidationDiagnostic> {
    let existing: std::collections::HashSet<String> = crate::validator::validate_diagnostics(base)
        .into_iter()
        .map(|diagnostic| diagnostic.error.to_string())
        .collect();
    crate::validator::validate_diagnostics(candidate)
        .into_iter()
        .filter(|diagnostic| !existing.contains(&diagnostic.error.to_string()))
        .collect()
}

fn introduced_errors_located(base: &Project, candidate: &Project) -> Vec<String> {
    let document = candidate_document(candidate);
    introduced_diagnostics(base, candidate)
        .into_iter()
        .map(|diagnostic| {
            match document
                .as_ref()
                .and_then(|document| diagnostic.locate(document))
            {
                Some((line, column)) => format!("{line}:{column}: {}", diagnostic.error),
                None => diagnostic.error.to_string(),
            }
        })
        .collect()
}

// ============================================================================
// Diff
// ============================================================================

/// Simple line-based diff between old and new DSL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslDiff {
    pub old_text: String,
    pub new_text: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One contiguous run of changed DSL lines.
pub struct DiffHunk {
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One diff line with its kind.
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Whether a diff line is unchanged, added or removed.
pub enum DiffKind {
    Same,
    Added,
    Removed,
}

/// Generate a simple diff between old and new DSL text.
pub fn diff(old: &str, new: &str) -> DslDiff {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple LCS-based diff
    let mut lines = Vec::new();
    let (mut oi, mut ni) = (0, 0);

    while oi < old_lines.len() && ni < new_lines.len() {
        if old_lines[oi] == new_lines[ni] {
            lines.push(DiffLine {
                kind: DiffKind::Same,
                text: old_lines[oi].to_string(),
            });
            oi += 1;
            ni += 1;
        } else {
            // Look ahead for a match
            let ahead_new = new_lines[ni..]
                .iter()
                .position(|l| oi < old_lines.len() && *l == old_lines[oi]);
            let ahead_old = old_lines[oi..]
                .iter()
                .position(|l| ni < new_lines.len() && *l == new_lines[ni]);

            match (ahead_new, ahead_old) {
                (Some(an), Some(ao)) if an <= ao => {
                    // New lines were added before the match
                    for line in &new_lines[ni..ni + an] {
                        lines.push(DiffLine {
                            kind: DiffKind::Added,
                            text: line.to_string(),
                        });
                    }
                    ni += an;
                }
                (_, Some(ao)) => {
                    // Old lines were removed before the match
                    for line in &old_lines[oi..oi + ao] {
                        lines.push(DiffLine {
                            kind: DiffKind::Removed,
                            text: line.to_string(),
                        });
                    }
                    oi += ao;
                }
                (Some(an), None) => {
                    for line in &new_lines[ni..ni + an] {
                        lines.push(DiffLine {
                            kind: DiffKind::Added,
                            text: line.to_string(),
                        });
                    }
                    ni += an;
                }
                (None, None) => {
                    lines.push(DiffLine {
                        kind: DiffKind::Removed,
                        text: old_lines[oi].to_string(),
                    });
                    lines.push(DiffLine {
                        kind: DiffKind::Added,
                        text: new_lines[ni].to_string(),
                    });
                    oi += 1;
                    ni += 1;
                }
            }
        }
    }

    // Remaining old lines = removed
    while oi < old_lines.len() {
        lines.push(DiffLine {
            kind: DiffKind::Removed,
            text: old_lines[oi].to_string(),
        });
        oi += 1;
    }
    // Remaining new lines = added
    while ni < new_lines.len() {
        lines.push(DiffLine {
            kind: DiffKind::Added,
            text: new_lines[ni].to_string(),
        });
        ni += 1;
    }

    DslDiff {
        old_text: old.to_string(),
        new_text: new.to_string(),
        hunks: vec![DiffHunk {
            old_start: 0,
            new_start: 0,
            lines,
        }],
    }
}

// ============================================================================
// System prompt
// ============================================================================

fn system_prompt() -> String {
    r#"You are an AI music production assistant for OpenCompose DAW.
You work with the OpenCompose DSL — a text-based music description language.

RULES:
1. Output ONLY valid OpenCompose DSL. No explanations outside DSL comments.
2. Preserve the structure: @track, @phrase, @pattern, @preset, @audio_clip, @bus, @master.
3. Every bar in 4/4 time must contain exactly 4 beats of notes/rests.
4. Use correct pitch notation: C4, Bb3, F#5 (scientific pitch, C4=middle C).
5. Use correct durations: w=4 beats, h=2, q=1, 8th=0.5, 16th=0.25.
6. Dynamics: ppp pp p mp mf f ff fff (quiet to loud). `@2.0 cresc` / `@2.0 dim`
   in a phrase swells to the next written dynamic or the phrase end.
7. Articulations: legato, staccato, accent, sustained, etc.
8. Bar lines (|) are optional separators, not required.
9. Keep the same track name and time range unless asked to change them.
10. When editing, output the COMPLETE replacement for the target — not a fragment.

DSL QUICK REFERENCE:
  @track name | instrument: category/name
  @track name | preset: preset_name
    mix: vol=-3dB, pan=0, send(reverb=25%)
    @phrase name [start_bar:beat:sub - end_bar:beat:sub]
      C4 q mf articulation=legato
      rest h
    @pattern name [start_bar:beat:sub] x4
      kick 1:0 f
      snare 2:0 f
  @preset name
    source: soundfont
    program: 0
  @audio_clip name [start_bar:beat:sub - end_bar:beat:sub] | track: track_name
    file: "media.wav"
    offset: 0s
    gain: -3dB
  @bus name
    routes: [track1, track2]
    effects:
      - reverb: decay=2s, mix=30%
  @master
    effects:
      - limiter: ceiling=-0.3dB"#
        .to_string()
}

// ============================================================================
// Helpers
// ============================================================================

fn resolve_track_from_path<'a>(
    project: &'a Project,
    path: &str,
) -> Result<(usize, &'a Track), String> {
    let ast_path = ops::AstPath::parse(path)?;
    if let Some(ops::PathSegment::Track(track_ref)) = ast_path.segments.first() {
        match track_ref {
            ops::TrackRef::Index(idx) => project
                .tracks
                .get(*idx)
                .map(|t| (*idx, t))
                .ok_or_else(|| format!("Track {} not found", idx)),
            ops::TrackRef::Name(name) => project
                .tracks
                .iter()
                .enumerate()
                .find(|(_, t)| t.name == *name)
                .ok_or_else(|| format!("Track '{}' not found", name)),
        }
    } else {
        Err("Path does not start with a track".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_properties_replace_through_the_documented_path() {
        let project = crate::parse("@project\n  tempo: 120\n").unwrap();
        let edit = AiEdit {
            new_dsl: "tempo: 132".into(),
            target_path: "project.properties".into(),
            explanation: None,
        };
        let check = validate_edit(&edit, &project);
        assert!(check.valid, "{:?}", check.errors);
        let mut applied = project.clone();
        apply_edit(&mut applied, &edit).unwrap();
        assert_eq!(applied.properties.tempo, 132.0);
    }

    #[test]
    fn a_replacement_that_parses_but_cannot_render_is_refused_before_it_is_applied() {
        let project = crate::parse(
            "@project\n  tempo: 120\n\n@track keys | instrument: piano/acoustic_grand\n",
        )
        .unwrap();
        let before = crate::serialize(&project);
        // `gain_automation` points are linear factors; dB text parses and only
        // the validator, which every render runs, refuses it.
        let edit = AiEdit {
            new_dsl: "@master\n  gain_automation: linear [1:1 - 9:1] {1:1=0dB, 9:1=0dB}".into(),
            target_path: "master".into(),
            explanation: None,
        };
        let check = validate_edit(&edit, &project);
        assert!(!check.valid, "the renderer would refuse this");
        assert!(
            check
                .errors
                .iter()
                .any(|error| error.contains("gain must be a linear numeric factor")),
            "{:?}",
            check.errors
        );
        assert!(
            check.errors.iter().any(|error| {
                error
                    .split_once(':')
                    .is_some_and(|(line, _)| line.parse::<u32>().is_ok())
            }),
            "the semantic error should carry a one-based line: {:?}",
            check.errors
        );
        let mut applied = project.clone();
        assert!(
            apply_edit(&mut applied, &edit).is_err(),
            "apply refuses too"
        );
        assert_eq!(crate::serialize(&applied), before);
    }

    #[test]
    fn an_error_the_project_already_had_does_not_block_an_unrelated_edit() {
        let sloppy = crate::parse(
            "@project\n  tempo: 120\n\n@track keys | instrument: piano/acoustic_grand\n  mix: vol=-500dB\n",
        )
        .unwrap();
        assert!(
            crate::validator::validate(&sloppy).is_err(),
            "the volume is out of range"
        );
        let edit = AiEdit {
            new_dsl: "tempo: 132".into(),
            target_path: "project.properties".into(),
            explanation: None,
        };
        let check = validate_edit(&edit, &sloppy);
        assert!(check.valid, "{:?}", check.errors);
    }
}
