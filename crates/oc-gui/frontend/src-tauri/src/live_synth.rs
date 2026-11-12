//! Native synth audition. Patch preparation and stream ownership stay off the callback.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use oc_audio::audio_io::{RenderSink, RenderSource, StreamSpec};
use oc_dsl::ast::PresetDef;
use oc_engine::midi_io::{LiveEvent, LiveEventTarget};
use oc_synth::synth::{MpeConfig, OpenSynth, SynthPatch};

pub(crate) use oc_synth::synth::load_project_patch as patch;

static CALIBRATION_WORKER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
static WARM_CALIBRATION_GENERATION: AtomicU64 = AtomicU64::new(0);
const CALIBRATION_PENDING: u64 = f64::NAN.to_bits();
const CALIBRATION_RETIRED: u64 = u64::MAX;

pub(crate) async fn warm_keyboard_trim(preset: PresetDef) {
    use oc_engine::loudness::{can_prepare_in_parallel, ready_native_trim, trim_db};

    let generation = WARM_CALIBRATION_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    if !can_prepare_in_parallel(&preset) || ready_native_trim(&preset).is_some() {
        return;
    }
    let current = move || WARM_CALIBRATION_GENERATION.load(Ordering::Relaxed) == generation;
    let permit = loop {
        if !current() {
            return;
        }
        // Keyboard jobs already queued for the worker get its next slot.
        if let Ok(permit) = CALIBRATION_WORKER.try_acquire() {
            break permit;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        if current() {
            trim_db(&preset);
        }
    })
    .await;
}

#[derive(Default)]
pub(crate) struct KeyboardPatch {
    pub patch: SynthPatch,
    calibration: Option<Arc<AtomicU64>>,
}

impl From<SynthPatch> for KeyboardPatch {
    fn from(patch: SynthPatch) -> Self {
        Self {
            patch,
            calibration: None,
        }
    }
}

pub(crate) fn prepare_keyboard_patch(
    preset: &PresetDef,
    root: Option<&std::path::Path>,
) -> Result<KeyboardPatch, String> {
    use oc_engine::loudness::{can_prepare_in_parallel, ready_native_trim, trim_db};

    let mut prepared = KeyboardPatch::from(patch(&preset.params, root)?);
    if !can_prepare_in_parallel(preset) {
        return Ok(prepared);
    }
    if let Some(db) = ready_native_trim(preset) {
        prepared.patch.loudness_gain = 10.0_f64.powf(db / 20.0);
        return Ok(prepared);
    }
    let target = Arc::new(AtomicU64::new(CALIBRATION_PENDING));
    prepared.calibration = Some(Arc::clone(&target));
    let target = Arc::downgrade(&target);
    let preset = preset.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(permit) = CALIBRATION_WORKER.acquire().await else {
            return;
        };
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            if let Some(target) = target.upgrade() {
                let db = trim_db(&preset);
                let _ = target.compare_exchange(
                    CALIBRATION_PENDING,
                    db.to_bits(),
                    Ordering::Release,
                    Ordering::Relaxed,
                );
            }
        });
    });
    Ok(prepared)
}

pub(crate) enum Command {
    On(u8, u8),
    Off(u8),
    /// Live-input event routed from the MIDI thread.
    Live(LiveEvent),
    /// Live MPE zones from the MIDI preferences.
    Mpe(Option<MpeConfig>),
    Param {
        name: [u8; 64],
        len: u8,
        value: f64,
    },
}

impl Command {
    pub(crate) fn parameter(key: &str, value: f64) -> Option<Self> {
        if key.len() > 64 || key.is_empty() || !value.is_finite() {
            return None;
        }
        let mut name = [0; 64];
        name[..key.len()].copy_from_slice(key.as_bytes());
        Some(Self::Param {
            name,
            len: key.len() as u8,
            value,
        })
    }
}

struct Runtime {
    synth: OpenSynth,
    commands: rtrb::Consumer<Command>,
    authored_mpe: Option<MpeConfig>,
    applied_mpe: Option<MpeConfig>,
    pending_mpe: Option<Option<MpeConfig>>,
    calibration: Option<Arc<AtomicU64>>,
    calibration_settled: bool,
}

#[derive(Default)]
struct Activity {
    pending: AtomicUsize,
    sounding: AtomicBool,
    failed: AtomicBool,
    failure: Mutex<Option<String>>,
}

pub(crate) type Route = Arc<Mutex<Option<CommandProducer>>>;

pub(crate) struct CommandProducer {
    producer: rtrb::Producer<Command>,
    activity: Arc<Activity>,
    midi_notes: [u128; 16],
    midi_sustain: [bool; 16],
    midi_releases: usize,
}

impl CommandProducer {
    fn new(producer: rtrb::Producer<Command>, activity: Arc<Activity>) -> Self {
        Self {
            producer,
            activity,
            midi_notes: [0; 16],
            midi_sustain: [false; 16],
            midi_releases: 0,
        }
    }

    pub(crate) fn push(&mut self, command: Command) -> Result<(), ()> {
        if self.producer.slots() <= self.midi_releases {
            return Err(());
        }
        self.enqueue(command)
    }

    /// Every admitted external press keeps one slot for its eventual release.
    /// Both events use the same FIFO, so retriggers cannot overtake releases.
    pub(crate) fn push_midi(&mut self, event: LiveEvent) {
        let channel = match event {
            LiveEvent::NoteOn { channel, .. }
            | LiveEvent::NoteOff { channel, .. }
            | LiveEvent::ControlChange { channel, .. }
            | LiveEvent::PolyPressure { channel, .. }
            | LiveEvent::ProgramChange { channel, .. }
            | LiveEvent::ChannelPressure { channel, .. }
            | LiveEvent::PitchBend { channel, .. } => usize::from(channel.min(15)),
            LiveEvent::Clock | LiveEvent::Start | LiveEvent::Continue | LiveEvent::Stop => return,
        };
        let mut notes = self.midi_notes[channel];
        let mut sustain = self.midi_sustain[channel];
        let before = notes.count_ones() as usize + usize::from(sustain);
        match event {
            LiveEvent::NoteOn { note, velocity, .. } if velocity > 0 => {
                notes |= 1 << (note & 0x7f);
            }
            LiveEvent::NoteOn { note, .. } | LiveEvent::NoteOff { note, .. } => {
                let mask = 1 << (note & 0x7f);
                if notes & mask == 0 {
                    return;
                }
                notes &= !mask;
            }
            LiveEvent::ControlChange {
                controller: 64,
                value,
                ..
            } => {
                if value < 64 && !sustain {
                    return;
                }
                sustain = value >= 64;
            }
            LiveEvent::ControlChange {
                controller: 120, ..
            } => {
                notes = 0;
                sustain = false;
            }
            LiveEvent::ControlChange {
                controller: 121, ..
            } => sustain = false,
            LiveEvent::ControlChange {
                controller: 123, ..
            } => notes = 0,
            _ => {}
        }

        let after = notes.count_ones() as usize + usize::from(sustain);
        let reserved = self.midi_releases - before + after;
        // While the queue is busy, leave room for local keys and parameter
        // changes. A release may spend its own slot even without that headroom.
        let channel_release = matches!(
            event,
            LiveEvent::ControlChange {
                controller: 120 | 121 | 123,
                ..
            }
        );
        let local_headroom = if after < before || channel_release {
            0
        } else {
            (self.producer.buffer().capacity() / 4).min(32)
        };
        if self.producer.slots() > reserved + local_headroom
            && self.enqueue(Command::Live(event)).is_ok()
        {
            self.midi_notes[channel] = notes;
            self.midi_sustain[channel] = sustain;
            self.midi_releases = reserved;
        }
    }

    fn enqueue(&mut self, command: Command) -> Result<(), ()> {
        if self.failed() {
            return Err(());
        }
        self.activity.pending.fetch_add(1, Ordering::AcqRel);
        let result = self.producer.push(command).map_err(|_| ());
        if result.is_err() {
            self.activity.pending.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }

    pub(crate) fn sounding_or_pending(&self) -> bool {
        !self.failed()
            && (self.activity.pending.load(Ordering::Acquire) != 0
                || self.activity.sounding.load(Ordering::Acquire))
    }

    pub(crate) fn failed(&self) -> bool {
        self.activity.failed.load(Ordering::Acquire)
    }

    pub(crate) fn take_failure(&self) -> Option<String> {
        self.activity
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    #[cfg(test)]
    pub(crate) fn test_producer(producer: rtrb::Producer<Command>) -> Self {
        Self::new(producer, Arc::new(Activity::default()))
    }
}

impl Runtime {
    fn new(
        patch: SynthPatch,
        tempo: f64,
        preference: Option<MpeConfig>,
        commands: rtrb::Consumer<Command>,
    ) -> Self {
        let authored_mpe = patch.mpe;
        let applied_mpe = preference.or(authored_mpe);
        let mut synth = OpenSynth::new_with_tempo(patch, tempo);
        if applied_mpe != authored_mpe {
            synth.set_mpe_config(applied_mpe);
        }
        Self {
            synth,
            commands,
            authored_mpe,
            applied_mpe,
            pending_mpe: None,
            calibration: None,
            calibration_settled: false,
        }
    }

    fn apply_pending_mpe(&mut self) {
        // A zone change clears master-to-member note mappings. Keep those
        // mappings until all held notes and releases have finished.
        if let Some(config) = self.pending_mpe
            && self.synth.is_idle()
        {
            self.pending_mpe = None;
            self.synth.set_mpe_config(config);
            self.applied_mpe = config;
        }
    }

    fn apply_pending_calibration(&mut self) {
        if !self.calibration_settled
            && let Some(calibration) = &self.calibration
        {
            let db = f64::from_bits(calibration.load(Ordering::Acquire));
            if db.is_finite() && self.synth.is_idle() {
                self.synth.set_loudness_trim_db(db);
                self.calibration_settled = true;
            }
        }
    }

    fn render(&mut self, output: &mut [f32], rate: u32, activity: &Activity) {
        // Bound control work even if the producer keeps submitting commands.
        let mut handled = 0;
        for _ in 0..256 {
            let Ok(command) = self.commands.pop() else {
                break;
            };
            handled += 1;
            match command {
                Command::On(note, velocity) => {
                    self.apply_pending_mpe();
                    self.apply_pending_calibration();
                    self.synth.note_on(0, note, velocity);
                }
                Command::Off(note) => self.synth.note_off(0, note),
                Command::Live(event) => {
                    self.apply_pending_mpe();
                    self.apply_pending_calibration();
                    self.synth.apply_live_event(event);
                }
                Command::Mpe(preference) => {
                    let config = preference.or(self.authored_mpe);
                    self.pending_mpe = (config != self.applied_mpe).then_some(config);
                }
                Command::Param { name, len, value } => {
                    if let Ok(name) = std::str::from_utf8(&name[..usize::from(len)]) {
                        // A pending result belongs to the original patch.
                        // Keep the current gain while editing this stream.
                        if !self.calibration_settled
                            && let Some(calibration) = &self.calibration
                        {
                            calibration.store(CALIBRATION_RETIRED, Ordering::Release);
                        }
                        self.calibration_settled = true;
                        self.synth.set_param(name, value);
                    }
                }
            }
        }
        self.apply_pending_mpe();
        self.apply_pending_calibration();
        self.synth.render_interleaved(output, rate);
        // The block that ends a release can still contain audible samples.
        let last_block_audible = || {
            (handled != 0 || activity.sounding.load(Ordering::Relaxed))
                && output.iter().any(|sample| sample.abs() > 1e-7)
        };
        activity.sounding.store(
            !self.synth.is_idle() || last_block_audible(),
            Ordering::Release,
        );
        // A reader that sees the queue clear must also see the synth state
        // resulting from those commands.
        if handled != 0 {
            activity.pending.fetch_sub(handled, Ordering::AcqRel);
        }
    }
}

/// Frames, peak and validity of one audition stream; used by the ignored
/// native harness, which is the only place a real device is opened.
#[derive(Default)]
pub(crate) struct Observation {
    frames: AtomicU64,
    peak: AtomicU32,
    invalid: AtomicBool,
}

/// The keyboard/live-MIDI instrument, rebuildable for a new device.
///
/// Rebuilding resets the synth, so a note held across a device switch is cut;
/// the next event starts the new stream's voice.
pub(crate) struct LiveSource {
    patch: KeyboardPatch,
    tempo: f64,
    mpe: Option<MpeConfig>,
    route: Route,
    observation: Option<std::sync::Arc<Observation>>,
}

impl LiveSource {
    pub(crate) fn new(
        patch: KeyboardPatch,
        tempo: f64,
        mpe: Option<MpeConfig>,
        route: Route,
    ) -> Self {
        Self {
            patch,
            tempo,
            mpe,
            route,
            observation: None,
        }
    }
}

impl RenderSource for LiveSource {
    fn create(&self, spec: &StreamSpec) -> std::sync::Arc<dyn RenderSink> {
        let (producer, commands) = rtrb::RingBuffer::new(256);
        let activity = Arc::new(Activity::default());
        let producer = CommandProducer::new(producer, Arc::clone(&activity));
        let mut runtime = Runtime::new(self.patch.patch.clone(), self.tempo, self.mpe, commands);
        runtime.calibration = self.patch.calibration.clone();
        std::sync::Arc::new(CommandSink {
            runtime: std::cell::UnsafeCell::new(runtime),
            rate: spec.sample_rate,
            activity,
            route: Arc::clone(&self.route),
            unpublished: Mutex::new(Some(producer)),
            observation: self.observation.clone(),
        })
    }
}

struct CommandSink {
    runtime: std::cell::UnsafeCell<Runtime>,
    rate: u32,
    activity: Arc<Activity>,
    route: Route,
    unpublished: Mutex<Option<CommandProducer>>,
    observation: Option<std::sync::Arc<Observation>>,
}

// SAFETY: cpal calls one sink from one device callback thread, and a sink is
// created for exactly one stream open.
unsafe impl Sync for CommandSink {}

impl RenderSink for CommandSink {
    fn opened(&self) {
        let producer = self
            .unpublished
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(producer) = producer {
            *self.route.lock().unwrap_or_else(|error| error.into_inner()) = Some(producer);
        }
    }

    fn fill(&self, output: &mut [f32]) {
        if self.activity.failed.load(Ordering::Acquire) {
            output.fill(0.0);
            return;
        }
        // SAFETY: the backend owns this sink for one callback thread only.
        let runtime = unsafe { &mut *self.runtime.get() };
        runtime.render(output, self.rate, &self.activity);
        if let Some(probe) = &self.observation {
            let mut peak = 0.0f32;
            for sample in output.iter() {
                if !sample.is_finite() || sample.abs() >= 1.0 {
                    probe.invalid.store(true, Ordering::Relaxed);
                }
                peak = peak.max(sample.abs());
            }
            probe.peak.store(peak.to_bits(), Ordering::Relaxed);
            probe
                .frames
                .fetch_add((output.len() / 2) as u64, Ordering::Relaxed);
        }
    }

    fn failed(&self, error: String) {
        let mut failure = self
            .activity
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.activity.failed.load(Ordering::Relaxed) {
            *failure = Some(error);
            self.activity.failed.store(true, Ordering::Release);
        }
    }
}

/// Open one audition stream on `io`'s current format; returns the command
/// producer of the stream that was just created. The ignored native harness
/// uses this; the application opens through [`LiveSource`] instead.
#[cfg(test)]
pub(crate) fn open_with_mpe(
    io: &oc_audio::audio_io::AudioIo,
    tempo: f64,
    patch: SynthPatch,
    mpe: Option<MpeConfig>,
    observation: Option<std::sync::Arc<Observation>>,
) -> Result<(oc_audio::audio_io::StreamSink, CommandProducer), String> {
    let route = std::sync::Arc::new(Mutex::new(None));
    let mut source = LiveSource::new(patch.into(), tempo, mpe, std::sync::Arc::clone(&route));
    source.observation = observation;
    let sink = io.open_stream(std::sync::Arc::new(source))?;
    let producer = route
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .ok_or("the audition stream installed no command queue")?;
    Ok((sink, producer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "opens the default audio device for a quiet sub-second audition"]
    fn native_device_audition_and_release() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering::Relaxed;
        use std::time::Duration;

        use oc_audio::audio_io::{AudioIo, AudioSelection, CpalBackend};

        for rate in [44_100, 48_000] {
            let io = AudioIo::new(Arc::new(CpalBackend::new()), None);
            io.apply(&AudioSelection {
                sample_rate: Some(rate),
                ..AudioSelection::default()
            });
            let mut patch = SynthPatch::default();
            patch.set_param("output.gain", 0.001);
            patch.set_param("amp.release", 0.005);
            let probe = Arc::new(Observation::default());
            let (stream, mut commands) =
                open_with_mpe(&io, 120.0, patch, None, Some(probe.clone())).unwrap();
            commands.push(Command::On(60, 64)).ok().unwrap();
            std::thread::sleep(Duration::from_millis(250));
            let sounding_frames = probe.frames.load(Relaxed);
            let sounding_peak = f32::from_bits(probe.peak.load(Relaxed));
            assert!(sounding_frames > 0);
            assert!(sounding_peak > 1e-7 && sounding_peak < 0.01);
            commands.push(Command::Off(60)).ok().unwrap();
            std::thread::sleep(Duration::from_millis(250));
            assert!(probe.frames.load(Relaxed) > sounding_frames);
            assert!(f32::from_bits(probe.peak.load(Relaxed)) < 1e-6);
            assert!(!probe.invalid.load(Relaxed));
            eprintln!(
                "device_rate={rate}, sounding_frames={sounding_frames}, sounding_peak={sounding_peak}, release_frames={}",
                probe.frames.load(Relaxed)
            );
            drop(stream);
        }
    }

    #[test]
    fn missing_resources_fail_instead_of_using_default_sound() {
        use oc_dsl::ast::ParamValue;
        for key in ["wavetable.file", "open_synth_state_file"] {
            let params = vec![(
                key.into(),
                ParamValue::String("missing-resource.wav".into()),
            )];
            assert!(patch(&params, None).is_err());
            assert!(
                patch(
                    &params,
                    Some(std::path::Path::new("/nonexistent-audition-fixture"))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn parameter_messages_are_bounded_and_finite() {
        assert!(Command::parameter("", 1.0).is_none());
        assert!(Command::parameter(&"x".repeat(65), 1.0).is_none());
        assert!(Command::parameter("output.gain", f64::NAN).is_none());
        assert!(Command::parameter("output.gain", f64::INFINITY).is_none());
        assert!(Command::parameter("output.gain", 0.0).is_some());
    }

    #[test]
    fn queued_notes_and_release_audio_reserve_the_live_synth_until_idle() {
        let mut patch = SynthPatch::default();
        patch.set_param("amp.release", 0.2);
        let route: Route = Arc::new(Mutex::new(None));
        let source = LiveSource::new(patch.into(), 120.0, None, Arc::clone(&route));
        let sink = source.create(&StreamSpec::new(None, 48_000, None));
        sink.opened();
        let mut producer = route.lock().unwrap().take().unwrap();
        let mut samples = [0.0; 1024];
        sink.fill(&mut samples);
        assert!(!producer.sounding_or_pending());
        producer.push(Command::On(60, 100)).ok().unwrap();
        assert!(producer.sounding_or_pending());
        for _ in 0..8 {
            sink.fill(&mut samples);
        }
        producer.push(Command::Off(60)).ok().unwrap();
        sink.fill(&mut samples);
        assert_eq!(producer.activity.pending.load(Ordering::Acquire), 0);
        assert!(producer.sounding_or_pending());
        assert!(samples.iter().any(|sample| sample.abs() > 1e-5));
        for _ in 0..300 {
            sink.fill(&mut samples);
            if !producer.sounding_or_pending() {
                break;
            }
        }
        assert!(!producer.sounding_or_pending());
        assert!(samples.iter().all(|sample| sample.abs() <= 1e-7));
    }

    #[test]
    fn midi_releases_keep_their_fifo_slots_under_queue_pressure() {
        let (queue, mut consumer) = rtrb::RingBuffer::new(12);
        let mut producer = CommandProducer::test_producer(queue);
        let press = LiveEvent::NoteOn {
            channel: 3,
            note: 60,
            velocity: 90,
        };
        let release = LiveEvent::NoteOn {
            channel: 3,
            note: 60,
            velocity: 0,
        };
        let down = LiveEvent::ControlChange {
            channel: 3,
            controller: 64,
            value: 127,
        };
        let up = LiveEvent::ControlChange {
            channel: 3,
            controller: 64,
            value: 0,
        };
        let pressure = LiveEvent::ChannelPressure {
            channel: 3,
            pressure: 64,
        };
        producer.push_midi(press);
        producer.push_midi(down);
        for _ in 0..32 {
            producer.push_midi(pressure);
        }
        assert_eq!(producer.producer.slots(), 5);
        for note in 70..73 {
            producer.push(Command::On(note, 100)).unwrap();
        }
        assert!(producer.push(Command::On(73, 100)).is_err());
        producer.push_midi(release);
        producer.push_midi(up);
        producer.push_midi(press); // No room for the press and its future release.
        assert_eq!(producer.midi_releases, 0);

        let mut external = Vec::new();
        let mut local = Vec::new();
        while let Ok(command) = consumer.pop() {
            match command {
                Command::Live(event) => external.push(event),
                Command::On(note, _) => local.push(note),
                _ => unreachable!(),
            }
        }
        assert_eq!(&external[..2], &[press, down]);
        assert_eq!(&external[external.len() - 2..], &[release, up]);
        assert_eq!(local, [70, 71, 72]);

        // The ignored retrigger did not create a note to release.
        producer.push_midi(release);
        assert!(consumer.pop().is_err());
        producer.push_midi(press);
        producer.push_midi(release);
        assert!(matches!(consumer.pop(), Ok(Command::Live(event)) if event == press));
        assert!(matches!(consumer.pop(), Ok(Command::Live(event)) if event == release));
    }

    #[test]
    fn midi_controller_flood_does_not_leave_a_note_or_pedal_held() {
        let mut patch = SynthPatch::default();
        patch.set_param("amp.release", 0.001);
        let route: Route = Arc::new(Mutex::new(None));
        let source = LiveSource::new(patch.into(), 120.0, None, Arc::clone(&route));
        let sink = source.create(&StreamSpec::new(None, 48_000, None));
        sink.opened();
        let mut producer = route.lock().unwrap().take().unwrap();
        let mut samples = [0.0; 1024];

        producer.push(Command::On(72, 100)).unwrap();
        producer.push_midi(LiveEvent::NoteOn {
            channel: 1,
            note: 60,
            velocity: 90,
        });
        producer.push_midi(LiveEvent::ControlChange {
            channel: 1,
            controller: 64,
            value: 127,
        });
        sink.fill(&mut samples);
        for _ in 0..512 {
            producer.push_midi(LiveEvent::ChannelPressure {
                channel: 1,
                pressure: 64,
            });
        }
        producer.push_midi(LiveEvent::NoteOff {
            channel: 1,
            note: 60,
            velocity: 0,
        });
        producer.push_midi(LiveEvent::ControlChange {
            channel: 1,
            controller: 64,
            value: 0,
        });
        sink.fill(&mut samples);
        assert!(
            producer.sounding_or_pending(),
            "the local key still owns a note"
        );
        producer.push(Command::Off(72)).unwrap();
        for _ in 0..32 {
            sink.fill(&mut samples);
            if !producer.sounding_or_pending() {
                break;
            }
        }
        assert!(
            !producer.sounding_or_pending(),
            "all admitted releases reached the synth"
        );
    }

    #[test]
    fn clearing_mpe_waits_for_release_and_restores_authored_routing() {
        use oc_synth::synth::MpeMemberRange;

        let lower_zone = |member| MpeConfig {
            lower: Some(MpeMemberRange::new(member, member)),
            ..MpeConfig::default()
        };
        let override_mpe = lower_zone(2);
        for (authored_mpe, next_channel) in [(None, 0), (Some(lower_zone(3)), 2)] {
            let mut patch = SynthPatch::default();
            patch.set_param("amp.release", 0.001);
            patch.mpe = authored_mpe;
            let (producer, commands) = rtrb::RingBuffer::new(256);
            let mut producer = CommandProducer::test_producer(producer);
            let mut runtime = Runtime::new(patch, 120.0, Some(override_mpe), commands);
            let mut samples = [0.0; 1024];

            producer.push(Command::On(60, 100)).unwrap();
            runtime.render(&mut samples, 48_000, &producer.activity);
            assert!(samples.iter().any(|sample| sample.abs() > 1e-5));

            producer.push(Command::Mpe(None)).unwrap();
            runtime.render(&mut samples, 48_000, &producer.activity);
            assert_eq!(runtime.applied_mpe, Some(override_mpe));
            assert_eq!(runtime.pending_mpe, Some(authored_mpe));

            producer.push(Command::Mpe(Some(override_mpe))).unwrap();
            runtime.render(&mut samples, 48_000, &producer.activity);
            assert_eq!(runtime.pending_mpe, None);
            producer.push(Command::Mpe(None)).unwrap();
            producer.push(Command::Off(60)).unwrap();
            for _ in 0..32 {
                runtime.render(&mut samples, 48_000, &producer.activity);
                if runtime.synth.is_idle() {
                    break;
                }
            }
            assert!(runtime.synth.is_idle(), "the held note must release");
            assert_eq!(runtime.applied_mpe, Some(override_mpe));

            producer.push(Command::On(62, 100)).unwrap();
            runtime.render(&mut samples, 48_000, &producer.activity);
            assert_eq!(runtime.applied_mpe, authored_mpe);
            assert!(samples.iter().any(|sample| sample.abs() > 1e-5));
            producer
                .push(Command::Live(LiveEvent::ControlChange {
                    channel: next_channel,
                    controller: 120,
                    value: 0,
                }))
                .unwrap();
            runtime.render(&mut samples, 48_000, &producer.activity);
            assert!(
                runtime.synth.is_idle(),
                "the next note uses authored routing"
            );
        }
    }

    #[test]
    fn audition_matches_engine_and_releases_without_device() {
        let mut patch = SynthPatch::default();
        patch.set_param("amp.release", 0.001);
        let mut reference = OpenSynth::new_with_tempo(patch.clone(), 130.0);
        let (producer, commands) = rtrb::RingBuffer::new(256);
        let mut producer = CommandProducer::test_producer(producer);
        let mut runtime = Runtime::new(patch, 130.0, None, commands);
        assert!(!producer.sounding_or_pending());
        producer.push(Command::On(60, 100)).ok().unwrap();
        assert!(producer.sounding_or_pending());
        reference.note_on(0, 60, 100);
        let mut actual = [0.0; 4096];
        let mut expected = [0.0; 4096];
        runtime.render(&mut actual, 48000, &producer.activity);
        assert!(producer.sounding_or_pending());
        reference.render_interleaved(&mut expected, 48000);
        assert_eq!(actual, expected);
        assert!(actual.iter().all(|x| x.is_finite()));
        assert!(actual.iter().any(|x| x.abs() > 0.001));
        for (key, value) in [
            ("filter.cutoff", 500.0),
            ("osc.b.semitones", 0.07),
            ("output.gain", 0.1),
        ] {
            producer
                .push(Command::parameter(key, value).unwrap())
                .ok()
                .unwrap();
            reference.set_param(key, value);
            runtime.render(&mut actual, 48000, &producer.activity);
            reference.render_interleaved(&mut expected, 48000);
            assert_eq!(actual, expected);
            assert!(actual.iter().all(|x| x.is_finite()));
        }
        producer.push(Command::Off(60)).ok().unwrap();
        reference.note_off(0, 60);
        for _ in 0..8 {
            runtime.render(&mut actual, 48000, &producer.activity);
            reference.render_interleaved(&mut expected, 48000);
            assert_eq!(actual, expected);
        }
        assert!(actual.iter().all(|x| x.abs() < 1e-6));
        assert!(!producer.sounding_or_pending());
    }

    #[test]
    fn keyboard_calibration_is_prepared_in_the_background_and_reused() {
        use std::time::{Duration, Instant};

        let project = oc_dsl::parse(
            "@project\n@preset keys\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.2174, output.gain: 0.0417}\n",
        ).unwrap();
        let preset = &project.presets[0];
        let prepared = prepare_keyboard_patch(preset, None).unwrap();
        assert_eq!(prepared.patch.loudness_gain, 1.0);
        let pending = prepared.calibration.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let db = loop {
            let db = f64::from_bits(pending.load(Ordering::Acquire));
            if db.is_finite() {
                break db;
            }
            assert!(Instant::now() < deadline, "keyboard trim did not finish");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_ne!(db, 0.0);
        assert_eq!(db, oc_engine::loudness::trim_db(preset));
        let prepared = prepare_keyboard_patch(preset, None).unwrap();
        assert_eq!(prepared.patch.loudness_gain, 10.0_f64.powf(db / 20.0));
        assert!(prepared.calibration.is_none());
    }

    #[test]
    fn keyboard_prewarm_replaces_queued_preset_selection() {
        use oc_engine::loudness::ready_native_trim;

        let project = oc_dsl::parse(
            "@project\n@preset first\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.0712}\n@preset latest\n  extends: first\n  params: {output.gain: 0.0453}\n",
        )
        .unwrap();
        let first = oc_dsl::preset::resolve_preset(&project.presets, "first").unwrap();
        let latest = oc_dsl::preset::resolve_preset(&project.presets, "latest").unwrap();
        tauri::async_runtime::block_on(async {
            let permit = CALIBRATION_WORKER.acquire().await.unwrap();
            let first_job = warm_keyboard_trim(first.clone());
            let latest_job = warm_keyboard_trim(latest.clone());
            tokio::pin!(first_job, latest_job);
            // Poll both requests while the worker is occupied, in editor order.
            tokio::select! {
                biased;
                _ = &mut first_job => panic!("first warm request did not wait"),
                _ = std::future::ready(()) => {}
            }
            tokio::select! {
                biased;
                _ = &mut latest_job => panic!("latest warm request did not wait"),
                _ = std::future::ready(()) => {}
            }
            drop(permit);
            tokio::join!(first_job, latest_job);
        });
        assert!(ready_native_trim(&first).is_none());
        let trim = ready_native_trim(&latest).expect("current selection is calibrated");
        let keyboard = prepare_keyboard_patch(&latest, None).unwrap();
        assert!(keyboard.calibration.is_none());
        assert_eq!(keyboard.patch.loudness_gain, 10.0_f64.powf(trim / 20.0));
    }

    #[test]
    fn keyboard_calibration_waits_for_release_and_discards_edited_snapshots() {
        let mut patch = SynthPatch::default();
        patch.set_param("amp.release", 0.001);
        patch.gain = 0.1;
        let mut reference = OpenSynth::new_with_tempo(patch.clone(), 120.0);
        let (producer, commands) = rtrb::RingBuffer::new(16);
        let mut producer = CommandProducer::test_producer(producer);
        let mut runtime = Runtime::new(patch, 120.0, None, commands);
        let calibration = Arc::new(AtomicU64::new(CALIBRATION_PENDING));
        runtime.calibration = Some(Arc::clone(&calibration));
        let mut actual = [0.0; 1024];
        let mut expected = [0.0; 1024];

        producer.push(Command::On(60, 100)).unwrap();
        reference.note_on(0, 60, 100);
        runtime.render(&mut actual, 48_000, &producer.activity);
        reference.render_interleaved(&mut expected, 48_000);
        assert_eq!(actual, expected);
        calibration.store((-6.0_f64).to_bits(), Ordering::Release);
        runtime.render(&mut actual, 48_000, &producer.activity);
        reference.render_interleaved(&mut expected, 48_000);
        assert_eq!(actual, expected, "a held note must keep its previous gain");

        producer.push(Command::Off(60)).unwrap();
        reference.note_off(0, 60);
        for _ in 0..64 {
            runtime.render(&mut actual, 48_000, &producer.activity);
            reference.render_interleaved(&mut expected, 48_000);
            assert_eq!(actual, expected, "the release must keep its previous gain");
            if runtime.synth.is_idle() {
                break;
            }
        }
        assert!(runtime.synth.is_idle());
        producer.push(Command::On(62, 100)).unwrap();
        reference.note_on(0, 62, 100);
        runtime.render(&mut actual, 48_000, &producer.activity);
        reference.render_interleaved(&mut expected, 48_000);
        let factor = 10.0_f32.powf(-6.0 / 20.0);
        assert!(expected.iter().any(|sample| sample.abs() > 1e-4));
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(trimmed, dry)| (trimmed - dry * factor).abs() < 1e-6)
        );
        assert!(runtime.calibration_settled);

        runtime.calibration_settled = false;
        calibration.store(CALIBRATION_PENDING, Ordering::Release);
        producer
            .push(Command::parameter("output.gain", 0.5).unwrap())
            .unwrap();
        runtime.render(&mut actual, 48_000, &producer.activity);
        assert_eq!(calibration.load(Ordering::Acquire), CALIBRATION_RETIRED);
    }
}
