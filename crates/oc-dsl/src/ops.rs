//! AST operations — address, extract, replace subtrees.
//!
//! Used by the AI tool to read/write specific parts of the project
//! without touching the rest.
//!
//! ```text
//! AstPath examples:
//!   "project"                    → entire project
//!   "project.properties"         → tempo, key, time_sig
//!   "track:0"                    → first track (all of it)
//!   "track:drums"                → track by name
//!   "track:0.phrase:0"           → first phrase of first track
//!   "track:0.phrase:intro"       → phrase by name
//!   "track:0.mix"                → track mix state
//!   "track:0.effects"            → track effect chain
//!   "bus:hall_reverb"            → bus by name
//!   "master"                     → master section
//!   "preset:warm_pad"            → preset by name
//! ```

use crate::ast::*;
use crate::instruments;
use crate::parser;
use crate::serializer;
use serde::{Deserialize, Serialize};

/// A path into the AST tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstPath {
    pub segments: Vec<PathSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One segment of an operation path.
pub enum PathSegment {
    Project,
    Properties,
    Track(TrackRef),
    Phrase(ItemRef),
    Pattern(ItemRef),
    Mix,
    Effects,
    Automation,
    Bus(String),
    Master,
    Preset(String),
    Sections,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Index or name reference to a track.
pub enum TrackRef {
    Index(usize),
    Name(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Index or name reference to an item.
pub enum ItemRef {
    Index(usize),
    Name(String),
}

/// Result of extracting a subtree — the DSL text for that node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtreeExtract {
    pub path: String,
    pub dsl_text: String,
    /// Which track indices are covered by this subtree (for dirty tracking).
    pub affected_tracks: Vec<usize>,
}

/// Result of applying an edit — what changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResult {
    /// Track indices that were modified (for incremental re-render).
    pub affected_tracks: Vec<usize>,
    /// Whether project properties changed (tempo, key, etc.).
    pub properties_changed: bool,
    /// Whether bus/master routing changed.
    pub routing_changed: bool,
    /// The full updated DSL text.
    pub dsl_text: String,
}

impl AstPath {
    /// Parse a path string like "track:0.phrase:1" into segments.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        for part in s.split('.') {
            let seg = if part == "project" {
                PathSegment::Project
            } else if part == "properties" {
                PathSegment::Properties
            } else if part == "master" {
                PathSegment::Master
            } else if part == "mix" {
                PathSegment::Mix
            } else if part == "effects" {
                PathSegment::Effects
            } else if part == "automation" {
                PathSegment::Automation
            } else if part == "sections" {
                PathSegment::Sections
            } else if let Some(rest) = part.strip_prefix("track:") {
                if let Ok(idx) = rest.parse::<usize>() {
                    PathSegment::Track(TrackRef::Index(idx))
                } else {
                    PathSegment::Track(TrackRef::Name(rest.to_string()))
                }
            } else if let Some(rest) = part.strip_prefix("phrase:") {
                if let Ok(idx) = rest.parse::<usize>() {
                    PathSegment::Phrase(ItemRef::Index(idx))
                } else {
                    PathSegment::Phrase(ItemRef::Name(rest.to_string()))
                }
            } else if let Some(rest) = part.strip_prefix("pattern:") {
                if let Ok(idx) = rest.parse::<usize>() {
                    PathSegment::Pattern(ItemRef::Index(idx))
                } else {
                    PathSegment::Pattern(ItemRef::Name(rest.to_string()))
                }
            } else if let Some(rest) = part.strip_prefix("bus:") {
                PathSegment::Bus(rest.to_string())
            } else if let Some(rest) = part.strip_prefix("preset:") {
                PathSegment::Preset(rest.to_string())
            } else {
                return Err(format!("Unknown path segment: {}", part));
            };
            segments.push(seg);
        }
        if segments.is_empty() {
            return Err("Empty path".into());
        }
        Ok(Self { segments })
    }
}

// ============================================================================
// Extract subtree
// ============================================================================

/// Extract a subtree from the project as DSL text.
pub fn extract(project: &Project, path: &str) -> Result<SubtreeExtract, String> {
    let ast_path = AstPath::parse(path)?;
    let (dsl_text, affected) = extract_node(project, &ast_path.segments)?;

    Ok(SubtreeExtract {
        path: path.to_string(),
        dsl_text,
        affected_tracks: affected,
    })
}

fn extract_node(
    project: &Project,
    segments: &[PathSegment],
) -> Result<(String, Vec<usize>), String> {
    if segments.is_empty() {
        return Err("Empty path".into());
    }

    match &segments[0] {
        PathSegment::Project => {
            if segments.len() == 1 {
                let all_tracks: Vec<usize> = (0..project.tracks.len()).collect();
                Ok((serializer::serialize(project), all_tracks))
            } else {
                extract_node(project, &segments[1..])
            }
        }
        PathSegment::Properties => {
            let mut out = String::new();
            out.push_str(&format!("tempo: {}\n", project.properties.tempo));
            out.push_str(&format!(
                "time_signature: {}/{}\n",
                project.properties.time_signature.numerator,
                project.properties.time_signature.denominator
            ));
            if let Some(key) = &project.properties.key {
                out.push_str(&format!("key: {:?} {:?}\n", key.root, key.mode));
            }
            Ok((out, vec![]))
        }
        PathSegment::Track(track_ref) => {
            let (idx, track) = resolve_track(project, track_ref)?;
            if segments.len() == 1 {
                // Serialize just this track
                let mut out = String::new();
                serializer::serialize_track_to(track, &mut out);
                Ok((out, vec![idx]))
            } else {
                extract_track_child(track, idx, &segments[1..])
            }
        }
        PathSegment::Pattern(item_ref) => {
            let idx = resolve_project_pattern_idx(project, item_ref)?;
            let pattern = &project.patterns[idx];
            let mut out = String::new();
            serializer::serialize_pattern_def_to(pattern, &mut out);
            Ok((out, clips_of_pattern(project, &pattern.name)))
        }
        PathSegment::Bus(name) => {
            let bus = project
                .buses
                .iter()
                .find(|b| &b.name == name)
                .ok_or_else(|| format!("Bus '{}' not found", name))?;
            let mut out = String::new();
            serializer::serialize_bus_to(bus, &mut out);
            Ok((out, vec![]))
        }
        PathSegment::Master => {
            if let Some(master) = &project.master {
                let mut out = String::new();
                serializer::serialize_master_to(master, &mut out);
                Ok((out, vec![]))
            } else {
                Ok(("// No master section\n".into(), vec![]))
            }
        }
        PathSegment::Preset(name) => {
            let preset = project
                .presets
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| format!("Preset '{}' not found", name))?;
            let mut out = String::new();
            serializer::serialize_preset_to(preset, &mut out);
            Ok((out, vec![]))
        }
        PathSegment::Sections => {
            let mut out = String::new();
            for section in &project.sections {
                let mut s = String::new();
                serializer::serialize_section(section, &mut s);
                out.push_str(&s);
                out.push('\n');
            }
            Ok((out, vec![]))
        }
        _ => Err("Invalid top-level path segment".to_string()),
    }
}

fn extract_track_child(
    track: &Track,
    track_idx: usize,
    segments: &[PathSegment],
) -> Result<(String, Vec<usize>), String> {
    match &segments[0] {
        PathSegment::Phrase(item_ref) => {
            let phrase = resolve_phrase(track, item_ref)?;
            let mut out = String::new();
            serializer::serialize_phrase_to(phrase, &mut out);
            Ok((out, vec![track_idx]))
        }
        PathSegment::Pattern(item_ref) => {
            let pattern = resolve_pattern(track, item_ref)?;
            let mut out = String::new();
            serializer::serialize_pattern_to(pattern, &mut out);
            Ok((out, vec![track_idx]))
        }
        PathSegment::Mix => {
            let mix = &track.mix;
            let mut fields = vec![
                format!("vol={}dB", mix.volume_db),
                format!("pan={}", mix.pan),
            ];
            if let Some(output) = &mix.output {
                fields.push(format!("output={output}"));
            }
            for send in &mix.sends {
                fields.push(format!(
                    "send({}{}={}%{})",
                    send.source
                        .as_ref()
                        .map(|source| format!("{}.out{}->", source.owner, source.output))
                        .unwrap_or_default(),
                    send.target,
                    send.amount * 100.0,
                    if send.pre_fader { ", pre" } else { "" }
                ));
            }
            fields.push(format!("mute={}", mix.mute));
            fields.push(format!("solo={}", mix.solo));
            if mix.polarity_inverted {
                fields.push("polarity=invert".into());
            }
            if !mix.latency_compensation.is_on() {
                fields.push(format!("pdc={}", mix.latency_compensation.as_str()));
            }
            if mix.latency_offset != 0 {
                fields.push(format!("latency={}", mix.latency_offset));
            }
            if let Some(display_name) = &mix.overrides.display_name {
                fields.push(format!("display_name=\"{display_name}\""));
            }
            if let Some(color) = &mix.overrides.color {
                fields.push(format!("color=\"{color}\""));
            }
            if mix.overrides.solo_safe {
                fields.push("solo_safe=true".into());
            }
            Ok((format!("mix: {}\n", fields.join(", ")), vec![track_idx]))
        }
        PathSegment::Effects => {
            let mut out = String::new();
            if !track.effects.is_empty() {
                serializer::serialize_effects_to(&track.effects, &mut out);
            }
            Ok((out, vec![track_idx]))
        }
        PathSegment::Automation => {
            let mut out = String::new();
            for lane in track.automation() {
                serializer::serialize_automation_to(lane, &mut out);
            }
            Ok((out, vec![track_idx]))
        }
        _ => Err("Invalid track child path".into()),
    }
}

// ============================================================================
// Replace subtree
// ============================================================================

/// Replace a subtree in the project with new DSL text.
/// Returns the modified project + info about what changed.
pub fn replace(project: &mut Project, path: &str, new_dsl: &str) -> Result<EditResult, String> {
    let ast_path = AstPath::parse(path)?;
    let affected = replace_node(project, &ast_path.segments, new_dsl)?;
    // A replacement track can name only an instrument family; give it the same
    // default the parser would, on the full project so the preset names and the
    // preset list stay in one place.
    instruments::apply_defaults(project);

    let dsl_text = serializer::serialize(project);

    Ok(EditResult {
        affected_tracks: affected.tracks,
        properties_changed: affected.properties,
        routing_changed: affected.routing,
        dsl_text,
    })
}

struct AffectedInfo {
    tracks: Vec<usize>,
    properties: bool,
    routing: bool,
}

fn replace_node(
    project: &mut Project,
    segments: &[PathSegment],
    new_dsl: &str,
) -> Result<AffectedInfo, String> {
    match &segments[0] {
        PathSegment::Project => {
            if segments.len() > 1 {
                return replace_node(project, &segments[1..], new_dsl);
            }
            let parsed = parser::parse(new_dsl).map_err(|e| format!("Parse error: {}", e))?;
            if parsed.tracks.is_empty() {
                return Err("New DSL contains no track".into());
            }
            let affected: Vec<usize> = (0..parsed.tracks.len()).collect();
            *project = parsed;
            Ok(AffectedInfo {
                tracks: affected,
                properties: true,
                routing: true,
            })
        }
        PathSegment::Pattern(item_ref) => {
            let wrapper = format!(
                "@project\n  tempo: {}\n  time_signature: {}/{}\n\n{}",
                project.properties.tempo,
                project.properties.time_signature.numerator,
                project.properties.time_signature.denominator,
                new_dsl
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            let pattern = parsed
                .patterns
                .into_iter()
                .next()
                .ok_or("New DSL contains no pattern")?;
            let name = pattern.name.clone();
            match item_ref {
                ItemRef::Index(idx) => {
                    if *idx >= project.patterns.len() {
                        return Err(format!("Pattern index {} out of range", idx));
                    }
                    project.patterns[*idx] = pattern;
                }
                ItemRef::Name(existing) => {
                    match project.patterns.iter().position(|p| p.name == *existing) {
                        Some(idx) => project.patterns[idx] = pattern,
                        None => project.patterns.push(pattern),
                    }
                }
            }
            Ok(AffectedInfo {
                tracks: clips_of_pattern(project, &name),
                properties: false,
                routing: false,
            })
        }
        PathSegment::Sections => {
            let wrapper = format!(
                "@project\n  tempo: {}\n  time_signature: {}/{}\n\n{}",
                project.properties.tempo,
                project.properties.time_signature.numerator,
                project.properties.time_signature.denominator,
                new_dsl
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            if parsed.sections.is_empty() {
                return Err("New DSL contains no section".into());
            }
            project.sections = parsed.sections;
            Ok(AffectedInfo {
                tracks: (0..project.tracks.len()).collect(),
                properties: true,
                routing: false,
            })
        }
        PathSegment::Track(track_ref) => {
            let (idx, _) = resolve_track(project, track_ref)?;

            if segments.len() == 1 {
                // Replace entire track — parse new_dsl as a full project, take the first track
                let wrapper = format!(
                    "@project\n  tempo: {}\n  time_signature: {}/{}\n\n{}",
                    project.properties.tempo,
                    project.properties.time_signature.numerator,
                    project.properties.time_signature.denominator,
                    new_dsl
                );
                let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
                if parsed.tracks.is_empty() {
                    return Err("New DSL contains no track".into());
                }
                project.tracks[idx] = parsed.tracks.into_iter().next().unwrap();
                Ok(AffectedInfo {
                    tracks: vec![idx],
                    properties: false,
                    routing: false,
                })
            } else {
                replace_track_child(project, idx, &segments[1..], new_dsl)
            }
        }
        PathSegment::Preset(name) => {
            // Parse as a full project, extract the first preset
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n{}",
                new_dsl
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            if let Some(new_preset) = parsed.presets.into_iter().next() {
                if let Some(existing) = project.presets.iter_mut().find(|p| p.name == *name) {
                    *existing = new_preset;
                } else {
                    project.presets.push(new_preset);
                }
            }
            Ok(AffectedInfo {
                tracks: vec![],
                properties: false,
                routing: false,
            })
        }
        PathSegment::Bus(name) => {
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n{}",
                new_dsl
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            if let Some(new_bus) = parsed.buses.into_iter().next() {
                if let Some(existing) = project.buses.iter_mut().find(|b| b.name == *name) {
                    *existing = new_bus;
                } else {
                    project.buses.push(new_bus);
                }
            }
            Ok(AffectedInfo {
                tracks: vec![],
                properties: false,
                routing: true,
            })
        }
        PathSegment::Master => {
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n{}",
                new_dsl
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            project.master = parsed.master;
            Ok(AffectedInfo {
                tracks: vec![],
                properties: false,
                routing: true,
            })
        }
        PathSegment::Properties => {
            // Parse properties from key: value lines
            let wrapper = format!("@project\n  {}\n", new_dsl.replace('\n', "\n  "));
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            project.properties = parsed.properties;
            let all: Vec<usize> = (0..project.tracks.len()).collect();
            Ok(AffectedInfo {
                tracks: all,
                properties: true,
                routing: false,
            })
        }
        _ => Err("Unsupported replace path".into()),
    }
}

fn replace_track_child(
    project: &mut Project,
    track_idx: usize,
    segments: &[PathSegment],
    new_dsl: &str,
) -> Result<AffectedInfo, String> {
    let track = &mut project.tracks[track_idx];

    match &segments[0] {
        PathSegment::Phrase(item_ref) => {
            let phrase_idx = resolve_phrase_idx(track, item_ref)?;
            // Parse phrase from DSL — wrap in a minimal track context
            let wrapper = format!(
                "@project\n  tempo: {}\n  time_signature: {}/{}\n\n@track _tmp | instrument: piano/acoustic_grand\n  {}",
                project.properties.tempo,
                project.properties.time_signature.numerator,
                project.properties.time_signature.denominator,
                new_dsl.replace('\n', "\n  ")
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            let new_phrase = parsed
                .tracks
                .into_iter()
                .next()
                .and_then(|t| t.phrases.into_iter().next())
                .ok_or("New DSL contains no phrase")?;
            track.phrases[phrase_idx] = new_phrase;
            Ok(AffectedInfo {
                tracks: vec![track_idx],
                properties: false,
                routing: false,
            })
        }
        PathSegment::Mix => {
            // Parse mix line
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track _tmp | instrument: piano/acoustic_grand\n  {}",
                new_dsl.trim()
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            if let Some(t) = parsed.tracks.into_iter().next() {
                track.mix = t.mix;
            }
            Ok(AffectedInfo {
                tracks: vec![track_idx],
                properties: false,
                routing: false,
            })
        }
        PathSegment::Effects => {
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track _tmp | instrument: piano/acoustic_grand\n  {}",
                new_dsl.replace('\n', "\n  ")
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            if let Some(t) = parsed.tracks.into_iter().next() {
                track.effects = t.effects;
            }
            Ok(AffectedInfo {
                tracks: vec![track_idx],
                properties: false,
                routing: false,
            })
        }
        PathSegment::Automation => {
            let wrapper = format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track _tmp | instrument: piano/acoustic_grand\n  {}",
                new_dsl.replace('\n', "\n  ")
            );
            let parsed = parser::parse(&wrapper).map_err(|e| format!("Parse error: {}", e))?;
            let lanes = parsed
                .tracks
                .into_iter()
                .next()
                .map(|t| t.automation().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            track.set_automation_lanes(lanes);
            Ok(AffectedInfo {
                tracks: vec![track_idx],
                properties: false,
                routing: false,
            })
        }
        _ => Err("Invalid track child for replace".into()),
    }
}

// ============================================================================
// Resolve helpers
// ============================================================================

fn resolve_track<'a>(
    project: &'a Project,
    track_ref: &TrackRef,
) -> Result<(usize, &'a Track), String> {
    match track_ref {
        TrackRef::Index(idx) => project
            .tracks
            .get(*idx)
            .map(|t| (*idx, t))
            .ok_or_else(|| format!("Track index {} out of range", idx)),
        TrackRef::Name(name) => project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| t.name == *name)
            .ok_or_else(|| format!("Track '{}' not found", name)),
    }
}

fn resolve_phrase<'a>(track: &'a Track, item_ref: &ItemRef) -> Result<&'a Phrase, String> {
    match item_ref {
        ItemRef::Index(idx) => track
            .phrases
            .get(*idx)
            .ok_or_else(|| format!("Phrase index {} out of range", idx)),
        ItemRef::Name(name) => track
            .phrases
            .iter()
            .find(|p| p.name == *name)
            .ok_or_else(|| format!("Phrase '{}' not found", name)),
    }
}

fn resolve_phrase_idx(track: &Track, item_ref: &ItemRef) -> Result<usize, String> {
    match item_ref {
        ItemRef::Index(idx) => {
            if *idx < track.phrases.len() {
                Ok(*idx)
            } else {
                Err(format!("Phrase index {} out of range", idx))
            }
        }
        ItemRef::Name(name) => track
            .phrases
            .iter()
            .position(|p| p.name == *name)
            .ok_or_else(|| format!("Phrase '{}' not found", name)),
    }
}

fn resolve_pattern<'a>(track: &'a Track, item_ref: &ItemRef) -> Result<&'a Pattern, String> {
    match item_ref {
        ItemRef::Index(idx) => track
            .patterns
            .get(*idx)
            .ok_or_else(|| format!("Pattern index {} out of range", idx)),
        ItemRef::Name(name) => track
            .patterns
            .iter()
            .find(|p| p.name == *name)
            .ok_or_else(|| format!("Pattern '{}' not found", name)),
    }
}

fn resolve_project_pattern_idx(project: &Project, item_ref: &ItemRef) -> Result<usize, String> {
    match item_ref {
        ItemRef::Index(idx) => {
            if *idx < project.patterns.len() {
                Ok(*idx)
            } else {
                Err(format!("Pattern index {} out of range", idx))
            }
        }
        ItemRef::Name(name) => project
            .patterns
            .iter()
            .position(|p| p.name == *name)
            .ok_or_else(|| format!("Pattern '{}' not found", name)),
    }
}

/// Tracks whose clips place the named top-level pattern.
fn clips_of_pattern(project: &Project, name: &str) -> Vec<usize> {
    project
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track.pattern_clips().any(|clip| clip.pattern == name))
        .map(|(idx, _)| idx)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"@project
  tempo: 120
  time_signature: 4/4

@pattern beat | instrument: drums/acoustic_kit
  hits:
    kick 1:0 f
    snare 1:2 mf

@track drums | instrument: drums/acoustic_kit
  clip: beat [1:1 - 3:1]
  automation: brightness {1:1=20%, 2:1=50%}

@track lead | instrument: synth/lead_saw
  @phrase main [1:1 - 2:1]
    C4 q mf

@bus drums_bus
  gain_automation: linear [1:1 - 5:1] {1:1=1, 5:1=0.5}

@section verse [1:1 - 9:1]
  repeat: 2
"#;

    fn project() -> Project {
        parser::parse(FIXTURE).expect("the fixture parses")
    }

    #[test]
    fn a_top_level_pattern_reads_back_as_its_own_block() {
        let read = extract(&project(), "pattern:beat").expect("the pattern extracts");
        assert!(read.dsl_text.contains("@pattern beat"), "{}", read.dsl_text);
        assert!(read.dsl_text.contains("kick"), "{}", read.dsl_text);
        // The clip on `drums` places it, so that track is dirty.
        assert_eq!(read.affected_tracks, vec![0]);
        assert!(extract(&project(), "pattern:missing").is_err());
    }

    #[test]
    fn a_whole_project_replacement_swaps_the_document() {
        let mut project = project();
        let replacement = FIXTURE
            .replace("swing_missing", "")
            .replace(
                "  time_signature: 4/4\n",
                "  time_signature: 4/4\n  swing: 55%\n",
            )
            .replace(
                "@section verse [1:1 - 9:1]",
                "@pattern extra | instrument: synth/lead_saw\n  events:\n    E4 8th mf\n\n@section verse [1:1 - 9:1]",
            );
        let result = replace(&mut project, "project", &replacement).expect("the document replaces");
        assert_eq!(result.affected_tracks, vec![0, 1]);
        assert!(result.properties_changed && result.routing_changed);
        assert_eq!(project.properties.swing, Some(55.0));
        assert_eq!(project.patterns.len(), 2);
        // The clip still points at the pattern the document kept.
        assert_eq!(project.tracks[0].pattern_clip(0).unwrap().pattern, "beat");
        assert_eq!(
            extract(&project, "project")
                .expect("it reads back")
                .dsl_text
                .trim(),
            serializer::serialize(&project).trim()
        );
    }

    #[test]
    fn a_refused_replacement_leaves_the_project_untouched() {
        let mut project = project();
        let before = serializer::serialize(&project);
        for (path, dsl) in [
            ("project", "this is not DSL"),
            ("project", "@project\n  tempo: 120\n  time_signature: 4/4\n"),
            (
                "pattern:beat",
                "@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick not-a-position f\n",
            ),
            ("sections", ""),
            ("track:0.automation", "automation: volume {1:1=}"),
        ] {
            let error = replace(&mut project, path, dsl).expect_err(path);
            assert!(!error.is_empty(), "{path}");
            assert_eq!(
                serializer::serialize(&project),
                before,
                "{path} touched the project"
            );
        }
    }

    #[test]
    fn a_pattern_is_replaced_by_name_and_created_when_absent() {
        let mut project = project();
        replace(
            &mut project,
            "pattern:beat",
            "@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick 1:0 ff\n",
        )
        .expect("the existing pattern is replaced");
        assert_eq!(project.patterns.len(), 1);
        assert_eq!(project.patterns[0].hits.len(), 1);

        replace(
            &mut project,
            "pattern:fill",
            "@pattern fill | instrument: drums/acoustic_kit\n  hits:\n    snare 1:0 f\n",
        )
        .expect("a new pattern is created");
        assert_eq!(project.patterns.len(), 2);
        assert_eq!(project.patterns[1].name, "fill");
        // The new pattern is only dirty where a clip places it, which is nowhere yet.
        assert!(replace(&mut project, "pattern:7", "").is_err());
    }

    #[test]
    fn sections_and_track_automation_are_replaceable() {
        let mut project = project();
        replace(
            &mut project,
            "sections",
            "@section intro [1:1 - 5:1]\n  repeat: 1\n\n@section verse [5:1 - 9:1]\n  repeat: 2\n",
        )
        .expect("the section list is replaced");
        assert_eq!(project.sections.len(), 2);
        assert_eq!(project.sections[1].name, "verse");

        replace(
            &mut project,
            "track:lead.automation",
            "automation: brightness {1:1=0%, 2:1=100%}\n",
        )
        .expect("the automation lanes are replaced");
        assert_eq!(project.tracks[1].automation_count(), 1);
        // The other track keeps its own lane.
        assert_eq!(project.tracks[0].automation_count(), 1);
    }

    #[test]
    fn extract_then_replace_round_trips_every_new_path() {
        let project = project();
        let mut next = project.clone();
        for path in [
            "pattern:beat",
            "sections",
            "track:drums.automation",
            "project",
        ] {
            let text = extract(&project, path).expect(path).dsl_text;
            replace(&mut next, path, &text).expect(path);
        }
        let mut expected = project.clone();
        instruments::apply_defaults(&mut expected);
        assert_eq!(
            serializer::serialize(&next),
            serializer::serialize(&expected)
        );
    }
}
