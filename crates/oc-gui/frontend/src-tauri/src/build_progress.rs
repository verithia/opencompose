//! What the render thread is doing while it prepares a project.
//!
//! A project change rebuilds every instrument, which can take seconds when
//! hosted plugins are instantiated or a SoundFont bank is decoded. The render
//! thread publishes one snapshot of that work so the panel can name the stage
//! instead of showing a spinner with no progress.

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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Stage {
    #[default]
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
    /// Identifies one load across its foreground and background stages.
    pub generation: u64,
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

#[derive(Default)]
struct BuildProgress {
    generation: u64,
    stage: Stage,
    label: String,
    done: usize,
    total: usize,
    lanes: usize,
    units_done: usize,
    units_total: usize,
}

impl BuildProgress {
    fn set(&mut self, stage: Stage, label: &str, done: usize, total: usize) {
        self.stage = stage;
        self.label.clear();
        self.label.push_str(label);
        self.done = done;
        self.total = total;
    }

    fn snapshot(&self) -> BuildSnapshot {
        BuildSnapshot {
            generation: self.generation,
            stage: self.stage.code(),
            label: self.label.clone(),
            done: self.done,
            total: self.total,
            progress: (self.units_total > 0)
                .then(|| (self.units_done.min(self.units_total) * 1000 / self.units_total) as u16),
        }
    }
}

fn shared() -> &'static Arc<Mutex<BuildProgress>> {
    static PROGRESS: OnceLock<Arc<Mutex<BuildProgress>>> = OnceLock::new();
    PROGRESS.get_or_init(|| Arc::new(Mutex::new(BuildProgress::default())))
}

fn update(report: &Mutex<BuildProgress>, generation: u64, apply: impl FnOnce(&mut BuildProgress)) {
    let mut state = report.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.generation == generation && state.stage != Stage::Idle {
        apply(&mut state);
    }
}

/// A load owns its report until preparation fails or the final lane joins.
/// An older load's scope and callbacks cannot change a newer load's report.
pub struct BuildScope {
    report: Arc<Mutex<BuildProgress>>,
    generation: u64,
}

impl BuildScope {
    fn begin(report: &Arc<Mutex<BuildProgress>>, total: usize) -> Self {
        let mut state = report.lock().unwrap_or_else(|poison| poison.into_inner());
        let generation = state.generation.wrapping_add(1);
        *state = BuildProgress {
            generation,
            stage: Stage::Reading,
            total,
            lanes: total,
            ..BuildProgress::default()
        };
        Self {
            report: Arc::clone(report),
            generation,
        }
    }

    fn update(&self, apply: impl FnOnce(&mut BuildProgress)) {
        update(&self.report, self.generation, apply);
    }

    pub fn lane(&self, done: usize, label: &str) {
        self.update(|state| state.set(Stage::Instruments, label, done, state.lanes));
    }

    pub fn stage(&self, stage: Stage) {
        self.update(|state| state.set(stage, "", 0, 0));
    }

    pub fn plan(&self, lanes: usize, units: usize) {
        self.update(|state| {
            state.lanes = lanes;
            state.total = lanes;
            state.units_total = units;
            state.units_done = 0;
        });
    }

    pub fn advance(&self, units: usize) {
        self.update(|state| state.units_done = units);
    }

    pub fn graph_report(&self, base: usize, span: usize) -> oc_engine::graph::GraphBuildReport {
        let report = Arc::clone(&self.report);
        let generation = self.generation;
        Arc::new(move |step: oc_engine::graph::GraphBuildStep| {
            let filled = (span * step.done.min(step.total))
                .checked_div(step.total)
                .unwrap_or(span);
            update(&report, generation, |state| {
                state.units_done = base + filled
            });
        })
    }

    /// Deferred lanes are hosted instruments. Only the still-pending ones
    /// count as outstanding work, including when a rebuild reuses some lanes.
    pub fn background(&self, stage: Stage, done: usize, total: usize) {
        self.update(|state| {
            let pending_units = total.saturating_sub(done) * HOSTED_INSTRUMENT_UNITS;
            state.units_done = state.units_total.saturating_sub(pending_units);
            state.set(stage, "", done, total);
        });
    }
}

impl Drop for BuildScope {
    fn drop(&mut self) {
        self.update(|state| {
            *state = BuildProgress {
                generation: self.generation,
                ..BuildProgress::default()
            };
        });
    }
}

pub fn scope(total: usize) -> BuildScope {
    BuildScope::begin(shared(), total)
}

/// Route SoundFont preparation reports into the current foreground build.
pub fn install_engine_reports() {
    oc_engine::sf2::set_report_sink(Some(Arc::new(|stage, done, total| {
        let mut state = shared().lock().unwrap_or_else(|poison| poison.into_inner());
        if !matches!(
            state.stage,
            Stage::Idle | Stage::Background | Stage::Waiting
        ) {
            state.set(Stage::SoundFont, stage, done, total);
        }
    })));
}

/// What the render thread is doing now; `None` once the build is finished.
pub fn snapshot() -> Option<BuildSnapshot> {
    let snapshot = shared()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .snapshot();
    snapshot.is_active().then_some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_names_its_stage_and_keeps_its_weighted_progress() {
        let report = Arc::new(Mutex::new(BuildProgress::default()));
        let snapshot = || report.lock().unwrap().snapshot();
        let load = BuildScope::begin(&report, 3);
        let generation = snapshot().generation;
        assert_eq!(snapshot().stage, "reading");
        assert_eq!(snapshot().progress, None);

        let foreground = HOSTED_INSTRUMENT_UNITS;
        let effects = 2 * HOSTED_EFFECT_UNITS;
        let planned = 3 * HOSTED_INSTRUMENT_UNITS + effects;
        load.plan(3, planned);
        assert_eq!(snapshot().progress, Some(0));
        load.lane(0, "Hosted Instrument");
        let lane = snapshot();
        assert_eq!(lane.stage, "instruments");
        assert_eq!(lane.label, "Hosted Instrument");
        assert_eq!((lane.done, lane.total), (0, 3));

        load.advance(foreground);
        let before_mixer = snapshot().progress;
        load.stage(Stage::Effects);
        let mixer = load.graph_report(foreground, effects);
        mixer(oc_engine::graph::GraphBuildStep { done: 1, total: 2 });
        assert!(snapshot().progress > before_mixer);
        mixer(oc_engine::graph::GraphBuildStep { done: 2, total: 2 });
        let handoff = snapshot().progress;
        assert_eq!(handoff, Some(365));

        load.background(Stage::Background, 0, 2);
        assert_eq!(snapshot().stage, "background");
        assert_eq!(snapshot().progress, handoff);
        assert_eq!(snapshot().generation, generation);
        load.background(Stage::Waiting, 1, 2);
        assert_eq!(snapshot().stage, "waiting");
        assert_eq!((snapshot().done, snapshot().total), (1, 2));
        assert_eq!(snapshot().progress, Some(682));
        load.background(Stage::Background, 2, 2);
        assert_eq!(snapshot().progress, Some(1000));
        drop(load);
        assert!(!snapshot().is_active());
    }

    #[test]
    fn a_replacement_reports_pending_work_and_ignores_the_old_load() {
        let report = Arc::new(Mutex::new(BuildProgress::default()));
        let snapshot = || report.lock().unwrap().snapshot();
        let old = BuildScope::begin(&report, 3);
        let old_mixer = old.graph_report(0, 10);
        let old_generation = snapshot().generation;
        let load = BuildScope::begin(&report, 3);
        assert_ne!(snapshot().generation, old_generation);
        load.plan(3, HOSTED_EFFECT_UNITS + HOSTED_INSTRUMENT_UNITS);
        load.advance(HOSTED_EFFECT_UNITS);
        let handoff = snapshot().progress;
        old_mixer(oc_engine::graph::GraphBuildStep { done: 1, total: 1 });
        drop(old);
        load.background(Stage::Background, 1, 2);
        assert_eq!(handoff, Some(69));
        assert_eq!(snapshot().progress, handoff);
    }
}
