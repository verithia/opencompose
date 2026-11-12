//! Audio capture: one input stream, a lock-free monitor path to the output
//! callback, a peak meter, and the ring a take is recorded from.
//!
//! Two lock-free single-producer/single-consumer rings carry capture frames:
//! the monitor ring is read by the output callback, the record ring by the
//! thread that pumps a take. The capture callback only pushes and publishes the
//! peak; it never allocates, locks or blocks.
//!
//! The monitor ring carries stereo frames at the capture clock. The output
//! callback converts them to the output clock without touching the take.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use oc_audio::audio_io::{
    AudioIo, InputSink, InputSpec, ManagedStream, RenderSink, RenderSource, StreamSink, StreamSpec,
};

use crate::audio_slot::{CallbackPermit, CallbackRoute};

/// Monitor backlog: 8192 frames is ~0.19 s at 44.1 kHz, far more
/// than the one callback period the ring normally holds.
const MONITOR_FRAMES: usize = 8192;
const CLOCK_SCALE: u64 = 1_000_000;
const MAX_CLOCK_CORRECTION_PPM: i64 = 2_500;

/// Record backlog. The take is pumped from the transport poll, so this bounds
/// how long the poll may stall before frames are dropped: ~2 s of stereo.
const RECORD_SLOTS: usize = 96_000;

#[derive(Clone, Copy)]
struct MonitorFrame {
    stereo: [f32; 2],
    epoch: u64,
    stream: u64,
    sample_rate: u32,
}

/// Correct small clock differences from the queue's low-water mark. A single
/// callback can see either side of an input block arrival, so adjust over a
/// quarter second instead of changing pitch at each arrival.
#[derive(Default)]
struct MonitorClock {
    elapsed: u64,
    low_reserve: Option<usize>,
    correction_ppm: i64,
}

impl MonitorClock {
    fn step(
        &mut self,
        input_rate: u64,
        output_rate: u64,
        input_period: usize,
        frames: usize,
        queued: usize,
    ) -> u64 {
        let needed = (frames as u64 * input_rate).div_ceil(output_rate) as usize;
        let reserve = queued.saturating_sub(needed);
        let low = self
            .low_reserve
            .map_or(reserve, |previous| previous.min(reserve));
        self.elapsed += frames as u64;
        if self.elapsed >= (output_rate / 4).max(1) {
            // Keep one capture period plus 5 ms in reserve. Almost matching
            // periods can look aligned for several seconds before an input
            // block arrives just after its usual output callback.
            let target = (input_period + input_rate as usize / 200)
                .min(MONITOR_FRAMES.saturating_sub(needed) / 2);
            let error = low as i64 - target as i64;
            self.correction_ppm = (error * CLOCK_SCALE as i64 / (input_rate as i64 * 2))
                .clamp(-MAX_CLOCK_CORRECTION_PPM, MAX_CLOCK_CORRECTION_PPM);
            self.elapsed = 0;
            self.low_reserve = None;
        } else {
            self.low_reserve = Some(low);
        }
        input_rate * (CLOCK_SCALE as i64 + self.correction_ppm) as u64
    }
}

struct MonitorReader {
    queue: rtrb::Consumer<MonitorFrame>,
    epoch: u64,
    stream: u64,
    output_rate: u32,
    /// Fraction of a capture frame, in units of 1 / (output_rate * CLOCK_SCALE).
    phase: u64,
    clock: MonitorClock,
}

impl MonitorReader {
    fn mix(
        &mut self,
        out: &mut [f32],
        gain: f32,
        output_rate: u32,
        input_period: usize,
        epoch: u64,
        stream: u64,
    ) {
        if (epoch, stream, output_rate) != (self.epoch, self.stream, self.output_rate) {
            self.epoch = epoch;
            self.stream = stream;
            self.output_rate = output_rate;
            self.phase = 0;
            self.clock = MonitorClock::default();
        }
        let queued = self.queue.slots();
        let Ok(chunk) = self.queue.read_chunk(queued) else {
            return;
        };
        let (first, second) = chunk.as_slices();
        let at = |index: usize| {
            first
                .get(index)
                .or_else(|| second.get(index.saturating_sub(first.len())))
        };
        let frame =
            |index| at(index).filter(|frame| frame.epoch == epoch && frame.stream == stream);
        let mut index = 0;
        // Discard the old stream's tail, keeping new frames captured before
        // this output callback had a chance to see the generation change.
        while at(index).is_some_and(|frame| frame.epoch < epoch || frame.stream < stream) {
            index += 1;
        }
        let Some(current) = frame(index) else {
            chunk.commit(index);
            return;
        };
        let output_rate = u64::from(output_rate.max(1));
        let step = self.clock.step(
            u64::from(current.sample_rate.max(1)),
            output_rate,
            input_period,
            out.len() / 2,
            queued - index,
        );
        let output_rate = output_rate * CLOCK_SCALE;
        for output in out.chunks_exact_mut(2) {
            let Some(current) = frame(index) else {
                break;
            };
            let end = self.phase + step;
            let value = if step <= output_rate {
                if self.phase == 0 {
                    current.stereo
                } else {
                    let Some(next) = frame(index + 1) else {
                        break;
                    };
                    let fraction = self.phase as f32 / output_rate as f32;
                    std::array::from_fn(|ch| {
                        current.stereo[ch] + (next.stereo[ch] - current.stereo[ch]) * fraction
                    })
                }
            } else {
                // Average the covered capture interval. This attenuates
                // high-frequency content before reducing the sample count.
                let needed = end.div_ceil(output_rate) as usize;
                if frame(index + needed - 1).is_none() {
                    break;
                }
                let mut sum = [0.0; 2];
                for offset in 0..needed {
                    let start = offset as u64 * output_rate;
                    let width = (start + output_rate).min(end) - start.max(self.phase);
                    let Some(sample) = at(index + offset) else {
                        break;
                    };
                    let weight = width as f32 / step as f32;
                    for (ch, sum) in sum.iter_mut().enumerate() {
                        *sum += sample.stereo[ch] * weight;
                    }
                }
                sum
            };
            output[0] += value[0] * gain;
            output[1] += value[1] * gain;
            index += (end / output_rate) as usize;
            self.phase = end % output_rate;
        }
        chunk.commit(index);
    }
}

/// The capture path of one application instance.
pub struct InputPort {
    /// Capture side of the monitor ring; written by the capture callback only.
    monitor_tx: UnsafeCell<rtrb::Producer<MonitorFrame>>,
    /// Monitor side of the monitor ring; read by the output callback only.
    monitor_rx: UnsafeCell<MonitorReader>,
    monitor_reading: AtomicBool,
    /// Capture side of the record ring; written by the capture callback only.
    record_tx: UnsafeCell<Option<rtrb::Producer<f32>>>,
    /// Only non-realtime take readers acquire this mutex.
    record_rx: Mutex<Option<rtrb::Consumer<f32>>>,
    monitoring: AtomicBool,
    /// Linear gain as `f32` bits.
    monitor_gain: AtomicU32,
    /// Bumped when monitoring is switched on or the capture stream reopens; the
    /// output callback flushes the stale tail so a click from before the switch
    /// is never heard.
    monitor_epoch: AtomicU64,
    monitor_stream: AtomicU64,
    /// Frames the monitor path dropped because the ring was full.
    monitor_dropped: AtomicU64,
    /// Frames in the last capture callback: the capture side's own delay.
    block_frames: AtomicU64,
    /// Peak absolute sample since the last read, as `f32` bits.
    peak: AtomicU32,
    clipped: AtomicBool,
    record_channels: AtomicU32,
    record_writing: AtomicBool,
    record_layout_changed: AtomicBool,
    /// Capture frames dropped because the record ring was full.
    record_dropped: AtomicU64,
    /// The capture stream reported an error.
    failed: AtomicBool,
    error: Mutex<Option<String>>,
}

impl InputPort {
    /// Create the input monitor and recorder queues.
    pub fn new() -> Self {
        let (monitor_tx, queue) = rtrb::RingBuffer::new(MONITOR_FRAMES);
        let (record_tx, record_rx) = rtrb::RingBuffer::new(RECORD_SLOTS);
        Self {
            monitor_tx: UnsafeCell::new(monitor_tx),
            monitor_rx: UnsafeCell::new(MonitorReader {
                queue,
                epoch: 0,
                stream: 0,
                output_rate: 0,
                phase: 0,
                clock: MonitorClock::default(),
            }),
            record_tx: UnsafeCell::new(Some(record_tx)),
            record_rx: Mutex::new(Some(record_rx)),
            monitor_reading: AtomicBool::new(false),
            monitoring: AtomicBool::new(false),
            monitor_gain: AtomicU32::new(1.0f32.to_bits()),
            monitor_epoch: AtomicU64::new(0),
            monitor_stream: AtomicU64::new(0),
            monitor_dropped: AtomicU64::new(0),
            block_frames: AtomicU64::new(0),
            peak: AtomicU32::new(0.0f32.to_bits()),
            clipped: AtomicBool::new(false),
            record_channels: AtomicU32::new(0),
            record_writing: AtomicBool::new(false),
            record_layout_changed: AtomicBool::new(false),
            record_dropped: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    /// Turn monitoring on or off. Enabling flushes whatever the ring held.
    pub fn set_monitoring(&self, on: bool) {
        if on && !self.monitoring.load(Ordering::Acquire) {
            self.flush_monitor();
        }
        self.monitoring.store(on, Ordering::Release);
    }

    /// Frames the monitor path dropped because the output did not keep up,
    /// since the ring was last flushed.
    pub fn monitor_dropped_frames(&self) -> u64 {
        self.monitor_dropped.load(Ordering::Acquire)
    }

    /// Discard the monitor ring and its drop count before a reopened capture
    /// stream's frames are heard.
    pub(crate) fn flush_monitor(&self) {
        self.monitor_dropped.store(0, Ordering::Release);
        self.monitor_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Linear monitor gain from a dB setting.
    pub fn set_monitor_gain_db(&self, db: f64) {
        let gain = 10f64.powf(db / 20.0) as f32;
        self.monitor_gain.store(gain.to_bits(), Ordering::Release);
    }

    /// Peak since the last read, and whether anything clipped.
    pub fn take_level(&self) -> (f64, bool) {
        let peak = f32::from_bits(self.peak.swap(0.0f32.to_bits(), Ordering::AcqRel));
        let clipped = self.clipped.swap(false, Ordering::AcqRel);
        (f64::from(peak), clipped)
    }

    /// Frames in the last capture callback; 0 before the first one arrives.
    pub fn block_frames(&self) -> u64 {
        self.block_frames.load(Ordering::Acquire)
    }

    /// Start or stop filling the record ring.
    pub fn set_recording_channels(&self, channels: u16) {
        if channels != 0 {
            self.record_dropped.store(0, Ordering::Release);
            self.record_layout_changed.store(false, Ordering::Release);
        }
        self.record_channels
            .store(u32::from(channels), Ordering::SeqCst);
        if channels == 0 {
            // Stop drains the ring immediately afterwards. Wait for a block
            // that saw the old channel count to finish committing its frames.
            while self.record_writing.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
        }
    }

    pub(crate) fn interrupt_recording(&self, error: &str) {
        self.set_recording_channels(0);
        self.publish_error(error.to_owned());
    }

    fn publish_error(&self, error: String) {
        let mut pending = self.error.lock().unwrap_or_else(|e| e.into_inner());
        *pending = Some(error);
        self.failed.store(true, Ordering::Release);
    }

    /// Frames dropped because the take pump fell behind.
    pub fn dropped_frames(&self) -> u64 {
        self.record_dropped.load(Ordering::Acquire)
    }

    /// Append up to `max_floats` captured floats to `out`; returns how many.
    pub fn take_recorded(&self, out: &mut Vec<f32>, max_floats: usize) -> usize {
        let mut rx = self.record_rx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rx) = rx.as_mut() else { return 0 };
        let available = rx.slots().min(max_floats);
        if available == 0 {
            return 0;
        }
        let Ok(chunk) = rx.read_chunk(available) else {
            return 0;
        };
        let (first, second) = chunk.as_slices();
        out.extend_from_slice(first);
        out.extend_from_slice(second);
        let taken = first.len() + second.len();
        chunk.commit_all();
        taken
    }

    /// Take the capture stream's last error, if it reported one.
    pub fn take_error(&self) -> Option<String> {
        if self.record_layout_changed.swap(false, Ordering::AcqRel) {
            return Some("The input device changed its channel layout".into());
        }
        if !self.failed.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// Mix monitored capture into one interleaved stereo output block.
    fn mix(&self, out: &mut [f32], output_rate: u32) {
        if !self.monitoring.load(Ordering::Acquire) {
            return;
        }
        let Some(reader) = self.try_monitor_reader() else {
            return;
        };
        reader.reader.mix(
            out,
            f32::from_bits(self.monitor_gain.load(Ordering::Acquire)),
            output_rate,
            self.block_frames() as usize,
            self.monitor_epoch.load(Ordering::Acquire),
            self.monitor_stream.load(Ordering::Acquire),
        );
    }

    fn try_monitor_reader(&self) -> Option<MonitorRead<'_>> {
        self.monitor_reading
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(MonitorRead {
            busy: &self.monitor_reading,
            // SAFETY: the successful atomic acquisition owns the sole mutable
            // reader borrow until MonitorRead releases it, including on unwind.
            reader: unsafe { &mut *self.monitor_rx.get() },
        })
    }

    /// The capture callback: one device block, interleaved at `channels`.
    fn capture(&self, samples: &[f32], channels: u16, sample_rate: u32, stream: u64) {
        let channels = channels.max(1);
        self.block_frames.store(
            (samples.len() / usize::from(channels)) as u64,
            Ordering::Release,
        );

        let mut peak = 0.0f32;
        for sample in samples {
            let magnitude = sample.abs();
            if magnitude > peak {
                peak = magnitude;
            }
        }
        if peak >= 1.0 {
            self.clipped.store(true, Ordering::Release);
        }
        let previous = f32::from_bits(self.peak.load(Ordering::Relaxed));
        if peak > previous {
            self.peak.store(peak.to_bits(), Ordering::Relaxed);
        }

        if self.monitoring.load(Ordering::Acquire) {
            let epoch = self.monitor_epoch.load(Ordering::Acquire);
            // SAFETY: the capture route admits only one monitor writer at a time.
            let tx = unsafe { &mut *self.monitor_tx.get() };
            let width = usize::from(channels);
            let frames = samples.len() / width;
            let whole = frames.min(tx.slots());
            if whole > 0
                && let Ok(chunk) = tx.write_chunk_uninit(whole)
            {
                chunk.fill_from_iter(samples.chunks_exact(width).map(|input| MonitorFrame {
                    stereo: [input[0], *input.get(1).unwrap_or(&input[0])],
                    epoch,
                    stream,
                    sample_rate,
                }));
            }
            let dropped = frames - whole + usize::from(!samples.len().is_multiple_of(width));
            self.monitor_dropped
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
        self.record_writing.store(true, Ordering::SeqCst);
        let expected = self.record_channels.load(Ordering::SeqCst);
        if expected != 0 && expected != u32::from(channels) {
            self.record_channels.store(0, Ordering::SeqCst);
            self.record_layout_changed.store(true, Ordering::Release);
        } else if expected != 0 {
            // SAFETY: this callback is the only writer of the record ring.
            let tx = unsafe { &mut *self.record_tx.get() };
            if let Some(tx) = tx.as_mut() {
                let width = usize::from(channels);
                let frames = samples.len() / width;
                let accepted = frames.min(tx.slots() / width);
                if accepted > 0
                    && let Ok(mut chunk) = tx.write_chunk(accepted * width)
                {
                    let (first, second) = chunk.as_mut_slices();
                    first.copy_from_slice(&samples[..first.len()]);
                    second.copy_from_slice(&samples[first.len()..accepted * width]);
                    chunk.commit_all();
                }
                self.record_dropped
                    .fetch_add((frames - accepted) as u64, Ordering::Relaxed);
            }
        }
        self.record_writing.store(false, Ordering::SeqCst);
    }
}

impl Default for InputPort {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: only the private, CallbackPermit-guarded capture path mutates the
// producers. MonitorRead exclusively borrows the monitor consumer with an
// atomic guard; non-realtime take readers serialize on record_rx's mutex.
// Remaining shared state is atomic or protected by a mutex.
unsafe impl Sync for InputPort {}

struct MonitorRead<'a> {
    busy: &'a AtomicBool,
    reader: &'a mut MonitorReader,
}

impl Drop for MonitorRead<'_> {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

/// Routes capture to the shared port once the stream is accepted.
struct PortSink {
    port: Arc<InputPort>,
    permit: CallbackPermit,
    sample_rate: AtomicU32,
}

impl PortSink {
    fn activate(&self, sample_rate: u32) {
        self.permit.activate(
            || {
                self.port.take_error();
                self.port.flush_monitor();
                self.sample_rate.store(sample_rate, Ordering::Relaxed);
                self.port
                    .monitor_stream
                    .store(self.permit.id(), Ordering::Release);
            },
            |error| self.publish_error(error),
        );
    }

    fn publish_error(&self, error: String) {
        log::error!("audio input stream stopped: {error}");
        self.port.publish_error(error);
    }
}

impl InputSink for PortSink {
    fn capture(&self, samples: &[f32], channels: u16) {
        if let Some(_guard) = self.permit.enter() {
            self.port.capture(
                samples,
                channels,
                self.sample_rate.load(Ordering::Relaxed),
                self.permit.id(),
            );
        }
    }

    fn failed(&self, error: String) {
        self.permit
            .report_failure(error, |error| self.publish_error(error));
    }
}

/// Supplies silence to the main output when input monitoring is its sole owner.
#[derive(Clone, Default)]
pub(crate) struct MonitorOnlyOutput(Arc<Mutex<Option<String>>>);

impl MonitorOnlyOutput {
    pub(crate) fn take_failure(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl RenderSource for MonitorOnlyOutput {
    fn create(&self, _spec: &StreamSpec) -> Arc<dyn RenderSink> {
        Arc::new(self.clone())
    }
}

impl RenderSink for MonitorOnlyOutput {
    fn opened(&self) {
        self.take_failure();
    }

    fn fill(&self, output: &mut [f32]) {
        output.fill(0.0);
    }

    fn failed(&self, error: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
    }
}

/// Wraps a main-output source so the monitored input is summed into every
/// block, whatever the transport is doing.
pub(crate) struct MonitorSource {
    inner: Arc<dyn RenderSource>,
    port: Arc<InputPort>,
}

impl MonitorSource {
    pub(crate) fn new(inner: Arc<dyn RenderSource>, port: Arc<InputPort>) -> Self {
        Self { inner, port }
    }
}

impl RenderSource for MonitorSource {
    fn create(&self, spec: &StreamSpec) -> Arc<dyn RenderSink> {
        Arc::new(MonitorSink {
            inner: self.inner.create(spec),
            port: Arc::clone(&self.port),
            sample_rate: AtomicU32::new(spec.sample_rate),
        })
    }
}

struct MonitorSink {
    inner: Arc<dyn RenderSink>,
    port: Arc<InputPort>,
    sample_rate: AtomicU32,
}

impl RenderSink for MonitorSink {
    fn opened(&self) {
        self.opened_at(0);
    }

    fn opened_at(&self, sample_rate: u32) {
        if sample_rate != 0 {
            self.sample_rate.store(sample_rate, Ordering::Relaxed);
        }
        // A new output starts from current capture, even if the old output
        // was closed while the input device continued filling the ring.
        self.port.flush_monitor();
        self.inner.opened_at(sample_rate);
    }

    fn fill(&self, output: &mut [f32]) {
        self.inner.fill(output);
        self.port
            .mix(output, self.sample_rate.load(Ordering::Relaxed));
    }

    fn failed(&self, error: String) {
        self.inner.failed(error);
    }
}

/// The capture stream, rebuilt by a device sweep like any other managed stream.
pub(crate) struct InputSlot {
    io: Arc<AudioIo>,
    port: Arc<InputPort>,
    route: Arc<CallbackRoute>,
    state: Mutex<Option<StreamSink>>,
}

impl InputSlot {
    pub(crate) fn new(io: Arc<AudioIo>, port: Arc<InputPort>) -> Self {
        Self {
            io,
            port,
            route: Arc::new(CallbackRoute::default()),
            state: Mutex::new(None),
        }
    }

    /// Open on the current capture format, replacing whatever was open.
    pub(crate) fn open(&self) -> Result<(), String> {
        self.replace(false, || self.io.input_spec())
    }

    pub(crate) fn close(&self) {
        let mut state = self.lock();
        self.route.retire_current();
        let previous = state.take();
        if previous.is_some() {
            self.port.monitor_stream.fetch_add(1, Ordering::AcqRel);
            self.port.flush_monitor();
        }
        drop(state);
        drop(previous);
    }

    pub(crate) fn format(&self) -> Option<(u32, u16)> {
        let state = self.lock();
        let stream = state.as_ref()?;
        Some((stream.sample_rate, stream.input_channels?))
    }

    fn replace(&self, reopening: bool, spec: impl FnOnce() -> InputSpec) -> Result<(), String> {
        let mut state = self.lock();
        if reopening && state.is_none() {
            return Ok(());
        }
        let sink = Arc::new(PortSink {
            port: Arc::clone(&self.port),
            permit: self.route.register(),
            sample_rate: AtomicU32::new(0),
        });
        let stream = self
            .io
            .open_input_stream_with(&spec(), sink.clone())
            .map_err(|error| capture_reason(&self.io, &error))?;
        sink.activate(stream.sample_rate);
        let previous = state.replace(stream);
        drop(state);
        drop(previous);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<StreamSink>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl ManagedStream for InputSlot {
    fn role(&self) -> &'static str {
        "input"
    }

    fn is_open(&self) -> bool {
        // AudioIo calls this while holding its status lock. Opening capture
        // holds the slot lock and can update status, so keep this lock-free.
        self.route.has_owner()
    }

    fn reopen(&self, spec: &StreamSpec) -> Result<(), String> {
        // A capture stream follows the output rate on a device change.
        self.replace(true, || {
            self.io.input_spec_for_output_rate(spec.sample_rate)
        })
    }
}

/// A capture open error, prefixed with the device it was for when the backend
/// left the device out of the message.
fn capture_reason(io: &AudioIo, error: &str) -> String {
    let device = io
        .status()
        .opened_input
        .map(|opened| opened.name)
        .unwrap_or_default();
    if device.is_empty() || error.contains(&device) {
        error.to_string()
    } else {
        format!("{device}: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames the monitor ring holds at the stereo width used here.
    const FRAMES: usize = MONITOR_FRAMES;

    /// A port with monitoring already on and the switch's flush consumed.
    fn listening() -> InputPort {
        let port = InputPort::new();
        port.set_monitoring(true);
        let mut stale = [0.0; 2];
        port.mix(&mut stale, 48_000);
        port
    }

    /// A stereo block whose left channel carries the frame index.
    fn ramp(frames: usize) -> Vec<f32> {
        let mut block = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            block.push(index as f32);
            block.push(-(index as f32) - 1.0);
        }
        block
    }

    fn assert_ramp(out: &[f32], frames: usize) {
        for index in 0..frames {
            assert_eq!(out[index * 2], index as f32, "left of frame {index}");
            assert_eq!(
                out[index * 2 + 1],
                -(index as f32) - 1.0,
                "right of frame {index}"
            );
        }
    }

    #[test]
    fn an_odd_block_drops_whole_frames_and_stays_aligned() {
        let port = listening();
        let mut block = ramp(FRAMES);
        block.push(9_999.0); // one sample that cannot complete a frame

        port.capture(&block, 2, 48_000, 0);
        assert_eq!(
            port.monitor_dropped_frames(),
            1,
            "an incomplete frame is one dropped frame, not one sample"
        );

        let mut out = vec![0.0; FRAMES * 2];
        port.mix(&mut out, 48_000);
        assert_ramp(&out, FRAMES);
    }

    #[test]
    fn an_odd_block_leaves_the_following_frames_aligned() {
        let port = listening();
        port.capture(&[0.0, -1.0, 2.0], 2, 48_000, 0); // one frame plus an incomplete one
        assert_eq!(port.monitor_dropped_frames(), 1);

        port.capture(&[3.0, -4.0, 4.0, -5.0], 2, 48_000, 0);
        let mut out = [0.0; 8];
        port.mix(&mut out, 48_000);
        assert_eq!(
            out,
            [0.0, -1.0, 3.0, -4.0, 4.0, -5.0, 0.0, 0.0],
            "the frames after the incomplete one keep their channel order"
        );
    }

    #[test]
    fn repeated_overflows_accumulate_frames_and_keep_the_next_frames_ordered() {
        let port = listening();
        let block = ramp(FRAMES + 1); // one frame more than the ring holds

        port.capture(&block, 2, 48_000, 0);
        assert_eq!(port.monitor_dropped_frames(), 1);
        let mut out = vec![0.0; FRAMES * 2];
        port.mix(&mut out, 48_000);

        port.capture(&block, 2, 48_000, 0);
        assert_eq!(
            port.monitor_dropped_frames(),
            2,
            "the counter accumulates dropped frames"
        );
        port.mix(&mut out, 48_000);

        let normal = [10.0, -10.0, 11.0, -11.0, 12.0, -12.0];
        port.capture(&normal, 2, 48_000, 0);
        let mut heard = [0.0; 6];
        port.mix(&mut heard, 48_000);
        assert_eq!(heard, normal, "the next frames start on a frame boundary");
    }

    #[test]
    fn a_flush_discards_the_ring_and_restarts_at_frame_zero() {
        let port = listening();
        port.capture(&[9.0, -9.0, 8.0, -8.0], 2, 48_000, 0);

        port.flush_monitor();
        let mut flushed = [0.0; 2];
        port.mix(&mut flushed, 48_000);
        assert_eq!(flushed, [0.0, 0.0], "the stale frames are discarded");

        port.capture(&[1.0, -1.0], 2, 48_000, 0);
        let mut out = [0.0; 2];
        port.mix(&mut out, 48_000);
        assert_eq!(
            out,
            [1.0, -1.0],
            "the first frame after the flush is at index 0"
        );
    }

    #[test]
    fn a_mismatched_clock_keeps_fractional_phase_across_callbacks() {
        let port = listening();
        let rate = 44_100;
        let source = ramp(443);
        let mut rendered = Vec::new();
        let mut input_start = 0;
        for (input_end, output_frames) in [(99, 100), (193, 102), (313, 131), (443, 147)] {
            port.capture(&source[input_start * 2..input_end * 2], 2, rate, 0);
            input_start = input_end;
            let mut out = vec![0.0; output_frames * 2];
            port.mix(&mut out, 48_000);
            rendered.extend(out);
        }
        assert_eq!(rendered.len(), 960);
        for (index, stereo) in rendered.chunks_exact(2).enumerate() {
            let expected = index as f32 * 44_100.0 / 48_000.0;
            assert!((stereo[0] - expected).abs() < 0.00004);
            assert!((stereo[1] + expected + 1.0).abs() < 0.00004);
        }
    }

    #[test]
    fn long_monitor_sessions_absorb_clock_drift_in_both_directions() {
        const INPUT_RATE: u32 = 8_000;
        const BLOCK: usize = 128;
        const CAPTURE_BLOCK: usize = 64;
        for (input_ppm, output_rate) in [
            (700_i64, INPUT_RATE),
            (-700, INPUT_RATE),
            (700, 12_000),
            (-700, 12_000),
        ] {
            let port = listening();
            let samples = [0.25; CAPTURE_BLOCK];
            port.capture(&samples, 1, INPUT_RATE, 0);
            port.capture(&samples, 1, INPUT_RATE, 0);
            let capture_quantum = u64::from(output_rate) * CLOCK_SCALE * CAPTURE_BLOCK as u64;
            let mut phase = 0;
            let mut max_queue = 0;
            let mut silent_frames = 0;
            let mut out = [0.0; BLOCK * 2];
            for tick in 0..(output_rate as usize * 120 / BLOCK) {
                phase +=
                    BLOCK as u64 * u64::from(INPUT_RATE) * (CLOCK_SCALE as i64 + input_ppm) as u64;
                while phase >= capture_quantum {
                    port.capture(&samples, 1, INPUT_RATE, 0);
                    phase -= capture_quantum;
                }
                out.fill(0.0);
                port.mix(&mut out, output_rate);
                if tick > 10 {
                    silent_frames += out
                        .chunks_exact(2)
                        .filter(|stereo| stereo[0] < 0.24)
                        .count();
                    // SAFETY: this test is the only output consumer.
                    max_queue = max_queue.max(unsafe { &*port.monitor_rx.get() }.queue.slots());
                }
            }
            assert_eq!(port.monitor_dropped_frames(), 0, "{input_ppm} ppm");
            assert_eq!(silent_frames, 0, "{input_ppm} ppm");
            assert!(
                max_queue < BLOCK * 3,
                "{input_ppm} ppm left {max_queue} frames"
            );
            println!(
                "{input_ppm:+} ppm, {INPUT_RATE} → {output_rate} Hz: peak reserve {max_queue} input frames"
            );
        }
    }

    #[test]
    fn monitor_downsampling_averages_frames_without_changing_the_take() {
        let port = listening();
        port.set_recording_channels(3);
        let first = [1.0, -2.0, 9.0];
        let second = [3.0, -4.0, 10.0];
        port.capture(&first, 3, 96_000, 0);
        let mut out = [0.0; 2];
        port.mix(&mut out, 48_000);
        assert_eq!(out, [0.0, 0.0], "wait for a complete output interval");
        port.capture(&second, 3, 96_000, 0);
        port.mix(&mut out, 48_000);
        assert_eq!(out, [2.0, -3.0]);

        let mut recorded = Vec::new();
        port.take_recorded(&mut recorded, 6);
        assert_eq!(recorded, [first, second].concat());
    }

    #[test]
    fn a_stale_reader_does_not_discard_new_generation_frames() {
        let port = listening();
        let old_epoch = port.monitor_epoch.load(Ordering::Acquire);
        port.capture(&[0.1, 0.2], 2, 48_000, 0);
        port.flush_monitor();
        port.capture(&[0.3, 0.4], 2, 48_000, 0);
        let mut old = [0.0; 4];
        // Simulate the control change after an output callback read the epoch.
        port.try_monitor_reader()
            .unwrap()
            .reader
            .mix(&mut old, 1.0, 48_000, 1, old_epoch, 0);
        assert_eq!(old, [0.1, 0.2, 0.0, 0.0]);
        let mut current = [0.0; 2];
        port.mix(&mut current, 48_000);
        assert_eq!(current, [0.3, 0.4]);
    }

    #[test]
    fn a_retired_capture_callback_cannot_mix_into_the_new_stream() {
        let port = listening();
        port.flush_monitor();
        port.monitor_stream.store(2, Ordering::Release);
        port.capture(&[0.1, 0.2], 2, 44_100, 1);
        port.capture(&[0.3, 0.4], 2, 48_000, 2);

        let mut heard = [0.0; 4];
        port.mix(&mut heard, 48_000);
        assert_eq!(heard, [0.3, 0.4, 0.0, 0.0]);
    }

    #[test]
    fn capture_replacement_flushes_monitoring_and_retires_the_old_writer() {
        use oc_audio::audio_io::{AudioIo, FakeBackend, InputDevice};

        let backend = Arc::new(FakeBackend::new(Vec::new(), None).with_inputs(
            vec![InputDevice {
                key: "coreaudio:mic".into(),
                name: "Built-in Microphone".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![48_000],
                default_rate: 48_000,
            }],
            Some("coreaudio:mic".into()),
        ));
        let io = Arc::new(AudioIo::new(backend.clone(), None));
        let port = Arc::new(InputPort::new());
        let slot = InputSlot::new(io.clone(), Arc::clone(&port));
        slot.open().expect("the fake device captures");
        port.set_monitoring(true);
        let mut stale = [0.0; 2];
        port.mix(&mut stale, 48_000);

        backend.push_input(&[9.0, -9.0], 2);
        let candidate = StreamSpec::new(None, 96_000, None);
        slot.reopen(&candidate).expect("capture can reopen");
        assert_eq!(backend.input_opens().last().unwrap().1.sample_rate, 48_000);

        let mut flushed = [0.0; 2];
        port.mix(&mut flushed, 48_000);
        assert_eq!(flushed, [0.0, 0.0]);

        port.set_recording_channels(2);
        // The fake backend invokes both the original and the replacement sink.
        backend.push_input(&[0.25, -0.25], 2);
        let mut out = [0.0; 4];
        port.mix(&mut out, 48_000);
        assert_eq!(out, [0.25, -0.25, 0.0, 0.0]);
        let mut recorded = Vec::new();
        assert_eq!(port.take_recorded(&mut recorded, 4), 2);
        assert_eq!(recorded, [0.25, -0.25]);

        slot.close();
        slot.reopen(&io.spec()).expect("a closed slot stays closed");
        assert!(!slot.is_open());
        backend.push_input(&[1.0, -1.0], 2);
        assert_eq!(port.take_recorded(&mut recorded, 4), 0);
    }

    #[test]
    fn concurrent_take_readers_consume_each_recorded_sample_once() {
        let port = InputPort::new();
        port.set_recording_channels(1);
        let samples: Vec<f32> = (1..=4096).map(|sample| sample as f32).collect();
        port.capture(&samples, 1, 48_000, 0);
        let start = std::sync::Barrier::new(2);
        let mut taken = std::thread::scope(|scope| {
            let read = || {
                let mut samples = Vec::new();
                start.wait();
                while port.take_recorded(&mut samples, 7) != 0 {}
                samples
            };
            let a = scope.spawn(read);
            let b = scope.spawn(read);
            let mut all = a.join().unwrap();
            all.extend(b.join().unwrap());
            all
        });
        taken.sort_by(f32::total_cmp);
        assert_eq!(taken, samples);
    }

    #[test]
    fn competing_monitor_sinks_do_not_duplicate_captured_frames() {
        let port = Arc::new(listening());
        let source = MonitorSource::new(Arc::new(MonitorOnlyOutput::default()), port.clone());
        let spec = StreamSpec::new(None, 48_000, None);
        let a = source.create(&spec);
        let b = source.create(&spec);
        port.capture(&[1.0, 1.0, 2.0, 2.0], 2, 48_000, 0);
        let start = std::sync::Barrier::new(2);
        let blocks = std::thread::scope(|scope| {
            let run = |sink: &dyn RenderSink| {
                let mut block = [0.0; 4];
                start.wait();
                let allocations = crate::test_alloc::operations(|| sink.fill(&mut block));
                assert_eq!(allocations, 0);
                block
            };
            let a = scope.spawn(move || run(a.as_ref()));
            let b = scope.spawn(move || run(b.as_ref()));
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(
            blocks[0].iter().sum::<f32>() + blocks[1].iter().sum::<f32>(),
            6.0
        );
        let mut tail = [0.0; 4];
        port.mix(&mut tail, 48_000);
        assert_eq!(tail, [0.0; 4]);
    }

    #[test]
    fn a_busy_monitor_reader_skips_without_waiting_and_resumes_after_release() {
        let port = Arc::new(listening());
        let source = MonitorSource::new(Arc::new(MonitorOnlyOutput::default()), port.clone());
        let sink = source.create(&StreamSpec::new(None, 48_000, None));
        port.capture(&[0.25, -0.25], 2, 48_000, 0);
        let reader = port.try_monitor_reader().unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut block = [9.0; 2];
                    assert_eq!(crate::test_alloc::operations(|| sink.fill(&mut block)), 0);
                    assert_eq!(block, [0.0; 2]);
                })
                .join()
                .unwrap();
        });
        drop(reader);
        let mut block = [0.0; 2];
        assert_eq!(crate::test_alloc::operations(|| sink.fill(&mut block)), 0);
        assert_eq!(block, [0.25, -0.25]);
    }

    #[test]
    fn only_the_accepted_capture_stream_reports_failure() {
        let route = Arc::new(CallbackRoute::default());
        let port = Arc::new(InputPort::new());
        let old = PortSink {
            port: port.clone(),
            permit: route.register(),
            sample_rate: AtomicU32::new(0),
        };
        let candidate = PortSink {
            port: port.clone(),
            permit: route.register(),
            sample_rate: AtomicU32::new(0),
        };
        old.activate(48_000);
        candidate.failed("candidate disconnected".into());
        assert_eq!(port.take_error(), None);

        candidate.activate(48_000);
        old.failed("old device disconnected".into());
        assert_eq!(port.take_error().as_deref(), Some("candidate disconnected"));
        assert_eq!(port.take_error(), None);
    }
}
