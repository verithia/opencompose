//! What the render thread is doing while it prepares a project.
//!
//! A project change rebuilds every instrument, which can take seconds when
//! hosted plugins are instantiated or a SoundFont bank is decoded. The render
//! thread publishes one snapshot of that work so the panel can name the stage
//! instead of showing a spinner with no progress.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

/// Relative cost of the items a project load prepares, so the panel's bar
/// moves at roughly even speed: restoring a hosted instrument's state takes
/// about as long as a dozen hosted effects, and both dwarf a built-in lane.
pub const LANE_UNITS: usize = 1;
pub const HOSTED_EFFECT_UNITS: usize = 6;
pub const HOSTED_INSTRUMENT_UNITS: usize = 80;

/// The units one source lane costs to build now.
pub fn lane_units(hosted: bool) -> usize {
    if hosted {
        HOSTED_INSTRUMENT_UNITS
    } else {
        LANE_UNITS
    }
}

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
    /// The project plays while later instruments load behind the playhead.
    Background,
    /// Playback reached an instrument that has not loaded and waits for it.
    Waiting,
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
            Stage::Background => "background",
            Stage::Waiting => "waiting",
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
    /// How much of the whole load is done, in thousandths; `None` while the
    /// load has not said how much work it holds.
    pub progress: Option<u16>,
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
    /// Weighted work finished and planned across every stage of the load.
    units_done: AtomicUsize,
    units_total: AtomicUsize,
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
            units_done: AtomicUsize::new(0),
            units_total: AtomicUsize::new(0),
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

    fn set_units(&self, done: usize, total: usize) {
        self.units_total.store(total, Ordering::Release);
        self.units_done.store(done.min(total), Ordering::Release);
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
            progress: permille(
                self.units_done.load(Ordering::Acquire),
                self.units_total.load(Ordering::Acquire),
            ),
        }
    }
}

fn permille(done: usize, total: usize) -> Option<u16> {
    (total > 0).then(|| (done.min(total) * 1000 / total) as u16)
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
        self.progress.set_units(0, 0);
        self.progress.set(Stage::Reading, "", 0, total);
    }

    /// The load builds `lanes` source lanes and holds `units` of weighted work
    /// in all; the bar fills against that.
    pub fn plan(&self, lanes: usize, units: usize) {
        self.progress.total.store(lanes, Ordering::Release);
        self.progress.set_units(0, units);
    }

    /// `units` of the planned work are finished.
    pub fn advance(&self, units: usize) {
        let total = self.progress.units_total.load(Ordering::Acquire);
        self.progress.set_units(units, total);
    }

    /// A mixer build report that fills the `span` units after `base`, in step
    /// with the hosted plugins the graph loads.
    pub fn graph_report(&self, base: usize, span: usize) -> oc_engine::graph::GraphBuildReport {
        let progress = Arc::clone(&self.progress);
        Arc::new(move |step: oc_engine::graph::GraphBuildStep| {
            // A mixer with no hosted plugins has nothing to wait for.
            let filled = (span * step.done.min(step.total))
                .checked_div(step.total)
                .unwrap_or(span);
            let total = progress.units_total.load(Ordering::Acquire);
            progress.set_units(base + filled, total);
        })
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

    /// Instruments still loading after the project was published: `done` of
    /// `total` have joined, and `stage` says whether playback waits for them.
    pub fn background(&self, stage: Stage, done: usize, total: usize) {
        self.progress.set_units(done, total);
        self.progress.set(stage, "", done, total);
    }

    /// The build finished or failed; nothing is left to report.
    pub fn finish(&self) {
        self.progress.generation.fetch_add(1, Ordering::AcqRel);
        self.progress.set_units(0, 0);
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

    pub fn plan(&self, lanes: usize, units: usize) {
        self.handle.plan(lanes, units);
    }

    pub fn advance(&self, units: usize) {
        self.handle.advance(units);
    }

    pub fn graph_report(&self, base: usize, span: usize) -> oc_engine::graph::GraphBuildReport {
        self.handle.graph_report(base, span)
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
        assert_eq!(reading.progress, None, "no bar before the load is planned");

        handle.plan(3, 100);
        assert_eq!(snapshot().unwrap().progress, Some(0));
        handle.advance(40);
        assert_eq!(snapshot().unwrap().progress, Some(400));
        // The mixer fills the rest in step with the plugins it loads.
        let report = handle.graph_report(40, 60);
        report(oc_engine::graph::GraphBuildStep { done: 1, total: 2 });
        assert_eq!(snapshot().unwrap().progress, Some(700));
        report(oc_engine::graph::GraphBuildStep { done: 2, total: 2 });
        assert_eq!(snapshot().unwrap().progress, Some(1000));
        handle.advance(40);

        handle.lane(1, "Hosted Instrument");
        let lane = snapshot().expect("a lane build is reported");
        assert_eq!(lane.stage, "instruments");
        assert_eq!(lane.label, "Hosted Instrument");
        assert_eq!((lane.done, lane.total), (1, 3));

        handle.stage(Stage::Effects);
        assert_eq!(snapshot().expect("effects are reported").stage, "effects");

        handle.finish();
        assert!(snapshot().is_none());

        // Instruments loading behind playback are reported until the last
        // joins. One test owns the process-wide report, so this stays here.
        handle.background(Stage::Background, 2, 5);
        let loading = snapshot().expect("background loading is reported");
        assert_eq!(loading.stage, "background");
        assert_eq!((loading.done, loading.total), (2, 5));
        assert_eq!(loading.progress, Some(400));
        handle.background(Stage::Waiting, 2, 5);
        assert_eq!(snapshot().expect("a wait is reported").stage, "waiting");
        handle.finish();
        assert!(snapshot().is_none());
        // A new load starts with no bar left from the last one.
        handle.begin(1);
        assert_eq!(snapshot().unwrap().progress, None);
        handle.finish();
    }
}
