#include "../../../oc-host/tests/fixtures/vst3_guard_fixture.cc"

namespace {
// Non-Windows VST3 interfaces store the four UID words in big-endian order.
constexpr unsigned char factory2_id[] = {
    0x00, 0x07, 0xb6, 0x50, 0xf2, 0x4b, 0x4c, 0x0b, 0xa4, 0x64, 0xed, 0xb9, 0xf0, 0x0b, 0x2a, 0xbb};
constexpr unsigned char processor_ids[][16] = {
    {0xe8, 0x31, 0xff, 0x31, 0xf2, 0xd5, 0x43, 0x01, 0x92, 0x8e, 0xbb, 0xee, 0x25, 0x69, 0x78, 0x02},
    {0x42, 0x04, 0x3f, 0x99, 0xb7, 0xda, 0x45, 0x3c, 0xa5, 0x69, 0xe7, 0x9d, 0x9a, 0xae, 0xc3, 0x3d},
    {0xdc, 0xd7, 0xbb, 0xe3, 0x77, 0x42, 0x44, 0x8d, 0xa8, 0x74, 0xaa, 0xcc, 0x97, 0x9c, 0x75, 0x9e}};
constexpr char primary_id[16] = "OC Primary";
constexpr char alternate_id[16] = "OC Alternate";
constexpr int32_t no_interface = -1;
constexpr int32_t invalid_argument = 2;

template <class Info>
void describe(int32_t index, Info &info) {
  info = {};
  std::memcpy(info.cid, index == 0 ? primary_id : alternate_id, sizeof(info.cid));
  info.cardinality = 1;
  std::strcpy(info.category, "Audio Module Class");
  std::strcpy(info.name, index == 0 ? "Fixture Primary" : "Fixture Alternate");
}

struct Factory final : OcVst3Factory2 {
  uint32_t refs = 0;
  Plugin primary{false, invalid_argument, no_interface};
  Plugin alternate{false, invalid_argument, no_interface};

  Factory() {
    for (Plugin *plugin : {&primary, &alternate}) {
      plugin->refs = 0;
      std::memcpy(plugin->iids, processor_ids, sizeof(processor_ids));
    }
    alternate.parameter_mode = 5;
  }

  int32_t OC_VST3_CALL queryInterface(const char *id, void **object) override {
    *object = nullptr;
    if (std::memcmp(id, factory2_id, sizeof(factory2_id)) != 0) return no_interface;
    *object = this;
    addRef();
    return 0;
  }

  uint32_t OC_VST3_CALL addRef() override { return ++refs; }
  uint32_t OC_VST3_CALL release() override { return --refs; }
  int32_t OC_VST3_CALL getFactoryInfo(OcVst3FactoryInfo *) override { return no_interface; }
  int32_t OC_VST3_CALL countClasses() override { return 2; }

  int32_t OC_VST3_CALL getClassInfo(int32_t index, OcVst3ClassInfo *info) override {
    if (index < 0 || index >= countClasses()) return invalid_argument;
    describe(index, *info);
    return 0;
  }

  int32_t OC_VST3_CALL getClassInfo2(int32_t index, OcVst3ClassInfo2 *info) override {
    if (index < 0 || index >= countClasses()) return invalid_argument;
    describe(index, *info);
    std::strcpy(info->subCategories, "Instrument");
    return 0;
  }

  int32_t OC_VST3_CALL createInstance(const char *cid, const char *iid, void **object) override {
    *object = nullptr;
    Plugin *plugin = nullptr;
    if (std::memcmp(cid, primary_id, sizeof(primary_id)) == 0) plugin = &primary;
    if (std::memcmp(cid, alternate_id, sizeof(alternate_id)) == 0) plugin = &alternate;
    if (!plugin || plugin->refs != 0) return invalid_argument;
    return plugin->component.queryInterface(iid, object);
  }
};
}

extern "C" OcVst3Factory *GetPluginFactory() {
  static Factory factory;
  factory.addRef();
  return &factory;
}
