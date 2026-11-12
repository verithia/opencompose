//! Native factory calls exercise the same guarded ABI used by module loading.
use crate::{native_fault::FaultKind, vst3_guard as guard};
use std::{
    ffi::c_void,
    mem::{align_of, size_of},
    ptr,
};
use vst3::Steinberg::{IPluginFactory, IPluginFactory2, PClassInfo, PClassInfo2, kResultOk};

#[link(name = "oc_vst3_guard_fixture", kind = "static")]
unsafe extern "C" {
    fn oc_vst3_factory_fixture_create() -> *mut c_void;
    fn oc_vst3_factory_fixture_dispose(fixture: *mut c_void);
    fn oc_vst3_factory_fixture_configure(
        fixture: *mut c_void,
        point: u32,
        kind: u32,
        count: i32,
        malformed: bool,
        null_output: bool,
        refusal: i32,
    );
    fn oc_vst3_factory_fixture_bind_iid(fixture: *mut c_void, iid: *const u8);
    fn oc_vst3_factory_fixture_refs(fixture: *mut c_void) -> u32;
    fn oc_vst3_factory_fixture_calls(fixture: *mut c_void, point: u32) -> u32;
    fn oc_vst3_factory_fixture_symbol(fixture: *mut c_void, point: u32) -> *mut c_void;
    fn oc_vst3_factory_fixture_layout(index: u32) -> usize;
}

/// Controlled native factory whose allocation is independently reclaimable
/// after a test deliberately quarantines a partially returned COM reference.
pub(crate) struct FactoryFixture(*mut c_void);
impl FactoryFixture {
    pub(crate) fn new() -> Self {
        // SAFETY: noexcept fixture construction returns its independently owned allocation.
        let raw = unsafe { oc_vst3_factory_fixture_create() };
        assert!(!raw.is_null());
        Self(raw)
    }
    /// Borrowed factory interface: this call does not transfer a COM reference.
    pub(crate) fn raw(&self) -> *mut IPluginFactory {
        self.0.cast()
    }
    /// Transfers one newly acquired COM reference to the caller. The fixture's
    /// independent allocation remains alive until this controlled owner drops.
    pub(crate) fn acquire(&self) -> Result<*mut IPluginFactory, String> {
        // SAFETY: the independently owned factory is live on this fixture thread.
        unsafe { guard::add_ref(self.0) }.map_err(|error| error.describe_for("VST3"))?;
        Ok(self.raw())
    }
    pub(crate) fn set_create_iid(&self, iid: &[u8; 16]) {
        // SAFETY: the native fixture and exact-sized IID buffer remain live.
        unsafe { oc_vst3_factory_fixture_bind_iid(self.0, iid.as_ptr()) }
    }
    pub(crate) fn configure(
        &self,
        point: u32,
        kind: u32,
        count: i32,
        malformed: bool,
        null_output: bool,
    ) {
        // SAFETY: this owner keeps the fixture allocation live on this thread.
        unsafe {
            oc_vst3_factory_fixture_configure(self.0, point, kind, count, malformed, null_output, 1)
        }
    }
    pub(crate) fn symbol(&self, point: u32) -> *mut c_void {
        // SAFETY: symbols remain valid while this native fixture is alive.
        unsafe { oc_vst3_factory_fixture_symbol(self.0, point) }
    }
    pub(crate) fn refs(&self) -> u32 {
        // SAFETY: the independent fixture owner remains live after COM faults.
        unsafe { oc_vst3_factory_fixture_refs(self.0) }
    }
    pub(crate) fn calls(&self, point: u32) -> u32 {
        // SAFETY: reads counters from this live native fixture.
        unsafe { oc_vst3_factory_fixture_calls(self.0, point) }
    }
}
impl Drop for FactoryFixture {
    fn drop(&mut self) {
        // SAFETY: tests own all calls synchronously; independent disposal does
        // not invoke the deliberately faulted native COM release operation.
        unsafe { oc_vst3_factory_fixture_dispose(self.0) }
    }
}

#[derive(Debug)]
/// Coverage of VST3 factory boundary checks.
pub struct FactoryBoundaryAudit {
    pub guarded_throw_cases: usize,
    pub guarded_refusal_cases: usize,
    pub partial_reference_retained: bool,
}

/// Runs in the existing native audit process as well as the library test owner.
/// No plugin discovery, dynamic module load, window or audio device is used.
pub fn audit_factory_boundary() -> Result<FactoryBoundaryAudit, String> {
    let fixture = FactoryFixture::new();
    let expected = [
        size_of::<PClassInfo>(),
        align_of::<PClassInfo>(),
        std::mem::offset_of!(PClassInfo, name),
        size_of::<PClassInfo2>(),
        align_of::<PClassInfo2>(),
        std::mem::offset_of!(PClassInfo2, classFlags),
        std::mem::offset_of!(PClassInfo2, subCategories),
        std::mem::offset_of!(PClassInfo2, sdkVersion),
    ];
    for (index, expected) in expected.into_iter().enumerate() {
        assert_eq!(
            // SAFETY: probe returns only compile-time native layout constants.
            unsafe { oc_vst3_factory_fixture_layout(index as u32) },
            expected
        );
    }
    let cid = [7_i8; 16];
    let iid = [8_i8; 16];
    let mut throws = 0;
    for point in 1..=9 {
        for kind in 1..=3 {
            fixture.configure(point, kind, 1, false, false);
            // SAFETY: every native interface, ID and stack output buffer is live;
            // all potentially throwing virtual/symbol calls occur in guarded C++.
            let result = unsafe {
                match point {
                    1 => guard::factory_count(fixture.raw()).map(|_| ()),
                    2 => {
                        let mut info = std::mem::zeroed::<PClassInfo>();
                        guard::factory_class_info(fixture.raw(), 0, &mut info).map(|_| ())
                    }
                    3 => {
                        let mut info = std::mem::zeroed::<PClassInfo2>();
                        guard::factory_class_info2(
                            fixture.raw().cast::<IPluginFactory2>(),
                            0,
                            &mut info,
                        )
                        .map(|_| ())
                    }
                    4 => {
                        let mut output = ptr::null_mut();
                        let before = fixture.refs();
                        let result = guard::factory_create(
                            fixture.raw(),
                            cid.as_ptr().cast(),
                            iid.as_ptr().cast(),
                            &mut output,
                        );
                        assert!(!output.is_null());
                        assert_eq!(fixture.refs(), before + 1);
                        // The output is deliberately quarantined: no release or
                        // another call is made through a faulted plugin reference.
                        result.map(|_| ())
                    }
                    5 => guard::module_entry(fixture.symbol(point), fixture.0).map(|_| ()),
                    6 => guard::module_exit(fixture.symbol(point)).map(|_| ()),
                    7 => guard::get_factory(fixture.symbol(point)).map(|_| ()),
                    8 => guard::module_init(fixture.symbol(point)).map(|_| ()),
                    9 => guard::bundle_entry(fixture.symbol(point), fixture.0).map(|_| ()),
                    _ => unreachable!(),
                }
            };
            let fault = result.expect_err("native factory throw must be caught");
            assert_eq!(
                fault.kind,
                if kind == 2 {
                    FaultKind::UnknownException
                } else {
                    FaultKind::CppException
                }
            );
            assert!(fault.describe_for("VST3").len() <= 280);
            throws += 1;
        }
    }
    assert_eq!(
        fixture.calls(10),
        0,
        "quarantined references must not be released"
    );
    let partial_reference_retained = fixture.refs() == 4;
    assert!(partial_reference_retained);

    let healthy = FactoryFixture::new();
    healthy.set_create_iid(&[8; 16]);
    let acquired = healthy.acquire()?;
    assert_eq!(healthy.refs(), 2);
    // SAFETY: this newly acquired healthy COM reference is released exactly once.
    unsafe { guard::release(acquired.cast()) }.map_err(|error| error.describe_for("VST3"))?;
    assert_eq!(healthy.refs(), 1);
    let mut refusals = 0;
    for point in [2, 3, 4] {
        healthy.configure(point, 4, 1, false, true);
        // SAFETY: owned fixture and fixed-size buffers remain live across each call.
        let result = unsafe {
            match point {
                2 => guard::factory_class_info(
                    healthy.raw(),
                    0,
                    &mut std::mem::zeroed::<PClassInfo>(),
                ),
                3 => guard::factory_class_info2(
                    healthy.raw().cast(),
                    0,
                    &mut std::mem::zeroed::<PClassInfo2>(),
                ),
                4 => {
                    let mut output = ptr::null_mut();
                    let result = guard::factory_create(
                        healthy.raw(),
                        cid.as_ptr().cast(),
                        iid.as_ptr().cast(),
                        &mut output,
                    );
                    assert!(output.is_null());
                    result
                }
                _ => unreachable!(),
            }
        };
        assert_eq!(result.map_err(|error| error.describe_for("VST3"))?, 1);
        refusals += 1;
    }
    for point in [5, 6, 8, 9] {
        healthy.configure(point, 4, 1, false, false);
        // SAFETY: the supplied native fixture symbol has the wrapper's exact ABI.
        let refused = unsafe {
            match point {
                5 => !guard::module_entry(healthy.symbol(point), healthy.0).unwrap(),
                6 => !guard::module_exit(healthy.symbol(point)).unwrap(),
                8 => !guard::module_init(healthy.symbol(point)).unwrap(),
                9 => guard::bundle_entry(healthy.symbol(point), healthy.0).unwrap() == 0,
                _ => unreachable!(),
            }
        };
        assert!(refused);
        refusals += 1;
    }
    healthy.configure(0, 0, 1, false, true);
    // SAFETY: get-factory and create use the live controlled fixture symbol/IDs.
    unsafe {
        assert!(guard::get_factory(healthy.symbol(7)).unwrap().is_null());
        let mut output = ptr::null_mut();
        assert_eq!(
            guard::factory_create(
                healthy.raw(),
                cid.as_ptr().cast(),
                iid.as_ptr().cast(),
                &mut output
            )
            .unwrap(),
            kResultOk
        );
        assert!(output.is_null());
    }
    for count in [-1, 0, 1, i32::MAX] {
        healthy.configure(0, 0, count, false, false);
        assert_eq!(
            // SAFETY: valid native factory; this call does not allocate based on its count.
            unsafe { guard::factory_count(healthy.raw()) }.unwrap(),
            count
        );
    }
    healthy.configure(0, 0, 1, true, false);
    // SAFETY: typed fixed-size outputs bound all string reads, even without a terminator.
    unsafe {
        let mut info = std::mem::zeroed::<PClassInfo>();
        let mut info2 = std::mem::zeroed::<PClassInfo2>();
        assert_eq!(
            guard::factory_class_info(healthy.raw(), 0, &mut info).unwrap(),
            kResultOk
        );
        assert_eq!(
            guard::factory_class_info2(healthy.raw().cast(), 0, &mut info2).unwrap(),
            kResultOk
        );
        assert!(
            info.name
                .iter()
                .all(|byte| *byte == b'n' as vst3::Steinberg::char8)
        );
        assert!(
            info2
                .subCategories
                .iter()
                .all(|byte| *byte == b's' as vst3::Steinberg::char8)
        );
    }
    for kind in 1..=3 {
        let mut component_fixture = super::NativeFixture::new(false);
        component_fixture.set_fault(42, kind);
        let raw = component_fixture.instance().raw_interfaces();
        let mut cid = [0; 16];
        // SAFETY: fixture component and fixed 16-byte class-ID buffer are live.
        let fault =
            unsafe { guard::controller_class_id(raw.component.cast(), &mut cid) }.unwrap_err();
        assert_eq!(
            fault.kind,
            if kind == 2 {
                FaultKind::UnknownException
            } else {
                FaultKind::CppException
            }
        );
        let first = component_fixture
            .instance()
            .native_fault(guard::Operation::ControllerClassId, fault);
        let later = component_fixture.instance().native_fault(
            guard::Operation::FactoryCreate,
            crate::native_fault::Fault::host_contract("later failure"),
        );
        assert_eq!(first, later);
        assert_eq!(
            component_fixture.instance().failure().first_fault(),
            Some(first)
        );
        let refs = component_fixture.refs();
        component_fixture.retire();
        assert_eq!(
            component_fixture.refs(),
            refs,
            "faulted controller-class discovery retains acquired interfaces"
        );
        assert_eq!(
            component_fixture.calls(10),
            0,
            "quarantine skips native termination"
        );
        throws += 1;
    }
    let mut refused_component = super::NativeFixture::new(false);
    refused_component.set_fault(42, 4);
    let mut cid = [0; 16];
    assert_ne!(
        // SAFETY: the fixture retains its component and exact-sized output buffer.
        unsafe {
            guard::controller_class_id(
                refused_component
                    .instance()
                    .raw_interfaces()
                    .component
                    .cast(),
                &mut cid,
            )
        }
        .unwrap(),
        kResultOk
    );
    assert!(!refused_component.instance().failure().failed());
    refused_component.retire();
    refusals += 1;
    Ok(FactoryBoundaryAudit {
        guarded_throw_cases: throws,
        guarded_refusal_cases: refusals,
        partial_reference_retained,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_factory_virtual_and_module_entry_exceptions_are_contained() {
        let audit = super::audit_factory_boundary().unwrap();
        assert_eq!(audit.guarded_throw_cases, 30);
        assert_eq!(audit.guarded_refusal_cases, 8);
        assert!(audit.partial_reference_retained);
    }
}
