//! Bounded preparation cohorts overlap loading, then render exclusive processors.

use super::offline_warmup::{WarmupDeadline, wait_until};
use super::vst2_group::Vst2GroupOutput;
use crate::render_cancel::CancelToken;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_PREPARED_GROUPS: usize = 4;

pub(super) fn capacity(workers: usize) -> usize {
    workers.clamp(1, MAX_PREPARED_GROUPS)
}

type RenderPrepared = Box<dyn FnOnce(&CancelToken) -> Result<Vst2GroupOutput, String> + Send>;

/// Native state and numeric schedules are complete before this work is created.
pub(super) struct PreparedGroup {
    deadline: WarmupDeadline,
    render: RenderPrepared,
    native_retirement: bool,
}

impl PreparedGroup {
    pub(super) fn new(deadline: WarmupDeadline, render: RenderPrepared) -> Self {
        Self {
            deadline,
            render,
            native_retirement: false,
        }
    }

    pub(super) fn native(deadline: WarmupDeadline, render: RenderPrepared) -> Self {
        Self {
            deadline,
            render,
            native_retirement: true,
        }
    }

    pub(super) fn completed(output: Vst2GroupOutput) -> Result<Self, String> {
        Ok(Self::new(
            WarmupDeadline::after_preparation(0.0)?,
            Box::new(move |_| Ok(output)),
        ))
    }
}

/// All native initialization stays on its UI owner. Only existing transferable
/// exclusive processing leases enter these workers; results retain source order.
pub(super) fn render(
    prepared: Vec<PreparedGroup>,
    cancel: Option<&CancelToken>,
) -> Result<Vec<Vst2GroupOutput>, String> {
    render_with_retirement(prepared, cancel, retire_native)
}

fn render_with_retirement(
    prepared: Vec<PreparedGroup>,
    cancel: Option<&CancelToken>,
    retire: impl FnOnce() -> Result<(), String>,
) -> Result<Vec<Vst2GroupOutput>, String> {
    let native_retirement = prepared.iter().any(|group| group.native_retirement);
    let result = render_inner(prepared, cancel);
    if native_retirement {
        retire()?;
    }
    result
}

/// Drain retired leases on their existing UI owner before preparing a new cohort.
pub(super) fn retire_native() -> Result<(), String> {
    oc_host::application_main::ApplicationMainExecutor::registered()?
        .dispatch_control(|| {
            oc_host::vst2::poll_owned_vst2_retirements_checked()
                .map(|_| ())
                .map_err(|error| error.to_string())
        })?
        .wait()
}

pub(super) fn discard(prepared: Vec<PreparedGroup>) -> Result<(), String> {
    let native_retirement = prepared.iter().any(|group| group.native_retirement);
    drop(prepared);
    if native_retirement {
        retire_native()?;
    }
    Ok(())
}

fn render_inner(
    prepared: Vec<PreparedGroup>,
    cancel: Option<&CancelToken>,
) -> Result<Vec<Vst2GroupOutput>, String> {
    if prepared.len() > MAX_PREPARED_GROUPS {
        return Err("VST2 preparation cohort exceeds its instance limit".into());
    }
    {
        let _timer = oc_diag::perf_counter("cache.vst2.readiness.ms").map(oc_diag::Timer::start);
        for group in &prepared {
            wait_until(group.deadline, cancel)?;
        }
    }
    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    let stop = CancelToken::new();
    let _timer = oc_diag::perf_counter("cache.vst2.processing.ms").map(oc_diag::Timer::start);
    let first_failure = Arc::new(AtomicUsize::new(usize::MAX));
    let mut service_error = None;
    let results = std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(prepared.len());
        for (index, group) in prepared.into_iter().enumerate() {
            let worker_stop = stop.clone();
            let first_failure = first_failure.clone();
            workers.push(scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (group.render)(&worker_stop)
                }))
                .unwrap_or_else(|_| Err("VST2 source worker panicked".into()));
                if result.is_err() && !worker_stop.is_cancelled() {
                    let _ = first_failure.compare_exchange(
                        usize::MAX,
                        index,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    worker_stop.cancel();
                }
                result
            }));
        }
        let mut results = Vec::with_capacity(workers.len());
        for worker in workers {
            while !worker.is_finished() {
                if crate::render_cancel::stopped(cancel) {
                    stop.cancel();
                }
                if oc_host::application_main::offline_main_is_active()
                    && let Err(error) = oc_host::application_main::service_offline_main()
                {
                    stop.cancel();
                    if service_error.is_none() {
                        service_error = Some(error);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            results.push(
                worker
                    .join()
                    .unwrap_or_else(|_| Err("VST2 source worker panicked".into())),
            );
        }
        results
    });
    if let Some(error) = service_error {
        return Err(error);
    }
    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    let failed = first_failure.load(Ordering::Acquire);
    if failed < results.len() {
        return Err(results.into_iter().nth(failed).unwrap().err().unwrap());
    }
    results.into_iter().collect()
}

#[cfg(test)]
mod tests;
