//! Actual C++ exception cases, run by the existing literal-main native harness.
use super::*;
use crate::native_fault::FaultKind;
use std::ffi::c_void;
mod claim_audit;
mod preparation_audit;
mod runtime_audit;

/// Native guard case names exercised by the audit.
pub const VST2_NATIVE_GUARD_CASES: &[&str] = &[
    "entry-std",
    "entry-unknown",
    "entry-foreign",
    "entry-panic",
    "entry-panic-foreign",
    "entry-panic-first",
    "open",
    "name-output",
    "feature-input",
    "getter",
    "setter",
    "properties-output",
    "chunk-output",
    "chunk-input",
    "editor-output",
    "process-replacing",
    "process-accumulating",
    "events",
    "deactivate",
    "close",
    "close-deleted",
    "long-message",
    "entry-slot-isolation",
    "ordinary-zero",
    "callback-long-query",
    "preparation-success",
    "preparation-owned-success",
    "preparation-shadowed",
    "preparation-invalid-final",
    "preparation-state-refused",
    "preparation-state-fault",
    "preparation-param-fault",
    "preparation-activate-fault",
    "preparation-tail-fault",
    "preparation-close-fault",
    "preparation-invalid-close-fault",
    "runtime-batch-success",
    "runtime-batch-preflight",
    "runtime-batch-wrong-source",
    "runtime-batch-setter-fault",
    "runtime-batch-events-fault",
    "runtime-batch-structure",
    "runtime-batch-setter-identity",
    "runtime-batch-no-setter",
    "runtime-batch-no-setter-binding",
    "claim-routing-only",
    "claim-routing-no-setter",
    "claim-native-collision",
    "claim-native-ambiguity",
    "claim-native-only",
    "claim-invalid-final",
    "claim-unknown-profile",
    "claim-reserved-collision",
    "claim-native-no-setter",
];
type Configure = unsafe extern "C" fn(u32, i32, u32, u32, usize);
type Count = unsafe extern "C" fn(u32) -> u64;
type Number = unsafe extern "C" fn() -> u64;
type Address = unsafe extern "C" fn() -> usize;
type SavedAddress = unsafe extern "C" fn(u32) -> usize;
type Late = unsafe extern "C" fn(u32, usize, i32, f32, bool) -> isize;
type Touch = unsafe extern "C" fn() -> u32;
type Query = unsafe extern "C" fn(u32, u32) -> isize;
type PreparationMode = unsafe extern "C" fn(bool);
type RuntimeMode = unsafe extern "C" fn(u32, u64);
type RuntimeDetail = unsafe extern "C" fn(u32) -> u64;
type ClaimMode = unsafe extern "C" fn(u32);
struct Fixture {
    _module: module_authority::ModuleAuthority,
    path: PathBuf,
    configure: Configure,
    count: Count,
    live: Number,
    helper_rate: Number,
    arena: Address,
    saved_address: SavedAddress,
    late: Late,
    touch: Touch,
    query: Query,
    preparation_mode: PreparationMode,
    preparation_status: Number,
    runtime_mode: RuntimeMode,
    runtime_detail: RuntimeDetail,
    claim_mode: ClaimMode,
}
impl Fixture {
    fn new(secondary: bool) -> Result<Self, Vst2OwnedError> {
        let token = application_main::ui_authority()?;
        let _phase = Phase::enter()?;
        let path = PathBuf::from(if secondary {
            env!("OC_VST2_GUARD_SECONDARY_PATH")
        } else {
            env!("OC_VST2_GUARD_FIXTURE_PATH")
        });
        let module = module_authority::acquire(&token, &path)
            .map_err(|error| Vst2OwnedError::Native(error.to_string()))?;
        macro_rules! symbol {
            ($name:expr, $ty:ty) => {{
                let pointer = module.fixture_symbol($name);
                if pointer.is_null() {
                    return Err(Vst2OwnedError::Control(
                        "Controlled native fixture export missing".into(),
                    ));
                }
                // SAFETY: This built-in controlled fixture exports this exact C
                // signature; the acquired actual-image authority pins its code.
                unsafe { std::mem::transmute::<*mut c_void, $ty>(pointer) }
            }};
        }
        Ok(Self {
            configure: symbol!(c"oc_vst2_fixture_configure", Configure),
            count: symbol!(c"oc_vst2_fixture_count", Count),
            live: symbol!(c"oc_vst2_fixture_live", Number),
            helper_rate: symbol!(c"oc_vst2_fixture_entry_helper_rate", Number),
            arena: symbol!(c"oc_vst2_fixture_arena", Address),
            saved_address: symbol!(c"oc_vst2_fixture_saved_effect", SavedAddress),
            late: symbol!(c"oc_vst2_fixture_late_callback", Late),
            touch: symbol!(c"oc_vst2_fixture_touch_loans", Touch),
            query: symbol!(c"oc_vst2_fixture_query_host", Query),
            preparation_mode: symbol!(c"oc_vst2_fixture_preparation_mode", PreparationMode),
            preparation_status: symbol!(c"oc_vst2_fixture_preparation_status", Number),
            runtime_mode: symbol!(c"oc_vst2_fixture_runtime_mode", RuntimeMode),
            runtime_detail: symbol!(c"oc_vst2_fixture_runtime_detail", RuntimeDetail),
            claim_mode: symbol!(c"oc_vst2_fixture_claim_mode", ClaimMode),
            _module: module,
            path,
        })
    }
    fn configure(&self, point: u32, opcode: i32, kind: u32, flags: u32, address: usize) {
        // SAFETY: This trusted observer only writes bounded atomic fixture
        // configuration, never dereferences a faulted native object.
        unsafe { (self.configure)(point, opcode, kind, flags, address) };
    }
    fn count(&self, point: u32) -> u64 {
        // SAFETY: The trusted observer reads static counters in the pinned image.
        unsafe { (self.count)(point) }
    }
    fn live(&self) -> u64 {
        // SAFETY: Static observer data outlives every quarantined native object.
        unsafe { (self.live)() }
    }
    fn counts(&self) -> [u64; 9] {
        std::array::from_fn(|point| self.count(point as u32))
    }
    fn load(&self) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
        Vst2OwnedLoadPlan {
            path: self.path.clone(),
            sample_rate: 48_000,
            max_frames: 64,
        }
        .execute_on_ui()
    }
    fn late(&self, address: usize, opcode: i32, value: f32) -> isize {
        // SAFETY: This observer invokes a saved host callback on a native C++
        // thread using only the address value; it never reads the old AEffect.
        unsafe { (self.late)(0, address, opcode, value, true) }
    }
}
fn fault<T>(result: Result<T, Vst2OwnedError>) -> Result<Arc<Vst2Failure>, Vst2OwnedError> {
    match result {
        Err(Vst2OwnedError::Faulted(failure)) => Ok(failure),
        Err(error) => Err(error),
        Ok(_) => Err(Vst2OwnedError::Control(
            "Expected native fault was not returned".into(),
        )),
    }
}
fn control<T>(
    handle: &Vst2ControlHandle,
    work: impl FnOnce(&mut Vst2Instance) -> T,
) -> Result<T, Vst2OwnedError> {
    let _token = application_main::ui_authority()?;
    let _phase = Phase::enter()?;
    let _permit = handle.link.acquire(CONTROL)?;
    // SAFETY: Real UI authority and the exclusive control gate retain the
    // complete native candidate; this helper calls the actual domain method.
    let native = unsafe { &mut *handle.link.native.as_ptr() };
    let result = work(native);
    if handle.link.failure.failed() {
        return Err(Vst2OwnedError::Faulted(handle.link.failure.clone()));
    }
    Ok(result)
}
#[derive(Debug)]
/// Audit results for one native guard case.
pub struct Vst2NativeGuardAudit {
    pub exact_first_cause: bool,
    pub first_report_once: bool,
    pub no_later_native_calls: bool,
    pub payload_retained: bool,
    pub loans_retained: bool,
    pub process_heap: super::audit::Vst2HeapCounters,
    pub slot_isolation: bool,
}

/// Audit one native guard case.
pub fn audit_owned_vst2_native_case(
    case: &str,
    heap: fn() -> super::audit::Vst2HeapCounters,
) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    let _token = application_main::ui_authority()?;
    let fixture = Fixture::new(false)?;
    if case.starts_with("claim-") {
        return claim_audit::audit(&fixture, case);
    }
    if case.starts_with("runtime-batch-") {
        return runtime_audit::audit(&fixture, case, heap);
    }
    if case.starts_with("preparation-") {
        return preparation_audit::audit(&fixture, case);
    }
    if case == "callback-long-query" {
        return callback_queries(&fixture, heap);
    }
    if case == "ordinary-zero" {
        return ordinary(&fixture);
    }
    if case == "entry-slot-isolation" {
        return isolation(&fixture);
    }
    let (point, opcode, kind, flags, expected, during_load) = match case {
        "entry-std" => (1, 0, 1, 0, Vst2Operation::Entry, true),
        "entry-unknown" => (1, 0, 2, 0, Vst2Operation::Entry, true),
        "entry-foreign" => (1, 0, 1, 2, Vst2Operation::Entry, true),
        "entry-panic" => (1, 0, 4, 0, Vst2Operation::HostCallback(0x4f430010), true),
        "entry-panic-foreign" => (1, 0, 4, 2, Vst2Operation::HostCallback(0x4f430010), true),
        "entry-panic-first" => (1, 0, 5, 0, Vst2Operation::HostCallback(0x4f430010), true),
        "open" => (2, 0, 1, 0, Vst2Operation::Dispatcher(0), true),
        "name-output" => (2, 45, 1, 1, Vst2Operation::Dispatcher(45), true),
        "feature-input" => (2, 51, 1, 0, Vst2Operation::Dispatcher(51), true),
        "getter" => (4, 0, 1, 0, Vst2Operation::GetParameter(0), true),
        "setter" => (3, 0, 1, 0, Vst2Operation::SetParameter(0), false),
        "properties-output" => (2, 56, 1, 1, Vst2Operation::Dispatcher(56), false),
        "chunk-output" => (2, 23, 1, 1, Vst2Operation::Dispatcher(23), false),
        "chunk-input" => (2, 24, 1, 0, Vst2Operation::Dispatcher(24), false),
        "editor-output" => (2, 13, 1, 1, Vst2Operation::Dispatcher(13), false),
        "process-replacing" => (5, 0, 1, 0, Vst2Operation::ProcessReplacing, false),
        "process-accumulating" => (6, 0, 1, 0, Vst2Operation::ProcessAccumulating, false),
        "events" => (2, 25, 1, 0, Vst2Operation::Dispatcher(25), false),
        "deactivate" => (7, 0, 1, 0, Vst2Operation::Deactivate, false),
        "close" => (8, 0, 1, 0, Vst2Operation::Close, false),
        "close-deleted" => (8, 0, 1, 4, Vst2Operation::Close, false),
        "long-message" => (1, 0, 6, 0, Vst2Operation::Entry, true),
        _ => {
            return Err(Vst2OwnedError::Control(
                "Unknown native VST2 audit case".into(),
            ));
        }
    };
    let mut process_heap = super::audit::Vst2HeapCounters::default();
    let mut repeated_calls_refused = true;
    let failure = if during_load {
        fixture.configure(point, opcode, kind, flags, 0);
        fault(fixture.load())?
    } else {
        // Accumulating selection is a descriptor capability set during entry.
        fixture.configure(
            if point == 6 || case == "editor-output" {
                point
            } else {
                0
            },
            opcode,
            0,
            0,
            0,
        );
        let mut instance = fixture.load()?;
        let failure = instance.control.failure();
        fixture.configure(point, opcode, kind, flags, 0);
        match case {
            "setter" => {
                fault(instance.control.write_parameter_on_ui("Gain".into(), 0.75))?;
            }
            "properties-output" => {
                fault(control(&instance.control, |native| {
                    native.parameter_surface()
                }))?;
            }
            "chunk-output" => {
                fault(instance.control.capture_state_on_ui())?;
            }
            "chunk-input" => {
                let bytes = vec![0; 8];
                fault(control(&instance.control, |native| {
                    native.set_state_bytes(&bytes)
                }))?;
                drop(bytes);
            }
            "editor-output" => {
                fault(control(&instance.control, |native| native.editor_size()))?;
            }
            "process-replacing" | "process-accumulating" | "events" => {
                if case == "events" {
                    instance.processor.queue_midi(0x90, 60, 100, 0)?;
                }
                let mut output = [0.25; 128];
                let before = heap();
                let result = if point == 6 {
                    instance
                        .processor
                        .process_instrument_into(&mut output, None)
                } else {
                    instance.processor.process_effect_into(&mut output, None)
                };
                fault(result)?;
                let after = heap();
                process_heap = super::audit::Vst2HeapCounters {
                    allocations: after.allocations - before.allocations,
                    reallocations: after.reallocations - before.reallocations,
                    deallocations: after.deallocations - before.deallocations,
                };
                if output != [0.0; 128] {
                    return Err(Vst2OwnedError::Control(
                        "Native fault published partial output".into(),
                    ));
                }
            }
            "deactivate" | "close" | "close-deleted" => {}
            _ => unreachable!(),
        }
        if failure.failed() {
            let before = fixture.counts();
            let mut unchanged = [0.125; 128];
            repeated_calls_refused = matches!(
                instance.control.write_parameter_on_ui("Gain".into(), 0.9),
                Err(Vst2OwnedError::Faulted(_))
            ) && matches!(
                instance.control.capture_state_on_ui(),
                Err(Vst2OwnedError::Faulted(_))
            ) && matches!(
                instance.processor.process_effect_into(&mut unchanged, None),
                Err(Vst2OwnedError::Faulted(_))
            ) && unchanged == [0.125; 128]
                && before == fixture.counts();
        }
        drop(instance);
        poll_owned_vst2_retirements()?;
        failure
    };
    let record = failure
        .first_fault()
        .ok_or_else(|| Vst2OwnedError::Control("Native first cause missing".into()))?;
    let expected_kind = if kind == 2 {
        FaultKind::UnknownException
    } else if kind == 4 || kind == 5 {
        FaultKind::CallbackPanic
    } else {
        FaultKind::CppException
    };
    let exact_first_cause = record.operation == expected
        && record.fault.kind == expected_kind
        && (case != "long-message"
            || record.fault.describe_for("VST2").len() <= "VST2 plugin threw: ".len() + 255);
    let first_report_once =
        failure.take_report() == Some(record) && failure.take_report().is_none();
    let before = fixture.counts();
    poll_owned_vst2_retirements()?;
    let mut no_later_native_calls = fixture.counts() == before
        && failure.first_fault() == Some(record)
        && repeated_calls_refused;
    // SAFETY: The observer returns only the saved pointer value. In particular,
    // deleted-CLOSE callbacks must be rejected without reading that descriptor.
    let saved_address = unsafe { (fixture.saved_address)(0) };
    no_later_native_calls &= fixture.late(saved_address, abi::host_op::GET_TIME, 0.0) == 0;
    if point == 1 {
        no_later_native_calls &=
            matches!(fixture.load(), Err(Vst2OwnedError::Faulted(_))) && fixture.count(1) == 1;
        if case == "entry-foreign" {
            // SAFETY: A pinned trusted observer returns only its stored scalar.
            no_later_native_calls &= f64::from_bits(unsafe { (fixture.helper_rate)() }) == 48_000.0;
        }
    }
    let retained_route = callback_route::occupied_budget_for_audit() == (1, 1024);
    let retained_registry = during_load || registry::entries_for_audit() == 1;
    let payload_retained = retained_route
        && retained_registry
        && if case == "close-deleted" {
            fixture.live() == 0 && fixture.count(8) == 1
        } else {
            fixture.live() == 1 && (point == 8 || fixture.count(8) == 0)
        };
    let expects_loan = matches!(
        case,
        "name-output"
            | "feature-input"
            | "properties-output"
            | "chunk-output"
            | "chunk-input"
            | "editor-output"
            | "process-replacing"
            | "process-accumulating"
            | "events"
    );
    // SAFETY: The trusted observer exercises only retained host argument loans,
    // not an operational method or a potentially deleted native descriptor.
    let loans_retained =
        !expects_loan || unsafe { (fixture.touch)() } >= if case == "events" { 2 } else { 1 };
    Ok(Vst2NativeGuardAudit {
        exact_first_cause,
        first_report_once,
        no_later_native_calls,
        payload_retained,
        loans_retained,
        process_heap,
        slot_isolation: true,
    })
}
fn ordinary(fixture: &Fixture) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    fixture.configure(1, 0, 3, 0, 0);
    if !matches!(fixture.load(), Err(Vst2OwnedError::Native(_))) {
        return Err(Vst2OwnedError::Control(
            "Ordinary null entry became native fault".into(),
        ));
    }
    fixture.configure(0, 0, 0, 0, 0);
    let instance = fixture.load()?;
    let failure = instance.control.failure();
    fixture.configure(2, 24, 3, 0, 0);
    let rejected = control(&instance.control, |native| native.set_state_bytes(&[0; 8]))?;
    let healthy = !failure.failed() && rejected.is_err();
    drop(instance);
    poll_owned_vst2_retirements()?;
    Ok(Vst2NativeGuardAudit {
        exact_first_cause: healthy,
        first_report_once: true,
        no_later_native_calls: true,
        payload_retained: fixture.live() == 0 && fixture.count(8) == 1,
        loans_retained: true,
        process_heap: super::audit::Vst2HeapCounters::default(),
        slot_isolation: true,
    })
}
fn callback_queries(
    fixture: &Fixture,
    heap: fn() -> super::audit::Vst2HeapCounters,
) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    let instance = fixture.load()?;
    let before = heap();
    let replies: [isize; 7] = std::array::from_fn(|query| {
        // SAFETY: The pinned native fixture owns each NUL-terminated query,
        // including the short allocations and 1 MiB unsupported query. This
        // calls the saved slot thunk while its live instance remains admitted.
        unsafe { (fixture.query)(0, query as u32) }
    });
    let after = heap();
    let healthy = replies == [1, 1, 0, 0, 0, 0, 0] && !instance.control.failure().failed();
    drop(instance);
    poll_owned_vst2_retirements()?;
    Ok(Vst2NativeGuardAudit {
        exact_first_cause: healthy,
        first_report_once: true,
        no_later_native_calls: true,
        payload_retained: fixture.live() == 0 && fixture.count(8) == 1,
        loans_retained: true,
        process_heap: super::audit::Vst2HeapCounters {
            allocations: after.allocations - before.allocations,
            reallocations: after.reallocations - before.reallocations,
            deallocations: after.deallocations - before.deallocations,
        },
        slot_isolation: true,
    })
}
fn isolation(a: &Fixture) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    // SAFETY: This pinned fixture's aligned static storage is retained for process
    // life, and the controlled second image reuses it by placement construction.
    let address = unsafe { (a.arena)() };
    a.configure(1, 0, 1, 2, address);
    let failure = fault(a.load())?;
    let b = Fixture::new(true)?;
    b.configure(0, 0, 0, 0, address);
    let instance = b.load()?;
    let before = control(&instance.control, |native| native.host.value(0))?;
    let late = a.late(address, abi::host_op::AUTOMATE, 0.99);
    let after = control(&instance.control, |native| native.host.value(0))?;
    let slot_isolation = late == 0
        && before == Some(0.5)
        && after == before
        && a.late(address, abi::host_op::GET_TIME, 0.0) == 0;
    drop(instance);
    poll_owned_vst2_retirements()?;
    let record = failure
        .first_fault()
        .ok_or_else(|| Vst2OwnedError::Control("Entry cause missing".into()))?;
    Ok(Vst2NativeGuardAudit {
        exact_first_cause: record.operation == Vst2Operation::Entry,
        first_report_once: failure.take_report() == Some(record) && failure.take_report().is_none(),
        no_later_native_calls: a.count(1) == 1,
        payload_retained: a.live() == 1,
        loans_retained: true,
        process_heap: super::audit::Vst2HeapCounters::default(),
        slot_isolation,
    })
}
