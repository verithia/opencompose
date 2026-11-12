//! Keep a VST3 preview instance processing while its editor is open, and make
//! what the editor plays audible.
//!
//! The editor's instance is not part of the playback graph, so a worker drives
//! it with blocks at real time. Its output goes through a lock-free ring to a
//! dedicated output stream; while the transport plays that stream is muted so
//! the renderer's own instance is never doubled. Without an editor there is no
//! worker and no stream, so the main audio callback pays nothing.
//!
//! The worker owns the preview instance through its own lock and reads the
//! document only through [`AppState::try_lock_inner`], so a command holding the
//! document never delays a block and a plugin that blocks inside `process`
//! freezes the preview instead of the app.
//!
//! It also questions the editor window, and stops within one poll when the
//! operator closes it from its title bar — a close AppKit reports to nobody.
//!
//! The monitor stream is registered with the audio-IO owner, so a device change
//! rebuilds it with every other stream instead of leaving it on the old device.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oc_audio::audio_io::{AudioIo, ManagedStream, RenderSink, StreamSink, StreamSpec};
use oc_host::editor_monitor::{MonitorReader, MonitorWriter, channel};
use oc_host::vst3_instance::Vst3Instance;

use crate::state::{AppState, Vst3Preview};

const BLOCK_FRAMES: u32 = 512;
const BLOCK_SLACK_FRAMES: usize = BLOCK_FRAMES as usize;
/// How long the worker waits for the device before falling back to wall time.
const DEVICE_STALL: Duration = Duration::from_millis(750);
const WAIT_SLICE: Duration = Duration::from_millis(1);
const REPORT_EVERY: Duration = Duration::from_secs(5);
/// How often the worker asks whether the editor window is still there.
const WINDOW_POLL: Duration = Duration::from_millis(100);
/// How often the block picks up a tempo or device-rate change.
const CLOCK_REFRESH: Duration = Duration::from_secs(1);
/// Notes applied per block; a fast player cannot push more.
const NOTE_BATCH: usize = 64;

/// Note from our own keybed for the editor's preview instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewNote {
    On { note: u8, velocity: u8 },
    Off { note: u8 },
}

/// Sample rate and tempo the preview block reports to the plugin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewClock {
    pub sample_rate: u32,
    pub tempo: f64,
}

/// Pick up the document's sample rate and tempo without waiting for it.
///
/// A blocked document leaves the previous values in place: the preview keeps
/// playing on the clock it had, which is what its instance was loaded with.
pub fn refresh_preview_clock(state: &AppState, clock: &mut PreviewClock) {
    let Some(inner) = state.try_lock_inner() else {
        return;
    };
    clock.sample_rate = inner.audio.sample_rate.max(1);
    clock.tempo = inner.project.properties.tempo;
}

/// The instance a worker drives.
///
/// Production wraps a live VST3 instance; tests stand in a counter, so the lock
/// discipline and the window poll stay measurable without a plugin bundle.
pub(crate) trait PreviewInstance: Send {
    type Error: std::fmt::Display;
    fn push_note_on(&mut self, note: u8, velocity: f32);
    fn push_note_off(&mut self, note: u8);
    /// Render `frames` of stereo into `outputs[0]`, replacing its contents.
    fn process_block(
        &mut self,
        frames: u32,
        clock: PreviewClock,
        outputs: &mut [Vec<f32>],
    ) -> Result<(), Self::Error>;
}

impl PreviewInstance for Vst3Instance {
    type Error = oc_host::vst3_instance::Vst3ProcessError;
    fn push_note_on(&mut self, note: u8, velocity: f32) {
        self.note_on(note, velocity, 0);
    }

    fn push_note_off(&mut self, note: u8) {
        self.note_off(note, 0);
    }

    fn process_block(
        &mut self,
        frames: u32,
        clock: PreviewClock,
        outputs: &mut [Vec<f32>],
    ) -> Result<(), Self::Error> {
        // The editor's transport is stopped, so a tempo-synced plugin still
        // knows the project tempo without being told the transport runs.
        let mut transport = oc_host::PluginTransport::new(0, clock.sample_rate, clock.tempo);
        transport.playing = false;
        self.process_block_outputs_into(frames, Some(transport), &[0], outputs)
    }
}

/// Drives one preview instance with blocks, and nothing else.
///
/// Every access to the instance goes through its own lock; this struct holds no
/// reference to the document.
pub(crate) struct PreviewWorker<P: PreviewInstance> {
    instance: Arc<Mutex<P>>,
    clock: PreviewClock,
    outputs: Vec<Vec<f32>>,
}

impl<P: PreviewInstance> PreviewWorker<P> {
    pub(crate) fn new(instance: Arc<Mutex<P>>, clock: PreviewClock) -> Self {
        Self {
            instance,
            clock,
            outputs: vec![Vec::with_capacity((BLOCK_FRAMES * 2) as usize)],
        }
    }

    /// Apply the queued notes, then render one block. Takes only the preview
    /// lock, and only for the length of the block.
    pub(crate) fn step(&mut self, notes: &mut rtrb::Consumer<PreviewNote>) -> Result<(), P::Error> {
        let mut instance = self
            .instance
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for _ in 0..NOTE_BATCH {
            let Ok(command) = notes.pop() else { break };
            match command {
                PreviewNote::On { note, velocity } => {
                    instance.push_note_on(note, f32::from(velocity) / 127.0);
                }
                PreviewNote::Off { note } => instance.push_note_off(note),
            }
        }
        self.outputs[0].clear();
        instance.process_block(BLOCK_FRAMES, self.clock, &mut self.outputs)
    }

    pub(crate) fn output(&self) -> &[f32] {
        &self.outputs[0]
    }

    /// Replace the last block with silence after a failed one.
    pub(crate) fn silence(&mut self) {
        self.outputs[0].clear();
        self.outputs[0].resize((BLOCK_FRAMES * 2) as usize, 0.0);
    }
}

/// Everything one keep-alive worker needs, resolved while the document is at
/// hand so that the worker itself never asks for it.
pub struct KeepAliveSetup {
    pub io: Arc<AudioIo>,
    pub gate: Arc<AtomicBool>,
    pub preset: String,
    pub instance: Vst3Preview,
    pub clock: PreviewClock,
    /// Whether the editor window is gone; runs on the worker thread.
    pub window_gone: Box<dyn FnMut() -> bool + Send>,
    /// Read the clock from the document; must never wait for it.
    pub refresh_clock: Box<dyn FnMut(&mut PreviewClock) + Send>,
}

/// Stop flag and note queue of one open VST3 editor.
pub struct EditorKeepAlive {
    stop: Arc<AtomicBool>,
    window_closed: Arc<AtomicBool>,
    notes: Mutex<rtrb::Producer<PreviewNote>>,
}

impl EditorKeepAlive {
    /// Start feeding the preview instance blocks at real time.
    pub fn spawn(setup: KeepAliveSetup) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let window_closed = Arc::new(AtomicBool::new(false));
        let (notes, commands) = rtrb::RingBuffer::new(64);
        let worker_stop = Arc::clone(&stop);
        let worker_closed = Arc::clone(&window_closed);
        let spawned = std::thread::Builder::new()
            .name("vst3-editor-keepalive".to_string())
            .spawn(move || run(setup, worker_stop, worker_closed, commands));
        if let Err(error) = spawned {
            log::warn!("could not start the VST3 editor keep-alive: {error}");
        }
        Self {
            stop,
            window_closed,
            notes: Mutex::new(notes),
        }
    }

    /// Whether the operator closed the editor from its own title bar.
    pub fn window_closed(&self) -> bool {
        self.window_closed.load(Ordering::Acquire)
    }

    /// Play a note on the editor's instance; false when the queue is full.
    pub fn note_on(&self, note: u8, velocity: u8) -> bool {
        self.push(PreviewNote::On {
            note: note.min(127),
            velocity: velocity.clamp(1, 127),
        })
    }

    /// Release a note on the editor's instance.
    pub fn note_off(&self, note: u8) -> bool {
        self.push(PreviewNote::Off {
            note: note.min(127),
        })
    }

    fn push(&self, note: PreviewNote) -> bool {
        self.notes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(note)
            .is_ok()
    }

    /// A keep-alive whose window already reported itself closed.
    #[cfg(test)]
    pub(crate) fn closed_for_test() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            window_closed: Arc::new(AtomicBool::new(true)),
            notes: Mutex::new(rtrb::RingBuffer::new(8).0),
        }
    }
}

impl Drop for EditorKeepAlive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// One worker's loop body, apart from the thread so a test can drive it.
pub(crate) struct KeepAliveRun<P: PreviewInstance> {
    preset: String,
    preview: PreviewWorker<P>,
    monitor: Option<Arc<EditorMonitor>>,
    gate: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    notes: rtrb::Consumer<PreviewNote>,
    window_closed: Arc<AtomicBool>,
    /// Wall-clock cadence, used only when no device drains the ring.
    next_block: Instant,
    last_clock: Instant,
    last_report: Instant,
    last_published: u64,
}

impl<P: PreviewInstance> KeepAliveRun<P> {
    pub(crate) fn new(
        preset: String,
        preview: PreviewWorker<P>,
        monitor: Option<Arc<EditorMonitor>>,
        gate: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        notes: rtrb::Consumer<PreviewNote>,
        window_closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            preset,
            preview,
            monitor,
            gate,
            stop,
            notes,
            window_closed,
            next_block: Instant::now(),
            last_clock: Instant::now(),
            last_report: Instant::now(),
            last_published: 0,
        }
    }

    /// Whether the editor window is gone as far as this worker knows.
    #[cfg(test)]
    pub(crate) fn window_closed(&self) -> bool {
        self.window_closed.load(Ordering::Acquire)
    }

    /// One pass of the loop; false means the worker must stop.
    ///
    /// `window_gone` is asked at most once per block and must not block on the
    /// document either; `refresh_clock` is the worker's only document access.
    pub(crate) fn tick(
        &mut self,
        window_gone: &mut (impl FnMut() -> bool + ?Sized),
        refresh_clock: &mut (impl FnMut(&mut PreviewClock) + ?Sized),
    ) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        // A window the operator closed from its title bar looks open to the
        // host, so the editor would otherwise keep processing forever.
        if window_gone() {
            self.window_closed.store(true, Ordering::Release);
            return false;
        }

        if let Some(monitor) = self.monitor.as_deref()
            && monitor.is_open()
            && !monitor.dead()
            && !monitor.wait_for_room(&self.stop)
        {
            log::warn!("[editor-monitor] output stalled; keeping the plugin alive silently");
            monitor.mark_stalled();
        }

        if self.last_clock.elapsed() >= CLOCK_REFRESH {
            self.last_clock = Instant::now();
            refresh_clock(&mut self.preview.clock);
        }

        if let Err(error) = self.preview.step(&mut self.notes) {
            log::warn!("[editor-monitor] block failed: {error}");
            self.preview.silence();
        }

        let monitor_open = self
            .monitor
            .as_deref()
            .is_some_and(|monitor| monitor.is_open() && !monitor.dead());
        if monitor_open {
            let audible = self.gate.load(Ordering::Acquire);
            if let Some(monitor) = self.monitor.as_deref() {
                monitor.publish(self.preview.output(), audible);
            }
            self.report();
        } else {
            // The device could not keep up or is missing: keep the process
            // cadence (the editor must not show "audio processing disabled")
            // but throw the samples away, as the old keep-alive did.
            let frames = Duration::from_secs_f64(
                BLOCK_FRAMES as f64 / f64::from(self.preview.clock.sample_rate.max(1)),
            );
            self.next_block += frames;
            let now = Instant::now();
            if self.next_block > now {
                std::thread::sleep(self.next_block - now);
            } else if now - self.next_block > Duration::from_millis(500) {
                self.next_block = now;
            }
        }
        true
    }

    fn report(&mut self) {
        if self.last_report.elapsed() < REPORT_EVERY {
            return;
        }
        self.last_report = Instant::now();
        let Some(report) = self.monitor.as_deref().and_then(EditorMonitor::report) else {
            return;
        };
        let preset = &self.preset;
        oc_diag::diag_debug!(
            "plugin monitor preset={preset} queued={} underruns={} dropped={} published={} played={}",
            report.queued_frames,
            report.underruns,
            report.dropped,
            report.published_frames,
            report.played_frames
        );
        if report.published_frames == self.last_published {
            log::warn!("[editor-monitor] plugin produced no blocks");
        }
        self.last_published = report.published_frames;
    }
}

fn run(
    setup: KeepAliveSetup,
    stop: Arc<AtomicBool>,
    window_closed: Arc<AtomicBool>,
    notes: rtrb::Consumer<PreviewNote>,
) {
    let monitor = Arc::new(EditorMonitor::new(
        Arc::clone(&setup.io),
        Arc::clone(&setup.gate),
    ));
    setup
        .io
        .register_stream(Arc::clone(&monitor) as Arc<dyn ManagedStream>);
    if let Err(error) = monitor.open() {
        log::warn!("[editor-monitor] no output stream ({error}); the editor stays silent");
    }
    let preview = PreviewWorker::new(Arc::clone(&setup.instance), setup.clock);
    let mut worker = KeepAliveRun::new(
        setup.preset,
        preview,
        Some(monitor),
        Arc::clone(&setup.gate),
        stop,
        notes,
        window_closed,
    );
    let mut window_gone = setup.window_gone;
    let mut refresh_clock = setup.refresh_clock;
    while worker.tick(&mut *window_gone, &mut *refresh_clock) {}
}

/// Polls an editor window for a worker thread.
///
/// The AppKit query belongs to the main thread, so the check is dispatched
/// there: at most one is in flight, and a busy main thread only delays it.
#[cfg(target_os = "macos")]
pub struct WindowWatch {
    app: tauri::AppHandle,
    gui: oc_host::vst3_instance::Vst3GuiHandle,
    gone: Arc<AtomicBool>,
    in_flight: Arc<AtomicBool>,
    next_check: Instant,
}

#[cfg(target_os = "macos")]
impl WindowWatch {
    pub fn new(app: tauri::AppHandle, gui: oc_host::vst3_instance::Vst3GuiHandle) -> Self {
        Self {
            app,
            gui,
            gone: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicBool::new(false)),
            next_check: Instant::now() + WINDOW_POLL,
        }
    }

    /// Ask the main thread whether the window is gone, when a check is due.
    pub fn poll(&mut self) {
        if self.next_check.elapsed() < WINDOW_POLL {
            return;
        }
        self.next_check = Instant::now();
        if self.in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let gui = self.gui.clone();
        let gone = Arc::clone(&self.gone);
        let in_flight = Arc::clone(&self.in_flight);
        let dispatched = self.app.run_on_main_thread(move || {
            gone.store(gui.window_gone(), Ordering::Release);
            in_flight.store(false, Ordering::Release);
        });
        if dispatched.is_err() {
            self.in_flight.store(false, Ordering::Release);
        }
    }

    /// Whether the last dispatched check found the window gone.
    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }
}

/// The editor's monitor stream: a ring writer for the worker and the device
/// stream that drains it. Rebuilt by the audio-IO sweep.
pub(crate) struct EditorMonitor {
    io: Arc<AudioIo>,
    gate: Arc<AtomicBool>,
    state: Mutex<MonitorState>,
}

#[derive(Default)]
struct MonitorState {
    writer: Option<MonitorWriter>,
    sink: Option<StreamSink>,
    dead: Arc<AtomicBool>,
}

impl EditorMonitor {
    fn new(io: Arc<AudioIo>, gate: Arc<AtomicBool>) -> Self {
        Self {
            io,
            gate,
            state: Mutex::new(MonitorState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MonitorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn open(&self) -> Result<(), String> {
        self.open_with(&self.io.spec())
    }

    fn open_with(&self, spec: &StreamSpec) -> Result<(), String> {
        let (writer, reader) = channel(
            oc_host::editor_monitor::DEFAULT_CAPACITY_FRAMES,
            Arc::clone(&self.gate),
        );
        let dead = Arc::new(AtomicBool::new(false));
        let sink = self.io.backend().open_output(
            spec,
            Arc::new(MonitorSink {
                reader: UnsafeCell::new(reader),
                dead: Arc::clone(&dead),
                io: Arc::clone(&self.io),
            }),
        )?;
        let mut state = self.lock();
        state.writer = Some(writer);
        state.sink = Some(sink);
        state.dead = dead;
        Ok(())
    }

    fn dead(&self) -> bool {
        self.lock().dead.load(Ordering::Acquire)
    }

    /// Give up on the current stream but keep the slot open, so the next device
    /// switch rebuilds it instead of leaving the editor silent forever.
    fn mark_stalled(&self) {
        self.lock().dead.store(true, Ordering::Release);
    }

    /// Wait until the device has room for one more block. False when it stalled.
    fn wait_for_room(&self, stop: &AtomicBool) -> bool {
        let mut waiting_since: Option<Instant> = None;
        loop {
            if stop.load(Ordering::Acquire) || self.dead() {
                return false;
            }
            let Some((queued, target)) = self.queued_and_target() else {
                return false;
            };
            if queued + BLOCK_SLACK_FRAMES <= target {
                return true;
            }
            let waiting = *waiting_since.get_or_insert_with(Instant::now);
            if waiting.elapsed() > DEVICE_STALL {
                return false;
            }
            std::thread::sleep(WAIT_SLICE);
        }
    }

    fn queued_and_target(&self) -> Option<(usize, usize)> {
        let state = self.lock();
        let writer = state.writer.as_ref()?;
        let target = writer
            .target_frames(BLOCK_SLACK_FRAMES)
            .max(BLOCK_SLACK_FRAMES);
        Some((writer.queued_frames(), target))
    }

    fn publish(&self, block: &[f32], audible: bool) -> bool {
        let mut state = self.lock();
        match state.writer.as_mut() {
            Some(writer) => writer.publish(block, audible),
            None => false,
        }
    }

    fn report(&self) -> Option<oc_host::editor_monitor::MonitorReport> {
        self.lock().writer.as_ref().map(MonitorWriter::report)
    }
}

impl ManagedStream for EditorMonitor {
    fn role(&self) -> &'static str {
        "editor-monitor"
    }

    fn is_open(&self) -> bool {
        self.lock().sink.is_some()
    }

    fn reopen(&self, spec: &StreamSpec) -> Result<(), String> {
        self.open_with(spec)
    }
}

/// Device side of the monitor ring. One sink serves one device callback thread.
struct MonitorSink {
    reader: UnsafeCell<MonitorReader>,
    dead: Arc<AtomicBool>,
    io: Arc<AudioIo>,
}

// SAFETY: cpal calls one sink from one device callback thread, and a sink is
// created for exactly one stream open.
unsafe impl Sync for MonitorSink {}

impl RenderSink for MonitorSink {
    fn fill(&self, output: &mut [f32]) {
        // SAFETY: the backend owns this sink for one callback thread only.
        let reader = unsafe { &mut *self.reader.get() };
        reader.read(output);
    }

    fn failed(&self, error: String) {
        self.dead.store(true, Ordering::Release);
        log::error!("[editor-monitor] output error: {error}");
        self.io.note_stream_failure("editor-monitor", &error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A preview that counts its blocks, so a test can see whether one ran.
    struct CountingPreview {
        blocks: Arc<AtomicUsize>,
    }

    impl PreviewInstance for CountingPreview {
        type Error = std::convert::Infallible;
        fn push_note_on(&mut self, _note: u8, _velocity: f32) {}

        fn push_note_off(&mut self, _note: u8) {}

        fn process_block(
            &mut self,
            frames: u32,
            _clock: PreviewClock,
            outputs: &mut [Vec<f32>],
        ) -> Result<(), Self::Error> {
            self.blocks.fetch_add(1, Ordering::AcqRel);
            outputs[0].resize((frames * 2) as usize, 0.0);
            Ok(())
        }
    }

    /// A run with no device stream, so a test drives its blocks by hand.
    fn run_for<P: PreviewInstance>(
        preview: PreviewWorker<P>,
    ) -> (KeepAliveRun<P>, rtrb::Producer<PreviewNote>) {
        let (producer, notes) = rtrb::RingBuffer::new(8);
        let run = KeepAliveRun::new(
            "test".to_string(),
            preview,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            notes,
            Arc::new(AtomicBool::new(false)),
        );
        (run, producer)
    }

    #[test]
    fn a_preview_block_does_not_wait_for_the_document_lock() {
        let state = Arc::new(AppState::new());
        let instance = oc_host::vst3_instance::test_support::fake_instance_for_id(
            "test.fake.one_bus",
            BLOCK_FRAMES,
        )
        .expect("in-process fake VST3");
        let preview = PreviewWorker::new(
            Arc::new(Mutex::new(instance)),
            PreviewClock {
                sample_rate: 48_000,
                tempo: 120.0,
            },
        );
        let (mut run, _notes) = run_for(preview);

        // A command holding the document for two seconds: a save, a snapshot
        // read or an agent render all look like this.
        let holder = Arc::clone(&state);
        let held = Arc::new(AtomicBool::new(false));
        let entered = Arc::clone(&held);
        let locking = std::thread::spawn(move || {
            let _document = holder.lock_inner();
            entered.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_secs(2));
        });
        while !held.load(Ordering::Acquire) {
            std::thread::yield_now();
        }

        let start = Instant::now();
        for _ in 0..3 {
            assert!(run.tick(&mut || false, &mut |clock| {
                refresh_preview_clock(&state, clock)
            }));
        }
        let elapsed = start.elapsed();
        locking.join().expect("the lock holder finished");
        assert!(
            elapsed < Duration::from_millis(500),
            "three preview blocks took {elapsed:?} while the document was locked"
        );
    }

    #[test]
    fn the_window_poll_stops_the_worker_and_reports_the_editor_closed() {
        let blocks = Arc::new(AtomicUsize::new(0));
        let preview = PreviewWorker::new(
            Arc::new(Mutex::new(CountingPreview {
                blocks: Arc::clone(&blocks),
            })),
            PreviewClock {
                sample_rate: 48_000,
                tempo: 120.0,
            },
        );
        let (mut run, _notes) = run_for(preview);
        let mut polls = 0;
        let mut window_gone = || {
            polls += 1;
            polls > 2
        };
        assert!(run.tick(&mut window_gone, &mut |_| {}));
        assert!(run.tick(&mut window_gone, &mut |_| {}));
        assert!(
            !run.tick(&mut window_gone, &mut |_| {}),
            "the worker stops on the poll that finds the window gone"
        );
        assert!(run.window_closed(), "the editor state reports closed");
        assert_eq!(polls, 3, "the window is questioned once per block");
        assert_eq!(
            blocks.load(Ordering::Acquire),
            2,
            "no block runs after the window is gone"
        );
    }
}
