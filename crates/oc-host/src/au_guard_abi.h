#pragma once
#include <AudioToolbox/AudioToolbox.h>
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMIDI/CoreMIDI.h>
#include "native_fault.h"

struct OcAuFindNextArgs {
  AudioComponent component;
  const AudioComponentDescription * description;
};
struct OcAuFindNext { decltype(&::AudioComponentFindNext) call; OcAuFindNextArgs args; };
extern "C" bool oc_au_find_next(const OcAuFindNext *, AudioComponent *, OcNativeFault *) noexcept;

struct OcAuCopyNameArgs {
  AudioComponent component;
  CFStringRef * name;
};
struct OcAuCopyName { decltype(&::AudioComponentCopyName) call; OcAuCopyNameArgs args; };
extern "C" bool oc_au_copy_name(const OcAuCopyName *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuDescriptionArgs {
  AudioComponent component;
  AudioComponentDescription * description;
};
struct OcAuDescription { decltype(&::AudioComponentGetDescription) call; OcAuDescriptionArgs args; };
extern "C" bool oc_au_description(const OcAuDescription *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuInstanceNewArgs {
  AudioComponent component;
  AudioComponentInstance * instance;
};
struct OcAuInstanceNew { decltype(&::AudioComponentInstanceNew) call; OcAuInstanceNewArgs args; };
extern "C" bool oc_au_instance_new(const OcAuInstanceNew *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuDisposeArgs {
  AudioUnit unit;
};
struct OcAuDispose { decltype(&::AudioComponentInstanceDispose) call; OcAuDisposeArgs args; };
extern "C" bool oc_au_dispose(const OcAuDispose *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuInitializeArgs {
  AudioUnit unit;
};
struct OcAuInitialize { decltype(&::AudioUnitInitialize) call; OcAuInitializeArgs args; };
extern "C" bool oc_au_initialize(const OcAuInitialize *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuUninitializeArgs {
  AudioUnit unit;
};
struct OcAuUninitialize { decltype(&::AudioUnitUninitialize) call; OcAuUninitializeArgs args; };
extern "C" bool oc_au_uninitialize(const OcAuUninitialize *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuResetArgs {
  AudioUnit unit;
  AudioUnitScope scope;
  AudioUnitElement element;
};
struct OcAuReset { decltype(&::AudioUnitReset) call; OcAuResetArgs args; };
extern "C" bool oc_au_reset(const OcAuReset *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuSetPropertyArgs {
  AudioUnit unit;
  AudioUnitPropertyID property;
  AudioUnitScope scope;
  AudioUnitElement element;
  const void * data;
  UInt32 size;
};
struct OcAuSetProperty { decltype(&::AudioUnitSetProperty) call; OcAuSetPropertyArgs args; };
extern "C" bool oc_au_set_property(const OcAuSetProperty *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuGetPropertyArgs {
  AudioUnit unit;
  AudioUnitPropertyID property;
  AudioUnitScope scope;
  AudioUnitElement element;
  void * data;
  UInt32 * size;
};
struct OcAuGetProperty { decltype(&::AudioUnitGetProperty) call; OcAuGetPropertyArgs args; };
extern "C" bool oc_au_get_property(const OcAuGetProperty *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuPropertyInfoArgs {
  AudioUnit unit;
  AudioUnitPropertyID property;
  AudioUnitScope scope;
  AudioUnitElement element;
  UInt32 * size;
  Boolean * writable;
};
struct OcAuPropertyInfo { decltype(&::AudioUnitGetPropertyInfo) call; OcAuPropertyInfoArgs args; };
extern "C" bool oc_au_property_info(const OcAuPropertyInfo *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuRenderArgs {
  AudioUnit unit;
  AudioUnitRenderActionFlags * flags;
  const AudioTimeStamp * timestamp;
  UInt32 bus;
  UInt32 frames;
  AudioBufferList * data;
};
struct OcAuRender { decltype(&::AudioUnitRender) call; OcAuRenderArgs args; };
extern "C" bool oc_au_render(const OcAuRender *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuSetParameterArgs {
  AudioUnit unit;
  AudioUnitParameterID parameter;
  AudioUnitScope scope;
  AudioUnitElement element;
  AudioUnitParameterValue value;
  UInt32 offset;
};
struct OcAuSetParameter { decltype(&::AudioUnitSetParameter) call; OcAuSetParameterArgs args; };
extern "C" bool oc_au_set_parameter(const OcAuSetParameter *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuGetParameterArgs {
  AudioUnit unit;
  AudioUnitParameterID parameter;
  AudioUnitScope scope;
  AudioUnitElement element;
  AudioUnitParameterValue * value;
};
struct OcAuGetParameter { decltype(&::AudioUnitGetParameter) call; OcAuGetParameterArgs args; };
extern "C" bool oc_au_get_parameter(const OcAuGetParameter *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuMidiArgs {
  MusicDeviceComponent unit;
  UInt32 status;
  UInt32 data1;
  UInt32 data2;
  UInt32 offset;
};
struct OcAuMidi { decltype(&::MusicDeviceMIDIEvent) call; OcAuMidiArgs args; };
extern "C" bool oc_au_midi(const OcAuMidi *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuMidiListArgs {
  MusicDeviceComponent unit;
  UInt32 offset;
  const MIDIEventList * events;
};
struct OcAuMidiList { decltype(&::MusicDeviceMIDIEventList) call; OcAuMidiListArgs args; };
extern "C" bool oc_au_midi_list(const OcAuMidiList *, OSStatus *, OcNativeFault *) noexcept;

struct OcAuStringLengthArgs {
  CFStringRef text;
};
struct OcAuStringLength { decltype(&::CFStringGetLength) call; OcAuStringLengthArgs args; };
extern "C" bool oc_au_string_length(const OcAuStringLength *, CFIndex *, OcNativeFault *) noexcept;

struct OcAuStringCapacityArgs {
  CFIndex length;
  CFStringEncoding encoding;
};
struct OcAuStringCapacity { decltype(&::CFStringGetMaximumSizeForEncoding) call; OcAuStringCapacityArgs args; };
extern "C" bool oc_au_string_capacity(const OcAuStringCapacity *, CFIndex *, OcNativeFault *) noexcept;

struct OcAuStringCStringArgs {
  CFStringRef text;
  char * buffer;
  CFIndex capacity;
  CFStringEncoding encoding;
};
struct OcAuStringCString { decltype(&::CFStringGetCString) call; OcAuStringCStringArgs args; };
extern "C" bool oc_au_string_c_string(const OcAuStringCString *, Boolean *, OcNativeFault *) noexcept;

struct OcAuDataCreateArgs {
  CFAllocatorRef allocator;
  const UInt8 * bytes;
  CFIndex length;
};
struct OcAuDataCreate { decltype(&::CFDataCreate) call; OcAuDataCreateArgs args; };
extern "C" bool oc_au_data_create(const OcAuDataCreate *, CFDataRef *, OcNativeFault *) noexcept;

struct OcAuDataLengthArgs {
  CFDataRef data;
};
struct OcAuDataLength { decltype(&::CFDataGetLength) call; OcAuDataLengthArgs args; };
extern "C" bool oc_au_data_length(const OcAuDataLength *, CFIndex *, OcNativeFault *) noexcept;

struct OcAuDataBytesArgs {
  CFDataRef data;
};
struct OcAuDataBytes { decltype(&::CFDataGetBytePtr) call; OcAuDataBytesArgs args; };
extern "C" bool oc_au_data_bytes(const OcAuDataBytes *, const UInt8 * *, OcNativeFault *) noexcept;

struct OcAuPlistDataArgs {
  CFAllocatorRef allocator;
  CFPropertyListRef plist;
  CFPropertyListFormat format;
  CFOptionFlags options;
  CFErrorRef * error;
};
struct OcAuPlistData { decltype(&::CFPropertyListCreateData) call; OcAuPlistDataArgs args; };
extern "C" bool oc_au_plist_data(const OcAuPlistData *, CFDataRef *, OcNativeFault *) noexcept;

struct OcAuPlistCreateArgs {
  CFAllocatorRef allocator;
  CFDataRef data;
  CFOptionFlags options;
  CFPropertyListFormat * format;
  CFErrorRef * error;
};
struct OcAuPlistCreate { decltype(&::CFPropertyListCreateWithData) call; OcAuPlistCreateArgs args; };
extern "C" bool oc_au_plist_create(const OcAuPlistCreate *, CFPropertyListRef *, OcNativeFault *) noexcept;

struct OcAuReleaseArgs {
  CFTypeRef value;
};
struct OcAuRelease { decltype(&::CFRelease) call; OcAuReleaseArgs args; };
extern "C" bool oc_au_release(const OcAuRelease *, uint8_t *, OcNativeFault *) noexcept;
