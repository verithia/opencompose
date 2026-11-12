//! Tool identity and policy shared by discovery, permissions and dispatch.

use super::{Approval, Panel};

/// Who executes a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    Backend,
    PanelLocal,
    HiddenBackend,
}
/// Static metadata for one tool.
#[derive(Debug)]
pub struct ToolMetadata {
    pub name: &'static str,
    pub area: &'static str,
    pub panel: Panel,
    pub mutation: bool,
    pub approval: Approval,
    pub handler: Handler,
}
macro_rules! tool_ids {
    ($($id:ident => ($name:literal, $area:literal, $panel:expr, $mutation:literal, $approval:ident, $handler:ident)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        /// Stable identity of every tool.
        pub enum ToolId { $($id),+ }
        impl ToolId {
            /// Every tool id.
            pub const ALL: &'static [Self] = &[$(Self::$id),+];
            /// Parse a tool name.
            pub fn parse(name: &str) -> Option<Self> { Self::ALL.iter().copied().find(|id| id.name() == name) }
            /// Static metadata for this tool.
            pub const fn metadata(self) -> &'static ToolMetadata {
                match self { $(Self::$id => &ToolMetadata { name: $name, area: $area, panel: $panel, mutation: $mutation, approval: Approval::$approval, handler: Handler::$handler }),+ }
            }
            /// Wire name of this tool.
            pub const fn name(self) -> &'static str { self.metadata().name }
        }
    }
}

tool_ids! {
    SystemCapabilities => ("system_capabilities", "system", Panel::Excluded("host details are not a musical action"), false, Automatic, Backend),
    ProjectGet => ("project_get", "project", Panel::Enabled, false, Automatic, Backend),
    AudioClipsList => ("audio_clips_list", "project", Panel::Enabled, false, Automatic, Backend),
    AudioClipTransients => ("audio_clip_transients", "project", Panel::Enabled, false, Automatic, Backend),
    ImportAlignedStems => ("import_aligned_stems", "project", Panel::Enabled, true, Automatic, Backend),
    AudioClipEditRange => ("audio_clip_edit_range", "project", Panel::Enabled, true, Automatic, Backend),
    AudioClipSetPlacement => ("audio_clip_set_placement", "project", Panel::Enabled, true, Automatic, Backend),
    AudioClipSetPlayback => ("audio_clip_set_playback", "project", Panel::Enabled, true, Automatic, Backend),
    ProjectDsl => ("project_dsl", "project", Panel::Enabled, false, Automatic, Backend),
    ProjectNew => ("project_new", "project", Panel::Excluded("replaces the current session"), true, Confirm, Backend),
    ProjectOpen => ("project_open", "project", Panel::Excluded("reads an arbitrary file path"), true, Confirm, Backend),
    ProjectSave => ("project_save", "project", Panel::Excluded("writes an arbitrary file path"), false, Automatic, Backend),
    ProjectSetTempo => ("project_set_tempo", "project", Panel::Enabled, true, Automatic, Backend),
    ProjectSetName => ("project_set_name", "project", Panel::Enabled, true, Automatic, Backend),
    PictureTimecode => ("picture_timecode", "project", Panel::Enabled, false, Automatic, Backend),
    PictureClockSet => ("picture_clock_set", "project", Panel::Enabled, true, Automatic, Backend),
    TempoMapSet => ("tempo_map_set", "project", Panel::Enabled, true, Automatic, Backend),
    TimeSignatureSet => ("time_signature_set", "project", Panel::Enabled, true, Automatic, Backend),
    MidiImportHandoff => ("midi_import_handoff", "midi", Panel::Enabled, true, Automatic, Backend),
    MidiExportHandoff => ("midi_export_handoff", "midi", Panel::Enabled, false, Automatic, Backend),
    MidiImportPreview => ("midi_import_preview", "midi", Panel::Excluded("reads an arbitrary MIDI file"), false, Automatic, Backend),
    MidiImportApply => ("midi_import_apply", "midi", Panel::Excluded("reads an arbitrary MIDI file"), true, Automatic, Backend),
    MidiExport => ("midi_export", "midi", Panel::Excluded("writes an arbitrary file path"), false, Automatic, Backend),
    ProjectContext => ("project_context", "project", Panel::Enabled, false, Automatic, Backend),
    ProjectGenerateContext => ("project_generate_context", "project", Panel::Enabled, false, Automatic, Backend),
    ProjectValidate => ("project_validate", "project", Panel::Enabled, false, Automatic, Backend),
    ProjectEdit => ("project_edit", "project", Panel::Excluded("whole-subtree replacement goes through the edit_subtree proposal"), true, Automatic, Backend),
    ProjectExtract => ("project_extract", "project", Panel::Enabled, false, Automatic, Backend),
    ProjectDiff => ("project_diff", "project", Panel::Enabled, false, Automatic, Backend),
    TracksList => ("tracks_list", "tracks", Panel::Enabled, false, Automatic, Backend),
    TracksAdd => ("tracks_add", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksCreate => ("tracks_create", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksRemove => ("tracks_remove", "tracks", Panel::Enabled, true, Confirm, Backend),
    TracksDuplicate => ("tracks_duplicate", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksRename => ("tracks_rename", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksMix => ("tracks_mix", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksMute => ("tracks_mute", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksSolo => ("tracks_solo", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksPreset => ("tracks_preset", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksOutput => ("tracks_output", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksSendAdd => ("tracks_send_add", "tracks", Panel::Enabled, true, Automatic, Backend),
    TracksSendRemove => ("tracks_send_remove", "tracks", Panel::Enabled, true, Automatic, Backend),
    SectionsList => ("sections_list", "project", Panel::Enabled, false, Automatic, Backend),
    SectionsAdd => ("sections_add", "project", Panel::Enabled, true, Automatic, Backend),
    SectionsSet => ("sections_set", "project", Panel::Enabled, true, Automatic, Backend),
    SectionsRemove => ("sections_remove", "project", Panel::Enabled, true, Confirm, Backend),
    ContentLibraries => ("content_libraries", "content", Panel::Enabled, false, Automatic, Backend),
    ContentLibraryRegister => ("content_library_register", "content", Panel::Excluded("writes the app content catalogue"), false, Automatic, Backend),
    ContentLibraryRemove => ("content_library_remove", "content", Panel::Excluded("writes the app content catalogue"), false, Automatic, Backend),
    ContentLibraryRescan => ("content_library_rescan", "content", Panel::Excluded("writes the app content catalogue"), false, Automatic, Backend),
    PresetsList => ("presets_list", "presets", Panel::Enabled, false, Automatic, Backend),
    PresetBundleExport => ("preset_bundle_export", "presets", Panel::Enabled, false, Automatic, Backend),
    PresetBundleImport => ("preset_bundle_import", "presets", Panel::Enabled, true, Automatic, Backend),
    PresetsAdd => ("presets_add", "presets", Panel::Enabled, true, Automatic, Backend),
    PresetsRemove => ("presets_remove", "presets", Panel::Enabled, true, Confirm, Backend),
    PresetsRename => ("presets_rename", "presets", Panel::Enabled, true, Automatic, Backend),
    PresetsDuplicate => ("presets_duplicate", "presets", Panel::Enabled, true, Automatic, Backend),
    PresetsSetParam => ("presets_set_param", "presets", Panel::Enabled, true, Automatic, Backend),
    PresetsSetParams => ("presets_set_params", "presets", Panel::Enabled, true, Automatic, Backend),
    PluginSearch => ("plugin_search", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginPull => ("plugin_pull", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginIndexRefresh => ("plugin_index_refresh", "plugin_index", Panel::Excluded("network catalog refresh"), false, Automatic, Backend),
    SoundfontsList => ("soundfonts_list", "soundfont", Panel::Enabled, false, Automatic, Backend),
    SoundfontInstall => ("soundfont_install", "soundfont", Panel::Excluded("downloads a large instrument pack into the user's soundfont folder"), false, Automatic, Backend),
    SoundfontsRemove => ("soundfonts_remove", "soundfont", Panel::Excluded("deletes a file from the user's soundfont folder"), false, Automatic, Backend),
    SampleSearch => ("sample_search", "sample", Panel::Enabled, false, Automatic, Backend),
    SamplePull => ("sample_pull", "sample", Panel::Enabled, false, Automatic, Backend),
    PluginsScan => ("plugins_scan", "plugin", Panel::Excluded("process-level plugin scan"), false, Automatic, Backend),
    PluginsLoad => ("plugins_load", "plugin", Panel::Excluded("creates a process-level plugin instance"), false, Automatic, Backend),
    PluginsParams => ("plugins_params", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginsOpenGui => ("plugins_open_gui", "plugin", Panel::Excluded("opens a native window outside the panel"), false, Automatic, Backend),
    PluginsCloseGui => ("plugins_close_gui", "plugin", Panel::Excluded("closes a native window outside the panel"), false, Automatic, Backend),
    PluginsHasGui => ("plugins_has_gui", "plugin", Panel::Excluded("UI plumbing, not a musical action"), false, Automatic, Backend),
    PluginsSaveState => ("plugins_save_state", "plugin", Panel::Excluded("writes plugin state to an arbitrary file path"), true, Automatic, Backend),
    PluginsProgramCapabilities => ("plugins_program_capabilities", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginsPrograms => ("plugins_programs", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginsSelectProgram => ("plugins_select_program", "plugin", Panel::Enabled, true, Automatic, Backend),
    PluginsReadState => ("plugins_read_state", "plugin", Panel::Enabled, false, Automatic, Backend),
    PluginsApplyState => ("plugins_apply_state", "plugin", Panel::Enabled, true, Automatic, Backend),
    TransportPlay => ("transport_play", "transport", Panel::Enabled, false, Automatic, Backend),
    TransportPause => ("transport_pause", "transport", Panel::Enabled, false, Automatic, Backend),
    TransportStop => ("transport_stop", "transport", Panel::Enabled, false, Automatic, Backend),
    TransportSeek => ("transport_seek", "transport", Panel::Enabled, false, Automatic, Backend),
    TransportState => ("transport_state", "transport", Panel::Enabled, false, Automatic, Backend),
    TransportMute => ("transport_mute", "transport", Panel::Excluded("device-level mute is not a musical edit"), false, Automatic, Backend),
    TransportUnmute => ("transport_unmute", "transport", Panel::Excluded("device-level mute is not a musical edit"), false, Automatic, Backend),
    RenderOffline => ("render_offline", "render", Panel::Excluded("unbounded full-song render is too slow for the panel; use render_range"), false, Automatic, Backend),
    RenderRange => ("render_range", "render", Panel::Enabled, false, Automatic, Backend),
    RenderCompatReport => ("render_compat_report", "render", Panel::Enabled, false, Automatic, Backend),
    RenderStems => ("render_stems", "render", Panel::Excluded("renders the mix and every stem of the whole song, the same unbounded cost the panel refuses for render_offline; render_range with stems covers a window"), false, Automatic, Backend),
    AuditionRange => ("audition_range", "render", Panel::Enabled, false, Automatic, Backend),
    AuditionClipSource => ("audition_clip_source", "render", Panel::Enabled, false, Automatic, Backend),
    AudioAnalyze => ("audio_analyze", "analysis", Panel::Enabled, false, Automatic, Backend),
    AnalyzeNotes => ("analyze_notes", "analysis", Panel::Enabled, false, Automatic, Backend),
    SeparateStems => ("separate_stems", "analysis", Panel::Enabled, false, Automatic, Backend),
    AudioCompare => ("audio_compare", "analysis", Panel::Enabled, false, Automatic, Backend),
    AudioDevices => ("audio_devices", "audio", Panel::Excluded("host audio configuration"), false, Automatic, Backend),
    AudioConfig => ("audio_config", "audio", Panel::Excluded("host audio configuration"), false, Automatic, Backend),
    Undo => ("undo", "undo", Panel::Enabled, true, Automatic, Backend),
    Redo => ("redo", "undo", Panel::Enabled, true, Automatic, Backend),
    UndoState => ("undo_state", "undo", Panel::Enabled, false, Automatic, Backend),
    UndoHistory => ("undo_history", "undo", Panel::Enabled, false, Automatic, Backend),
    SamplerAssets => ("sampler_assets", "devices", Panel::Excluded("lists filesystem paths"), false, Automatic, Backend),
    MeterSnapshot => ("meter_snapshot", "analysis", Panel::Enabled, false, Automatic, Backend),
    DevicesCatalog => ("devices_catalog", "devices", Panel::Enabled, false, Automatic, Backend),
    DevicePresetsList => ("device_presets_list", "devices", Panel::Enabled, false, Automatic, Backend),
    DevicePresetApply => ("device_preset_apply", "devices", Panel::Enabled, true, Automatic, Backend),
    EffectsAdd => ("effects_add", "effects", Panel::Enabled, true, Automatic, Backend),
    EffectsRemove => ("effects_remove", "effects", Panel::Enabled, true, Confirm, Backend),
    EffectsSetBypass => ("effects_set_bypass", "effects", Panel::Enabled, true, Automatic, Backend),
    EffectsSetParam => ("effects_set_param", "effects", Panel::Enabled, true, Automatic, Backend),
    BusesAdd => ("buses_add", "buses", Panel::Enabled, true, Automatic, Backend),
    BusesRoutes => ("buses_routes", "buses", Panel::Enabled, true, Automatic, Backend),
    BusesMix => ("buses_mix", "buses", Panel::Enabled, true, Automatic, Backend),
    BusesRemove => ("buses_remove", "buses", Panel::Enabled, true, Confirm, Backend),
    TransactionBegin => ("transaction_begin", "transaction", Panel::Enabled, true, Automatic, Backend),
    TransactionCommit => ("transaction_commit", "transaction", Panel::Enabled, false, Automatic, Backend),
    TransactionRollback => ("transaction_rollback", "transaction", Panel::Enabled, false, Confirm, Backend),
    ListSkills => ("list_skills", "skills", Panel::Enabled, false, Automatic, Backend),
    ReadSkill => ("read_skill", "skills", Panel::Enabled, false, Automatic, Backend),
    SessionsList => ("sessions_list", "sessions", Panel::Enabled, false, Automatic, Backend),
    SessionsNew => ("sessions_new", "sessions", Panel::Enabled, false, Automatic, Backend),
    SessionsLoad => ("sessions_load", "sessions", Panel::Enabled, false, Automatic, Backend),
    SessionsRename => ("sessions_rename", "sessions", Panel::Enabled, false, Automatic, Backend),
    SessionsDelete => ("sessions_delete", "sessions", Panel::Excluded("removes a saved conversation"), false, Automatic, Backend),
    SessionsCompact => ("sessions_compact", "sessions", Panel::Enabled, false, Automatic, Backend),
    EditSubtree => ("edit_subtree", "project", Panel::Enabled, false, Automatic, PanelLocal),
    PluginIndexStatus => ("plugin_index_status", "unmapped", Panel::Excluded("not enabled for the built-in panel"), false, Automatic, HiddenBackend),
}
