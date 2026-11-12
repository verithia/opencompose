/** Pure mixer-routing helpers: bus naming, usage counts and the send dB law. */
import { afterEach, describe, it, expect } from "vitest";
import { makeSnapshot, makeTrack } from "../setup";
import { i18n } from "../../stores/i18n";
import type { Project } from "../../types/project";
import {
  amountToDb,
  busReferences,
  busTotalReferences,
  dbToAmount,
  describeBusReferences,
  effectiveBusRoutes,
  nextBusName,
} from "../../utils/mixerRouting";

function project(): Project {
  return makeSnapshot({
    tracks: [
      {
        ...makeTrack("Keys", 0),
        mix: {
          output: "space",
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "space", amount: 0.5 }],
        },
      },
      {
        ...makeTrack("Drums", 1),
        mix: {
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "space", amount: 0.25 }, { target: "glue", amount: 0.1 }],
        },
      },
    ],
    buses: [
      { name: "space", routes: ["master"], effects: [] },
      { name: "glue", routes: ["space"], effects: [] },
      { name: "room", routes: ["master"], effects: [] },
    ],
  }).project;
}

describe("mixerRouting", () => {
  it("names the next bus without colliding with an existing one", () => {
    expect(nextBusName([])).toBe("Bus 1");
    expect(nextBusName([{ name: "Bus 1" }])).toBe("Bus 2");
    expect(nextBusName([{ name: "Bus 1" }, { name: "Bus 3" }])).toBe("Bus 2");
    expect(nextBusName([{ name: "space" }])).toBe("Bus 1");
  });

  it("treats empty bus routes as the master default", () => {
    expect(effectiveBusRoutes({ name: "x", routes: [], effects: [] })).toEqual(["master"]);
    expect(effectiveBusRoutes({ name: "x", routes: ["glue"], effects: [] })).toEqual(["glue"]);
  });

  it("counts every reference that keeps a bus alive", () => {
    const refs = busReferences(project(), "space");
    expect(refs.trackOutputs).toEqual([0]);
    expect(refs.sends).toEqual([0, 1]);
    expect(refs.busRoutes).toEqual(["glue"]);
    expect(busTotalReferences(refs)).toBe(4);
    expect(describeBusReferences(refs, i18n.t)).toBe("1 track output, 2 sends and 1 bus route");

    const free = busReferences(project(), "room");
    expect(busTotalReferences(free)).toBe(0);
    expect(describeBusReferences(free, i18n.t)).toBe("");
  });

  it("describes one and two reference kinds without a list", () => {
    expect(describeBusReferences({ trackOutputs: [0], sends: [], busRoutes: [] }, i18n.t)).toBe("1 track output");
    expect(describeBusReferences({ trackOutputs: [0], sends: [1], busRoutes: [] }, i18n.t)).toBe("1 track output and 1 send");
  });

  it("converts send level dB to a bounded linear gain", () => {
    expect(amountToDb(0)).toBe(-Infinity);
    expect(amountToDb(1)).toBeCloseTo(0, 12);
    expect(amountToDb(0.5)).toBeCloseTo(-6.020599913279624, 9);
    expect(dbToAmount(-6.020599913279624)).toBeCloseTo(0.5, 12);
    expect(dbToAmount(-Infinity)).toBe(0);
    expect(dbToAmount(24)).toBe(4);
    expect(amountToDb(dbToAmount(-12))).toBeCloseTo(-12, 9);
  });

  it("localizes the reference list for a Chinese inspector", () => {
    afterEach(() => i18n.setLocale("en"));
    i18n.setLocale("zh-Hans");
    const refs = busReferences(project(), "space");
    expect(describeBusReferences(refs, i18n.t)).toBe("1 个轨道输出、2 个发送和1 条总线路由");
  });
});
