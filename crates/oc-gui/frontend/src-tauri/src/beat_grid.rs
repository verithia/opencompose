//! Where the beat grid `analyze_notes` reports comes from.
//!
//! Precedence: an explicit tempo/time_signature argument, the sidecar a
//! `render_range` output carries, an explicit `project` path, then the engine
//! default. Every origin but the arguments leaves a warning naming it.

use oc_engine::note_analysis::BeatGrid;

pub(crate) struct Resolved {
    pub grid: BeatGrid,
    pub warnings: Vec<String>,
}

#[derive(Clone, PartialEq)]
enum GridOrigin {
    Argument,
    /// The sidecar a `render_range` output carries.
    Render,
    Project(String),
    Default,
}

pub(crate) fn resolve(
    path: &str,
    tempo_arg: Option<f64>,
    signature_arg: Option<String>,
    project_arg: Option<String>,
) -> Result<Resolved, String> {
    let inferred = match crate::render_grid::read(std::path::Path::new(path)) {
        Some(render) => Some((render, GridOrigin::Render)),
        None => match project_arg {
            Some(project_path) => {
                let source = std::fs::read_to_string(&project_path)
                    .map_err(|error| format!("could not read project '{project_path}': {error}"))?;
                let project = oc_dsl::parse(&source).map_err(|error| {
                    format!("could not parse project '{project_path}': {error}")
                })?;
                Some((
                    crate::render_grid::RenderGrid::from_project(&project),
                    GridOrigin::Project(project_path),
                ))
            }
            None => None,
        },
    };
    let inferred_tempo = inferred.as_ref().map(|(render, _)| render.tempo_bpm);
    let inferred_signature = inferred
        .as_ref()
        .map(|(render, _)| parse_time_signature(&render.time_signature))
        .transpose()?;
    let default = oc_engine::note_analysis::BeatGrid::default();
    let (tempo, tempo_origin) = match tempo_arg {
        Some(tempo) => (tempo, GridOrigin::Argument),
        None => match inferred_tempo {
            Some(tempo) => (tempo, inferred_origin(&inferred)),
            None => (default.tempo_bpm, GridOrigin::Default),
        },
    };
    let (numerator, denominator, signature_origin) = match signature_arg {
        Some(text) => {
            let (numerator, denominator) = parse_time_signature(&text)?;
            (numerator, denominator, GridOrigin::Argument)
        }
        None => match inferred_signature {
            Some((numerator, denominator)) => (numerator, denominator, inferred_origin(&inferred)),
            None => (default.numerator, default.denominator, GridOrigin::Default),
        },
    };
    let grid = BeatGrid {
        tempo_bpm: tempo,
        numerator,
        denominator,
    };
    grid.validate()?;

    let mut warnings = Vec::new();
    match (&tempo_origin, &signature_origin) {
        (GridOrigin::Argument, GridOrigin::Argument) => {}
        (GridOrigin::Default, GridOrigin::Default) => warnings.push(format!(
            "no tempo or time signature was given and '{path}' has no render metadata: assuming \
             the engine default {} BPM {}; pass tempo, time_signature or project so the grid \
             matches the audio",
            tempo,
            grid.time_signature()
        )),
        (left, right) if left == right => {
            let source = match left {
                GridOrigin::Render => "the render metadata written by render_range".to_owned(),
                GridOrigin::Project(path) => format!("the project '{path}'"),
                _ => unreachable!(),
            };
            warnings.push(format!(
                "the beat grid ({} BPM, {}) came from {source}; pass tempo and time_signature to \
                 override",
                tempo,
                grid.time_signature()
            ));
        }
        _ => {
            if tempo_origin != GridOrigin::Argument {
                warnings.push(grid_assumption(
                    "tempo",
                    &format!("{tempo} BPM"),
                    &tempo_origin,
                ));
            }
            if signature_origin != GridOrigin::Argument {
                warnings.push(grid_assumption(
                    "time signature",
                    &grid.time_signature(),
                    &signature_origin,
                ));
            }
        }
    }
    if tempo_origin == GridOrigin::Default
        && let Some(assumption) = warnings.first_mut()
    {
        assumption.push_str(
            ". For pulse evidence, call audio_analyze with a beat_grid passage and confirm the meter and downbeat before applying a grid.",
        );
    }
    if let Some((render, source)) = &inferred
        && (render.tempo_changes > 0 || render.meter_changes > 0)
    {
        warnings.push(format!(
            "{} has {} tempo change(s) and {} time-signature change(s); the beat grid is constant, \
             so beats and bars are exact only while the opening grid holds",
            match source {
                GridOrigin::Render => "the render's project".to_owned(),
                GridOrigin::Project(path) => format!("the project '{path}'"),
                _ => unreachable!(),
            },
            render.tempo_changes,
            render.meter_changes
        ));
    }
    Ok(Resolved { grid, warnings })
}

fn inferred_origin(inferred: &Option<(crate::render_grid::RenderGrid, GridOrigin)>) -> GridOrigin {
    inferred
        .as_ref()
        .map(|(_, origin)| origin.clone())
        .unwrap_or(GridOrigin::Default)
}

fn grid_assumption(field: &str, value: &str, origin: &GridOrigin) -> String {
    let argument = field.replace(' ', "_");
    match origin {
        GridOrigin::Render => format!(
            "the {field} {value} came from the render metadata written by render_range; pass \
             {argument} to override"
        ),
        GridOrigin::Project(path) => format!(
            "the {field} {value} came from the project '{path}'; pass {argument} to override"
        ),
        GridOrigin::Default => format!(
            "the {field} was assumed to be {value} (the engine default; no {argument} argument and \
             no render metadata); pass {argument} to override"
        ),
        GridOrigin::Argument => unreachable!(),
    }
}

/// `4/4`, `3/4`, `6/8` into a meter the engine clock accepts.
fn parse_time_signature(text: &str) -> Result<(u8, u8), String> {
    let (numerator, denominator) = text
        .split_once('/')
        .ok_or_else(|| format!("time_signature must look like \"4/4\", got '{text}'"))?;
    let numerator = numerator
        .trim()
        .parse::<u8>()
        .map_err(|_| format!("time_signature numerator must be a number, got '{text}'"))?;
    let denominator = denominator
        .trim()
        .parse::<u8>()
        .map_err(|_| format!("time_signature denominator must be a number, got '{text}'"))?;
    Ok((numerator, denominator))
}
