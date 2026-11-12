import { describe, it, expect } from "vitest";
import { createProjectClock } from "../utils/projectClock";
import type { ProjectProperties } from "../types/project";

const properties = (): ProjectProperties => ({ tempo: 120, time_signature: {numerator:7,denominator:8}, key:null,swing:null,key_map:[],
  tempo_map:[{position:{bar:1,beat:5,subdivision:0},bpm:60}],
  time_sig_map:[{position:{bar:3,beat:1,subdivision:0},numerator:3,denominator:4}] });
describe("project clock contract", () => {
  it("matches Rust fine tempo residual integration", () => {
    const p = properties();
    p.time_signature = { numerator:4, denominator:4 }; p.time_sig_map=[];
    p.tempo_map=[{position:{bar:1,beat:1,subdivision:0},offset_quarters:0.125,bpm:60}];
    const clock = createProjectClock(p);
    expect(clock.secondsAt(0.125)).toBe(0.0625);
    expect(clock.secondsAt(1)).toBe(0.9375);
    expect(clock.quarterAtSeconds(0.9375)).toBe(1);
    p.tempo_map[0].offset_quarters=0.25;
    expect(() => createProjectClock(p)).toThrow("Invalid tempo residual");
  });
  it("matches Rust mapped transport fixture and integrates across a change", () => {
    const clock = createProjectClock(properties());
    expect(clock.quarterAt({bar:2,beat:1,subdivision:0})).toBe(3.5);
    expect(clock.quarterAt({bar:3,beat:2,subdivision:0})).toBe(8);
    expect(clock.barBeatAt(3.625)).toEqual({bar:2,beat:1,subdivision:1});
    expect(clock.barBeatAt(7.5)).toEqual({bar:3,beat:1,subdivision:2});
    for (const quarter of [0, 3.5, 3.625, 7, 7.5, 10]) {
      expect(clock.quarterAt(clock.barBeatAt(quarter))).toBe(quarter);
    }
    expect(clock.quarterAtSeconds(6.5)).toBe(7.5);
    expect(clock.secondsAt(7.5)).toBe(6.5);
    expect(clock.quarterAtSeconds(clock.secondsAt(1.5)+1)).toBe(2.75);
  });
  it("retains constant tempo and rejects invalid maps", () => {
    const p = properties(); p.tempo_map=[]; p.time_sig_map=[];
    expect(createProjectClock(p).secondsAt(8)).toBe(4);
    p.tempo_map=[{position:{bar:1,beat:1,subdivision:0},bpm:60}];
    expect(createProjectClock(p).secondsAt(8)).toBe(8);
    p.tempo_map.push(p.tempo_map[0]);
    expect(() => createProjectClock(p)).toThrow("Invalid tempo map");
  });
  it("exposes map lookups for the tempo lane", () => {
    const clock = createProjectClock(properties());
    expect(clock.barStartQuarter(2)).toBe(3.5);
    expect(clock.barStartQuarter(4)).toBe(10);
    expect(clock.meterAtBar(2)).toEqual({ numerator: 7, denominator: 8 });
    expect(clock.meterAtBar(3)).toEqual({ numerator: 3, denominator: 4 });
    expect(clock.tempoAt(1.9)).toBe(120);
    expect(clock.tempoAt(2)).toBe(60);
    expect(clock.tempoAt(12)).toBe(60);
    expect(() => clock.meterAtBar(0)).toThrow("Invalid bar");
  });
});
