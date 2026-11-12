//! Parallel track rendering with a global thread pool.
//!
//! Architecture:
//! ┌──────────┐  ┌──────────┐  ┌──────────┐
//! │  Track 0 │  │  Track 1 │  │  Track N │   ← parallel (each own SoundFont renderer)
//! └────┬─────┘  └────┬─────┘  └────┬─────┘
//!      │              │              │
//!      └──────────────┼──────────────┘
//!                     ▼
//!              ┌─────────────┐
//!              │  AudioGraph  │   ← sequential (mix buses + master)
//!              └─────────────┘
//!
//! Workers pull the longest (most events) task first from a shared priority queue.
//!
//! Each worker owns its renderer. The player has no shared state, so
//! construction and rendering are both parallel.

use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::interpret::MidiEvent;
use crate::sf2::DynamicRamp;
use crate::soundfont::SoundFontRenderer;

/// A rendering task for one track.
#[derive(Clone)]
pub struct TrackRenderJob {
    pub track_index: usize,
    pub events: Vec<MidiEvent>,
    pub total_duration_seconds: f64,
    pub is_drum_track: bool,
    /// Hairpin ramps in output frames; empty keeps every note on one layer.
    pub dynamic_ramps: Vec<DynamicRamp>,
    /// False restores hard velocity-layer selection for this track.
    pub dynamic_layers: bool,
    /// The preset's `output.gain` in dB and `output.pan`.
    pub output_gain_db: f64,
    pub output_pan: f64,
}

/// Priority queue entry with original index.
struct QueueEntry {
    job: TrackRenderJob,
    original_index: usize,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.job.events.len() == other.job.events.len()
    }
}
impl Eq for QueueEntry {}
impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.job.events.len().cmp(&other.job.events.len())
    }
}

/// Parallel render pool — a fresh SoundFont renderer for every track job.
pub struct RenderPool {
    soundfont: PathBuf,
    sample_rate: u32,
    worker_options: RenderWorkerOptions,
}

/// An immutable worker-count snapshot for one render request. Explicit GUI
/// preferences never need to modify the process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderWorkerOptions {
    workers: std::num::NonZeroUsize,
}

impl RenderWorkerOptions {
    /// Explicit worker count.
    pub fn new(workers: std::num::NonZeroUsize) -> Self {
        Self { workers }
    }

    /// Configured worker count.
    pub fn workers(self) -> usize {
        self.workers.get()
    }

    /// Worker count from available parallelism, capped by configuration.
    pub fn machine() -> Self {
        Self::from_pin(None, || {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
        })
    }

    /// Resolve an optional CLI pin without reading or changing the environment.
    pub fn from_pin(pin: Option<&str>, available: impl FnOnce() -> usize) -> Self {
        let count = resolve_render_workers(pin, available).max(1);
        Self::new(std::num::NonZeroUsize::new(count).expect("positive worker count"))
    }
}

impl Default for RenderWorkerOptions {
    /// Snapshot the inherited CLI pin. GUI requests supply their own options.
    fn default() -> Self {
        let pin = std::env::var("OCP_RENDER_THREADS").ok();
        Self::from_pin(pin.as_deref(), || Self::machine().workers())
    }
}

/// Compatibility accessor for callers without an explicit request snapshot.
pub fn render_workers() -> usize {
    RenderWorkerOptions::default().workers()
}

fn resolve_render_workers(pin: Option<&str>, available: impl FnOnce() -> usize) -> usize {
    pin.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or_else(available)
}
impl RenderPool {
    /// Pool using configured worker defaults.
    pub fn new(soundfont: PathBuf, sample_rate: u32) -> Self {
        Self::with_options(soundfont, sample_rate, RenderWorkerOptions::default())
    }

    /// Construct a pool without consulting process-wide configuration.
    pub fn with_options(
        soundfont: PathBuf,
        sample_rate: u32,
        worker_options: RenderWorkerOptions,
    ) -> Self {
        Self {
            soundfont,
            sample_rate,
            worker_options,
        }
    }

    /// Replace the pool worker options.
    pub fn with_worker_options(mut self, options: RenderWorkerOptions) -> Self {
        self.worker_options = options;
        self
    }

    /// Render track buffers in parallel. Returns `Vec<Vec<f32>>` in original track order.
    ///
    /// Each job gets its own renderer, so controller state and release tails
    /// cannot leak between tracks; independent renderers render in parallel.
    pub fn render_tracks(&self, jobs: Vec<TrackRenderJob>) -> Result<Vec<Vec<f32>>, String> {
        self.render_tracks_with_cancel(jobs, None)
    }

    /// Render tracks and stop at the next chunk when `cancel` is set.
    pub fn render_tracks_with_cancel(
        &self,
        jobs: Vec<TrackRenderJob>,
        cancel: Option<&crate::render_cancel::CancelToken>,
    ) -> Result<Vec<Vec<f32>>, String> {
        self.render_tracks_with_workers(jobs, self.worker_options.workers(), cancel)
    }

    fn render_tracks_with_workers(
        &self,
        jobs: Vec<TrackRenderJob>,
        workers: usize,
        cancel: Option<&crate::render_cancel::CancelToken>,
    ) -> Result<Vec<Vec<f32>>, String> {
        if jobs.is_empty() {
            return Ok(vec![]);
        }

        let n = jobs.len();
        let num_workers = workers.max(1).min(n);

        // Priority queue: longest job first (heuristic for minimizing total time)
        let mut heap = BinaryHeap::new();
        for (i, job) in jobs.into_iter().enumerate() {
            heap.push(QueueEntry {
                job,
                original_index: i,
            });
        }
        let queue = Arc::new(Mutex::new(heap));

        let results: Arc<Vec<Mutex<Option<Vec<f32>>>>> =
            Arc::new((0..n).map(|_| Mutex::new(None)).collect());

        // Track init errors from workers
        let init_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let sf = Arc::new(self.soundfont.clone());
        let sr = self.sample_rate;

        let mut handles = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let sf = Arc::clone(&sf);
            let init_errors = Arc::clone(&init_errors);
            let worker_cancel = cancel.cloned();

            handles.push(std::thread::spawn(move || {
                loop {
                    if crate::render_cancel::stopped(worker_cancel.as_ref()) {
                        break;
                    }
                    let entry = {
                        let mut q = queue.lock().unwrap();
                        q.pop()
                    };
                    match entry {
                        Some(entry) => {
                            let started = std::time::Instant::now();
                            // Workers can process more than one job. Reusing a
                            // synth leaks controllers, sounding release tails and
                            // its internal clock into unrelated tracks, making
                            // output depend on worker scheduling. A fresh instance
                            // is bounded to one per active worker, not all jobs.
                            let mut renderer = match SoundFontRenderer::new(sr, &sf) {
                                Ok(r) => r,
                                Err(e) => {
                                    init_errors.lock().unwrap().push(e);
                                    return;
                                }
                            };
                            renderer.set_cancel_token(worker_cancel.clone());
                            renderer.enable_drum_channel();
                            renderer.set_output(entry.job.output_gain_db, entry.job.output_pan);
                            renderer.set_dynamic_layers(entry.job.dynamic_layers);
                            renderer.set_dynamic_ramps(entry.job.dynamic_ramps.clone());
                            let buf = match renderer.try_render_events(
                                &entry.job.events,
                                entry.job.total_duration_seconds,
                            ) {
                                Ok(buf) => {
                                    crate::stage_log::push_worker_track(
                                        "soundfont track",
                                        entry.job.track_index,
                                        started,
                                    );
                                    buf
                                }
                                Err(error) => {
                                    init_errors.lock().unwrap().push(error);
                                    return;
                                }
                            };
                            *results[entry.original_index].lock().unwrap() = Some(buf);
                        }
                        None => break,
                    }
                }
            }));
        }

        // Join all workers, collecting any panics
        let mut worker_panic = None;
        for (i, h) in handles.into_iter().enumerate() {
            if let Err(e) = h.join() {
                let msg = if let Some(s) = e.downcast_ref::<&str>() {
                    format!("Render worker {} panicked: {}", i, s)
                } else if let Some(s) = e.downcast_ref::<String>() {
                    format!("Render worker {} panicked: {}", i, s)
                } else {
                    format!("Render worker {} panicked (unknown cause)", i)
                };
                log::error!("{}", msg);
                if worker_panic.is_none() {
                    worker_panic = Some(msg);
                }
            }
        }

        // A stop request discards every buffer the workers finished; the
        // renderer's caller publishes nothing on a cancelled render.
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }

        if let Some(panic_msg) = worker_panic {
            return Err(panic_msg);
        }

        // Check for init errors
        let errors = init_errors.lock().unwrap();
        if !errors.is_empty() {
            return Err(format!("SoundFont worker init failed: {}", errors[0]));
        }

        // Collect results — all slots should be filled since workers didn't panic
        let output: Result<Vec<Vec<f32>>, String> = results
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                cell.lock().unwrap().take().ok_or_else(|| {
                    format!(
                        "Track {} was not rendered (worker may have failed to init)",
                        i
                    )
                })
            })
            .collect();

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpret::MidiMessage;

    fn test_soundfont() -> PathBuf {
        let path = std::env::var_os("OCP_TEST_SOUNDFONT")
            .map(PathBuf::from)
            .expect("set OCP_TEST_SOUNDFONT to a locally licensed SoundFont");
        assert!(
            path.is_file(),
            "OCP_TEST_SOUNDFONT must name an existing file"
        );
        path
    }

    #[test]
    fn worker_snapshots_are_independent_and_need_no_environment_mutation() {
        let a = RenderWorkerOptions::from_pin(Some("4"), || 8);
        let b = RenderWorkerOptions::from_pin(None, || 2);
        assert_eq!(a.workers(), 4);
        assert_eq!(b.workers(), 2);
        assert_eq!(a.workers(), 4);
        assert_eq!(RenderWorkerOptions::from_pin(None, || 0).workers(), 1);
        let pool = RenderPool::new(PathBuf::new(), 48000).with_worker_options(b);
        assert_eq!(pool.worker_options, b);
    }

    #[test]
    fn render_worker_count_honours_the_pin() {
        assert_eq!(
            resolve_render_workers(Some("4"), || panic!(
                "pinned worker count must not probe hardware"
            )),
            4
        );
        assert_eq!(resolve_render_workers(Some(" 3 "), || 8), 3);
        for pin in [None, Some("0"), Some("garbage"), Some("-1")] {
            assert_eq!(resolve_render_workers(pin, || 8), 8);
        }
    }

    #[test]
    #[ignore = "requires OCP_TEST_SOUNDFONT; run explicitly with --ignored"]
    fn concurrent_lifecycle_preserves_exact_track_audio() {
        let font = test_soundfont();
        let pool = RenderPool::new(font, 48000);
        // Unequal jobs repeatedly overlap loading/unloading with other voices.
        let jobs: Vec<_> = (0..24)
            .map(|i| TrackRenderJob {
                track_index: i,
                total_duration_seconds: 0.04 + (i % 4) as f64 * 0.03,
                is_drum_track: false,
                dynamic_ramps: Vec::new(),
                dynamic_layers: true,
                output_gain_db: 0.0,
                output_pan: 0.0,
                events: vec![MidiEvent {
                    time_samples: 0,
                    track_index: i,
                    event: MidiMessage::NoteOn {
                        channel: 0,
                        note: 48 + i as u8,
                        velocity: 90,
                    },
                }],
            })
            .collect();
        let serial = pool
            .render_tracks_with_workers(jobs.clone(), 1, None)
            .unwrap();
        for _ in 0..3 {
            let parallel = pool
                .render_tracks_with_workers(jobs.clone(), 8, None)
                .unwrap();
            assert_eq!(serial, parallel, "concurrent lifecycle changed audio");
        }
    }

    #[test]
    #[ignore = "requires OCP_TEST_SOUNDFONT; run explicitly with --ignored"]
    fn reused_worker_must_not_leak_synth_state_between_tracks() {
        let soundfont = test_soundfont();
        assert!(
            soundfont.is_file(),
            "qualified local SoundFont fixture required"
        );
        let event = |time_samples, event| MidiEvent {
            time_samples,
            event,
            track_index: 0,
        };
        let mut first = TrackRenderJob {
            track_index: 0,
            total_duration_seconds: 0.25,
            is_drum_track: false,
            dynamic_ramps: Vec::new(),
            dynamic_layers: true,
            output_gain_db: 0.0,
            output_pan: 0.0,
            events: vec![
                event(
                    0,
                    MidiMessage::ProgramChange {
                        channel: 0,
                        program: 0,
                    },
                ),
                event(
                    0,
                    MidiMessage::ControlChange {
                        channel: 0,
                        controller: 11,
                        value: 0,
                    },
                ),
                event(
                    0,
                    MidiMessage::NoteOn {
                        channel: 0,
                        note: 60,
                        velocity: 90,
                    },
                ),
                event(
                    1000,
                    MidiMessage::NoteOff {
                        channel: 0,
                        note: 60,
                    },
                ),
            ],
        };
        let second = TrackRenderJob {
            track_index: 1,
            total_duration_seconds: 0.25,
            is_drum_track: false,
            dynamic_ramps: Vec::new(),
            dynamic_layers: true,
            output_gain_db: 0.0,
            output_pan: 0.0,
            events: vec![
                event(
                    0,
                    MidiMessage::ProgramChange {
                        channel: 0,
                        program: 0,
                    },
                ),
                event(
                    0,
                    MidiMessage::NoteOn {
                        channel: 0,
                        note: 64,
                        velocity: 90,
                    },
                ),
                event(
                    1000,
                    MidiMessage::NoteOff {
                        channel: 0,
                        note: 64,
                    },
                ),
            ],
        };
        let pool = RenderPool::new(soundfont, 48000);
        let baseline = pool
            .render_tracks_with_workers(vec![second.clone()], 1, None)
            .unwrap();
        assert!(baseline[0].iter().any(|s| s.abs() > 1e-5));
        // More events force the contaminating job to run first in the heap.
        first.events.push(event(
            2000,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 11,
                value: 0,
            },
        ));
        let serial = pool
            .render_tracks_with_workers(vec![first.clone(), second.clone()], 1, None)
            .unwrap();
        assert!(
            serial[1] == baseline[0],
            "previous job changed the next track's audio"
        );
        let parallel = pool
            .render_tracks_with_workers(vec![first, second], 2, None)
            .unwrap();
        assert!(
            serial == parallel,
            "worker scheduling changed rendered audio"
        );
    }
}
