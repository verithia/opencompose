// Run a host callback behind a C++ exception barrier.
//
// A plugin may throw across its C++ ABI while the host calls into it. Rust's
// `catch_unwind` only handles Rust panics: a foreign exception that reaches a
// Rust frame aborts the process at `__rust_foreign_exception`, which turns a
// recoverable plugin failure into SIGABRT. This translation unit catches the
// exception where C++ can still handle it and reports the message, so the host
// can return an error instead.
#include <cstddef>
#include <cstring>
#include <exception>

namespace {

void write_message(char *message, std::size_t capacity, const char *text) {
  if (message == nullptr || capacity == 0) {
    return;
  }
  std::size_t length = text == nullptr ? 0 : std::strlen(text);
  if (length >= capacity) {
    length = capacity - 1;
  }
  if (length > 0) {
    std::memcpy(message, text, length);
  }
  message[length] = '\0';
}

} // namespace

extern "C" int oc_host_guard_call(void (*work)(void *), void *context,
                                  char *message, std::size_t capacity) {
  try {
    work(context);
    return 0;
  } catch (const std::exception &error) {
    write_message(message, capacity, error.what());
    return 1;
  } catch (...) {
    write_message(message, capacity, "unknown C++ exception");
    return 2;
  }
}
