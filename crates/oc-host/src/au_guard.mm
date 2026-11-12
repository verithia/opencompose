#include "au_guard_abi.h"
#include <exception>

namespace {
template <class F> bool guarded(F &&work, OcNativeFault *fault) noexcept {
  try {
    @try { work(); return true; }
    @catch (id exception) {
      (void)exception;
      oc_native_fault_message(fault, 3, "Objective-C exception");
      return false;
    }
  } catch (const std::exception &error) {
    oc_native_fault_message(fault, 1, error.what());
  } catch (...) {
    oc_native_fault_message(fault, 2, "Nonstandard native exception");
  }
  return false;
}
} // namespace

extern "C" bool oc_au_find_next(const OcAuFindNext *in, AudioComponent *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.component, in->args.description); }, fault);
}

extern "C" bool oc_au_copy_name(const OcAuCopyName *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.component, in->args.name); }, fault);
}

extern "C" bool oc_au_description(const OcAuDescription *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.component, in->args.description); }, fault);
}

extern "C" bool oc_au_instance_new(const OcAuInstanceNew *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.component, in->args.instance); }, fault);
}

extern "C" bool oc_au_dispose(const OcAuDispose *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit); }, fault);
}

extern "C" bool oc_au_initialize(const OcAuInitialize *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit); }, fault);
}

extern "C" bool oc_au_uninitialize(const OcAuUninitialize *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit); }, fault);
}

extern "C" bool oc_au_reset(const OcAuReset *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.scope, in->args.element); }, fault);
}

extern "C" bool oc_au_set_property(const OcAuSetProperty *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.property, in->args.scope, in->args.element, in->args.data, in->args.size); }, fault);
}

extern "C" bool oc_au_get_property(const OcAuGetProperty *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.property, in->args.scope, in->args.element, in->args.data, in->args.size); }, fault);
}

extern "C" bool oc_au_property_info(const OcAuPropertyInfo *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.property, in->args.scope, in->args.element, in->args.size, in->args.writable); }, fault);
}

extern "C" bool oc_au_render(const OcAuRender *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.flags, in->args.timestamp, in->args.bus, in->args.frames, in->args.data); }, fault);
}

extern "C" bool oc_au_set_parameter(const OcAuSetParameter *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.parameter, in->args.scope, in->args.element, in->args.value, in->args.offset); }, fault);
}

extern "C" bool oc_au_get_parameter(const OcAuGetParameter *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.parameter, in->args.scope, in->args.element, in->args.value); }, fault);
}

extern "C" bool oc_au_midi(const OcAuMidi *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.status, in->args.data1, in->args.data2, in->args.offset); }, fault);
}

extern "C" bool oc_au_midi_list(const OcAuMidiList *in, OSStatus *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.unit, in->args.offset, in->args.events); }, fault);
}

extern "C" bool oc_au_string_length(const OcAuStringLength *in, CFIndex *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.text); }, fault);
}

extern "C" bool oc_au_string_capacity(const OcAuStringCapacity *in, CFIndex *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.length, in->args.encoding); }, fault);
}

extern "C" bool oc_au_string_c_string(const OcAuStringCString *in, Boolean *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.text, in->args.buffer, in->args.capacity, in->args.encoding); }, fault);
}

extern "C" bool oc_au_data_create(const OcAuDataCreate *in, CFDataRef *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.allocator, in->args.bytes, in->args.length); }, fault);
}

extern "C" bool oc_au_data_length(const OcAuDataLength *in, CFIndex *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.data); }, fault);
}

extern "C" bool oc_au_data_bytes(const OcAuDataBytes *in, const UInt8 * *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.data); }, fault);
}

extern "C" bool oc_au_plist_data(const OcAuPlistData *in, CFDataRef *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.allocator, in->args.plist, in->args.format, in->args.options, in->args.error); }, fault);
}

extern "C" bool oc_au_plist_create(const OcAuPlistCreate *in, CFPropertyListRef *out, OcNativeFault *fault) noexcept {
  return guarded([&] { *out = in->call(in->args.allocator, in->args.data, in->args.options, in->args.format, in->args.error); }, fault);
}

extern "C" bool oc_au_release(const OcAuRelease *in, uint8_t *out, OcNativeFault *fault) noexcept {
  return guarded([&] { in->call(in->args.value); *out = 0; }, fault);
}
