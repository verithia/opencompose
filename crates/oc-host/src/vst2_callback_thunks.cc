// VST2 has no host userdata parameter. A fixed callback address identifies each
// retained reservation even when native entry never returns an AEffect.
#include <array>
#include <cstdint>
#include <utility>
#if defined(_WIN32) && (defined(_M_IX86) || defined(__i386__))
#define OC_VST2_CALL __cdecl
#else
#define OC_VST2_CALL
#endif
struct OcVst2Effect;
using OcVst2Host = intptr_t (OC_VST2_CALL *)(OcVst2Effect *, int32_t, int32_t, intptr_t, void *, float);
extern "C" intptr_t oc_vst2_owned_callback_bridge(uint32_t, OcVst2Effect *, int32_t, int32_t, intptr_t, void *, float);
template<std::size_t Slot>
static intptr_t OC_VST2_CALL callback(OcVst2Effect *effect, int32_t op, int32_t index, intptr_t value, void *ptr, float opt) noexcept {
    return oc_vst2_owned_callback_bridge(static_cast<uint32_t>(Slot), effect, op, index, value, ptr, opt);
}
template<std::size_t... Slots>
static constexpr auto table(std::index_sequence<Slots...>) {
    return std::array<OcVst2Host, sizeof...(Slots)>{{ &callback<Slots>... }};
}
static constexpr auto callbacks = table(std::make_index_sequence<OC_VST2_CALLBACK_CAPACITY>{});
extern "C" OcVst2Host oc_vst2_host_callback_for_slot(uint32_t slot) noexcept {
    return slot < callbacks.size() ? callbacks[slot] : nullptr;
}
extern "C" uint32_t oc_vst2_host_callback_capacity() noexcept { return static_cast<uint32_t>(callbacks.size()); }
