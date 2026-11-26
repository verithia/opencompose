//! Offline library loading finishes before authored time and MIDI begin.

use crate::render_cancel::CancelToken;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct WarmupOptions<'a> {
    pub seconds: f64,
    pub cancel: Option<&'a CancelToken>,
}

impl WarmupOptions<'_> {
    fn check_cancel(self) -> Result<(), String> {
        if crate::render_cancel::stopped(self.cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        Ok(())
    }
}

pub(super) fn wait(options: WarmupOptions<'_>) -> Result<(), String> {
    wait_with_service(options, service_main)
}

fn service_main() -> Result<(), String> {
    if oc_host::application_main::offline_main_is_active() {
        oc_host::application_main::service_offline_main()?;
    }
    Ok(())
}

/// The minimum loading interval starts after this instance's state is prepared.
/// Preparing another instance does not restart an existing deadline.
#[derive(Clone, Copy)]
pub(super) struct WarmupDeadline(Instant);

impl WarmupDeadline {
    pub(super) fn after_preparation(seconds: f64) -> Result<Self, String> {
        Self::at(seconds, Instant::now())
    }

    fn at(seconds: f64, prepared: Instant) -> Result<Self, String> {
        if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
            return Err("Plugin pre-roll must be finite and within 0..600 seconds".into());
        }
        Ok(Self(prepared + Duration::from_secs_f64(seconds)))
    }
}

pub(super) fn wait_until(
    deadline: WarmupDeadline,
    cancel: Option<&CancelToken>,
) -> Result<(), String> {
    wait_until_with_clock(
        deadline,
        cancel,
        service_main,
        Instant::now,
        std::thread::sleep,
    )
}

fn wait_until_with_clock(
    deadline: WarmupDeadline,
    cancel: Option<&CancelToken>,
    mut service: impl FnMut() -> Result<(), String>,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<(), String> {
    loop {
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        let remaining = deadline.0.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(());
        }
        service()?;
        sleep(remaining.min(Duration::from_millis(5)));
    }
}

fn wait_with_service(
    options: WarmupOptions<'_>,
    service: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let deadline = WarmupDeadline::after_preparation(options.seconds)?;
    wait_until_with_clock(
        deadline,
        options.cancel,
        service,
        Instant::now,
        std::thread::sleep,
    )
}

#[cfg(any(target_os = "macos", test))]
trait Warmup {
    fn prime(&mut self) -> Result<(), String>;
    fn wait(&mut self, options: WarmupOptions<'_>) -> Result<(), String>;
    fn reset(&mut self) -> Result<(), String>;
}

#[cfg(any(target_os = "macos", test))]
fn prepare(unit: &mut impl Warmup, options: WarmupOptions<'_>) -> Result<(), String> {
    let seconds = options.seconds;
    if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
        return Err("Plugin pre-roll must be finite and within 0..600 seconds".into());
    }
    options.check_cancel()?;
    if seconds > 0.0 {
        unit.prime()?;
        options.check_cancel()?;
        unit.wait(options)?;
        options.check_cancel()?;
        unit.reset()?;
    }
    options.check_cancel()
}

#[cfg(target_os = "macos")]
impl Warmup for oc_host::au_instance::AuInstance {
    fn prime(&mut self) -> Result<(), String> {
        // Some sample loaders accept restored state on the first audio call.
        // Discard its silent block before waiting, without submitting any MIDI.
        self.process_block_into(&mut [0.0; 256], None)
            .map_err(|error| error.to_string())
    }

    fn wait(&mut self, options: WarmupOptions<'_>) -> Result<(), String> {
        wait(options)
    }

    fn reset(&mut self) -> Result<(), String> {
        self.reset_prepared().map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn prepare_au(
    instance: &mut oc_host::au_instance::AuInstance,
    options: WarmupOptions<'_>,
) -> Result<(), String> {
    prepare(instance, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_deadlines_overlap_without_shortening_loading_or_restarting_waits() {
        let start = Instant::now();
        let elapsed = std::cell::Cell::new(Duration::from_millis(300));
        let services = std::cell::Cell::new(0usize);
        for prepared_ms in [0, 100, 200] {
            let deadline =
                WarmupDeadline::at(4.0, start + Duration::from_millis(prepared_ms)).unwrap();
            wait_until_with_clock(
                deadline,
                None,
                || {
                    services.set(services.get() + 1);
                    Ok(())
                },
                || start + elapsed.get(),
                |duration| elapsed.set(elapsed.get() + duration),
            )
            .unwrap();
            assert!(elapsed.get() >= Duration::from_millis(prepared_ms + 4000));
        }
        assert_eq!(elapsed.get(), Duration::from_millis(4200));
        assert!(services.get() > 0);
        for seconds in [f64::NAN, -1.0, 600.1] {
            assert!(WarmupDeadline::at(seconds, start).is_err());
        }
    }

    #[test]
    fn expired_deadline_still_observes_cancel_before_processing() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let deadline = WarmupDeadline::at(0.0, Instant::now()).unwrap();
        assert_eq!(
            wait_until(deadline, Some(&cancel)),
            Err(crate::render_cancel::cancelled())
        );
    }

    #[test]
    fn cancelled_preparation_never_primes_even_with_zero_warmup() {
        let cancel = CancelToken::new();
        cancel.cancel();
        for seconds in [0.0, 60.0] {
            let mut library = AsyncLibrary::default();
            let options = WarmupOptions {
                seconds,
                cancel: Some(&cancel),
            };
            assert_eq!(
                prepare(&mut library, options),
                Err(crate::render_cancel::cancelled())
            );
            assert_eq!(wait(options), Err(crate::render_cancel::cancelled()));
            assert!(library.steps.is_empty());
        }
    }

    #[test]
    fn cancellation_interrupts_an_in_progress_wait() {
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut ready = Some(ready_tx);
            let result = wait_with_service(
                WarmupOptions {
                    seconds: 60.0,
                    cancel: Some(&worker_cancel),
                },
                || {
                    if let Some(ready) = ready.take() {
                        ready.send(()).unwrap();
                        resume_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                    }
                    Ok(())
                },
            );
            let _ = result_tx.send(result);
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel();
        resume_tx.send(()).unwrap();
        let result = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation must not wait for the remaining warmup duration");
        worker.join().unwrap();
        assert_eq!(result, Err(crate::render_cancel::cancelled()));
    }

    #[test]
    fn cancellation_during_native_preparation_stops_later_steps() {
        struct CancellingLibrary<'a> {
            cancel: &'a CancelToken,
            cancel_on: &'static str,
            steps: Vec<&'static str>,
        }
        impl Warmup for CancellingLibrary<'_> {
            fn prime(&mut self) -> Result<(), String> {
                self.steps.push("prime");
                if self.cancel_on == "prime" {
                    self.cancel.cancel();
                }
                Ok(())
            }
            fn wait(&mut self, _: WarmupOptions<'_>) -> Result<(), String> {
                self.steps.push("wait");
                self.cancel.cancel();
                Ok(())
            }
            fn reset(&mut self) -> Result<(), String> {
                panic!("cancelled preparation must not enter reset")
            }
        }
        for (cancel_on, expected) in [("prime", vec!["prime"]), ("wait", vec!["prime", "wait"])] {
            let cancel = CancelToken::new();
            let mut library = CancellingLibrary {
                cancel: &cancel,
                cancel_on,
                steps: Vec::new(),
            };
            assert_eq!(
                prepare(
                    &mut library,
                    WarmupOptions {
                        seconds: 60.0,
                        cancel: Some(&cancel)
                    }
                ),
                Err(crate::render_cancel::cancelled())
            );
            assert_eq!(library.steps, expected);
        }
    }

    #[test]
    fn invalid_wait_duration_is_rejected_before_native_service() {
        for seconds in [-1.0, f64::NAN, f64::INFINITY, 600.1] {
            assert!(
                wait(WarmupOptions {
                    seconds,
                    cancel: None
                })
                .is_err()
            );
        }
        wait(WarmupOptions {
            seconds: 0.0,
            cancel: None,
        })
        .unwrap();
    }

    #[derive(Default)]
    struct AsyncLibrary {
        steps: Vec<&'static str>,
        ready: bool,
        position: usize,
        fail: Option<&'static str>,
    }

    impl Warmup for AsyncLibrary {
        fn prime(&mut self) -> Result<(), String> {
            self.steps.push("prime");
            self.position = 128;
            (self.fail != Some("prime"))
                .then_some(())
                .ok_or("prime failed".into())
        }
        fn wait(&mut self, _: WarmupOptions<'_>) -> Result<(), String> {
            assert_eq!(self.steps, ["prime"]);
            self.steps.push("wait");
            self.ready = true;
            (self.fail != Some("wait"))
                .then_some(())
                .ok_or("wait failed".into())
        }
        fn reset(&mut self) -> Result<(), String> {
            self.steps.push("reset");
            self.position = 0;
            (self.fail != Some("reset"))
                .then_some(())
                .ok_or("reset failed".into())
        }
    }

    #[test]
    fn async_state_starts_loading_before_the_wait_and_keeps_authored_time_zero() {
        let mut library = AsyncLibrary::default();
        prepare(
            &mut library,
            WarmupOptions {
                seconds: 0.01,
                cancel: None,
            },
        )
        .unwrap();
        assert!(library.ready);
        assert_eq!(library.position, 0);
        assert_eq!(library.steps, ["prime", "wait", "reset"]);
    }

    #[test]
    fn disabled_or_invalid_warmup_never_advances_the_instrument() {
        for seconds in [0.0, -1.0, f64::NAN, f64::INFINITY, 600.1] {
            let mut library = AsyncLibrary::default();
            assert_eq!(
                prepare(
                    &mut library,
                    WarmupOptions {
                        seconds,
                        cancel: None
                    }
                )
                .is_ok(),
                seconds == 0.0
            );
            assert!(library.steps.is_empty());
        }
    }

    #[test]
    fn failed_preparation_stops_before_later_native_steps() {
        for (failure, steps) in [
            ("prime", vec!["prime"]),
            ("wait", vec!["prime", "wait"]),
            ("reset", vec!["prime", "wait", "reset"]),
        ] {
            let mut library = AsyncLibrary {
                fail: Some(failure),
                ..Default::default()
            };
            assert!(
                prepare(
                    &mut library,
                    WarmupOptions {
                        seconds: 1.0,
                        cancel: None
                    }
                )
                .is_err()
            );
            assert_eq!(library.steps, steps);
        }
    }
}
