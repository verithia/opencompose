//! Output-device ownership: the device list with stable keys, the chosen
//! device, the negotiated rate and buffer, and the health of what is open.
//!
//! Every output stream the application opens goes through this module, so a
//! device change sweeps all of them together instead of only the main one. The
//! device layer is a trait so selection, negotiation, persistence and sweeping
//! run headless against [`FakeBackend`].
//!
//! Hotplug is not supported: cpal 0.15 has no disconnect event API, and a
//! macOS default-device stream installs no removal listener. The error callback
//! and the next open are the only signals, which is why a dead stream pauses the
//! transport instead of being retried automatically.

use std::any::Any;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};

/// Rate used only when no device reports one.
pub const FALLBACK_RATE: u32 = 44_100;

/// Rates offered when a device reports a range rather than a discrete list.
pub const STANDARD_RATES: [u32; 11] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
];

// ============================================================================
// Wire types
// ============================================================================

/// One output device, keyed by a backend-stable identity.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputDevice {
    /// Backend-stable identity: CoreAudio UID, WASAPI endpoint id or ALSA PCM
    /// name. Falls back to `name:<display name>` when the backend exposes none.
    pub key: String,
    pub name: String,
    pub is_default: bool,
    /// Highest output channel count the device reports.
    pub channels: u16,
    /// Rates the device accepts for a two-channel stream, ascending.
    pub sample_rates: Vec<u32>,
    /// The device's own preferred rate.
    pub default_rate: u32,
    /// Frames the device lets a stream request; `None` when it controls the period.
    pub buffer_frames: Option<(u32, u32)>,
}

/// One input device, keyed by a backend-stable identity.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputDevice {
    /// Backend-stable identity, built the same way as [`OutputDevice::key`].
    pub key: String,
    pub name: String,
    pub is_default: bool,
    /// Highest capture channel count the device reports.
    pub channels: u16,
    /// Rates the device accepts for a one- or two-channel stream, ascending.
    pub sample_rates: Vec<u32>,
    /// The device's own preferred rate.
    pub default_rate: u32,
}

/// Capture channel layout for the input stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputChannels {
    /// One channel; a stereo interface is downmixed on capture.
    Mono,
    /// Two channels; a mono device is duplicated. The default: a take keeps
    /// the interface's stereo image unless the user asks for one channel.
    #[default]
    Stereo,
}

impl InputChannels {
    /// Number of channels this mode captures.
    pub fn count(self) -> u16 {
        match self {
            InputChannels::Mono => 1,
            InputChannels::Stereo => 2,
        }
    }
}

/// The persisted capture choice. Monitoring is off unless the user turns it on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputSelection {
    /// Stable device key; `None` follows the system default input.
    pub input_device: Option<String>,
    /// Display name kept next to the key so a missing device can be named.
    pub input_device_name: Option<String>,
    pub input_channels: InputChannels,
    /// Route capture to the output. Off by default: a microphone through the
    /// speakers feeds back.
    pub input_monitor: bool,
    /// Monitor gain in dB, applied only while monitoring is on.
    pub input_monitor_gain_db: f64,
}

impl Default for InputSelection {
    fn default() -> Self {
        Self {
            input_device: None,
            input_device_name: None,
            input_channels: InputChannels::Stereo,
            input_monitor: false,
            input_monitor_gain_db: 0.0,
        }
    }
}

/// What one input stream is opened with. `key: None` follows the system default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSpec {
    pub key: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl InputSpec {
    /// A capture request; `channels` is clamped to at least one.
    pub fn new(key: Option<String>, sample_rate: u32, channels: u16) -> Self {
        Self {
            key,
            sample_rate,
            channels: channels.max(1),
        }
    }
}

/// The input device and format that are actually open.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenedInput {
    pub key: Option<String>,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Capture callback side of one input stream. `capture` runs on the backend's
/// capture thread and must not allocate, lock or block.
pub trait InputSink: Send + Sync {
    /// `samples` are interleaved at `channels` channels.
    fn capture(&self, samples: &[f32], channels: u16);
    /// The backend reported this stream dead. Runs off the capture callback.
    fn failed(&self, error: String) {
        let _ = error;
    }
}

/// What one stream is opened with. `key: None` follows the system default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSpec {
    pub key: Option<String>,
    pub sample_rate: u32,
    /// `None` leaves the period to the device.
    pub buffer_frames: Option<u32>,
}

impl StreamSpec {
    /// An output request; `None` buffer frames use the device default.
    pub fn new(key: Option<String>, sample_rate: u32, buffer_frames: Option<u32>) -> Self {
        Self {
            key,
            sample_rate,
            buffer_frames,
        }
    }
}

/// The persisted choice. `None` means the system default for every field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSelection {
    /// Stable device key; `None` follows the system default.
    pub output_device: Option<String>,
    /// Display name kept next to the key so a missing device can be named.
    pub output_device_name: Option<String>,
    /// `None` uses the device's own rate.
    pub sample_rate: Option<u32>,
    /// `None` uses the device's own period.
    pub buffer_frames: Option<u32>,
}

/// The device and format that are actually open.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenedOutput {
    pub key: Option<String>,
    pub name: String,
    pub sample_rate: u32,
    pub buffer_frames: Option<u32>,
    /// True when the device accepted a fixed period.
    pub buffer_fixed: bool,
}

/// A condition the user has to know about but that did not fail the action.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioNotice {
    /// The stored device is not connected; the system default took over.
    MissingDevice { key: String, name: String },
    /// The requested rate is not offered; the nearest one opened instead.
    RateSubstituted { requested: u32, chosen: u32 },
    /// The device controls its own period, so the requested buffer was dropped.
    BufferUnsupported { requested: u32 },
    /// The requested buffer was outside the device's range and was clamped.
    BufferClamped {
        requested: u32,
        chosen: u32,
        min: u32,
        max: u32,
    },
    /// The backend refused the fixed period; the device default was used.
    BufferRejected { requested: u32, error: String },
    /// The settings file could not be read.
    SettingsUnreadable { error: String },
    /// The settings file could not be written.
    SettingsUnwritable { error: String },
    /// An open stream reported a backend error.
    StreamFailed { role: String, error: String },
    /// The stored input device is not connected; the system default took over.
    MissingInputDevice { key: String, name: String },
    /// The input does not offer the output rate; the nearest one opened instead.
    InputRateSubstituted { requested: u32, chosen: u32 },
    /// The input has fewer channels than requested; capture was narrowed.
    InputChannelsReduced { requested: u16, chosen: u16 },
    /// The open input stream reported a backend error.
    InputFailed { error: String },
}

/// An action that did not take effect.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioFailure {
    /// Changing device settings would interrupt a take.
    RecordingActive,
    /// No device is connected at all.
    NoDevices,
    /// Enumerating devices failed.
    ListFailed { error: String },
    /// The selected device could not be opened; the previous one stayed.
    OpenFailed {
        device: String,
        error: String,
        fallback: Option<String>,
    },
    /// Every stream the application had open failed to move to the new device.
    StreamLost { device: String, error: String },
    /// No capture device is connected at all.
    NoInputDevices,
    /// The selected input could not be opened; the previous one stayed.
    InputOpenFailed {
        device: String,
        error: String,
        fallback: Option<String>,
    },
}

/// One snapshot of the audio-IO owner, as the preferences section reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioStatus {
    pub selection: AudioSelection,
    /// The device that is open, or the one the next stream would open.
    pub opened: Option<OpenedOutput>,
    pub devices: Vec<OutputDevice>,
    pub default_output: Option<String>,
    /// The stored device is not connected.
    pub missing_device: bool,
    pub notices: Vec<AudioNotice>,
    pub failure: Option<AudioFailure>,
    /// At least one stream this owner tracks is open.
    pub stream_open: bool,
    /// The persisted capture choice.
    pub input: InputSelection,
    pub inputs: Vec<InputDevice>,
    pub default_input: Option<String>,
    /// The capture device that is open, or the one the next open would use.
    pub opened_input: Option<OpenedInput>,
    /// The stored input device is not connected.
    pub missing_input: bool,
    pub input_notices: Vec<AudioNotice>,
    pub input_failure: Option<AudioFailure>,
}

/// The settings file: one flat JSON object holding both selections.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct StoredSettings {
    #[serde(flatten)]
    output: AudioSelection,
    #[serde(flatten)]
    input: InputSelection,
    /// The audition click, which is audio settings rather than device state.
    #[serde(default)]
    click: crate::click::ClickSettings,
}

// ============================================================================
// Backend trait
// ============================================================================

/// The device side of audio I/O: enumeration and stream construction.
pub trait DeviceBackend: Send + Sync {
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, String>;
    /// Stable key of the system default output device.
    fn default_output(&self) -> Option<String>;
    /// Build and start one output stream; `spec.key: None` opens the default.
    fn open_output(
        &self,
        spec: &StreamSpec,
        sink: Arc<dyn RenderSink>,
    ) -> Result<StreamSink, String>;

    fn list_inputs(&self) -> Result<Vec<InputDevice>, String>;
    /// Stable key of the system default capture device.
    fn default_input(&self) -> Option<String>;
    /// Build and start one capture stream; `spec.key: None` opens the default.
    fn open_input(&self, spec: &InputSpec, sink: Arc<dyn InputSink>) -> Result<StreamSink, String>;
}

/// Device callback side of one stream. `fill` runs on the backend's audio
/// thread and must not allocate, lock or block.
pub trait RenderSink: Send + Sync {
    fn fill(&self, output: &mut [f32]);
    /// Publish control routes after the backend accepts this callback.
    /// Runs on the stream-opening thread, outside the audio callback.
    fn opened(&self) {}
    /// Report the accepted rate when it differs from the requested format.
    fn opened_at(&self, _sample_rate: u32) {
        self.opened();
    }
    /// The backend reported this stream dead. Runs off the audio callback.
    fn failed(&self, error: String) {
        let _ = error;
    }
}

/// Builds the callback for one stream open. Called again for every reopen, so a
/// device switch gets a fresh callback around the same playback state.
pub trait RenderSource: Send + Sync {
    fn create(&self, spec: &StreamSpec) -> Arc<dyn RenderSink>;
}

/// What one sweep touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Roles of the streams that were open and were rebuilt.
    pub attempted: Vec<&'static str>,
    /// Roles that failed to come back, with the backend reason.
    pub failed: Vec<(&'static str, String)>,
}

impl SweepReport {
    /// Whether every attempted device failed to open.
    pub fn all_failed(&self) -> bool {
        !self.attempted.is_empty() && self.failed.len() == self.attempted.len()
    }
}

/// One rebuilt-on-demand output stream.
pub trait ManagedStream: Send + Sync {
    /// Short name for diagnostics and notices ("main", "audition", "editor").
    fn role(&self) -> &'static str;
    fn is_open(&self) -> bool;
    /// Rebuild this stream for `spec`. Keep the existing stream on failure.
    fn reopen(&self, spec: &StreamSpec) -> Result<(), String>;
}

/// A started stream. Create and drop on control threads: dropping it waits
/// for its native owner to stop the device and finish teardown.
pub struct StreamSink {
    pub sample_rate: u32,
    pub input_channels: Option<u16>,
    pub buffer_frames: Option<u32>,
    pub buffer_fixed: bool,
    /// The backend error that forced a fall back to the device period.
    pub buffer_fallback: Option<String>,
    _handle: Box<dyn Any + Send + Sync>,
}

impl StreamSink {
    /// Wrap a live stream handle with its format and buffer fallback.
    pub fn new(
        handle: impl Any + Send + Sync,
        sample_rate: u32,
        buffer_frames: Option<u32>,
        buffer_fixed: bool,
        buffer_fallback: Option<String>,
    ) -> Self {
        Self {
            sample_rate,
            input_channels: None,
            buffer_frames,
            buffer_fixed,
            buffer_fallback,
            _handle: Box::new(handle),
        }
    }
}

impl std::fmt::Debug for StreamSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSink")
            .field("sample_rate", &self.sample_rate)
            .field("buffer_frames", &self.buffer_frames)
            .field("buffer_fixed", &self.buffer_fixed)
            .finish_non_exhaustive()
    }
}

// ============================================================================
// Negotiation
// ============================================================================

/// Resolve the device a selection names. A key that is not present falls back
/// to the system default, which is what a reconnected device later replaces.
pub fn effective_device<'a>(
    devices: &'a [OutputDevice],
    key: Option<&str>,
) -> Option<&'a OutputDevice> {
    match key {
        Some(key) => devices
            .iter()
            .find(|device| device.key == key)
            .or_else(|| default_device(devices)),
        None => default_device(devices),
    }
}

fn default_device(devices: &[OutputDevice]) -> Option<&OutputDevice> {
    devices
        .iter()
        .find(|device| device.is_default)
        .or_else(|| devices.first())
}

/// Nearest offered rate; ties prefer the lower rate.
pub fn nearest_rate(rates: &[u32], requested: u32) -> u32 {
    let mut best = requested;
    let mut best_distance = u32::MAX;
    for &rate in rates {
        let distance = rate.abs_diff(requested);
        if distance < best_distance || (distance == best_distance && rate < best) {
            best = rate;
            best_distance = distance;
        }
    }
    best
}

/// Turn a selection into the stream format to request, with the reasons any
/// part of it changed.
pub fn negotiate(
    device: &OutputDevice,
    selection: &AudioSelection,
) -> (StreamSpec, Vec<AudioNotice>) {
    let mut notices = Vec::new();
    let rate = match selection.sample_rate {
        Some(requested) => {
            let chosen = if device.sample_rates.is_empty() {
                device.default_rate.max(1)
            } else {
                nearest_rate(&device.sample_rates, requested)
            };
            if chosen != requested {
                notices.push(AudioNotice::RateSubstituted { requested, chosen });
            }
            chosen
        }
        None => device.default_rate.max(1),
    };
    let buffer_frames = match selection.buffer_frames {
        Some(requested) => match device.buffer_frames {
            Some((min, max)) => {
                let chosen = requested.clamp(min, max);
                if chosen != requested {
                    notices.push(AudioNotice::BufferClamped {
                        requested,
                        chosen,
                        min,
                        max,
                    });
                }
                Some(chosen)
            }
            None => {
                notices.push(AudioNotice::BufferUnsupported { requested });
                None
            }
        },
        None => None,
    };
    (
        StreamSpec::new(Some(device.key.clone()), rate, buffer_frames),
        notices,
    )
}

/// Resolve the input device a selection names, with the same fallback rule as
/// [`effective_device`].
pub fn effective_input<'a>(
    devices: &'a [InputDevice],
    key: Option<&str>,
) -> Option<&'a InputDevice> {
    match key {
        Some(key) => devices
            .iter()
            .find(|device| device.key == key)
            .or_else(|| default_input_device(devices)),
        None => default_input_device(devices),
    }
}

fn default_input_device(devices: &[InputDevice]) -> Option<&InputDevice> {
    devices
        .iter()
        .find(|device| device.is_default)
        .or_else(|| devices.first())
}

/// Turn an input selection into the capture format to request. The output rate
/// is the preference: recording at the rate the transport runs at needs no
/// resampling, and a device that offers it is asked for it.
pub fn negotiate_input(
    device: &InputDevice,
    selection: &InputSelection,
    output_rate: u32,
) -> (InputSpec, Vec<AudioNotice>) {
    let mut notices = Vec::new();
    let wanted = output_rate.max(1);
    let chosen = if device.sample_rates.is_empty() {
        device.default_rate.max(1)
    } else if device.sample_rates.contains(&wanted) {
        wanted
    } else {
        nearest_rate(&device.sample_rates, wanted)
    };
    if chosen != wanted {
        notices.push(AudioNotice::InputRateSubstituted {
            requested: wanted,
            chosen,
        });
    }
    let requested = selection.input_channels.count();
    let channels = if requested > device.channels.max(1) {
        let fallback = device.channels.max(1);
        notices.push(AudioNotice::InputChannelsReduced {
            requested,
            chosen: fallback,
        });
        fallback
    } else {
        requested
    };
    (
        InputSpec::new(Some(device.key.clone()), chosen, channels),
        notices,
    )
}

// ============================================================================
// The owner
// ============================================================================

/// Every output stream and device decision of one application instance.
pub struct AudioIo {
    backend: Arc<dyn DeviceBackend>,
    path: Option<PathBuf>,
    state: Mutex<IoState>,
    output_change: Mutex<()>,
    streams: Mutex<Vec<Weak<dyn ManagedStream>>>,
}

struct IoState {
    selection: AudioSelection,
    devices: Vec<OutputDevice>,
    default_key: Option<String>,
    opened: Option<OpenedOutput>,
    opened_spec: Option<StreamSpec>,
    /// Notices produced by the last successful open.
    open_notices: Vec<AudioNotice>,
    /// Notices produced by a stream that died.
    stream_notices: Vec<AudioNotice>,
    failure: Option<AudioFailure>,
    settings_notice: Option<AudioNotice>,
    loaded: bool,
    /// The device list has been read at least once.
    listed: bool,
    input: InputSelection,
    inputs: Vec<InputDevice>,
    default_input_key: Option<String>,
    opened_input: Option<OpenedInput>,
    input_notices: Vec<AudioNotice>,
    input_failure: Option<AudioFailure>,
    /// The input list has been read at least once.
    input_listed: bool,
    click: crate::click::ClickSettings,
}

impl AudioIo {
    /// Create I/O state; `path: None` keeps the selection in memory only.
    pub fn new(backend: Arc<dyn DeviceBackend>, path: Option<PathBuf>) -> Self {
        Self {
            backend,
            path,
            state: Mutex::new(IoState {
                selection: AudioSelection::default(),
                devices: Vec::new(),
                default_key: None,
                opened: None,
                opened_spec: None,
                open_notices: Vec::new(),
                stream_notices: Vec::new(),
                failure: None,
                settings_notice: None,
                loaded: false,
                listed: false,
                input: InputSelection::default(),
                inputs: Vec::new(),
                default_input_key: None,
                opened_input: None,
                input_notices: Vec::new(),
                input_failure: None,
                input_listed: false,
                click: crate::click::ClickSettings::default(),
            }),
            streams: Mutex::new(Vec::new()),
            output_change: Mutex::new(()),
        }
    }

    /// Path of the persisted selection, `~/.opencompose/audio.json`. A test
    /// build keeps this out of the operator's home.
    pub fn preferences_path() -> Option<PathBuf> {
        crate::private_fs::available_config_dir().map(|root| root.join("audio.json"))
    }

    /// The device backend this handle owns.
    pub fn backend(&self) -> Arc<dyn DeviceBackend> {
        Arc::clone(&self.backend)
    }

    /// Read the persisted selection once. A missing file means system defaults.
    pub fn ensure_loaded(&self) {
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.loaded {
                return;
            }
        }
        let mut notice = None;
        let mut stored = StoredSettings::default();
        if let Some(path) = &self.path {
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_json::from_str::<StoredSettings>(&text) {
                    Ok(loaded) => stored = loaded,
                    Err(error) => {
                        log::warn!(
                            "ignoring invalid audio preferences at {}: {error}",
                            path.display()
                        );
                        notice = Some(AudioNotice::SettingsUnreadable {
                            error: error.to_string(),
                        });
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    log::warn!(
                        "could not read audio preferences at {}: {error}",
                        path.display()
                    );
                    notice = Some(AudioNotice::SettingsUnreadable {
                        error: error.to_string(),
                    });
                }
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.loaded {
            return;
        }
        state.selection = stored.output;
        state.input = stored.input;
        state.click = stored.click;
        state.settings_notice = notice;
        state.loaded = true;
        if let Some((resolved, _)) = self.resolve(&state) {
            state.opened = Some(resolved);
            state.opened_spec = Some(self.spec_for_state(&state).0);
        }
        if let Some(resolved) = self.resolve_input(&state) {
            state.opened_input = Some(resolved);
        }
    }

    /// Load output devices only. Status polling and playback must not probe capture.
    fn ensure_ready(&self) {
        self.ensure_loaded();
        let listed = self.state.lock().unwrap_or_else(|e| e.into_inner()).listed;
        if !listed && let Err(error) = self.refresh() {
            log::warn!("could not list audio devices: {error}");
        }
    }

    /// Capture negotiation is opt-in, never part of output startup or polling.
    fn ensure_inputs_ready(&self) {
        self.ensure_ready();
        let listed = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input_listed;
        if !listed && let Err(error) = self.refresh_inputs() {
            log::warn!("could not list audio input devices: {error}");
        }
    }

    /// Re-read the capture device list and the default input.
    pub fn refresh_inputs(&self) -> Result<(), String> {
        self.ensure_loaded();
        let inputs = self.backend.list_inputs()?;
        let default_key = self.backend.default_input();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.inputs = inputs;
        state.default_input_key = default_key;
        state.input_listed = true;
        if state.opened_input.is_none() {
            state.opened_input = self.resolve_input(&state);
        }
        Ok(())
    }

    /// Re-read the device list and the default device.
    pub fn refresh(&self) -> Result<(), String> {
        self.ensure_loaded();
        let devices = self.backend.list_outputs()?;
        let default_key = self.backend.default_output();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.devices = devices;
            state.default_key = default_key;
            state.listed = true;
            // Re-resolve "what would open" against the new list. A stream that is
            // still open stays recorded only while its device is still present.
            let opened_present = state.opened.as_ref().is_some_and(|opened| {
                opened
                    .key
                    .as_deref()
                    .is_some_and(|key| state.devices.iter().any(|device| device.key == key))
            });
            if !opened_present || !self.any_stream_open() {
                state.opened = None;
                state.opened_spec = None;
            }
            if state.opened.is_none()
                && let Some((resolved, _)) = self.resolve(&state)
            {
                state.opened = Some(resolved);
                state.opened_spec = Some(self.spec_for_state(&state).0);
            }
        }
        Ok(())
    }

    /// Re-open a stored device that came back after being missing.
    pub fn reconnect(&self) -> bool {
        self.ensure_ready();
        let (key, present, opened_elsewhere) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(key) = state.selection.output_device.clone() else {
                return false;
            };
            let present = state.devices.iter().any(|device| device.key == key);
            let opened_elsewhere = state
                .opened
                .as_ref()
                .is_none_or(|opened| opened.key.as_deref() != Some(key.as_str()));
            (key, present, opened_elsewhere)
        };
        if !present || !opened_elsewhere {
            return false;
        }
        let status = self.apply(&self.selection());
        status.failure.is_none()
            && status
                .opened
                .as_ref()
                .and_then(|opened| opened.key.as_deref())
                == Some(key.as_str())
    }

    /// The output selection, loading it on first use.
    pub fn selection(&self) -> AudioSelection {
        self.ensure_ready();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selection
            .clone()
    }

    /// Whether any output device is known. Without one, a rate is meaningless.
    pub fn has_devices(&self) -> bool {
        self.ensure_ready();
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .devices
            .is_empty()
    }

    /// Whether capture discovery has run; unknown must not be presented as absent.
    pub fn input_devices_listed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input_listed
    }

    /// Whether a previously enumerated capture device is known; never probes hardware.
    pub fn has_inputs(&self) -> bool {
        self.ensure_ready();
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inputs
            .is_empty()
    }

    /// The persisted click settings.
    pub fn click_settings(&self) -> crate::click::ClickSettings {
        self.ensure_loaded();
        self.state.lock().unwrap_or_else(|e| e.into_inner()).click
    }

    /// Store the click's level and count-in choice. The level is clamped to a
    /// range that cannot drown or disappear under the master.
    pub fn apply_click(
        &self,
        settings: crate::click::ClickSettings,
    ) -> crate::click::ClickSettings {
        self.ensure_loaded();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = settings;
        next.level_db = next.level_db.clamp(-60.0, 6.0);
        state.click = next;
        self.persist(&mut state);
        state.click
    }

    /// The capture selection, loading it on first use.
    pub fn input_selection(&self) -> InputSelection {
        self.ensure_loaded();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input
            .clone()
    }

    /// The capture format the next input open would request.
    pub fn input_spec(&self) -> InputSpec {
        self.ensure_inputs_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.input_spec_for_state(&state).0
    }

    /// The capture format a different input selection would request.
    pub fn input_spec_for(&self, selection: &InputSelection) -> InputSpec {
        self.ensure_inputs_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let rate = self.spec_for_state(&state).0.sample_rate;
        let mut spec = self.input_spec_for_selection(&state, selection, rate).0;
        spec.key = selection.input_device.clone();
        spec
    }

    /// The current input selection negotiated against a candidate output rate.
    pub fn input_spec_for_output_rate(&self, output_rate: u32) -> InputSpec {
        self.ensure_inputs_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.input_spec_for_selection(&state, &state.input, output_rate)
            .0
    }

    /// Open one capture stream on the current input format.
    pub fn open_input_stream(&self, sink: Arc<dyn InputSink>) -> Result<StreamSink, String> {
        let spec = self.input_spec();
        self.open_input_stream_with(&spec, sink)
    }

    /// Open one capture stream on an explicit format.
    pub fn open_input_stream_with(
        &self,
        spec: &InputSpec,
        sink: Arc<dyn InputSink>,
    ) -> Result<StreamSink, String> {
        let mut stream = self.backend.open_input(spec, sink)?;
        let channels = *stream.input_channels.get_or_insert(spec.channels);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(device) = effective_input(&state.inputs, state.input.input_device.as_deref()) {
            state.opened_input = Some(OpenedInput {
                key: Some(device.key.clone()),
                name: device.name.clone(),
                sample_rate: stream.sample_rate,
                channels,
            });
            state.input_failure = None;
        }
        Ok(stream)
    }

    /// Record a dead capture stream so the recording state can stop cleanly.
    pub fn note_input_failure(&self, error: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .input_notices
            .retain(|notice| !matches!(notice, AudioNotice::InputFailed { .. }));
        state.input_notices.push(AudioNotice::InputFailed {
            error: error.to_string(),
        });
    }

    /// Apply monitor settings without probing or replacing the input stream.
    pub fn apply_input_monitor(&self, monitor: bool, gain_db: f64) -> InputSelection {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.input.input_monitor = monitor;
        state.input.input_monitor_gain_db = gain_db.clamp(-60.0, 12.0);
        state.input_failure = None;
        self.persist(&mut state);
        state.input.clone()
    }

    /// Apply a capture selection: negotiate, prove the device captures, and
    /// persist. On failure the previous choice and device stay.
    pub fn apply_input(&self, selection: &InputSelection) -> AudioStatus {
        self.ensure_ready();
        if let Err(error) = self.refresh_inputs() {
            return self.fail(AudioFailure::ListFailed { error });
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut candidate = selection.clone();
        candidate.input_monitor_gain_db = candidate.input_monitor_gain_db.clamp(-60.0, 12.0);
        let Some(device) = self.candidate_input(&state, &candidate) else {
            if state.inputs.is_empty() {
                return self.snapshot_after_input_failure(&mut state, AudioFailure::NoInputDevices);
            }
            let name = candidate
                .input_device_name
                .clone()
                .or_else(|| candidate.input_device.clone())
                .unwrap_or_default();
            let fallback = effective_input(&state.inputs, None).map(|d| d.name.clone());
            let failure = AudioFailure::InputOpenFailed {
                device: name,
                error: "the device is not connected".to_string(),
                fallback,
            };
            return self.snapshot_after_input_failure(&mut state, failure);
        };
        candidate.input_device_name = Some(device.name.clone());
        let output_rate = self.spec_for_state(&state).0.sample_rate;
        let (spec, notices) = negotiate_input(device, &candidate, output_rate);
        let spec = InputSpec {
            key: if selection.input_device.is_some() {
                Some(device.key.clone())
            } else {
                None
            },
            ..spec
        };
        // Prove the device captures before committing the choice.
        let probe = match self.backend.open_input(&spec, Arc::new(SilentInput)) {
            Ok(probe) => probe,
            Err(error) => {
                let fallback = state
                    .input
                    .input_device_name
                    .clone()
                    .or_else(|| {
                        state
                            .opened_input
                            .as_ref()
                            .map(|opened| opened.name.clone())
                    })
                    .or_else(|| effective_input(&state.inputs, None).map(|d| d.name.clone()));
                let failure = AudioFailure::InputOpenFailed {
                    device: device.name.clone(),
                    error,
                    fallback,
                };
                return self.snapshot_after_input_failure(&mut state, failure);
            }
        };
        let opened = OpenedInput {
            key: Some(device.key.clone()),
            name: device.name.clone(),
            sample_rate: probe.sample_rate,
            channels: probe.input_channels.unwrap_or(spec.channels),
        };
        drop(probe);
        state.input = candidate;
        state.opened_input = Some(opened);
        state.input_notices = notices;
        state.input_failure = None;
        self.persist(&mut state);
        self.snapshot(&state)
    }

    /// The format the next stream open would request.
    pub fn spec(&self) -> StreamSpec {
        self.ensure_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.spec_for_state(&state).0
    }

    /// The format a different selection would request, without opening it.
    pub fn spec_for(&self, selection: &AudioSelection) -> StreamSpec {
        self.ensure_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match effective_device(&state.devices, selection.output_device.as_deref()) {
            Some(device) => {
                let (mut spec, _) = negotiate(device, selection);
                spec.key = selection.output_device.clone();
                spec
            }
            None => StreamSpec::new(
                selection.output_device.clone(),
                selection.sample_rate.unwrap_or(FALLBACK_RATE),
                selection.buffer_frames,
            ),
        }
    }

    /// Rate the next stream open (or the open one) runs at.
    pub fn sample_rate(&self) -> u32 {
        self.spec().sample_rate
    }

    /// A snapshot of the devices, selections and open streams.
    pub fn status(&self) -> AudioStatus {
        self.ensure_ready();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot(&state)
    }

    /// Register one stream so a device change sweeps it. Weak: the caller keeps
    /// the stream alive and the registry prunes it otherwise.
    pub fn register_stream(&self, stream: Arc<dyn ManagedStream>) {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.retain(|weak| weak.strong_count() > 0);
        streams.push(Arc::downgrade(&stream));
    }

    /// Rebuild every registered stream that is currently open.
    pub fn sweep(&self, spec: &StreamSpec) -> SweepReport {
        let live: Vec<Arc<dyn ManagedStream>> = {
            let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
            streams.retain(|weak| weak.strong_count() > 0);
            streams.iter().filter_map(Weak::upgrade).collect()
        };
        let mut report = SweepReport::default();
        for stream in live {
            if !stream.is_open() {
                continue;
            }
            report.attempted.push(stream.role());
            if let Err(error) = stream.reopen(spec) {
                report.failed.push((stream.role(), error));
            }
        }
        report
    }

    /// Open one stream on the current format and record it as the opened device.
    pub fn open_stream(&self, source: Arc<dyn RenderSource>) -> Result<StreamSink, String> {
        let spec = self.spec();
        self.open_stream_with(&spec, source)
    }

    /// Open one stream on an explicit format. The opened device is only updated
    /// when `spec` is what the current selection resolves to.
    pub fn open_stream_with(
        &self,
        spec: &StreamSpec,
        source: Arc<dyn RenderSource>,
    ) -> Result<StreamSink, String> {
        let callback = source.create(spec);
        let sink = self.backend.open_output(spec, Arc::clone(&callback))?;
        callback.opened_at(sink.sample_rate);
        self.record_open(spec, &sink);
        Ok(sink)
    }

    fn record_open(&self, spec: &StreamSpec, sink: &StreamSink) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = self.spec_for_state(&state).0;
        if &current != spec {
            return;
        }
        let Some(device) =
            effective_device(&state.devices, state.selection.output_device.as_deref())
        else {
            return;
        };
        if state.opened_spec.as_ref() != Some(spec) {
            state.opened = Some(OpenedOutput {
                key: Some(device.key.clone()),
                name: device.name.clone(),
                sample_rate: sink.sample_rate,
                buffer_frames: sink.buffer_frames,
                buffer_fixed: sink.buffer_fixed,
            });
            state.opened_spec = Some(spec.clone());
            state
                .open_notices
                .retain(|notice| !matches!(notice, AudioNotice::BufferRejected { .. }));
            if let Some(error) = &sink.buffer_fallback {
                state.open_notices.push(AudioNotice::BufferRejected {
                    requested: spec.buffer_frames.unwrap_or(0),
                    error: error.clone(),
                });
            }
        }
        state.failure = None;
    }

    /// Record a dead stream so the preferences section and the transport agree.
    pub fn note_stream_failure(&self, role: &str, error: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stream_notices.retain(|notice| {
            !matches!(notice, AudioNotice::StreamFailed { role: failed, .. } if failed == role)
        });
        state.stream_notices.push(AudioNotice::StreamFailed {
            role: role.to_string(),
            error: error.to_string(),
        });
    }

    /// Apply a full selection: negotiate, prove the device opens, sweep the open
    /// streams and persist. On failure the previous selection and device stay.
    pub fn apply(&self, selection: &AudioSelection) -> AudioStatus {
        self.apply_inner(selection, true)
    }

    /// Apply a selection without writing the settings file. A client that only
    /// borrows the stored device (the CLI) must not overwrite the operator's
    /// choice with its own rate.
    pub fn apply_transient(&self, selection: &AudioSelection) -> AudioStatus {
        self.apply_inner(selection, false)
    }

    fn apply_inner(&self, selection: &AudioSelection, save: bool) -> AudioStatus {
        let _change = self
            .output_change
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_ready();
        if let Err(error) = self.refresh() {
            return self.fail(AudioFailure::ListFailed { error });
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut candidate = selection.clone();
        if candidate.output_device.is_none() {
            candidate.output_device_name = None;
        }
        let Some(device) = self.candidate_device(&state, &candidate) else {
            if state.devices.is_empty() {
                return self.snapshot_after_failure(&mut state, AudioFailure::NoDevices);
            }
            let name = candidate
                .output_device_name
                .clone()
                .or_else(|| candidate.output_device.clone())
                .unwrap_or_default();
            let fallback = effective_device(&state.devices, None).map(|d| d.name.clone());
            let failure = AudioFailure::OpenFailed {
                device: name,
                error: "the device is not connected".to_string(),
                fallback,
            };
            return self.snapshot_after_failure(&mut state, failure);
        };
        candidate.output_device_name = Some(device.name.clone());
        let (spec, _) = negotiate(device, &candidate);
        // "System default" stays `None` so it keeps following the OS default.
        let spec = StreamSpec {
            key: if selection.output_device.is_some() {
                Some(device.key.clone())
            } else {
                None
            },
            ..spec
        };
        let device_name = device.name.clone();
        let device_key = device.key.clone();
        let fallback = state
            .selection
            .output_device_name
            .clone()
            .or_else(|| state.opened.as_ref().map(|opened| opened.name.clone()))
            .or_else(|| effective_device(&state.devices, None).map(|device| device.name.clone()));
        // A managed capture stream reads its selection while reopening, and
        // backend error callbacks may also need the status lock.
        drop(state);

        // Prove the device opens before touching a live stream.
        let probe = match self.backend.open_output(&spec, Arc::new(SilentSink)) {
            Ok(probe) => probe,
            Err(error) => {
                let failure = AudioFailure::OpenFailed {
                    device: device_name,
                    error,
                    fallback,
                };
                return self.fail(failure);
            }
        };
        let opened = OpenedOutput {
            key: Some(device_key),
            name: device_name.clone(),
            sample_rate: probe.sample_rate,
            buffer_frames: probe.buffer_frames,
            buffer_fixed: probe.buffer_fixed,
        };
        let mut open_notices = Vec::new();
        if let Some(error) = &probe.buffer_fallback {
            open_notices.push(AudioNotice::BufferRejected {
                requested: spec.buffer_frames.unwrap_or(0),
                error: error.clone(),
            });
        }
        // A stream reports the rate it actually runs at, which a backend may
        // answer differently from the one negotiation asked for. Name both, so
        // the substitution is visible instead of inferred from the footer.
        if probe.sample_rate != spec.sample_rate {
            open_notices.push(AudioNotice::RateSubstituted {
                requested: spec.sample_rate,
                chosen: probe.sample_rate,
            });
        }
        drop(probe);

        let report = self.sweep(&spec);
        if report.all_failed() {
            // Failed reopens keep their working callbacks and control state.
            let failure = AudioFailure::StreamLost {
                device: device_name,
                error: report.failed[0].1.clone(),
            };
            return self.fail(failure);
        }

        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.selection = candidate;
        state.opened = Some(opened);
        state.opened_spec = Some(spec);
        state.open_notices = open_notices;
        state
            .stream_notices
            .retain(|notice| !matches!(notice, AudioNotice::StreamFailed { .. }));
        for (role, error) in &report.failed {
            state.stream_notices.push(AudioNotice::StreamFailed {
                role: (*role).to_string(),
                error: error.clone(),
            });
        }
        state.failure = None;
        if save {
            self.persist(&mut state);
        }
        self.snapshot(&state)
    }

    fn candidate_device<'a>(
        &self,
        state: &'a IoState,
        selection: &AudioSelection,
    ) -> Option<&'a OutputDevice> {
        match selection.output_device.as_deref() {
            Some(key) => state.devices.iter().find(|device| device.key == key),
            None => default_device(&state.devices),
        }
    }

    fn resolve(&self, state: &IoState) -> Option<(OpenedOutput, StreamSpec)> {
        let device = effective_device(&state.devices, state.selection.output_device.as_deref())?;
        let (spec, _) = self.spec_for_state(state);
        Some((
            OpenedOutput {
                key: Some(device.key.clone()),
                name: device.name.clone(),
                sample_rate: spec.sample_rate,
                buffer_frames: spec.buffer_frames,
                buffer_fixed: spec.buffer_frames.is_some(),
            },
            spec,
        ))
    }

    fn candidate_input<'a>(
        &self,
        state: &'a IoState,
        selection: &InputSelection,
    ) -> Option<&'a InputDevice> {
        match selection.input_device.as_deref() {
            Some(key) => state.inputs.iter().find(|device| device.key == key),
            None => default_input_device(&state.inputs),
        }
    }

    fn resolve_input(&self, state: &IoState) -> Option<OpenedInput> {
        let device = effective_input(&state.inputs, state.input.input_device.as_deref())?;
        let spec = self.input_spec_for_state(state).0;
        Some(OpenedInput {
            key: Some(device.key.clone()),
            name: device.name.clone(),
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        })
    }

    /// The capture format the stored input selection resolves to.
    fn input_spec_for_state(&self, state: &IoState) -> (InputSpec, Vec<AudioNotice>) {
        self.input_spec_for_selection(
            state,
            &state.input,
            self.spec_for_state(state).0.sample_rate,
        )
    }

    fn input_spec_for_selection(
        &self,
        state: &IoState,
        selection: &InputSelection,
        output_rate: u32,
    ) -> (InputSpec, Vec<AudioNotice>) {
        match effective_input(&state.inputs, selection.input_device.as_deref()) {
            Some(device) => {
                let (mut spec, notices) = negotiate_input(device, selection, output_rate);
                spec.key = selection
                    .input_device
                    .as_ref()
                    .filter(|key| **key == device.key)
                    .cloned();
                (spec, notices)
            }
            None => (
                InputSpec::new(
                    selection.input_device.clone(),
                    output_rate,
                    selection.input_channels.count(),
                ),
                Vec::new(),
            ),
        }
    }

    fn spec_for_state(&self, state: &IoState) -> (StreamSpec, Vec<AudioNotice>) {
        match effective_device(&state.devices, state.selection.output_device.as_deref()) {
            Some(device) => {
                let (mut spec, notices) = negotiate(device, &state.selection);
                // A stored device that is present is pinned; a missing one and
                // "System default" both follow the OS default at open time.
                spec.key = state
                    .selection
                    .output_device
                    .as_ref()
                    .filter(|key| **key == device.key)
                    .cloned();
                (spec, notices)
            }
            None => (
                StreamSpec::new(
                    state.selection.output_device.clone(),
                    state.selection.sample_rate.unwrap_or(FALLBACK_RATE),
                    state.selection.buffer_frames,
                ),
                Vec::new(),
            ),
        }
    }

    fn persist(&self, state: &mut IoState) {
        let Some(path) = &self.path else {
            return;
        };
        match save_selection(path, &state.selection, &state.input, &state.click) {
            Ok(()) => {
                if matches!(
                    state.settings_notice,
                    Some(AudioNotice::SettingsUnwritable { .. })
                ) {
                    state.settings_notice = None;
                }
            }
            Err(error) => {
                log::warn!("could not save audio preferences: {error}");
                state.settings_notice = Some(AudioNotice::SettingsUnwritable { error });
            }
        }
    }

    fn fail(&self, failure: AudioFailure) -> AudioStatus {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_after_failure(&mut state, failure)
    }

    fn snapshot_after_failure(&self, state: &mut IoState, failure: AudioFailure) -> AudioStatus {
        state.failure = Some(failure);
        self.snapshot(state)
    }

    fn snapshot_after_input_failure(
        &self,
        state: &mut IoState,
        failure: AudioFailure,
    ) -> AudioStatus {
        state.input_failure = Some(failure);
        state
            .input_notices
            .retain(|notice| !matches!(notice, AudioNotice::InputFailed { .. }));
        self.snapshot(state)
    }

    fn snapshot(&self, state: &IoState) -> AudioStatus {
        let missing_device = state
            .selection
            .output_device
            .as_ref()
            .is_some_and(|key| !state.devices.iter().any(|device| &device.key == key));
        let mut notices = Vec::new();
        if missing_device {
            let name = state
                .selection
                .output_device_name
                .clone()
                .or_else(|| state.selection.output_device.clone())
                .unwrap_or_default();
            notices.push(AudioNotice::MissingDevice {
                key: state.selection.output_device.clone().unwrap_or_default(),
                name,
            });
        }
        if let Some(notice) = &state.settings_notice {
            notices.push(notice.clone());
        }
        notices.extend(self.spec_for_state(state).1);
        notices.extend(state.open_notices.iter().cloned());
        notices.extend(state.stream_notices.iter().cloned());

        let opened = state.opened.clone().or_else(|| {
            effective_device(&state.devices, state.selection.output_device.as_deref()).map(
                |device| {
                    let (spec, _) = self.spec_for_state(state);
                    OpenedOutput {
                        key: Some(device.key.clone()),
                        name: device.name.clone(),
                        sample_rate: spec.sample_rate,
                        buffer_frames: spec.buffer_frames,
                        buffer_fixed: spec.buffer_frames.is_some(),
                    }
                },
            )
        });

        AudioStatus {
            selection: state.selection.clone(),
            opened,
            devices: state.devices.clone(),
            default_output: state.default_key.clone(),
            missing_device,
            notices,
            failure: state.failure.clone(),
            stream_open: self.any_stream_open(),
            input: state.input.clone(),
            inputs: state.inputs.clone(),
            default_input: state.default_input_key.clone(),
            opened_input: state
                .opened_input
                .clone()
                .or_else(|| self.resolve_input(state)),
            missing_input: state.input_listed
                && state
                    .input
                    .input_device
                    .as_ref()
                    .is_some_and(|key| !state.inputs.iter().any(|device| &device.key == key)),
            input_notices: self.input_notices_for(state),
            input_failure: state.input_failure.clone(),
        }
    }

    /// Everything the preferences section says about capture, in one list.
    fn input_notices_for(&self, state: &IoState) -> Vec<AudioNotice> {
        if !state.input_listed {
            return Vec::new();
        }
        let mut notices = Vec::new();
        if state
            .input
            .input_device
            .as_ref()
            .is_some_and(|key| !state.inputs.iter().any(|device| &device.key == key))
        {
            let name = state
                .input
                .input_device_name
                .clone()
                .or_else(|| state.input.input_device.clone())
                .unwrap_or_default();
            notices.push(AudioNotice::MissingInputDevice {
                key: state.input.input_device.clone().unwrap_or_default(),
                name,
            });
        }
        notices.extend(
            self.input_spec_for_state(state)
                .1
                .into_iter()
                .filter(|notice| !matches!(notice, AudioNotice::MissingInputDevice { .. })),
        );
        notices.extend(state.input_notices.iter().cloned());
        notices
    }

    fn any_stream_open(&self) -> bool {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.retain(|weak| weak.strong_count() > 0);
        streams
            .iter()
            .filter_map(Weak::upgrade)
            .any(|stream| stream.is_open())
    }
}

fn save_selection(
    path: &Path,
    selection: &AudioSelection,
    input: &InputSelection,
    click: &crate::click::ClickSettings,
) -> Result<(), String> {
    let stored = StoredSettings {
        output: selection.clone(),
        input: input.clone(),
        click: *click,
    };
    let json = serde_json::to_string_pretty(&stored)
        .map_err(|error| format!("encode audio settings: {error}"))?;
    crate::private_fs::write_atomic(path, json.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))
}

struct SilentSink;

impl RenderSink for SilentSink {
    fn fill(&self, output: &mut [f32]) {
        output.fill(0.0);
    }
}

/// Consumes capture frames for a probe open without keeping them.
struct SilentInput;

impl InputSink for SilentInput {
    fn capture(&self, _samples: &[f32], _channels: u16) {}
}

// ============================================================================
// cpal backend
// ============================================================================

/// The real backend: cpal's default host.
pub struct CpalBackend;

impl CpalBackend {
    /// Create the platform's CPAL device backend.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Capture format the device accepts for `channels` at `rate`, preferring
/// the requested layout and a rate that falls inside the range.
fn pick_input_range(
    device: &cpal::Device,
    channels: u16,
    rate: u32,
) -> Option<cpal::SupportedStreamConfigRange> {
    use cpal::traits::DeviceTrait;
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .ok()?
        .filter(|range| range.sample_format() == cpal::SampleFormat::F32)
        .collect();
    let covers_rate = |range: &cpal::SupportedStreamConfigRange| {
        rate >= range.min_sample_rate().0 && rate <= range.max_sample_rate().0
    };
    ranges
        .iter()
        .filter(|range| covers_rate(range) && range.channels() >= channels)
        .min_by_key(|range| range.channels())
        .or_else(|| ranges.iter().find(|range| covers_rate(range)))
        .or_else(|| {
            ranges
                .iter()
                .filter(|range| range.channels() >= channels)
                .min_by_key(|range| range.channels())
        })
        .cloned()
}

impl DeviceBackend for CpalBackend {
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let default_name = host
            .default_output_device()
            .and_then(|device| device.name().ok());
        let mut devices = Vec::new();
        let iter = host.output_devices().map_err(|error| error.to_string())?;
        for device in iter {
            let Ok(name) = device.name() else { continue };
            let key = stable_key(&name);
            let is_default = default_name.as_deref() == Some(name.as_str());
            let mut rates = BTreeSet::new();
            let mut channels = 0u16;
            let mut buffer = None;
            if let Ok(configs) = device.supported_output_configs() {
                for range in configs {
                    channels = channels.max(range.channels());
                    if range.channels() < 2 {
                        continue;
                    }
                    for rate in STANDARD_RATES {
                        if rate >= range.min_sample_rate().0 && rate <= range.max_sample_rate().0 {
                            rates.insert(rate);
                        }
                    }
                    if buffer.is_none()
                        && let cpal::SupportedBufferSize::Range { min, max } = range.buffer_size()
                    {
                        buffer = Some((*min, *max));
                    }
                }
            }
            let default_rate = device
                .default_output_config()
                .ok()
                .map(|config| config.sample_rate().0)
                .or_else(|| rates.contains(&48_000).then_some(48_000))
                .or_else(|| rates.first().copied())
                .unwrap_or(FALLBACK_RATE);
            devices.push(OutputDevice {
                key,
                name,
                is_default,
                channels,
                sample_rates: rates.into_iter().collect(),
                default_rate,
                buffer_frames: buffer,
            });
        }
        Ok(devices)
    }

    fn default_output(&self) -> Option<String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        Some(stable_key(&device.name().ok()?))
    }

    fn list_inputs(&self) -> Result<Vec<InputDevice>, String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|device| device.name().ok());
        let mut devices = Vec::new();
        let iter = host.input_devices().map_err(|error| error.to_string())?;
        for device in iter {
            let Ok(name) = device.name() else { continue };
            let key = stable_key(&name);
            let is_default = default_name.as_deref() == Some(name.as_str());
            let mut rates = BTreeSet::new();
            let mut channels = 0u16;
            if let Ok(configs) = device.supported_input_configs() {
                for range in configs {
                    channels = channels.max(range.channels());
                    for rate in STANDARD_RATES {
                        if rate >= range.min_sample_rate().0 && rate <= range.max_sample_rate().0 {
                            rates.insert(rate);
                        }
                    }
                }
            }
            let default_rate = device
                .default_input_config()
                .ok()
                .map(|config| config.sample_rate().0)
                .or_else(|| rates.contains(&48_000).then_some(48_000))
                .or_else(|| rates.first().copied())
                .unwrap_or(FALLBACK_RATE);
            devices.push(InputDevice {
                key,
                name,
                is_default,
                channels,
                sample_rates: rates.into_iter().collect(),
                default_rate,
            });
        }
        Ok(devices)
    }

    fn default_input(&self) -> Option<String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let device = host.default_input_device()?;
        Some(stable_key(&device.name().ok()?))
    }

    fn open_input(&self, spec: &InputSpec, sink: Arc<dyn InputSink>) -> Result<StreamSink, String> {
        let spec = spec.clone();
        let (owner, details) =
            crate::stream_owner::StreamOwner::open("audio-input-owner", move || {
                open_cpal_input(&spec, sink)
            })?;
        Ok(details.into_sink(owner))
    }

    fn open_output(
        &self,
        spec: &StreamSpec,
        sink: Arc<dyn RenderSink>,
    ) -> Result<StreamSink, String> {
        let spec = spec.clone();
        let (owner, details) =
            crate::stream_owner::StreamOwner::open("audio-output-owner", move || {
                open_cpal_output(&spec, sink)
            })?;
        Ok(details.into_sink(owner))
    }
}

struct StreamDetails {
    sample_rate: u32,
    input_channels: Option<u16>,
    buffer_frames: Option<u32>,
    buffer_fixed: bool,
    fallback: Option<String>,
}
impl StreamDetails {
    fn into_sink(self, owner: crate::stream_owner::StreamOwner) -> StreamSink {
        let mut stream = StreamSink::new(
            owner,
            self.sample_rate,
            self.buffer_frames,
            self.buffer_fixed,
            self.fallback,
        );
        stream.input_channels = self.input_channels;
        stream
    }
}
fn open_cpal_input(
    spec: &InputSpec,
    sink: Arc<dyn InputSink>,
) -> Result<(cpal::Stream, StreamDetails), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let device = match &spec.key {
        Some(key) => host
            .input_devices()
            .map_err(|error| error.to_string())?
            .find(|device| {
                device
                    .name()
                    .ok()
                    .is_some_and(|name| &stable_key(&name) == key)
            })
            .ok_or_else(|| format!("input device {key} is not connected"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?,
    };
    let name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
    let range = pick_input_range(&device, spec.channels, spec.sample_rate).ok_or_else(|| {
        format!(
            "{name}: no 32-bit float capture format at {} Hz",
            spec.sample_rate
        )
    })?;
    let channels = range.channels();
    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(spec.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let capture = Arc::clone(&sink);
    let errors = Arc::clone(&sink);
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| capture.capture(data, channels),
            move |error| {
                log::error!("audio input stream failed: {error}");
                errors.failed(error.to_string());
            },
            None,
        )
        .map_err(|error| format!("{name}: {error}"))?;
    stream.play().map_err(|error| format!("{name}: {error}"))?;
    log::info!(
        "audio: opened input {} at {} Hz, {} channel(s)",
        name,
        spec.sample_rate,
        channels
    );
    Ok((
        stream,
        StreamDetails {
            sample_rate: spec.sample_rate,
            input_channels: Some(channels),
            buffer_frames: None,
            buffer_fixed: false,
            fallback: None,
        },
    ))
}
fn open_cpal_output(
    spec: &StreamSpec,
    sink: Arc<dyn RenderSink>,
) -> Result<(cpal::Stream, StreamDetails), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let device = match &spec.key {
        Some(key) => host
            .output_devices()
            .map_err(|error| error.to_string())?
            .find(|device| {
                device
                    .name()
                    .ok()
                    .is_some_and(|name| &stable_key(&name) == key)
            })
            .ok_or_else(|| format!("device {key} is not connected"))?,
        None => host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?,
    };
    let name = device.name().unwrap_or_else(|_| "<unknown>".to_string());

    // Ask for a fixed period when one was chosen; a backend that refuses it
    // falls back to the device period with a reason instead of failing.
    let mut attempts = vec![spec.buffer_frames];
    if spec.buffer_frames.is_some() {
        attempts.push(None);
    }
    let mut fallback = None;
    let mut last_error = String::new();
    for frames in attempts {
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(spec.sample_rate),
            buffer_size: frames
                .map(cpal::BufferSize::Fixed)
                .unwrap_or(cpal::BufferSize::Default),
        };
        let render = Arc::clone(&sink);
        let errors = Arc::clone(&sink);
        let built = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| render.fill(data),
            move |error| {
                log::error!("audio output stream failed: {error}");
                errors.failed(error.to_string());
            },
            None,
        );
        let stream = match built {
            Ok(stream) => stream,
            Err(error) if frames.is_some() => {
                fallback = Some(error.to_string());
                last_error = error.to_string();
                continue;
            }
            Err(error) => {
                return Err(format!("{name}: {error}"));
            }
        };
        stream.play().map_err(|error| format!("{name}: {error}"))?;
        log::info!(
            "audio: opened {} at {} Hz, buffer {}",
            name,
            spec.sample_rate,
            frames
                .map(|frames| format!("{frames} frames"))
                .unwrap_or_else(|| "device default".to_string())
        );
        return Ok((
            stream,
            StreamDetails {
                sample_rate: spec.sample_rate,
                input_channels: None,
                buffer_frames: frames,
                buffer_fixed: frames.is_some(),
                fallback,
            },
        ));
    }
    Err(format!("{name}: {last_error}"))
}

// ============================================================================
// Stable device keys
// ============================================================================

/// Backend-stable identity of the device that reports `name`.
fn stable_key(name: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(uid) = coreaudio::device_uid(name) {
            return format!("coreaudio:{uid}");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(id) = wasapi::endpoint_id(name) {
            return format!("wasapi:{id}");
        }
    }
    #[cfg(target_os = "linux")]
    {
        // cpal reports the ALSA PCM name here, which is the stable identifier.
        format!("alsa:{name}")
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("name:{name}")
    }
}

#[cfg(target_os = "macos")]
mod coreaudio {
    use std::ffi::c_void;
    use std::mem;
    use std::ptr::null;

    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use coreaudio_sys::{
        AudioDeviceID, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectPropertyAddress, kAudioDevicePropertyDeviceNameCFString,
        kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDevices,
        kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject,
    };

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        }
    }

    fn device_ids() -> Vec<AudioDeviceID> {
        let address = address(kAudioHardwarePropertyDevices);
        let mut size = 0u32;
        // SAFETY: the address and writable size scalar live through this query;
        // no qualifier data is supplied.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &address, 0, null(), &mut size)
        };
        if status != 0
            || size == 0
            || !(size as usize).is_multiple_of(mem::size_of::<AudioDeviceID>())
        {
            return Vec::new();
        }
        let count = size as usize / mem::size_of::<AudioDeviceID>();
        let mut ids = vec![0 as AudioDeviceID; count];
        // SAFETY: the initialized, aligned IDs buffer has exactly `size` bytes;
        // CoreAudio uses that in/out capacity and rejects a larger property.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &address,
                0,
                null(),
                &mut size,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        let returned = size as usize / mem::size_of::<AudioDeviceID>();
        if returned > ids.len() || !(size as usize).is_multiple_of(mem::size_of::<AudioDeviceID>())
        {
            return Vec::new();
        }
        ids.truncate(returned);
        ids
    }

    fn string_property(device: AudioDeviceID, selector: u32) -> Option<String> {
        let address = address(selector);
        let mut value: core_foundation_sys::string::CFStringRef = std::ptr::null();
        let mut size = mem::size_of::<core_foundation_sys::string::CFStringRef>() as u32;
        // SAFETY: `value` is writable pointer-sized storage matching this CFString
        // property; address and in/out capacity remain live during the query.
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                &address,
                0,
                null(),
                &mut size,
                &mut value as *mut _ as *mut c_void,
            )
        };
        if status != 0 || value.is_null() {
            return None;
        }
        // SAFETY: this successful CFString-valued CoreAudio property transfers
        // one non-null reference to the caller; the wrapper releases it once.
        Some(unsafe { CFString::wrap_under_create_rule(value) }.to_string())
    }

    /// CoreAudio UID of the device that reports `name`.
    pub(super) fn device_uid(name: &str) -> Option<String> {
        for id in device_ids() {
            let Some(candidate) = string_property(id, kAudioDevicePropertyDeviceNameCFString)
            else {
                continue;
            };
            if candidate == name {
                return string_property(id, kAudioDevicePropertyDeviceUID);
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
mod wasapi {
    //! WASAPI endpoint identity. cpal exposes only the friendly name, so the
    //! endpoint id comes from the same device enumerator the host uses.

    use std::marker::PhantomData;
    use std::rc::Rc;

    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::core::{HRESULT, PWSTR};

    // COM initialization is per-thread. A changed mode borrows the caller's
    // existing apartment; all successful calls, including S_FALSE, own one balance.
    fn initialization_ownership(result: HRESULT) -> Option<bool> {
        if result.is_ok() {
            Some(true)
        } else if result == RPC_E_CHANGED_MODE {
            Some(false)
        } else {
            None
        }
    }

    struct ComApartment {
        initialized: bool,
        _thread: PhantomData<Rc<()>>,
    }
    impl ComApartment {
        fn initialize(mode: COINIT) -> Option<Self> {
            // SAFETY: reserved storage is null and `mode` is an SDK COINIT value.
            // The guard is !Send/!Sync and balances only this thread's successful call.
            let result = unsafe { CoInitializeEx(None, mode) };
            Some(Self {
                initialized: initialization_ownership(result)?,
                _thread: PhantomData,
            })
        }
    }
    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.initialized {
                // SAFETY: this !Send guard remains on its initialization thread,
                // after all endpoint interfaces have dropped, and owns exactly one
                // successful CoInitializeEx call (S_OK or S_FALSE).
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Owns the CoTaskMem allocation returned by a successful IMMDevice::GetId.
    struct EndpointIdString(PWSTR);
    impl EndpointIdString {
        fn decode(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            // SAFETY: GetId's successful output (or the test's task allocation)
            // is a null-terminated UTF-16 buffer, kept allocated by this owner
            // through conversion. Invalid UTF-16 returns None without losing ownership.
            unsafe { self.0.to_string().ok() }
        }
    }
    impl Drop for EndpointIdString {
        fn drop(&mut self) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: the successful GetId call transfers one task-allocated
            // string. This non-Clone owner is its sole releaser, including when
            // decoding fails; neither PWSTR nor IMMDevice releases that allocation.
            unsafe { CoTaskMemFree(Some(self.0.0.cast())) };
            #[cfg(test)]
            FREED_ENDPOINT_IDS.with(|count| count.set(count.get() + 1));
        }
    }
    #[cfg(test)]
    std::thread_local! { static FREED_ENDPOINT_IDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

    fn friendly_name(device: &IMMDevice, _apartment: &ComApartment) -> Option<String> {
        // SAFETY: this IMMDevice was obtained in the guard's current apartment;
        // STGM_READ requests a read-only store and the generated binding owns
        // the successful output interface until `store` drops on this thread.
        let store: IPropertyStore = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
        // SAFETY: the SDK property key has static storage and the binding supplies
        // initialized PROPVARIANT output storage. On success windows-core owns
        // the value and its Drop calls PropVariantClear exactly once.
        let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok()? };
        Some(value.to_string())
    }

    fn endpoint_id_for(device: &IMMDevice, _apartment: &ComApartment) -> Option<String> {
        // SAFETY: the interface belongs to this thread's guarded apartment.
        // The binding supplies initialized pointer output; GetId returns a
        // caller-owned CoTaskMem, null-terminated string only after success.
        let id = EndpointIdString(unsafe { device.GetId().ok()? });
        id.decode()
    }

    /// WASAPI endpoint id of the render endpoint with this friendly name.
    pub(super) fn endpoint_id(name: &str) -> Option<String> {
        // Declared first: all COM interfaces below drop before this guard.
        let apartment = ComApartment::initialize(COINIT_MULTITHREADED)?;
        // SAFETY: the current thread has a successful initialization or an
        // existing apartment, and no aggregation/outer IUnknown is requested.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()? };
        // SAFETY: enumerator is owned on this initialized thread, SDK flags
        // select active render endpoints, and the binding owns the output collection.
        let collection = unsafe {
            enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .ok()?
        };
        // SAFETY: collection stays owned in this apartment while the binding
        // provides writable count storage and checks native success.
        let count = unsafe { collection.GetCount().ok()? };
        for index in 0..count {
            // SAFETY: index is below the collection's reported count. A changing
            // device list may refuse Item; the checked Result simply skips it.
            // Each successful interface remains scoped to this apartment.
            let Ok(device) = (unsafe { collection.Item(index) }) else {
                continue;
            };
            if friendly_name(&device, &apartment).as_deref() == Some(name) {
                return endpoint_id_for(&device, &apartment);
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::Foundation::{CO_E_NOTINITIALIZED, E_OUTOFMEMORY, S_FALSE, S_OK};
        use windows::Win32::System::Com::{
            APTTYPE, APTTYPE_MTA, APTTYPEQUALIFIER, APTTYPEQUALIFIER_IMPLICIT_MTA,
            APTTYPEQUALIFIER_NONE, COINIT_APARTMENTTHREADED, CoGetApartmentType, CoTaskMemAlloc,
        };

        fn apartment_type() -> windows::core::Result<(APTTYPE, APTTYPEQUALIFIER)> {
            let mut kind = APTTYPE::default();
            let mut qualifier = APTTYPEQUALIFIER::default();
            // SAFETY: both outputs are initialized SDK scalars with writable
            // stack storage throughout the query, which is valid even before initialization.
            unsafe { CoGetApartmentType(&mut kind, &mut qualifier)? };
            Ok((kind, qualifier))
        }

        fn assert_no_owned_initialization() {
            match apartment_type() {
                Err(error) => assert_eq!(error.code(), CO_E_NOTINITIALIZED),
                Ok((kind, qualifier)) => {
                    // Another test thread can keep the process MTA alive. Its
                    // implicit membership is not an initialization owned here.
                    assert_eq!(kind, APTTYPE_MTA);
                    assert_eq!(qualifier, APTTYPEQUALIFIER_IMPLICIT_MTA);
                }
            }
        }

        #[test]
        fn initialization_refuses_failure_and_balances_success_including_s_false() {
            assert_eq!(initialization_ownership(S_OK), Some(true));
            assert_eq!(initialization_ownership(S_FALSE), Some(true));
            assert_eq!(initialization_ownership(RPC_E_CHANGED_MODE), Some(false));
            assert_eq!(initialization_ownership(E_OUTOFMEMORY), None);
            std::thread::spawn(|| {
                assert_no_owned_initialization();
                let first = ComApartment::initialize(COINIT_MULTITHREADED).unwrap();
                let second = ComApartment::initialize(COINIT_MULTITHREADED).unwrap();
                assert!(first.initialized && second.initialized);
                drop(second);
                assert_eq!(
                    apartment_type().unwrap(),
                    (APTTYPE_MTA, APTTYPEQUALIFIER_NONE)
                );
                drop(first);
                assert_no_owned_initialization();
            })
            .join()
            .unwrap();
        }

        #[test]
        fn changed_mode_guard_does_not_uninitialize_the_callers_sta() {
            std::thread::spawn(|| {
                let outer = ComApartment::initialize(COINIT_APARTMENTTHREADED).unwrap();
                let before = apartment_type().unwrap();
                let borrowed = ComApartment::initialize(COINIT_MULTITHREADED).unwrap();
                assert!(!borrowed.initialized);
                drop(borrowed);
                assert_eq!(apartment_type().unwrap(), before);
                drop(outer);
                assert_no_owned_initialization();
            })
            .join()
            .unwrap();
        }

        fn task_string(units: &[u16]) -> EndpointIdString {
            assert_eq!(units.last(), Some(&0));
            // SAFETY: the task allocator needs no COM initialization; the exact
            // byte count holds every input UTF-16 unit including the terminator.
            let buffer = unsafe { CoTaskMemAlloc(std::mem::size_of_val(units)) }.cast::<u16>();
            assert!(!buffer.is_null());
            // SAFETY: buffer is a distinct live allocation of units.len()*2
            // bytes with sufficient alignment, and the input slice remains live.
            unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), buffer, units.len()) };
            EndpointIdString(PWSTR(buffer))
        }

        #[test]
        fn endpoint_id_task_memory_is_freed_on_valid_and_invalid_utf16_return_paths() {
            let before = FREED_ENDPOINT_IDS.with(std::cell::Cell::get);
            let valid = task_string(&[b'e' as u16, b'p' as u16, 0]);
            assert_eq!(valid.decode().as_deref(), Some("ep"));
            assert_eq!(FREED_ENDPOINT_IDS.with(std::cell::Cell::get), before);
            drop(valid);
            assert_eq!(FREED_ENDPOINT_IDS.with(std::cell::Cell::get), before + 1);
            fn invalid_result() -> Option<String> {
                let invalid = task_string(&[0xd800, 0]);
                invalid.decode()
            }
            assert_eq!(invalid_result(), None);
            assert_eq!(FREED_ENDPOINT_IDS.with(std::cell::Cell::get), before + 2);
            assert_eq!(EndpointIdString(PWSTR::null()).decode(), None);
            assert_eq!(FREED_ENDPOINT_IDS.with(std::cell::Cell::get), before + 2);
        }
    }
}

// ============================================================================
// Test double
// ============================================================================

/// Headless backend: a mutable device list, per-device formats, scripted open
/// failures and disconnect reports.
#[derive(Default)]
pub struct FakeBackend {
    inner: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    devices: Vec<OutputDevice>,
    default: Option<String>,
    /// `(device name, message)` pairs that refuse to open.
    open_failures: Vec<(String, String)>,
    /// Devices that refuse a fixed period.
    fixed_refused: Vec<String>,
    /// Every open attempt, including probes.
    opens: Vec<(Option<String>, StreamSpec)>,
    /// Callbacks of the streams that are currently open.
    sinks: Vec<Arc<dyn RenderSink>>,
    inputs: Vec<InputDevice>,
    input_lists: usize,
    default_input: Option<String>,
    /// `(device name, message)` pairs that refuse to capture.
    input_failures: Vec<(String, String)>,
    /// Every capture open attempt, probes included.
    input_opens: Vec<(Option<String>, InputSpec)>,
    /// Capture callbacks of the input streams that are open.
    input_sinks: Vec<Arc<dyn InputSink>>,
}

impl FakeBackend {
    pub fn new(devices: Vec<OutputDevice>, default: Option<String>) -> Self {
        Self {
            inner: Mutex::new(FakeState {
                devices,
                default,
                ..FakeState::default()
            }),
        }
    }

    /// Add capture devices to a fake backend.
    pub fn with_inputs(self, inputs: Vec<InputDevice>, default: Option<String>) -> Self {
        {
            let mut state = self.lock();
            state.inputs = inputs;
            state.default_input = default;
        }
        self
    }

    pub fn set_inputs(&self, inputs: Vec<InputDevice>) {
        self.lock().inputs = inputs;
    }

    pub fn fail_input_open(&self, device: &str, message: &str) {
        self.lock()
            .input_failures
            .push((device.to_string(), message.to_string()));
    }

    /// Every capture open attempt in order, probes included.
    pub fn input_opens(&self) -> Vec<(Option<String>, InputSpec)> {
        self.lock().input_opens.clone()
    }

    /// Deliver capture frames to every open input stream, exactly as a device
    /// callback would.
    pub fn push_input(&self, samples: &[f32], channels: u16) {
        let sinks = self.lock().input_sinks.clone();
        for sink in sinks {
            sink.capture(samples, channels);
        }
    }

    /// Report an error to every open input stream.
    pub fn disconnect_input(&self, message: &str) {
        let sinks = self.lock().input_sinks.clone();
        for sink in sinks {
            sink.failed(message.to_string());
        }
    }

    pub fn set_devices(&self, devices: Vec<OutputDevice>) {
        self.lock().devices = devices;
    }

    pub fn fail_open(&self, device: &str, message: &str) {
        self.lock()
            .open_failures
            .push((device.to_string(), message.to_string()));
    }

    pub fn refuse_fixed_buffer(&self, device: &str) {
        self.lock().fixed_refused.push(device.to_string());
    }

    /// Every open attempt in order, probes included.
    pub fn opens(&self) -> Vec<(Option<String>, StreamSpec)> {
        self.lock().opens.clone()
    }

    /// Report a device error to the last stream opened on it.
    pub fn disconnect(&self, message: &str) {
        let sinks = self.lock().sinks.clone();
        for sink in sinks {
            sink.failed(message.to_string());
        }
    }

    /// Drive the last output callback without opening a real audio device.
    pub fn fill_output(&self, samples: &mut [f32]) {
        let sink = self.lock().sinks.last().cloned();
        if let Some(sink) = sink {
            sink.fill(samples);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl DeviceBackend for FakeBackend {
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, String> {
        Ok(self.lock().devices.clone())
    }

    fn default_output(&self) -> Option<String> {
        self.lock().default.clone()
    }

    fn open_output(
        &self,
        spec: &StreamSpec,
        sink: Arc<dyn RenderSink>,
    ) -> Result<StreamSink, String> {
        let mut state = self.lock();
        let device = match &spec.key {
            Some(key) => state
                .devices
                .iter()
                .find(|device| &device.key == key)
                .cloned(),
            None => {
                let key = state.default.clone();
                state
                    .devices
                    .iter()
                    .find(|device| Some(&device.key) == key.as_ref())
                    .or_else(|| state.devices.iter().find(|device| device.is_default))
                    .cloned()
            }
        }
        .ok_or_else(|| "no default output device".to_string())?;
        let name = device.name.clone();
        let failure = state
            .open_failures
            .iter()
            .find(|(failed, _)| failed == &name)
            .map(|(_, message)| message.clone());
        if let Some(message) = failure {
            state.opens.push((spec.key.clone(), spec.clone()));
            return Err(message);
        }
        let mut fallback = None;
        if spec.buffer_frames.is_some() && state.fixed_refused.iter().any(|fixed| fixed == &name) {
            fallback = Some("the device refuses a fixed period".to_string());
        }
        if fallback.is_some() {
            state.opens.push((spec.key.clone(), spec.clone()));
            state.sinks.push(Arc::clone(&sink));
            return Ok(StreamSink::new((), spec.sample_rate, None, false, fallback));
        }
        state.opens.push((spec.key.clone(), spec.clone()));
        state.sinks.push(sink);
        Ok(StreamSink::new(
            (),
            spec.sample_rate,
            spec.buffer_frames,
            spec.buffer_frames.is_some(),
            None,
        ))
    }

    fn list_inputs(&self) -> Result<Vec<InputDevice>, String> {
        let mut state = self.lock();
        state.input_lists += 1;
        Ok(state.inputs.clone())
    }

    fn default_input(&self) -> Option<String> {
        self.lock().default_input.clone()
    }

    fn open_input(&self, spec: &InputSpec, sink: Arc<dyn InputSink>) -> Result<StreamSink, String> {
        let mut state = self.lock();
        let device = match &spec.key {
            Some(key) => state
                .inputs
                .iter()
                .find(|device| &device.key == key)
                .cloned(),
            None => {
                let key = state.default_input.clone();
                state
                    .inputs
                    .iter()
                    .find(|device| Some(&device.key) == key.as_ref())
                    .or_else(|| state.inputs.iter().find(|device| device.is_default))
                    .cloned()
            }
        }
        .ok_or_else(|| "no default input device".to_string())?;
        let name = device.name.clone();
        let failure = state
            .input_failures
            .iter()
            .find(|(failed, _)| failed == &name)
            .map(|(_, message)| message.clone());
        state.input_opens.push((spec.key.clone(), spec.clone()));
        if let Some(message) = failure {
            return Err(message);
        }
        state.input_sinks.push(sink);
        Ok(StreamSink::new((), spec.sample_rate, None, false, None))
    }
}

/// A backend with no devices at all, for headless runs.
pub struct NullBackend;

impl DeviceBackend for NullBackend {
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, String> {
        Ok(Vec::new())
    }

    fn default_output(&self) -> Option<String> {
        None
    }

    fn open_output(
        &self,
        _spec: &StreamSpec,
        _sink: Arc<dyn RenderSink>,
    ) -> Result<StreamSink, String> {
        Err("no audio output device".to_string())
    }

    fn list_inputs(&self) -> Result<Vec<InputDevice>, String> {
        Ok(Vec::new())
    }

    fn default_input(&self) -> Option<String> {
        None
    }

    fn open_input(
        &self,
        _spec: &InputSpec,
        _sink: Arc<dyn InputSink>,
    ) -> Result<StreamSink, String> {
        Err("no audio input device".to_string())
    }
}

#[cfg(test)]
mod tests;
