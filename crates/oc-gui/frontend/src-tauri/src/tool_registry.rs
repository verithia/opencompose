//! Shared tool registry for the external MCP surface and the built-in AI panel.
//!
//! One table defines every DAW tool: name, description, JSON schema, area and
//! panel policy. Both surfaces read it; handlers live in `agent_api::dispatch`,
//! keyed by the same name, so a tool can never be advertised without a handler.

use serde_json::{Value, json};

pub(crate) mod arguments;
mod identity;
pub use identity::{Handler, ToolId, ToolMetadata};

/// Whether the built-in AI panel may call a tool, and why not when it may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Enabled,
    Excluded(&'static str),
}

/// What the panel does before it dispatches a tool that changes the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Apply the call inside the turn's transaction, as one undo entry.
    Automatic,
    /// Ask the user first; a session with nobody to ask refuses the call.
    Confirm,
}

impl Approval {
    /// The panel wire spelling of the decision.
    pub fn wire(self) -> &'static str {
        match self {
            Approval::Automatic => "automatic",
            Approval::Confirm => "confirm",
        }
    }
}

#[derive(Debug)]
/// Tool identity, description and argument schema.
pub struct ToolSpec {
    pub id: ToolId,
    pub description: &'static str,
    pub schema: Value,
}
impl std::ops::Deref for ToolSpec {
    type Target = ToolMetadata;
    fn deref(&self) -> &Self::Target {
        self.id.metadata()
    }
}

/// Tools that change the project and therefore honor `expected_revision`.
pub fn is_mutation_tool(tool: &str) -> bool {
    ToolId::parse(tool).is_some_and(|id| id.metadata().mutation)
}
/// MCP wire shape: the full surface, unchanged.
pub fn mcp_tools() -> Vec<Value> {
    catalog().iter().map(ToolSpec::wire).collect()
}

/// DAW tools the built-in AI panel may call; panel-local tools live in TS.
pub fn panel_daw_tools() -> Vec<ToolSpec> {
    catalog()
        .into_iter()
        .filter(|def| def.panel == Panel::Enabled)
        .collect()
}

/// The panel surface narrowed by a conversation's toolset selection; `None`
/// keeps the whole surface. A selection can only narrow the policy, never
/// widen it: excluded tools stay excluded.
pub fn panel_tools_for(selection: Option<&[String]>) -> Vec<Value> {
    let Some(selection) = selection else {
        let mut tools: Vec<ToolSpec> = panel_daw_tools();
        tools.extend(local_tools());
        return tools.iter().map(ToolSpec::panel_wire).collect();
    };
    let allowed = crate::ai::toolsets::resolve(selection);
    let offered = |name: &str| allowed.iter().any(|tool| tool == name);
    panel_daw_tools()
        .into_iter()
        .chain(local_tools())
        .filter(|def| offered(def.name))
        .map(|def| def.panel_wire())
        .collect()
}

/// Whether the panel knows this tool at all, regardless of policy or toolsets.
pub fn is_panel_tool(name: &str) -> bool {
    ToolId::parse(name).is_some_and(|id| id.metadata().panel == Panel::Enabled)
}

/// Tools whose handler lives in the panel frontend, not in `agent_api::dispatch`.
pub fn local_tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: ToolId::EditSubtree,
        description: "Propose one complete subtree replacement. A session with a user asks for acceptance; a session with nobody to ask applies it in the same turn, as one undo entry.",
        schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "OCP subtree path, e.g. project or track:bass" },
                "new_dsl": { "type": "string" }
            },
            "required": ["path", "new_dsl"],
            "additionalProperties": false
        }),
    }]
}

/// Resolve a panel tool call to its DAW definition, or explain the refusal.
pub fn panel_tool(name: &str) -> Result<ToolSpec, String> {
    if let Some(def) = panel_daw_tools().into_iter().find(|def| def.name == name) {
        return Ok(def);
    }
    if ToolId::parse(name).is_some_and(|id| id.metadata().handler == Handler::PanelLocal) {
        return Err(format!(
            "refused: '{name}' is handled inside the panel and cannot be dispatched"
        ));
    }
    match catalog().into_iter().find(|def| def.name == name) {
        Some(def) => match def.panel {
            Panel::Excluded(reason) => Err(format!(
                "refused: '{name}' is excluded from the built-in AI panel: {reason}"
            )),
            Panel::Enabled => Ok(def),
        },
        None => Err(format!("unknown tool '{name}'")),
    }
}

/// Resolve a panel call against the conversation's toolset selection, then the
/// panel policy. Only tools the panel knows can be refused for their set, so an
/// unknown name keeps its own error and an excluded tool keeps its reason.
pub fn panel_tool_in(name: &str, selection: Option<&[String]>) -> Result<ToolSpec, String> {
    if let Some(selection) = selection
        && is_panel_tool(name)
        && !crate::ai::toolsets::offers(name, selection)
    {
        return Err(crate::ai::toolsets::refusal(name, selection));
    }
    panel_tool(name)
}

/// Enforce the advertised schema for panel calls. MCP keeps its historical
/// coercion rules; the panel is strict about types, enums and unknown keys.
pub fn validate_args(def: &ToolSpec, args: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err(format!("'{}' expects a JSON object", def.name));
    }
    validate_node(&def.schema, args, def.name)
}

fn validate_node(schema: &Value, value: &Value, tool: &str) -> Result<(), String> {
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        return Err(format!(
            "'{tool}': value {value} is not one of {enum_values:?}"
        ));
    }
    if let Some(kinds) = schema.get("type") {
        let allowed: Vec<&str> = match kinds {
            Value::String(kind) => vec![kind.as_str()],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !allowed.is_empty() && !allowed.iter().any(|kind| matches_type(kind, value)) {
            return Err(format!(
                "'{tool}': expected {}, got {value}",
                allowed.join(" or ")
            ));
        }
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && value.as_f64().is_some_and(|number| number < minimum)
    {
        return Err(format!("'{tool}': value must be at least {minimum}"));
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
        && value.as_f64().is_some_and(|number| number > maximum)
    {
        return Err(format!("'{tool}': value must be at most {maximum}"));
    }
    if let Some(items) = schema.get("items")
        && let Some(values) = value.as_array()
    {
        for item in values {
            validate_node(items, item, tool)?;
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let object = value.as_object().expect("object checked above");
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("'{tool}': unknown argument '{key}'"));
                }
            }
        }
        for (key, property) in properties {
            if let Some(inner) = object.get(key) {
                validate_node(property, inner, tool)?;
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(format!("'{tool}': missing argument '{required}'"));
            }
        }
    }
    Ok(())
}

fn matches_type(kind: &str, value: &Value) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

impl ToolSpec {
    /// MCP wire shape; pinned so the external surface cannot drift.
    pub fn wire(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.schema,
        })
    }

    /// Panel wire shape adds the area used to group the prompt summary,
    /// whether the call changes the project (the panel loop opens one undo
    /// transaction per turn around the mutating calls), and whether it needs
    /// the user's approval before it runs.
    pub fn panel_wire(&self) -> Value {
        let mut value = self.wire();
        value["area"] = json!(self.area);
        value["mutation"] = json!(self.mutation);
        value["approval"] = json!(self.approval.wire());
        value
    }
}

/// Build the tool catalogue.
pub fn catalog() -> Vec<ToolSpec> {
    let mut tools = vec![
        arguments::tool::<crate::agent_tools::misc::SystemCapabilitiesArgs>(
            "Report agent version, platform, plugin formats, and audio devices.",
        ),
        // project
        arguments::tool::<crate::agent_tools::project_query::GetArgs>(
            "Return the full project snapshot (AST, DSL, path, dirty flag, track count).",
        ),
        arguments::tool::<crate::agent_tools::misc::AudioClipsListArgs>(
            "List audio clips with their zero-based indices, media paths, timeline ranges, playback settings, and project revision.",
        ),
        arguments::tool::<crate::agent_tools::audio_clip::AudioClipTransientsArgs>(
            "Estimate attack times in the original media of a project clip, before offset, stretch, or warp. Returns source duration, timing resolution, analyzed end, and candidate source_seconds ranked by strength. Use source_seconds for warp anchors and audition to verify musical placement.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::ImportAlignedStemsArgs>(
            "Import up to 64 same-origin mono or stereo WAV stems and one optional MIDI file into a saved project with no tracks or sections. Supply explicit project-relative paths inside handoff/, where the operator staged the exports. WAVs start at 1:1 with source speed and leading silence intact. Stereo files use unity-gain balance at center, including dual-mono prints. MIDI sets the project clock and starts muted, with raw key gates and separate sustain lanes by channel. Use the current revision from project_get. Conflicting Broadcast WAV start timestamps are refused. Returns revision, import counts and the number of timestamped stems. Missing timestamps leave common origin unverified. Inspect with audio_clips_list, then verify alignment before editing.",
        ),
        arguments::tool::<crate::agent_tools::audio_clip::AudioClipEditRangeArgs>(
            "Split or trim an audio clip using the same undoable edits as the timeline. Supply exactly one of split_at or trim_to. Optional trim_edge identifies the dragged edge when snapping a fine offset back to its unchanged musical anchor. Positions use one-based bar and beat and zero-based subdivision. Use audio_clips_list first and refresh it after a split because indices can change.",
        ),
        arguments::tool::<crate::agent_tools::audio_clip::AudioClipSetPlacementArgs>(
            "Move an audio clip precisely without trimming media or changing its elapsed duration. Supply exactly one of start_seconds (absolute project time) or nudge_ms (signed milliseconds: negative earlier, positive later). Get clip_index and expected_revision from audio_clips_list. The musical anchors remain authored separately; fine start/end offsets move together. This is an artistic placement edit, not latency compensation. Refresh the clip list after editing and compare aligned attacks with audition_range. One undo restores the placement.",
        ),
        arguments::tool::<crate::agent_tools::audio_clip::AudioClipSetPlaybackArgs>(
            "Change an audio clip's speed, pitch, formants, or warp anchors in one undoable edit. Get clip_index and expected_revision from audio_clips_list. Omitted fields keep their current values. Set stretch_to to resize and scale the clip together, like Alt-dragging its right edge. Supply stretch or stretch_to, never both. Varispeed links pitch to speed like tape. Clear pitch, formant, preserve_formants and warp_markers when switching to varispeed. Other edits keep the timeline range fixed. Stretch is output duration divided by source duration; explicit tempo_follow=true instead fits the whole source to the clip range and clears its source offset and loop. Do not supply stretch together with tempo_follow=true; a processing mode is required. Unrelated edits that omit tempo_follow preserve existing trim/loop settings. Pitch and formant are in semitones. Set preserve_formants to compensate for pitch-induced formant movement. Compare the same window with audition_range.",
        ),
        arguments::tool::<crate::agent_tools::project_query::DslArgs>(
            "Return the current project as OpenCompose DSL text.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::ProjectNewArgs>(
            "Create a new empty project.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::ProjectOpenArgs>(
            "Open an .ocp project from disk.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::ProjectSaveArgs>(
            "Save the project. Omit path to save to the current file.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::SetTempoArgs>(
            "Set the project tempo in BPM (20-999).",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::SetNameArgs>(
            "Set the project name.",
        ),
        arguments::tool::<crate::agent_tools::picture::PictureTimecodeArgs>(
            "Convert editorial timecode, project seconds or bar/beat positions using the current tempo and meter map. Use the saved project clock or supply both the exact picture rate and the timecode at project zero. Colons mean non-drop; a final semicolon enables drop-frame at 30000/1001 only. Each result gives the nearest picture frame, signed project seconds and grid offset. Pre-roll can be negative. High-frame-rate field-pair labels and 24-hour wraps require source frame indices supplied separately.",
        ),
        arguments::tool::<crate::agent_tools::picture::PictureClockSetArgs>(
            "Save the editorial frame rate and timecode at project zero as one undoable edit. Use the current revision from project_get. Pass clock=null to clear the saved setting. Only save settings explicitly confirmed by the operator.",
        ),
        arguments::tool::<crate::agent_tools::transport::TempoMapSetArgs>(
            "Replace the tempo map with the given points, as one undoable Transport edit; an empty list returns the song to the project tempo. Use it when the tempo has to change inside the song (e.g. after bar 9) — project_set_tempo only moves the starting tempo. Points must be strictly ascending, and each tempo 1-400 BPM; an invalid map is refused with the points that were sent and leaves the project untouched. Returns the written points, the project tempo and the new revision.",
        ),
        arguments::tool::<crate::agent_tools::transport::TimeSignatureSetArgs>(
            "Replace the time-signature map with the given bar points, as one undoable Transport edit; an empty list returns to the base meter. Use it for meter changes inside the song (3/4 after bar 9). A bar-1 point overrides the base meter for playback, while the toolbar and project.properties keep showing the base field, so set both with project_edit when the whole song's meter changes. Arranged content keeps its absolute musical time. Bars are one-indexed and strictly ascending, and each meter is 1-32 over 1/2/4/8/16/32/64; an invalid map is refused with the points that were sent and leaves the project untouched. Returns the written points, the base meter and the new revision.",
        ),
        // MIDI
        arguments::tool::<crate::agent_tools::midi::MidiImportHandoffArgs>(
            "Import one staged .mid or .midi from handoff/ into the saved project. Supply the current project_get revision. Preserves key-release note lengths and separate CC64 sustain by channel. Imports the MIDI clock for an empty project and keeps the clock of an existing arrangement.",
        ),
        arguments::tool::<crate::agent_tools::midi::MidiExportHandoffArgs>(
            "Export a committed saved project or track selection to a new MIDI file in handoff/. Supply the current project_get revision. Returns the relative path and export loss report. Commit the current transaction before exporting.",
        ),
        arguments::tool::<crate::agent_tools::midi::MidiImportPreviewArgs>(
            "Preview a Standard MIDI File import without changing the project. Returns the import summary (tracks, ignored events, tick error).",
        ),
        arguments::tool::<crate::agent_tools::midi::MidiImportApplyArgs>(
            "Import a Standard MIDI File into the project using the mapping options. Returns the summary and new track count.",
        ),
        arguments::tool::<crate::agent_tools::midi::MidiExportArgs>(
            "Export the project (or a selection) as a Standard MIDI File and return the loss report.",
        ),
        arguments::tool::<crate::agent_tools::project_query::ContextArgs>(
            "Build an AI editing context for a DSL path (project, track:bass, ...); path grammar is described by `read_skill dsl-subtree-paths`.",
        ),
        arguments::tool::<crate::agent_tools::project_query::GenerateContextArgs>(
            "Build a context for generating a new track, phrase, or preset.",
        ),
        arguments::tool::<crate::agent_tools::project_query::ValidateArgs>(
            "Validate a DSL replacement without applying it, including that every preset `file:` it names exists. Path grammar is described by `read_skill dsl-subtree-paths`.",
        ),
        arguments::tool::<crate::agent_tools::project_mutation::EditArgs>(
            "Apply one complete DSL subtree replacement. Returns the new snapshot and a diff; path grammar is described by `read_skill dsl-subtree-paths`.",
        ),
        arguments::tool::<crate::agent_tools::project_query::ExtractArgs>(
            "Extract a DSL subtree without changing the project; path grammar is described by `read_skill dsl-subtree-paths`.",
        ),
        arguments::tool::<crate::agent_tools::project_query::DiffArgs>(
            "Line diff between two DSL strings.",
        ),
        // tracks
        arguments::tool::<crate::agent_tools::misc::TracksListArgs>(
            "List tracks with pattern clip count (clips), audio clip count (audio_clips), preset, instrument, mix, mute/solo, and output.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksAddArgs>(
            "Add a track. A track created without a preset gets the built-in OpenSynth default for its instrument family, so it renders; pass an existing playable preset to choose another sound. `read_skill playable-instruments` lists the families.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksCreateArgs>(
            "Create a track and apply a complete @track DSL block in one call. Without an explicit preset the track gets a visible built-in default for its family; a declared SoundFont `file:` that is missing is refused. `read_skill playable-instruments` explains both. Shape: `@track lead | instrument: synth/lead` then optional `effects:` lines written `- delay: time=200ms, feedback=25%, mix=18%`, then `@phrase hook [1:1 - 5:1]` with `@0.0 C5 q f` lines. Never write `rest` events or `pedal` lines; a gap is an absent `@` position.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksRemoveArgs>("Remove a track by index."),
        arguments::tool::<crate::agent_tools::tracks::TracksDuplicateArgs>(
            "Duplicate a track: the copy is appended after the last track with the name you give, the source's instrument, engine and output routing, and its own copy of the preset, so editing one patch no longer changes the other. Use it when the producer wants a second part on the same sound; the copy starts with an empty mix, no clips, effects or automation (the GUI's duplicate is the same), so add those with project_edit if they are wanted. One undo entry covers the track and the preset copy. Returns the new track index and name, its preset and the new revision.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksRenameArgs>("Rename a track."),
        arguments::tool::<crate::agent_tools::tracks::TracksMixArgs>(
            "Set track volume and/or pan.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksMuteArgs>("Mute or unmute a track."),
        arguments::tool::<crate::agent_tools::tracks::TracksSoloArgs>("Solo or unsolo a track."),
        arguments::tool::<crate::agent_tools::tracks::TracksPresetArgs>(
            "Assign or clear a track preset.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksOutputArgs>(
            "Set a track's exclusive output (master or bus name).",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksSendAddArgs>(
            "Add or replace a send from a track to a bus.",
        ),
        arguments::tool::<crate::agent_tools::tracks::TracksSendRemoveArgs>(
            "Remove a send from a track to a bus.",
        ),
        // arrangement sections
        arguments::tool::<crate::agent_tools::misc::SectionsListArgs>(
            "List the arrangement sections in written order with their spans, repeats and labels, and the rendered end (`rendered_end`, `rendered_bars`). Read-only; call it before editing so the name and range are known.",
        ),
        arguments::tool::<crate::agent_tools::sections::SectionsAddArgs>(
            "Add a section: a named span of the arrangement, plus an optional repeat. Returns the updated section list and rendered end. Spans must not overlap; the list stays in written order and the arrangement must fit the 600-second project extent.",
        ),
        arguments::tool::<crate::agent_tools::sections::SectionsSetArgs>(
            "Move or resize a section, set its repeat, rename it or change its label — use it when a part should repeat, last longer or move. Omitted fields keep their value; `repeat: null` or 1 clears a repeat and `label: null` or an empty string clears the label. Returns the updated section list and rendered end.",
        ),
        arguments::tool::<crate::agent_tools::sections::SectionsRemoveArgs>(
            "Remove a section by name. Returns the updated section list and rendered end.",
        ),
        // content libraries
        arguments::tool::<crate::agent_tools::content::ContentLibrariesArgs>(
            "List registered content libraries and their instruments, plus discovered_soundfonts from known local SF2/SF3 locations without requiring registration. Read-only. Load a discovered path using presets_add(source=soundfont, file=path, program=0), then tracks_preset.",
        ),
        arguments::tool::<crate::agent_tools::content::ContentLibraryRegisterArgs>(
            "Register a content-library root (a directory of .sf2/.sf3/.sfz instruments or a single instrument file). The root is scanned first; a write is backed up and verified, and a failed write restores the previous catalogue.",
        ),
        arguments::tool::<crate::agent_tools::content::ContentLibraryRemoveArgs>(
            "Remove a registered content library by id. The files on disk are never touched; the catalogue is backed up first.",
        ),
        arguments::tool::<crate::agent_tools::content::ContentLibraryRescanArgs>(
            "Rescan one registered library or all of them, updating the instrument list. Missing roots are kept and marked missing; the catalogue is backed up first.",
        ),
        // presets
        arguments::tool::<crate::agent_tools::content::PresetsListArgs>(
            "List authored project presets and SoundFonts. With name, inspect one preset and its effective inherited parameters and insert order without scanning SoundFonts.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetBundleExportArgs>(
            "Share an existing OpenSynth project preset as one self-contained .ocppatch bundle, including its inherited voice, samples and inserts. Requires a saved, committed project and its current revision. Finish an active transaction first; in the panel, export in a new turn. Writes a new numbered file under handoff/ without overwriting an earlier export, and returns its relative path and byte count. External insert sidechains must be removed before sharing.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetBundleImportArgs>(
            "Load a staged .ocppatch file from the saved project's handoff/ folder into an existing OpenSynth project preset as one undoable edit. Replaces the whole voice and insert chain, detaches inheritance, and preserves the target's project name and track references. Also keeps a reusable library copy; Undo restores the project but retains that library copy. Duplicate library names get numbered copies. Requires current revision and transaction ownership. External insert sidechains are refused.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsAddArgs>(
            "Create a preset. For a local SoundFont use source=soundfont, file=<absolute SF2/SF3 path from soundfonts_list.discovered or content_libraries.discovered_soundfonts>, program=0 (or another program), then tracks_preset to assign it. Use source=clap|vst3|lv2 with plugin=<scanned plugin id> for instrument plugins.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsRemoveArgs>(
            "Remove a preset and clear it from tracks.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsRenameArgs>(
            "Rename a project preset and rewrite every reference to it (tracks, patterns, patches that extend it) in the same undoable Preset edit, so nothing is left pointing at the old name. Use it when a patch is misnamed or before sharing it. An unknown name, an existing new name or an empty name is refused and nothing changes. Returns the new name, the tracks and patterns that now point at it, and the new revision.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsDuplicateArgs>(
            "Copy a project preset under a new name as one undoable Preset edit. With reassign true (the default) the tracks and patterns that played the original play the copy, so the original stays free to edit or remove; with false they keep playing the original. An unknown preset, an existing new name or an empty name is refused and nothing changes. Returns the new name, whether the references moved, and the new revision.",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsSetParamArgs>(
            "Set or remove one preset parameter (state_file, articulation_..., cc_..._param, ...).",
        ),
        arguments::tool::<crate::agent_tools::presets::PresetsSetParamsArgs>(
            "Set multiple preset parameters atomically.",
        ),
        // plugins
        arguments::tool::<crate::agent_tools::plugin_search::PluginSearchArgs>(
            "Search the bundled free-plugin catalog. Read-only; never downloads or installs. Platform defaults to this host; invalid platform/format values are rejected.",
        ),
        arguments::tool::<crate::agent_tools::plugin_pull::PluginPullArgs>(
            "Resolve or execute a catalog install. dry_run defaults to true and performs no network or disk writes; pass dry_run=false to install. URLs always come from the verified catalog. The built-in panel permits signed-index, checksum/size-pinned native plugin archives (VST3, VST2, CLAP, LV2) and managed separator runtimes. Use plugin_search then one call with dry_run=false to install. Manual DMG/PKG/EXE installers are refused. Never run shell or pip commands.",
        ),
        arguments::tool::<crate::agent_tools::plugin_index::PluginIndexRefreshArgs>(
            "Refresh the signed remote plugin index and return its status. The index is fetched only when the plugin browser opens or a caller asks for it; a check inside the 6-hour freshness interval answers from the cache, and a refresh is limited to once per 5 minutes.",
        ),
        // instrument packs
        arguments::tool::<crate::agent_tools::soundfonts::SoundfontsListArgs>(
            "List the downloadable instrument packs the signed index offers beside what is installed in the user's soundfont folder, with each pack's size and license. The discovered field also lists existing SF2/SF3 files found in known local instrument folders (15-second cache), without registration. DLS is not supported by the SoundFont engine. Load a discovered path using presets_add(source=soundfont, file=path, program=0), then assign it with tracks_preset. Discovered files are not managed packs and cannot be removed with soundfonts_remove. Read-only; installs nothing. An installed pack is playable through a preset with source: soundfont and that pack's path. The plugin browser's Instruments section is the user-facing install path: name the pack and let the user click Install.",
        ),
        arguments::tool::<crate::agent_tools::soundfonts::SoundfontInstallArgs>(
            "Download one instrument pack from the verified index into the user's soundfont folder, checking the declared size and sha256 before the atomic rename. A refused or aborted download leaves no file behind.",
        ),
        arguments::tool::<crate::agent_tools::soundfonts::SoundfontsRemoveArgs>(
            "Delete an installed instrument pack from the user's soundfont folder. Refused when the pack is not installed.",
        ),
        arguments::tool::<crate::samples::SearchArgs>(
            "Search several external sample markets at once for drum one-shots, loops, vocals, FX and instrument phrases. This build is a client: it hosts no catalogue. Markets with local debug plugins: openverse (an aggregator over Freesound, Jamendo, Wikimedia and others; no key), wikimedia-commons (no key), internet-archive (no key), freesound (needs FREESOUND_API_KEY or the org.opencompose.samples/freesound keyring item; a Freesound OAuth2 connection in External Sources preferences pulls the original, otherwise the 128 kbps preview), jamendo (needs JAMENDO_CLIENT_ID or the jamendo keyring item). Looperman, Splice, SampleSwap, SoundBible, Mixkit, Pixabay, Sample Focus, Bedroom Producers Blog and Cymatics are reported as unavailable with the reason and are never scraped. Each item carries its provider, id, kind, duration, licence, credit line and license_ok: CC0, public domain, CC BY and CC BY-SA need no confirmation; non-commercial, no-derivatives, GFDL and community licences are free to pull once the caller passes accept_licence to sample_pull. Every requested market appears in the providers array with available and reason, even when it could not answer, and a failing market is reported per page. Paged: pass next_offset back as offset. Read-only; downloads nothing. Name the markets with `providers` and keep `limit` small: a market that cannot be reached costs up to 15 seconds, so repeating the same broad query is the slowest thing this panel can do. Page with next_offset instead.",
        ),
        arguments::tool::<crate::agent_tools::sample::SamplePullArgs>(
            "Download one sample by provider and id from sample_search into the project's media/samples folder and return its path, media path and credit line. The licence and attribution are written into a .json sidecar beside the audio and recorded in index.json, so they travel with the project. The id is resolved by the named provider or by its prefix (commons:, openverse:, ia:, freesound:, jamendo:); a file URL always comes from the provider, never from the caller. Pass accept_licence: true after telling the user the licence; a non-free licence (non-commercial, no-derivatives, GFDL, a community licence or an unknown one) is refused with licence_notice and its terms sentence until then, and the receipt records licence_accepted. Refused with provider_unknown for an id no market carries, provider_unavailable for a page-only market or a keyed one without its key, size_mismatch or checksum_mismatch when the declared size or checksum does not match, and project_unsaved when the project has no folder. A Freesound pull fetches the original when an OAuth2 token is stored, otherwise its preview, and quality reports which one landed. Nothing is written on a refusal.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsScanArgs>(
            "Scan installed CLAP/VST2/VST3/AU plugins (metadata only). Set rescan=true to retry bundles an earlier scan quarantined.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsLoadArgs>(
            "Load a preset\'s CLAP/VST2/VST3 instrument and return its parameters.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsParamsArgs>(
            "Return parameters for an already-loaded external plugin preset. For built-in OpenSynth use devices_catalog with instrument builtin.open_synth; it needs no plugin loading.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsOpenGuiArgs>(
            "Open the native plugin editor (macOS VST3 editor support is qualified).",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsCloseGuiArgs>(
            "Close the native plugin editor.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsHasGuiArgs>(
            "Check whether a loaded plugin exposes a native editor.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsSaveStateArgs>(
            "Capture plugin state into the project (embedded) or a sidecar file.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsProgramCapabilitiesArgs>(
            "Return the per-format program capability table: which formats expose a native program list and which selection mechanisms are available. Data only; never loads a plugin.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsProgramsArgs>(
            "List a preset's programs. VST3 plugins expose their own list; other formats return an empty list with the capability that says so. Loads a temporary instance when none is live.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsSelectProgramArgs>(
            "Select one program and write its state into the project (embedded by default, or a verified sidecar file). Uses the plugin's program data where available; pass state_file to select a program by state when it is not. Honours the project revision guard and the plugin-state undo scope.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsReadStateArgs>(
            "Read a preset's current CLAP/VST2/VST3/AU/LV2 state as a portable value: state_hex, bytes, sha256 and truncated. The blob is capped at max_bytes (default 262144, ceiling 3145728); over the cap state_hex is null and truncated is true, never a fragment. Hand the value to `plugins_apply_state`. Loads a temporary AU/VST3/VST2/LV2 instance when the preset is not loaded; a CLAP preset must be loaded first.",
        ),
        arguments::tool::<crate::agent_tools::plugins::PluginsApplyStateArgs>(
            "Write a plugin state value (from `plugins_read_state` or a saved envelope) into the preset. Verifies the envelope's format, plugin identity, version and SHA-256 before writing, clears any state_file and pushes one PluginState undo entry, so the next render uses the applied state.",
        ),
        // transport
        arguments::tool::<crate::agent_tools::transport::PlayArgs>("Start playback."),
        arguments::tool::<crate::agent_tools::transport::PauseArgs>("Pause playback."),
        arguments::tool::<crate::agent_tools::transport::StopArgs>("Stop and return to the start."),
        arguments::tool::<crate::agent_tools::transport::SeekArgs>("Seek to a beat position."),
        arguments::tool::<crate::agent_tools::transport::StateArgs>(
            "Return the current transport snapshot.",
        ),
        arguments::tool::<crate::agent_tools::transport::MuteArgs>("Mute audio output."),
        arguments::tool::<crate::agent_tools::transport::UnmuteArgs>("Unmute audio output."),
        // render / audio
        arguments::tool::<crate::agent_tools::render::RenderOfflineArgs>(
            "Render the project to a WAV file (faster than realtime). The sample depth and dither default to the project's export setting.",
        ),
        arguments::tool::<crate::agent_tools::render::RenderRangeArgs>(
            "Render a time window in seconds or bars as an exact slice of the full render, served by the render cache. Optional track subset (a slice with the other tracks muted) and WAV stems. Returns the master path and each stem path with frame count, sample rate and channels, at the app's current output sample rate (Preferences → Audio) and pre-normalisation (a project loudness target applies to render_offline, not here). The result includes pre_encode peak and clipping evidence for the mix and each stem. To hear the window, use `audition_range`.",
        ),
        arguments::tool::<crate::agent_tools::render::RenderCompatReportArgs>(
            "Report plugin/feature compatibility for the current project.",
        ),
        arguments::tool::<crate::agent_tools::render::RenderStemsArgs>(
            "Export the whole mix, one file per track and bus, and stems.json into an existing directory, exactly like the toolbar's Export stems. Use it to hand stems to a mix engineer or to read every part's level at once; for one window of the song use render_range with stems instead. The directory must already exist. The set uses the project's export format at the app's current output sample rate (Preferences → Audio; this call cannot override either), and the mix is normalised to the project loudness target while the stems are not. Returns the manifest path, shared container, sample format, dither, sample rate and channels, and each file's absolute path, frames, peak, RMS and integrated LUFS; a render superseded by a newer one is refused.",
        ),
        arguments::tool::<crate::agent_tools::render::AuditionRangeArgs>(
            "Render a time window exactly like render_range and return a WAV of it inline, so you can listen to the part you are working on. The clip is shaped for the model's audio input: mono at 16 kHz by default (the rate and channel count the provider keeps), and `channel` selects which side you hear — mono is the equal-weight downmix, left or right isolates one side, stereo keeps both when the model accepts two channels. `clip_sample_rate` overrides the inline rate (8000..48000). Windows follow render_range's rules; longer than 30 s returns the first 30 s with truncated=true. The full render is written to the requested path or temporary scratch. The pre_encode measurements describe that full-rate window and its saved format, even when the inline clip is truncated. A saved panel session may keep an operator copy subject to its archive format and storage limits.",
        ),
        arguments::tool::<crate::agent_tools::render::AuditionClipSourceArgs>(
            "Hear a bounded window from an unprocessed clip source, an attached audio reference, or a saved arrangement render. Use exactly one of clip_index from audio_clips_list or an exact path from the current AI conversation. Times are relative to the source file, including for saved renders. Compare with audition_range for the current edited arrangement. Returns inline WAV and the actual source range heard. Does not write the project.",
        ),
        arguments::tool::<crate::agent_tools::audio_analysis::AnalyzeArgs>(
            "Analyze audio levels, spectrum and silence. Peaks and levels are not notes; for note events (start, end, midi, name, cents, confidence) call `analyze_notes`, and use `audition_range` to hear a project render. Set beat_grid for pulse evidence only: tempo candidates and observed attack anchors in a 4–30 second passage. Confirm meter and downbeat before editing. Use transients instead to measure attacks in a bounded source or rendered file passage. Results are in file seconds. Use level_window for raw sample peak and RMS on a short window, such as the last 40 ms before a hit. Use mono_fold for unlevelled octave-band loss when stereo is folded to (L+R)/2.",
        ),
        arguments::tool::<crate::agent_tools::audio_analysis::NotesArgs>(
            "Analyze and transcribe an external audio file or stem into notes: note events (start, end, MIDI number, name, cents off that pitch, detector confidence) with one monophonic detector; prefer writing the notes from what you hear, and use this only for audio whose notes are not written in the project DSL and that you cannot transcribe reliably by ear. Note times are relative to `start`, and the window may be a section. Every note also carries `start_beat` (quarter-note beats from the window start, 0-based), `duration_beats`, `bar` (1-based) and `suggested_duration`, the nearest DSL token (w, h, q, 8th, 16th, 32nd, 64th, dotted or triplet). Derive the duration token from `suggested_duration`; do not invent one. An octave is only decided when it beats the other octave by 6 dB on its own fundamental: a note whose sub-octave or octave-up layer is that close carries `octave_ambiguous: true` and `octave_runner_up` (the MIDI number it would take instead), and a repeated pitch holds its earlier octave while the reading stays ambiguous. When a note you need is flagged, do not pick an octave from it — re-run on a window where that voice sounds alone (or a stem), and say which octave you could not pin down. The top level reports the grid the beats assume (`tempo_bpm`, `time_signature`, `beats_per_bar`, `beat_seconds`, `quantise` tolerance); it comes from the tempo/time_signature arguments, else this WAV's render metadata, else the `project` you pass, and only then the engine default 120 BPM 4/4 with a warning. If the external tempo is unknown, ask audio_analyze for beat_grid evidence before supplying a confirmed grid. A note more than `quantise` beats off the 1/16 grid is named in `warnings` and its suggestion is a guess. A chord sets `polyphonic: true` and only the strongest voice is transcribed; drums, chords and ornaments under about 50 ms are not. Use `audition_range` to hear the same window.",
        ),
        arguments::tool::<crate::agent_tools::separation::SeparateStemsArgs>(
            "Split a rendered WAV into stems before transcribing: `mode` auto (default) runs demucs or spleeter when one is on PATH and otherwise falls back to the built-in band split, external refuses when neither is installed, band always uses the fallback. The band split is a 200 Hz low/high filter split plus the mid for a stereo source — an approximation, not a source separation — and `method` says which path ran. Feed each stem to `analyze_notes`.",
        ),
        arguments::tool::<crate::agent_tools::audio_analysis::CompareArgs>(
            "Compare two audio files (WAV or supported compressed media such as MP3/FLAC) using shared comparison definitions: LUFS delta, per-octave and body deltas, envelope and waveform correlation, max sample difference, level-matched log-mel error, tilt, crest/attack, bass harmonics and headline score. Compressed media is decoded to stereo at its native sample rate. Both analyzed signals must share a sample rate and frame count; render both sides at the same rate, which is the app's current output rate (Preferences → Audio). Different lengths or sample rates are refused unless allow_mismatch=true. For a passage, specify duration (up to 30 seconds) and independent reference_start and candidate_start in source seconds. A passage must fit both files. Set listen=true with a 0.4–8 second passage to receive two level-matched listening copies when measurable. They preserve source timing; the report verifies the actual delivered PCM. The listening comparison is labeled, so it is open rather than blind.",
        ),
        arguments::tool::<crate::agent_tools::misc::AudioDevicesArgs>("List output audio devices."),
        arguments::tool::<crate::agent_tools::misc::AudioConfigArgs>(
            "Return the current audio configuration.",
        ),
        // undo
        arguments::tool::<crate::agent_tools::project_mutation::UndoArgs>("Undo one project edit."),
        arguments::tool::<crate::agent_tools::project_mutation::RedoArgs>("Redo one project edit."),
        arguments::tool::<crate::agent_tools::misc::UndoStateArgs>(
            "Return undo/redo availability and labels.",
        ),
        arguments::tool::<crate::agent_tools::misc::UndoHistoryArgs>(
            "List the undo stack in order — index, label, scopes, timestamp and whether each entry is applied (levels above `applied` are redo) — so you can tell the user what changed and in which order. Read-only: it never moves the cursor; undo and redo do that. Returns the entries plus the applied and redoable counts.",
        ),
        // sampler
        arguments::tool::<crate::agent_tools::misc::SamplerAssetsArgs>(
            "List bundled sampler assets (soundfonts, kits, etc.).",
        ),
        // effects / buses
        arguments::tool::<crate::agent_tools::metering::SnapshotArgs>(
            "Read the live mixer's current published meters without starting playback or rendering. Returns the same available/reason/playing state, master channels, mono state, master/track/bus peak and RMS levels in dBFS, gain reduction/effect bands and ceiling margins in dB, clipping latches, optional short-term LUFS (3-second window), stereo correlation and goniometer, and spectrum when already enabled. Unavailable or absent readings are not zero or silence measurements; paused readings may retain the last published chunk. This is live monitoring, not an offline analysis of current edits. For a reproducible headless measurement use render_range then audio_analyze.",
        ),
        arguments::tool::<crate::device_catalog::Query>(
            "Without arguments, list built-in mixer effects. For authoritative OpenSynth parameters use instrument builtin.open_synth, optionally prefix (e.g. osc.a. or mod.1.), query, or group. Returns bounded pages with ranges, units, defaults, enums, aliases, indexed families and implementation status; use next_offset for more. Defaults are not current preset values. Every entry carries primary: the ten to twenty parameters that carry the device — start there instead of reading every page. Prefer prefix or query over paging: pages hold 24 entries and the syntax has hundreds.",
        ),
        arguments::tool::<crate::agent_tools::device::DevicePresetsListArgs>(
            "List the device preset library: the shipped factory bank merged with the user library under ~/.opencompose, optionally for one device. Every row carries source (factory or user), shadows (a user row replaced a factory row with the same id) and deletable (false for factory rows). A broken user file is reported in warnings while the factory rows still list.",
        ),
        arguments::tool::<crate::agent_tools::device::DevicePresetApplyArgs>(
            "Apply a device preset to the effect insert at index on target, as one undo entry. The preset must belong to the device the insert holds: a preset for another device is refused with the same sentence the apply_device_preset command returns. An unknown preset id is refused by name. Use device_presets_list for ids and devices_catalog for the insert's device.",
        ),
        arguments::tool::<crate::agent_tools::effects::EffectsAddArgs>(
            "Add an effect to track:N, bus:name, master, or preset:name.",
        ),
        arguments::tool::<crate::agent_tools::effects::EffectsRemoveArgs>(
            "Remove an effect by index.",
        ),
        arguments::tool::<crate::agent_tools::effects::EffectsSetBypassArgs>(
            "Bypass or enable an existing effect without changing its parameters or removing the insert. The index is zero-based within the target's effects chain.",
        ),
        arguments::tool::<crate::agent_tools::effects::EffectsSetParamArgs>(
            "Set an effect parameter.",
        ),
        arguments::tool::<crate::agent_tools::bus::BusesAddArgs>("Create a bus."),
        arguments::tool::<crate::agent_tools::bus::BusesRoutesArgs>("Replace a bus's routes."),
        arguments::tool::<crate::agent_tools::bus::BusesMixArgs>(
            "Update a bus return's fader, mute and solo.",
        ),
        arguments::tool::<crate::agent_tools::bus::BusesRemoveArgs>(
            "Remove an unused bus. A bus that still receives track outputs, sends or bus routes is rejected unless reroute is true; rerouting returns those track outputs to master and drops the sends/routes into it.",
        ),
        // transactions / revision
        arguments::tool::<crate::agent_tools::session::TransactionBeginArgs>(
            "Begin an atomic batch of agent edits. Returns the base revision to pass as expected_revision until the batch is committed or rolled back.",
        ),
        arguments::tool::<crate::agent_tools::session::TransactionCommitArgs>(
            "Commit the active agent transaction and squash its edits into one undo entry.",
        ),
        arguments::tool::<crate::agent_tools::session::TransactionRollbackArgs>(
            "Discard this turn's agent transaction, restoring the project and undo history to before the turn's first edit. The panel asks the user before it runs.",
        ),
        // skills: reference text, read-only, no project state
        arguments::tool::<crate::agent_tools::reference::ListSkillsArgs>(
            "List the skill index (id, title, when, category, version, source). Bodies stay unread until read_skill; a skill is reference text and never grants a tool.",
        ),
        arguments::tool::<crate::agent_tools::reference::ReadSkillArgs>(
            "Read one skill body by exact id from list_skills. For id production-curriculum, pass topic to get a genre, scene, or technique exercise, or omit it to browse fields. Returns Markdown and referenced panel tools.",
        ),
        // saved conversations: one directory per project under ~/.opencompose
        arguments::tool::<crate::agent_tools::session::SessionsListArgs>(
            "List saved AI conversations newest first, with title, turns, tokens and update time. Omit project to use the open project; all lists every project.",
        ),
        arguments::tool::<crate::agent_tools::session::SessionsNewArgs>(
            "Start a saved AI conversation and make it the one this session autosaves into. parent forks that conversation.",
        ),
        arguments::tool::<crate::agent_tools::session::SessionsLoadArgs>(
            "Return a saved conversation's metadata and its wire messages, ready to replay. Loading makes it the autosave target.",
        ),
        arguments::tool::<crate::agent_tools::session::SessionsRenameArgs>(
            "Rename a saved AI conversation.",
        ),
        arguments::tool::<crate::agent_tools::session::SessionsDeleteArgs>(
            "Delete a saved AI conversation and its messages.",
        ),
        arguments::tool::<crate::agent_tools::session::SessionsCompactArgs>(
            "Fold a saved conversation's older turns into one plain-text summary that also lists the most recent tool calls as text. No tool call, provider state or signature survives, so a model that requires thought signatures can replay it. Returns the compacted messages.",
        ),
    ];
    for entry in &mut tools {
        if entry.mutation {
            entry.schema["properties"]["expected_revision"] =
                arguments::RevisionGuard::property_schema();
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_daw_names() -> Vec<&'static str> {
        panel_daw_tools().iter().map(|def| def.name).collect()
    }

    #[test]
    fn full_mcp_schema_contract_is_unchanged() {
        let expected: Value =
            serde_json::from_str(include_str!("tool_registry/schema-contract.json")).unwrap();
        assert_eq!(json!(mcp_tools()), expected);
    }

    #[test]
    fn every_tool_identity_and_policy_matches_the_independent_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("tool_registry/metadata-contract.json")).unwrap();
        let definitions: Vec<_> = catalog().into_iter().chain(local_tools()).collect();
        assert_eq!(definitions.len(), expected.len());
        let mut seen = std::collections::BTreeSet::new();
        for (definition, expected) in definitions.iter().zip(expected) {
            let id = definition.id;
            assert!(
                seen.insert(id),
                "duplicate registered identity {}",
                id.name()
            );
            assert_eq!(ToolId::parse(id.name()), Some(id));
            let panel = match definition.panel {
                Panel::Enabled => json!({"enabled": true}),
                Panel::Excluded(reason) => json!({"excluded": reason}),
            };
            let handler = match definition.handler {
                Handler::Backend => "backend",
                Handler::PanelLocal => "panel_local",
                Handler::HiddenBackend => "hidden_backend",
            };
            assert_eq!(
                json!({"name": definition.name, "area": definition.area, "mutation": definition.mutation, "approval": definition.approval.wire(), "panel": panel, "handler": handler}),
                expected,
                "{} policy",
                id.name()
            );
            assert_eq!(is_mutation_tool(id.name()), definition.mutation);
            if definition.mutation {
                assert!(
                    definition.schema["properties"]
                        .get("expected_revision")
                        .is_some(),
                    "{} revision contract",
                    id.name()
                );
            }
        }
        assert_eq!(
            seen,
            ToolId::ALL
                .iter()
                .copied()
                .filter(|id| id.metadata().handler != Handler::HiddenBackend)
                .collect()
        );
        assert_eq!(ToolId::parse("invented_tool"), None);
        assert!(!is_mutation_tool("invented_tool"));
    }

    #[test]
    fn legacy_dispatch_only_identity_stays_hidden_from_discovery_and_panel() {
        let hidden: Vec<_> = ToolId::ALL
            .iter()
            .copied()
            .filter(|id| id.metadata().handler == Handler::HiddenBackend)
            .collect();
        assert_eq!(hidden, [ToolId::PluginIndexStatus]);
        let metadata = ToolId::PluginIndexStatus.metadata();
        assert_eq!(metadata.name, "plugin_index_status");
        assert_eq!(metadata.area, "unmapped");
        assert_eq!(
            metadata.panel,
            Panel::Excluded("not enabled for the built-in panel")
        );
        assert!(!metadata.mutation);
        assert_eq!(metadata.approval, Approval::Automatic);
        assert_eq!(
            ToolId::parse("plugin_index_status"),
            Some(ToolId::PluginIndexStatus)
        );
        assert!(!is_panel_tool(metadata.name));
        assert!(!mcp_tools().iter().any(|tool| tool["name"] == metadata.name));
        assert_eq!(
            panel_tool(metadata.name).unwrap_err(),
            "unknown tool 'plugin_index_status'"
        );
    }

    #[test]
    fn mcp_surface_is_pinned() {
        let tools = mcp_tools();
        assert_eq!(tools.len(), 126);
        assert_eq!(tools[0]["name"], "system_capabilities");
        for tool in &tools {
            let mut keys: Vec<_> = tool.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["description", "inputSchema", "name"]);
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
        }
        let render = tools
            .iter()
            .find(|tool| tool["name"] == "render_range")
            .unwrap();
        assert_eq!(render["inputSchema"]["required"], json!(["path"]));
        assert_eq!(
            render["inputSchema"]["properties"]["start"]["type"],
            "number"
        );
        assert_eq!(
            render["inputSchema"]["properties"]["start_bar"]["type"],
            "integer"
        );
        // Every argument the dispatcher reads must be discoverable.
        let render_offline = tools
            .iter()
            .find(|tool| tool["name"] == "render_offline")
            .unwrap();
        assert_eq!(render_offline["inputSchema"]["required"], json!(["path"]));
        for name in ["path", "preRoll", "pre_roll", "sample_format", "dither"] {
            assert!(
                !render_offline["inputSchema"]["properties"][name].is_null(),
                "render_offline accepts {name} but does not declare it"
            );
        }
        assert_eq!(
            render_offline["inputSchema"]["properties"]["sample_format"]["type"],
            "string"
        );
        assert_eq!(
            render_offline["inputSchema"]["properties"]["dither"]["type"],
            "boolean"
        );
        let audition = tools
            .iter()
            .find(|tool| tool["name"] == "audition_range")
            .unwrap();
        assert_eq!(audition["inputSchema"]["required"], json!([]));
        assert_eq!(
            audition["inputSchema"]["properties"]["end"]["type"],
            "number"
        );
        assert_eq!(
            audition["inputSchema"]["properties"]["tracks"]["type"],
            "array"
        );
        for tool in [render, audition] {
            assert_eq!(
                tool["inputSchema"]["properties"]["float_wav"]["type"],
                "boolean"
            );
        }
        assert!(audition["inputSchema"]["properties"]["stems"].is_null());
        let compare = tools
            .iter()
            .find(|tool| tool["name"] == "audio_compare")
            .unwrap();
        assert_eq!(
            compare["inputSchema"]["required"],
            json!(["reference", "candidate"])
        );
        assert_eq!(
            compare["inputSchema"]["properties"]["allow_mismatch"]["type"],
            "boolean"
        );
        let analyze = tools
            .iter()
            .find(|tool| tool["name"] == "audio_analyze")
            .unwrap();
        assert_eq!(
            analyze["inputSchema"]["properties"]["top_peaks"]["type"],
            "integer"
        );
    }

    #[test]
    fn aligned_stems_tool_requires_staged_paths_and_a_project_revision() {
        let def = panel_tool_in("import_aligned_stems", Some(&["project-edit".into()])).unwrap();
        assert!(def.mutation);
        assert!(validate_args(&def, &json!({"paths": ["handoff/Drums.wav"]})).is_err());
        assert!(validate_args(&def, &json!({"paths": [123], "expected_revision": 0})).is_err());
        assert!(
            validate_args(
                &def,
                &json!({"paths": ["handoff/Drums.wav"], "expected_revision": 0})
            )
            .is_ok()
        );
    }

    #[test]
    fn preset_bundles_are_revision_guarded_and_available_in_the_panel() {
        let enabled = Some(&["project-edit".into()][..]);
        let export = panel_tool_in("preset_bundle_export", enabled).unwrap();
        let import = panel_tool_in("preset_bundle_import", enabled).unwrap();
        assert!(!export.mutation);
        assert!(import.mutation);
        assert!(validate_args(&export, &json!({"name":"Lead"})).is_err());
        assert!(validate_args(&export, &json!({"name":"Lead","expected_revision":0})).is_ok());
        assert!(
            validate_args(
                &import,
                &json!({"path":"handoff/lead.ocppatch","name":"Lead","expected_revision":0})
            )
            .is_ok()
        );
        assert!(
            validate_args(
                &import,
                &json!({"path":"handoff/lead.ocppatch","name":"Lead"})
            )
            .is_err()
        );
    }

    #[test]
    fn midi_handoff_tools_are_scoped_and_revision_guarded_in_the_panel() {
        let enabled = Some(&["project-edit".into()][..]);
        let import = panel_tool_in("midi_import_handoff", enabled).unwrap();
        let export = panel_tool_in("midi_export_handoff", enabled).unwrap();
        assert!(import.mutation);
        assert!(!export.mutation);
        assert!(validate_args(&import, &json!({"path":"handoff/take.mid"})).is_err());
        assert!(validate_args(&export, &json!({})).is_err());
        assert!(
            validate_args(
                &export,
                &json!({"expected_revision":0,"path":"elsewhere.mid"})
            )
            .is_err()
        );
        assert!(
            validate_args(
                &import,
                &json!({"path":"handoff/take.mid","expected_revision":0})
            )
            .is_ok()
        );
        assert!(
            validate_args(
                &export,
                &json!({"expected_revision":0,"options":{"scope":{"Track":0}}})
            )
            .is_ok()
        );
        for name in ["midi_import_preview", "midi_import_apply", "midi_export"] {
            assert!(panel_tool_in(name, enabled).is_err());
        }
    }

    #[test]
    fn the_compare_description_states_the_shared_rate_rule() {
        let compare = panel_tool("audio_compare").unwrap();
        let description = compare.description.to_lowercase();
        assert!(
            description.contains("both analyzed signals must share a sample rate"),
            "{}",
            compare.description
        );
        let render = panel_tool("render_range").unwrap();
        assert!(
            render.description.contains("current output sample rate"),
            "{}",
            render.description
        );
    }

    #[test]
    fn audio_analyze_sends_a_note_question_to_analyze_notes() {
        let tools = mcp_tools();
        let analyze = tools
            .iter()
            .find(|tool| tool["name"] == "audio_analyze")
            .unwrap();
        let description = analyze["description"].as_str().unwrap();
        assert!(
            description.contains("not notes"),
            "audio_analyze does not say that peaks are not notes: {description}"
        );
        assert!(
            description.contains("analyze_notes"),
            "audio_analyze does not name the note-level tool: {description}"
        );
        for field in ["start", "end", "midi", "name", "cents", "confidence"] {
            assert!(
                description.contains(field),
                "the note clause does not name {field}: {description}"
            );
        }
        assert!(
            description.contains("`audition_range`"),
            "audio_analyze lost its listening clause: {description}"
        );
    }

    #[test]
    fn every_path_tool_documents_the_grammar() {
        let tools = mcp_tools();
        for name in [
            "project_context",
            "project_validate",
            "project_edit",
            "project_extract",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} is listed"));
            let description = tool["inputSchema"]["properties"]["path"]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{name} has a path description"));
            for form in [
                "project",
                "track:<name>",
                "track:<n>",
                "track:<name>.phrase:<name>",
                "bus:<name>",
                "preset:<name>",
                "pattern:<name>",
                "project.properties",
            ] {
                assert!(
                    description.contains(form),
                    "{name} does not document {form}: {description}"
                );
            }
            assert!(
                description.contains("Not replaceable"),
                "{name} does not say what cannot be replaced: {description}"
            );
            assert_eq!(
                description.lines().count(),
                2,
                "{name} keeps the path description to two lines"
            );
        }
    }

    #[test]
    fn the_content_and_program_tools_are_advertised() {
        let names: Vec<&str> = catalog().iter().map(|def| def.name).collect();
        for name in [
            "content_libraries",
            "content_library_register",
            "content_library_remove",
            "content_library_rescan",
            "plugins_program_capabilities",
            "plugins_programs",
            "plugins_select_program",
            "plugins_read_state",
            "plugins_apply_state",
        ] {
            assert!(names.contains(&name), "{name} is missing");
        }
        assert!(is_mutation_tool("plugins_select_program"));
        assert!(is_mutation_tool("plugins_apply_state"));
        assert!(!is_mutation_tool("plugins_read_state"));
        assert!(!is_mutation_tool("content_library_register"));
        let capabilities = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_program_capabilities")
            .unwrap();
        assert_eq!(capabilities.schema["properties"], json!({}));
        assert!(matches!(capabilities.panel, Panel::Enabled));
    }

    #[test]
    fn the_plugin_state_tools_trade_a_capped_value() {
        let read = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_read_state")
            .expect("plugins_read_state is in the catalog");
        assert!(matches!(read.panel, Panel::Enabled));
        assert_eq!(read.area, "plugin");
        assert_eq!(read.schema["required"], json!(["preset"]));
        assert_eq!(
            read.schema["properties"]["max_bytes"]["maximum"],
            json!(3145728.0)
        );
        assert!(read.description.contains("truncated"));
        assert!(read.description.contains("plugins_apply_state"));

        let apply = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_apply_state")
            .expect("plugins_apply_state is in the catalog");
        assert!(matches!(apply.panel, Panel::Enabled));
        assert!(apply.mutation);
        assert_eq!(apply.schema["required"], json!(["preset", "state_hex"]));
        for name in [
            "plugins_programs",
            "plugins_select_program",
            "plugins_read_state",
            "plugins_apply_state",
        ] {
            let set = crate::ai::toolsets::resolve(&["plugins".to_owned()]);
            assert!(
                set.contains(&name.to_owned()),
                "{name} is not in the plugins set"
            );
            assert!(
                panel_tools_for(None)
                    .iter()
                    .any(|tool| tool["name"] == name),
                "{name} must be offered to the panel"
            );
        }
    }

    #[test]
    fn analyze_notes_is_a_read_only_ears_tool() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "analyze_notes")
            .expect("analyze_notes is in the catalog");
        assert!(matches!(def.panel, Panel::Enabled));
        assert_eq!(def.area, "analysis");
        assert_eq!(ToolId::AnalyzeNotes.metadata().panel, Panel::Enabled);
        assert_eq!(ToolId::AnalyzeNotes.metadata().area, "analysis");
        assert!(!is_mutation_tool("analyze_notes"));
        assert_eq!(def.schema["required"], json!(["path"]));
        for name in ["start", "end", "min_confidence"] {
            assert_eq!(def.schema["properties"][name]["type"], "number", "{name}");
        }
        assert_eq!(def.schema["properties"]["max_notes"]["type"], "integer");
        let ears = crate::ai::toolsets::resolve(&["ears".to_owned()]);
        assert!(
            ears.contains(&"analyze_notes".to_owned()),
            "analyze_notes must sit in the ears set: {ears:?}"
        );
        assert!(
            panel_tools_for(None)
                .iter()
                .any(|tool| tool["name"] == "analyze_notes")
        );
    }

    /// Producer-facing editing tools belong to the `project-edit` and `preset-edit` sets.
    #[test]
    fn the_producer_tools_are_edits_in_the_project_and_preset_sets() {
        let project_edit = crate::ai::toolsets::resolve(&["project-edit".to_owned()]);
        for name in [
            "tempo_map_set",
            "time_signature_set",
            "tracks_duplicate",
            "presets_rename",
            "presets_duplicate",
            "undo_history",
        ] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert!(matches!(def.panel, Panel::Enabled), "{name}");
            assert!(
                panel_tools_for(None)
                    .iter()
                    .any(|tool| tool["name"] == name),
                "{name} must be offered to the panel"
            );
            assert!(
                project_edit.contains(&name.to_owned()),
                "{name} must sit in project-edit: {project_edit:?}"
            );
        }
        for name in [
            "tempo_map_set",
            "time_signature_set",
            "tracks_duplicate",
            "presets_rename",
            "presets_duplicate",
        ] {
            assert!(is_mutation_tool(name), "{name} changes the project");
        }
        assert!(!is_mutation_tool("undo_history"));
        assert!(!is_mutation_tool("render_stems"));

        let tempo = catalog()
            .into_iter()
            .find(|def| def.name == "tempo_map_set")
            .unwrap();
        assert_eq!(tempo.area, "project");
        assert_eq!(tempo.schema["required"], json!(["points"]));
        assert_eq!(tempo.schema["properties"]["points"]["type"], "array");
        assert_eq!(
            tempo.schema["properties"]["points"]["items"]["required"],
            json!(["bar", "bpm"])
        );
        assert!(tempo.description.contains("project_set_tempo"));

        let meter = catalog()
            .into_iter()
            .find(|def| def.name == "time_signature_set")
            .unwrap();
        assert_eq!(meter.area, "project");
        assert_eq!(
            meter.schema["properties"]["points"]["items"]["required"],
            json!(["bar", "denominator", "numerator"])
        );
        assert_eq!(
            meter.schema["properties"]["points"]["items"]["properties"]["denominator"]["enum"],
            json!([1, 2, 4, 8, 16, 32, 64])
        );

        // The panel's schema door refuses an impossible meter before dispatch.
        let meter_def = panel_tool("time_signature_set").unwrap();
        assert!(
            validate_args(
                &meter_def,
                &json!({"points": [{"bar": 5, "numerator": 3, "denominator": 4}]})
            )
            .is_ok()
        );
        assert!(
            validate_args(
                &meter_def,
                &json!({"points": [{"bar": 5, "numerator": 3, "denominator": 5}]})
            )
            .is_err()
        );
        assert!(
            validate_args(
                &meter_def,
                &json!({"points": [{"bar": 5, "numerator": 3, "denominator": 4, "bpm": 120}]})
            )
            .is_err()
        );

        let duplicate = catalog()
            .into_iter()
            .find(|def| def.name == "tracks_duplicate")
            .unwrap();
        assert_eq!(duplicate.area, "tracks");
        assert_eq!(duplicate.schema["required"], json!(["index", "name"]));

        for name in ["presets_rename", "presets_duplicate"] {
            let def = catalog().into_iter().find(|def| def.name == name).unwrap();
            assert_eq!(def.area, "presets", "{name}");
            assert_eq!(
                def.schema["required"],
                json!(["name", "new_name"]),
                "{name}"
            );
        }

        let history = catalog()
            .into_iter()
            .find(|def| def.name == "undo_history")
            .unwrap();
        assert_eq!(history.area, "undo");
        assert_eq!(history.schema["properties"], json!({}));
        assert!(
            history.description.contains("never moves the cursor"),
            "{}",
            history.description
        );

        // Stems is a whole-song export at an agent-chosen path: kept out of the
        // panel for the same unbounded-cost reason as render_offline, and so it
        // belongs to no toolset.
        let stems = catalog()
            .into_iter()
            .find(|def| def.name == "render_stems")
            .unwrap();
        assert_eq!(stems.area, "render");
        match stems.panel {
            Panel::Excluded(reason) => assert!(reason.contains("render_offline"), "{reason}"),
            Panel::Enabled => panic!("render_stems must stay out of the panel"),
        }
        assert_eq!(stems.schema["required"], json!(["directory"]));
    }

    #[test]
    fn content_and_preset_list_schemas_declare_filters() {
        let content = catalog()
            .into_iter()
            .find(|def| def.name == "content_libraries")
            .expect("content_libraries is in the catalog");
        assert_eq!(content.schema["required"], json!([]));
        assert_eq!(content.schema["properties"]["catalogue"]["type"], "string");
        let presets = catalog()
            .into_iter()
            .find(|def| def.name == "presets_list")
            .expect("presets_list is in the catalog");
        assert_eq!(presets.schema["required"], json!([]));
        assert_eq!(presets.schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn project_list_schemas_declare_no_payload() {
        for name in [
            "system_capabilities",
            "audio_clips_list",
            "tracks_list",
            "sections_list",
        ] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!([]), "{name}");
            assert_eq!(def.schema["properties"], json!({}), "{name}");
        }
    }

    #[test]
    fn empty_host_schemas_declare_no_payload() {
        for name in [
            "audio_devices",
            "audio_config",
            "undo_state",
            "undo_history",
            "sampler_assets",
        ] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!([]), "{name}");
            assert_eq!(def.schema["properties"], json!({}), "{name}");
        }
    }

    #[test]
    fn transaction_begin_schema_declares_label_and_guard() {
        let begin = catalog()
            .into_iter()
            .find(|def| def.name == "transaction_begin")
            .expect("transaction_begin is in the catalog");
        assert_eq!(begin.schema["required"], json!([]));
        assert_eq!(begin.schema["properties"]["label"]["type"], "string");
        assert_eq!(
            begin.schema["properties"]["expected_revision"]["type"],
            "integer"
        );
    }

    #[test]
    fn bus_schemas_declare_their_payloads() {
        let add = catalog()
            .into_iter()
            .find(|def| def.name == "buses_add")
            .expect("buses_add is in the catalog");
        assert_eq!(add.schema["required"], json!(["name"]));
        assert_eq!(add.schema["properties"]["routes"]["type"], "array");
        let routes = catalog()
            .into_iter()
            .find(|def| def.name == "buses_routes")
            .expect("buses_routes is in the catalog");
        assert_eq!(routes.schema["required"], json!(["name", "routes"]));
        let mix = catalog()
            .into_iter()
            .find(|def| def.name == "buses_mix")
            .expect("buses_mix is in the catalog");
        assert_eq!(mix.schema["required"], json!(["name"]));
        assert_eq!(mix.schema["properties"]["volume_db"]["type"], "number");
        let remove = catalog()
            .into_iter()
            .find(|def| def.name == "buses_remove")
            .expect("buses_remove is in the catalog");
        assert_eq!(remove.schema["required"], json!(["name"]));
    }

    #[test]
    fn sample_search_schema_declares_its_filters() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "sample_search")
            .expect("sample_search is in the catalog");
        assert_eq!(def.schema["required"], json!(["query"]));
        assert_eq!(def.schema["properties"]["tags"]["default"], json!([]));
        assert_eq!(def.schema["properties"]["providers"]["default"], json!([]));
        assert_eq!(def.schema["properties"]["offset"]["default"], json!(0));
        assert_eq!(def.schema["properties"]["limit"]["default"], json!(20));
    }

    #[test]
    fn devices_catalog_schema_declares_its_filters() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "devices_catalog")
            .expect("devices_catalog is in the catalog");
        assert_eq!(def.schema["required"], json!([]));
        assert_eq!(def.schema["properties"]["offset"]["minimum"], json!(0.0));
        assert_eq!(def.schema["properties"]["limit"]["minimum"], json!(0.0));
    }

    #[test]
    fn effect_set_bypass_schema_declares_its_payload() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "effects_set_bypass")
            .expect("effects_set_bypass is in the catalog");
        assert_eq!(def.schema["required"], json!(["target", "index", "bypass"]));
        assert_eq!(def.schema["properties"]["index"]["minimum"], json!(0.0));
        assert_eq!(def.schema["properties"]["bypass"]["type"], "boolean");
    }

    #[test]
    fn effect_set_param_schema_declares_its_payload() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "effects_set_param")
            .expect("effects_set_param is in the catalog");
        assert_eq!(
            def.schema["required"],
            json!(["target", "index", "param", "value"])
        );
        assert_eq!(def.schema["properties"]["index"]["minimum"], json!(0.0));
        assert_eq!(def.schema["properties"]["value"]["type"], "string");
    }

    #[test]
    fn effect_add_and_remove_schemas_declare_their_payloads() {
        let add = catalog()
            .into_iter()
            .find(|def| def.name == "effects_add")
            .expect("effects_add is in the catalog");
        assert_eq!(add.schema["required"], json!(["target", "effect_type"]));
        let remove = catalog()
            .into_iter()
            .find(|def| def.name == "effects_remove")
            .expect("effects_remove is in the catalog");
        assert_eq!(remove.schema["required"], json!(["target", "index"]));
        assert_eq!(remove.schema["properties"]["index"]["minimum"], json!(0.0));
    }

    #[test]
    fn compact_and_device_apply_schemas_declare_their_payloads() {
        let compact = catalog()
            .into_iter()
            .find(|def| def.name == "sessions_compact")
            .expect("sessions_compact is in the catalog");
        assert_eq!(compact.schema["required"], json!(["id"]));
        assert_eq!(
            compact.schema["properties"]["target_tokens"]["minimum"],
            json!(0.0)
        );
        let apply = catalog()
            .into_iter()
            .find(|def| def.name == "device_preset_apply")
            .expect("device_preset_apply is in the catalog");
        assert_eq!(
            apply.schema["required"],
            json!(["id", "target", "effect_index", "device"])
        );
        assert_eq!(
            apply.schema["properties"]["effect_index"]["minimum"],
            json!(0.0)
        );
    }

    #[test]
    fn session_new_and_device_schemas_declare_their_payloads() {
        let new = catalog()
            .into_iter()
            .find(|def| def.name == "sessions_new")
            .expect("sessions_new is in the catalog");
        assert_eq!(new.schema["required"], json!([]));
        let rename = catalog()
            .into_iter()
            .find(|def| def.name == "sessions_rename")
            .expect("sessions_rename is in the catalog");
        assert_eq!(rename.schema["required"], json!(["id", "title"]));
        let devices = catalog()
            .into_iter()
            .find(|def| def.name == "device_presets_list")
            .expect("device_presets_list is in the catalog");
        assert_eq!(devices.schema["required"], json!([]));
        assert_eq!(devices.schema["properties"]["device"]["type"], "string");
    }

    #[test]
    fn session_schemas_declare_their_payloads() {
        for name in ["transaction_commit", "transaction_rollback"] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!([]), "{name}");
            assert_eq!(def.schema["properties"], json!({}), "{name}");
        }
        let list = catalog()
            .into_iter()
            .find(|def| def.name == "sessions_list")
            .expect("sessions_list is in the catalog");
        assert_eq!(list.schema["required"], json!([]));
        assert_eq!(list.schema["properties"]["project"]["type"], "string");
        assert_eq!(list.schema["properties"]["all"]["type"], "boolean");
        for name in ["sessions_load", "sessions_delete"] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!(["id"]), "{name}");
        }
    }

    #[test]
    fn plugin_state_schemas_declare_their_payloads() {
        let save = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_save_state")
            .expect("plugins_save_state is in the catalog");
        assert_eq!(save.schema["required"], json!(["preset", "mode"]));
        assert_eq!(save.schema["properties"]["path"]["type"], "string");
        let apply = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_apply_state")
            .expect("plugins_apply_state is in the catalog");
        assert_eq!(apply.schema["required"], json!(["preset", "state_hex"]));
        let select = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_select_program")
            .expect("plugins_select_program is in the catalog");
        assert_eq!(select.schema["required"], json!(["preset"]));
        assert_eq!(select.schema["properties"]["index"]["minimum"], json!(0.0));
        let capabilities = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_program_capabilities")
            .expect("plugins_program_capabilities is in the catalog");
        assert_eq!(capabilities.schema["required"], json!([]));
    }

    #[test]
    fn simple_plugin_schemas_declare_preset() {
        let scan = catalog()
            .into_iter()
            .find(|def| def.name == "plugins_scan")
            .expect("plugins_scan is in the catalog");
        assert_eq!(scan.schema["required"], json!([]));
        assert_eq!(scan.schema["properties"]["rescan"]["type"], "boolean");
        for name in [
            "plugins_load",
            "plugins_params",
            "plugins_open_gui",
            "plugins_close_gui",
            "plugins_has_gui",
        ] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!(["preset"]), "{name}");
            assert_eq!(
                def.schema["properties"]["preset"]["type"], "string",
                "{name}"
            );
        }
    }

    #[test]
    fn soundfonts_list_schema_declares_inspection_filters() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "soundfonts_list")
            .expect("soundfonts_list is in the catalog");
        assert_eq!(def.schema["required"], json!([]));
        assert_eq!(def.schema["properties"]["path"]["type"], "string");
        assert_eq!(def.schema["properties"]["bank"]["type"], "integer");
        assert_eq!(def.schema["properties"]["query"]["type"], "string");
        assert_eq!(def.schema["properties"]["offset"]["type"], "integer");
        assert_eq!(def.schema["properties"]["limit"]["type"], "integer");
    }

    #[test]
    fn soundfont_pack_schemas_require_id() {
        for name in ["soundfont_install", "soundfonts_remove"] {
            let def = catalog()
                .into_iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is in the catalog"));
            assert_eq!(def.schema["required"], json!(["id"]), "{name}");
            assert_eq!(def.schema["properties"]["id"]["type"], "string", "{name}");
        }
    }

    #[test]
    fn plugin_index_refresh_schema_declares_force() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "plugin_index_refresh")
            .expect("plugin_index_refresh is in the catalog");
        assert_eq!(def.schema["required"], json!([]));
        assert_eq!(def.schema["properties"]["force"]["type"], "boolean");
    }

    #[test]
    fn plugin_pull_schema_declares_typed_install() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "plugin_pull")
            .expect("plugin_pull is in the catalog");
        assert_eq!(def.schema["required"], json!(["id"]));
        assert_eq!(def.schema["properties"]["id"]["type"], "string");
        assert_eq!(def.schema["properties"]["platform"]["type"], "string");
        assert_eq!(def.schema["properties"]["format"]["type"], "string");
        assert_eq!(def.schema["properties"]["dry_run"]["type"], "boolean");
    }

    #[test]
    fn plugin_search_schema_declares_typed_filters() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "plugin_search")
            .expect("plugin_search is in the catalog");
        assert_eq!(def.schema["required"], json!([]));
        assert_eq!(def.schema["properties"]["query"]["type"], "string");
        assert_eq!(def.schema["properties"]["tags"]["type"], "array");
        assert_eq!(
            def.schema["properties"]["installed_only"]["type"],
            "boolean"
        );
        assert_eq!(def.schema["properties"]["limit"]["type"], "integer");
    }

    #[test]
    fn separate_stems_is_a_read_only_analysis_tool() {
        let def = catalog()
            .into_iter()
            .find(|def| def.name == "separate_stems")
            .expect("separate_stems is in the catalog");
        assert!(matches!(def.panel, Panel::Enabled));
        assert_eq!(def.area, "analysis");
        assert_eq!(ToolId::SeparateStems.metadata().panel, Panel::Enabled);
        assert_eq!(ToolId::SeparateStems.metadata().area, "analysis");
        assert!(!is_mutation_tool("separate_stems"));
        assert!(!def.mutation);
        assert_eq!(def.schema["required"], json!(["path"]));
        assert_eq!(def.schema["properties"]["mode"]["type"], "string");
        assert_eq!(
            def.schema["properties"]["mode"]["enum"],
            json!(["auto", "external", "band"])
        );
        assert_eq!(def.schema["properties"]["out_dir"]["type"], "string");
        assert!(
            panel_tools_for(None)
                .iter()
                .any(|tool| tool["name"] == "separate_stems")
        );
    }

    #[test]
    fn every_catalog_entry_has_a_policy_arm() {
        for def in catalog() {
            let area = def.id.metadata().area;
            assert_ne!(area, "unmapped", "{} has no policy arm", def.name);
            if let Panel::Excluded(reason) = def.panel {
                assert!(!reason.is_empty(), "{} has no exclusion reason", def.name);
            }
        }
    }

    #[test]
    fn the_two_surfaces_partition_the_catalog() {
        let (mut enabled, mut excluded) = (0, 0);
        for def in catalog() {
            match def.panel {
                Panel::Enabled => enabled += 1,
                Panel::Excluded(_) => excluded += 1,
            }
        }
        assert_eq!(enabled, panel_daw_names().len());
        assert_eq!(enabled + excluded, 126);
        assert_eq!(enabled, 98);
        assert_eq!(excluded, 28);
        let unique: std::collections::HashSet<_> = panel_daw_names().into_iter().collect();
        assert_eq!(unique.len(), 98);
        assert_eq!(panel_tools_for(None).len(), 99);
    }

    /// The panel loop groups a turn by opening one transaction around its
    /// mutating calls, so every offered tool must say whether it mutates.
    #[test]
    fn the_panel_wire_says_whether_a_tool_mutates_the_project() {
        let panel = panel_tools_for(None);
        let find = |name: &str| {
            panel
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} is offered to the panel"))
        };
        for (name, mutation) in [
            ("tracks_add", true),
            ("tracks_list", false),
            ("render_range", false),
            ("effects_set_bypass", true),
            ("transaction_begin", true),
            ("transaction_commit", false),
            // Closing the turn's batch is not an edit that joins it: the loop
            // must not open a new transaction for the call that discards one.
            ("transaction_rollback", false),
            ("edit_subtree", false),
        ] {
            assert_eq!(find(name)["mutation"], json!(mutation), "{name}");
        }
        for tool in &panel {
            assert!(tool["mutation"].is_boolean(), "{}", tool["name"]);
            assert!(
                matches!(tool["approval"].as_str(), Some("automatic" | "confirm")),
                "{}",
                tool["name"]
            );
        }
        // The MCP wire is pinned; the panel-only fields stay off it.
        for tool in mcp_tools() {
            assert!(tool.get("mutation").is_none(), "{}", tool["name"]);
            assert!(tool.get("area").is_none(), "{}", tool["name"]);
            assert!(tool.get("approval").is_none(), "{}", tool["name"]);
        }
    }

    /// The destructive set is a policy, not a prompt and not a suffix match:
    /// every tool that deletes a user-built object, replaces the whole project
    /// or discards the turn's batch is listed here, and nothing else is. A new
    /// tool has to be classified on purpose.
    #[test]
    fn the_destructive_set_is_pinned() {
        let mut destructive: Vec<&str> = catalog()
            .iter()
            .filter(|def| def.approval == Approval::Confirm)
            .map(|def| def.name)
            .collect();
        destructive.sort_unstable();
        assert_eq!(
            destructive,
            [
                "buses_remove",
                "effects_remove",
                "presets_remove",
                "project_new",
                "project_open",
                "sections_remove",
                "tracks_remove",
                "transaction_rollback",
            ]
        );
        // Removing or replacing data is a project change, and the loop's
        // transaction rule already applies to it. A rollback discards the batch
        // instead of joining it, so it stays outside the mutating set.
        for name in &destructive {
            if *name == "transaction_rollback" {
                assert!(
                    !is_mutation_tool(name),
                    "{name} must not join the batch it discards"
                );
            } else {
                assert!(is_mutation_tool(name), "{name} changes the project");
            }
        }
        // Names that read as destructive but only move or edit data stay
        // automatic: the decision is the explicit table above.
        for name in [
            "tracks_add",
            "tracks_duplicate",
            "tracks_rename",
            "tracks_preset",
            "tracks_send_remove",
            "presets_add",
            "presets_rename",
            "presets_duplicate",
            "presets_set_params",
            "sections_add",
            "sections_set",
            "effects_add",
            "effects_set_bypass",
            "effects_set_param",
            "buses_add",
            "buses_routes",
            "buses_mix",
            "plugins_apply_state",
            "undo",
            "redo",
            "transaction_begin",
            "project_set_tempo",
        ] {
            assert_eq!(
                ToolId::parse(name).unwrap().metadata().approval,
                Approval::Automatic,
                "{name}"
            );
        }
    }

    /// Every destructive tool the panel can reach asks; the two whole-project
    /// tools are excluded from the panel, so the card cannot be the only gate
    /// that mentions them.
    #[test]
    fn the_panel_asks_before_it_deletes() {
        let panel = panel_tools_for(None);
        let mut asking: Vec<&str> = panel
            .iter()
            .filter(|tool| tool["approval"] == json!("confirm"))
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        asking.sort_unstable();
        assert_eq!(
            asking,
            [
                "buses_remove",
                "effects_remove",
                "presets_remove",
                "sections_remove",
                "tracks_remove",
                "transaction_rollback",
            ]
        );
        for excluded in ["project_new", "project_open"] {
            assert!(
                !panel.iter().any(|tool| tool["name"] == excluded),
                "{excluded} cannot be offered to the panel"
            );
        }
        assert_eq!(ToolId::TracksList.metadata().approval, Approval::Automatic);
    }

    #[test]
    fn enabled_tools_have_a_schema_and_a_backend_handler_identity() {
        // The exhaustive ToolId match in agent_api enforces backend arm coverage.
        for def in panel_daw_tools() {
            assert_eq!(def.schema["type"], "object", "{}", def.name);
            assert!(def.schema["properties"].is_object(), "{}", def.name);
            assert_eq!(def.panel, Panel::Enabled);
            assert_eq!(def.handler, Handler::Backend);
            assert_eq!(ToolId::parse(def.name), Some(def.id));
        }
    }

    /// Every argument an arm reads must be in that tool's schema. An undeclared
    /// name is one the model cannot learn about and the panel's strict
    /// validator rejects before the handler runs.
    #[test]
    fn every_dispatched_argument_is_declared() {
        let source = include_str!("agent_api.rs");
        let body = source
            .split("fn dispatch_tool")
            .nth(1)
            .expect("the dispatcher body")
            .split(
                "// ============================================================================",
            )
            .next()
            .expect("the dispatcher ends before the tests");
        let tools = catalog();
        let identities: Vec<ToolId> = tools.iter().map(|def| def.id).collect();
        for def in &tools {
            let marker = format!("ToolId::{:?}", def.id);
            assert!(body.contains(&marker), "{} has no dispatcher arm", def.name);
            // A grouped arm names several tools once; the arm that starts the
            // group owns their shared argument reads.
            let Some(start) = body.find(&format!("\n        {marker} =>")) else {
                continue;
            };
            let end = identities
                .iter()
                .filter_map(|other| {
                    body[start + 1..]
                        .find(&format!("\n        ToolId::{other:?} =>"))
                        .map(|offset| start + 1 + offset)
                })
                .min()
                .unwrap_or(body.len());
            let arm = &body[start..end];
            let properties = def.schema["properties"]
                .as_object()
                .expect("a properties object");
            for argument in arguments_read(arm) {
                if argument == "expected_revision" {
                    // Added to every mutation tool by `tools()`, not `catalog()`.
                    continue;
                }
                assert!(
                    properties.contains_key(argument),
                    "{} reads '{argument}' but does not declare it",
                    def.name
                );
            }
        }
    }

    /// The argument names an arm reads through the dispatcher's helpers.
    fn arguments_read(arm: &str) -> Vec<&str> {
        let mut names = Vec::new();
        for marker in ["(args, \"", "args.get(\""] {
            let mut rest = arm;
            while let Some(index) = rest.find(marker) {
                let after = &rest[index + marker.len()..];
                let Some(end) = after.find('"') else { break };
                names.push(&after[..end]);
                rest = after;
            }
        }
        names
    }

    #[test]
    fn panel_runtime_install_is_one_catalog_call_with_no_caller_control_of_guard() {
        let def = panel_tool("plugin_pull").unwrap();
        assert_eq!(def.approval, Approval::Automatic);
        validate_args(&def, &json!({"id":"spleeter", "dry_run":false})).unwrap();
        assert!(
            validate_args(&def, &json!({"id":"spleeter", "_panel_install_only":false})).is_err()
        );
        assert!(
            validate_args(
                &def,
                &json!({"id":"spleeter", "url":"https://example.test/arbitrary"})
            )
            .is_err()
        );
        assert!(panel_tool_in("plugin_pull", Some(&["plugins".into()])).is_ok());
    }

    #[test]
    fn excluded_tools_are_refused_with_a_reason() {
        for name in [
            "project_new",
            "project_open",
            "project_save",
            "project_edit",
            "midi_export",
            "plugins_save_state",
            "render_offline",
            "audio_devices",
            "transport_mute",
            "sampler_assets",
            "system_capabilities",
        ] {
            let error = panel_tool(name).unwrap_err();
            assert!(error.starts_with("refused:"), "{name}: {error}");
            assert!(
                error.contains("excluded from the built-in AI panel"),
                "{name}: {error}"
            );
        }
        assert_eq!(
            panel_tool("save_file").unwrap_err(),
            "unknown tool 'save_file'"
        );
    }

    #[test]
    fn panel_local_tools_are_not_dispatched_and_skills_are() {
        assert!(
            panel_tool("edit_subtree")
                .unwrap_err()
                .contains("handled inside the panel")
        );
        for name in ["list_skills", "read_skill"] {
            assert!(panel_tool(name).is_ok(), "{name} must reach the dispatcher");
            assert!(
                panel_tools_for(None)
                    .iter()
                    .any(|tool| tool["name"] == name)
            );
        }
        assert!(
            panel_tools_for(None)
                .iter()
                .any(|tool| tool["name"] == "edit_subtree")
        );
    }

    #[test]
    fn a_deselected_toolset_is_neither_offered_nor_dispatched() {
        let dsl_only = vec!["dsl-edit".to_string()];
        let offered = |name: &str| {
            panel_tools_for(Some(&dsl_only))
                .iter()
                .any(|tool| tool["name"] == name)
        };
        assert!(offered("edit_subtree"));
        assert!(offered("project_extract"));
        assert!(!offered("tracks_mix"));
        assert!(!offered("render_range"));
        // The same list reaches the provider, not just the panel's own copy.
        assert!(
            crate::ai::tool_specs_for(Some(&dsl_only))
                .iter()
                .all(|spec| spec.name != "tracks_mix")
        );
        // Out of set: refused, and the message names the set that would allow it.
        let error = panel_tool_in("tracks_mix", Some(&dsl_only)).unwrap_err();
        assert!(error.starts_with("refused:"), "{error}");
        assert!(error.contains("'project-edit' (Project edit)"), "{error}");
        // Unset selection keeps today's whole surface.
        assert!(panel_tool_in("tracks_mix", None).is_ok());
        assert_eq!(panel_tools_for(Some(&[])).len(), 0);
    }

    #[test]
    fn a_known_tool_outside_the_selection_is_not_unknown() {
        let dsl_only = vec!["dsl-edit".to_string()];
        let refused = panel_tool_in("transport_play", Some(&dsl_only)).unwrap_err();
        assert!(
            refused.contains("not offered by the current toolsets"),
            "{refused}"
        );
        assert!(refused.contains("'transport' (Transport)"), "{refused}");
        assert_eq!(
            panel_tool_in("save_file", Some(&dsl_only)).unwrap_err(),
            "unknown tool 'save_file'",
            "a name no toolset carries still reads unknown"
        );
    }

    #[test]
    fn a_selection_never_widens_the_policy() {
        let everything: Vec<String> = crate::ai::toolsets::effective()
            .into_iter()
            .map(|set| set.id)
            .collect();
        let error = panel_tool_in("render_offline", Some(&everything)).unwrap_err();
        assert!(
            error.contains("excluded from the built-in AI panel"),
            "{error}"
        );
        assert_eq!(
            panel_tool_in("save_file", Some(&everything)).unwrap_err(),
            "unknown tool 'save_file'"
        );
    }

    #[test]
    fn schema_validation_rejects_bad_panel_arguments() {
        let render = panel_tool("render_range").unwrap();
        assert!(
            validate_args(
                &render,
                &json!({"path": "out.wav", "start": 0.0, "end": 1.0})
            )
            .is_ok()
        );
        assert!(
            validate_args(
                &render,
                &json!({"path": "out.wav", "start_bar": 1, "end_bar": 3})
            )
            .is_ok()
        );
        assert!(
            validate_args(&render, &json!({"path": "out.wav", "end": 1.0})).is_ok(),
            "the handler enforces the seconds/bar pairing, not the JSON schema"
        );
        assert!(
            validate_args(
                &render,
                &json!({"path": "out.wav", "start": "0", "end": 1.0})
            )
            .is_err()
        );
        assert!(
            validate_args(
                &render,
                &json!({"path": "out.wav", "start_bar": "one", "end_bar": 3})
            )
            .is_err()
        );
        assert!(
            validate_args(
                &render,
                &json!({"path": "out.wav", "start": 0.0, "end": 1.0, "nope": 1})
            )
            .is_err()
        );
        assert!(validate_args(&render, &json!([])).is_err());
        let analyze = panel_tool("audio_analyze").unwrap();
        assert!(validate_args(&analyze, &json!({"path": 3})).is_err());
        assert!(validate_args(
            &analyze,
            &json!({"path": "a.wav", "transients": {"start": 0.25, "duration": 1, "min_gap_ms": 5, "limit": 4}})
        ).is_ok());
        assert!(validate_args(&analyze, &json!({"path": "a.wav", "top_peaks": 8})).is_ok());
        assert!(validate_args(&analyze, &json!({"path": "a.wav", "top_peaks": "8"})).is_err());
        let compare = panel_tool("audio_compare").unwrap();
        assert!(
            validate_args(
                &compare,
                &json!({"reference": "a.wav", "candidate": "b.wav", "allow_mismatch": true})
            )
            .is_ok()
        );
        assert!(
            validate_args(
                &compare,
                &json!({"reference": "a.wav", "candidate": "b.wav", "allow_mismatch": 1})
            )
            .is_err()
        );
    }

    #[test]
    fn the_catalog_points_sideways_at_the_remedy() {
        // The model reads this catalog once, so a description that only says what
        // a tool returns leaves it guessing. These name the next call instead.
        let tools = mcp_tools();
        let description = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} is not in the catalog"))["description"]
                .as_str()
                .expect("every catalog entry has a description")
                .to_owned()
        };
        for name in [
            "project_context",
            "project_validate",
            "project_edit",
            "project_extract",
        ] {
            assert!(
                description(name).contains("`read_skill dsl-subtree-paths`"),
                "{name}: {}",
                description(name)
            );
        }
        assert!(
            description("tracks_create").contains("`read_skill playable-instruments`"),
            "{}",
            description("tracks_create")
        );
        for name in ["render_range", "audio_analyze"] {
            assert!(
                description(name).contains("`audition_range`"),
                "{name}: {}",
                description(name)
            );
        }
        // A cross-reference to a skill the loader cannot read is dead text.
        for id in ["dsl-subtree-paths", "playable-instruments"] {
            assert!(crate::ai::skills::read(id).is_ok(), "{id}");
        }
    }

    #[test]
    fn synth_catalog_tool_accepts_bounded_discovery_fields() {
        let def = panel_tool("devices_catalog").unwrap();
        validate_args(
            &def,
            &json!({"instrument":"builtin.open_synth","prefix":"osc.a.","limit":12,"offset":0}),
        )
        .unwrap();
        validate_args(&def, &json!({})).unwrap();
        assert!(
            validate_args(
                &def,
                &json!({"instrument":"builtin.open_synth","invented":true})
            )
            .is_err()
        );
    }

    #[test]
    fn documented_counts_match_the_registry() {
        let doc = include_str!("../../../../../docs/agent-surface.md");
        let marker = doc
            .split("<!-- tool-registry: ")
            .nth(1)
            .and_then(|rest| rest.split(" -->").next())
            .expect("docs/agent-surface.md carries a tool-registry marker");
        assert_eq!(
            marker,
            format!(
                "mcp={} panel={}",
                mcp_tools().len(),
                panel_daw_names().len()
            )
        );
    }
}
