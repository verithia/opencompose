/**
 * Effect glyph coverage — a new built-in device family must not fall back to
 * the generic plug, and every family gets a distinct icon.
 */
import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { For } from "solid-js";
import {
  BUILTIN_EFFECT_TYPES,
  EffectIcon,
  effectGlyph,
  effectKey,
  effectLabel,
  hasEffectGlyph,
} from "../components/effectIcons";
import type { EffectType } from "../types/project";

const PLUGIN: EffectType = { Plugin: "Acme.BusComp" };

// Families the device catalog could add before the frontend knew their names.
const LATE_FAMILIES: { id: string; type: EffectType }[] = [
  { id: "breath-split", type: "BreathSplit" },
  { id: "reverb-fdn", type: "ReverbFdn" },
  { id: "resonance", type: "Resonance" },
  { id: "tuner", type: "Tuner" },
];

describe("effect glyphs", () => {
  it("covers every built-in EffectType member with a non-fallback glyph", () => {
    expect(BUILTIN_EFFECT_TYPES.length).toBe(28);
    const plug = effectGlyph(PLUGIN);
    for (const type of BUILTIN_EFFECT_TYPES) {
      expect(hasEffectGlyph(type), `${type} has no glyph`).toBe(true);
      expect(effectKey(type)).toBe(type);
      expect(effectGlyph(type), `${type} falls back to the plug`).not.toBe(plug);
      expect(effectLabel(type)).not.toBe("Plugin");
      expect(effectLabel(type).length).toBeGreaterThan(0);
    }
  });

  it("falls back to the plug only for plugins and unknown types", () => {
    const plug = effectGlyph(PLUGIN);
    expect(hasEffectGlyph(PLUGIN)).toBe(false);
    expect(effectKey(PLUGIN)).toBe("Plugin");
    expect(effectGlyph(PLUGIN)).toBe(plug);
    expect(effectLabel(PLUGIN)).toBe("Acme.BusComp");

    const unknown = "NotADevice" as EffectType;
    expect(hasEffectGlyph(unknown)).toBe(false);
    expect(effectGlyph(unknown)).toBe(plug);
  });

  it("draws a distinct glyph for every built-in family", () => {
    const { container } = render(() => (
      <For each={BUILTIN_EFFECT_TYPES}>{(type) => <EffectIcon type={type} />}</For>
    ));
    const icons = Array.from(container.querySelectorAll(".effect-icon"));
    expect(icons.length).toBe(BUILTIN_EFFECT_TYPES.length);
    expect(icons.map((icon) => icon.getAttribute("data-effect"))).toEqual([...BUILTIN_EFFECT_TYPES]);

    const paths = icons.map((icon) => icon.querySelector("path")?.getAttribute("d"));
    expect(paths.every(Boolean)).toBe(true);
    expect(new Set(paths).size).toBe(BUILTIN_EFFECT_TYPES.length);
  });

  it("gives the late families their own glyphs, not the plug", () => {
    const { container } = render(() => (
      <For each={BUILTIN_EFFECT_TYPES}>{(type) => <EffectIcon type={type} />}</For>
    ));
    const pathFor = (type: EffectType) =>
      container.querySelector(`.effect-icon[data-effect="${effectKey(type)}"] path`)?.getAttribute("d");
    const plugContainer = render(() => <EffectIcon type={PLUGIN} />);
    const plugPath = plugContainer.container.querySelector("path")?.getAttribute("d");
    expect(plugPath).toBeTruthy();

    const latePaths = LATE_FAMILIES.map(({ id, type }) => {
      expect(hasEffectGlyph(type), `${id} has no glyph`).toBe(true);
      expect(effectKey(type)).toBe(type);
      expect(effectLabel(type).length).toBeGreaterThan(0);
      const drawn = pathFor(type);
      expect(drawn, `${id} draws the plug`).toBeTruthy();
      expect(drawn, `${id} draws the plug`).not.toBe(plugPath);
      return drawn;
    });
    expect(new Set(latePaths).size).toBe(LATE_FAMILIES.length);

    const earlyPaths = BUILTIN_EFFECT_TYPES
      .filter((type) => !LATE_FAMILIES.some((late) => late.type === type))
      .map((type) => pathFor(type));
    for (const path of latePaths) expect(earlyPaths).not.toContain(path);
    expect(effectLabel("BreathSplit")).toBe("Breath Split");
    expect(effectLabel("ReverbFdn")).toBe("FDN Reverb");
    expect(effectLabel("Resonance")).toBe("Resonance Suppressor");
    expect(effectLabel("Tuner")).toBe("Tuner");
  });
});
