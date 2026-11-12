//! Main-run-loop attach with bounded timing and unwind-safe selector cancellation.
use super::*;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker};
use objc2_foundation::{NSDate, NSObject, NSObjectProtocol, NSRunLoop};
use std::marker::PhantomData;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Timing {
    delay: Duration,
    timeout: Duration,
}
impl Timing {
    pub(super) fn parse(raw: Option<&str>) -> Result<Self, String> {
        const MAX_DELAY_MS: f64 = 30_000.0;
        let milliseconds = match raw {
            None => 0.0,
            Some(value) => value.trim().parse::<f64>().map_err(|_| {
                "VST3 editor attach delay must be a finite number in 0..=30000 milliseconds"
                    .to_owned()
            })?,
        };
        if !milliseconds.is_finite() || !(0.0..=MAX_DELAY_MS).contains(&milliseconds) {
            return Err(
                "VST3 editor attach delay must be a finite number in 0..=30000 milliseconds".into(),
            );
        }
        // Both durations are constructed before any selector is scheduled.
        let delay = if milliseconds == 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(milliseconds / 1000.0)
        };
        Ok(Self {
            delay,
            timeout: delay + Duration::from_secs(5),
        })
    }
    pub(super) fn from_env() -> Result<Self, String> {
        match std::env::var("OCP_EDITOR_ATTACH_DELAY_MS") {
            Ok(raw) => Self::parse(Some(&raw)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("VST3 editor attach delay must be valid UTF-8".into())
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct Cancellation {
    handles: Arc<Vst3EditorHandles>,
    generation: usize,
    window: Option<usize>,
    view: Option<usize>,
}
impl Cancellation {
    pub(super) fn opening(
        handles: &Arc<Vst3EditorHandles>,
        generation: usize,
        window: usize,
    ) -> Self {
        Self {
            handles: Arc::clone(handles),
            generation,
            window: Some(window),
            view: None,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    fn registered(handles: &Arc<Vst3EditorHandles>, view: usize) -> Self {
        Self {
            handles: Arc::clone(handles),
            generation: handles.attach_generation.load(Ordering::Acquire),
            window: None,
            view: Some(view),
        }
    }
    fn current(&self) -> bool {
        self.handles.attach_generation.load(Ordering::Acquire) == self.generation
            && self.window.is_none_or(|window| {
                self.handles.opening.load(Ordering::Acquire)
                    && self.handles.window.load(Ordering::Acquire) == window
            })
            && self
                .view
                .is_none_or(|view| self.handles.view.load(Ordering::Acquire) == view)
    }
}
pub(super) struct Options {
    pub timing: Timing,
    pub before_dispatch: Option<Box<dyn FnOnce()>>,
    pub after_schedule: Option<Box<dyn FnOnce()>>,
}
impl Options {
    pub(super) fn from_env() -> Result<Self, String> {
        Ok(Self {
            timing: Timing::from_env()?,
            before_dispatch: None,
            after_schedule: None,
        })
    }
}
struct AttachIvars {
    context: Cell<usize>,
    cancellation: Cancellation,
    before_dispatch: RefCell<Option<Box<dyn FnOnce()>>>,
}
objc2::define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AttachIvars]
    struct AttachTarget;

    // SAFETY: the NSObject superclass supplies this protocol; the subclass adds no protocol requirements.
    unsafe impl NSObjectProtocol for AttachTarget {}
    impl AttachTarget {
        #[unsafe(method(runAttach))]
        fn run_attach(&self) {
            crate::native_callback::callback((), || {
                // One-shot: cancellation invalidates this before pending can leave
                // its stack, even if an already queued selector is later delivered.
                let context = self.ivars().context.replace(0);
                if context == 0 { return; }
                let action = self.ivars().before_dispatch.borrow_mut().take();
                if let Some(action) = action { action(); }
                if !self.ivars().cancellation.current() { return; }
                // SAFETY: ScheduledAttach borrows the pending state until this
                // synchronous main-thread callback returns or is invalidated.
                unsafe { attach_on_main_run_loop(context as *mut c_void) };
            }, || {
                // Bounded first-fault publication and Cell invalidation cannot
                // unwind. The shared callback owner also handles panic payloads
                // whose destructor would panic, before returning through ObjC.
                self.ivars().context.set(0);
                self.ivars().cancellation.handles.failure.slot.record(
                    Vst3ProcessError::native(Operation::Legacy, Fault::callback_panic()));
            });
        }
    }
);

/// The retained target drops before its borrowed pending/view/parent backing.
/// No transfer to another thread is possible for this main-only ObjC object.
struct ScheduledAttach<'a> {
    target: Retained<AttachTarget>,
    _pending: PhantomData<&'a PendingAttach>,
}
impl<'a> ScheduledAttach<'a> {
    fn new(
        pending: &'a PendingAttach,
        timing: Timing,
        main: MainThreadMarker,
        cancellation: Cancellation,
        before_dispatch: Option<Box<dyn FnOnce()>>,
    ) -> Self {
        let target = AttachTarget::alloc(main).set_ivars(AttachIvars {
            context: Cell::new(pending as *const PendingAttach as usize),
            cancellation,
            before_dispatch: RefCell::new(before_dispatch),
        });
        // SAFETY: allocated NSObject subclass has initialized ivars; super init
        // returns its owned retained receiver on main.
        let target: Retained<AttachTarget> = unsafe { objc2::msg_send![super(target), init] };
        let guard = Self {
            target,
            _pending: PhantomData,
        };
        // The guard exists BEFORE scheduling: a subsequent Rust unwind clears
        // the ivar and cancels this selector while all borrowed backing is live.
        // SAFETY: target is a live main-thread NSObject, selector has no arguments,
        // and the validated finite delay is in 0..=30 seconds. Cocoa retains target.
        let _: () = unsafe {
            objc2::msg_send![&*guard.target,
                performSelector: objc2::sel!(runAttach),
                withObject: Option::<&objc2::runtime::AnyObject>::None,
                afterDelay: timing.delay.as_secs_f64()]
        };
        guard
    }
}
impl Drop for ScheduledAttach<'_> {
    fn drop(&mut self) {
        // Invalidate FIRST: pending is never accessed even if cancellation itself
        // fails or a selector already queued for delivery outlives this guard.
        self.target.ivars().context.set(0);
        // SAFETY: this guard retains the exact main-thread target, selector and
        // optional nil argument used to schedule it; cancellation calls no plugin.
        let _: () = unsafe {
            objc2::msg_send![objc2::class!(NSObject),
                cancelPreviousPerformRequestsWithTarget: &*self.target,
                selector: objc2::sel!(runAttach),
                object: Option::<&objc2::runtime::AnyObject>::None]
        };
    }
}

pub(super) struct Request<'a> {
    pub handles: &'a Arc<Vst3EditorHandles>,
    pub view: *mut IPlugView,
    pub parent: *mut c_void,
    pub main: MainThreadMarker,
    pub cancellation: Cancellation,
}
pub(super) fn run(request: Request<'_>, options: Options) -> Result<tresult, String> {
    let Request {
        handles,
        view,
        parent,
        main,
        cancellation,
    } = request;
    handles.check()?;
    if !cancellation.current() {
        return Err("VST3 editor was closed while attaching".into());
    }
    let pending = PendingAttach {
        failure: Arc::clone(&handles.failure),
        view,
        parent,
        result: Cell::new(kResultFalse),
        done: Cell::new(false),
    };
    let _scheduled = ScheduledAttach::new(
        &pending,
        options.timing,
        main,
        cancellation.clone(),
        options.before_dispatch,
    );
    if let Some(action) = options.after_schedule {
        action();
    }
    let started = Instant::now();
    while !pending.done.get()
        && !handles.failure.failed()
        && cancellation.current()
        && started.elapsed() < options.timing.timeout
    {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
    }
    handles.check()?;
    // A completed successful attach must be reported even after a nested close,
    // so the caller can balance it with removed before refusing publication.
    if pending.done.get() {
        return Ok(pending.result.get());
    }
    if !cancellation.current() {
        return Err("VST3 editor was closed while attaching".into());
    }
    Err("VST3 editor attach did not run on the main run loop".into())
}

#[cfg(any(test, feature = "test-support"))]
/// Validate a deferred-attach delay override for tests.
pub fn validate_deferred_attach_delay_for_test(raw: Option<&str>) -> Result<Duration, String> {
    Timing::parse(raw).map(|timing| timing.delay)
}

/// Exercise the actual scheduler using a controlled, registered fixture view.
///
/// # Safety
/// The registered view must be an UNATTACHED controlled fixture, not an editor
/// already attached to a real window. Its ABI must permit this temporary parent;
/// attached must not itself run a nested event loop or retire its registration.
/// A successful attach is balanced with guarded removed before parent release.
#[cfg(any(test, feature = "test-support"))]
pub unsafe fn exercise_deferred_attach_for_test(
    handle: &Vst3GuiHandle,
    delay: Option<&str>,
    before_dispatch: Option<Box<dyn FnOnce()>>,
) -> Result<tresult, String> {
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    let main = MainThreadMarker::new().ok_or("deferred attach audit requires AppKit main")?;
    let timing = Timing::parse(delay)?;
    handle.handles.check()?;
    let owner = loan_editor_view(&handle.handles)?;
    let view = owner.view.as_ptr();
    let cancellation = Cancellation::registered(&handle.handles, view as usize);
    let parent = NSView::initWithFrame(
        NSView::alloc(main),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 480.0)),
    );
    let result = run(
        Request {
            handles: &handle.handles,
            view,
            parent: Retained::as_ptr(&parent).cast_mut().cast(),
            main,
            cancellation,
        },
        Options {
            timing,
            before_dispatch,
            after_schedule: None,
        },
    );
    if matches!(result, Ok(code) if code == kResultOk) {
        // SAFETY: the main-only registry loan retains the unattached fixture view
        // through this balanced detach; the temporary parent is still retained.
        match unsafe { vst3_guard::view_removed(view) } {
            Ok(code) if code == kResultOk => {}
            Ok(code) => {
                handle
                    .handles
                    .failure
                    .slot
                    .record(Vst3ProcessError::refused(Operation::Legacy, code));
            }
            Err(fault) => {
                handle
                    .handles
                    .failure
                    .slot
                    .record(Vst3ProcessError::native(Operation::Legacy, fault));
            }
        }
    }
    if handle.handles.failure.failed() {
        std::mem::forget(parent);
        return Err(handle
            .handles
            .failure
            .message()
            .unwrap_or_else(|| "VST3 editor fixture is quarantined".into()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deferred_attach_delay_is_bounded_before_any_scheduling() {
        assert_eq!(Timing::parse(None).unwrap().delay, Duration::ZERO);
        assert_eq!(Timing::parse(Some("0")).unwrap().delay, Duration::ZERO);
        assert_eq!(Timing::parse(Some("-0")).unwrap().delay, Duration::ZERO);
        assert_eq!(
            Timing::parse(Some("12.5")).unwrap().delay,
            Duration::from_micros(12500)
        );
        assert_eq!(
            Timing::parse(Some("30000")).unwrap().timeout,
            Duration::from_secs(35)
        );
        for raw in [
            "-10000", "-1", "NaN", "inf", "-inf", "1e308", "30000.1", "", "invalid",
        ] {
            assert!(
                Timing::parse(Some(raw)).is_err(),
                "accepted invalid delay {raw}"
            );
        }
    }
}
