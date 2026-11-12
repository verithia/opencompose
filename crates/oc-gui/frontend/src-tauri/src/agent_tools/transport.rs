//! Typed transport requests delegate to the shared playback commands.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::commands;
use crate::state::AppState;
use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ArgumentError, ToolArguments, empty_arguments};

empty_arguments! {
    PlayArgs => TransportPlay,
    PauseArgs => TransportPause,
    StopArgs => TransportStop,
    StateArgs => TransportState,
    MuteArgs => TransportMute,
    UnmuteArgs => TransportUnmute,
}
#[derive(Debug, Deserialize, JsonSchema)]
/// Seek to a beat position.
pub struct SeekArgs {
    /// Non-negative beat
    pub beat: f64,
}
impl ToolArguments for SeekArgs {
    const ID: ToolId = ToolId::TransportSeek;
}

fn inline_schema(value: Value) -> Schema {
    serde_json::from_value(value).expect("an inline tool schema is valid")
}

/// A tempo point before conversion to the project map. Optional fields keep
/// their authored defaults without advertising schema defaults.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempoPointArgs {
    pub bar: u32,
    pub beat: Option<u32>,
    pub subdivision: Option<u32>,
    pub bpm: f64,
}

/// Tempo-map points in wire form.
pub struct TempoPointsSchema;

impl JsonSchema for TempoPointsSchema {
    fn schema_name() -> String {
        "TempoPoints".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": "array",
            "description": "Tempo points, ascending; an empty array clears the map back to the project tempo",
            "items": {
                "type": "object",
                "properties": {
                    "bar": {"type": "integer", "minimum": 1, "description": "One-indexed bar of the change"},
                    "beat": {"type": "integer", "minimum": 1, "description": "Beat in that bar (default 1)"},
                    "subdivision": {"type": "integer", "minimum": 0, "maximum": 3, "description": "Quarter-beat subdivision (default 0)"},
                    "bpm": {"type": "number", "description": "Tempo from this point on"}
                },
                "required": ["bar", "bpm"],
                "additionalProperties": false
            }
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Replace the tempo map with the given points, as one undoable Transport edit; an empty list returns the song to the project tempo. Use it when the tempo has to change inside the song (e.g. after bar 9) — project_set_tempo only moves the starting tempo. Points must be strictly ascending, and each tempo 1-400 BPM; an invalid map is refused with the points that were sent and leaves the project untouched. Returns the written points, the project tempo and the new revision.
pub struct TempoMapSetArgs {
    #[schemars(with = "TempoPointsSchema")]
    pub points: Vec<TempoPointArgs>,
}

impl TempoMapSetArgs {
    /// Convert wire tempo points into AST points.
    pub fn points(&self) -> Vec<oc_dsl::ast::TempoPoint> {
        self.points
            .iter()
            .map(|point| oc_dsl::ast::TempoPoint {
                position: oc_dsl::ast::BarBeat::new(
                    point.bar,
                    point.beat.unwrap_or(1),
                    point.subdivision.unwrap_or(0),
                ),
                offset_quarters: 0.0,
                bpm: point.bpm,
            })
            .collect()
    }
}

impl ToolArguments for TempoMapSetArgs {
    const ID: ToolId = ToolId::TempoMapSet;
    const FIELD_ORDER: &'static [&'static str] = &["points"];
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// One meter-map point.
pub struct MeterPointArgs {
    pub bar: u32,
    pub numerator: u8,
    pub denominator: u8,
}

/// Meter-map points in wire form.
pub struct MeterPointsSchema;

impl JsonSchema for MeterPointsSchema {
    fn schema_name() -> String {
        "MeterPoints".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": "array",
            "description": "Meter points at bar starts, ascending; an empty array clears the map back to the base meter",
            "items": {
                "type": "object",
                "properties": {
                    "bar": {"type": "integer", "minimum": 1, "description": "One-indexed bar of the change"},
                    "numerator": {"type": "integer", "minimum": 1, "maximum": 32},
                    "denominator": {"type": "integer", "enum": [1, 2, 4, 8, 16, 32, 64]}
                },
                "required": ["bar", "numerator", "denominator"],
                "additionalProperties": false
            }
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Replace the time-signature map with the given bar points, as one undoable Transport edit; an empty list returns to the base meter. Use it for meter changes inside the song (3/4 after bar 9). A bar-1 point overrides the base meter for playback, while the toolbar and project.properties keep showing the base field, so set both with project_edit when the whole song's meter changes. Arranged content keeps its absolute musical time. Bars are one-indexed and strictly ascending, and each meter is 1-32 over 1/2/4/8/16/32/64; an invalid map is refused with the points that were sent and leaves the project untouched. Returns the written points, the base meter and the new revision.
pub struct TimeSignatureSetArgs {
    #[schemars(with = "MeterPointsSchema")]
    pub points: Vec<MeterPointArgs>,
}

impl TimeSignatureSetArgs {
    /// Convert wire meter points into AST points.
    pub fn points(&self) -> Vec<oc_dsl::ast::TimeSigPoint> {
        self.points
            .iter()
            .map(|point| oc_dsl::ast::TimeSigPoint {
                position: oc_dsl::ast::BarBeat::new(point.bar, 1, 0),
                numerator: point.numerator,
                denominator: point.denominator,
            })
            .collect()
    }
}

impl ToolArguments for TimeSignatureSetArgs {
    const ID: ToolId = ToolId::TimeSignatureSet;
    const FIELD_ORDER: &'static [&'static str] = &["points"];
}

#[derive(Debug)]
/// Transport tool request variants.
pub enum Request {
    Play(PlayArgs),
    Pause(PauseArgs),
    Stop(StopArgs),
    Seek(SeekArgs),
    State(StateArgs),
    Mute(MuteArgs),
    Unmute(UnmuteArgs),
}
impl Request {
    /// Decode a tagged transport request.
    pub fn decode(id: ToolId, args: &Value) -> Result<Self, ArgumentError> {
        match id {
            PlayArgs::ID => PlayArgs::decode(args).map(Self::Play),
            PauseArgs::ID => PauseArgs::decode(args).map(Self::Pause),
            StopArgs::ID => StopArgs::decode(args).map(Self::Stop),
            SeekArgs::ID => SeekArgs::decode(args).map(Self::Seek),
            StateArgs::ID => StateArgs::decode(args).map(Self::State),
            MuteArgs::ID => MuteArgs::decode(args).map(Self::Mute),
            UnmuteArgs::ID => UnmuteArgs::decode(args).map(Self::Unmute),
            _ => Err(ArgumentError::wrong_tool(id)),
        }
    }
}

/// Execute a decoded transport request.
pub fn execute(app: &AppHandle, request: Request) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let snapshot = match request {
        Request::Play(_) => tauri::async_runtime::block_on(commands::transport_play(app.clone()))?,
        Request::Pause(_) => commands::transport_pause(state)?,
        Request::Stop(_) => commands::transport_stop(state)?,
        Request::Seek(args) => commands::transport_seek(args.beat, state)?,
        Request::State(_) => commands::get_transport_state(state)?,
        Request::Mute(_) => {
            commands::transport_mute(state)?;
            return Ok(json!({"muted":true}));
        }
        Request::Unmute(_) => {
            commands::transport_unmute(state)?;
            return Ok(json!({"muted":false}));
        }
    };
    serde_json::to_value(snapshot).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_decodes_numbers_and_rejects_malformed_requests_before_device_access() {
        for value in [
            json!({"beat":0}),
            json!({"beat":12.5, "legacy_extension":true}),
        ] {
            assert!(matches!(
                Request::decode(ToolId::TransportSeek, &value).unwrap(),
                Request::Seek(_)
            ));
        }
        for value in [
            json!({}),
            json!({"beat":null}),
            json!({"beat":"12"}),
            json!([]),
        ] {
            assert!(Request::decode(ToolId::TransportSeek, &value).is_err());
        }
        let missing = Request::decode(ToolId::TransportSeek, &json!({})).unwrap_err();
        assert_eq!(missing.tool, ToolId::TransportSeek);
        assert_eq!(
            missing.code,
            crate::tool_registry::arguments::ArgumentErrorCode::Missing
        );
        assert_eq!(missing.field.as_deref(), Some("beat"));
        assert_eq!(missing.to_string(), "missing argument 'beat'");
        let invalid = Request::decode(ToolId::TransportSeek, &json!({"beat":"12"})).unwrap_err();
        assert_eq!(
            invalid.code,
            crate::tool_registry::arguments::ArgumentErrorCode::InvalidType
        );
        assert_eq!(invalid.field.as_deref(), Some("beat"));
        assert!(Request::decode(ToolId::ReadSkill, &json!({"beat":0})).is_err());
    }

    #[test]
    fn all_empty_transport_requests_keep_legacy_external_payload_compatibility() {
        for id in [
            PlayArgs::ID,
            PauseArgs::ID,
            StopArgs::ID,
            StateArgs::ID,
            MuteArgs::ID,
            UnmuteArgs::ID,
        ] {
            for value in [Value::Null, json!([]), json!({"legacy_extension":true})] {
                Request::decode(id, &value).unwrap();
            }
        }
    }

    #[test]
    fn transport_schemas_derive_from_the_requests_the_handler_consumes() {
        let expected = crate::tool_registry::arguments::schema::<SeekArgs>();
        let spec = crate::tool_registry::panel_tool("transport_seek").unwrap();
        assert_eq!(spec.schema, expected);
        assert_eq!(spec.schema["required"], json!(["beat"]));
        assert!(crate::tool_registry::validate_args(&spec, &json!({"beat":null})).is_err());
        assert!(
            crate::tool_registry::validate_args(&spec, &json!({"beat":0, "unknown":true})).is_err()
        );
    }

    #[test]
    fn typed_maps_supply_the_authored_point_defaults() {
        let tempo = TempoMapSetArgs::decode(&json!({
            "points": [{"bar": 5, "bpm": 140.0}],
        }))
        .unwrap();
        let points = tempo.points();
        assert_eq!(points[0].position, oc_dsl::ast::BarBeat::new(5, 1, 0));
        assert_eq!(points[0].bpm, 140.0);

        let meter = TimeSignatureSetArgs::decode(&json!({
            "points": [{"bar": 9, "numerator": 3, "denominator": 4}],
        }))
        .unwrap();
        let points = meter.points();
        assert_eq!(points[0].position, oc_dsl::ast::BarBeat::new(9, 1, 0));
        assert_eq!((points[0].numerator, points[0].denominator), (3, 4));
    }

    #[test]
    fn typed_map_schemas_preserve_the_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [ToolId::TempoMapSet, ToolId::TimeSignatureSet] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }
}
