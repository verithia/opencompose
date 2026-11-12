//! Named tool sets: the switchboard over the panel's tool registry.
//!
//! A set is a list of tool names. A conversation offers the union of its
//! selected sets, so deselecting a set removes its tools from both the
//! advertised list and the request. The bundled sets partition the whole panel
//! surface, so every panel tool stays reachable; the user file
//! `~/.opencompose/ai/toolsets.json` adds sets of its own and may not redefine
//! a bundled id. A broken file never blocks the panel: the bundled sets are
//! used and the problem is logged.
use crate::private_fs;
use crate::tool_registry::{self, ToolId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// User toolset file name.
pub const FILE_NAME: &str = "toolsets.json";
const VERSION: u32 = 1;
const MAX_ID_CHARS: usize = 64;
const MAX_TITLE_CHARS: usize = 80;
const MAX_TOOLS_PER_SET: usize = 200;

/// One switch on the panel's board: a named list of tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolset {
    pub id: String,
    pub title: String,
    pub description: String,
    pub tools: Vec<String>,
    #[serde(default)]
    pub default_on: bool,
}

/// (id, title, description, tools, default_on)
const BUNDLED: &[(&str, &str, &str, &[ToolId], bool)] = &[
    (
        "dsl-edit",
        "DSL edit",
        "Read a subtree and propose one whole-subtree replacement",
        &[
            ToolId::ProjectGet,
            ToolId::PictureTimecode,
            ToolId::ProjectDsl,
            ToolId::ProjectExtract,
            ToolId::ProjectValidate,
            ToolId::ProjectDiff,
            ToolId::ProjectContext,
            ToolId::ProjectGenerateContext,
            ToolId::EditSubtree,
            ToolId::ListSkills,
            ToolId::ReadSkill,
        ],
        true,
    ),
    (
        "project-edit",
        "Project edit",
        "Tracks, sections, presets, effects, buses, clock maps, MIDI handoff, undo and transactions",
        &[
            ToolId::TracksList,
            ToolId::AudioClipsList,
            ToolId::AudioClipTransients,
            ToolId::AudioClipSetPlayback,
            ToolId::AudioClipSetPlacement,
            ToolId::AudioClipEditRange,
            ToolId::ImportAlignedStems,
            ToolId::MidiImportHandoff,
            ToolId::MidiExportHandoff,
            ToolId::TracksAdd,
            ToolId::TracksCreate,
            ToolId::TracksRemove,
            ToolId::TracksDuplicate,
            ToolId::TracksRename,
            ToolId::TracksMix,
            ToolId::TracksMute,
            ToolId::TracksSolo,
            ToolId::TracksPreset,
            ToolId::TracksOutput,
            ToolId::TracksSendAdd,
            ToolId::TracksSendRemove,
            ToolId::SectionsList,
            ToolId::SectionsAdd,
            ToolId::SectionsSet,
            ToolId::SectionsRemove,
            ToolId::PresetsList,
            ToolId::PresetBundleExport,
            ToolId::PresetBundleImport,
            ToolId::PresetsAdd,
            ToolId::PresetsRemove,
            ToolId::PresetsRename,
            ToolId::PresetsDuplicate,
            ToolId::PresetsSetParam,
            ToolId::PresetsSetParams,
            ToolId::DevicePresetsList,
            ToolId::DevicePresetApply,
            ToolId::EffectsAdd,
            ToolId::EffectsRemove,
            ToolId::EffectsSetBypass,
            ToolId::EffectsSetParam,
            ToolId::BusesAdd,
            ToolId::BusesRoutes,
            ToolId::BusesRemove,
            ToolId::BusesMix,
            ToolId::ProjectSetTempo,
            ToolId::ProjectSetName,
            ToolId::PictureClockSet,
            ToolId::TempoMapSet,
            ToolId::TimeSignatureSet,
            ToolId::Undo,
            ToolId::Redo,
            ToolId::UndoState,
            ToolId::UndoHistory,
            ToolId::TransactionBegin,
            ToolId::TransactionCommit,
            ToolId::TransactionRollback,
        ],
        true,
    ),
    (
        "ears",
        "Ears",
        "Measure and split a rendered part before transcribing it: audition, analyse, compare, and separate stems",
        &[
            ToolId::RenderRange,
            ToolId::AuditionRange,
            ToolId::AuditionClipSource,
            ToolId::AudioAnalyze,
            ToolId::AnalyzeNotes,
            ToolId::SeparateStems,
            ToolId::AudioCompare,
            ToolId::MeterSnapshot,
        ],
        true,
    ),
    (
        "listen",
        "Render compatibility",
        "Check plugin and feature compatibility for the current project",
        &[ToolId::RenderCompatReport],
        true,
    ),
    (
        "plugins",
        "Plugins",
        "Search the plugin catalog, list downloadable instrument packs, read parameters, list content libraries, browse and select plugin programs, and read or apply plugin state",
        &[
            ToolId::PluginSearch,
            ToolId::PluginPull,
            ToolId::SoundfontsList,
            ToolId::PluginsParams,
            ToolId::PluginsProgramCapabilities,
            ToolId::PluginsPrograms,
            ToolId::PluginsSelectProgram,
            ToolId::PluginsReadState,
            ToolId::PluginsApplyState,
            ToolId::ContentLibraries,
            ToolId::DevicesCatalog,
        ],
        true,
    ),
    (
        "samples",
        "Samples",
        "Search free sample libraries by kind and pull a sample into the project with its licence",
        &[ToolId::SampleSearch, ToolId::SamplePull],
        true,
    ),
    (
        "conversations",
        "Saved conversations",
        "List, open, fork, rename and compact this workspace's saved conversations",
        &[
            ToolId::SessionsList,
            ToolId::SessionsLoad,
            ToolId::SessionsNew,
            ToolId::SessionsRename,
            ToolId::SessionsCompact,
        ],
        true,
    ),
    (
        "transport",
        "Transport",
        "Move and query the playhead",
        &[
            ToolId::TransportPlay,
            ToolId::TransportPause,
            ToolId::TransportStop,
            ToolId::TransportSeek,
            ToolId::TransportState,
        ],
        false,
    ),
];

/// Built-in toolset definitions.
pub fn bundled() -> Vec<Toolset> {
    BUNDLED
        .iter()
        .map(|(id, title, description, tools, default_on)| Toolset {
            id: (*id).to_owned(),
            title: (*title).to_owned(),
            description: (*description).to_owned(),
            tools: tools.iter().map(|tool| tool.name().to_owned()).collect(),
            default_on: *default_on,
        })
        .collect()
}

/// The whole file. `version` is required to be current; sets are optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsetFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sets: Vec<Toolset>,
}

fn default_version() -> u32 {
    VERSION
}

/// User toolset file path.
pub fn path() -> PathBuf {
    private_fs::config_dir().join("ai").join(FILE_NAME)
}

/// Parses the file. Field errors are reported before anything is used.
pub fn parse(text: &str) -> Result<ToolsetFile, String> {
    let file: ToolsetFile =
        serde_json::from_str(text).map_err(|error| format!("{FILE_NAME} is not valid: {error}"))?;
    if file.version != VERSION {
        return Err(format!(
            "{FILE_NAME}: unsupported version {}; expected {VERSION}",
            file.version
        ));
    }
    Ok(file)
}

fn valid_key(kind: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{FILE_NAME}: a {kind} is empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "{FILE_NAME}: {kind} '{value}' is longer than {max_chars} characters"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!(
            "{FILE_NAME}: {kind} '{value}' has a control character"
        ));
    }
    Ok(())
}

/// Bundled sets plus every user set this file can add. A user set is dropped,
/// with a warning, when its id or title is unusable, when it redefines a
/// bundled id, when it repeats another user set, or when none of its tool names
/// exist. Unknown tool names are dropped individually, so one stale name cannot
/// hide the rest of the set.
pub fn accept(file: ToolsetFile) -> (Vec<Toolset>, Vec<String>) {
    let mut sets = bundled();
    let mut warnings = Vec::new();
    let mut seen: BTreeSet<String> = sets.iter().map(|set| set.id.clone()).collect();
    for mut set in file.sets {
        if let Err(error) = valid_key("toolset id", &set.id, MAX_ID_CHARS) {
            warnings.push(format!("{error}; the set was ignored"));
            continue;
        }
        if let Err(error) = valid_key("toolset title", &set.title, MAX_TITLE_CHARS) {
            warnings.push(format!("{error}; the set was ignored"));
            continue;
        }
        if set.tools.len() > MAX_TOOLS_PER_SET {
            warnings.push(format!(
                "{FILE_NAME}: toolset '{}' lists more than {MAX_TOOLS_PER_SET} tools; the set was ignored",
                set.id
            ));
            continue;
        }
        let mut kept = Vec::new();
        for name in &set.tools {
            if tool_registry::is_panel_tool(name) {
                if !kept.contains(name) {
                    kept.push(name.clone());
                }
            } else {
                warnings.push(format!(
                    "{FILE_NAME}: toolset '{}' names unknown panel tool '{name}'; the name was ignored",
                    set.id
                ));
            }
        }
        if kept.is_empty() {
            warnings.push(format!(
                "{FILE_NAME}: toolset '{}' offers no panel tool; the set was ignored",
                set.id
            ));
            continue;
        }
        if !seen.insert(set.id.clone()) {
            warnings.push(format!(
                "{FILE_NAME}: toolset id '{}' is already defined; the duplicate was ignored",
                set.id
            ));
            continue;
        }
        set.tools = kept;
        sets.push(set);
    }
    (sets, warnings)
}

/// Reads the file. Any problem yields the bundled sets plus a warning: the
/// switchboard is an optional layer, never a reason to fail the panel.
pub fn load_from(path: &Path) -> (Vec<Toolset>, Vec<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (bundled(), Vec::new());
        }
        Err(error) => {
            return (
                bundled(),
                vec![format!(
                    "could not read {}: {error}; using the bundled toolsets",
                    path.display()
                )],
            );
        }
    };
    match parse(&text) {
        Ok(file) => accept(file),
        Err(error) => (
            bundled(),
            vec![format!("{error}; using the bundled toolsets")],
        ),
    }
}

static SETS: LazyLock<Vec<Toolset>> = LazyLock::new(|| {
    if cfg!(test) {
        // Tests install their own file; never read the developer's home file.
        return bundled();
    }
    let (sets, warnings) = load_from(&path());
    for warning in &warnings {
        log::warn!("[ai] {warning}");
    }
    sets
});

/// The effective sets: bundled first, then the user file's valid additions.
pub fn effective() -> Vec<Toolset> {
    SETS.clone()
}

/// Tool names a selection offers, in registry order. Unknown set ids are
/// ignored so a deleted set does not break a saved selection; an empty
/// selection offers nothing and the model answers in prose.
pub fn resolve(selection: &[String]) -> Vec<String> {
    let selected: BTreeSet<&str> = selection.iter().map(String::as_str).collect();
    let mut allowed: BTreeSet<&str> = BTreeSet::new();
    for set in SETS.iter().filter(|set| selected.contains(set.id.as_str())) {
        for name in &set.tools {
            allowed.insert(name.as_str());
        }
    }
    let mut names: Vec<String> = Vec::new();
    for def in tool_registry::panel_daw_tools() {
        if allowed.contains(def.name) {
            names.push(def.name.to_owned());
        }
    }
    for def in tool_registry::local_tools() {
        if allowed.contains(def.name) {
            names.push(def.name.to_owned());
        }
    }
    names
}

/// Whether the selection enables a tool.
pub fn offers(name: &str, selection: &[String]) -> bool {
    resolve(selection).iter().any(|tool| tool == name)
}

/// The refusal a call deserves when its toolset is not selected. Names the set
/// that would enable the tool, or the file to edit when no set carries it.
pub fn refusal(name: &str, selection: &[String]) -> String {
    let enablers: Vec<String> = SETS
        .iter()
        .filter(|set| set.tools.iter().any(|tool| tool == name))
        .map(|set| format!("'{}' ({})", set.id, set.title))
        .collect();
    if enablers.is_empty() {
        return format!(
            "refused: '{name}' is not in any toolset; add it to one in ~/.opencompose/ai/{FILE_NAME}"
        );
    }
    let withheld = if selection.is_empty() {
        "no toolset is selected".to_owned()
    } else {
        selection.join(", ")
    };
    format!(
        "refused: '{name}' is not offered by the current toolsets ({withheld}); enable {} in the panel's Toolset switch",
        enablers.join(" or ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(selection: &[String]) -> Vec<String> {
        resolve(selection)
    }

    fn selection(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn every_bundled_set_is_a_partition_of_the_panel_surface() {
        let mut union: BTreeSet<String> = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for set in bundled() {
            assert!(
                ids.insert(set.id.clone()),
                "duplicate bundled id {}",
                set.id
            );
            assert!(!set.title.is_empty() && !set.description.is_empty());
            for name in &set.tools {
                assert!(
                    tool_registry::is_panel_tool(name),
                    "{} names '{}' which the panel does not enable",
                    set.id,
                    name
                );
                assert!(union.insert(name.clone()), "{name} is in two sets");
            }
        }
        let mut expected: Vec<String> = tool_registry::panel_daw_tools()
            .iter()
            .map(|def| def.name.to_owned())
            .collect();
        expected.extend(
            tool_registry::local_tools()
                .iter()
                .map(|def| def.name.to_owned()),
        );
        let covered: Vec<String> = union.into_iter().collect();
        expected.sort_unstable();
        assert_eq!(covered, expected, "every panel tool needs a home set");
        let defaults: Vec<String> = bundled()
            .into_iter()
            .filter(|set| set.default_on)
            .map(|set| set.id)
            .collect();
        assert_eq!(
            defaults,
            selection(&[
                "dsl-edit",
                "project-edit",
                "ears",
                "listen",
                "plugins",
                "samples",
                "conversations"
            ])
        );
    }

    #[test]
    fn the_ears_set_offers_the_measure_and_split_tools() {
        let ears = bundled()
            .into_iter()
            .find(|set| set.id == "ears")
            .expect("the ears set is bundled");
        assert!(ears.default_on);
        assert!(ears.description.contains("split"));
        assert_eq!(
            ears.tools,
            vec![
                "render_range",
                "audition_range",
                "audition_clip_source",
                "audio_analyze",
                "analyze_notes",
                "separate_stems",
                "audio_compare",
                "meter_snapshot",
            ]
        );
        for name in &ears.tools {
            assert!(
                tool_registry::is_panel_tool(name),
                "ears names '{name}' which the panel does not offer"
            );
        }
        let offered = resolve(&selection(&["ears"]));
        assert_eq!(offered, ears.tools, "the set resolves in registry order");
        assert!(offers("separate_stems", &selection(&["ears"])));
        // The compat report stayed with its own set, so the two do not overlap.
        assert!(!offers("render_compat_report", &selection(&["ears"])));
        let compat = resolve(&selection(&["listen"]));
        assert_eq!(compat, vec!["render_compat_report".to_owned()]);
    }

    #[test]
    fn a_deselected_set_contributes_no_tools() {
        let dsl_only = selection(&["dsl-edit"]);
        assert!(names(&dsl_only).contains(&"edit_subtree".to_owned()));
        assert!(names(&dsl_only).contains(&"read_skill".to_owned()));
        for tool in [
            "tracks_mix",
            "render_range",
            "audio_analyze",
            "transport_play",
        ] {
            assert!(!names(&dsl_only).contains(&tool.to_owned()), "{tool}");
            assert!(!offers(tool, &dsl_only), "{tool}");
        }
        // The union keeps registry order and lists each tool once.
        let defaults: Vec<String> = bundled()
            .into_iter()
            .filter(|set| set.default_on)
            .map(|set| set.id)
            .collect();
        let everything = names(&defaults);
        let mut sorted = everything.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), everything.len());
        assert_eq!(everything.last().map(String::as_str), Some("edit_subtree"));
    }

    #[test]
    fn an_out_of_set_tool_names_the_set_that_would_enable_it() {
        let dsl_only = selection(&["dsl-edit"]);
        let message = refusal("tracks_mix", &dsl_only);
        assert!(message.starts_with("refused:"), "{message}");
        assert!(
            message.contains("not offered by the current toolsets"),
            "{message}"
        );
        assert!(
            message.contains("'project-edit' (Project edit)"),
            "{message}"
        );
        assert!(message.contains("Toolset switch"), "{message}");
        assert!(refusal("audio_analyze", &dsl_only).contains("'ears' (Ears)"));
    }

    #[test]
    fn an_empty_selection_offers_nothing() {
        assert!(resolve(&[]).is_empty());
        assert!(refusal("project_save", &[]).contains("not in any toolset"));
    }

    #[test]
    fn a_user_set_adds_tools_and_drops_unknown_names() {
        let file = parse(
            r#"{
                "version": 1,
                "sets": [{
                    "id": "my-mix",
                    "title": "My mix",
                    "description": "Just the faders",
                    "tools": ["tracks_mix", "no_such_tool", "tracks_mix"],
                    "default_on": false
                }]
            }"#,
        )
        .unwrap();
        let (sets, warnings) = accept(file);
        let mix = sets.iter().find(|set| set.id == "my-mix").unwrap();
        assert_eq!(mix.tools, vec!["tracks_mix".to_owned()]);
        assert!(!mix.default_on);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no_such_tool"));
        assert!(sets.len() > bundled().len());
    }

    #[test]
    fn a_user_set_cannot_redefine_a_bundled_id_or_offer_nothing() {
        let file = parse(
            r#"{"version": 1, "sets": [
                {"id": "listen", "title": "Fake listen", "description": "", "tools": ["tracks_mix"]},
                {"id": "empty", "title": "Empty", "description": "", "tools": ["gone"]}
            ]}"#,
        )
        .unwrap();
        let (sets, warnings) = accept(file);
        assert_eq!(sets.len(), bundled().len());
        assert!(warnings.iter().any(|w| w.contains("already defined")));
        assert!(warnings.iter().any(|w| w.contains("offers no panel tool")));
    }

    #[test]
    fn a_broken_or_future_file_falls_back_to_the_bundled_sets() {
        let dir = std::env::temp_dir().join(format!("oc-toolsets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("toolsets.json");

        std::fs::write(&path, "{ not json").unwrap();
        let (sets, warnings) = load_from(&path);
        assert_eq!(sets, bundled());
        assert_eq!(warnings.len(), 1);

        std::fs::write(&path, r#"{"version": 99, "sets": []}"#).unwrap();
        let (sets, warnings) = load_from(&path);
        assert_eq!(sets, bundled());
        assert!(warnings[0].contains("unsupported version"));

        std::fs::remove_file(&path).unwrap();
        let (sets, warnings) = load_from(&path);
        assert_eq!(sets, bundled());
        assert!(warnings.is_empty(), "{warnings:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
