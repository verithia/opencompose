//! Error types for the OpenCompose DSL

use thiserror::Error;

/// Errors that occur during parsing
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Syntax error at line {line}, column {col}: {message}")]
    Syntax {
        line: usize,
        col: usize,
        message: String,
    },

    #[error("Unexpected token: expected {expected}, found '{found}'")]
    UnexpectedToken {
        expected: String,
        found: String,
        line: usize,
        col: usize,
    },

    #[error("Unexpected end of input")]
    UnexpectedEOF,

    #[error("Invalid pitch: '{0}'")]
    InvalidPitch(String),

    #[error("Invalid duration: '{0}'")]
    InvalidDuration(String),

    #[error("Invalid dynamics: '{0}'")]
    InvalidDynamics(String),

    #[error("Invalid instrument path: '{0}' (expected category/name)")]
    InvalidInstrumentPath(String),
}

/// Errors that occur during semantic validation
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("Unknown instrument: '{path}' (in track '{track}')")]
    UnknownInstrument { track: String, path: String },

    #[error(
        "Invalid articulation '{articulation}' for instrument family '{family}' (in track '{track}')"
    )]
    InvalidArticulation {
        track: String,
        family: String,
        articulation: String,
    },

    #[error(
        "Pitch {pitch} is out of range for instrument '{instrument}' (valid range: {low}-{high})"
    )]
    PitchOutOfRange {
        instrument: String,
        pitch: String,
        low: String,
        high: String,
    },

    #[error("Bus routing cycle detected: {cycle}")]
    RoutingCycle { cycle: String },

    #[error("Sidechain key cycle detected: {cycle}")]
    SidechainCycle { cycle: String },

    #[error(
        "Sidechain key '{key}' in {owner} names its own chain; a post-insert key cannot read its own output"
    )]
    SidechainSelfKey { owner: String, key: String },

    #[error("Unknown sidechain key '{key}' (in {owner})")]
    UnknownSidechainKey { owner: String, key: String },

    #[error("Sidechain key '{key}' in {owner} is a bus; a track effect can only key a track")]
    SidechainKeyBusOnTrack { owner: String, key: String },

    #[error("Sidechain key '{key}' in {owner} is ambiguous: both a track and a bus use that name")]
    AmbiguousSidechainKey { owner: String, key: String },

    #[error("Unknown bus reference: '{bus}' (in track '{track}')")]
    UnknownBus { track: String, bus: String },

    #[error("Track '{track}': phrase '{phrase}' time range extends beyond project")]
    TimeRangeOutOfBounds { track: String, phrase: String },

    #[error("Track '{name}' is defined multiple times")]
    DuplicateTrack { name: String },

    #[error("Preset '{name}' is defined multiple times")]
    DuplicatePreset { name: String },

    #[error("Pattern '{name}' is defined multiple times")]
    DuplicatePattern { name: String },

    #[error("Audio clip '{name}' is defined multiple times")]
    DuplicateAudioClip { name: String },

    #[error("Unknown audio clip track reference: '{track}' (in audio clip '{clip}')")]
    UnknownAudioClipTrack { clip: String, track: String },

    #[error("Vocal tune '{name}' is defined multiple times")]
    DuplicateVocalTuneName { name: String },

    #[error("Audio clip '{clip}' already has a vocal tune: '{existing}' (in vocal tune '{tune}')")]
    DuplicateVocalTuneClip {
        tune: String,
        clip: String,
        existing: String,
    },

    #[error("Unknown audio clip reference: '{clip}' (in vocal tune '{tune}')")]
    UnknownVocalTuneClip { tune: String, clip: String },

    #[error("Unknown reference clip: '{reference}' (in vocal tune '{tune}')")]
    UnknownVocalTuneReference { tune: String, reference: String },

    #[error("Unknown track reference: '{track}' (in vocal tune '{tune}')")]
    UnknownVocalTuneTrack { tune: String, track: String },

    #[error("Track '{track}' must own exactly one audio clip for vocal tune '{tune}'")]
    MissingVocalTuneClip { tune: String, track: String },

    #[error("Vocal harmony '{name}' is defined multiple times")]
    DuplicateVocalHarmonyName { name: String },

    #[error(
        "Audio clip '{clip}' already has a vocal harmony: '{existing}' (in vocal harmony '{harmony}')"
    )]
    DuplicateVocalHarmonyClip {
        harmony: String,
        clip: String,
        existing: String,
    },

    #[error("Unknown audio clip reference: '{clip}' (in vocal harmony '{harmony}')")]
    UnknownVocalHarmonyClip { harmony: String, clip: String },

    #[error("Unknown track reference: '{track}' (in vocal harmony '{harmony}')")]
    UnknownVocalHarmonyTrack { harmony: String, track: String },

    #[error("Track '{track}' must own exactly one audio clip for vocal harmony '{harmony}'")]
    MissingVocalHarmonyClip { harmony: String, track: String },

    #[error("Control source '{name}' is defined multiple times")]
    DuplicateControlSource { name: String },

    #[error("Unknown track reference: '{track}' (in control source '{control}')")]
    UnknownControlSourceTrack { control: String, track: String },

    #[error("Unknown control source reference: '{control}' (in track '{track}')")]
    UnknownControlSource { track: String, control: String },

    #[error("Bus '{name}' is defined multiple times")]
    DuplicateBus { name: String },

    #[error("Unknown preset reference: '{preset}' (in {owner})")]
    UnknownPreset { owner: String, preset: String },

    #[error("Unknown pattern reference: '{pattern}' (in track '{track}')")]
    UnknownPattern { track: String, pattern: String },

    #[error("Unknown route reference: '{route}' (in bus '{bus}')")]
    UnknownRoute { bus: String, route: String },

    #[error("Invalid route target '{target}' in {owner}: {reason}")]
    InvalidRouteTarget {
        owner: String,
        target: String,
        reason: String,
    },

    #[error("Unknown {kind} route source '{name}' (in {owner})")]
    UnknownRouteSource {
        owner: String,
        kind: String,
        name: String,
    },

    #[error(
        "Control input refused: {effect} in {owner} does not accept a control signal; its inputs are {ports}"
    )]
    ControlInputRefused {
        owner: String,
        effect: String,
        ports: String,
    },

    #[error("Duplicate {kind} route into '{target}' (in {owner})")]
    DuplicateInputRoute {
        owner: String,
        target: String,
        kind: String,
    },

    #[error("Control route from '{control}' into {owner} would close a loop: {reason}")]
    ControlRouteCycle {
        owner: String,
        control: String,
        reason: String,
    },

    #[error(
        "Control routes close a loop between {cycle}; control is evaluated before its consumer"
    )]
    ControlRouteLoop { cycle: String },

    #[error("Invalid time range in {owner}: end must be after start")]
    InvalidTimeRange { owner: String },

    #[error("Invalid value for {field}: {value}")]
    InvalidValue { field: String, value: String },

    #[error("Invalid value for {field}: {value} (valid range {min}..={max})")]
    OutOfRange {
        field: String,
        value: f64,
        min: f64,
        max: f64,
    },

    #[error("Effect parameter '{param}' has invalid value for effect type '{effect}'")]
    InvalidEffectParam { effect: String, param: String },

    #[error("Invalid automation lane '{lane}' in {owner}: {reason}")]
    InvalidAutomationLane {
        owner: String,
        lane: String,
        reason: String,
    },
}
