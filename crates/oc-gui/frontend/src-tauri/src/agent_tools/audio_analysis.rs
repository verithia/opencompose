//! Typed audio-evidence requests shared by the panel and external agent API.

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ArgumentError, ToolArguments, typed_arguments};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
/// Mono-fold octave-loss measurement window.
pub struct MonoFoldWindow {
    #[schemars(description = "File start in seconds (default 0)")]
    pub start: Option<f64>,
    #[schemars(description = "Passage duration from 0.5 to 30 seconds")]
    pub duration: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Direct sample peak and RMS measurement window.
pub struct LevelWindow {
    #[schemars(description = "File start in seconds (default 0)")]
    pub start: Option<f64>,
    #[schemars(description = "Passage duration greater than 0 and at most 30 seconds")]
    pub duration: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Attack detection window with spacing and count limits.
pub struct TransientWindow {
    #[schemars(description = "File start in seconds (default 0)")]
    pub start: Option<f64>,
    #[schemars(description = "Passage duration greater than 0 and at most 20 seconds")]
    pub duration: f64,
    #[schemars(description = "Minimum spacing in milliseconds, 1 to 250 (default 35)")]
    pub min_gap_ms: Option<f64>,
    #[schemars(with = "Option<i64>")]
    #[schemars(description = "Maximum number of attacks, 1 to 128 (default 24)")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Pulse and beat-grid measurement window.
pub struct BeatGridWindow {
    #[schemars(description = "Source start in seconds (default 0)")]
    pub start: Option<f64>,
    #[schemars(description = "Passage duration in seconds (4 to 30)")]
    pub duration: f64,
}

// Inline schemas preserve the provider's existing nested object shape while
// Option retains omission semantics; the request structs own all field types.
macro_rules! inline_window_schema {
    ($inline:ident, $window:ty) => {
        struct $inline;
        impl JsonSchema for $inline {
            fn schema_name() -> String {
                stringify!($window).into()
            }
            fn is_referenceable() -> bool {
                false
            }
            fn json_schema(
                generator: &mut schemars::r#gen::SchemaGenerator,
            ) -> schemars::schema::Schema {
                let mut schema = <$window>::json_schema(generator).into_object();
                schema.object().additional_properties = Some(Box::new(false.into()));
                schema.into()
            }
        }
    };
}
inline_window_schema!(InlineMonoFoldWindow, MonoFoldWindow);
inline_window_schema!(InlineLevelWindow, LevelWindow);
inline_window_schema!(InlineTransientWindow, TransientWindow);
inline_window_schema!(InlineBeatGridWindow, BeatGridWindow);

typed_arguments! {
    /// Analyze audio levels, spectrum and silence for a WAV or decoded media file.
    pub struct AnalyzeArgs => AudioAnalyze {
        #[schemars(description = "Absolute audio file path, e.g. from render_range or a conversation reference") ]
        pub path: String,
        #[schemars(with = "Option<i64>")]
        #[schemars(description = "Number of spectral peaks to return in levels mode (0-64, default 8)") ]
        pub top_peaks: Option<usize>,
        #[schemars(description = "Measure octave-band loss against the stereo channel mean without normalization. Choose only one of mono_fold, level_window, beat_grid and transients.", with = "Option<InlineMonoFoldWindow>")]
        pub mono_fold: Option<MonoFoldWindow>,
        #[schemars(description = "Read sample peak and RMS directly on a file window. Choose only one of mono_fold, level_window, beat_grid and transients.", with = "Option<InlineLevelWindow>")]
        pub level_window: Option<LevelWindow>,
        #[schemars(description = "Optional attack evidence mode. The passage must fit within the file.", with = "Option<InlineTransientWindow>")]
        pub transients: Option<TransientWindow>,
        #[schemars(description = "Optional pulse evidence mode. The passage must fit within the file.", with = "Option<InlineBeatGridWindow>")]
        pub beat_grid: Option<BeatGridWindow>,
    }
    /// Analyze and transcribe an external audio file or stem into notes: note events (start, end, MIDI number, name, cents off that pitch, detector confidence) with one monophonic detector; prefer writing the notes from what you hear, and use this only for audio whose notes are not written in the project DSL and that you cannot transcribe reliably by ear. Note times are relative to `start`, and the window may be a section. Every note also carries `start_beat` (quarter-note beats from the window start, 0-based), `duration_beats`, `bar` (1-based) and `suggested_duration`, the nearest DSL token (w, h, q, 8th, 16th, 32nd, 64th, dotted or triplet). Derive the duration token from `suggested_duration`; do not invent one. An octave is only decided when it beats the other octave by 6 dB on its own fundamental: a note whose sub-octave or octave-up layer is that close carries `octave_ambiguous: true` and `octave_runner_up` (the MIDI number it would take instead), and a repeated pitch holds its earlier octave while the reading stays ambiguous. When a note you need is flagged, do not pick an octave from it — re-run on a window where that voice sounds alone (or a stem), and say which octave you could not pin down. The top level reports the grid the beats assume (`tempo_bpm`, `time_signature`, `beats_per_bar`, `beat_seconds`, `quantise` tolerance); it comes from the tempo/time_signature arguments, else this WAV's render metadata, else the `project` you pass, and only then the engine default 120 BPM 4/4 with a warning. If the external tempo is unknown, ask audio_analyze for beat_grid evidence before supplying a confirmed grid. A note more than `quantise` beats off the 1/16 grid is named in `warnings` and its suggestion is a guess. A chord sets `polyphonic: true` and only the strongest voice is transcribed; drums, chords and ornaments under about 50 ms are not. Use `audition_range` to hear the same window.
    pub struct NotesArgs => AnalyzeNotes {
        #[schemars(description = "Absolute WAV path, e.g. from render_range") ]
        pub path: String,
        #[schemars(description = "Window start in seconds (default 0)") ]
        pub start: Option<f64>,
        #[schemars(description = "Exclusive window end in seconds (default the file end)") ]
        pub end: Option<f64>,
        #[schemars(description = "Drop notes scored below this detector confidence (0-1, default 0)") ]
        pub min_confidence: Option<f64>,
        #[schemars(with = "Option<i64>")]
        #[schemars(description = "Maximum notes to return, earliest first (1-1024, default 256)") ]
        pub max_notes: Option<usize>,
        #[schemars(description = "Tempo in BPM for the beat fields; default the render's project tempo, else the engine default 120 (warned)") ]
        pub tempo: Option<f64>,
        #[schemars(description = "Time signature such as \"4/4\" or \"3/4\"; default the render's project meter, else 4/4 (warned)") ]
        pub time_signature: Option<String>,
        #[schemars(description = "Snap start_beat and duration_beats to the nearest 1/16 and also report start_beat_quantised and duration_beats_quantised (default false: the reported beats stay the measurement)") ]
        pub quantize: Option<bool>,
        #[schemars(description = "Optional .ocp path to read tempo and time_signature from when the WAV is not a render_range output") ]
        pub project: Option<String>,
    }
    /// Compare two audio files (WAV or supported compressed media such as MP3/FLAC) using shared comparison definitions: LUFS delta, per-octave and body deltas, envelope and waveform correlation, max sample difference, level-matched log-mel error, tilt, crest/attack, bass harmonics and headline score. Compressed media is decoded to stereo at its native sample rate. Both analyzed signals must share a sample rate and frame count; render both sides at the same rate, which is the app's current output rate (Preferences → Audio). Different lengths or sample rates are refused unless allow_mismatch=true. For a passage, specify duration (up to 30 seconds) and independent reference_start and candidate_start in source seconds. A passage must fit both files. Set listen=true with a 0.4–8 second passage to receive two level-matched listening copies when measurable. They preserve source timing; the report verifies the actual delivered PCM. The listening comparison is labeled, so it is open rather than blind.
    pub struct CompareArgs => AudioCompare {
        #[schemars(description = "Reference audio file path (WAV, MP3, FLAC, or other supported media)") ]
        pub reference: String,
        #[schemars(description = "Candidate audio file path (WAV, MP3, FLAC, or other supported media)") ]
        pub candidate: String,
        #[schemars(description = "Reference passage start in source seconds, default 0. Requires duration.") ]
        pub reference_start: Option<f64>,
        #[schemars(description = "Candidate passage start in source seconds, default 0. Requires duration.") ]
        pub candidate_start: Option<f64>,
        #[schemars(description = "Shared passage length in seconds, greater than 0 and at most 30; omit for whole files") ]
        pub duration: Option<f64>,
        #[schemars(description = "Deliver a reference and candidate WAV pair with verified playback loudness; requires a duration of 0.4–8 seconds") ]
        pub listen: Option<bool>,
        #[schemars(description = "With listen=true, return the matched pair and source windows without alignment or similarity metrics. Each source is converted independently to the listening rate.") ]
        pub listening_only: Option<bool>,
        #[schemars(description = "Listening copies only: mono (default), left, right or stereo") ]
        pub channel: Option<String>,
        #[schemars(with = "Option<i64>")]
        #[schemars(description = "Listening copies only: delivery rate in Hz, 8000..48000; default 16000") ]
        pub clip_sample_rate: Option<u32>,
        #[schemars(description = "Use the narrower stem alignment window (default false)") ]
        pub stems: Option<bool>,
        #[schemars(description = "Resample and trim unequal lengths/rates instead of refusing (default false)") ]
        pub allow_mismatch: Option<bool>,
    }
}

#[derive(Debug)]
/// Decoded request for the audio-analysis tools.
pub enum Request {
    Analyze(AnalyzeArgs),
    Notes(NotesArgs),
    Compare(CompareArgs),
}
impl Request {
    /// Decode a tool call into a request.
    pub fn decode(id: ToolId, value: &Value) -> Result<Self, ArgumentError> {
        match id {
            ToolId::AudioAnalyze => AnalyzeArgs::decode(value).map(Self::Analyze),
            ToolId::AnalyzeNotes => NotesArgs::decode(value).map(Self::Notes),
            ToolId::AudioCompare => CompareArgs::decode(value).map(Self::Compare),
            _ => Err(ArgumentError::wrong_tool(id)),
        }
    }
}

/// Execute a decoded audio-analysis request.
pub fn execute(request: Request) -> Result<Value, String> {
    match request {
        Request::Analyze(args) => crate::agent_api::audio_analyze_request(args),
        Request::Notes(args) => crate::agent_api::analyze_notes_request(args),
        Request::Compare(args) => crate::agent_api::audio_compare_request(args),
    }
}

/// Decode and execute one audio-analysis call.
pub fn dispatch(id: ToolId, value: &Value) -> Result<Value, String> {
    execute(Request::decode(id, value).map_err(|error| error.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::arguments::ArgumentErrorCode;
    use serde_json::json;

    #[test]
    fn typed_audio_analysis_schemas_preserve_each_existing_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::AudioAnalyze,
            ToolId::AnalyzeNotes,
            ToolId::AudioCompare,
        ] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
            assert!(!id.metadata().mutation);
        }
    }

    #[test]
    fn typed_audio_analysis_decodes_nested_windows_and_external_nulls_once() {
        let request = Request::decode(ToolId::AudioAnalyze, &json!({
            "path":"source.wav", "top_peaks":null, "mono_fold":null,
            "beat_grid":null, "level_window":null,
            "transients":{"start":null,"duration":0.25,"min_gap_ms":null,"limit":null,"extension":true},
            "extension":true,
        })).unwrap();
        let Request::Analyze(args) = request else {
            panic!("wrong request family")
        };
        assert_eq!(args.path, "source.wav");
        assert!(args.top_peaks.is_none());
        let window = args.transients.unwrap();
        assert_eq!(window.duration, 0.25);
        assert!(window.start.is_none());
        assert!(window.limit.is_none());
        let Request::Notes(args) = Request::decode(
            ToolId::AnalyzeNotes,
            &json!({
                "path":"source.wav", "start":null, "end":null, "min_confidence":null,
                "max_notes":null, "tempo":null, "time_signature":null, "project":null,
                "quantize":null, "extension":true,
            }),
        )
        .unwrap() else {
            panic!("wrong request family")
        };
        assert!(args.max_notes.is_none());
        assert!(args.quantize.is_none());
        let Request::Compare(args) = Request::decode(
            ToolId::AudioCompare,
            &json!({
                "reference":"source.wav", "candidate":"render.wav", "listen":null,
                "channel":null, "clip_sample_rate":null, "stems":null, "allow_mismatch":null,
                "extension":true,
            }),
        )
        .unwrap() else {
            panic!("wrong request family")
        };
        assert!(args.listen.is_none());
        assert!(args.clip_sample_rate.is_none());
    }

    #[test]
    fn typed_audio_analysis_dispatch_refuses_malformed_arguments_before_media_io() {
        for id in [
            ToolId::AudioAnalyze,
            ToolId::AnalyzeNotes,
            ToolId::AudioCompare,
        ] {
            let error = Request::decode(id, &json!({})).unwrap_err();
            assert_eq!(error.code, ArgumentErrorCode::Missing);
            assert_eq!(
                error.field.as_deref(),
                Some(if id == ToolId::AudioCompare {
                    "reference"
                } else {
                    "path"
                })
            );
        }
        for (id, value) in [
            (ToolId::AudioAnalyze, json!({"path":true})),
            (
                ToolId::AudioAnalyze,
                json!({"path":"absent.wav", "top_peaks":-1}),
            ),
            (
                ToolId::AudioAnalyze,
                json!({"path":"absent.wav", "level_window":{}}),
            ),
            (
                ToolId::AudioAnalyze,
                json!({"path":"absent.wav", "beat_grid":true}),
            ),
            (
                ToolId::AudioAnalyze,
                json!({"path":"absent.wav", "transients":{"duration":"0.5"}}),
            ),
            (
                ToolId::AnalyzeNotes,
                json!({"path":"absent.wav", "max_notes":-1}),
            ),
            (
                ToolId::AnalyzeNotes,
                json!({"path":"absent.wav", "time_signature":4}),
            ),
            (
                ToolId::AudioCompare,
                json!({"reference":"absent.wav", "candidate":null}),
            ),
            (
                ToolId::AudioCompare,
                json!({"reference":"absent.wav", "candidate":"absent.wav", "duration":"1"}),
            ),
            (
                ToolId::AudioCompare,
                json!({"reference":"absent.wav", "candidate":"absent.wav", "clip_sample_rate":4_294_967_296u64}),
            ),
        ] {
            let error = dispatch(id, &value).unwrap_err();
            assert!(!error.contains("failed:"), "{error}");
            assert!(Request::decode(id, &value).is_err(), "{id:?}: {value}");
        }
        assert_eq!(
            dispatch(
                ToolId::AudioCompare,
                &json!({
                    "reference":"absent.wav", "candidate":"absent.wav", "listening_only":true,
                })
            )
            .unwrap_err(),
            "listening_only requires listen=true"
        );
        assert_eq!(
            dispatch(
                ToolId::AudioAnalyze,
                &json!({
                    "path":"absent.wav", "mono_fold":{"duration":1}, "beat_grid":{"duration":4},
                })
            )
            .unwrap_err(),
            "Choose one of mono_fold, level_window, beat_grid or transients"
        );
    }

    #[test]
    fn typed_audio_analysis_dispatch_executes_generated_media_with_existing_defaults() {
        let path = oc_audio::private_fs::test_root().join("typed-audio-evidence.wav");
        let rate = 8_000;
        let samples: Vec<f32> = (0..rate)
            .flat_map(|frame| {
                let sample =
                    (std::f32::consts::TAU * 220.0 * frame as f32 / rate as f32).sin() * 0.25;
                [sample, sample]
            })
            .collect();
        oc_engine::wav::write_stereo_f32_wav(&path, &samples, rate).unwrap();
        let levels = dispatch(
            ToolId::AudioAnalyze,
            &json!({
                "path":path, "level_window":{"start":null,"duration":0.5},
                "legacy_extension":true,
            }),
        )
        .unwrap();
        assert_eq!(levels["sample_rate"], rate);
        assert_eq!(levels["level_window"]["start_s"], 0.0);
        assert_eq!(levels["level_window"]["end_s"], 0.5);
        let comparison = dispatch(
            ToolId::AudioCompare,
            &json!({
                "reference":path, "candidate":path, "duration":null, "listen":null,
                "stems":null, "allow_mismatch":null, "legacy_extension":true,
            }),
        )
        .unwrap();
        assert_eq!(comparison["sample"]["max_abs_difference"], 0.0);
        assert!(comparison.get("listening").is_none());
        let notes = dispatch(
            ToolId::AnalyzeNotes,
            &json!({
                "path":path, "tempo":120, "time_signature":"4/4", "max_notes":null,
                "quantize":null, "legacy_extension":true,
            }),
        )
        .unwrap();
        assert!(!notes["notes"].as_array().unwrap().is_empty(), "{notes}");
        // The public helpers and protocol dispatcher share exactly one decoder
        // and executor, including nested defaults and extension compatibility.
        assert_eq!(
            crate::agent_api::audio_analyze_tool(&json!({
                "path":path,"level_window":{"duration":0.5}
            }))
            .unwrap(),
            levels
        );
    }
}
