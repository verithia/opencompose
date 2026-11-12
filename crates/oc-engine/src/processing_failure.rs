//! Offline publication boundary for hosted source and graph failures.
//!
//! Native callbacks publish a bounded immutable first-fault record. This owner
//! waits only outside DSP, with a deadline and cancellation, and never treats
//! an in-progress or previously claimed diagnostic as successful audio.

use crate::graph::{AudioGraph, EffectChain};
use crate::render_cancel::CancelToken;
use oc_audio::{AudioEffect, ProcessingFailureReport};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OfflineProcessingError {
    Diagnostic(String),
    Pending { cancelled: bool },
    Reported,
    Unavailable,
}

impl std::fmt::Display for OfflineProcessingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostic(message) => formatter.write_str(message),
            Self::Pending { cancelled: false } => formatter.write_str(
                "Render refused: processor failure diagnostic is still pending publication",
            ),
            Self::Pending { cancelled: true } => formatter.write_str(
                "Render cancelled while processor failure diagnostic was pending publication",
            ),
            Self::Reported => formatter.write_str(
                "Render refused: a processor failed and its diagnostic was already reported",
            ),
            Self::Unavailable => formatter.write_str(
                "Render refused: an audio processor failed without a detailed diagnostic",
            ),
        }
    }
}
impl std::error::Error for OfflineProcessingError {}

fn verify(
    failed: bool,
    report: impl Fn() -> ProcessingFailureReport,
    cancel: Option<&CancelToken>,
) -> Result<(), OfflineProcessingError> {
    if !failed {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(25);
    loop {
        match report() {
            ProcessingFailureReport::Ready(message) => {
                return Err(OfflineProcessingError::Diagnostic(message));
            }
            ProcessingFailureReport::Reported => return Err(OfflineProcessingError::Reported),
            ProcessingFailureReport::Unavailable => {
                return Err(OfflineProcessingError::Unavailable);
            }
            ProcessingFailureReport::Pending => {
                let cancelled = cancel.is_some_and(CancelToken::is_cancelled);
                if cancelled || std::time::Instant::now() >= deadline {
                    return Err(OfflineProcessingError::Pending { cancelled });
                }
                std::thread::yield_now();
            }
        }
    }
}

pub(crate) fn effect(
    effect: &dyn AudioEffect,
    cancel: Option<&CancelToken>,
) -> Result<(), OfflineProcessingError> {
    verify(
        effect.processing_failed(),
        || effect.take_processing_failure(),
        cancel,
    )
}
/// Format an owned VST2 refusal outside processing, preserving pending native
/// first-cause publication through the shared offline readiness boundary.
pub(crate) fn vst2_owned(
    error: &oc_host::vst2::Vst2OwnedError,
    cancel: Option<&CancelToken>,
) -> String {
    if let oc_host::vst2::Vst2OwnedError::Faulted(failure) = error {
        return verify(
            true,
            || {
                if failure.first_fault().is_none() {
                    ProcessingFailureReport::Pending
                } else if let Some(record) = failure.take_report() {
                    ProcessingFailureReport::Ready(format!(
                        "VST2 {:?}: {}",
                        record.operation,
                        record.fault.describe_for("VST2")
                    ))
                } else {
                    ProcessingFailureReport::Reported
                }
            },
            cancel,
        )
        .unwrap_err()
        .to_string();
    }
    error.to_string()
}

pub(crate) fn chain(
    chain: &EffectChain,
    cancel: Option<&CancelToken>,
) -> Result<(), OfflineProcessingError> {
    verify(
        chain.processing_failed(),
        || chain.take_processing_failure(),
        cancel,
    )
}
pub(crate) fn graph(
    graph: &AudioGraph,
    cancel: Option<&CancelToken>,
) -> Result<(), OfflineProcessingError> {
    verify(
        graph.processing_failed(),
        || graph.take_processing_failure(),
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Deferred(AtomicUsize);
    impl AudioEffect for Deferred {
        fn process(&mut self, _: &mut [f32], _: u32) {
            panic!("failed audio must not run");
        }
        fn processing_failed(&self) -> bool {
            true
        }
        fn take_processing_failure(&self) -> ProcessingFailureReport {
            match self.0.fetch_add(1, Ordering::Relaxed) {
                0 => ProcessingFailureReport::Pending,
                1 => ProcessingFailureReport::Ready("native process: original fault".into()),
                _ => ProcessingFailureReport::Reported,
            }
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "deferred diagnostic"
        }
    }

    #[test]
    fn offline_failure_boundary_waits_for_original_cause_and_distinguishes_claimed() {
        let instance = Deferred(AtomicUsize::new(0));
        assert_eq!(
            effect(&instance, None),
            Err(OfflineProcessingError::Diagnostic(
                "native process: original fault".into()
            ))
        );
        assert_eq!(
            effect(&instance, None),
            Err(OfflineProcessingError::Reported)
        );
    }

    #[test]
    fn offline_failure_boundary_preserves_pending_on_cancellation_and_deadline() {
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            verify(true, || ProcessingFailureReport::Pending, Some(&cancel)),
            Err(OfflineProcessingError::Pending { cancelled: true })
        );
        assert_eq!(
            verify(true, || ProcessingFailureReport::Pending, None),
            Err(OfflineProcessingError::Pending { cancelled: false })
        );
        assert_eq!(
            verify(true, || ProcessingFailureReport::Unavailable, None),
            Err(OfflineProcessingError::Unavailable)
        );
        assert_eq!(
            verify(
                false,
                || panic!("healthy processor must not consume failure state"),
                None
            ),
            Ok(())
        );
    }
}
