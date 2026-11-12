//! Typed C++ invocation; callers own admission and immutable first-fault policy.
#![expect(
    clippy::result_large_err,
    reason = "The fixed native cause stays inline so audio fault capture never allocates"
)]
use super::abi;
use crate::native_fault::{Fault, RawFault};
use std::ffi::c_void;
unsafe extern "C" {
    fn oc_vst2_entry(
        call: abi::PluginMain,
        host: abi::AudioMasterCallback,
        result: *mut *mut abi::AEffect,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst2_dispatch(
        call: abi::DispatcherProc,
        effect: *mut abi::AEffect,
        opcode: i32,
        index: i32,
        value: isize,
        ptr: *mut c_void,
        opt: f32,
        result: *mut isize,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst2_set(
        call: abi::SetParameterProc,
        effect: *mut abi::AEffect,
        index: i32,
        value: f32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst2_get(
        call: abi::GetParameterProc,
        effect: *mut abi::AEffect,
        index: i32,
        result: *mut f32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst2_process(
        call: abi::ProcessProc,
        effect: *mut abi::AEffect,
        inputs: *const *const f32,
        outputs: *mut *mut f32,
        frames: i32,
        fault: *mut RawFault,
    ) -> bool;
}
fn raw() -> RawFault {
    RawFault {
        kind: 0,
        message: [0; 256],
    }
}
fn value<T>(call: impl FnOnce(*mut T, *mut RawFault) -> bool) -> Result<T, Fault> {
    let mut result = std::mem::MaybeUninit::uninit();
    let mut fault = raw();
    if call(result.as_mut_ptr(), &mut fault) {
        // SAFETY: Each typed native shim writes its result only after the
        // foreign call returns normally; its true result admits that write.
        Ok(unsafe { result.assume_init() })
    } else {
        Err(Fault::from_raw(fault))
    }
}
/// # Safety
/// Entry and callback must have the declared ABI, pinned code and retained host backing.
pub(super) unsafe fn entry(
    call: abi::PluginMain,
    host: abi::AudioMasterCallback,
) -> Result<*mut abi::AEffect, Fault> {
    // SAFETY: Caller retains the exact entry/callback ABI through the typed shim.
    value(|result, fault| unsafe { oc_vst2_entry(call, host, result, fault) })
}
#[derive(Clone, Copy)]
pub(super) struct DispatchArgs {
    pub opcode: i32,
    pub index: i32,
    pub value: isize,
    pub ptr: *mut c_void,
    pub opt: f32,
}
/// # Safety
/// Effect/callback and every pointer loan must remain valid and exclusively admitted.
pub(super) unsafe fn dispatch(
    call: abi::DispatcherProc,
    effect: *mut abi::AEffect,
    args: DispatchArgs,
) -> Result<isize, Fault> {
    // SAFETY: Caller owns ABI, affinity and retained argument loans for this call.
    value(|result, fault| unsafe {
        oc_vst2_dispatch(
            call,
            effect,
            args.opcode,
            args.index,
            args.value,
            args.ptr,
            args.opt,
            result,
            fault,
        )
    })
}
/// # Safety
/// The admitted live effect and setter/index must remain valid for this exclusive call.
pub(super) unsafe fn set(
    call: abi::SetParameterProc,
    effect: *mut abi::AEffect,
    index: i32,
    input: f32,
) -> Result<(), Fault> {
    let mut fault = raw();
    // SAFETY: Caller provides the exact admitted setter, live effect and scalar index.
    if unsafe { oc_vst2_set(call, effect, index, input, &mut fault) } {
        Ok(())
    } else {
        Err(Fault::from_raw(fault))
    }
}
/// # Safety
/// Getter/index/effect must satisfy the native ABI and exclusive control admission.
pub(super) unsafe fn get(
    call: abi::GetParameterProc,
    effect: *mut abi::AEffect,
    index: i32,
) -> Result<f32, Fault> {
    // SAFETY: Caller retains the admitted getter/effect through the typed call.
    value(|result, fault| unsafe { oc_vst2_get(call, effect, index, result, fault) })
}
/// # Safety
/// Prepared pointer arrays/planes must remain valid for the admitted frame count and fault lifetime.
pub(super) unsafe fn process(
    call: abi::ProcessProc,
    effect: *mut abi::AEffect,
    inputs: *const *const f32,
    outputs: *mut *mut f32,
    frames: i32,
) -> Result<(), Fault> {
    let mut fault = raw();
    // SAFETY: Caller owns the live processor and retained, bounded process packet.
    if unsafe { oc_vst2_process(call, effect, inputs, outputs, frames, &mut fault) } {
        Ok(())
    } else {
        Err(Fault::from_raw(fault))
    }
}
