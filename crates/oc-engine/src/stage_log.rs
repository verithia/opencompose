//! Opt-in stage timing for offline renders.
//!
//! A caller brackets a render with [`begin`] and [`take`]. Stages recorded on a
//! render worker use [`push_worker`]; the calling thread uses [`push`] and
//! [`stage`]. When no collection is active every entry point is a no-op, so the
//! playback path pays only one thread-local read per stage and no clock read.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// One labelled stage of a render, in completion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageTiming {
    pub label: &'static str,
    pub track: Option<usize>,
    pub elapsed: Duration,
}

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

static COLLECTING: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Vec<StageTiming>> = Mutex::new(Vec::new());

/// Start collecting stage timings, discarding an earlier table.
pub fn begin() {
    SINK.lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
    ACTIVE.set(true);
    COLLECTING.store(true, Ordering::Relaxed);
}

/// Stop collecting and return the table collected since [`begin`].
pub fn take() -> Vec<StageTiming> {
    ACTIVE.set(false);
    COLLECTING.store(false, Ordering::Relaxed);
    std::mem::take(&mut *SINK.lock().unwrap_or_else(|poison| poison.into_inner()))
}

fn record(label: &'static str, track: Option<usize>, elapsed: Duration) {
    SINK.lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(StageTiming {
            label,
            track,
            elapsed,
        });
}

/// Record a stage that has already finished on the collecting thread.
pub fn push(label: &'static str, started: Instant) {
    if !ACTIVE.get() {
        return;
    }
    record(label, None, started.elapsed());
}

/// Record a stage that finished on a render worker.
pub fn push_worker(label: &'static str, started: Instant) {
    if !COLLECTING.load(Ordering::Relaxed) {
        return;
    }
    record(label, None, started.elapsed());
}

/// Record a worker stage for one track or bus.
pub fn push_worker_track(label: &'static str, track: usize, started: Instant) {
    if !COLLECTING.load(Ordering::Relaxed) {
        return;
    }
    record(label, Some(track), started.elapsed());
}

/// A stage that records itself when it leaves scope, including on `?`.
///
/// Inactive collection returns a guard with no clock reading.
pub struct Stage(Option<(&'static str, Instant)>);

impl Drop for Stage {
    fn drop(&mut self) {
        if let Some((label, started)) = self.0 {
            push(label, started);
        }
    }
}

/// Time the enclosing scope under `label` when collection is active.
pub fn stage(label: &'static str) -> Stage {
    Stage(ACTIVE.get().then(|| (label, Instant::now())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `begin`/`take` drive one process-global table, so the tests here must not
    /// hold overlapping collection windows.
    static TEST_TABLE_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_TABLE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn inactive_collection_records_nothing() {
        let _serial = serial();
        let _stage = stage("ignored");
        push_worker("ignored", Instant::now());
        assert!(take().is_empty());
    }

    #[test]
    fn active_collection_records_named_stages_in_order() {
        let _serial = serial();
        begin();
        let window = Instant::now();
        {
            let _first = stage("first");
            let _second = stage("second");
        }
        let stages = take();
        assert_eq!(
            stages.iter().map(|stage| stage.label).collect::<Vec<_>>(),
            ["second", "first"]
        );
        // A stage cannot outlast the collection window, but may measure zero.
        assert!(stages.iter().all(|stage| stage.elapsed <= window.elapsed()));
        assert!(take().is_empty(), "take must clear the table");
    }

    #[test]
    fn worker_stages_land_in_the_same_table() {
        let _serial = serial();
        begin();
        std::thread::scope(|scope| {
            scope.spawn(|| push_worker("worker", Instant::now()));
        });
        let stages = take();
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].label, "worker");
        assert_eq!(stages[0].track, None);
    }
}
