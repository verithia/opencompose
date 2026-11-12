#pragma once
#include <cstdint>
#include "native_fault.h"
#if defined(_WIN32) && (defined(_M_IX86) || defined(__i386__))
#define OC_VST2_CALL __cdecl
#else
#define OC_VST2_CALL
#endif
struct OcVst2Effect;
using OcVst2Host = intptr_t (OC_VST2_CALL *)(OcVst2Effect *, int32_t, int32_t, intptr_t, void *, float);
using OcVst2Entry = OcVst2Effect * (OC_VST2_CALL *)(OcVst2Host);
using OcVst2Dispatcher = intptr_t (OC_VST2_CALL *)(OcVst2Effect *, int32_t, int32_t, intptr_t, void *, float);
using OcVst2Set = void (OC_VST2_CALL *)(OcVst2Effect *, int32_t, float);
using OcVst2Get = float (OC_VST2_CALL *)(OcVst2Effect *, int32_t);
using OcVst2Process = void (OC_VST2_CALL *)(OcVst2Effect *, const float * const *, float **, int32_t);
static_assert(sizeof(int32_t) == 4 && sizeof(float) == 4 && sizeof(intptr_t) == sizeof(void *), "VST2 scalar ABI");
