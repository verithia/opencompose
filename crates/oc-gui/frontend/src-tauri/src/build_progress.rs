//! What the render thread is doing while it prepares a project.
//!
//! A project change rebuilds every instrument, which can take seconds when
//! hosted plugins are instantiated or a SoundFont bank is decoded. The render
//! thread publishes one snapshot of that work so the panel can name the stage
//! instead of showing a spinner with no progress.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

/// The stage a project load is in. Wire values are stable strings, so the panel
/// can translate them without tracking enum order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Idle,
    Reading,
    Instruments,
    Arrangement,
    Effects,
    SoundFont,
}

impl Stage {
    fn code(self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Reading => "reading",
            Stage::Instruments => "instruments",
            Stage::Arrangement => "arrangement",
            Stage::Effects => "effects",
            Stage::SoundFont => "soundfont",
        }
    }
}

/// One published state of a project build.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BuildSnapshot {
    pub stage: &'static str,
    /// The track or preset being prepared, empty outside a per-instrument stage.
    pub label: String,
    pub done: usize,
    pub total: usize,
}

impl BuildSnapshot {
    /// Whether the panel should show this at all.
    pub fn is_active(&self) -> bool {
        self.stage != "idle"
    }
}

struct BuildProgress {
    stage: Mutex<Stage>,
    label: Mutex<String>,
    done: AtomicUsize,
    total: AtomicUsize,
    /// Bumped whenever a build starts or stops, so a reader can tell a fresh
    /// build from a snapshot left by the previous one.
    generation: AtomicU64,
}

impl BuildProgress {
    fn new() -> Self {
        Self {
            stage: Mutex::new(Stage::Idle),
            label: Mutex::new(String::new()),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
        }
    }

    fn set(&self, stage: Stage, label: &str, done: usize, total: usize) {
        *self
            .stage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = stage;
        *self
            .label
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = label.to_string();
        self.done.store(done, Ordering::Release);
        self.total.store(total, Ordering::Release);
    }

    fn snapshot(&self) -> BuildSnapshot {
        let stage = *self
            .stage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        BuildSnapshot {
            stage: stage.code(),
            label: self
                .label
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
            done: self.done.load(Ordering::Acquire),
            total: self.total.load(Ordering::Acquire),
        }
    }
}

fn shared() -> &'static Arc<BuildProgress> {
    static PROGRESS: OnceLock<Arc<BuildProgress>> = OnceLock::new();
    PROGRESS.get_or_init(|| Arc::new(BuildProgress::new()))
}

/// The handle the render thread publishes through.
pub fn handle() -> Handle {
    Handle {
        progress: Arc::clone(shared()),
    }
}

/// Publication side, held by the render thread for the length of a build.
pub struct Handle {
    progress: Arc<BuildProgress>,
}

impl Handle {
    /// Start a build over `total` instruments.
    pub fn begin(&self, total: usize) {
        self.progress.generation.fetch_add(1, Ordering::AcqRel);
        self.progress.set(Stage::Reading, "", 0, total);
    }

    pub fn stage(&self, stage: Stage) {
        let total = self.progress.total.load(Ordering::Acquire);
        let done = self.progress.done.load(Ordering::Acquire);
        self.progress.set(stage, "", done, total);
    }

    /// Name one instrument while its lane is being prepared.
    pub fn lane(&self, done: usize, label: &str) {
        let total = self.progress.total.load(Ordering::Acquire);
        self.progress.set(Stage::Instruments, label, done, total);
    }

    /// A SoundFont stage inside the current build: the engine reports `read`,
    /// `index` or `decode` while it prepares one bank.
    pub fn detail(&self, stage: &'static str, done: usize, total: usize) {
        // Preparation also runs outside a build (the preset browser inspects a
        // bank); only a running build owns the report.
        let running = *self
            .progress
            .stage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            != Stage::Idle;
        if !running {
            return;
        }
        self.progress.set(Stage::SoundFont, stage, done, total);
    }

    /// The build finished or failed; nothing is left to report.
    pub fn finish(&self) {
        self.progress.generation.fetch_add(1, Ordering::AcqRel);
        self.progress.set(Stage::Idle, "", 0, 0);
    }
}

/// A build in progress. Dropping it publishes `idle`, so an early return or a
/// failed load cannot leave a stale stage on the panel.
pub struct BuildScope {
    handle: Handle,
}

impl BuildScope {
    pub fn lane(&self, done: usize, label: &str) {
        self.handle.lane(done, label);
    }

    pub fn stage(&self, stage: Stage) {
        self.handle.stage(stage);
    }
}

impl Drop for BuildScope {
    fn drop(&mut self) {
        self.handle.finish();
    }
}

/// Start reporting a build over `total` tracks; the scope finishes it.
pub fn scope(total: usize) -> BuildScope {
    let handle = handle();
    handle.begin(total);
    BuildScope { handle }
}

/// Route SoundFont preparation reports into the current build. Called once at
/// startup; without it a bank preparation only reaches the log.
pub fn install_engine_reports() {
    oc_engine::sf2::set_report_sink(Some(Arc::new(|stage, done, total| {
        handle().detail(stage, done, total);
    })));
}

/// What the render thread is doing now; `None` once the build is finished.
pub fn snapshot() -> Option<BuildSnapshot> {
    let snapshot = shared().snapshot();
    snapshot.is_active().then_some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_names_its_stage_and_each_lane() {
        let handle = handle();
        handle.begin(3);
        let reading = snapshot().expect("a started build is reported");
        assert_eq!(reading.stage, "reading");
        assert_eq!(reading.total, 3);

        handle.lane(1, "Hosted Instrument");
        let lane = snapshot().expect("a lane build is reported");
        assert_eq!(lane.stage, "instruments");
        assert_eq!(lane.label, "Hosted Instrument");
        assert_eq!((lane.done, lane.total), (1, 3));

        handle.stage(Stage::Effects);
        assert_eq!(snapshot().expect("effects are reported").stage, "effects");

        handle.finish();
        assert!(snapshot().is_none());
    }
}
