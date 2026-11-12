// Direct typed foreign calls: no Rust forwarding frame lies on the exception path.
#include "vst2_guard_abi.h"
#include <exception>
#define OC_VST2_CATCH \
    catch (const std::exception &error) { oc_native_fault_message(fault, 1, error.what()); return false; } \
    catch (...) { oc_native_fault_message(fault, 2, "unknown C++ exception"); return false; }
extern "C" bool oc_vst2_entry(OcVst2Entry call, OcVst2Host host, OcVst2Effect **result, OcNativeFault *fault) noexcept {
    try { *result = call(host); return true; } OC_VST2_CATCH
}
extern "C" bool oc_vst2_dispatch(OcVst2Dispatcher call, OcVst2Effect *effect, int32_t op, int32_t index, intptr_t value, void *ptr, float opt, intptr_t *result, OcNativeFault *fault) noexcept {
    try { *result = call(effect, op, index, value, ptr, opt); return true; } OC_VST2_CATCH
}
extern "C" bool oc_vst2_set(OcVst2Set call, OcVst2Effect *effect, int32_t index, float value, OcNativeFault *fault) noexcept {
    try { call(effect, index, value); return true; } OC_VST2_CATCH
}
extern "C" bool oc_vst2_get(OcVst2Get call, OcVst2Effect *effect, int32_t index, float *result, OcNativeFault *fault) noexcept {
    try { *result = call(effect, index); return true; } OC_VST2_CATCH
}
extern "C" bool oc_vst2_process(OcVst2Process call, OcVst2Effect *effect, const float * const *inputs, float **outputs, int32_t frames, OcNativeFault *fault) noexcept {
    try { call(effect, inputs, outputs, frames); return true; } OC_VST2_CATCH
}
