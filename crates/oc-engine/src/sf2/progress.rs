//! Optional progress reports from SoundFont preparation.
//!
//! A GUI installs a sink so a multi-second bank preparation can name its stage
//! and count the samples it has decoded. Without a sink every report is a
//! no-op, so offline renders and tests pay nothing.

use std::sync::{Arc, Mutex, OnceLock};

/// Receives `(stage, done, total)`; `total` is zero when the stage has no count.
pub type Report = Arc<dyn Fn(&'static str, usize, usize) + Send + Sync>;

fn sink() -> &'static Mutex<Option<Report>> {
    static SINK: OnceLock<Mutex<Option<Report>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Install the process-wide receiver; `None` silences reports.
pub fn set_report_sink(report: Option<Report>) {
    *sink().lock().unwrap_or_else(|poison| poison.into_inner()) = report;
}

/// Report one stage of a preparation. The sink runs outside the global lock, so
/// a slow receiver cannot stall another bank's preparer.
pub(crate) fn report(stage: &'static str, done: usize, total: usize) {
    let receiver = sink()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    if let Some(receiver) = receiver {
        receiver(stage, done, total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_sink_receives_each_stage_and_silence_costs_nothing() {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        set_report_sink(Some(Arc::new(move |stage, done, total| {
            assert!(!stage.is_empty());
            assert!(total >= done);
            counter.fetch_add(1, Ordering::Relaxed);
        })));
        report("decode", 1, 4);
        report("decode", 4, 4);
        assert_eq!(seen.load(Ordering::Relaxed), 2);

        set_report_sink(None);
        report("decode", 4, 4);
        assert_eq!(seen.load(Ordering::Relaxed), 2);
    }
}
