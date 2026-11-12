#include "native_fault.h"
// Typed CLAP calls catch C++ exceptions before they can cross a Rust frame.
#include <cstdint>
#include <cstring>
#include <exception>

struct clap_plugin;
struct clap_plugin_factory;
struct clap_plugin_descriptor;
struct clap_host;
struct clap_process;
struct clap_audio_port_info;
struct clap_note_port_info;
struct clap_param_info;
struct clap_input_events;
struct clap_output_events;
struct clap_istream;
struct clap_ostream;
struct clap_window;

using OcClapFault = OcNativeFault;
static void fault_message(OcClapFault *fault, int32_t kind, const char *text) noexcept {
  oc_native_fault_message(fault, kind, text);
}

using entry_init_fn = bool (*)(const char *);
extern "C" bool oc_clap_entry_init(entry_init_fn call, const char * path, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(path); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using entry_void_fn = void (*)();
extern "C" bool oc_clap_entry_void(entry_void_fn call, OcClapFault *fault) noexcept {
  try { call(); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using factory_fn = const void * (*)(const char *);
extern "C" bool oc_clap_factory(factory_fn call, const char * id, const void * *result, OcClapFault *fault) noexcept {
  try { *result = call(id); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using factory_count_fn = uint32_t (*)(const clap_plugin_factory *);
extern "C" bool oc_clap_factory_count(factory_count_fn call, const clap_plugin_factory * factory, uint32_t *result, OcClapFault *fault) noexcept {
  try { *result = call(factory); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using factory_descriptor_fn = const clap_plugin_descriptor * (*)(const clap_plugin_factory *, uint32_t);
extern "C" bool oc_clap_factory_descriptor(factory_descriptor_fn call, const clap_plugin_factory * factory, uint32_t index, const clap_plugin_descriptor * *result, OcClapFault *fault) noexcept {
  try { *result = call(factory, index); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using create_fn = const clap_plugin * (*)(const clap_plugin_factory *, const clap_host *, const char *);
extern "C" bool oc_clap_create(create_fn call, const clap_plugin_factory * factory, const clap_host * host, const char * id, const clap_plugin * *result, OcClapFault *fault) noexcept {
  try { *result = call(factory, host, id); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using plugin_bool_fn = bool (*)(const clap_plugin *);
extern "C" bool oc_clap_plugin_bool(plugin_bool_fn call, const clap_plugin * plugin, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using plugin_void_fn = void (*)(const clap_plugin *);
extern "C" bool oc_clap_plugin_void(plugin_void_fn call, const clap_plugin * plugin, OcClapFault *fault) noexcept {
  try { call(plugin); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using plugin_u32_fn = uint32_t (*)(const clap_plugin *);
extern "C" bool oc_clap_plugin_u32(plugin_u32_fn call, const clap_plugin * plugin, uint32_t *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using activate_fn = bool (*)(const clap_plugin *, double, uint32_t, uint32_t);
extern "C" bool oc_clap_activate(activate_fn call, const clap_plugin * plugin, double rate, uint32_t minimum, uint32_t maximum, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, rate, minimum, maximum); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using extension_fn = const void * (*)(const clap_plugin *, const char *);
extern "C" bool oc_clap_extension(extension_fn call, const clap_plugin * plugin, const char * id, const void * *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, id); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using process_fn = int32_t (*)(const clap_plugin *, const clap_process *);
extern "C" bool oc_clap_process(process_fn call, const clap_plugin * plugin, const clap_process * process, int32_t *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, process); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using ports_count_fn = uint32_t (*)(const clap_plugin *, bool);
extern "C" bool oc_clap_ports_count(ports_count_fn call, const clap_plugin * plugin, bool input, uint32_t *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, input); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using audio_port_fn = bool (*)(const clap_plugin *, uint32_t, bool, clap_audio_port_info *);
extern "C" bool oc_clap_audio_port(audio_port_fn call, const clap_plugin * plugin, uint32_t index, bool input, clap_audio_port_info * info, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, index, input, info); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using note_port_fn = bool (*)(const clap_plugin *, uint32_t, bool, clap_note_port_info *);
extern "C" bool oc_clap_note_port(note_port_fn call, const clap_plugin * plugin, uint32_t index, bool input, clap_note_port_info * info, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, index, input, info); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using param_info_fn = bool (*)(const clap_plugin *, uint32_t, clap_param_info *);
extern "C" bool oc_clap_param_info(param_info_fn call, const clap_plugin * plugin, uint32_t index, clap_param_info * info, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, index, info); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using param_value_fn = bool (*)(const clap_plugin *, uint32_t, double *);
extern "C" bool oc_clap_param_value(param_value_fn call, const clap_plugin * plugin, uint32_t id, double * value, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, id, value); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using param_text_fn = bool (*)(const clap_plugin *, uint32_t, double, char *, uint32_t);
extern "C" bool oc_clap_param_text(param_text_fn call, const clap_plugin * plugin, uint32_t id, double value, char * text, uint32_t capacity, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, id, value, text, capacity); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using flush_fn = void (*)(const clap_plugin *, const clap_input_events *, const clap_output_events *);
extern "C" bool oc_clap_flush(flush_fn call, const clap_plugin * plugin, const clap_input_events * input, const clap_output_events * output, OcClapFault *fault) noexcept {
  try { call(plugin, input, output); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using load_fn = bool (*)(const clap_plugin *, const clap_istream *);
extern "C" bool oc_clap_load(load_fn call, const clap_plugin * plugin, const clap_istream * stream, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, stream); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using save_fn = bool (*)(const clap_plugin *, const clap_ostream *);
extern "C" bool oc_clap_save(save_fn call, const clap_plugin * plugin, const clap_ostream * stream, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, stream); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using gui_api_fn = bool (*)(const clap_plugin *, const char *, bool);
extern "C" bool oc_clap_gui_api(gui_api_fn call, const clap_plugin * plugin, const char * api, bool floating, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, api, floating); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using gui_size_fn = bool (*)(const clap_plugin *, uint32_t *, uint32_t *);
extern "C" bool oc_clap_gui_size(gui_size_fn call, const clap_plugin * plugin, uint32_t * width, uint32_t * height, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, width, height); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}

using gui_parent_fn = bool (*)(const clap_plugin *, const clap_window *);
extern "C" bool oc_clap_gui_parent(gui_parent_fn call, const clap_plugin * plugin, const clap_window * window, bool *result, OcClapFault *fault) noexcept {
  try { *result = call(plugin, window); return true; }
  catch (const std::exception &error) { fault_message(fault, 1, error.what()); return false; }
  catch (...) { fault_message(fault, 2, "unknown C++ exception"); return false; }
}
