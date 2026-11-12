#include "../../src/au_guard_abi.h"
#import <Foundation/Foundation.h>
#include <stdexcept>

namespace {
thread_local uint32_t mode = 0;
thread_local uint32_t calls[25] = {};
constexpr OSStatus fixture_return_status = 71;
thread_local uint32_t target = UINT32_MAX;
thread_local uint32_t occurrence = 1;
thread_local int32_t unit_status = 0;
thread_local float parameter_value = 0.5f;
thread_local void *input_refcon = nullptr;
thread_local void *clock_refcon = nullptr;
void enter(uint32_t index) {
  ++calls[index];
  if ((target != UINT32_MAX && target != index) || calls[index] != occurrence) return;
  if (mode == 1) throw std::runtime_error("AU fixture C++ fault");
  if (mode == 2) throw uint32_t(17);
  if (mode == 3) @throw @"AU fixture Objective-C fault";
}
}

extern "C" void oc_au_fixture_mode(uint32_t value) noexcept {
  mode = value;
  target = UINT32_MAX;
  occurrence = 1;
  for (auto &count : calls) count = 0;
}
extern "C" uint32_t oc_au_fixture_calls(uint32_t index) noexcept { return index < 25 ? calls[index] : 0; }

static AudioComponent fixture_find_next(AudioComponent component, const AudioComponentDescription * description) {
  (void)component; (void)description;
  enter(0); return reinterpret_cast<AudioComponent>(uintptr_t(1));
}
extern "C" decltype(&::AudioComponentFindNext) oc_au_fixture_find_next() noexcept { return &fixture_find_next; }

static OSStatus fixture_copy_name(AudioComponent component, CFStringRef * name) {
  (void)component; (void)name;
  enter(1); return fixture_return_status;
}
extern "C" decltype(&::AudioComponentCopyName) oc_au_fixture_copy_name() noexcept { return &fixture_copy_name; }

static OSStatus fixture_description(AudioComponent component, AudioComponentDescription * description) {
  (void)component; (void)description;
  enter(2); return fixture_return_status;
}
extern "C" decltype(&::AudioComponentGetDescription) oc_au_fixture_description() noexcept { return &fixture_description; }

static OSStatus fixture_instance_new(AudioComponent component, AudioComponentInstance * instance) {
  (void)component; (void)instance;
  enter(3); return fixture_return_status;
}
extern "C" decltype(&::AudioComponentInstanceNew) oc_au_fixture_instance_new() noexcept { return &fixture_instance_new; }

static OSStatus fixture_dispose(AudioUnit unit) {
  (void)unit;
  enter(4); return fixture_return_status;
}
extern "C" decltype(&::AudioComponentInstanceDispose) oc_au_fixture_dispose() noexcept { return &fixture_dispose; }

static OSStatus fixture_initialize(AudioUnit unit) {
  (void)unit;
  enter(5); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitInitialize) oc_au_fixture_initialize() noexcept { return &fixture_initialize; }

static OSStatus fixture_uninitialize(AudioUnit unit) {
  (void)unit;
  enter(6); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitUninitialize) oc_au_fixture_uninitialize() noexcept { return &fixture_uninitialize; }

static OSStatus fixture_reset(AudioUnit unit, AudioUnitScope scope, AudioUnitElement element) {
  (void)unit; (void)scope; (void)element;
  enter(7); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitReset) oc_au_fixture_reset() noexcept { return &fixture_reset; }

static OSStatus fixture_set_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, const void * data, UInt32 size) {
  (void)unit; (void)property; (void)scope; (void)element; (void)data; (void)size;
  enter(8); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitSetProperty) oc_au_fixture_set_property() noexcept { return &fixture_set_property; }

static OSStatus fixture_get_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, void * data, UInt32 * size) {
  (void)unit; (void)property; (void)scope; (void)element; (void)data; (void)size;
  enter(9); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitGetProperty) oc_au_fixture_get_property() noexcept { return &fixture_get_property; }

static OSStatus fixture_property_info(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, UInt32 * size, Boolean * writable) {
  (void)unit; (void)property; (void)scope; (void)element; (void)size; (void)writable;
  enter(10); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitGetPropertyInfo) oc_au_fixture_property_info() noexcept { return &fixture_property_info; }

static OSStatus fixture_render(AudioUnit unit, AudioUnitRenderActionFlags * flags, const AudioTimeStamp * timestamp, UInt32 bus, UInt32 frames, AudioBufferList * data) {
  (void)unit; (void)flags; (void)timestamp; (void)bus; (void)frames; (void)data;
  enter(11); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitRender) oc_au_fixture_render() noexcept { return &fixture_render; }

static OSStatus fixture_set_parameter(AudioUnit unit, AudioUnitParameterID parameter, AudioUnitScope scope, AudioUnitElement element, AudioUnitParameterValue value, UInt32 offset) {
  (void)unit; (void)parameter; (void)scope; (void)element; (void)value; (void)offset;
  enter(12); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitSetParameter) oc_au_fixture_set_parameter() noexcept { return &fixture_set_parameter; }

static OSStatus fixture_get_parameter(AudioUnit unit, AudioUnitParameterID parameter, AudioUnitScope scope, AudioUnitElement element, AudioUnitParameterValue * value) {
  (void)unit; (void)parameter; (void)scope; (void)element; (void)value;
  enter(13); return fixture_return_status;
}
extern "C" decltype(&::AudioUnitGetParameter) oc_au_fixture_get_parameter() noexcept { return &fixture_get_parameter; }

static OSStatus fixture_midi(MusicDeviceComponent unit, UInt32 status, UInt32 data1, UInt32 data2, UInt32 offset) {
  (void)unit; (void)status; (void)data1; (void)data2; (void)offset;
  enter(14); return fixture_return_status;
}
extern "C" decltype(&::MusicDeviceMIDIEvent) oc_au_fixture_midi() noexcept { return &fixture_midi; }

static OSStatus fixture_midi_list(MusicDeviceComponent unit, UInt32 offset, const MIDIEventList * events) {
  (void)unit; (void)offset; (void)events;
  enter(15); return fixture_return_status;
}
extern "C" decltype(&::MusicDeviceMIDIEventList) oc_au_fixture_midi_list() noexcept { return &fixture_midi_list; }

static CFIndex fixture_string_length(CFStringRef text) {
  (void)text;
  enter(16); return 9;
}
extern "C" decltype(&::CFStringGetLength) oc_au_fixture_string_length() noexcept { return &fixture_string_length; }

static CFIndex fixture_string_capacity(CFIndex length, CFStringEncoding encoding) {
  (void)length; (void)encoding;
  enter(17); return 9;
}
extern "C" decltype(&::CFStringGetMaximumSizeForEncoding) oc_au_fixture_string_capacity() noexcept { return &fixture_string_capacity; }

static Boolean fixture_string_c_string(CFStringRef text, char * buffer, CFIndex capacity, CFStringEncoding encoding) {
  (void)text; (void)buffer; (void)capacity; (void)encoding;
  enter(18); return 1;
}
extern "C" decltype(&::CFStringGetCString) oc_au_fixture_string_c_string() noexcept { return &fixture_string_c_string; }

static CFDataRef fixture_data_create(CFAllocatorRef allocator, const UInt8 * bytes, CFIndex length) {
  (void)allocator; (void)bytes; (void)length;
  enter(19); return reinterpret_cast<CFDataRef>(uintptr_t(1));
}
extern "C" decltype(&::CFDataCreate) oc_au_fixture_data_create() noexcept { return &fixture_data_create; }

static CFIndex fixture_data_length(CFDataRef data) {
  (void)data;
  enter(20); return 9;
}
extern "C" decltype(&::CFDataGetLength) oc_au_fixture_data_length() noexcept { return &fixture_data_length; }

static const UInt8 * fixture_data_bytes(CFDataRef data) {
  (void)data;
  enter(21); return reinterpret_cast<const UInt8 *>(uintptr_t(1));
}
extern "C" decltype(&::CFDataGetBytePtr) oc_au_fixture_data_bytes() noexcept { return &fixture_data_bytes; }

static CFDataRef fixture_plist_data(CFAllocatorRef allocator, CFPropertyListRef plist, CFPropertyListFormat format, CFOptionFlags options, CFErrorRef * error) {
  (void)allocator; (void)plist; (void)format; (void)options; (void)error;
  enter(22); return reinterpret_cast<CFDataRef>(uintptr_t(1));
}
extern "C" decltype(&::CFPropertyListCreateData) oc_au_fixture_plist_data() noexcept { return &fixture_plist_data; }

static CFPropertyListRef fixture_plist_create(CFAllocatorRef allocator, CFDataRef data, CFOptionFlags options, CFPropertyListFormat * format, CFErrorRef * error) {
  (void)allocator; (void)data; (void)options; (void)format; (void)error;
  enter(23); return reinterpret_cast<CFPropertyListRef>(uintptr_t(1));
}
extern "C" decltype(&::CFPropertyListCreateWithData) oc_au_fixture_plist_create() noexcept { return &fixture_plist_create; }

static void fixture_release(CFTypeRef value) {
  (void)value;
  enter(24);
}
extern "C" decltype(&::CFRelease) oc_au_fixture_release() noexcept { return &fixture_release; }


extern "C" void oc_au_unit_config(uint32_t operation, uint32_t hit, uint32_t fault_mode, int32_t returned_status) noexcept {
  mode = fault_mode;
  target = operation;
  occurrence = hit;
  unit_status = returned_status;
  for (auto &count : calls) count = 0;
}
extern "C" void *oc_au_unit_input_refcon() noexcept { return input_refcon; }
extern "C" void *oc_au_unit_clock_refcon() noexcept { return clock_refcon; }
static int32_t unit_result(uint32_t index) noexcept {
  return mode == 0 && target == index && calls[index] == occurrence ? unit_status : 0;
}

static AudioComponent unit_find_next(AudioComponent component, const AudioComponentDescription * description) {
  (void)component; (void)description;
  enter(0);
  return reinterpret_cast<AudioComponent>(uintptr_t(1));
}
extern "C" decltype(&::AudioComponentFindNext) oc_au_unit_find_next() noexcept { return &unit_find_next; }

static OSStatus unit_description(AudioComponent component, AudioComponentDescription * description) {
  (void)component; (void)description;
  enter(2);
  if (description != nullptr) *description = {}; return unit_result(2);
}
extern "C" decltype(&::AudioComponentGetDescription) oc_au_unit_description() noexcept { return &unit_description; }

static OSStatus unit_instance_new(AudioComponent component, AudioComponentInstance * instance) {
  (void)component; (void)instance;
  if (instance != nullptr) *instance = reinterpret_cast<AudioComponentInstance>(uintptr_t(2));
  enter(3);
  return unit_result(3);
}
extern "C" decltype(&::AudioComponentInstanceNew) oc_au_unit_instance_new() noexcept { return &unit_instance_new; }

static OSStatus unit_dispose(AudioUnit unit) {
  (void)unit;
  enter(4);
  return unit_result(4);
}
extern "C" decltype(&::AudioComponentInstanceDispose) oc_au_unit_dispose() noexcept { return &unit_dispose; }

static OSStatus unit_initialize(AudioUnit unit) {
  (void)unit;
  enter(5);
  return unit_result(5);
}
extern "C" decltype(&::AudioUnitInitialize) oc_au_unit_initialize() noexcept { return &unit_initialize; }

static OSStatus unit_uninitialize(AudioUnit unit) {
  (void)unit;
  enter(6);
  return unit_result(6);
}
extern "C" decltype(&::AudioUnitUninitialize) oc_au_unit_uninitialize() noexcept { return &unit_uninitialize; }

static OSStatus unit_reset(AudioUnit unit, AudioUnitScope scope, AudioUnitElement element) {
  (void)unit; (void)scope; (void)element;
  enter(7);
  return unit_result(7);
}
extern "C" decltype(&::AudioUnitReset) oc_au_unit_reset() noexcept { return &unit_reset; }

static OSStatus unit_set_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, const void * data, UInt32 size) {
  (void)unit; (void)property; (void)scope; (void)element; (void)data; (void)size;
  enter(8);
  if (auto result = unit_result(8)) return result;
  if (property == kAudioUnitProperty_SetRenderCallback && data != nullptr) {
    auto callback = static_cast<const AURenderCallbackStruct *>(data);
    input_refcon = callback->inputProcRefCon;
  }
  if (property == kAudioUnitProperty_HostCallbacks && data != nullptr) {
    clock_refcon = static_cast<const HostCallbackInfo *>(data)->hostUserData;
  }
  return 0;
}
extern "C" decltype(&::AudioUnitSetProperty) oc_au_unit_set_property() noexcept { return &unit_set_property; }

static OSStatus unit_get_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, void * data, UInt32 * size) {
  (void)unit; (void)property; (void)scope; (void)element; (void)data; (void)size;
  enter(9);
  if (auto result = unit_result(9)) return result;
  if (scope != kAudioUnitScope_Global) return kAudioUnitErr_InvalidScope;
  if (property == kAudioUnitProperty_ParameterList) {
    if (data == nullptr || size == nullptr || *size < sizeof(AudioUnitParameterID)) return kAudio_ParamError;
    *static_cast<AudioUnitParameterID *>(data) = 7;
    *size = sizeof(AudioUnitParameterID);
  } else if (property == kAudioUnitProperty_ParameterInfo) {
    if (data == nullptr || size == nullptr || *size < sizeof(AudioUnitParameterInfo)) return kAudio_ParamError;
    auto info = static_cast<AudioUnitParameterInfo *>(data);
    *info = {};
    std::memcpy(info->name, "Gain", 5);
    info->minValue = 0;
    info->maxValue = 1;
    info->defaultValue = 0.5;
    info->flags = kAudioUnitParameterFlag_IsWritable;
    *size = sizeof(AudioUnitParameterInfo);
  } else if (property == kAudioUnitProperty_Latency || property == kAudioUnitProperty_TailTime) {
    if (data == nullptr || size == nullptr || *size < sizeof(Float64)) return kAudio_ParamError;
    *static_cast<Float64 *>(data) = 0;
    *size = sizeof(Float64);
  } else if (property == 64) {
    if (data == nullptr || size == nullptr || *size < sizeof(SInt32)) return kAudio_ParamError;
    *static_cast<SInt32 *>(data) = 1;
    *size = sizeof(SInt32);
  } else if (property == kAudioUnitProperty_ClassInfo) {
    if (data == nullptr || size == nullptr || *size < sizeof(CFPropertyListRef)) return kAudio_ParamError;
    static const unsigned char xml[] = "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>fixture</key><true/></dict></plist>";
    CFDataRef bytes = CFDataCreate(nullptr, xml, sizeof(xml)-1);
    *static_cast<CFPropertyListRef *>(data) = CFPropertyListCreateWithData(nullptr, bytes, 0, nullptr, nullptr);
    CFRelease(bytes);
    *size = sizeof(CFPropertyListRef);
  } else { return kAudioUnitErr_InvalidProperty; }
  return 0;
}
extern "C" decltype(&::AudioUnitGetProperty) oc_au_unit_get_property() noexcept { return &unit_get_property; }

static OSStatus unit_property_info(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, UInt32 * size, Boolean * writable) {
  (void)unit; (void)property; (void)scope; (void)element; (void)size; (void)writable;
  enter(10);
  if (auto result = unit_result(10)) return result;
  if (scope != kAudioUnitScope_Global) return kAudioUnitErr_InvalidScope;
  if (size != nullptr) *size = sizeof(AudioUnitParameterID);
  if (writable != nullptr) *writable = false;
  return 0;
}
extern "C" decltype(&::AudioUnitGetPropertyInfo) oc_au_unit_property_info() noexcept { return &unit_property_info; }

static OSStatus unit_render(AudioUnit unit, AudioUnitRenderActionFlags * flags, const AudioTimeStamp * timestamp, UInt32 bus, UInt32 frames, AudioBufferList * data) {
  (void)unit; (void)flags; (void)timestamp; (void)bus; (void)frames; (void)data;
  enter(11);
  if (auto result = unit_result(11)) return result;
  if (data == nullptr || data->mNumberBuffers != 2) return kAudio_ParamError;
  for (uint32_t channel = 0; channel < 2; ++channel) {
    if (data->mBuffers[channel].mDataByteSize < frames * sizeof(float)) return kAudio_ParamError;
    auto output = static_cast<float *>(data->mBuffers[channel].mData);
    for (uint32_t frame = 0; frame < frames; ++frame) output[frame] = (channel == 0 ? 1.f : -1.f) * (frame + 1.f) / 64.f;
  }
  return 0;
}
extern "C" decltype(&::AudioUnitRender) oc_au_unit_render() noexcept { return &unit_render; }

static OSStatus unit_set_parameter(AudioUnit unit, AudioUnitParameterID parameter, AudioUnitScope scope, AudioUnitElement element, AudioUnitParameterValue value, UInt32 offset) {
  (void)unit; (void)parameter; (void)scope; (void)element; (void)value; (void)offset;
  enter(12);
  if (auto result = unit_result(12)) return result;
  parameter_value = value;
  return 0;
}
extern "C" decltype(&::AudioUnitSetParameter) oc_au_unit_set_parameter() noexcept { return &unit_set_parameter; }

static OSStatus unit_get_parameter(AudioUnit unit, AudioUnitParameterID parameter, AudioUnitScope scope, AudioUnitElement element, AudioUnitParameterValue * value) {
  (void)unit; (void)parameter; (void)scope; (void)element; (void)value;
  enter(13);
  if (auto result = unit_result(13)) return result;
  if (value != nullptr) *value = parameter_value;
  return 0;
}
extern "C" decltype(&::AudioUnitGetParameter) oc_au_unit_get_parameter() noexcept { return &unit_get_parameter; }

static OSStatus unit_midi(MusicDeviceComponent unit, UInt32 status, UInt32 data1, UInt32 data2, UInt32 offset) {
  (void)unit; (void)status; (void)data1; (void)data2; (void)offset;
  enter(14);
  return unit_result(14);
}
extern "C" decltype(&::MusicDeviceMIDIEvent) oc_au_unit_midi() noexcept { return &unit_midi; }

static OSStatus unit_midi_list(MusicDeviceComponent unit, UInt32 offset, const MIDIEventList * events) {
  (void)unit; (void)offset; (void)events;
  enter(15);
  return unit_result(15);
}
extern "C" decltype(&::MusicDeviceMIDIEventList) oc_au_unit_midi_list() noexcept { return &unit_midi_list; }

extern "C" void oc_au_unit_forget_callbacks() noexcept { input_refcon = nullptr; clock_refcon = nullptr; }
static OSStatus unit_copy_name(AudioComponent component, CFStringRef *name) {
  (void)component;
  enter(1);
  if (auto result = unit_result(1)) return result;
  if (name != nullptr) *name = CFStringCreateWithCString(nullptr, "OpenCompose AU fixture", kCFStringEncodingUTF8);
  return 0;
}
extern "C" decltype(&::AudioComponentCopyName) oc_au_unit_copy_name() noexcept { return &unit_copy_name; }

extern "C" void oc_au_unit_parameter_value(float value) noexcept { parameter_value = value; }

namespace { thread_local uint32_t malformed_list_bytes = 5; }
extern "C" void oc_au_unit_list_size(uint32_t bytes) noexcept { malformed_list_bytes = bytes; }

static OSStatus partial_state_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, void *data, UInt32 *size) {
  if (property != kAudioUnitProperty_ClassInfo) return unit_get_property(unit, property, scope, element, data, size);
  enter(9);
  if (data != nullptr) *static_cast<unsigned char *>(data) = 1;
  if (size != nullptr) *size = 1;
  return unit_result(9);
}
extern "C" decltype(&::AudioUnitGetProperty) oc_au_partial_state_property() noexcept { return &partial_state_property; }

static OSStatus partial_copy_name(AudioComponent component, CFStringRef *name) {
  (void)component;
  enter(1);
  if (name != nullptr) *reinterpret_cast<unsigned char *>(name) = 1;
  return unit_result(1);
}
extern "C" decltype(&::AudioComponentCopyName) oc_au_partial_copy_name() noexcept { return &partial_copy_name; }

static void counted_release(CFTypeRef value) {
  enter(24);
  // The sentinel has no CF object. Count attempted admission without dereferencing it.
  if (value != reinterpret_cast<CFTypeRef>(uintptr_t(1))) CFRelease(value);
}
extern "C" decltype(&::CFRelease) oc_au_counted_release() noexcept { return &counted_release; }

static OSStatus malformed_list_info(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, UInt32 *size, Boolean *writable) {
  if (property != kAudioUnitProperty_ParameterList) return unit_property_info(unit, property, scope, element, size, writable);
  enter(10);
  if (scope != kAudioUnitScope_Global) return kAudioUnitErr_InvalidScope;
  if (size != nullptr) *size = malformed_list_bytes;
  if (writable != nullptr) *writable = false;
  return 0;
}
extern "C" decltype(&::AudioUnitGetPropertyInfo) oc_au_malformed_list_info() noexcept { return &malformed_list_info; }

namespace { thread_local uint32_t list_read_calls = 0; }
extern "C" void oc_au_list_reads_reset() noexcept { list_read_calls = 0; }
extern "C" uint32_t oc_au_list_reads() noexcept { return list_read_calls; }
static OSStatus counted_list_property(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, void *data, UInt32 *size) {
  if (property == kAudioUnitProperty_ParameterList) ++list_read_calls;
  return unit_get_property(unit, property, scope, element, data, size);
}
extern "C" decltype(&::AudioUnitGetProperty) oc_au_counted_list_property() noexcept { return &counted_list_property; }

static OSStatus partial_parameter_info(AudioUnit unit, AudioUnitPropertyID property, AudioUnitScope scope, AudioUnitElement element, void *data, UInt32 *size) {
  if (property != kAudioUnitProperty_ParameterInfo) return unit_get_property(unit, property, scope, element, data, size);
  enter(9);
  if (data != nullptr) {
    auto info = static_cast<AudioUnitParameterInfo *>(data);
    *info = {};
    info->cfNameString = reinterpret_cast<CFStringRef>(uintptr_t(1));
    info->flags = kAudioUnitParameterFlag_HasCFNameString | kAudioUnitParameterFlag_CFNameRelease;
  }
  if (size != nullptr) *size = 1;
  return 0;
}
extern "C" decltype(&::AudioUnitGetProperty) oc_au_partial_parameter_info() noexcept { return &partial_parameter_info; }
