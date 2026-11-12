#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>

struct OcNativeFault {
  int32_t kind;
  unsigned char message[256];
};
inline void oc_native_fault_message(OcNativeFault *fault, int32_t kind, const char *text) noexcept {
  if (fault == nullptr) return;
  fault->kind = kind;
  std::size_t length = 0;
  if (text != nullptr) {
    while (length < sizeof(fault->message) - 1 && text[length] != '\0') ++length;
    std::memcpy(fault->message, text, length);
  }
  fault->message[length] = '\0';
}
