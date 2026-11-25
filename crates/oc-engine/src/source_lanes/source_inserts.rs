//! Bounded offline processing of independent source-owned insert chains.
use crate::graph::EffectChain;
use crate::render_cancel::CancelToken;
use crate::render_parallel::RenderWorkerOptions;
use oc_audio::PluginTransport;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct SourceInsertLanes<'a> {
    pub chains: &'a mut [EffectChain],
    pub buffers: &'a mut [Option<Vec<f32>>],
    pub latencies: &'a mut [u32],
    /// Checked latency plus the existing source-tail owner's bounded release.
    pub extra_frames: &'a [u64],
}
pub(crate) struct SourceInsertOptions<'a> {
    pub sample_rate: u32,
    pub transport: &'a PluginTransport,
    pub cancel: Option<&'a CancelToken>,
    pub workers: RenderWorkerOptions,
}
struct Job<'a> {
    lane: usize,
    chain: &'a mut EffectChain,
    buffer: &'a mut Vec<f32>,
    latency: &'a mut u32,
    extra_frames: u64,
}
struct Failure {
    lane: usize,
    error: String,
}

fn process(job: Job<'_>, options: &SourceInsertOptions<'_>) -> Result<(), String> {
    if crate::render_cancel::stopped(options.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    crate::processing_failure::chain(job.chain, options.cancel).map_err(|e| e.to_string())?;
    let extra = usize::try_from(job.extra_frames)
        .ok()
        .and_then(|frames| frames.checked_mul(2))
        .and_then(|samples| job.buffer.len().checked_add(samples))
        .ok_or("Source insert render length overflow")?;
    job.buffer.resize(extra, 0.0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        job.chain.process_with_transport(
            job.buffer,
            options.sample_rate,
            Some(options.transport.clone()),
        );
    }));
    // Native audio-thread state is released by the thread that processed it,
    // including refusal and Rust-panic paths.
    job.chain.end_processing();
    crate::processing_failure::chain(job.chain, options.cancel).map_err(|e| e.to_string())?;
    if result.is_err() {
        return Err("Source insert chain panicked".into());
    }
    if crate::render_cancel::stopped(options.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    *job.latency = job.latency.saturating_add(job.chain.latency_samples());
    Ok(())
}

/// Only disjoint source inserts run concurrently. The caller retains ordered
/// source alignment/folding and publishes no audio when this phase refuses.
pub(crate) fn process_source_inserts(
    lanes: SourceInsertLanes<'_>,
    options: SourceInsertOptions<'_>,
) -> Result<(), String> {
    let count = lanes.chains.len();
    if lanes.buffers.len() != count
        || lanes.latencies.len() != count
        || lanes.extra_frames.len() != count
    {
        return Err("Source insert lane count mismatch".into());
    }
    if crate::render_cancel::stopped(options.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    // Preserve source-index ordering for failures already known at admission.
    for (chain, buffer) in lanes.chains.iter().zip(lanes.buffers.iter()) {
        if buffer.is_some() {
            crate::processing_failure::chain(chain, options.cancel).map_err(|e| e.to_string())?;
        }
    }
    let dependent = lanes
        .chains
        .iter()
        .any(|c| c.has_sidechain() || c.has_control_inputs());
    let worker_count = if dependent {
        1
    } else {
        options.workers.workers()
    };
    let jobs = lanes
        .chains
        .iter_mut()
        .zip(lanes.buffers.iter_mut())
        .zip(lanes.latencies.iter_mut())
        .zip(lanes.extra_frames)
        .enumerate()
        .filter_map(|(lane, (((chain, buffer), latency), &extra_frames))| {
            buffer.as_mut().map(|buffer| Job {
                lane,
                chain,
                buffer,
                latency,
                extra_frames,
            })
        });
    if worker_count == 1 {
        for job in jobs {
            process(job, &options)?;
        }
        return Ok(());
    }
    let pending: VecDeque<_> = jobs.collect();
    if pending.is_empty() {
        return Ok(());
    }
    let workers = worker_count.min(pending.len());
    let queue = Mutex::new(pending);
    let stop = AtomicBool::new(false);
    let mut service_error = None;
    let failures = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let queue = &queue;
            let stop = &stop;
            let options = &options;
            handles.push(scope.spawn(move || {
                loop {
                    if stop.load(Ordering::Acquire) || crate::render_cancel::stopped(options.cancel)
                    {
                        return None;
                    }
                    // No plugin callback runs under this offline work-queue lock.
                    let next = match queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => {
                            stop.store(true, Ordering::Release);
                            return Some(Failure {
                                lane: usize::MAX,
                                error: "Source insert queue poisoned".into(),
                            });
                        }
                    };
                    let Some(job) = next else {
                        return None;
                    };
                    let lane = job.lane;
                    if let Err(error) = process(job, options) {
                        stop.store(true, Ordering::Release);
                        return Some(Failure { lane, error });
                    }
                }
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            while !handle.is_finished() {
                if crate::render_cancel::stopped(options.cancel) {
                    stop.store(true, Ordering::Release);
                }
                if oc_host::application_main::offline_main_is_active()
                    && let Err(error) = oc_host::application_main::service_offline_main()
                {
                    stop.store(true, Ordering::Release);
                    if service_error.is_none() {
                        service_error = Some(error);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            match handle.join() {
                Ok(Some(error)) => failures.push(error),
                Ok(None) => {}
                Err(_) => failures.push(Failure {
                    lane: usize::MAX,
                    error: "Source insert worker panicked".into(),
                }),
            }
        }
        failures
    });
    if crate::render_cancel::stopped(options.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    if let Some(failure) = failures.into_iter().min_by_key(|f| f.lane) {
        return Err(failure.error);
    }
    if let Some(error) = service_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
