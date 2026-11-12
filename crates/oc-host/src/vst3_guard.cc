// Native COM invocation and exception handling stay in one C++ frame.
#include "vst3_guard_abi.h"
#include "native_fault.h"
#include <exception>

#define OC_VST3_GUARD(expression) \
  try { *result = (expression); return true; } \
  catch (const std::exception &error) { oc_native_fault_message(fault, 1, error.what()); return false; } \
  catch (...) { oc_native_fault_message(fault, 2, "unknown C++ exception"); return false; }

extern "C" bool oc_vst3_process(OcVst3Processor *processor, OcVst3ProcessData *data, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->process(*data))
}
extern "C" bool oc_vst3_setup(OcVst3Processor *processor, OcVst3ProcessSetup *setup, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->setupProcessing(*setup))
}
extern "C" bool oc_vst3_processing(OcVst3Processor *processor, uint8_t state, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->setProcessing(state))
}
extern "C" bool oc_vst3_active(OcVst3Component *component, uint8_t state, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->setActive(state))
}
extern "C" bool oc_vst3_component_set_state(OcVst3Component *component, OcVst3Stream *stream, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->setState(stream))
}
extern "C" bool oc_vst3_component_get_state(OcVst3Component *component, OcVst3Stream *stream, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->getState(stream))
}
extern "C" bool oc_vst3_controller_set_state(OcVst3Controller *controller, OcVst3Stream *stream, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(controller->setState(stream))
}
extern "C" bool oc_vst3_controller_get_state(OcVst3Controller *controller, OcVst3Stream *stream, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(controller->getState(stream))
}
extern "C" bool oc_vst3_controller_component_state(OcVst3Controller *controller, OcVst3Stream *stream, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(controller->setComponentState(stream))
}
extern "C" bool oc_vst3_bus_count(OcVst3Component *component, int32_t media, int32_t direction, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->getBusCount(media, direction))
}
extern "C" bool oc_vst3_bus_info(OcVst3Component *component, int32_t media, int32_t direction, int32_t index, OcVst3BusInfo *info, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->getBusInfo(media, direction, index, *info))
}
extern "C" bool oc_vst3_bus_arrangement(OcVst3Processor *processor, int32_t direction, int32_t index, uint64_t *arrangement, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->getBusArrangement(direction, index, arrangement))
}
extern "C" bool oc_vst3_bus_arrangements(OcVst3Processor *processor, uint64_t *inputs, int32_t input_count, uint64_t *outputs, int32_t output_count, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->setBusArrangements(inputs, input_count, outputs, output_count))
}
extern "C" bool oc_vst3_activate_bus(OcVst3Component *component, int32_t media, int32_t direction, int32_t index, uint8_t active, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(component->activateBus(media, direction, index, active))
}
extern "C" bool oc_vst3_latency(OcVst3Processor *processor, uint32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->getLatencySamples())
}
extern "C" bool oc_vst3_tail(OcVst3Processor *processor, uint32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(processor->getTailSamples())
}
extern "C" bool oc_vst3_query(OcVst3Unknown *object, const char *iid, void **output, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->queryInterface(iid,output)) }
extern "C" bool oc_vst3_add_ref(OcVst3Unknown *object, uint32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->addRef()) }
extern "C" bool oc_vst3_release(OcVst3Unknown *object, uint32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->release()) }
extern "C" bool oc_vst3_initialize(OcVst3PluginBase *object, OcVst3Unknown *host, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->initialize(host)) }
extern "C" bool oc_vst3_terminate(OcVst3PluginBase *object, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->terminate()) }
extern "C" bool oc_vst3_connect(OcVst3Connection *object, OcVst3Connection *peer, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->connect(peer)) }
extern "C" bool oc_vst3_disconnect(OcVst3Connection *object, OcVst3Connection *peer, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(object->disconnect(peer)) }

extern "C" bool oc_vst3_parameter_count(OcVst3Controller *controller,int32_t *result,OcNativeFault *fault) noexcept {OC_VST3_GUARD(controller->getParameterCount())}
extern "C" bool oc_vst3_parameter_info(OcVst3Controller *controller,int32_t index,OcVst3ParameterInfo *info,int32_t *result,OcNativeFault *fault) noexcept {OC_VST3_GUARD(controller->getParameterInfo(index,*info))}
extern "C" bool oc_vst3_normalized_to_plain(OcVst3Controller *controller,uint32_t id,double value,double *result,OcNativeFault *fault) noexcept {OC_VST3_GUARD(controller->normalizedParamToPlain(id,value))}
extern "C" bool oc_vst3_plain_to_normalized(OcVst3Controller *controller,uint32_t id,double value,double *result,OcNativeFault *fault) noexcept {OC_VST3_GUARD(controller->plainParamToNormalized(id,value))}
extern "C" bool oc_vst3_set_normalized(OcVst3Controller *controller,uint32_t id,double value,int32_t *result,OcNativeFault *fault) noexcept {OC_VST3_GUARD(controller->setParamNormalized(id,value))}

extern "C" bool oc_vst3_factory_count(OcVst3Factory *factory, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(factory->countClasses()) }
extern "C" bool oc_vst3_factory_class_info(OcVst3Factory *factory, int32_t index, OcVst3ClassInfo *info, int32_t *result, OcNativeFault *fault) noexcept { *info = {}; OC_VST3_GUARD(factory->getClassInfo(index, info)) }
extern "C" bool oc_vst3_factory_class_info2(OcVst3Factory2 *factory, int32_t index, OcVst3ClassInfo2 *info, int32_t *result, OcNativeFault *fault) noexcept { *info = {}; OC_VST3_GUARD(factory->getClassInfo2(index, info)) }
extern "C" bool oc_vst3_factory_create(OcVst3Factory *factory, const char *cid, const char *iid, void **output, int32_t *result, OcNativeFault *fault) noexcept { *output = nullptr; OC_VST3_GUARD(factory->createInstance(cid, iid, output)) }
extern "C" bool oc_vst3_controller_class_id(OcVst3Component *component, char *cid, int32_t *result, OcNativeFault *fault) noexcept { for (int i = 0; i < 16; ++i) cid[i] = 0; OC_VST3_GUARD(component->getControllerClassId(cid)) }
extern "C" bool oc_vst3_get_factory(void *entry, OcVst3Factory **result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(reinterpret_cast<OcVst3Factory *(*)()>(entry)()) }
extern "C" bool oc_vst3_module_entry(void *entry, void *handle, bool *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(reinterpret_cast<bool (*)(void *)>(entry)(handle)) }
extern "C" bool oc_vst3_bundle_entry(void *entry, void *bundle, uint8_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(reinterpret_cast<uint8_t (*)(void *)>(entry)(bundle)) }
extern "C" bool oc_vst3_module_init(void *entry, bool *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(reinterpret_cast<bool (*)()>(entry)()) }
extern "C" bool oc_vst3_module_exit(void *entry, bool *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(reinterpret_cast<bool (*)()>(entry)()) }

extern "C" bool oc_vst3_midi_assignment(OcVst3MidiMapping *mapping, int32_t bus, int16_t channel, int16_t controller, uint32_t *id, int32_t *result, OcNativeFault *fault) noexcept { *id=0; OC_VST3_GUARD(mapping->getMidiControllerAssignment(bus,channel,controller,*id)) }
extern "C" bool oc_vst3_expression_count(OcVst3ExpressionController *controller, int32_t bus, int16_t channel, int32_t *result, OcNativeFault *fault) noexcept { OC_VST3_GUARD(controller->getNoteExpressionCount(bus,channel)) }
extern "C" bool oc_vst3_expression_info(OcVst3ExpressionController *controller, int32_t bus, int16_t channel, int32_t index, OcVst3ExpressionInfo *info, int32_t *result, OcNativeFault *fault) noexcept { *info={}; OC_VST3_GUARD(controller->getNoteExpressionInfo(bus,channel,index,*info)) }

extern "C" bool oc_vst3_view_attached(OcVst3View *view, void *parent, const char *platform, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(view->attached(parent, platform))
}

extern "C" bool oc_vst3_view_removed(OcVst3View *view, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(view->removed())
}

extern "C" bool oc_vst3_view_set_frame(OcVst3View *view, void *frame, int32_t *result, OcNativeFault *fault) noexcept {
  OC_VST3_GUARD(view->setFrame(frame))
}
