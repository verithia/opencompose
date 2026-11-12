//! One output stream that the audio-IO owner rebuilds after a device change.
//!
//! The slot keeps the callback factory of its current owner and the sink of the
//! open stream. A sweep asks every registered slot to rebuild; a slot that is
//! closed stays closed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use oc_audio::audio_io::{
    AudioIo, ManagedStream, RenderSink, RenderSource, StreamSink, StreamSpec,
};

/// One callback at a time may access shared audio rings, including during
/// device replacement when the old and candidate streams overlap.
#[derive(Default)]
pub(crate) struct CallbackRoute {
    next: AtomicU64,
    owner: AtomicU64,
    filling: AtomicBool,
    transition: Mutex<()>,
}

impl CallbackRoute {
    pub(crate) fn wrap(self: &Arc<Self>, source: Arc<dyn RenderSource>) -> Arc<dyn RenderSource> {
        Arc::new(RoutedSource {
            route: Arc::clone(self),
            source,
        })
    }

    fn bind(self: &Arc<Self>, sink: Arc<dyn RenderSink>) -> Arc<dyn RenderSink> {
        Arc::new(RoutedCallback {
            permit: self.register(),
            sink,
        })
    }

    pub(crate) fn register(self: &Arc<Self>) -> CallbackPermit {
        CallbackPermit {
            route: Arc::clone(self),
            id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
            failed: AtomicBool::new(false),
            opened: AtomicBool::new(false),
            early_error: Mutex::new(None),
        }
    }

    pub(crate) fn retire_current(&self) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        self.owner.store(0, Ordering::Release);
    }

    pub(crate) fn has_owner(&self) -> bool {
        self.owner.load(Ordering::Acquire) != 0
    }
}

struct RoutedSource {
    route: Arc<CallbackRoute>,
    source: Arc<dyn RenderSource>,
}

impl RenderSource for RoutedSource {
    fn create(&self, spec: &StreamSpec) -> Arc<dyn RenderSink> {
        self.route.bind(self.source.create(spec))
    }
}

struct RoutedCallback {
    permit: CallbackPermit,
    sink: Arc<dyn RenderSink>,
}

pub(crate) struct CallbackPermit {
    route: Arc<CallbackRoute>,
    id: u64,
    failed: AtomicBool,
    opened: AtomicBool,
    early_error: Mutex<Option<String>>,
}

pub(crate) struct FillGuard<'a>(&'a AtomicBool);

impl Drop for FillGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl CallbackPermit {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn activate(&self, opened: impl FnOnce(), report_failure: impl FnOnce(String)) {
        let _transition = self
            .route
            .transition
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        opened();
        if let Some(error) = self
            .early_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            report_failure(error);
        }
        self.opened.store(true, Ordering::Relaxed);
        self.route.owner.store(self.id, Ordering::Release);
    }

    pub(crate) fn enter(&self) -> Option<FillGuard<'_>> {
        let current = || {
            self.route.owner.load(Ordering::Acquire) == self.id
                && !self.failed.load(Ordering::Acquire)
        };
        if !current() || self.route.filling.swap(true, Ordering::AcqRel) {
            return None;
        }
        let guard = FillGuard(&self.route.filling);
        // A replacement may have been accepted since the first ownership check.
        current().then_some(guard)
    }

    pub(crate) fn report_failure(&self, error: String, report: impl FnOnce(String)) {
        let _transition = self
            .route
            .transition
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.failed.store(true, Ordering::Release);
        let owner = self.route.owner.load(Ordering::Acquire);
        if owner == self.id {
            report(error);
        } else if !self.opened.load(Ordering::Relaxed) {
            *self.early_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        }
    }
}

impl RenderSink for RoutedCallback {
    fn opened(&self) {
        self.opened_at(0);
    }

    fn opened_at(&self, sample_rate: u32) {
        self.permit.activate(
            || self.sink.opened_at(sample_rate),
            |error| self.sink.failed(error),
        );
    }

    fn fill(&self, output: &mut [f32]) {
        if let Some(_guard) = self.permit.enter() {
            self.sink.fill(output);
        } else {
            output.fill(0.0);
        }
    }

    fn failed(&self, error: String) {
        self.permit
            .report_failure(error, |error| self.sink.failed(error));
    }
}

/// A named stream with the callback factory of its current owner.
pub(crate) struct OutputSlot {
    role: &'static str,
    io: Arc<AudioIo>,
    state: Mutex<SlotState>,
}

#[derive(Default)]
struct SlotState {
    sink: Option<StreamSink>,
    source: Option<Arc<dyn RenderSource>>,
}

impl OutputSlot {
    pub(crate) fn new(role: &'static str, io: Arc<AudioIo>) -> Self {
        Self {
            role,
            io,
            state: Mutex::new(SlotState::default()),
        }
    }

    /// Open on the current format, replacing whatever was open.
    pub(crate) fn open(&self, source: Arc<dyn RenderSource>) -> Result<(), String> {
        let sink = self.io.open_stream(Arc::clone(&source))?;
        self.install(sink, source);
        Ok(())
    }

    /// Take an already opened sink and remember the factory that built it.
    pub(crate) fn install(&self, sink: StreamSink, source: Arc<dyn RenderSource>) {
        let mut state = self.lock();
        let previous = state.sink.replace(sink);
        state.source = Some(source);
        drop(state);
        drop(previous);
    }

    pub(crate) fn close(&self) {
        let previous = self.lock().sink.take();
        drop(previous);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl ManagedStream for OutputSlot {
    fn role(&self) -> &'static str {
        self.role
    }

    fn is_open(&self) -> bool {
        self.lock().sink.is_some()
    }

    fn reopen(&self, spec: &StreamSpec) -> Result<(), String> {
        let Some(source) = self.lock().source.clone() else {
            return Ok(());
        };
        let callback = source.create(spec);
        let sink = self.io.backend().open_output(spec, Arc::clone(&callback))?;
        let mut state = self.lock();
        if state.sink.is_none()
            || !state
                .source
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &source))
        {
            return Ok(());
        }
        let rate = sink.sample_rate;
        let previous = state.sink.replace(sink);
        callback.opened_at(rate);
        drop(state);
        drop(previous);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Default)]
    struct Probe {
        value: f32,
        errors: Mutex<Vec<String>>,
        block: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    }

    impl RenderSink for Probe {
        fn fill(&self, output: &mut [f32]) {
            if let Some((entered, release)) = &self.block {
                entered.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
            output.fill(self.value);
        }

        fn failed(&self, error: String) {
            self.errors.lock().unwrap().push(error);
        }
    }

    #[test]
    fn a_replacement_does_not_consume_while_the_old_callback_is_in_flight() {
        let route = Arc::new(CallbackRoute::default());
        let (entered, old_filling) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let old = route.bind(Arc::new(Probe {
            value: 1.0,
            block: Some((entered, Mutex::new(released))),
            ..Probe::default()
        }));
        let candidate = route.bind(Arc::new(Probe {
            value: 2.0,
            ..Probe::default()
        }));
        let mut block = [1.0; 2];
        candidate.fill(&mut block);
        assert_eq!(block, [0.0; 2]);
        old.opened();
        std::thread::scope(|scope| {
            let old_fill = scope.spawn(|| {
                let mut output = [0.0; 2];
                old.fill(&mut output);
                assert_eq!(output, [1.0; 2]);
            });
            old_filling.recv_timeout(Duration::from_secs(2)).unwrap();
            candidate.opened();
            candidate.fill(&mut block);
            release.send(()).unwrap();
            old_fill.join().unwrap();
            assert_eq!(block, [0.0; 2]);
        });
        old.fill(&mut block);
        assert_eq!(block, [0.0; 2]);
        candidate.fill(&mut block);
        assert_eq!(block, [2.0; 2]);
    }

    #[test]
    fn only_an_accepted_callback_reports_its_failure() {
        let route = Arc::new(CallbackRoute::default());
        let probe = Arc::new(Probe {
            value: 1.0,
            ..Probe::default()
        });
        let old = route.bind(probe.clone());
        let candidate = route.bind(probe.clone());
        old.opened();
        candidate.failed("candidate failed before acceptance".into());
        assert!(probe.errors.lock().unwrap().is_empty());
        let mut block = [0.0; 2];
        old.fill(&mut block);
        assert_eq!(block, [1.0; 2]);

        candidate.opened();
        old.failed("retired device failed".into());
        candidate.fill(&mut block);
        assert_eq!(block, [0.0; 2]);
        assert_eq!(
            *probe.errors.lock().unwrap(),
            ["candidate failed before acceptance"]
        );
    }
}
