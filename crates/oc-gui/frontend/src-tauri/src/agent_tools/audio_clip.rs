//! Typed arguments for audio-clip tools.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

/// A position within a bar, inlined so the tool schema keeps its flat shape.
struct BarBeatSchema;

impl JsonSchema for BarBeatSchema {
    fn schema_name() -> String {
        "BarBeat".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(bar_beat_value())
    }
}

fn bar_beat_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "bar": {"type": "integer", "description": "One-based bar"},
            "beat": {"type": "integer", "description": "One-based beat within the bar"},
            "subdivision": {"type": "integer", "description": "Zero-based subdivision within the beat"}
        },
        "required": ["bar", "beat", "subdivision"],
        "additionalProperties": false
    })
}

fn inline_schema(value: Value) -> Schema {
    serde_json::from_value(value).expect("an inline tool schema is valid")
}

struct TimeRangeSchema;

impl JsonSchema for TimeRangeSchema {
    fn schema_name() -> String {
        "TimeRange".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": "object",
            "properties": {"start": bar_beat_value(), "end": bar_beat_value()},
            "required": ["start", "end"],
            "additionalProperties": false
        }))
    }
}

struct TrimEdgeSchema;

impl JsonSchema for TrimEdgeSchema {
    fn schema_name() -> String {
        "AudioClipTrimEdge".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({"type": "string", "enum": ["start", "end"]}))
    }
}

struct StretchModeSchema;

impl JsonSchema for StretchModeSchema {
    fn schema_name() -> String {
        "StretchMode".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(
            json!({"type": "string", "enum": ["off", "percussive", "musical", "varispeed"]}),
        )
    }
}

struct WarpMarkersSchema;

impl JsonSchema for WarpMarkersSchema {
    fn schema_name() -> String {
        "WarpMarkers".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": "array",
            "description": "Replace all anchors. Each maps source-media seconds to processed-media seconds. Disable tempo follow. Empty clears anchors.",
            "items": {
                "type": "object",
                "properties": {
                    "source_seconds": {"type": "number", "description": "Source-media seconds"},
                    "target_seconds": {"type": "number", "description": "Processed-media seconds"}
                },
                "required": ["source_seconds", "target_seconds"],
                "additionalProperties": false
            }
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Estimate attack times in the original media of a project clip, before offset, stretch, or warp. Returns source duration, timing resolution, analyzed end, and candidate source_seconds ranked by strength. Use source_seconds for warp anchors and audition to verify musical placement.
pub struct AudioClipTransientsArgs {
    #[schemars(with = "i64")]
    #[schemars(description = "Zero-based clip index from audio_clips_list")]
    pub clip_index: usize,
    #[schemars(description = "Nonnegative source time to start searching, default 0")]
    pub source_start_seconds: Option<f64>,
    #[schemars(
        description = "Search window in seconds, greater than 0 and at most 30, default 30"
    )]
    pub duration_seconds: Option<f64>,
    #[schemars(with = "Option<i64>")]
    #[schemars(description = "Maximum candidates, 1–128, default 24")]
    pub limit: Option<usize>,
}

impl ToolArguments for AudioClipTransientsArgs {
    const ID: ToolId = ToolId::AudioClipTransients;
    const FIELD_ORDER: &'static [&'static str] = &[
        "clip_index",
        "source_start_seconds",
        "duration_seconds",
        "limit",
    ];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Move an audio clip precisely without trimming media or changing its elapsed duration. Supply exactly one of start_seconds (absolute project time) or nudge_ms (signed milliseconds: negative earlier, positive later). Get clip_index and expected_revision from audio_clips_list. The musical anchors remain authored separately; fine start/end offsets move together. This is an artistic placement edit, not latency compensation. Refresh the clip list after editing and compare aligned attacks with audition_range. One undo restores the placement.
pub struct AudioClipSetPlacementArgs {
    #[schemars(with = "i64")]
    #[schemars(description = "Zero-based clip index from audio_clips_list")]
    pub clip_index: usize,
    #[schemars(description = "Absolute project start time in seconds", range(min = 0))]
    pub start_seconds: Option<f64>,
    #[schemars(description = "Signed placement change in milliseconds")]
    pub nudge_ms: Option<f64>,
}

impl ToolArguments for AudioClipSetPlacementArgs {
    const ID: ToolId = ToolId::AudioClipSetPlacement;
    const FIELD_ORDER: &'static [&'static str] = &["clip_index", "start_seconds", "nudge_ms"];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Edge of an audio clip trim.
pub enum AudioClipTrimEdge {
    Start,
    End,
}

impl AudioClipTrimEdge {
    /// Wire label for the trim edge.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Split or trim an audio clip using the same undoable edits as the timeline. Supply exactly one of split_at or trim_to. Optional trim_edge identifies the dragged edge when snapping a fine offset back to its unchanged musical anchor. Positions use one-based bar and beat and zero-based subdivision. Use audio_clips_list first and refresh it after a split because indices can change.
pub struct AudioClipEditRangeArgs {
    #[schemars(with = "i64")]
    #[schemars(description = "Zero-based clip index from audio_clips_list")]
    pub clip_index: usize,
    #[schemars(with = "Option<BarBeatSchema>")]
    pub split_at: Option<oc_dsl::ast::BarBeat>,
    #[schemars(with = "Option<TrimEdgeSchema>")]
    pub trim_edge: Option<AudioClipTrimEdge>,
    #[schemars(with = "Option<TimeRangeSchema>")]
    pub trim_to: Option<oc_dsl::ast::TimeRange>,
}

impl ToolArguments for AudioClipEditRangeArgs {
    const ID: ToolId = ToolId::AudioClipEditRange;
    const FIELD_ORDER: &'static [&'static str] =
        &["clip_index", "split_at", "trim_edge", "trim_to"];
}

/// Time-stretch algorithm selected by one playback edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipStretchMode {
    Off,
    Percussive,
    Musical,
    Varispeed,
}

impl From<ClipStretchMode> for oc_dsl::ast::StretchMode {
    fn from(mode: ClipStretchMode) -> Self {
        match mode {
            ClipStretchMode::Off => Self::Off,
            ClipStretchMode::Percussive => Self::Percussive,
            ClipStretchMode::Musical => Self::Musical,
            ClipStretchMode::Varispeed => Self::Varispeed,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Arguments for this agent tool.
pub struct AudioClipPlaybackArgs {
    #[schemars(description = "Duration multiplier from 0.25 to 4; 0.5 is twice as fast")]
    pub stretch: Option<f64>,
    #[schemars(description = "Pitch in semitones from -24 to 24; fractional values are supported")]
    pub pitch: Option<f64>,
    #[schemars(description = "Independent formant shift in semitones, from -12 to 12")]
    pub formant: Option<f64>,
    #[schemars(description = "Compensate for the formant movement caused by pitch")]
    pub preserve_formants: Option<bool>,
    #[schemars(
        description = "Play the original media while retaining stretch, pitch, formants, and warp settings"
    )]
    pub bypass_processing: Option<bool>,
    #[schemars(with = "Option<WarpMarkersSchema>")]
    pub warp_markers: Option<Vec<oc_dsl::ast::AudioWarpMarker>>,
    #[schemars(with = "Option<BarBeatSchema>")]
    pub stretch_to: Option<oc_dsl::ast::BarBeat>,
    #[schemars(with = "Option<StretchModeSchema>")]
    pub stretch_mode: Option<ClipStretchMode>,
    #[schemars(
        description = "Fit the whole source to the clip range and reset source offset/loop; do not combine true with stretch"
    )]
    pub tempo_follow: Option<bool>,
}

impl From<AudioClipPlaybackArgs> for crate::commands::AudioClipPlayback {
    fn from(args: AudioClipPlaybackArgs) -> Self {
        Self {
            stretch: args.stretch,
            pitch: args.pitch,
            formant: args.formant,
            preserve_formants: args.preserve_formants,
            bypass_processing: args.bypass_processing,
            warp_markers: args.warp_markers,
            stretch_to: args.stretch_to,
            stretch_mode: args.stretch_mode.map(Into::into),
            tempo_follow: args.tempo_follow,
        }
    }
}

struct PlaybackSchema;

impl JsonSchema for PlaybackSchema {
    fn schema_name() -> String {
        "AudioClipPlayback".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema =
            <AudioClipPlaybackArgs as JsonSchema>::json_schema(generator).into_object();
        schema.object().additional_properties = Some(Box::new(false.into()));
        schema.into()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Change an audio clip's speed, pitch, formants, or warp anchors in one undoable edit. Get clip_index and expected_revision from audio_clips_list. Omitted fields keep their current values. Set stretch_to to resize and scale the clip together, like Alt-dragging its right edge. Supply stretch or stretch_to, never both. Varispeed links pitch to speed like tape. Clear pitch, formant, preserve_formants and warp_markers when switching to varispeed. Other edits keep the timeline range fixed. Stretch is output duration divided by source duration; explicit tempo_follow=true instead fits the whole source to the clip range and clears its source offset and loop. Do not supply stretch together with tempo_follow=true; a processing mode is required. Unrelated edits that omit tempo_follow preserve existing trim/loop settings. Pitch and formant are in semitones. Set preserve_formants to compensate for pitch-induced formant movement. Compare the same window with audition_range.
pub struct AudioClipSetPlaybackArgs {
    #[schemars(with = "i64")]
    #[schemars(description = "Zero-based clip index from audio_clips_list")]
    pub clip_index: usize,
    #[schemars(with = "PlaybackSchema")]
    pub playback: AudioClipPlaybackArgs,
}

impl ToolArguments for AudioClipSetPlaybackArgs {
    const ID: ToolId = ToolId::AudioClipSetPlayback;
    const FIELD_ORDER: &'static [&'static str] = &["clip_index", "playback"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn typed_audio_clip_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::AudioClipTransients,
            ToolId::AudioClipSetPlacement,
            ToolId::AudioClipEditRange,
            ToolId::AudioClipSetPlayback,
        ] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_audio_clip_decodes_nulls_and_legacy_keys() {
        let transients = AudioClipTransientsArgs::decode(&json!({
            "clip_index": 2,
            "source_start_seconds": null,
            "duration_seconds": 1.5,
            "limit": null,
            "extension": true,
        }))
        .unwrap();
        assert_eq!(transients.clip_index, 2);
        assert!(transients.source_start_seconds.is_none());
        assert_eq!(transients.duration_seconds, Some(1.5));
        assert!(transients.limit.is_none());

        let placement = AudioClipSetPlacementArgs::decode(&json!({
            "clip_index": 0,
            "start_seconds": null,
            "nudge_ms": -12.5,
            "expected_revision": 4,
        }))
        .unwrap();
        assert_eq!(placement.clip_index, 0);
        assert!(placement.start_seconds.is_none());
        assert_eq!(placement.nudge_ms, Some(-12.5));
    }

    #[test]
    fn typed_audio_clip_reports_a_missing_clip_index() {
        use crate::tool_registry::arguments::ArgumentErrorCode;
        let error = AudioClipTransientsArgs::decode(&json!({"duration_seconds": 1.0})).unwrap_err();
        assert_eq!(error.code, ArgumentErrorCode::Missing);
        assert_eq!(error.field.as_deref(), Some("clip_index"));
    }

    #[test]
    fn typed_audio_clip_decodes_range_and_playback_shapes() {
        let range = AudioClipEditRangeArgs::decode(&json!({
            "clip_index": 1,
            "trim_edge": "start",
            "trim_to": {
                "start": {"bar": 1, "beat": 2, "subdivision": 0},
                "end": {"bar": 2, "beat": 1, "subdivision": 0}
            },
            "expected_revision": 7,
        }))
        .unwrap();
        assert_eq!(range.trim_edge, Some(AudioClipTrimEdge::Start));
        let trim_to = range.trim_to.unwrap();
        assert_eq!(trim_to.start, oc_dsl::ast::BarBeat::new(1, 2, 0));
        assert_eq!(trim_to.end, oc_dsl::ast::BarBeat::new(2, 1, 0));

        let split = AudioClipEditRangeArgs::decode(&json!({
            "clip_index": 0,
            "split_at": {"bar": 3, "beat": 1, "subdivision": 0},
        }))
        .unwrap();
        assert_eq!(split.split_at, Some(oc_dsl::ast::BarBeat::new(3, 1, 0)));
        assert!(split.trim_to.is_none());

        let playback = AudioClipSetPlaybackArgs::decode(&json!({
            "clip_index": 0,
            "playback": {
                "stretch": 1.5,
                "stretch_mode": "varispeed",
                "stretch_to": {"bar": 2, "beat": 1, "subdivision": 0},
                "warp_markers": [{"source_seconds": 0.5, "target_seconds": 0.6}],
                "tempo_follow": false
            },
        }))
        .unwrap();
        assert_eq!(playback.playback.stretch, Some(1.5));
        assert_eq!(
            playback.playback.stretch_mode,
            Some(ClipStretchMode::Varispeed)
        );
        assert_eq!(playback.playback.warp_markers.unwrap().len(), 1);
    }

    #[test]
    fn typed_playback_rejects_an_unknown_field() {
        let error = AudioClipSetPlaybackArgs::decode(&json!({
            "clip_index": 0,
            "playback": {"extension": true},
        }))
        .unwrap_err();
        assert!(error.to_string().contains("extension"), "{error}");
    }
}
