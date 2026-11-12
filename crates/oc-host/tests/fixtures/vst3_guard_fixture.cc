// Native COM objects used through Vst3Instance, compiled with optimization.
#include "../../src/vst3_guard_abi.h"
#include "../../src/native_fault.h"
#include <cstring>
#include <cmath>
#include <stdexcept>
#include <limits>
#include <string>
#include <atomic>
#include <thread>
#include <initializer_list>

namespace {
thread_local char thread_marker_storage;
thread_local uint32_t thread_constructions = 0;
uintptr_t thread_marker() { return reinterpret_cast<uintptr_t>(&thread_marker_storage); }
struct Plugin;
struct Component final : OcVst3Component {
  Plugin *owner;
  explicit Component(Plugin *p) : owner(p) {}
  int32_t OC_VST3_CALL queryInterface(const char *, void **) override;
  uint32_t OC_VST3_CALL addRef() override;
  uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL initialize(OcVst3Unknown *) override;
  int32_t OC_VST3_CALL terminate() override;
  int32_t OC_VST3_CALL getControllerClassId(char *id) override;
  int32_t OC_VST3_CALL setIoMode(int32_t) override { return 0; }
  int32_t OC_VST3_CALL getBusCount(int32_t media, int32_t direction) override;
  int32_t OC_VST3_CALL getBusInfo(int32_t media, int32_t direction, int32_t index, OcVst3BusInfo &info) override;
  int32_t OC_VST3_CALL getRoutingInfo(OcVst3RoutingInfo &, OcVst3RoutingInfo &) override;
  int32_t OC_VST3_CALL activateBus(int32_t, int32_t, int32_t, uint8_t) override;
  int32_t OC_VST3_CALL setActive(uint8_t state) override;
  int32_t OC_VST3_CALL setState(OcVst3Stream *stream) override;
  int32_t OC_VST3_CALL getState(OcVst3Stream *stream) override;
};
struct Processor final : OcVst3Processor {
  Plugin *owner;
  explicit Processor(Plugin *p) : owner(p) {}
  int32_t OC_VST3_CALL queryInterface(const char *, void **) override;
  uint32_t OC_VST3_CALL addRef() override;
  uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL setBusArrangements(uint64_t *, int32_t, uint64_t *, int32_t) override;
  int32_t OC_VST3_CALL getBusArrangement(int32_t, int32_t, uint64_t *arrangement) override;
  int32_t OC_VST3_CALL canProcessSampleSize(int32_t size) override { return size == 0 ? 0 : 1; }
  uint32_t OC_VST3_CALL getLatencySamples() override;
  int32_t OC_VST3_CALL setupProcessing(OcVst3ProcessSetup &setup) override;
  int32_t OC_VST3_CALL setProcessing(uint8_t state) override;
  int32_t OC_VST3_CALL process(OcVst3ProcessData &data) override;
  uint32_t OC_VST3_CALL getTailSamples() override;
};
struct Controller final : OcVst3Controller {
  Plugin *owner;
  explicit Controller(Plugin *p) : owner(p) {}
  int32_t OC_VST3_CALL queryInterface(const char *, void **) override;
  uint32_t OC_VST3_CALL addRef() override;
  uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL initialize(OcVst3Unknown *) override;
  int32_t OC_VST3_CALL terminate() override;
  int32_t OC_VST3_CALL setComponentState(OcVst3Stream *stream) override;
  int32_t OC_VST3_CALL setState(OcVst3Stream *stream) override;
  int32_t OC_VST3_CALL getState(OcVst3Stream *stream) override;
  int32_t OC_VST3_CALL getParameterCount() override;
  int32_t OC_VST3_CALL getParameterInfo(int32_t, OcVst3ParameterInfo &) override;
  int32_t OC_VST3_CALL getParamStringByValue(uint32_t, double, char16_t *) override;
  int32_t OC_VST3_CALL getParamValueByString(uint32_t, char16_t *, double &) override;
  double OC_VST3_CALL normalizedParamToPlain(uint32_t, double) override;
  double OC_VST3_CALL plainParamToNormalized(uint32_t, double) override;
  double OC_VST3_CALL getParamNormalized(uint32_t) override;
  int32_t OC_VST3_CALL setParamNormalized(uint32_t, double value) override;
  int32_t OC_VST3_CALL setComponentHandler(void *) override { return 0; }
  void *OC_VST3_CALL createView(const char *) override { return nullptr; }
};
struct Connection final : OcVst3Connection {
  Plugin *owner; uint32_t role;
  Connection(Plugin *p,uint32_t r):owner(p),role(r){}
  int32_t OC_VST3_CALL queryInterface(const char *,void **) override;
  uint32_t OC_VST3_CALL addRef() override;
  uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL connect(OcVst3Connection *) override;
  int32_t OC_VST3_CALL disconnect(OcVst3Connection *) override;
  int32_t OC_VST3_CALL notify(void *) override {return 0;}
};
struct MidiMapping final : OcVst3MidiMapping {
  Plugin *owner; explicit MidiMapping(Plugin *p):owner(p){}
  int32_t OC_VST3_CALL queryInterface(const char *,void **) override;
  uint32_t OC_VST3_CALL addRef() override; uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL getMidiControllerAssignment(int32_t,int16_t,int16_t,uint32_t &) override;
};
struct Expressions final : OcVst3ExpressionController {
  Plugin *owner; explicit Expressions(Plugin *p):owner(p){}
  int32_t OC_VST3_CALL queryInterface(const char *,void **) override;
  uint32_t OC_VST3_CALL addRef() override; uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL getNoteExpressionCount(int32_t,int16_t) override;
  int32_t OC_VST3_CALL getNoteExpressionInfo(int32_t,int16_t,int32_t,OcVst3ExpressionInfo &) override;
};
struct View final : OcVst3View {
  Plugin *owner; explicit View(Plugin *p):owner(p){}
  int32_t OC_VST3_CALL queryInterface(const char *,void **) override;
  uint32_t OC_VST3_CALL addRef() override;
  uint32_t OC_VST3_CALL release() override;
  int32_t OC_VST3_CALL isPlatformTypeSupported(const char *) override {return 0;}
  int32_t OC_VST3_CALL attached(void *,const char *) override;
  int32_t OC_VST3_CALL removed() override;
  int32_t OC_VST3_CALL onWheel(float) override {return 0;}
  int32_t OC_VST3_CALL onKeyDown(char16_t,int16_t,int16_t) override {return 0;}
  int32_t OC_VST3_CALL onKeyUp(char16_t,int16_t,int16_t) override {return 0;}
  int32_t OC_VST3_CALL getSize(OcVst3ViewRect *r) override {*r={0,0,640,480};return 0;}
  int32_t OC_VST3_CALL onSize(OcVst3ViewRect *) override;
  int32_t OC_VST3_CALL onFocus(uint8_t) override {return 0;}
  int32_t OC_VST3_CALL setFrame(void *) override;
  int32_t OC_VST3_CALL canResize() override {return 0;}
  int32_t OC_VST3_CALL checkSizeConstraint(OcVst3ViewRect *) override {return 0;}
};
// Typed C-ABI slots preserve foreign COM dispatch without assuming a C++ dynamic type.
struct FixtureHandler;
struct FixtureHandlerVTable {
  int32_t (OC_VST3_CALL *queryInterface)(FixtureHandler *, const char *, void **);
  uint32_t (OC_VST3_CALL *addRef)(FixtureHandler *);
  uint32_t (OC_VST3_CALL *release)(FixtureHandler *);
  int32_t (OC_VST3_CALL *beginEdit)(FixtureHandler *, uint32_t);
  int32_t (OC_VST3_CALL *performEdit)(FixtureHandler *, uint32_t, double);
  int32_t (OC_VST3_CALL *endEdit)(FixtureHandler *, uint32_t);
  int32_t (OC_VST3_CALL *restartComponent)(FixtureHandler *, int32_t);
};
struct FixtureHandler { const FixtureHandlerVTable *vtbl; };
static_assert(sizeof(FixtureHandlerVTable) == 7 * sizeof(void (*)()));
static_assert(offsetof(FixtureHandlerVTable, restartComponent) == 6 * sizeof(void (*)()));
static_assert(offsetof(FixtureHandler, vtbl) == 0);
struct QueueObject;
struct ChangesObject;
struct QueueVTable {
  int32_t (OC_VST3_CALL *query)(QueueObject *, const char *, void **);
  uint32_t (OC_VST3_CALL *addRef)(QueueObject *);
  uint32_t (OC_VST3_CALL *release)(QueueObject *);
  uint32_t (OC_VST3_CALL *id)(QueueObject *);
  int32_t (OC_VST3_CALL *count)(QueueObject *);
  int32_t (OC_VST3_CALL *point)(QueueObject *, int32_t, int32_t *, double *);
  int32_t (OC_VST3_CALL *add)(QueueObject *, int32_t, double, int32_t *);
};
struct QueueObject { const QueueVTable *vtbl; };
struct ChangesVTable {
  int32_t (OC_VST3_CALL *query)(ChangesObject *, const char *, void **);
  uint32_t (OC_VST3_CALL *addRef)(ChangesObject *);
  uint32_t (OC_VST3_CALL *release)(ChangesObject *);
  int32_t (OC_VST3_CALL *count)(ChangesObject *);
  QueueObject *(OC_VST3_CALL *data)(ChangesObject *, int32_t);
  QueueObject *(OC_VST3_CALL *add)(ChangesObject *, const uint32_t *, int32_t *);
};
struct ChangesObject { const ChangesVTable *vtbl; };
static_assert(sizeof(QueueVTable) == 7 * sizeof(void (*)()));
static_assert(offsetof(QueueVTable, point) == 5 * sizeof(void (*)()));
static_assert(sizeof(ChangesVTable) == 6 * sizeof(void (*)()));
static_assert(offsetof(ChangesVTable, data) == 4 * sizeof(void (*)()));
struct Plugin {
  Component component{this};
  Processor processor{this};
  Controller controller{this};
  View view{this};
  MidiMapping midi{this};
  Expressions expressions{this};
  Connection component_connection{this,3};
  Connection controller_connection{this,4};
  bool connections = false;
  uintptr_t last_hosts[2] = {};
  uint32_t refs = 3;
  uint32_t fault_point = 0;
  uint32_t fault_kind = 0;
  std::atomic<uint32_t> calls[64] = {};
  std::atomic<uintptr_t> last_threads[64] = {};
  const uintptr_t ui_thread = thread_marker();
  std::atomic<uint32_t> ui_violations{0};
  std::atomic<bool> hold_process{false};
  std::atomic<bool> process_entered{false};
  unsigned char iids[6][16] = {};
  uint32_t secondary_point = 0;
  uint32_t secondary_kind = 0;
  uint32_t parameter_mode = 0;
  uint32_t preparation_mutation = 0;
  uint64_t delivered_queues = 0, delivered_points = 0, delivered_digest = 0;
  uint64_t midi_words[64][6] = {}, midi_parameters[64][3] = {};
  uint32_t midi_events = 0, midi_points = 0;
  uint32_t restart_point = 0;
  int32_t restart_flags = 0;
  FixtureHandler *parameter_handler = nullptr;
  double parameter_values[8] = {};
  uint32_t last_parameter = 0;
  void parameter_callback(uint32_t point) {
    if(point==restart_point && parameter_handler) parameter_handler->vtbl->restartComponent(parameter_handler, restart_flags);
  }
  int32_t refusal;
  int32_t not_implemented;
  double gain = 0.25;
  double controller_gain = 0.5;
  bool effect;
  int32_t channels = 2;
  bool active = true;
  bool processing = true;
  explicit Plugin(bool is_effect, int32_t fail, int32_t unimplemented)
      : refusal(fail), not_implemented(unimplemented), effect(is_effect) {}
  int32_t enter(uint32_t point) {
    if (point < 64) {
      ++calls[point];
      last_threads[point].store(thread_marker(), std::memory_order_release);
      if (point != 1 && point != 3 && thread_marker() != ui_thread) ++ui_violations;
    }
    if (point == 1) {
      process_entered.store(true, std::memory_order_release);
      while (hold_process.load(std::memory_order_acquire)) std::this_thread::yield();
      process_entered.store(false, std::memory_order_release);
    }
    const uint32_t kind = point == fault_point ? fault_kind : (point == secondary_point ? secondary_kind : 0);
    if (kind == 1) throw std::runtime_error("optimized native VST3 method exception");
    if (kind == 2) throw 17;
    if (kind == 3) throw std::runtime_error(std::string(4096, 'x'));
    return kind == 4 ? refusal : 0;
  }
};
int32_t query(Plugin *owner,uint32_t role,const char *iid,void **out) {
  *out=nullptr;
  if (std::memcmp(iid,owner->iids[0],16)==0) *out=&owner->component;
  else if (std::memcmp(iid,owner->iids[1],16)==0) *out=&owner->processor;
  else if (std::memcmp(iid,owner->iids[2],16)==0) *out=&owner->controller;
  else if(owner->parameter_mode>=11 && std::memcmp(iid,owner->iids[4],16)==0) *out=&owner->midi;
  else if(owner->parameter_mode>=11 && std::memcmp(iid,owner->iids[5],16)==0) *out=&owner->expressions;
  else if (owner->connections && std::memcmp(iid,owner->iids[3],16)==0) *out=role==2 || role==4 ? &owner->controller_connection : &owner->component_connection;
  if (*out) ++owner->refs;
  const int32_t result=owner->enter(20);
  return result!=0 ? result : (*out ? 0 : owner->not_implemented);
}
#define REFERENCES(Type,Role) \
int32_t OC_VST3_CALL Type::queryInterface(const char *iid, void **out) { return query(owner,Role,iid,out); } \
uint32_t OC_VST3_CALL Type::addRef() { ++owner->refs; owner->enter(19); return owner->refs; } \
uint32_t OC_VST3_CALL Type::release() { --owner->refs; owner->enter(11); return owner->refs; }
REFERENCES(Component,0)
REFERENCES(Processor,1)
REFERENCES(Controller,2)
REFERENCES(MidiMapping,5)
REFERENCES(Expressions,6)
int32_t OC_VST3_CALL MidiMapping::getMidiControllerAssignment(int32_t bus,int16_t channel,int16_t controller,uint32_t &id) {
  const int32_t result=owner->enter(43); owner->parameter_callback(43); if(result!=0)return result;
  if(bus!=0 || channel<0 || channel>15)return owner->not_implemented;
  switch(controller) {
    case 1:id=100;break; case 33:if(owner->parameter_mode==14)return owner->not_implemented;id=101;break; case 7:id=102;break;
    case 128:id=103;break;case 129:id=104;break;case 130:id=107;break;
    default:return owner->not_implemented;
  }
  return 0;
}
int32_t OC_VST3_CALL Expressions::getNoteExpressionCount(int32_t bus,int16_t channel) {
  owner->enter(44);owner->parameter_callback(44);
  if(bus!=0 || channel<0 || channel>15)return 0;
  return owner->parameter_mode==12 ? 257 : 6;
}
int32_t OC_VST3_CALL Expressions::getNoteExpressionInfo(int32_t bus,int16_t channel,int32_t index,OcVst3ExpressionInfo &info) {
  const int32_t result=owner->enter(45);owner->parameter_callback(45);if(result!=0)return result;
  if(bus!=0 || channel<0 || channel>15 || index<0 || index>=6)return owner->not_implemented;
  info={};info.typeId=uint32_t(index);info.valueDesc={0.5,0.0,1.0,0};
  if(owner->parameter_mode==13)info.valueDesc.maximum=std::numeric_limits<double>::quiet_NaN();
  return 0;
}
int32_t OC_VST3_CALL Connection::queryInterface(const char *iid,void **out) {return query(owner,role,iid,out);}
uint32_t OC_VST3_CALL Connection::addRef(){++owner->refs;owner->enter(19);return owner->refs;}
uint32_t OC_VST3_CALL Connection::release(){--owner->refs;owner->enter(11);return owner->refs;}
int32_t OC_VST3_CALL Connection::connect(OcVst3Connection *peer){
  if(peer != (role==3 ? &owner->controller_connection : &owner->component_connection)) throw std::runtime_error("native connection peer changed");
  return owner->enter(role==3 ? 23 : 24);
}
int32_t OC_VST3_CALL Connection::disconnect(OcVst3Connection *peer){
  if(peer != (role==3 ? &owner->controller_connection : &owner->component_connection)) throw std::runtime_error("native disconnect peer changed");
  return owner->enter(role==3 ? 25 : 26);
}
int32_t OC_VST3_CALL View::queryInterface(const char *iid,void **out){return query(owner,5,iid,out);}
uint32_t OC_VST3_CALL View::addRef(){++owner->refs;owner->enter(19);return owner->refs;}
uint32_t OC_VST3_CALL View::release(){++owner->calls[36];--owner->refs;owner->enter(11);return owner->refs;}
int32_t OC_VST3_CALL View::attached(void *,const char *){return owner->enter(29);}
int32_t OC_VST3_CALL View::setFrame(void *){return owner->enter(46);}
int32_t OC_VST3_CALL View::onSize(OcVst3ViewRect *){return owner->enter(30);}
int32_t OC_VST3_CALL View::removed(){return owner->enter(31);}
int32_t OC_VST3_CALL Component::getControllerClassId(char *id) {
  std::memset(id, 7, 16);
  return owner->enter(42);
}
int32_t OC_VST3_CALL Component::initialize(OcVst3Unknown *host) {owner->last_hosts[0]=reinterpret_cast<uintptr_t>(host);return owner->enter(21);}
int32_t OC_VST3_CALL Controller::initialize(OcVst3Unknown *host) {owner->last_hosts[1]=reinterpret_cast<uintptr_t>(host);return owner->enter(22);}
int32_t OC_VST3_CALL Component::terminate() {const int32_t result=owner->enter(28);return result!=0 ? result : owner->enter(10);}
int32_t OC_VST3_CALL Controller::terminate() {const int32_t result=owner->enter(27);return result!=0 ? result : owner->enter(10);}
int32_t OC_VST3_CALL Component::getBusCount(int32_t media, int32_t direction) {
  const int32_t result = owner->enter(14); if (result != 0) return result;
  if(media==1)return direction==0 && owner->parameter_mode>=11 && owner->parameter_mode!=15 ? 1 : 0;
  if (media != 0) return 0;
  if (owner->parameter_mode==19 && direction==1) return 2;
  return direction == 1 || owner->effect ? 1 : 0;
}
int32_t OC_VST3_CALL Component::getBusInfo(int32_t media, int32_t direction, int32_t index, OcVst3BusInfo &info) {
  const int32_t result = owner->enter(15); if (result != 0) return result;
  if(media==1 && direction==0 && index==0 && owner->parameter_mode>=11 && owner->parameter_mode!=15) {
    info={};info.mediaType=1;info.direction=0;info.channelCount=owner->parameter_mode==16 ? 2 : (owner->parameter_mode==17 ? 17 : (owner->parameter_mode==18 ? -1 : 16));info.flags=1;return 0;
  }
  if (media != 0 || index < 0 || index >= (owner->parameter_mode==19 && direction==1 ? 2 : 1) || (direction == 0 && !owner->effect)) return owner->refusal;
  std::memset(&info, 0, sizeof(info));
  info.mediaType = media; info.direction = direction; info.channelCount = owner->parameter_mode==19 ? (index==0 ? 1 : 2) : owner->channels; info.flags = 1;
  info.name[0] = u'N'; info.name[1] = u'a'; info.name[2] = u't'; info.name[3] = u'i'; info.name[4] = u'v'; info.name[5] = u'e';
  return 0;
}
int32_t OC_VST3_CALL Component::getRoutingInfo(OcVst3RoutingInfo &, OcVst3RoutingInfo &) { return owner->not_implemented; }
int32_t OC_VST3_CALL Component::setActive(uint8_t state) {
  int32_t result = owner->enter(4); if (result != 0) return result;
  owner->active = state != 0; return 0;
}
uint32_t OC_VST3_CALL Processor::getLatencySamples() { owner->enter(12); return owner->parameter_mode==7 ? (owner->active ? 31+uint32_t(owner->parameter_values[0]*100.0) : 0) : 31; }
uint32_t OC_VST3_CALL Processor::getTailSamples() { owner->enter(13); return owner->parameter_mode==7 ? (owner->active ? 37+uint32_t(owner->parameter_values[1]*1000.0) : 0) : 37; }
int32_t OC_VST3_CALL Processor::getBusArrangement(int32_t direction, int32_t index, uint64_t *arrangement) {
  const int32_t result = owner->enter(16); if (result != 0) return result;
  if (direction != 1 || index != 0 || arrangement == nullptr) throw std::runtime_error("native bus arrangement arguments changed");
  *arrangement = 3; return 0;
}
int32_t OC_VST3_CALL Processor::setBusArrangements(uint64_t *inputs, int32_t input_count, uint64_t *outputs, int32_t output_count) {
  const int32_t result = owner->enter(17); if (result != 0) return result;
  if (input_count != 0 || inputs != nullptr || output_count != 1 || outputs == nullptr || outputs[0] != 3) throw std::runtime_error("native bus arrangements arguments changed");
  return 0;
}
int32_t OC_VST3_CALL Component::activateBus(int32_t media, int32_t direction, int32_t index, uint8_t active) {
  const int32_t result = owner->enter(18); if (result != 0) return result;
  if ((index != 0 && !(owner->parameter_mode==19 && media==0 && direction==1 && index==1)) || active != 1 || !((media == 0 && direction == 1) || (media == 1 && direction == 0))) throw std::runtime_error("native activate bus arguments changed");
  return 0;
}
int32_t OC_VST3_CALL Processor::setupProcessing(OcVst3ProcessSetup &setup) {
  if (setup.sampleRate != 48000.0 || setup.maxSamplesPerBlock != 512 || setup.symbolicSampleSize != 0)
    throw std::runtime_error("native setup arguments changed");
  return owner->enter(2);
}
int32_t OC_VST3_CALL Processor::setProcessing(uint8_t state) {
  int32_t result = owner->enter(3); if (result != 0) return result;
  owner->processing = state != 0; return 0;
}
static double validate_delivery(Plugin *owner, void *raw) {
  auto changes = static_cast<ChangesObject *>(raw);
  if(changes == nullptr)throw std::runtime_error("missing native parameter changes");
  const bool repeated = owner->parameter_mode == 9;
  const int32_t count = changes->vtbl->count(changes);
  if(count != (repeated ? 1 : 16384))throw std::runtime_error("native parameter queue count changed");
  owner->delivered_queues=count;owner->delivered_points=0;
  uint64_t digest=14695981039346656037ULL;
  double last=0;
  for(int32_t queue=0;queue<count;++queue) {
    auto values=changes->vtbl->data(changes,queue);
    if(values==nullptr)throw std::runtime_error("missing native value queue");
    const uint32_t id=values->vtbl->id(values);
    if(id != (repeated ? 100u : 1000u+uint32_t(queue)))throw std::runtime_error("native parameter order changed");
    const int32_t points=values->vtbl->count(values);
    if(points != (repeated ? 1024 : 4))throw std::runtime_error("native point count changed");
    for(int32_t point=0;point<points;++point) {
      int32_t offset=-1;double value=-1;
      if(values->vtbl->point(values,point,&offset,&value)!=0)throw std::runtime_error("native point read failed");
      const int32_t j=point-1;
      const int32_t expected_offset=repeated ? (point==0 ? 0 : (j<255 ? 1+j/17 : 16+(j-255)/16)) : point*16;
      const double expected_value=repeated ? (point==0 ? 0.25 : 0.5) : double(4-point)/8.0;
      if(offset!=expected_offset || value!=expected_value)throw std::runtime_error("native point offset or value changed");
      uint64_t bits=0;std::memcpy(&bits,&value,sizeof(bits));
      for(uint64_t item:{uint64_t(id),uint64_t(offset),bits}) {digest^=item;digest*=1099511628211ULL;}
      ++owner->delivered_points;last=value;
    }
  }
  owner->delivered_digest=digest;
  return last;
}
struct FixtureNoteOn { int16_t channel, pitch; float tuning, velocity; int32_t length, note_id; };
struct FixtureNoteOff { int16_t channel, pitch; float velocity; int32_t note_id; float tuning; };
struct FixturePolyPressure { int16_t channel, pitch; float pressure; int32_t note_id; };
struct FixtureNoteExpression { uint32_t type_id; int32_t note_id; double value; };
union FixtureEventPayload { FixtureNoteOn note_on; FixtureNoteOff note_off; FixturePolyPressure pressure; FixtureNoteExpression expression; uint8_t bytes[24]; uint64_t alignment; };
struct FixtureEvent { int32_t bus, offset; double ppq; uint16_t flags, type; FixtureEventPayload payload; };
struct FixtureEventList;
struct FixtureEventListVTable {
  int32_t (OC_VST3_CALL *query)(FixtureEventList *, const char *, void **);
  uint32_t (OC_VST3_CALL *add_ref)(FixtureEventList *);
  uint32_t (OC_VST3_CALL *release)(FixtureEventList *);
  int32_t (OC_VST3_CALL *count)(FixtureEventList *);
  int32_t (OC_VST3_CALL *event)(FixtureEventList *, int32_t, FixtureEvent *);
  int32_t (OC_VST3_CALL *add_event)(FixtureEventList *, FixtureEvent *);
};
// Rust owns this COM object. Dispatch through its C-layout ABI rather than an
// internal-linkage abstract C++ class with no native concrete implementation.
struct FixtureEventList { const FixtureEventListVTable *vtbl; };
double validate_tuning_delivery(void *raw) {
  if(raw == nullptr)throw std::runtime_error("missing note-expression event list");
  auto *events=static_cast<FixtureEventList *>(raw);
  if(events->vtbl->count(events)!=2)throw std::runtime_error("tuning event count changed");
  FixtureEvent note{}, expression{};
  if(events->vtbl->event(events,0,&note)!=0 || events->vtbl->event(events,1,&expression)!=0)
    throw std::runtime_error("tuning event read refused");
  if(note.type!=0 || note.bus!=0 || note.offset!=3 || note.payload.note_on.channel!=2 || note.payload.note_on.pitch!=64 || note.payload.note_on.note_id!=0)
    throw std::runtime_error("tuning target note/channel/identity changed");
  if(expression.type!=4 || expression.bus!=0 || expression.offset!=5 || expression.payload.expression.type_id!=2 || expression.payload.expression.note_id!=note.payload.note_on.note_id)
    throw std::runtime_error("tuning expression association changed");
  const double normalized=expression.payload.expression.value;
  if(!std::isfinite(normalized) || normalized<0 || normalized>1)
    throw std::runtime_error("native tuning payload is not normalized");
  return std::exp2((240.0*(normalized-0.5))/12.0);
}
uint64_t double_bits(double value) {uint64_t bits=0;std::memcpy(&bits,&value,sizeof(bits));return bits;}
void capture_midi_payload(Plugin *owner,OcVst3ProcessData &data) {
  owner->midi_events=0;owner->midi_points=0;
  auto *events=static_cast<FixtureEventList *>(data.inputEvents);
  if(events) {
    const int32_t count=events->vtbl->count(events);
    if(count<0 || count>65536)throw std::runtime_error("invalid native MIDI event count");
    owner->midi_events=uint32_t(count);
    for(int32_t index=0;index<count;++index) {
      FixtureEvent event{};if(events->vtbl->event(events,index,&event)!=0)throw std::runtime_error("native MIDI event read refused");
      if(index>=64)continue;
      auto &out=owner->midi_words[index];out[0]=event.type;out[1]=uint64_t(event.offset);
      if(event.type==0){out[2]=event.payload.note_on.channel;out[3]=event.payload.note_on.pitch;out[4]=uint64_t(event.payload.note_on.note_id);out[5]=double_bits(event.payload.note_on.velocity);}
      else if(event.type==1){out[2]=event.payload.note_off.channel;out[3]=event.payload.note_off.pitch;out[4]=uint64_t(event.payload.note_off.note_id);out[5]=double_bits(event.payload.note_off.velocity);}
      else if(event.type==3){out[2]=event.payload.pressure.channel;out[3]=event.payload.pressure.pitch;out[4]=uint64_t(event.payload.pressure.note_id);out[5]=double_bits(event.payload.pressure.pressure);}
      else if(event.type==4){out[2]=event.payload.expression.type_id;out[3]=uint64_t(event.payload.expression.note_id);out[4]=double_bits(event.payload.expression.value);}
      else throw std::runtime_error("unexpected native MIDI event type");
    }
  }
  auto *parameters=static_cast<ChangesObject *>(data.inputParameterChanges);
  if(parameters) {
    const int32_t count=parameters->vtbl->count(parameters);
    if(count<0 || count>16384)throw std::runtime_error("invalid native MIDI parameter count");
    for(int32_t index=0;index<count;++index) {
      auto *queue=parameters->vtbl->data(parameters,index);if(!queue)throw std::runtime_error("missing MIDI parameter queue");
      const uint32_t id=queue->vtbl->id(queue);const int32_t points=queue->vtbl->count(queue);
      if(points<0 || points>1024)throw std::runtime_error("invalid MIDI point count");
      for(int32_t point=0;point<points;++point) {
        int32_t offset=0;double value=0;if(queue->vtbl->point(queue,point,&offset,&value)!=0)throw std::runtime_error("MIDI point read refused");
        if(owner->midi_points<64){auto &out=owner->midi_parameters[owner->midi_points];out[0]=id;out[1]=uint64_t(offset);out[2]=double_bits(value);}
        ++owner->midi_points;
      }
    }
  }
}
int32_t OC_VST3_CALL Processor::process(OcVst3ProcessData &data) {
  if (data.symbolicSampleSize != 0 || data.numOutputs != (owner->parameter_mode==19 ? 2 : 1) || data.outputs == nullptr || data.outputs[0].numChannels != (owner->parameter_mode==19 ? 1 : 2)
      || (owner->parameter_mode==19 && data.outputs[1].numChannels!=2))
    throw std::runtime_error("native process ABI arguments changed");
  if (!owner->active || !owner->processing) throw std::runtime_error("native process while inactive");
  if (data.numSamples > 0) data.outputs[0].buffers.channelBuffers32[0][0] = 777.0f;
  const int32_t result = owner->enter(1);
  owner->parameter_callback(1);
  if (result != 0) return result;
  if(owner->parameter_mode==8 || owner->parameter_mode==9)owner->gain=validate_delivery(owner,data.inputParameterChanges);
  if(owner->parameter_mode==10)owner->gain=validate_tuning_delivery(data.inputEvents);
  if(owner->parameter_mode>=11)capture_midi_payload(owner,data);
  // The multi-output fixture applies the final delivered Gain point, so audit
  // PCM proves equal-offset native queue ordering, not only Rust preparation.
  if(owner->parameter_mode==19)for(uint32_t index=0;index<owner->midi_points && index<64;++index) {
    if(owner->midi_parameters[index][0]==100)std::memcpy(&owner->gain,&owner->midi_parameters[index][2],sizeof(double));
  }
  int32_t flat_channel=0;
  for(int32_t bus=0;bus<data.numOutputs;++bus) {
    for (int32_t channel=0;channel<data.outputs[bus].numChannels;++channel,++flat_channel) {
      for (int32_t frame=0;frame<data.numSamples;++frame) {
        float input=static_cast<float>(flat_channel+1);
        if(owner->effect && data.numInputs>0 && data.inputs!=nullptr)
          input=data.inputs[0].buffers.channelBuffers32[channel][frame];
        data.outputs[bus].buffers.channelBuffers32[channel][frame]=input*static_cast<float>(owner->gain);
      }
    }
  }
  return 0;
}
int32_t stream_read(OcVst3Stream *stream, double &value, int32_t refusal) {
  int32_t bytes = 0; const int32_t result = stream->read(&value, sizeof(value), &bytes);
  return result == 0 && bytes == sizeof(value) ? 0 : refusal;
}
int32_t stream_write(OcVst3Stream *stream, double value, int32_t refusal) {
  int32_t bytes = 0; const int32_t result = stream->write(&value, sizeof(value), &bytes);
  return result == 0 && bytes == sizeof(value) ? 0 : refusal;
}
int32_t OC_VST3_CALL Component::setState(OcVst3Stream *stream) {
  const int32_t result = owner->enter(5); if (result != 0) return result;
  return stream_read(stream, owner->gain, owner->refusal);
}
int32_t OC_VST3_CALL Component::getState(OcVst3Stream *stream) {
  const int32_t result = owner->enter(6); if (result != 0) return result;
  return stream_write(stream, owner->gain, owner->refusal);
}
int32_t OC_VST3_CALL Controller::setState(OcVst3Stream *stream) {
  const int32_t result = owner->enter(7); if (result != 0) return result;
  return stream_read(stream, owner->controller_gain, owner->refusal);
}
int32_t OC_VST3_CALL Controller::getState(OcVst3Stream *stream) {
  const int32_t result = owner->enter(8); if (result != 0) return result;
  return stream_write(stream, owner->controller_gain, owner->refusal);
}
int32_t OC_VST3_CALL Controller::setComponentState(OcVst3Stream *stream) {
  const int32_t result = owner->enter(9); if (result != 0) return result;
  return stream_read(stream, owner->controller_gain, owner->refusal);
}
int32_t OC_VST3_CALL Controller::getParameterCount() {
  owner->enter(37);owner->parameter_callback(37);return owner->parameter_mode==0 ? 0 : (owner->parameter_mode==8 ? 16384 : 8);
}
int32_t OC_VST3_CALL Controller::getParameterInfo(int32_t index, OcVst3ParameterInfo &info) {
  const int32_t result=owner->enter(38);if(result!=0)return result;
  owner->parameter_callback(38);
  if(owner->parameter_mode==8) {
    if(index<0 || index>=16384)return owner->not_implemented;
    std::memset(&info,0,sizeof(info)); info.id=1000+uint32_t(index);info.defaultNormalizedValue=0.5;info.flags=1;return 0;
  }
  if(owner->parameter_mode==0 || index<0 || index>=8)return owner->not_implemented;
  const char16_t *names[]={u"Gain",u"Hidden Depth",u"Read Only",u"Rate-Hz",u"Rate Hz",u"Gain Amount",u"Bad Plain Range",u"\u660e\u308b\u3055"};
  const uint32_t ids[]={100,101,102,103,104,100,106,107};
  std::memset(&info,0,sizeof(info));info.id=ids[index];info.defaultNormalizedValue=0.5;
  info.flags=index==1 ? 16 : (index==2 ? 2 : 0);
  if(((owner->parameter_mode==5 || owner->parameter_mode==7 || owner->parameter_mode==9) && index!=2) || owner->parameter_mode==6 || (owner->parameter_mode>=11 && index!=2 && index!=7))info.flags|=1;
  if(owner->parameter_mode==7 && owner->preparation_mutation==1 && owner->calls[41].load()!=0 && index==1)info.flags|=2;
  if(owner->parameter_mode>=11 && info.id==107)info.stepCount=15;
  if(owner->parameter_mode==7 && owner->preparation_mutation==3)info.defaultNormalizedValue=1.5;
  if(owner->parameter_mode==7 && owner->preparation_mutation==4)info.stepCount=-1;
  const char16_t *name=names[index];for(int i=0;name[i] && i<127;++i)info.title[i]=name[i];
  return 0;
}
double OC_VST3_CALL Controller::normalizedParamToPlain(uint32_t id,double value) {
  owner->enter(39);owner->parameter_callback(39);
  if(owner->parameter_mode==0)return value;
  if(id==106)return std::numeric_limits<double>::quiet_NaN();
  if(id==101)return value*24.0-12.0;
  return owner->parameter_mode==4 ? (1.0-value)*100.0 : value*100.0;
}
double OC_VST3_CALL Controller::plainParamToNormalized(uint32_t id,double value) {
  owner->enter(40);owner->parameter_callback(40);
  if(owner->parameter_mode==0)return value;
  if(owner->parameter_mode==2)return std::numeric_limits<double>::quiet_NaN();
  if(owner->parameter_mode==3)return 2.0;
  if(id==101)return (value+12.0)/24.0;
  return owner->parameter_mode==4 ? 1.0-value/100.0 : value/100.0;
}
int32_t OC_VST3_CALL Controller::getParamStringByValue(uint32_t, double, char16_t *) { return owner->not_implemented; }
int32_t OC_VST3_CALL Controller::getParamValueByString(uint32_t, char16_t *, double &) { return owner->not_implemented; }
double OC_VST3_CALL Controller::getParamNormalized(uint32_t) { return owner->controller_gain; }
int32_t OC_VST3_CALL Controller::setParamNormalized(uint32_t id, double value) {
  const int32_t result=owner->enter(41);if(result!=0)return result;
  owner->parameter_callback(41);owner->last_parameter=id;
  if(owner->parameter_mode!=0 && id>=100 && id<108)owner->parameter_values[id-100]=value;
  owner->controller_gain=value;
  if(owner->parameter_mode==7 && owner->preparation_mutation==2 && owner->parameter_handler)
    owner->parameter_handler->vtbl->restartComponent(owner->parameter_handler, owner->restart_flags);
  return 0;
}
} // namespace

extern "C" void *oc_vst3_fixture_create(bool effect, int32_t refusal, int32_t not_implemented, OcVst3Component **component, OcVst3Processor **processor, OcVst3Controller **controller) noexcept {
  try {
    auto *plugin = new Plugin(effect, refusal, not_implemented);
    ++thread_constructions;
    *component = &plugin->component; *processor = &plugin->processor; *controller = &plugin->controller;
    return plugin;
  } catch (...) { return nullptr; }
}
// Test ownership remains outside the COM objects to inspect retained references
// after quarantine. This explicit fixture cleanup is never a plugin operation.
extern "C" void oc_vst3_fixture_dispose(void *handle) noexcept { delete static_cast<Plugin *>(handle); }
extern "C" void oc_vst3_fixture_fault(void *handle, uint32_t point, uint32_t kind) noexcept {
  auto *plugin = static_cast<Plugin *>(handle); plugin->fault_point = point; plugin->fault_kind = kind;
}
extern "C" void oc_vst3_fixture_refusal(void *handle, int32_t result) noexcept { static_cast<Plugin *>(handle)->refusal = result; }
extern "C" uint32_t oc_vst3_fixture_calls(void *handle, uint32_t point) noexcept {
  return point < 64 ? static_cast<Plugin *>(handle)->calls[point].load(std::memory_order_acquire) : 0;
}
extern "C" uint32_t oc_vst3_fixture_refs(void *handle) noexcept { return static_cast<Plugin *>(handle)->refs; }
extern "C" std::size_t oc_vst3_fixture_layout(uint32_t index) noexcept {
  const std::size_t values[] = {
    sizeof(OcVst3ProcessSetup), alignof(OcVst3ProcessSetup), offsetof(OcVst3ProcessSetup, sampleRate),
    sizeof(OcVst3ProcessData), alignof(OcVst3ProcessData), offsetof(OcVst3ProcessData, inputs), offsetof(OcVst3ProcessData, outputs),
    offsetof(OcVst3ProcessData, inputParameterChanges), offsetof(OcVst3ProcessData, outputParameterChanges), offsetof(OcVst3ProcessData, inputEvents), offsetof(OcVst3ProcessData, outputEvents), offsetof(OcVst3ProcessData, processContext),
    sizeof(OcVst3AudioBus), alignof(OcVst3AudioBus), offsetof(OcVst3AudioBus, silenceFlags), offsetof(OcVst3AudioBus, buffers),
    sizeof(OcVst3BusInfo), alignof(OcVst3BusInfo), offsetof(OcVst3BusInfo, name), offsetof(OcVst3BusInfo, flags),
    sizeof(OcVst3ParameterInfo), alignof(OcVst3ParameterInfo), offsetof(OcVst3ParameterInfo, defaultNormalizedValue), offsetof(OcVst3ParameterInfo, flags),
    sizeof(OcNativeFault), alignof(OcNativeFault), offsetof(OcNativeFault, message)
  };
  return index < sizeof(values) / sizeof(values[0]) ? values[index] : static_cast<std::size_t>(-1);
}

extern "C" void oc_vst3_fixture_channels(void *handle, int32_t channels) noexcept { static_cast<Plugin *>(handle)->channels = channels; }

extern "C" void oc_vst3_fixture_bind_iid(void *handle,uint32_t kind,const unsigned char *iid) noexcept {if(kind<6)std::memcpy(static_cast<Plugin *>(handle)->iids[kind],iid,16);}
extern "C" void oc_vst3_fixture_secondary_fault(void *handle,uint32_t point,uint32_t kind) noexcept {auto *owner=static_cast<Plugin *>(handle);owner->secondary_point=point;owner->secondary_kind=kind;}

extern "C" void oc_vst3_fixture_connections(void *handle,bool enabled) noexcept {static_cast<Plugin *>(handle)->connections=enabled;}
extern "C" uintptr_t oc_vst3_fixture_host(void *handle,uint32_t index) noexcept {return index<2 ? static_cast<Plugin *>(handle)->last_hosts[index] : 0;}

extern "C" OcVst3View *oc_vst3_fixture_view(void *handle) noexcept {auto *owner=static_cast<Plugin *>(handle);++owner->refs;return &owner->view;}

extern "C" void oc_vst3_fixture_parameters(void *handle,uint32_t mode,void *handler) noexcept {auto *owner=static_cast<Plugin *>(handle);owner->parameter_mode=mode;owner->parameter_handler=static_cast<FixtureHandler *>(handler);}
extern "C" void oc_vst3_fixture_parameter_restart(void *handle,uint32_t point,int32_t flags) noexcept {auto *owner=static_cast<Plugin *>(handle);owner->restart_point=point;owner->restart_flags=flags;}
extern "C" double oc_vst3_fixture_parameter_value(void *handle,uint32_t id) noexcept {return id>=100 && id<108 ? static_cast<Plugin *>(handle)->parameter_values[id-100] : 0.0;}

extern "C" uint32_t oc_vst3_fixture_thread_constructions() noexcept { return thread_constructions; }
extern "C" uint32_t oc_vst3_fixture_ui_violations(void *handle) noexcept { return static_cast<Plugin *>(handle)->ui_violations.load(std::memory_order_acquire); }
extern "C" bool oc_vst3_fixture_process_entered(void *handle) noexcept { return static_cast<Plugin *>(handle)->process_entered.load(std::memory_order_acquire); }
extern "C" void oc_vst3_fixture_hold_process(void *handle, bool hold) noexcept { static_cast<Plugin *>(handle)->hold_process.store(hold,std::memory_order_release); }
extern "C" bool oc_vst3_fixture_last_call_on_ui(void *handle, uint32_t point) noexcept {
  auto *owner=static_cast<Plugin *>(handle);
  return point<64 && owner->last_threads[point].load(std::memory_order_acquire)==owner->ui_thread;
}

extern "C" void oc_vst3_fixture_preparation_mutation(void *handle, uint32_t mode) noexcept { static_cast<Plugin *>(handle)->preparation_mutation=mode; }

extern "C" uint64_t oc_vst3_fixture_delivery(void *handle,uint32_t field) noexcept { auto *owner=static_cast<Plugin *>(handle);return field==0 ? owner->delivered_queues : (field==1 ? owner->delivered_points : owner->delivered_digest); }

namespace {
struct FactoryFixture final : OcVst3Factory2 {
  uint32_t refs = 1, calls[16] = {};
  uint32_t point = 0, kind = 0;
  int32_t count = 1, refusal = 1;
  bool malformed = false, null_output = false;
  char expected_iid[16];
  FactoryFixture() { std::memset(expected_iid, 8, sizeof(expected_iid)); }
  int32_t enter(uint32_t operation) {
    ++calls[operation];
    if(operation != point)return 0;
    if(kind == 1)throw std::runtime_error("native factory exception");
    if(kind == 2)throw 31;
    if(kind == 3)throw std::runtime_error(std::string(4096, 'f'));
    return kind == 4 ? refusal : 0;
  }
  int32_t OC_VST3_CALL queryInterface(const char *, void **out) override { *out=this; ++refs; return 0; }
  uint32_t OC_VST3_CALL addRef() override { return ++refs; }
  uint32_t OC_VST3_CALL release() override { ++calls[10]; return --refs; }
  int32_t OC_VST3_CALL getFactoryInfo(OcVst3FactoryInfo *) override { return refusal; }
  int32_t OC_VST3_CALL countClasses() override { enter(1); return count; }
  int32_t OC_VST3_CALL getClassInfo(int32_t index, OcVst3ClassInfo *info) override {
    if(index < 0 || index >= count)throw std::runtime_error("factory class index changed");
    *info = {}; std::memset(info->cid, 7, 16); info->cardinality=1;
    std::memcpy(info->category, "Audio Module Class", 18);
    std::memcpy(info->name, "Native factory", 14);
    if(malformed)std::memset(info->name, 'n', sizeof(info->name));
    return enter(2);
  }
  int32_t OC_VST3_CALL getClassInfo2(int32_t index, OcVst3ClassInfo2 *info) override {
    if(index < 0 || index >= count)throw std::runtime_error("factory2 class index changed");
    *info = {}; std::memset(info->cid, 7, 16); info->cardinality=1;
    std::memcpy(info->category, "Audio Module Class", 18);
    std::memcpy(info->name, "Native factory", 14);
    std::memcpy(info->subCategories, "Instrument", 10);
    if(malformed)std::memset(info->subCategories, 's', sizeof(info->subCategories));
    return enter(3);
  }
  int32_t OC_VST3_CALL createInstance(const char *cid, const char *iid, void **out) override {
    if(cid == nullptr || iid == nullptr || cid[0] != 7 || std::memcmp(iid, expected_iid, 16) != 0)
      throw std::runtime_error("factory creation IDs changed");
    *out = null_output ? nullptr : this;
    if(*out)++refs;
    return enter(4);
  }
};
thread_local FactoryFixture *symbol_factory = nullptr;
OcVst3Factory *fixture_get_factory() { symbol_factory->enter(7); if(symbol_factory->null_output)return nullptr; symbol_factory->addRef(); return symbol_factory; }
bool fixture_module_entry(void *handle) { if(handle != symbol_factory)throw std::runtime_error("module handle changed"); return symbol_factory->enter(5) == 0; }
bool fixture_module_exit() { return symbol_factory->enter(6) == 0; }
bool fixture_module_init() { return symbol_factory->enter(8) == 0; }
uint8_t fixture_bundle_entry(void *bundle) { if(bundle != symbol_factory)throw std::runtime_error("bundle changed"); return symbol_factory->enter(9) == 0 ? 1 : 0; }
}
extern "C" void *oc_vst3_factory_fixture_create() noexcept { return new FactoryFixture; }
extern "C" void oc_vst3_factory_fixture_dispose(void *fixture) noexcept { auto *value=static_cast<FactoryFixture *>(fixture); if(symbol_factory==value)symbol_factory=nullptr; delete value; }
extern "C" void oc_vst3_factory_fixture_configure(void *fixture,uint32_t point,uint32_t kind,int32_t count,bool malformed,bool null_output,int32_t refusal) noexcept { auto *value=static_cast<FactoryFixture *>(fixture);value->point=point;value->kind=kind;value->count=count;value->malformed=malformed;value->null_output=null_output;value->refusal=refusal; }
extern "C" uint32_t oc_vst3_factory_fixture_refs(void *fixture) noexcept { return static_cast<FactoryFixture *>(fixture)->refs; }
extern "C" uint32_t oc_vst3_factory_fixture_calls(void *fixture,uint32_t point) noexcept { return point<16 ? static_cast<FactoryFixture *>(fixture)->calls[point] : 0; }
extern "C" void *oc_vst3_factory_fixture_symbol(void *fixture,uint32_t point) noexcept {
  symbol_factory=static_cast<FactoryFixture *>(fixture);
  switch(point) {
    case 5:return reinterpret_cast<void *>(&fixture_module_entry);
    case 6:return reinterpret_cast<void *>(&fixture_module_exit);
    case 7:return reinterpret_cast<void *>(&fixture_get_factory);
    case 8:return reinterpret_cast<void *>(&fixture_module_init);
    case 9:return reinterpret_cast<void *>(&fixture_bundle_entry);
    default:return nullptr;
  }
}
extern "C" std::size_t oc_vst3_factory_fixture_layout(uint32_t index) noexcept {
  const std::size_t values[]={sizeof(OcVst3ClassInfo),alignof(OcVst3ClassInfo),offsetof(OcVst3ClassInfo,name),sizeof(OcVst3ClassInfo2),alignof(OcVst3ClassInfo2),offsetof(OcVst3ClassInfo2,classFlags),offsetof(OcVst3ClassInfo2,subCategories),offsetof(OcVst3ClassInfo2,sdkVersion)};
  return index<sizeof(values)/sizeof(values[0]) ? values[index] : static_cast<std::size_t>(-1);
}

extern "C" void oc_vst3_factory_fixture_bind_iid(void *fixture,const unsigned char *iid) noexcept { std::memcpy(static_cast<FactoryFixture *>(fixture)->expected_iid,iid,16); }

extern "C" double oc_vst3_fixture_controller_value(void *handle) noexcept { return static_cast<Plugin *>(handle)->controller_gain; }

extern "C" std::size_t oc_vst3_fixture_event_layout(uint32_t index) noexcept {
  const std::size_t values[]={sizeof(FixtureEvent),alignof(FixtureEvent),offsetof(FixtureEvent,payload),sizeof(FixtureNoteExpression),offsetof(FixtureNoteExpression,value),sizeof(FixtureEventListVTable),offsetof(FixtureEventListVTable,count),offsetof(FixtureEventListVTable,event)};
  return index<sizeof(values)/sizeof(values[0]) ? values[index] : static_cast<std::size_t>(-1);
}

extern "C" uint64_t oc_vst3_fixture_midi_word(void *handle,uint32_t kind,uint32_t index,uint32_t field) noexcept {
  auto *owner=static_cast<Plugin *>(handle);
  if(kind==0)return owner->midi_events;
  if(kind==1)return owner->midi_points;
  if(index>=64)return 0;
  if(kind==2 && field<6)return owner->midi_words[index][field];
  if(kind==3 && field<3)return owner->midi_parameters[index][field];
  return 0;
}

extern "C" std::size_t oc_vst3_fixture_midi_layout(uint32_t index) noexcept {
  const std::size_t values[]={sizeof(OcVst3ExpressionInfo),alignof(OcVst3ExpressionInfo),offsetof(OcVst3ExpressionInfo,valueDesc),offsetof(OcVst3ExpressionInfo,flags),sizeof(OcVst3ExpressionValueDescription),offsetof(OcVst3ExpressionValueDescription,stepCount),offsetof(FixtureNoteOn,velocity),offsetof(FixtureNoteOff,note_id),offsetof(FixturePolyPressure,note_id)};
  return index<sizeof(values)/sizeof(values[0]) ? values[index] : static_cast<std::size_t>(-1);
}
