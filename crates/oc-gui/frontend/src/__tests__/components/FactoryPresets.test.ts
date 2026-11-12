import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { EXTRA_PRESETS, type FactoryPreset, type PresetCategory } from "../../data/factoryPresets";
import { SYNTH_CATALOG } from "../../data/synthCatalog";

const CATEGORIES: PresetCategory[] = ["bass", "lead", "pad", "pluck", "keys", "brass", "fx"];

/** The documented size of each category; the manual and `docs/factory-presets.md` agree. */
const CATEGORY_COUNTS: Record<PresetCategory, number> = {
  bass: 10, lead: 10, pad: 12, pluck: 9, keys: 8, brass: 6, fx: 7,
};
const BANK_SIZE = 62;

/**
 * Keys a load can write: the catalog's parameter table and families (the
 * single source of truth for the patch default) plus the override table.
 */
function catalogKeys(): Set<string> {
  const keys = new Set<string>();
  for (const spec of SYNTH_CATALOG.params) {
    for (const id of spec.ids) keys.add(id);
  }
  for (const family of SYNTH_CATALOG.families) {
    for (let index = family.first; index <= family.last; index += 1) {
      for (const field of family.fields) {
        keys.add(field.suffix === ""
          ? `${family.prefix}${index}`
          : `${family.prefix}${index}.${field.suffix}`);
      }
    }
  }
  return keys;
}

function editorFactoryKeys(): Set<string> {
  const keys = catalogKeys();
  const source = readFileSync(`${process.cwd()}/src/components/OpenSynthEditor.tsx`, "utf8");
  const start = source.indexOf("const FACTORY_PATCHES");
  expect(start, "factory patch table").toBeGreaterThanOrEqual(0);
  const end = source.indexOf("\n};", start);
  expect(end, "factory patch table end").toBeGreaterThan(start);
  for (const match of source.slice(start, end).matchAll(/\["([^"]+)"/g)) keys.add(match[1]);
  for (const key of fxSlotKeys()) keys.add(key);
  return keys;
}

/** FX slot keys: the rack count, slot type/enabled and every catalogued FX field. */
function fxSlotKeys(): string[] {
  const fields = new Set(["type", "enabled"]);
  for (const fx of SYNTH_CATALOG.fxTypes) {
    for (const param of fx.params) fields.add(param.key);
  }
  expect(fields.size, "FX catalog parameter specs").toBeGreaterThan(20);
  const keys = ["fx.count"];
  for (let slot = 1; slot <= 16; slot += 1) {
    for (const field of fields) keys.push(`fx.${slot}.${field}`);
  }
  return keys;
}

/** The four patches already in the editor; a new preset must not reuse a name. */
function builtInPatchNames(): string[] {
  const source = readFileSync(`${process.cwd()}/src/components/OpenSynthEditor.tsx`, "utf8");
  const start = source.indexOf("const FACTORY_PATCHES");
  const end = source.indexOf("\n};", start);
  const names: string[] = [];
  for (const match of source.slice(start, end).matchAll(/\n {2}"?([A-Za-z][A-Za-z ]*)"?:\s*\[/g)) {
    names.push(match[1]);
  }
  return names;
}

function isSingleSentence(text: string): boolean {
  return (
    text.length > 0 &&
    text === text.trim() &&
    /^[A-Z]/.test(text) &&
    text.endsWith(".") &&
    !text.slice(0, -1).includes(".") &&
    !/[!?]/.test(text) &&
    !text.includes("  ")
  );
}

function numeric(preset: FactoryPreset, key: string): number | null {
  const found = preset.params.find(([name]) => name === key)?.[1];
  if (found === null || found === undefined) return null;
  const value = Number(found);
  return Number.isFinite(value) ? value : null;
}

/** Stable identifier derived from the name; the browser keys rows by name. */
function slug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

describe("OpenSynth extra factory presets", () => {
  it("ships the documented bank size and per-category counts", () => {
    expect(EXTRA_PRESETS).toHaveLength(BANK_SIZE);
    const counts = new Map<PresetCategory, number>(CATEGORIES.map((category) => [category, 0]));
    for (const preset of EXTRA_PRESETS) {
      expect(CATEGORIES, preset.name).toContain(preset.category);
      counts.set(preset.category, (counts.get(preset.category) ?? 0) + 1);
    }
    for (const category of CATEGORIES) {
      expect(counts.get(category), category).toBe(CATEGORY_COUNTS[category]);
    }
  });

  it("keeps product names and their identifiers unique", () => {
    const names = EXTRA_PRESETS.map((preset) => preset.name);
    expect(names.every((name) => name.trim() === name && name.length > 0)).toBe(true);
    expect(new Set(names).size).toBe(names.length);
    const slugs = names.map(slug);
    expect(new Set(slugs).size).toBe(slugs.length);
    const builtIn = new Set(builtInPatchNames());
    expect(builtIn.size).toBeGreaterThanOrEqual(4);
    expect(names.filter((name) => builtIn.has(name))).toEqual([]);
  });

  it("only names parameters the factory table already accepts", () => {
    const keys = editorFactoryKeys();
    // The editor generates a few families, so the table alone is smaller.
    expect(keys.size).toBeGreaterThan(300);
    for (const required of ["synth_version", "filter.cutoff", "mod.64.amount", "step.16", "macro.8", "fx.count"]) {
      expect(keys, required).toContain(required);
    }
    for (const preset of EXTRA_PRESETS) {
      const unknown = preset.params.map(([key]) => key).filter((key) => !keys.has(key));
      expect(unknown, preset.name).toEqual([]);
    }
  });

  it("keeps the hand-written Init defaults inside the catalog", () => {
    const source = readFileSync(`${process.cwd()}/src/components/OpenSynthEditor.tsx`, "utf8");
    const start = source.indexOf("  Init: [");
    expect(start, "Init table").toBeGreaterThanOrEqual(0);
    const end = source.indexOf("  ],", start);
    expect(end, "Init table end").toBeGreaterThan(start);
    const init = new Set<string>();
    for (const match of source.slice(start, end).matchAll(/\["([^"]+)"/g)) init.add(match[1]);
    expect(init.size).toBeGreaterThan(140);
    const known = catalogKeys();
    for (const key of init) expect(known, key).toContain(key);
  });

  it("writes a complete, self-consistent patch", () => {
    for (const preset of EXTRA_PRESETS) {
      // Every preset states its own gain and amplitude envelope rather than
      // inheriting whatever values Init happens to carry.
      for (const key of ["output.gain", "amp.attack", "amp.decay", "amp.sustain", "amp.release"]) {
        expect(preset.params.some(([name]) => name === key), `${preset.name}: ${key}`).toBe(true);
      }
      const gain = numeric(preset, "output.gain");
      expect(gain, preset.name).not.toBeNull();
      expect(gain!, preset.name).toBeGreaterThanOrEqual(0.05);
      expect(gain!, preset.name).toBeLessThanOrEqual(1);
      // Some source must be audible: an oscillator, the sub or the noise.
      const sources = ["osc.a.level", "osc.b.level", "osc.c.level", "sub.level", "noise.level"]
        .map((key) => numeric(preset, key) ?? 0)
        .reduce((sum, level) => sum + level, 0);
      expect(sources, `${preset.name} is silent`).toBeGreaterThan(0);
    }
  });

  it("describes every preset in one short sentence", () => {
    for (const preset of EXTRA_PRESETS) {
      expect(isSingleSentence(preset.description), `${preset.name}: ${preset.description}`).toBe(true);
      expect(preset.description.length, preset.name).toBeLessThanOrEqual(140);
      expect(preset.description.toLowerCase(), preset.name).not.toBe(preset.name.toLowerCase());
    }
  });

  it("leaves no placeholder text behind", () => {
    const placeholders = /todo|tbd|placeholder|untitled|lorem|preset \d|test preset|xxx/i;
    for (const preset of EXTRA_PRESETS) {
      expect(preset.name, preset.name).not.toMatch(placeholders);
      expect(preset.description, preset.name).not.toMatch(placeholders);
      expect(preset.name, preset.name).not.toMatch(/\s{2}|\s$/);
    }
  });

  it("stores params as key/value strings with no duplicate keys", () => {
    for (const preset of EXTRA_PRESETS) {
      const seen = new Set<string>();
      for (const [key, raw] of preset.params as [string, unknown][]) {
        expect(key, preset.name).toMatch(/^[a-z][a-z0-9_.]*$/);
        expect(seen.has(key), `${preset.name}: duplicate ${key}`).toBe(false);
        seen.add(key);
        const stringish =
          raw === null ||
          (typeof raw === "number" ? Number.isFinite(raw) : typeof raw === "string" && raw.length > 0);
        expect(stringish, `${preset.name}: ${key}`).toBe(true);
        // `fx.N.type` names the module by label; every other value is a number.
        if (typeof raw === "string" && !/^fx\.\d+\.type$/.test(key)) {
          expect(Number.isFinite(Number(raw)), `${preset.name}: ${key}=${raw}`).toBe(true);
        }
      }
    }
  });

  it("declares a rack before it names effect slots", () => {
    for (const preset of EXTRA_PRESETS) {
      let slots = 0;
      for (const [key] of preset.params) {
        const match = /^fx\.(\d+)\./.exec(key);
        if (match) slots = Math.max(slots, Number(match[1]));
      }
      if (slots === 0) continue;
      const count = numeric(preset, "fx.count");
      expect(count, `${preset.name}: fx.count`).not.toBeNull();
      expect(count!, preset.name).toBeGreaterThanOrEqual(slots);
    }
  });
});
