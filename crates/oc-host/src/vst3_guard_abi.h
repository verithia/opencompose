// The VST3 COM ABI subset used by the native exception wrappers and fixture.
// Rust layout tests compare every process/bus/setup field and interface slot.
#pragma once
#include <cstddef>
#include <cstdint>

#if defined(_WIN32) && (defined(_M_IX86) || defined(__i386__))
#define OC_VST3_CALL __stdcall
#else
#define OC_VST3_CALL
#endif

struct OcVst3Unknown {
  virtual int32_t OC_VST3_CALL queryInterface(const char *iid, void **object) = 0;
  virtual uint32_t OC_VST3_CALL addRef() = 0;
  virtual uint32_t OC_VST3_CALL release() = 0;
};
struct OcVst3FactoryInfo;
struct OcVst3ClassInfo {
  char cid[16]; int32_t cardinality; char category[32]; char name[64];
};
struct OcVst3ClassInfo2 {
  char cid[16]; int32_t cardinality; char category[32]; char name[64];
  uint32_t classFlags; char subCategories[128]; char vendor[64];
  char version[64]; char sdkVersion[64];
};
struct OcVst3Factory : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL getFactoryInfo(OcVst3FactoryInfo *info) = 0;
  virtual int32_t OC_VST3_CALL countClasses() = 0;
  virtual int32_t OC_VST3_CALL getClassInfo(int32_t index, OcVst3ClassInfo *info) = 0;
  virtual int32_t OC_VST3_CALL createInstance(const char *cid, const char *iid, void **object) = 0;
};
struct OcVst3Factory2 : OcVst3Factory {
  virtual int32_t OC_VST3_CALL getClassInfo2(int32_t index, OcVst3ClassInfo2 *info) = 0;
};
struct OcVst3Stream : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL read(void *buffer, int32_t bytes, int32_t *read) = 0;
  virtual int32_t OC_VST3_CALL write(void *buffer, int32_t bytes, int32_t *written) = 0;
  virtual int32_t OC_VST3_CALL seek(int64_t position, int32_t mode, int64_t *result) = 0;
  virtual int32_t OC_VST3_CALL tell(int64_t *position) = 0;
};
struct OcVst3PluginBase : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL initialize(OcVst3Unknown *context) = 0;
  virtual int32_t OC_VST3_CALL terminate() = 0;
};
struct OcVst3AudioBus {
  int32_t numChannels;
  uint64_t silenceFlags;
  union { float **channelBuffers32; double **channelBuffers64; } buffers;
};
struct OcVst3ProcessSetup {
  int32_t processMode;
  int32_t symbolicSampleSize;
  int32_t maxSamplesPerBlock;
  double sampleRate;
};
struct OcVst3ProcessData {
  int32_t processMode;
  int32_t symbolicSampleSize;
  int32_t numSamples;
  int32_t numInputs;
  int32_t numOutputs;
  OcVst3AudioBus *inputs;
  OcVst3AudioBus *outputs;
  void *inputParameterChanges;
  void *outputParameterChanges;
  void *inputEvents;
  void *outputEvents;
  void *processContext;
};
struct OcVst3Processor : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL setBusArrangements(uint64_t *inputs, int32_t input_count, uint64_t *outputs, int32_t output_count) = 0;
  virtual int32_t OC_VST3_CALL getBusArrangement(int32_t direction, int32_t index, uint64_t *arrangement) = 0;
  virtual int32_t OC_VST3_CALL canProcessSampleSize(int32_t size) = 0;
  virtual uint32_t OC_VST3_CALL getLatencySamples() = 0;
  virtual int32_t OC_VST3_CALL setupProcessing(OcVst3ProcessSetup &setup) = 0;
  virtual int32_t OC_VST3_CALL setProcessing(uint8_t state) = 0;
  virtual int32_t OC_VST3_CALL process(OcVst3ProcessData &data) = 0;
  virtual uint32_t OC_VST3_CALL getTailSamples() = 0;
};
struct OcVst3BusInfo {
  int32_t mediaType;
  int32_t direction;
  int32_t channelCount;
  char16_t name[128];
  int32_t busType;
  uint32_t flags;
};
struct OcVst3RoutingInfo;
struct OcVst3Component : OcVst3PluginBase {
  virtual int32_t OC_VST3_CALL getControllerClassId(char *class_id) = 0;
  virtual int32_t OC_VST3_CALL setIoMode(int32_t mode) = 0;
  virtual int32_t OC_VST3_CALL getBusCount(int32_t media, int32_t direction) = 0;
  virtual int32_t OC_VST3_CALL getBusInfo(int32_t media, int32_t direction, int32_t index, OcVst3BusInfo &info) = 0;
  virtual int32_t OC_VST3_CALL getRoutingInfo(OcVst3RoutingInfo &input, OcVst3RoutingInfo &output) = 0;
  virtual int32_t OC_VST3_CALL activateBus(int32_t media, int32_t direction, int32_t index, uint8_t state) = 0;
  virtual int32_t OC_VST3_CALL setActive(uint8_t state) = 0;
  virtual int32_t OC_VST3_CALL setState(OcVst3Stream *stream) = 0;
  virtual int32_t OC_VST3_CALL getState(OcVst3Stream *stream) = 0;
};
struct OcVst3ParameterInfo {
  uint32_t id;
  char16_t title[128];
  char16_t shortTitle[128];
  char16_t units[128];
  int32_t stepCount;
  double defaultNormalizedValue;
  int32_t unitId;
  int32_t flags;
};
struct OcVst3Controller : OcVst3PluginBase {
  virtual int32_t OC_VST3_CALL setComponentState(OcVst3Stream *stream) = 0;
  virtual int32_t OC_VST3_CALL setState(OcVst3Stream *stream) = 0;
  virtual int32_t OC_VST3_CALL getState(OcVst3Stream *stream) = 0;
  virtual int32_t OC_VST3_CALL getParameterCount() = 0;
  virtual int32_t OC_VST3_CALL getParameterInfo(int32_t index, OcVst3ParameterInfo &info) = 0;
  virtual int32_t OC_VST3_CALL getParamStringByValue(uint32_t id, double value, char16_t *text) = 0;
  virtual int32_t OC_VST3_CALL getParamValueByString(uint32_t id, char16_t *text, double &value) = 0;
  virtual double OC_VST3_CALL normalizedParamToPlain(uint32_t id, double value) = 0;
  virtual double OC_VST3_CALL plainParamToNormalized(uint32_t id, double value) = 0;
  virtual double OC_VST3_CALL getParamNormalized(uint32_t id) = 0;
  virtual int32_t OC_VST3_CALL setParamNormalized(uint32_t id, double value) = 0;
  virtual int32_t OC_VST3_CALL setComponentHandler(void *handler) = 0;
  virtual void *OC_VST3_CALL createView(const char *name) = 0;
};
struct OcVst3Connection : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL connect(OcVst3Connection *other) = 0;
  virtual int32_t OC_VST3_CALL disconnect(OcVst3Connection *other) = 0;
  virtual int32_t OC_VST3_CALL notify(void *message) = 0;
};
struct OcVst3ViewRect { int32_t left, top, right, bottom; };
struct OcVst3View : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL isPlatformTypeSupported(const char *) = 0;
  virtual int32_t OC_VST3_CALL attached(void *,const char *) = 0;
  virtual int32_t OC_VST3_CALL removed() = 0;
  virtual int32_t OC_VST3_CALL onWheel(float) = 0;
  virtual int32_t OC_VST3_CALL onKeyDown(char16_t,int16_t,int16_t) = 0;
  virtual int32_t OC_VST3_CALL onKeyUp(char16_t,int16_t,int16_t) = 0;
  virtual int32_t OC_VST3_CALL getSize(OcVst3ViewRect *) = 0;
  virtual int32_t OC_VST3_CALL onSize(OcVst3ViewRect *) = 0;
  virtual int32_t OC_VST3_CALL onFocus(uint8_t) = 0;
  virtual int32_t OC_VST3_CALL setFrame(void *) = 0;
  virtual int32_t OC_VST3_CALL canResize() = 0;
  virtual int32_t OC_VST3_CALL checkSizeConstraint(OcVst3ViewRect *) = 0;
};

struct OcVst3MidiMapping : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL getMidiControllerAssignment(int32_t bus, int16_t channel, int16_t controller, uint32_t &id) = 0;
};
struct OcVst3ExpressionValueDescription { double defaultValue, minimum, maximum; int32_t stepCount; };
struct OcVst3ExpressionInfo {
  uint32_t typeId; char16_t title[128], shortTitle[128], units[128]; int32_t unitId;
  OcVst3ExpressionValueDescription valueDesc; uint32_t associatedParameterId; int32_t flags;
};
struct OcVst3ExpressionController : OcVst3Unknown {
  virtual int32_t OC_VST3_CALL getNoteExpressionCount(int32_t bus, int16_t channel) = 0;
  virtual int32_t OC_VST3_CALL getNoteExpressionInfo(int32_t bus, int16_t channel, int32_t index, OcVst3ExpressionInfo &info) = 0;
};
