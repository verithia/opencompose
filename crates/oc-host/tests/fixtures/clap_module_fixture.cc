// Optimized fake DSO for actual CLAP entry lifetime and exception tests.
#include <atomic>
#include <cstdint>
#include <cstring>
#include <stdexcept>

#if defined(_WIN32)
#define FIXTURE_EXPORT __declspec(dllexport)
#else
#define FIXTURE_EXPORT __attribute__((visibility("default")))
#endif

struct clap_version_t { uint32_t major, minor, revision; };
struct clap_plugin;
struct clap_host;
struct clap_plugin_descriptor;
struct clap_plugin_factory {
  uint32_t (*get_plugin_count)(const clap_plugin_factory *);
  const clap_plugin_descriptor *(*get_plugin_descriptor)(const clap_plugin_factory *, uint32_t);
  const clap_plugin *(*create_plugin)(const clap_plugin_factory *, const clap_host *, const char *);
};
struct clap_plugin_entry {
  clap_version_t clap_version;
  bool (*init)(const char *);
  void (*deinit)();
  const void *(*get_factory)(const char *);
};
// CLAP 1.x ABI layouts used by the actual dynamically loaded test plugin.
struct clap_plugin_descriptor {
  clap_version_t clap_version;
  const char *id, *name, *vendor, *url, *manual_url, *support_url, *version, *description;
  const char *const *features;
};
struct clap_process;
struct clap_host {
  clap_version_t clap_version;
  void *host_data;
  const char *name, *vendor, *url, *version;
  const void *(*get_extension)(const clap_host *, const char *);
  void (*request_restart)(const clap_host *);
  void (*request_process)(const clap_host *);
  void (*request_callback)(const clap_host *);
};
struct clap_plugin {
  const clap_plugin_descriptor *desc;
  void *plugin_data;
  bool (*init)(const clap_plugin *);
  void (*destroy)(const clap_plugin *);
  bool (*activate)(const clap_plugin *, double, uint32_t, uint32_t);
  void (*deactivate)(const clap_plugin *);
  bool (*start_processing)(const clap_plugin *);
  void (*stop_processing)(const clap_plugin *);
  void (*reset)(const clap_plugin *);
  int32_t (*process)(const clap_plugin *, const clap_process *);
  const void *(*get_extension)(const clap_plugin *, const char *);
  void (*on_main_thread)(const clap_plugin *);
};
struct clap_host_thread_check {
  bool (*is_main_thread)(const clap_host *);
  bool (*is_audio_thread)(const clap_host *);
};
struct clap_param_info;
struct clap_input_events;
struct clap_output_events;
struct clap_plugin_params {
  uint32_t (*count)(const clap_plugin *);
  bool (*get_info)(const clap_plugin *, uint32_t, clap_param_info *);
  bool (*get_value)(const clap_plugin *, uint32_t, double *);
  bool (*value_to_text)(const clap_plugin *, uint32_t, double, char *, uint32_t);
  bool (*text_to_value)(const clap_plugin *, uint32_t, const char *, double *);
  void (*flush)(const clap_plugin *, const clap_input_events *, const clap_output_events *);
};
struct clap_host_params {
  void (*rescan)(const clap_host *, uint32_t);
  void (*clear)(const clap_host *, uint32_t, uint32_t);
  void (*request_flush)(const clap_host *);
};
namespace {
std::atomic<uint32_t> mode{0}, init_calls{0}, deinit_calls{0}, factory_calls{0};
bool initialize(const char *path) {
  ++init_calls;
  if (path == nullptr || path[0] == 0) throw std::runtime_error("missing fixture init path");
  // A child CLI process cannot call the in-process configure export.
  if (mode == 0 && std::strstr(path, "require-main-callback") != nullptr) mode = 11;
  if (mode == 2) throw std::runtime_error("fake entry init exception");
  return mode != 1;
}
void deinitialize() {
  ++deinit_calls;
  if (mode == 4) throw std::runtime_error("fake entry deinit exception");
}
std::atomic<uint32_t> calls[11]{};
const char *const features[]{"instrument", nullptr};
const clap_plugin_descriptor plugin_descriptor{{1, 0, 0}, "test.fixture.clap.silent", "Silent CLAP Fixture", "OpenCompose", "", "", "", "1", "Lifecycle-only silent fixture", features};
struct Instance { clap_plugin plugin; const clap_host *host; bool pending = false; bool active = false; };
Instance *owner(const clap_plugin *plugin) { return static_cast<Instance *>(plugin->plugin_data); }
void check_role(const clap_plugin *plugin, bool audio) {
  const auto host = owner(plugin)->host;
  const auto check = static_cast<const clap_host_thread_check *>(host->get_extension(host, "clap.thread-check"));
  if (check == nullptr || check->is_audio_thread(host) != audio || (!audio && !check->is_main_thread(host))) ++calls[10];
}
void request(const clap_plugin *plugin) {
  owner(plugin)->pending = true;
  owner(plugin)->host->request_callback(owner(plugin)->host);
}
bool plugin_init(const clap_plugin *plugin) { ++calls[1]; check_role(plugin, false); request(plugin); return mode != 12; }
void destroy(const clap_plugin *plugin) { ++calls[8]; check_role(plugin, false); delete owner(plugin); }
void transition_request(const clap_plugin *plugin) {
  if (mode != 13) return;
  request(plugin);
  const auto host = owner(plugin)->host;
  const auto params = static_cast<const clap_host_params *>(host->get_extension(host, "clap.params"));
  if (params == nullptr) { ++calls[10]; return; }
  params->request_flush(host);
}
bool activate(const clap_plugin *plugin, double, uint32_t, uint32_t) { ++calls[2]; check_role(plugin, false); owner(plugin)->active = true; transition_request(plugin); return true; }
void deactivate(const clap_plugin *plugin) { ++calls[7]; check_role(plugin, false); owner(plugin)->active = false; transition_request(plugin); }
bool start(const clap_plugin *plugin) { ++calls[3]; check_role(plugin, true); transition_request(plugin); return true; }
void stop(const clap_plugin *plugin) { ++calls[6]; check_role(plugin, true); transition_request(plugin); }
void reset(const clap_plugin *plugin) { check_role(plugin, true); }
int32_t process(const clap_plugin *plugin, const clap_process *) {
  ++calls[4]; check_role(plugin, true);
  if (mode == 11 && owner(plugin)->pending) return 0;
  request(plugin); return 1; // Host buffers are already zero: no hardware or sound.
}
uint32_t param_count(const clap_plugin *) { return 0; }
bool param_info(const clap_plugin *, uint32_t, clap_param_info *) { return false; }
void flush(const clap_plugin *plugin, const clap_input_events *, const clap_output_events *) {
  ++calls[9];
  check_role(plugin, owner(plugin)->active);
}
const clap_plugin_params params{param_count, param_info, nullptr, nullptr, nullptr, flush};
const void *extension(const clap_plugin *, const char *id) { return mode == 13 && std::strcmp(id, "clap.params") == 0 ? &params : nullptr; }
void on_main(const clap_plugin *plugin) { ++calls[5]; check_role(plugin, false); owner(plugin)->pending = false; }
uint32_t count(const clap_plugin_factory *) { return 1; }
const clap_plugin_descriptor *descriptor(const clap_plugin_factory *, uint32_t index) { return index == 0 ? &plugin_descriptor : nullptr; }
const clap_plugin *create(const clap_plugin_factory *, const clap_host *host, const char *id) {
  if (std::strcmp(id, plugin_descriptor.id) != 0) return nullptr;
  ++calls[0];
  auto *instance = new Instance{};
  instance->host = host;
  instance->plugin = {&plugin_descriptor, instance, plugin_init, destroy, activate, deactivate, start, stop, reset, process, extension, on_main};
  return &instance->plugin;
}
const clap_plugin_factory factory{count, descriptor, create};
const clap_plugin_factory incomplete_factory{count, descriptor, nullptr};
const void *get_factory(const char *id) {
  ++factory_calls;
  if (mode == 3) throw std::runtime_error("fake entry factory exception");
  if (mode == 5 || std::strcmp(id, "clap.plugin-factory") != 0) return nullptr;
  return mode == 6 ? &incomplete_factory : &factory;
}
}
extern "C" {
FIXTURE_EXPORT clap_plugin_entry clap_entry{{1, 0, 0}, initialize, deinitialize, get_factory};
FIXTURE_EXPORT void oc_clap_module_configure(uint32_t next) noexcept {
  mode = next; init_calls = 0; deinit_calls = 0; factory_calls = 0;
  for (auto &value : calls) value = 0;
  clap_entry.clap_version = {next == 7 ? 0u : 1u, 0, 0};
  clap_entry.init = next == 8 ? nullptr : initialize;
  clap_entry.deinit = next == 9 ? nullptr : deinitialize;
  clap_entry.get_factory = next == 10 ? nullptr : get_factory;
}
FIXTURE_EXPORT uint32_t oc_clap_module_counter(uint32_t kind) noexcept {
  switch (kind) {
  case 0: return init_calls;
  case 1: return deinit_calls;
  case 2: return factory_calls;
  default: return kind >= 3 && kind < 14 ? calls[kind - 3].load() : 0;
  }
}
}
