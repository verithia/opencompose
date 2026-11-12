import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { EXTRA_PRESETS } from "../data/factoryPresets";
import {
  SYNTH_CATALOG,
  enumOptions,
  isKnownParam,
  paramSpec,
  specForConcreteKey,
} from "../data/synthCatalog";
import { i18n, type I18nKey } from "../stores/i18n";

/** Expanded concrete keys; mirrors `parameter_keys()` in the Rust catalog. */
function expandedKeys(): string[] {
  const keys: string[] = [];
  for (const param of SYNTH_CATALOG.params) keys.push(...param.ids, ...param.aliases);
  for (const family of SYNTH_CATALOG.families) {
    for (let index = family.first; index <= family.last; index += 1) {
      if (family.fields.length === 1 && family.fields[0].suffix === "") {
        keys.push(`${family.prefix}${index}`);
      } else {
        for (const field of family.fields) keys.push(`${family.prefix}${index}.${field.suffix}`);
      }
    }
  }
  return keys;
}

function editorFactoryKeys(): string[] {
  const source = readFileSync(`${process.cwd()}/src/components/OpenSynthEditor.tsx`, "utf8");
  const start = source.indexOf("const FACTORY_PATCHES");
  expect(start, "factory patch table").toBeGreaterThanOrEqual(0);
  const end = source.indexOf("\n};", start);
  expect(end, "factory patch table end").toBeGreaterThan(start);
  const keys: string[] = [];
  for (const match of source.slice(start, end).matchAll(/\["([^"]+)"/g)) keys.push(match[1]);
  // factoryParams generates these families; the editor also writes the rack count.
  for (let macro = 1; macro <= 8; macro += 1) keys.push(`macro.${macro}`);
  for (let step = 1; step <= 16; step += 1) keys.push(`step.${step}`);
  for (let route = 1; route <= 64; route += 1) {
    for (const field of ["source", "destination", "amount"]) keys.push(`mod.${route}.${field}`);
  }
  keys.push("fx.count");
  return keys;
}

describe("OpenSynth parameter catalog", () => {
  it("pins the catalog shape so deleting entries fails", () => {
    expect(SYNTH_CATALOG.version).toBe(4);
    expect(SYNTH_CATALOG.pluginId).toBe("builtin.open_synth");
    expect(SYNTH_CATALOG.params).toHaveLength(123);
    for (const [key, value] of [["osc.a.waveform", 1], ["osc.b.waveform", 2], ["osc.c.waveform", 0],
      ["osc.a.level", 0.78], ["osc.b.level", 0], ["osc.c.level", 0]] as const) {
      expect(specForConcreteKey(key)?.ids).toEqual([key]);
      expect(specForConcreteKey(key)?.default, key).toBe(value);
    }
    expect(SYNTH_CATALOG.families).toHaveLength(7);
    expect(SYNTH_CATALOG.groups).toHaveLength(12);
    expect(expandedKeys()).toHaveLength(1517);
    expect(new Set(expandedKeys()).size).toBe(1517);
    expect(SYNTH_CATALOG.modSources.options).toHaveLength(29);
    expect(SYNTH_CATALOG.modDestinations.options).toHaveLength(35);
    expect(SYNTH_CATALOG.modAux.options).toHaveLength(30);
    expect(SYNTH_CATALOG.modLearnTargets).toHaveLength(20);
    expect(SYNTH_CATALOG.unmappedModDestinations).toHaveLength(4);
    expect(SYNTH_CATALOG.filterModels.options).toHaveLength(12);
    expect(SYNTH_CATALOG.warpModes.options).toHaveLength(70);
    expect(SYNTH_CATALOG.waveforms.options).toHaveLength(7);
    expect(SYNTH_CATALOG.lfoShapes.options).toHaveLength(5);
    expect(SYNTH_CATALOG.fxTypes).toHaveLength(19);
  });

  it("keeps learn targets inside the catalog and its destination ids", () => {
    const destinations = new Set(SYNTH_CATALOG.modDestinations.options.map((option) => option.id));
    const controls = new Set<string>();
    for (const target of SYNTH_CATALOG.modLearnTargets) {
      expect(isKnownParam(target.control), target.control).toBe(true);
      expect(controls.has(target.control), target.control).toBe(false);
      controls.add(target.control);
      expect(destinations.has(target.destination), target.control).toBe(true);
    }
    // The design's own counter-example: no destination, so no learn entry.
    expect(controls.has("unison.width")).toBe(false);
  });

  it("keeps no-op warp modes visible and marked", () => {    const plain = enumOptions("warpMode");
    const marked = enumOptions("warpMode", true);
    expect(marked).toHaveLength(70);
    expect(plain.filter((option) => option.label.includes("(no effect)"))).toHaveLength(0);
    expect(marked.filter((option) => option.label.includes("(no effect)"))).toHaveLength(35);
    expect(SYNTH_CATALOG.warpModes.options.filter((option) => option.status === "noOp")).toHaveLength(35);
    expect(enumOptions("filterModel")).toHaveLength(12);
    expect(enumOptions("modDestination")).toHaveLength(35);
  });

  it("resolves the keys the editor and the bank write", () => {
    for (const key of editorFactoryKeys()) {
      expect(isKnownParam(key), key).toBe(true);
    }
    for (const preset of EXTRA_PRESETS) {
      const unknown = preset.params.map(([key]) => key).filter((key) => !isKnownParam(key));
      expect(unknown, preset.name).toEqual([]);
    }
    // factoryParams writes warp_var even though the old editor had no control for it.
    expect(isKnownParam("osc.a.warp_var")).toBe(true);
    expect(specForConcreteKey("osc.a.warp_var")?.group).toBe("osc");
  });

  it("covers the controls it declares", () => {
    const filled = [
      "osc.a.warp_var",
      "osc.b.warp_var",
      "osc.c.warp_var",
      "osc.a.warp2_mode",
      "osc.b.warp2_amount",
      "osc.c.warp2_var",
      "osc.a.pan",
      "filter.wet",
      "filter2.wet",
    ];
    const source = readFileSync(`${process.cwd()}/src/components/OpenSynthEditor.tsx`, "utf8");
    for (const key of filled) {
      const spec = paramSpec(key);
      expect(spec, key).toBeDefined();
      expect(spec?.control, key).not.toBe("none");
      expect(source, key).toContain(`"${key}"`);
    }
  });

  it("gives every catalog label an en, zh-Hans and zh-Hant string", () => {
    const keys = new Set<string>();
    for (const param of SYNTH_CATALOG.params) keys.add(param.label);
    for (const family of SYNTH_CATALOG.families) {
      for (const field of family.fields) keys.add(field.label);
    }
    for (const group of SYNTH_CATALOG.groups) keys.add(group.label);
    for (const fx of SYNTH_CATALOG.fxTypes) {
      for (const param of fx.params) keys.add(param.label);
    }
    keys.add("synth.fx.noControls");
    expect(keys.size).toBeGreaterThan(120);
    for (const key of keys) {
      for (const locale of ["en", "zh-Hans", "zh-Hant"] as const) {
        expect(i18n.has(locale, key as I18nKey), `${locale} ${key}`).toBe(true);
      }
    }
  });

  it("has control metadata for every selectable parameter", () => {
    for (const param of SYNTH_CATALOG.params) {
      if (param.control === "none") continue;
      expect(param.label, param.ids[0]).toMatch(/^synth\.param\./);
      if (param.control === "select" && param.enumId) {
        const enumSpec = [
          SYNTH_CATALOG.modSources,
          SYNTH_CATALOG.modDestinations,
          SYNTH_CATALOG.filterModels,
          SYNTH_CATALOG.filterModes,
          SYNTH_CATALOG.filterRoutings,
          SYNTH_CATALOG.warpModes,
          SYNTH_CATALOG.waveforms,
          SYNTH_CATALOG.subShapes,
          SYNTH_CATALOG.lfoShapes,
          SYNTH_CATALOG.qualityModes,
          SYNTH_CATALOG.fxParamKinds,
        ].find((entry) => entry.id === param.enumId);
        expect(enumSpec, param.ids[0]).toBeDefined();
        expect(param.max, param.ids[0]).toBe((enumSpec?.options.length ?? 0) - 1);
      }
    }
  });
});
