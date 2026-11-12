import { beforeEach, describe, expect, it } from "vitest";
import { SYNTH_CATALOG, specForConcreteKey } from "../../data/synthCatalog";
import { i18n } from "../../stores/i18n";
import {
  automationLabel,
  automationLaneKey,
  automationLaneName,
  automationTarget,
  type AutomateTarget,
  type UnavailableTarget,
} from "../../utils/automationTarget";

beforeEach(() => i18n.setLocale("en"));

function target(key: string): AutomateTarget {
  const resolved = automationTarget(key);
  if (!resolved.available) throw new Error(`${key}: ${resolved.reason}`);
  return resolved;
}

function unavailable(key: string): UnavailableTarget {
  const resolved = automationTarget(key);
  if (resolved.available) throw new Error(`${key} should not be automatable`);
  return resolved;
}

describe("automation target encoding", () => {
  it("uses schema identity laws for all amplitude fractions without changing signed zero", () => {
    const keys = ["osc.a.level", "osc.b.level", "osc.c.level", "ring.level", "sub.level", "noise.level", "output.gain", "gain", "amp.sustain", "sustain", "filter_env.sustain", "env3.sustain", "env4.sustain", "amp.damping", "velocity.to_amp"];
    expect(keys).toHaveLength(15);
    for (const key of keys) {
      const spec = specForConcreteKey(key)!;
      expect(spec.dspNormalization, key).toBe("identity");
      const resolved = target(key);
      expect(resolved.domain, key).toEqual([0, 1]);
      for (const value of [-0, 0, Number.MIN_VALUE, 0.125, 0.72, 1]) {
        expect(Object.is(resolved.decode(value), value), `decode ${key}`).toBe(true);
        expect(Object.is(resolved.encode(value), value), `encode ${key}`).toBe(true);
        expect(Object.is(resolved.decode(resolved.encode(value)), value), `roundtrip ${key}`).toBe(true);
      }
      expect(resolved.decode(resolved.encode(spec.default)), key).toBe(spec.default);
      expect(resolved.decode(-1), key).toBe(0);
      expect(resolved.decode(2), key).toBe(1);
    }
  });

  it("uses shared signed-modulation laws for all 197 controls including every FX route", () => {
    const keys = ["amp.curve", "filter_env.curve", "env3.curve", "env4.curve", "filter.env.amount"];
    for (let slot = 1; slot <= 64; slot += 1) {
      keys.push(`mod.${slot}.amount`, `mod.${slot}.curve`, `fxmod.${slot}.amount`);
    }
    expect(keys).toHaveLength(197);
    for (const key of keys) {
      const spec = specForConcreteKey(key)!;
      expect(spec, key).toBeTruthy();
      expect(spec.dspNormalization, key).toBe("linear");
      const resolved = target(key);
      const min = key === "filter.env.amount" ? -8 : -1;
      const max = -min;
      expect(resolved.domain, key).toEqual([min, max]);
      expect(resolved.stepped, key).toBe(false);
      for (const position of [0, 0.125, 0.5, 0.875, 1]) {
        const physical = position * (max - min) + min;
        expect(resolved.decode(position), key).toBe(physical);
        expect(resolved.encode(physical), key).toBeCloseTo(position, 12);
        expect(resolved.decode(resolved.encode(physical)), key).toBeCloseTo(physical, 12);
      }
      expect(resolved.encode(min - 1), key).toBe(0);
      expect(resolved.encode(max + 1), key).toBe(1);
      expect(resolved.decode(resolved.encode(spec.default)), key).toBe(spec.default);
    }
    // Family membership comes from the catalog; removed fallback tables must
    // not advertise nonexistent routes as valid normalized targets.
    for (const key of ["mod.0.amount", "mod.65.curve", "fxmod.0.amount", "fxmod.65.amount"]) {
      expect(unavailable(key).reason, key).toContain("no normalized automation range");
    }
  });


  it("reads all LFO phase bend smooth and step affine laws from shared family metadata", () => {
    const keys = ["lfo.phase", "lfo.bend", "lfo.smooth", ...Array.from({ length: 9 }, (_, i) => [`lfo${i + 2}.phase`, `lfo${i + 2}.bend`, `lfo${i + 2}.smooth`]).flat(), ...Array.from({ length: 16 }, (_, i) => `step.${i + 1}`), "step.smoothing"];
    expect(keys).toHaveLength(47);
    for (const key of keys) {
      const spec = specForConcreteKey(key)!;
      expect(spec.dspNormalization, key).toBe("linear");
      const curve = target(key);
      for (const normalized of [0, -0, 0.125, 0.5, 0.875, 1]) {
        const expected = spec.min === 0 ? normalized * spec.max : spec.min + normalized * (spec.max - spec.min);
        expect(curve.decode(normalized), key).toBe(expected);
        expect(curve.encode(expected), key).toBeCloseTo(normalized, 12);
      }
      if (spec.min === 0) {
        expect(curve.decode(-0), key).toBe(-0);
        expect(curve.encode(-0), key).toBe(-0);
      }
    }
  });

  it("reads oscillator affine laws and exact pulse width coefficients from the schema", () => {
    const keys = ["osc.a.pan", "osc.b.pan", "osc.c.pan", "osc.a.pulse_width", "osc.b.pulse_width", "osc.c.pulse_width", "osc.b.semitones", "osc.c.semitones", "osc.a.detune", "osc.b.detune", "osc.c.detune", "unison.detune", "detune"];
    for (const key of keys) {
      const spec = specForConcreteKey(key)!;
      expect(spec.dspNormalization, key).toBe("linear");
      const span = spec.dspNormalizationSpan ?? (spec.max - spec.min);
      const resolved = target(key);
      for (const position of [0, 0.125, 0.5, 0.875, 1]) {
        const physical = spec.min + position * span;
        expect(resolved.decode(position), key).toBe(physical);
        expect(resolved.encode(physical), key).toBe(Math.min(1, Math.max(0, (physical - spec.min) / span)));
      }
    }
    for (const oscillator of ["a", "b", "c"]) {
      const key = `osc.${oscillator}.pulse_width`;
      expect(specForConcreteKey(key)?.dspNormalizationSpan).toBe(0.9);
      expect(target(key).decode(0.5)).toBe(0.5);
      expect(target(key).decode(1)).toBe(0.05 + 0.9);
    }
    expect(automationTarget("osc.a.semitones").available).toBe(false);
  });

  it("uses schema DSP laws for every power-time parameter and family", () => {
    const keys = new Set<string>();
    for (const spec of SYNTH_CATALOG.params) {
      if (spec.dspNormalization === "square" || spec.dspNormalization === "cubic") for (const key of [...spec.ids, ...spec.aliases]) keys.add(key);
    }
    for (const family of SYNTH_CATALOG.families) {
      for (const field of family.fields) {
        if (field.dspNormalization !== "square" && field.dspNormalization !== "cubic") continue;
        for (let index = family.first; index <= family.last; index++) {
          keys.add(`${family.prefix}${index}${field.suffix ? `.${field.suffix}` : ""}`);
        }
      }
    }
    expect(keys.size).toBeGreaterThanOrEqual(161);
    for (const key of keys) {
      const spec = specForConcreteKey(key)!;
      const resolved = target(key);
      const exponent = spec.dspNormalization === "square" ? 2 : 3;
      for (const position of [0, 0.125, 0.5, 0.875, 1]) {
        const physical = position ** exponent * spec.max;
        expect(resolved.decode(position), key).toBeCloseTo(physical, 12);
        expect(resolved.encode(physical), key).toBeCloseTo(position, 12);
      }
    }
    for (const [key, expected] of [["amp.attack", 1.25], ["amp.release", 2.5], ["voice.glide", 0.5], ["lfo10.rise", 1.25], ["mod.64.slew_up", 0.125]] as const) {
      expect(target(key).decode(0.5), key).toBe(expected);
    }
  });

  it("reads log/rate automation floors independently from physical storage and runtime cutoffs", () => {
    for (const key of ["filter.cutoff", "filter2.cutoff", "cutoff", "lfo.rate", ...Array.from({ length: 9 }, (_, index) => `lfo${index + 2}.rate`), "step.rate"]) {
      const spec = specForConcreteKey(key)!;
      const floor = spec.dspNormalizationFloor ?? spec.min;
      const curve = target(key);
      expect(curve.domain, key).toEqual([floor, spec.max]);
      for (const position of [0, 0.125, 0.5, 0.875, 1]) {
        const physical = floor * (spec.max / floor) ** position;
        expect(curve.decode(position), key).toBeCloseTo(physical, 8);
        expect(curve.encode(physical), key).toBeCloseTo(position, 12);
      }
    }
    expect(specForConcreteKey("lfo.rate")?.min).toBe(0);
    expect(target("lfo.rate").domain[0]).toBe(0.01);
    expect(target("lfo.rate").decode(0)).toBe(0.01);
    expect(specForConcreteKey("filter.cutoff")?.dspRuntimeBounds).toEqual({ min: 8, maxSampleRateRatio: 0.45 });
  });

  it("reuses the hosted-plugin key encoding for lane names", () => {
    expect(automationLaneName("filter.cutoff")).toBe("pluginparam_66696c7465722e6375746f6666");
    expect(automationLaneKey("pluginparam_66696c7465722e6375746f6666")).toBe("filter.cutoff");
    expect(automationLaneKey("pluginparam_46696c746572204375746f6666")).toBe("Filter Cutoff");
    expect(automationLaneKey("volume")).toBeNull();
    expect(automationLaneKey("pluginparam_")).toBeNull();
    expect(automationLaneKey("pluginparam_xyz")).toBeNull();
    expect(automationLaneKey("pluginparam_666")).toBeNull();
  });

  it("round-trips cutoff over the engine's log domain", () => {
    const cutoff = target("filter.cutoff");
    expect(cutoff.lane).toBe(automationLaneName("filter.cutoff"));
    expect(cutoff.label).toBe("Filter Cutoff");
    expect(cutoff.encode(20)).toBe(0);
    expect(cutoff.encode(20000)).toBe(1);
    // 200 Hz is one decade above the 20 Hz floor of three decades.
    expect(cutoff.encode(200)).toBeCloseTo(1 / 3, 12);
    expect(cutoff.encode(2000)).toBeCloseTo(2 / 3, 12);
    for (const hz of [20, 200, 440, 1000, 12000, 20000]) {
      expect(cutoff.decode(cutoff.encode(hz))).toBeCloseTo(hz, 6);
    }
    expect(cutoff.decode(0.5)).toBeCloseTo(20 * Math.sqrt(1000), 9);
  });

  it("round-trips resonance and a level as identity domains", () => {
    const resonance = target("filter.resonance");
    expect(resonance.label).toBe("Filter Resonance");
    for (const value of [0, 0.15, 0.5, 1]) {
      expect(resonance.encode(value)).toBeCloseTo(value, 12);
      expect(resonance.decode(value)).toBeCloseTo(value, 12);
    }
    expect(resonance.encode(2)).toBe(1);
    expect(resonance.decode(-1)).toBe(0);

    const level = target("osc.a.level");
    expect(level.label).toBe("Oscillator Level");
    for (const value of [0, 0.22, 0.78, 1]) {
      expect(level.decode(level.encode(value))).toBeCloseTo(value, 12);
    }
  });

  it("round-trips ratio and percent domains", () => {
    const pan = target("osc.a.pan");
    expect(pan.encode(-1)).toBe(0);
    expect(pan.encode(0)).toBe(0.5);
    expect(pan.encode(1)).toBe(1);
    expect(pan.decode(pan.encode(-0.35))).toBeCloseTo(-0.35, 12);

    const detune = target("unison.detune");
    expect(detune.encode(7)).toBeCloseTo(0.07, 12);
    expect(detune.decode(0.07)).toBeCloseTo(7, 10);

    const semitones = target("osc.b.semitones");
    expect(semitones.encode(-24)).toBe(0);
    expect(semitones.encode(24)).toBe(1);
    expect(semitones.decode(semitones.encode(7))).toBeCloseTo(7, 10);

    const amount = target("filter.env.amount");
    expect(amount.encode(-8)).toBe(0);
    expect(amount.encode(8)).toBe(1);
    expect(amount.decode(amount.encode(-2.5))).toBeCloseTo(-2.5, 10);
  });

  it("matches the catalog enum domains so the two tables cannot drift", () => {
    for (const key of [
      "osc.a.waveform",
      "lfo.waveform",
      "filter.model",
      "filter2.model",
      "filter.mode",
      "filter.routing",
      "sub.waveform",
      "quality.oversampling",
      "lfo.shape",
      "mod.1.source",
      "mod.1.destination",
      "osc.b.unison",
      "osc.c.unison",
      "unison.detune",
      "osc.b.detune",
      "osc.c.detune",
    ]) {
      const spec = specForConcreteKey(key);
      expect(spec, key).toBeTruthy();
      const resolved = target(key);
      expect(resolved.encode(spec!.max), key).toBeCloseTo(1, 12);
      expect(resolved.decode(1), key).toBeCloseTo(spec!.max, 12);
    }
  });

  it("spans every enum value the engine exposes", () => {
    const mode = target("filter.mode");
    expect(mode.stepped).toBe(true);
    expect(mode.encode(0)).toBe(0);
    expect(mode.encode(3)).toBe(1);
    expect(mode.decode(0.5)).toBe(2);
    for (let model = 0; model <= 3; model += 1) {
      expect(mode.decode(mode.encode(model))).toBe(model);
    }

    const shape = target("lfo.shape");
    for (let option = 0; option <= 4; option += 1) {
      expect(shape.decode(shape.encode(option))).toBe(option);
    }

    // All twelve filter models, including the appended ladder taps.
    const model = target("filter.model");
    expect(model.encode(0)).toBe(0);
    expect(model.encode(11)).toBe(1);
    for (let option = 0; option <= 11; option += 1) {
      expect(model.decode(option / 11)).toBe(option);
      expect(model.decode(model.encode(option))).toBe(option);
    }

    // Granular (5) and Spectral (6) are inside the widened waveform domain.
    const waveform = target("osc.a.waveform");
    expect(waveform.encode(0)).toBe(0);
    expect(waveform.encode(6)).toBe(1);
    for (let option = 0; option <= 6; option += 1) {
      expect(waveform.decode(option / 6)).toBe(option);
    }

    // Twenty-nine sources and thirty-five numeric destinations.
    const source = target("mod.1.source");
    for (let option = 0; option <= 28; option += 1) {
      expect(source.decode(option / 28)).toBe(option);
    }
    const destination = target("mod.1.destination");
    for (let option = 0; option <= 34; option += 1) {
      expect(destination.decode(option / 34)).toBe(option);
    }

    // B/C detune and unison gained normalized arms.
    for (const key of ["osc.b.detune", "osc.c.detune"]) {
      const detune = target(key);
      expect(detune.decode(0)).toBe(0);
      expect(detune.decode(1)).toBe(100);
    }
    for (const key of ["osc.b.unison", "osc.c.unison"]) {
      const unison = target(key);
      expect(unison.decode(0)).toBe(1);
      expect(unison.decode(1)).toBe(16);
    }
    expect(target("sub.waveform").decode(1)).toBe(5);
  });

  it("refuses non-numeric parameters and explains why", () => {
    expect(unavailable("wavetable.file").reason).toContain("not a numeric value");
    expect(unavailable("serum_file").reason).toContain("not a numeric value");
    expect(unavailable("").reason).toContain("empty");
  });

  it("names matrix, bank, macro and step targets", () => {
    expect(target("mod.3.amount").label).toBe("Route 3 amount");
    expect(target("mod.1.slew_up").label).toBe("Route 1 slew up");
    expect(target("lfo2.rate").label).toBe("LFO 2 rate");
    expect(target("lfo.sync_div").label).toBe("LFO 1 sync div");
    expect(target("macro.4").label).toBe("Macro 4");
    expect(target("step.5").label).toBe("Step 5");
    expect(automationLabel("filter.cutoff")).toBe("Filter Cutoff");
    expect(automationLabel("unknown.thing")).toBe("unknown.thing");
  });
});
