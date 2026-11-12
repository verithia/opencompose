//! Lock-free audio hand-off from a plugin editor's preview instance.
//!
//! The preview instance stays on the worker thread that drives it; the device
//! callback only reads this ring. The shared gate mutes the reader while the
//! transport plays, so the renderer's own instance is never doubled. The
//! callback allocates nothing, locks nothing and never blocks on the worker:
//! a full ring drops the newest block.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Ring capacity in stereo frames. Occupancy, not capacity, sets latency.
pub const DEFAULT_CAPACITY_FRAMES: usize = 8_192;

/// Counters shared between the worker and the device callback.
#[derive(Default)]
struct Stats {
    underruns: AtomicU64,
    dropped: AtomicU64,
    published_frames: AtomicU64,
    played_frames: AtomicU64,
    discarded_frames: AtomicU64,
    device_frames: AtomicUsize,
}

/// Counters and queue depth observed at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorReport {
    pub queued_frames: usize,
    pub underruns: u64,
    pub dropped: u64,
    pub published_frames: u64,
    pub played_frames: u64,
    pub discarded_frames: u64,
}

/// Worker side: publishes processed blocks and refuses to block.
pub struct MonitorWriter {
    producer: rtrb::Producer<f32>,
    stats: Arc<Stats>,
    capacity_frames: usize,
}

/// Device side: drains one callback's worth of frames, muting as the gate says.
pub struct MonitorReader {
    consumer: rtrb::Consumer<f32>,
    gate: Arc<AtomicBool>,
    stats: Arc<Stats>,
}

/// Create a monitor pair. `gate` is shared with the transport: true means the
/// preview instance is audible.
pub fn channel(capacity_frames: usize, gate: Arc<AtomicBool>) -> (MonitorWriter, MonitorReader) {
    let frames = capacity_frames.max(2) & !1;
    let (producer, consumer) = rtrb::RingBuffer::new(frames * 2);
    let stats = Arc::new(Stats::default());
    (
        MonitorWriter {
            producer,
            stats: Arc::clone(&stats),
            capacity_frames: frames,
        },
        MonitorReader {
            consumer,
            gate,
            stats,
        },
    )
}

impl MonitorWriter {
    /// Frames the ring can hold; the worker paces against this.
    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    /// Frames waiting for the device.
    pub fn queued_frames(&self) -> usize {
        (self.capacity_frames * 2).saturating_sub(self.producer.slots()) / 2
    }

    /// Frames the worker may keep queued: one device callback plus one block of
    /// slack, so scheduling jitter does not underrun the callback.
    pub fn target_frames(&self, slack_frames: usize) -> usize {
        let device = self.stats.device_frames.load(Ordering::Relaxed);
        let ceiling = self
            .capacity_frames
            .saturating_sub(slack_frames)
            .max(slack_frames);
        (device + slack_frames).clamp(slack_frames, ceiling)
    }

    /// Publish one interleaved stereo block. `audible` false writes silence so
    /// the reader's timeline stays continuous while the transport plays.
    ///
    /// Returns false when the ring had no room; the block is dropped rather
    /// than waiting, which is what a realtime producer must do.
    pub fn publish(&mut self, block: &[f32], audible: bool) -> bool {
        let frames = block.len() / 2;
        if frames == 0 {
            return true;
        }
        if block.len() > self.capacity_frames * 2 {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let Ok(mut chunk) = self.producer.write_chunk(block.len()) else {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if audible {
            let (first, second) = chunk.as_mut_slices();
            first.copy_from_slice(&block[..first.len()]);
            if !second.is_empty() {
                second.copy_from_slice(&block[first.len()..]);
            }
        } else {
            let (first, second) = chunk.as_mut_slices();
            first.fill(0.0);
            second.fill(0.0);
        }
        chunk.commit_all();
        self.stats
            .published_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
        true
    }

    /// Report counters for logs and tests.
    pub fn report(&self) -> MonitorReport {
        self.report_from(&self.stats, self.queued_frames())
    }

    fn report_from(&self, stats: &Stats, queued_frames: usize) -> MonitorReport {
        MonitorReport {
            queued_frames,
            underruns: stats.underruns.load(Ordering::Relaxed),
            dropped: stats.dropped.load(Ordering::Relaxed),
            published_frames: stats.published_frames.load(Ordering::Relaxed),
            played_frames: stats.played_frames.load(Ordering::Relaxed),
            discarded_frames: stats.discarded_frames.load(Ordering::Relaxed),
        }
    }
}

impl MonitorReader {
    /// Fill one device callback from the ring. Silence while muted or short.
    pub fn read(&mut self, output: &mut [f32]) {
        self.stats
            .device_frames
            .store(output.len() / 2, Ordering::Relaxed);
        if output.is_empty() {
            return;
        }
        if !self.gate.load(Ordering::Acquire) {
            // Discard at the device rate, not all at once: the worker stays
            // paced and the queue never holds audible audio from before play.
            let stale = self.consumer.slots().min(output.len());
            if stale > 0
                && let Ok(chunk) = self.consumer.read_chunk(stale)
            {
                chunk.commit_all();
                self.stats
                    .discarded_frames
                    .fetch_add((stale / 2) as u64, Ordering::Relaxed);
            }
            output.fill(0.0);
            return;
        }

        let available = self.consumer.slots().min(output.len());
        let mut written = 0;
        if available > 0
            && let Ok(chunk) = self.consumer.read_chunk(available)
        {
            let (first, second) = chunk.as_slices();
            output[..first.len()].copy_from_slice(first);
            written += first.len();
            if !second.is_empty() {
                let take = second.len().min(output.len() - written);
                output[written..written + take].copy_from_slice(&second[..take]);
                written += take;
            }
            chunk.commit_all();
        }
        if written < output.len() {
            output[written..].fill(0.0);
            self.stats.underruns.fetch_add(1, Ordering::Relaxed);
        }
        self.stats
            .played_frames
            .fetch_add((written / 2) as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::PluginTransport;
    use crate::vst3_instance::test_support::{FakeControl, fake_instance};

    use super::*;

    #[test]
    fn a_preview_instance_reaches_the_device_and_mutes_during_playback() {
        let control = Arc::new(FakeControl::default());
        control.tone_hz.store(1_000, Ordering::Relaxed);
        let mut instance = fake_instance(&control, 2, 512);
        let gate = Arc::new(AtomicBool::new(true));
        let (mut writer, mut reader) = channel(2_048, Arc::clone(&gate));

        let transport = PluginTransport::new(0, 48_000, 120.0);
        let mut outputs = vec![Vec::new()];
        for _ in 0..2 {
            outputs[0].clear();
            instance
                .process_block_outputs_into(512, Some(transport.clone()), &[0], &mut outputs)
                .unwrap();
            assert!(writer.publish(&outputs[0], true));
        }
        assert_eq!(control.process_calls.load(Ordering::Relaxed), 2);

        let mut heard = vec![0.0; 1_024 * 2];
        reader.read(&mut heard);
        assert!(
            heard.iter().any(|sample| sample.abs() > 0.05),
            "the fake plugin's tone did not reach the device buffer"
        );
        let zero_crossings = heard
            .chunks_exact(2)
            .zip(heard.chunks_exact(2).skip(1))
            .filter(|(before, after)| before[0] <= 0.0 && after[0] > 0.0)
            .count();
        assert!(
            zero_crossings.abs_diff(21) < 4,
            "expected about 1 kHz, got {zero_crossings} cycles per 1024 frames"
        );
        assert_eq!(writer.report().underruns, 0);

        // The transport starts: the reader must hear silence even though the
        // worker keeps the plugin processing.
        gate.store(false, Ordering::Release);
        outputs[0].clear();
        instance
            .process_block_outputs_into(512, Some(transport.clone()), &[0], &mut outputs)
            .unwrap();
        assert!(writer.publish(&outputs[0], false));
        let mut muted = vec![1.0; 1_024 * 2];
        reader.read(&mut muted);
        assert!(muted.iter().all(|sample| *sample == 0.0));
        assert_eq!(writer.report().dropped, 0);
    }

    #[test]
    fn a_full_ring_drops_the_block_instead_of_blocking() {
        let gate = Arc::new(AtomicBool::new(true));
        let (mut writer, mut reader) = channel(512, Arc::clone(&gate));
        let block = vec![0.5; 512 * 2];
        assert!(writer.publish(&block, true));
        assert!(!writer.publish(&block, true));
        assert_eq!(writer.report().dropped, 1);
        assert_eq!(writer.queued_frames(), 512);

        let mut out = vec![0.0; 512 * 2];
        reader.read(&mut out);
        assert!(out.iter().all(|sample| *sample == 0.5));
        assert_eq!(writer.report().dropped, 1);
    }

    #[test]
    fn queued_audio_is_discarded_once_the_gate_closes() {
        let gate = Arc::new(AtomicBool::new(true));
        let (mut writer, mut reader) = channel(2_048, Arc::clone(&gate));
        let block = vec![0.5; 512 * 2];
        assert!(writer.publish(&block, true));
        assert!(writer.publish(&block, true));

        gate.store(false, Ordering::Release);
        let mut out = vec![1.0; 1_024 * 2];
        reader.read(&mut out);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert_eq!(writer.report().queued_frames, 0);
        assert_eq!(writer.report().discarded_frames, 1_024);
    }
}
