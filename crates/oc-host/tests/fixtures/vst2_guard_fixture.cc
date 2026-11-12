// Controlled native C++ VST2 endpoint. Throws originate here, never in Rust.
#include "../../src/vst2_guard_abi.h"
#include <atomic>
#include <cstring>
#include <exception>
#include <new>
#include <stdexcept>
#include <thread>
#include <functional>
#if defined(_WIN32)
#define OC_EXPORT __declspec(dllexport)
#else
#define OC_EXPORT __attribute__((visibility("default")))
#endif
struct OcVst2Effect {
    int32_t magic; OcVst2Dispatcher dispatcher; OcVst2Process process;
    OcVst2Set set_parameter; OcVst2Get get_parameter;
    int32_t num_programs, num_params, num_inputs, num_outputs, flags;
    intptr_t reserved1, reserved2;
    int32_t initial_delay, real_qualities, off_qualities; float io_ratio;
    void *object, *user; int32_t unique_id, version;
    OcVst2Process process_replacing; void *process_double;
    unsigned char future[56];
};
static_assert(sizeof(void *) != 8 || sizeof(OcVst2Effect) == 192, "native effect header size");
static_assert(sizeof(void *) != 8 || offsetof(OcVst2Effect, reserved1) == 64, "native effect header offset");
namespace {
constexpr uint32_t Entry=1, Dispatch=2, Set=3, Get=4, Replacing=5, Accumulating=6, Deactivate=7, Close=8;
constexpr uint32_t Partial=1, Foreign=2, DeleteBeforeThrow=4;
std::atomic<uint32_t> configured_point{0}, configured_kind{0}, configured_flags{0};
std::atomic<int32_t> configured_opcode{0};
std::atomic<uint64_t> calls[9]{};
std::atomic<uint64_t> live{0}, destroyed{0}, saved_count{0};
std::atomic<uintptr_t> external_effect{0};
std::atomic<uint64_t> entry_helper_rate{0};
std::atomic<bool> preparation_mode{false};
std::atomic<uint64_t> preparation_status{0};
std::atomic<uint32_t> runtime_mode{0};
std::atomic<uint64_t> runtime_trace{0}, runtime_checks{0}, runtime_expected_position{0}, runtime_setter_thread{0};
std::atomic<uint64_t> runtime_process_thread{0};
std::atomic<uint32_t> claim_mode{0};
uint64_t thread_identity() { return static_cast<uint64_t>(std::hash<std::thread::id>{}(std::this_thread::get_id())); }
void trace(uint64_t code) { runtime_trace.store((runtime_trace.load() << 4) | code); }
struct Saved { OcVst2Host host{}; OcVst2Effect *effect{}; };
Saved saved[16]{};
std::atomic<void *> output_loan{nullptr};
std::atomic<uint32_t> output_kind{0};
std::atomic<const unsigned char *> input_loan{nullptr};
std::atomic<const float * const *> input_arrays{nullptr};
std::atomic<float **> output_arrays{nullptr};
std::atomic<const unsigned char *> event_packet_loan{nullptr}, midi_event_loan{nullptr};
std::atomic<const unsigned char *> midi_last_event_loan{nullptr};
std::atomic<int32_t> event_packet_count{0};
struct Instance { OcVst2Host host; OcVst2Effect *effect; float parameters[2]{0.5f, 0.0f}; bool external{}, state_loaded{}, prepared_write{}, active{}; };
void update_latency(Instance *instance) {
    if (preparation_mode.load()) instance->effect->initial_delay =
        (instance->state_loaded ? 64 : 0) + static_cast<int32_t>(instance->parameters[0] * 100) + (instance->active ? 8 : 0);
}
struct LongError final : std::exception {
    const char *what() const noexcept override {
        return "native bounded exception: 012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789";
    }
};
bool configured(uint32_t point, int32_t opcode=0) {
    return configured_point.load() == point && (point != Dispatch || configured_opcode.load() == opcode);
}
void callback_panic(Instance *instance) {
    auto call = [instance] { instance->host(instance->effect, 0x4f430010, 0, 0, nullptr, 0); };
    if (configured_flags.load() & Foreign) { std::thread worker(call); worker.join(); }
    else call();
}
bool fail(Instance *instance) {
    switch (configured_kind.load()) {
        case 1: throw std::runtime_error("controlled native VST2 exception");
        case 2: throw uint64_t{0x1234};
        case 3: return true;
        case 4: callback_panic(instance); return false;
        case 5: callback_panic(instance); throw std::runtime_error("after caught host panic");
        case 6: throw LongError{};
        default: return false;
    }
}
void destroy(Instance *instance) {
    if (!instance->external) delete instance->effect;
    else instance->effect->~OcVst2Effect();
    delete instance;
    live.fetch_sub(1); destroyed.fetch_add(1);
}
intptr_t OC_VST2_CALL dispatcher(OcVst2Effect *effect, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt) {
    auto *instance = static_cast<Instance *>(effect->object);
    calls[Dispatch].fetch_add(1);
    if (opcode == 25 && runtime_mode.load()) {
        trace(8);
        const auto *packet=static_cast<const unsigned char *>(ptr);
        int32_t count=0; std::memcpy(&count,packet,sizeof(count));
        if (count == 2) {
            const unsigned char *first=nullptr,*second=nullptr;
            std::memcpy(&first,packet+16,sizeof(first)); std::memcpy(&second,packet+16+sizeof(first),sizeof(second));
            int32_t offset=0;std::memcpy(&offset,second+8,sizeof(offset));
            if (first[24]==0xc2&&first[25]==65&&first[26]==0&&second[24]==0x92&&second[25]==60&&second[26]==100&&offset==63) runtime_checks.fetch_or(4);
        }
        if (runtime_mode.load()==1) instance->host(effect,42,0,0,nullptr,0);
    }
    if (opcode == 12 && value == 0) {
        calls[Deactivate].fetch_add(1);
        if (configured(Deactivate)) fail(instance);
    }
    if (opcode == 1) {
        calls[Close].fetch_add(1);
        instance->host(effect, 0x4f430002, 0, 0, nullptr, 0);
        if (configured(Close)) {
            if (configured_flags.load() & DeleteBeforeThrow) { destroy(instance); throw std::runtime_error("native close threw after deletion"); }
            fail(instance);
        }
        destroy(instance);
        return 0;
    }
    if (configured(Dispatch, opcode)) {
        if (configured_flags.load() & Partial) {
            if (opcode == 23 || opcode == 13) {
                *static_cast<void **>(ptr) = reinterpret_cast<void *>(uintptr_t{1});
                output_loan.store(ptr); output_kind.store(1);
            } else if (opcode == 7 || opcode == 8 || opcode == 45 || opcode == 47 || opcode == 48) {
                std::memcpy(ptr, "bad\0", 4); output_loan.store(ptr); output_kind.store(2);
            } else if (opcode == 56) {
                std::memset(ptr, 0, 16); output_loan.store(ptr); output_kind.store(2);
            }
        }
        if (opcode == 24 && value > 0) input_loan.store(static_cast<const unsigned char *>(ptr));
        if (opcode == 51) input_loan.store(static_cast<const unsigned char *>(ptr));
        if (opcode == 25) {
            auto *packet = static_cast<const unsigned char *>(ptr);
            int32_t count=0;std::memcpy(&count,packet,sizeof(count));
            if (count>0&&count<=1024) {
                const unsigned char *first=nullptr,*last=nullptr;
                std::memcpy(&first,packet+16,sizeof(first));
                std::memcpy(&last,packet+16+(count-1)*sizeof(last),sizeof(last));
                event_packet_loan.store(packet); event_packet_count.store(count);
                midi_event_loan.store(first); midi_last_event_loan.store(last);
            }
        }
        if (fail(instance)) return opcode == 24 ? 1 : 0;
    }
    switch (opcode) {
        case 0: case 10: case 11: return 0;
        case 12:
            if (value != 0 && preparation_mode.load()) {
                preparation_status.fetch_or(4);
                if (instance->state_loaded && instance->prepared_write) preparation_status.fetch_or(8);
            }
            instance->active = value != 0; update_latency(instance); return 0;
        case 6: if (ptr) std::strcpy(static_cast<char *>(ptr), ""); return 1;
        case 7: if (ptr) std::strcpy(static_cast<char *>(ptr), "0.5"); return 1;
        case 8: if (ptr) std::strcpy(static_cast<char *>(ptr), claim_mode.load()==2 || (claim_mode.load()==1&&index==0) ? "output_pair" : (index==0 ? "Gain" : "Offset")); return 1;
        case 13: return 0;
        case 22: return 0x4e763466;
        case 23: *static_cast<void **>(ptr) = instance->parameters; return sizeof(instance->parameters);
        case 24:
            if (value == sizeof(instance->parameters)) {
                std::memcpy(instance->parameters, ptr, sizeof(instance->parameters));
                instance->state_loaded=true; update_latency(instance);
                if (preparation_mode.load() && !instance->active) preparation_status.fetch_or(1);
            }
            return 0;
        case 25: instance->host(effect, 0x4f430004, 0, 0, nullptr, 0); return 1;
        case 26: return 1;
        case 35: return 1;
        case 45: case 48: if (ptr) std::strcpy(static_cast<char *>(ptr), preparation_mode.load() && instance->state_loaded ? "Prepared Guard Fixture" : "Native Guard Fixture"); return 1;
        case 47: if (ptr) std::strcpy(static_cast<char *>(ptr), "OpenCompose"); return 1;
        case 49: return 1;
        case 51: return std::strcmp(static_cast<const char *>(ptr), "receiveVstEvents") == 0 ? 1 : 0;
        case 56: return 0;
        case 52: return preparation_mode.load() ? 1000 + static_cast<intptr_t>(instance->parameters[0] * 1000) + (instance->state_loaded ? 100 : 0) + (instance->active ? 10 : 0) : 2400;
        case 58: return 2400;
        default: (void)opt; return 0;
    }
}
void OC_VST2_CALL setter(OcVst2Effect *effect, int32_t index, float value) {
    auto *instance = static_cast<Instance *>(effect->object); calls[Set].fetch_add(1);
    if (runtime_mode.load()) {
        trace(1); runtime_setter_thread.store(thread_identity());
        auto reply=instance->host(effect,7,0,0,nullptr,0);
        if (reply) {
            double position=0,rate=0;std::memcpy(&position,reinterpret_cast<const void *>(reply),sizeof(position));
            std::memcpy(&rate,reinterpret_cast<const unsigned char *>(reply)+8,sizeof(rate));
            if (position==static_cast<double>(runtime_expected_position.load())&&rate==48000) runtime_checks.fetch_or(1);
        }
        if (instance->host(effect,23,0,0,nullptr,0)==2) runtime_checks.fetch_or(2);
        if (runtime_mode.load()==1) instance->host(effect,42,0,0,nullptr,0);
    }
    if (configured(Set) && fail(instance)) return;
    if (preparation_mode.load() && index == 0 && instance->state_loaded && !instance->active && instance->parameters[0] == 0.25f) {
        preparation_status.fetch_or(2); instance->prepared_write=true;
    }
    if (index >= 0 && index < 2) instance->parameters[index] = value;
    if (runtime_mode.load()==2) effect->num_params=3;
    if (runtime_mode.load()==3) effect->set_parameter=nullptr;
    update_latency(instance);
}
float OC_VST2_CALL getter(OcVst2Effect *effect, int32_t index) {
    auto *instance = static_cast<Instance *>(effect->object); calls[Get].fetch_add(1);
    if (configured(Get) && fail(instance)) return 0;
    return index >= 0 && index < 2 ? instance->parameters[index] : 0;
}
void process(OcVst2Effect *effect, const float * const *inputs, float **outputs, int32_t frames, uint32_t point) {
    auto *instance = static_cast<Instance *>(effect->object); calls[point].fetch_add(1);
    if (runtime_mode.load()) {trace(9);runtime_process_thread.store(thread_identity());}
    instance->host(effect, 0x4f430003, 0, 0, nullptr, 0);
    if (configured(point)) {
        input_arrays.store(inputs); output_arrays.store(outputs);
        if (frames > 0 && outputs && outputs[0]) outputs[0][0] = 0.875f;
        if (fail(instance)) return;
    }
    for (int channel=0; channel<2; ++channel) for (int32_t frame=0; frame<frames; ++frame) {
        const float input = inputs && inputs[channel] ? inputs[channel][frame] : 0.25f;
        if (point == Accumulating) outputs[channel][frame] += input * instance->parameters[0];
        else outputs[channel][frame] = input * instance->parameters[0];
    }
}
void OC_VST2_CALL replacing(OcVst2Effect *effect, const float * const *inputs, float **outputs, int32_t frames) { process(effect, inputs, outputs, frames, Replacing); }
void OC_VST2_CALL accumulating(OcVst2Effect *effect, const float * const *inputs, float **outputs, int32_t frames) { process(effect, inputs, outputs, frames, Accumulating); }
}
extern "C" OC_EXPORT OcVst2Effect * OC_VST2_CALL VSTPluginMain(OcVst2Host host) {
    calls[Entry].fetch_add(1);
    const auto address = external_effect.load();
    auto *effect = address ? new(reinterpret_cast<void *>(address)) OcVst2Effect{} : new OcVst2Effect{};
    auto *instance = new Instance{host, effect}; instance->external = address != 0;
    effect->magic = 0x56737450; effect->dispatcher = dispatcher; effect->process = accumulating;
    effect->set_parameter = setter; effect->get_parameter = getter;
    effect->num_programs=1; effect->num_params=2; effect->num_inputs=2; effect->num_outputs=2;
    if (runtime_mode.load()) effect->num_inputs=0;
    if (runtime_mode.load()==4) {effect->num_params=0;effect->set_parameter=nullptr;}
    if (claim_mode.load()==3) {effect->num_params=0;effect->set_parameter=nullptr;}
    if (claim_mode.load()==4) effect->set_parameter=nullptr;
    effect->flags=16|32;
    if (configured(Dispatch, 13)) effect->flags |= 1; effect->object=instance; effect->unique_id=0x4f434e47; effect->version=1; effect->process_replacing=replacing;
    if (configured(Accumulating)) { effect->flags &= ~16; effect->num_inputs=0; }
    live.fetch_add(1);
    const auto slot = saved_count.fetch_add(1);
    if (slot < 16) saved[slot] = Saved{host, effect};
    host(effect, 0x4f430001, 0, 0, nullptr, 0);
    if (configured(Entry) && (configured_flags.load() & Foreign) && configured_kind.load() != 4 && configured_kind.load() != 5) {
        std::thread worker([=] {
            const auto reply = host(effect, 7, 0, 0, nullptr, 0);
            if (reply) { uint64_t bits=0; std::memcpy(&bits, reinterpret_cast<const unsigned char *>(reply) + sizeof(double), sizeof(bits)); entry_helper_rate.store(bits); }
        });
        worker.join();
    }
    if (configured(Entry) && fail(instance)) { destroy(instance); return nullptr; }
    return effect;
}
extern "C" OC_EXPORT void oc_vst2_fixture_configure(uint32_t point, int32_t opcode, uint32_t kind, uint32_t flags, uintptr_t address) noexcept {
    configured_point.store(point); configured_opcode.store(opcode); configured_kind.store(kind); configured_flags.store(flags); external_effect.store(address);
}
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_count(uint32_t point) noexcept { return point < 9 ? calls[point].load() : 0; }
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_entry_helper_rate() noexcept { return entry_helper_rate.load(); }
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_live() noexcept { return live.load(); }
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_destroyed() noexcept { return destroyed.load(); }
extern "C" OC_EXPORT void oc_vst2_fixture_preparation_mode(bool enabled) noexcept { preparation_mode.store(enabled); }
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_preparation_status() noexcept { return preparation_status.load(); }
extern "C" OC_EXPORT void oc_vst2_fixture_claim_mode(uint32_t mode) noexcept {claim_mode.store(mode);}
extern "C" OC_EXPORT void oc_vst2_fixture_runtime_mode(uint32_t mode,uint64_t position) noexcept { runtime_mode.store(mode); runtime_expected_position.store(position); runtime_trace.store(0);runtime_checks.store(0); }
extern "C" OC_EXPORT uint64_t oc_vst2_fixture_runtime_detail(uint32_t field) noexcept {
    switch(field) {case 0:return thread_identity();case 1:return runtime_setter_thread.load();case 2:return runtime_trace.load();case 3:return runtime_checks.load();case 4:return runtime_process_thread.load();default:return 0;}
}
// Query storage belongs to the native image. The longest valid input is NUL
// terminated after 1 MiB, but the host must reject it within its fixed budget.
extern "C" OC_EXPORT intptr_t oc_vst2_fixture_query_host(uint32_t index, uint32_t query) noexcept {
    if (index >= saved_count.load() || index >= 16) return -1;
    static char long_query[1024 * 1024 + 1]{};
    static const bool initialized = [] {
        std::memset(long_query, 'x', sizeof(long_query));
        std::memcpy(long_query, "sendVstEvents", sizeof("sendVstEvents") - 1);
        long_query[sizeof(long_query) - 1] = 0;
        return true;
    }();
    (void)initialized;
    const char *text = nullptr;
    switch (query) {
        case 0: text = "sendVstEvents"; break;
        case 1: text = "reportConnectionChanges"; break;
        case 2: text = ""; break;
        case 3: break;
        case 4: text = "\xff\0"; break;
        case 5: text = long_query; break;
        case 6: text = "sendVst"; break;
        default: return -1;
    }
    return saved[index].host(saved[index].effect, 37, 0, 0, const_cast<char *>(text), 0);
}
extern "C" OC_EXPORT uintptr_t oc_vst2_fixture_saved_effect(uint32_t index) noexcept { return index < 16 ? reinterpret_cast<uintptr_t>(saved[index].effect) : 0; }
extern "C" OC_EXPORT intptr_t oc_vst2_fixture_late_callback(uint32_t index, uintptr_t address, int32_t opcode, float value, bool foreign) noexcept {
    try {
    if (index >= saved_count.load() || index >= 16) return -1;
    const auto call = [=] { return saved[index].host(reinterpret_cast<OcVst2Effect *>(address), opcode, 0, 0, nullptr, value); };
    intptr_t result=0;
    if (foreign) { std::thread worker([&] { result=call(); }); worker.join(); } else result=call();
    return result;
    } catch (...) { return -2; }
}
extern "C" OC_EXPORT uintptr_t oc_vst2_fixture_arena() noexcept {
    alignas(OcVst2Effect) static unsigned char storage[sizeof(OcVst2Effect)];
    return reinterpret_cast<uintptr_t>(storage);
}
extern "C" OC_EXPORT uint32_t oc_vst2_fixture_touch_loans() noexcept {
    uint32_t touched=0;
    if (auto *ptr=output_loan.load()) {
        if (output_kind.load() == 1) *static_cast<void **>(ptr) = nullptr;
        else static_cast<unsigned char *>(ptr)[0] = 42;
        ++touched;
    }
    if (auto *ptr=input_loan.load()) { volatile auto byte=ptr[0]; (void)byte; ++touched; }
    if (auto ptr=input_arrays.load()) { if (ptr[0]) { volatile auto sample=ptr[0][0]; (void)sample; } ++touched; }
    if (auto ptr=output_arrays.load()) { if (ptr[0]) ptr[0][0]=0.5f; ++touched; }
    if (auto *packet=event_packet_loan.load()) {
        int32_t count=0; std::memcpy(&count, packet, sizeof(count));
        if (count==event_packet_count.load()&&count>0&&count<=1024) {
            const unsigned char *first=nullptr,*last=nullptr;
            std::memcpy(&first,packet+16,sizeof(first));
            std::memcpy(&last,packet+16+(count-1)*sizeof(last),sizeof(last));
            if (first==midi_event_loan.load()&&last==midi_last_event_loan.load()) ++touched;
        }
    }
    if (auto *event=midi_event_loan.load()) {
        int32_t type=0; std::memcpy(&type, event, sizeof(type));
        if (type == 1 && ((event[24] == 0x90 && event[25] == 60 && event[26] == 100) || (event[24] == 0xc2 && event[25] == 65 && event[26] == 0))) ++touched;
    }
    if (auto *event=midi_last_event_loan.load();event&&event!=midi_event_loan.load()) {
        int32_t type=0;std::memcpy(&type,event,sizeof(type));
        if (type==1&&event[24]==0x92&&event[25]==60&&event[26]==100) ++touched;
    }
    return touched;
}
