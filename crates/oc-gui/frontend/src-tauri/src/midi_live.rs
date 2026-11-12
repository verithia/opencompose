//! Live MIDI input: port discovery, persisted selection, and audition routing.
//!
//! One MIDI thread polls the selected port and pushes into the audition producer
//! slot; the audio callback drains its own consumer without allocating or locking.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::live_synth;
use crate::midi_bridge::{
    LiveEvent, MidiBackend, MidiInputEvent, MidiPortInfo, MidiSession, MidirBackend,
};
use crate::state::AppState;

const PREFERENCES_FILE: &str = "midi.json";
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const INPUT_BUFFER: usize = 64;

const KIND_NONE: u8 = 0;
const KIND_NOTE_ON: u8 = 1;
const KIND_NOTE_OFF: u8 = 2;
const KIND_POLY_PRESSURE: u8 = 3;
const KIND_CONTROL_CHANGE: u8 = 4;
const KIND_PROGRAM_CHANGE: u8 = 5;
const KIND_CHANNEL_PRESSURE: u8 = 6;
const KIND_PITCH_BEND: u8 = 7;

// ============================================================================
// Public wire types
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// Persisted live MIDI input selection.
pub struct MidiPreferences {
    pub input: Option<String>,
    pub monitor: bool,
    /// Live MPE zones; `None` keeps the controller on the plain MIDI path.
    pub mpe: Option<oc_synth::synth::MpeConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// One MIDI port as shown to the UI.
pub struct MidiPortEntry {
    pub index: usize,
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Available MIDI ports and backend support.
pub struct MidiPorts {
    pub supported: bool,
    pub inputs: Vec<MidiPortEntry>,
    pub outputs: Vec<MidiPortEntry>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
/// Current live MIDI configuration and activity.
pub struct MidiConfigView {
    pub input: Option<String>,
    pub monitor: bool,
    /// Live MPE zones; applied to the next audition or pushed to an open one.
    pub mpe: Option<oc_synth::synth::MpeConfig>,
    /// A MIDI thread is currently polling the selected input.
    pub active: bool,
    /// An OpenSynth audition is open, so input is audible.
    pub audition_active: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// One recent MIDI event for the activity meter.
pub struct MidiActivityEvent {
    pub kind: String,
    pub channel: u8,
    pub note: Option<u8>,
    pub velocity: Option<u8>,
    pub controller: Option<u8>,
    pub value: Option<i32>,
    pub millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
/// Live MIDI activity snapshot.
pub struct MidiActivityView {
    pub running: bool,
    pub events: u64,
    pub last: Option<MidiActivityEvent>,
    pub error: Option<String>,
    pub audition_active: bool,
}

// ============================================================================
// Activity probe — lock-free surface read by Tauri commands
// ============================================================================

struct MidiActivityProbe {
    started: Instant,
    events: AtomicU64,
    last: AtomicU64,
    last_millis: AtomicU64,
    running: AtomicBool,
}

impl MidiActivityProbe {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            events: AtomicU64::new(0),
            last: AtomicU64::new(0),
            last_millis: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }

    fn record(&self, event: LiveEvent) {
        self.events.fetch_add(1, Ordering::Relaxed);
        let packed = pack_event(event);
        if packed != 0 {
            self.last.store(packed, Ordering::Relaxed);
            self.last_millis
                .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
    }

    fn events(&self) -> u64 {
        self.events.load(Ordering::Relaxed)
    }

    fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Release);
    }

    fn last_event(&self) -> Option<MidiActivityEvent> {
        let packed = self.last.load(Ordering::Relaxed);
        if packed == 0 {
            return None;
        }
        let kind = (packed & 0xFF) as u8;
        let data1 = ((packed >> 8) & 0xFF) as u8;
        let data2 = ((packed >> 16) & 0xFF) as u8;
        let channel = ((packed >> 24) & 0xFF) as u8;
        let extra = ((packed >> 32) & 0xFFFF) as i32;
        let (name, note, velocity, controller, value) = match kind {
            KIND_NOTE_ON => ("note_on", Some(data1), Some(data2), None, None),
            KIND_NOTE_OFF => ("note_off", Some(data1), Some(data2), None, None),
            KIND_POLY_PRESSURE => (
                "poly_pressure",
                Some(data1),
                None,
                None,
                Some(i32::from(data2)),
            ),
            KIND_CONTROL_CHANGE => (
                "control_change",
                None,
                None,
                Some(data1),
                Some(i32::from(data2)),
            ),
            KIND_PROGRAM_CHANGE => ("program_change", None, None, None, Some(i32::from(data1))),
            KIND_CHANNEL_PRESSURE => ("channel_pressure", None, None, None, Some(i32::from(data1))),
            KIND_PITCH_BEND => ("pitch_bend", None, None, None, Some(extra - 8192)),
            _ => ("unknown", None, None, None, None),
        };
        Some(MidiActivityEvent {
            kind: name.to_string(),
            channel,
            note,
            velocity,
            controller,
            value,
            millis: self.last_millis.load(Ordering::Relaxed),
        })
    }
}

fn pack_event(event: LiveEvent) -> u64 {
    let pack = |kind: u8, data1: u8, data2: u8, channel: u8, extra: u16| {
        u64::from(kind)
            | (u64::from(data1) << 8)
            | (u64::from(data2) << 16)
            | (u64::from(channel) << 24)
            | (u64::from(extra) << 32)
    };
    match event {
        LiveEvent::NoteOn {
            channel,
            note,
            velocity,
        } => pack(KIND_NOTE_ON, note, velocity, channel, 0),
        LiveEvent::NoteOff {
            channel,
            note,
            velocity,
        } => pack(KIND_NOTE_OFF, note, velocity, channel, 0),
        LiveEvent::PolyPressure {
            channel,
            note,
            pressure,
        } => pack(KIND_POLY_PRESSURE, note, pressure, channel, 0),
        LiveEvent::ControlChange {
            channel,
            controller,
            value,
        } => pack(KIND_CONTROL_CHANGE, controller, value, channel, 0),
        LiveEvent::ProgramChange { channel, program } => {
            pack(KIND_PROGRAM_CHANGE, program, 0, channel, 0)
        }
        LiveEvent::ChannelPressure { channel, pressure } => {
            pack(KIND_CHANNEL_PRESSURE, pressure, 0, channel, 0)
        }
        LiveEvent::PitchBend { channel, value } => pack(
            KIND_PITCH_BEND,
            0,
            0,
            channel,
            (i32::from(value) + 8192).clamp(0, 16383) as u16,
        ),
        // Realtime clock is counted but never hides the last played note.
        LiveEvent::Clock | LiveEvent::Start | LiveEvent::Continue | LiveEvent::Stop => {
            KIND_NONE as u64
        }
    }
}

// ============================================================================
// Routing sink
// ============================================================================

pub(crate) trait LiveSink: Send + 'static {
    fn route(&mut self, event: LiveEvent);
}

/// Push live events into the active OpenSynth audition producer slot, and into
/// the take capture when one is open.
pub(crate) struct LiveInputSink {
    pub(crate) route: live_synth::Route,
    pub(crate) capture: Arc<crate::midi_record::MidiCapture>,
}

impl LiveSink for LiveInputSink {
    fn route(&mut self, event: LiveEvent) {
        self.capture.record(event);
        let mut route = self.route.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(producer) = route.as_mut() {
            producer.push_midi(event);
        }
    }
}

// ============================================================================
// Runtime
// ============================================================================

/// Polling MIDI thread and its lock-free activity probe.
pub struct MidiRuntime {
    probe: Arc<MidiActivityProbe>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}

impl MidiRuntime {
    /// Start a MIDI thread on a port selector with a live sink.
    pub fn start_midir<S: LiveSink + 'static>(selector: &str, sink: S) -> Result<Self, String> {
        let backend = MidirBackend::new().map_err(|error| error.to_string())?;
        Self::start_with(selector, backend, sink)
    }

    pub(crate) fn start_with<B: MidiBackend + 'static, S: LiveSink + 'static>(
        selector: &str,
        backend: B,
        sink: S,
    ) -> Result<Self, String> {
        let session = MidiSession::open(backend, Some(selector), None)
            .map_err(|error| format!("MIDI input '{selector}': {error}"))?;
        let probe = Arc::new(MidiActivityProbe::new());
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let thread = {
            let probe = Arc::clone(&probe);
            let stop = Arc::clone(&stop);
            let error = Arc::clone(&error);
            std::thread::Builder::new()
                .name("opencompose-midi-in".into())
                .spawn(move || {
                    run_input(session, sink, &probe, &stop, &error);
                    probe.set_running(false);
                })
                .map_err(|error| format!("Failed to start MIDI input thread: {error}"))?
        };
        Ok(Self {
            probe,
            stop,
            thread: Some(thread),
            error,
        })
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::warn!("MIDI input thread panicked");
        }
        self.probe.set_running(false);
    }

    fn running(&self) -> bool {
        self.probe.running()
    }

    fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn activity(&self) -> MidiActivityView {
        MidiActivityView {
            running: self.running(),
            events: self.probe.events(),
            last: self.probe.last_event(),
            error: self.error(),
            audition_active: false,
        }
    }
}

impl Drop for MidiRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_input<B: MidiBackend, S: LiveSink>(
    mut session: MidiSession<B>,
    mut sink: S,
    probe: &MidiActivityProbe,
    stop: &AtomicBool,
    error: &Mutex<Option<String>>,
) {
    let mut buffer = [MidiInputEvent::default(); INPUT_BUFFER];
    let mut held = [0u128; 16];
    let mut sustain = [false; 16];
    while !stop.load(Ordering::Acquire) {
        match session.poll(&mut buffer) {
            Ok(0) => std::thread::sleep(POLL_INTERVAL),
            Ok(count) => {
                for input in &buffer[..count] {
                    track_held_note(&mut held, input.event);
                    if let LiveEvent::ControlChange {
                        channel,
                        controller: 64,
                        value,
                    } = input.event
                    {
                        sustain[usize::from(channel & 0x0F)] = value >= 64;
                    }
                    probe.record(input.event);
                    sink.route(input.event);
                }
            }
            Err(failure) => {
                log::warn!("MIDI input stopped: {failure}");
                *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure.to_string());
                break;
            }
        }
    }
    release_held_notes(&held, &sustain, &mut sink);
}

fn track_held_note(held: &mut [u128; 16], event: LiveEvent) {
    match event {
        LiveEvent::NoteOn {
            channel,
            note,
            velocity,
        } if velocity > 0 => held[usize::from(channel & 0x0F)] |= 1 << note,
        LiveEvent::NoteOn { channel, note, .. } | LiveEvent::NoteOff { channel, note, .. } => {
            held[usize::from(channel & 0x0F)] &= !(1 << note);
        }
        _ => {}
    }
}

/// Stopping the monitor must not leave notes sounding on the audition synth.
fn release_held_notes<S: LiveSink>(held: &[u128; 16], sustain: &[bool; 16], sink: &mut S) {
    for (channel, notes) in held.iter().enumerate() {
        let mut notes = *notes;
        while notes != 0 {
            let note = notes.trailing_zeros() as u8;
            notes &= notes - 1;
            sink.route(LiveEvent::NoteOff {
                channel: channel as u8,
                note,
                velocity: 0,
            });
        }
        if sustain[channel] {
            sink.route(LiveEvent::ControlChange {
                channel: channel as u8,
                controller: 64,
                value: 0,
            });
        }
    }
}

// ============================================================================
// Preferences and selection state
// ============================================================================

/// Persisted MIDI selection and the optional runtime thread.
pub struct MidiLive {
    preferences: MidiPreferences,
    path: Option<PathBuf>,
    runtime: Option<MidiRuntime>,
    loaded: bool,
    error: Option<String>,
    /// The thread was opened for a take, not by the monitor preference, so
    /// stopping the take releases the device again.
    record_only: bool,
}

impl Default for MidiLive {
    fn default() -> Self {
        Self::new(preferences_path())
    }
}

impl MidiLive {
    /// Create the selection state with an optional preferences path.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            preferences: MidiPreferences::default(),
            path,
            runtime: None,
            loaded: false,
            error: None,
            record_only: false,
        }
    }

    /// Load the persisted selection once; missing or invalid files use defaults.
    pub fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Some(path) = &self.path {
            self.preferences = load_preferences(path);
        }
    }

    /// Apply input/monitor/MPE and start or stop the polling thread.
    pub fn configure_with_mpe(
        &mut self,
        input: Option<String>,
        monitor: bool,
        mpe: Option<oc_synth::synth::MpeConfig>,
        start: impl FnOnce(&str) -> Result<MidiRuntime, String>,
    ) -> MidiConfigView {
        self.ensure_loaded();
        self.stop();
        self.error = None;
        let input = input
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        self.preferences = MidiPreferences {
            input: input.clone(),
            monitor,
            mpe,
        };
        if let Some(path) = &self.path
            && let Err(error) = save_preferences(path, &self.preferences)
        {
            self.error = Some(error);
        }
        if monitor && let Some(selector) = input.as_deref() {
            match start(selector) {
                Ok(runtime) => self.runtime = Some(runtime),
                Err(error) => {
                    log::warn!("Live MIDI input unavailable: {error}");
                    self.error = Some(error);
                }
            }
        }
        self.view()
    }

    /// Stop the polling thread.
    pub fn stop(&mut self) {
        self.record_only = false;
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
            if let Some(error) = runtime.error() {
                self.error = Some(error);
            }
        }
    }

    /// The selected input, or `None` when no port is chosen.
    pub fn input(&self) -> Option<String> {
        self.preferences.input.clone()
    }

    /// A MIDI thread is polling the selected input.
    pub fn active(&self) -> bool {
        self.runtime.as_ref().is_some_and(MidiRuntime::running)
    }

    /// The last thread failure, if any.
    pub fn error(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.runtime.as_ref().and_then(MidiRuntime::error))
    }

    /// Open the selected input for a take, whether or not monitoring is on.
    ///
    /// A take must hear the keyboard even when the monitor is off, so this
    /// starts the thread the monitor preference did not. It leaves the stored
    /// preferences alone and marks the thread record-only, so stopping the take
    /// releases the device again.
    pub fn ensure_input(
        &mut self,
        start: impl FnOnce(&str) -> Result<MidiRuntime, String>,
    ) -> Result<(), String> {
        if self.active() {
            return Ok(());
        }
        self.ensure_loaded();
        let Some(selector) = self.preferences.input.clone() else {
            return Err(crate::midi_record::MidiRefusal::NoInput.reason());
        };
        match start(&selector) {
            Ok(runtime) => {
                // Without the monitor this thread exists only for the take.
                self.record_only = !self.preferences.monitor;
                self.runtime = Some(runtime);
                self.error = None;
                Ok(())
            }
            Err(error) => {
                log::warn!("MIDI input for recording unavailable: {error}");
                self.error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Stop a thread that only a take opened; a monitored thread stays.
    pub fn stop_if_record_only(&mut self) {
        if self.record_only {
            self.stop();
        }
    }

    /// Current configuration view.
    pub fn view(&self) -> MidiConfigView {
        MidiConfigView {
            input: self.preferences.input.clone(),
            monitor: self.preferences.monitor,
            mpe: self.preferences.mpe,
            active: self.runtime.as_ref().is_some_and(MidiRuntime::running),
            audition_active: false,
            error: self
                .error
                .clone()
                .or_else(|| self.runtime.as_ref().and_then(MidiRuntime::error)),
        }
    }

    pub(crate) fn monitoring_active(&self) -> bool {
        self.preferences.monitor && self.runtime.as_ref().is_some_and(MidiRuntime::running)
    }

    /// Current activity view.
    pub fn activity(&self) -> MidiActivityView {
        match &self.runtime {
            Some(runtime) => runtime.activity(),
            None => MidiActivityView {
                error: self.error.clone(),
                ..MidiActivityView::default()
            },
        }
    }
}

fn preferences_path() -> Option<PathBuf> {
    // A test build keeps MIDI preferences in the process scratch root.
    Some(crate::private_fs::config_dir().join(PREFERENCES_FILE))
}

fn load_preferences(path: &Path) -> MidiPreferences {
    let Ok(text) = std::fs::read_to_string(path) else {
        return MidiPreferences::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|error| {
        log::warn!(
            "Ignoring invalid MIDI preferences at {}: {error}",
            path.display()
        );
        MidiPreferences::default()
    })
}

fn save_preferences(path: &Path, preferences: &MidiPreferences) -> Result<(), String> {
    let json = serde_json::to_string_pretty(preferences)
        .map_err(|error| format!("Failed to encode MIDI preferences: {error}"))?;
    crate::private_fs::write_atomic(path, json.as_bytes()).map_err(|error| {
        format!(
            "Failed to save MIDI preferences '{}': {error}",
            path.display()
        )
    })
}

fn port_entries(ports: &[MidiPortInfo]) -> Vec<MidiPortEntry> {
    ports
        .iter()
        .map(|port| MidiPortEntry {
            index: port.index,
            name: port.name.clone(),
            id: port.id.clone(),
        })
        .collect()
}

// ============================================================================
// Tauri commands
// ============================================================================

#[tauri::command]
/// List available MIDI inputs and outputs.
pub fn list_midi_ports() -> MidiPorts {
    match MidirBackend::new() {
        Ok(backend) => match (backend.list_inputs(), backend.list_outputs()) {
            (Ok(inputs), Ok(outputs)) => MidiPorts {
                supported: true,
                inputs: port_entries(&inputs),
                outputs: port_entries(&outputs),
                error: None,
            },
            (input_error, output_error) => MidiPorts {
                supported: false,
                inputs: Vec::new(),
                outputs: Vec::new(),
                error: Some(first_error(input_error, output_error)),
            },
        },
        Err(error) => MidiPorts {
            supported: false,
            inputs: Vec::new(),
            outputs: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn first_error(
    input: Result<Vec<MidiPortInfo>, crate::midi_bridge::MidiError>,
    output: Result<Vec<MidiPortInfo>, crate::midi_bridge::MidiError>,
) -> String {
    match (input, output) {
        (Err(error), _) => error.to_string(),
        (_, Err(error)) => error.to_string(),
        _ => "MIDI port enumeration failed".into(),
    }
}

#[tauri::command]
/// Read the live MIDI configuration.
pub fn get_midi_config(state: tauri::State<'_, AppState>) -> MidiConfigView {
    let mut inner = state.lock_inner();
    inner.midi_live.ensure_loaded();
    let audition_active = inner.audio.live_synth_active();
    let mut view = inner.midi_live.view();
    view.audition_active = audition_active;
    view
}

#[tauri::command]
/// Apply a live MIDI configuration.
pub fn set_midi_config(
    input: Option<String>,
    monitor: bool,
    mpe: Option<oc_synth::synth::MpeConfig>,
    state: tauri::State<'_, AppState>,
) -> MidiConfigView {
    let mut inner = state.lock_inner();
    let sink = LiveInputSink {
        route: inner.audio.live_input_route(),
        capture: Arc::clone(&inner.audio.midi_recorder.capture),
    };
    let mut view = inner
        .midi_live
        .configure_with_mpe(input, monitor, mpe, move |selector| {
            MidiRuntime::start_midir(selector, sink)
        });
    inner.audio.set_mpe_config(mpe);
    view.audition_active = inner.audio.live_synth_active();
    view
}

#[tauri::command]
/// Read live MIDI activity.
pub fn get_midi_activity(state: tauri::State<'_, AppState>) -> MidiActivityView {
    let mut inner = state.lock_inner();
    let _ = inner.audio.reap_live_synth_failure();
    let audition_active = inner.audio.live_synth_active();
    let mut activity = inner.midi_live.activity();
    activity.audition_active = audition_active;
    activity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_bridge::{FakeBackend, MidiPortInfo, PortDirection};
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<LiveEvent>>>);

    impl LiveSink for RecordingSink {
        fn route(&mut self, event: LiveEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("oc-midi-live-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(PREFERENCES_FILE)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for(mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn port_entries_preserve_index_name_and_id() {
        let ports = vec![
            MidiPortInfo::new(PortDirection::Input, 0, "Keys", "in-0"),
            MidiPortInfo::new(PortDirection::Input, 3, "Pads", "in-3"),
        ];
        assert_eq!(
            port_entries(&ports),
            vec![
                MidiPortEntry {
                    index: 0,
                    name: "Keys".into(),
                    id: "in-0".into(),
                },
                MidiPortEntry {
                    index: 3,
                    name: "Pads".into(),
                    id: "in-3".into(),
                },
            ]
        );
    }

    #[test]
    fn preferences_round_trip_and_missing_file_uses_defaults() {
        let dir = TempDir::new("prefs");
        let path = dir.file();
        assert_eq!(load_preferences(&path), MidiPreferences::default());

        let preferences = MidiPreferences {
            input: Some("Keys".into()),
            monitor: true,
            mpe: None,
        };
        save_preferences(&path, &preferences).unwrap();
        assert_eq!(load_preferences(&path), preferences);
        assert!(path.exists());
    }

    #[test]
    fn corrupt_preferences_fall_back_to_defaults() {
        let dir = TempDir::new("corrupt");
        let path = dir.file();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_preferences(&path), MidiPreferences::default());
    }

    #[test]
    fn selection_persists_without_opening_a_device() {
        let dir = TempDir::new("select");
        let path = dir.file();
        let mut live = MidiLive::new(Some(path.clone()));
        let view = live.configure_with_mpe(Some("Keys".into()), false, None, |_| {
            panic!("monitor is off; no device should open")
        });
        assert_eq!(view.input.as_deref(), Some("Keys"));
        assert!(!view.monitor);
        assert!(!view.active);
        assert!(view.error.is_none());

        let reloaded = load_preferences(&path);
        assert_eq!(
            reloaded,
            MidiPreferences {
                input: Some("Keys".into()),
                monitor: false,
                mpe: None,
            }
        );

        let mut fresh = MidiLive::new(Some(path));
        fresh.ensure_loaded();
        assert_eq!(fresh.view().input.as_deref(), Some("Keys"));
    }

    #[test]
    fn absent_backend_is_reported_without_panicking() {
        let mut live = MidiLive::new(None);
        let view = live.configure_with_mpe(Some("Keys".into()), true, None, |_| {
            Err("MIDI support is unavailable: no backend".into())
        });
        assert!(!view.active);
        assert!(view.monitor);
        assert!(!live.monitoring_active());
        assert_eq!(view.input.as_deref(), Some("Keys"));
        assert!(
            view.error
                .as_deref()
                .is_some_and(|error| error.contains("unavailable"))
        );
        // A later configure with monitor off clears the failure and stops cleanly.
        let view = live.configure_with_mpe(Some("Keys".into()), false, None, |_| unreachable!());
        assert!(view.error.is_none());
        assert!(!live.activity().running);
    }

    #[test]
    fn input_thread_routes_events_and_releases_after_stop_or_overflow() {
        for overflow in [false, true] {
            let mut backend = FakeBackend::new(&["Keys"], &[]);
            let pedal_down = LiveEvent::ControlChange {
                channel: 0,
                controller: 64,
                value: 127,
            };
            let note_on = LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            };
            backend.queue_input("Keys", pedal_down);
            backend.queue_input("Keys", note_on);
            if overflow {
                backend.queue_input_failure(
                    "Keys",
                    oc_engine::midi_io::MidiError::InputOverflow {
                        name: "Keys".into(),
                    },
                );
            }
            let sink = RecordingSink::default();
            let thread_sink = sink.clone();
            let mut live = MidiLive::new(None);
            let view = live.configure_with_mpe(Some("Keys".into()), true, None, move |selector| {
                MidiRuntime::start_with(selector, backend, thread_sink)
            });
            if overflow {
                assert!(wait_for(|| !live.active()));
                assert_eq!(
                    live.error().as_deref(),
                    Some("MIDI input buffer overflow on 'Keys'")
                );
            } else {
                assert!(view.active);
                assert!(live.monitoring_active());
                assert!(wait_for(|| sink.0.lock().unwrap().len() == 2));
            }
            let event = live.activity().last.unwrap();
            assert_eq!(event.kind, "note_on");
            assert_eq!(event.note, Some(60));
            assert_eq!(event.velocity, Some(100));
            assert_eq!(event.channel, 0);

            live.stop();
            assert!(!live.activity().running);
            assert!(!live.monitoring_active());
            // Stopping releases both the held note and the pressed pedal.
            assert_eq!(
                *sink.0.lock().unwrap(),
                vec![
                    pedal_down,
                    note_on,
                    LiveEvent::NoteOff {
                        channel: 0,
                        note: 60,
                        velocity: 0,
                    },
                    LiveEvent::ControlChange {
                        channel: 0,
                        controller: 64,
                        value: 0
                    },
                ]
            );
            live.stop();
        }
    }

    #[test]
    fn missing_port_is_reported_by_the_runtime() {
        let mut live = MidiLive::new(None);
        let view = live.configure_with_mpe(Some("Missing".into()), true, None, |selector| {
            MidiRuntime::start_with(
                selector,
                FakeBackend::new(&["Keys"], &[]),
                RecordingSink::default(),
            )
        });
        assert!(!view.active);
        assert!(
            view.error
                .as_deref()
                .is_some_and(|error| error.contains("Missing"))
        );
    }

    #[test]
    fn activity_ignores_clock_and_keeps_the_last_note() {
        let probe = MidiActivityProbe::new();
        probe.record(LiveEvent::Clock);
        assert!(probe.last_event().is_none());
        probe.record(LiveEvent::NoteOn {
            channel: 2,
            note: 64,
            velocity: 90,
        });
        probe.record(LiveEvent::ControlChange {
            channel: 1,
            controller: 1,
            value: 64,
        });
        let last = probe.last_event().unwrap();
        assert_eq!(last.kind, "control_change");
        assert_eq!(last.controller, Some(1));
        assert_eq!(last.value, Some(64));
        assert_eq!(probe.events(), 3);
    }
}
