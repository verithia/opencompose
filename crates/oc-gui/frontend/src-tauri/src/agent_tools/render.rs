//! Typed arguments for render tools.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ToolArguments, empty_arguments};

empty_arguments! { RenderCompatReportArgs => RenderCompatReport }

#[derive(Debug, Deserialize, JsonSchema)]
/// Export the whole mix, one file per track and bus, and stems.json into an existing directory, exactly like the toolbar's Export stems. Use it to hand stems to a mix engineer or to read every part's level at once; for one window of the song use render_range with stems instead. The directory must already exist. The set uses the project's export format at the app's current output sample rate (Preferences → Audio; this call cannot override either), and the mix is normalised to the project loudness target while the stems are not. Returns the manifest path, shared container, sample format, dither, sample rate and channels, and each file's absolute path, frames, peak, RMS and integrated LUFS; a render superseded by a newer one is refused.
pub struct RenderStemsArgs {
    #[schemars(description = "Existing directory to write the stem set into")]
    pub directory: String,
}

impl ToolArguments for RenderStemsArgs {
    const ID: ToolId = ToolId::RenderStems;
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Render the project to a WAV file (faster than realtime). The sample depth and dither default to the project's export setting.
pub struct RenderOfflineArgs {
    #[schemars(description = "Output WAV path")]
    pub path: String,
    #[serde(rename = "preRoll")]
    #[schemars(
        description = "Wall-clock warm-up per plugin instance before rendering (0..600 s, default 0)"
    )]
    pub pre_roll: Option<f64>,
    #[serde(rename = "pre_roll")]
    #[schemars(description = "Deprecated alias for preRoll")]
    pub pre_roll_alias: Option<f64>,
    #[schemars(
        description = "WAV sample depth: i16, i24 or f32; omit to use the project export setting"
    )]
    pub sample_format: Option<String>,
    #[schemars(
        description = "TPDF dither at one LSB before integer quantisation; refused for f32; omit to use the project export setting"
    )]
    pub dither: Option<bool>,
}

impl ToolArguments for RenderOfflineArgs {
    const ID: ToolId = ToolId::RenderOffline;
    const FIELD_ORDER: &'static [&'static str] =
        &["path", "preRoll", "pre_roll", "sample_format", "dither"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Render a time window in seconds or bars as an exact slice of the full render, served by the render cache. Optional track subset (a slice with the other tracks muted) and WAV stems. Returns the master path and each stem path with frame count, sample rate and channels, at the app's current output sample rate (Preferences → Audio) and pre-normalisation (a project loudness target applies to render_offline, not here). The result includes pre_encode peak and clipping evidence for the mix and each stem. To hear the window, use `audition_range`.
pub struct RenderRangeArgs {
    #[schemars(description = "Output master WAV path")]
    pub path: String,
    #[schemars(description = "Range start in seconds; use with end")]
    pub start: Option<f64>,
    #[schemars(description = "Range end in seconds; at most 600 s after start")]
    pub end: Option<f64>,
    #[schemars(description = "One-indexed start bar; use with end_bar instead of seconds")]
    pub start_bar: Option<u32>,
    #[schemars(description = "Exclusive end bar; at most 600 s after start_bar")]
    pub end_bar: Option<u32>,
    #[schemars(description = "Optional track indices; omit for the full mix")]
    pub tracks: Option<Vec<usize>>,
    #[schemars(
        description = "Save 32-bit float WAV without changing project settings. Use a .wav path"
    )]
    pub float_wav: Option<bool>,
    #[schemars(description = "Write one WAV per selected track (default false)")]
    pub stems: Option<bool>,
    #[serde(rename = "preRoll")]
    #[schemars(
        description = "Wall-clock warm-up per plugin instance before rendering (0..600 s, default 0)"
    )]
    pub pre_roll: Option<f64>,
    #[serde(rename = "pre_roll")]
    #[schemars(description = "Deprecated alias for preRoll")]
    pub pre_roll_alias: Option<f64>,
    #[schemars(
        description = "Render one bus's processed return instead of the master mix. The whole graph keeps running, so a track feeding a sidechain key still ducks the return. Do not combine with tracks"
    )]
    pub bus: Option<String>,
}

impl ToolArguments for RenderRangeArgs {
    const ID: ToolId = ToolId::RenderRange;
    const FIELD_ORDER: &'static [&'static str] = &[
        "path",
        "start",
        "end",
        "start_bar",
        "end_bar",
        "tracks",
        "float_wav",
        "stems",
        "preRoll",
        "pre_roll",
    ];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Render a time window exactly like render_range and return a WAV of it inline, so you can listen to the part you are working on. The clip is shaped for the model's audio input: mono at 16 kHz by default (the rate and channel count the provider keeps), and `channel` selects which side you hear — mono is the equal-weight downmix, left or right isolates one side, stereo keeps both when the model accepts two channels. `clip_sample_rate` overrides the inline rate (8000..48000). Windows follow render_range's rules; longer than 30 s returns the first 30 s with truncated=true. The full render is written to the requested path or temporary scratch. The pre_encode measurements describe that full-rate window and its saved format, even when the inline clip is truncated. A saved panel session may keep an operator copy subject to its archive format and storage limits.
pub struct AuditionRangeArgs {
    #[schemars(
        description = "Output master WAV path; omit for a bounded scratch path the app may reclaim"
    )]
    pub path: Option<String>,
    #[schemars(description = "Range start in seconds; use with end")]
    pub start: Option<f64>,
    #[schemars(description = "Range end in seconds; at most 600 s after start")]
    pub end: Option<f64>,
    #[schemars(description = "One-indexed start bar; use with end_bar instead of seconds")]
    pub start_bar: Option<u32>,
    #[schemars(description = "Exclusive end bar; at most 600 s after start_bar")]
    pub end_bar: Option<u32>,
    #[schemars(description = "Optional track indices; omit for the full mix")]
    pub tracks: Option<Vec<usize>>,
    #[schemars(
        description = "Save the full-rate output as 32-bit float WAV. The inline listening format stays the same"
    )]
    pub float_wav: Option<bool>,
    #[schemars(description = "Which side to hear: mono (downmix, default), left, right or stereo")]
    pub channel: Option<String>,
    #[schemars(description = "Inline clip rate in Hz, 8000..48000; default 16000")]
    pub clip_sample_rate: Option<u32>,
}

impl ToolArguments for AuditionRangeArgs {
    const ID: ToolId = ToolId::AuditionRange;
    const FIELD_ORDER: &'static [&'static str] = &[
        "path",
        "start",
        "end",
        "start_bar",
        "end_bar",
        "tracks",
        "float_wav",
        "channel",
        "clip_sample_rate",
    ];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Hear a bounded window from an unprocessed clip source, an attached audio reference, or a saved arrangement render. Use exactly one of clip_index from audio_clips_list or an exact path from the current AI conversation. Times are relative to the source file, including for saved renders. Compare with audition_range for the current edited arrangement. Returns inline WAV and the actual source range heard. Does not write the project.
pub struct AuditionClipSourceArgs {
    #[schemars(description = "Zero-based clip index from audio_clips_list")]
    pub clip_index: Option<usize>,
    #[schemars(
        description = "Exact absolute path of an attached local audio reference or saved arrangement render from the current AI conversation; omit when using clip_index"
    )]
    pub path: Option<String>,
    #[schemars(description = "Nonnegative source time, default 0")]
    pub source_start_seconds: Option<f64>,
    #[schemars(description = "Duration greater than 0 and at most 30 seconds, default 8")]
    pub duration_seconds: Option<f64>,
    #[schemars(description = "Mono (default), left, right, or stereo")]
    pub channel: Option<String>,
    #[schemars(description = "Inline rate in Hz, 8000..48000; default 16000")]
    pub clip_sample_rate: Option<u32>,
}

impl ToolArguments for AuditionClipSourceArgs {
    const ID: ToolId = ToolId::AuditionClipSource;
    const FIELD_ORDER: &'static [&'static str] = &[
        "clip_index",
        "path",
        "source_start_seconds",
        "duration_seconds",
        "channel",
        "clip_sample_rate",
    ];
}
