// Optimized foreign callbacks used only by the Rust unit-test link.
#include <cstdint>
#include <stdexcept>
#include <string>
struct clap_plugin;
struct clap_host;
struct clap_process;
struct clap_istream;
struct clap_ostream;
namespace {
thread_local uint32_t fault_at = 0;
thread_local uint32_t events[64] = {};
thread_local uint32_t used = 0;
thread_local void (*host_callback)(const clap_host *) = nullptr;
thread_local const clap_host *host_pointer = nullptr;
thread_local bool (*audio_check)(const clap_host *) = nullptr;
thread_local const clap_host *checked_host = nullptr;
thread_local uint32_t audio_mask = 0;
void enter(uint32_t point) {
  if (audio_check != nullptr && audio_check(checked_host) && point < 32) audio_mask |= 1u << point;
  if (used < 64) events[used++] = point;
  if (fault_at == point) throw std::runtime_error("optimized CLAP fixture exception");
  if (fault_at == 100 + point) throw 17;
  if (fault_at == 200 + point) throw std::runtime_error(std::string(4096, 'x'));
}
}
extern "C" void oc_clap_fixture_reset(uint32_t point) noexcept {
  fault_at = point; used = 0; host_callback = nullptr; host_pointer = nullptr;
  audio_check = nullptr; checked_host = nullptr; audio_mask = 0;
}
extern "C" uint32_t oc_clap_fixture_count() noexcept { return used; }
extern "C" uint32_t oc_clap_fixture_event(uint32_t index) noexcept { return index < used ? events[index] : 0; }
extern "C" void oc_clap_fixture_callback(void (*callback)(const clap_host *), const clap_host *host) noexcept {
  host_callback = callback; host_pointer = host;
}
extern "C" bool oc_clap_fixture_init(const clap_plugin *) { enter(1); return true; }
extern "C" bool oc_clap_fixture_activate(const clap_plugin *, double rate, uint32_t minimum, uint32_t maximum) {
  if (rate != 48000.0 || minimum != 1 || maximum != 512) throw std::runtime_error("activate ABI arguments changed");
  enter(2); return true;
}
extern "C" bool oc_clap_fixture_start(const clap_plugin *) { enter(3); return true; }
extern "C" int32_t oc_clap_fixture_process(const clap_plugin *, const clap_process *process) {
  if (process == nullptr) throw std::runtime_error("process pointer is null");
  enter(4); if (host_callback != nullptr) host_callback(host_pointer); return fault_at == 304 ? 0 : 1;
}
extern "C" void oc_clap_fixture_stop(const clap_plugin *) { enter(5); }
extern "C" void oc_clap_fixture_deactivate(const clap_plugin *) { enter(6); }
extern "C" void oc_clap_fixture_destroy(const clap_plugin *) { enter(7); }
extern "C" const void *oc_clap_fixture_extension(const clap_plugin *, const char *) { return nullptr; }
extern "C" bool oc_clap_fixture_load(const clap_plugin *, const clap_istream *) { enter(8); return true; }
extern "C" bool oc_clap_fixture_save(const clap_plugin *, const clap_ostream *) { enter(9); return true; }
extern "C" bool oc_clap_fixture_gui_api(const clap_plugin *, const char *api, bool floating) {
  enter(10); return api != nullptr && api[0] == 't' && floating;
}
extern "C" bool oc_clap_fixture_gui_size(const clap_plugin *, uint32_t *width, uint32_t *height) {
  enter(11); *width = 123; *height = 45; return true;
}
extern "C" bool oc_clap_fixture_param_value(const clap_plugin *, uint32_t id, double *value) {
  enter(12); *value = 0.25 + static_cast<double>(id); return true;
}
extern "C" uint32_t oc_clap_fixture_ports_count(const clap_plugin *, bool input) {
  enter(13); return input ? 1 : 2;
}

extern "C" void oc_clap_fixture_check_audio(bool (*callback)(const clap_host *), const clap_host *host) noexcept {
  audio_check = callback; checked_host = host; audio_mask = 0;
}
extern "C" uint32_t oc_clap_fixture_audio_mask() noexcept { return audio_mask; }
extern "C" void oc_clap_fixture_on_main(const clap_plugin *) { enter(15); }
extern "C" void oc_clap_fixture_reset_plugin(const clap_plugin *) { enter(14); }

struct clap_input_events;
struct clap_output_events;
extern "C" void oc_clap_fixture_authored_flush(const clap_plugin *, const clap_input_events *, const clap_output_events *) {
  enter(16);
}
