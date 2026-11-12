import { ownedParameterSurface } from "../stores/ownedParameterGesture";
/**
 * The write path itself: a sampled sequence becomes deduplicated grid points,
 * a gesture opens and closes exactly one transaction, an unarmed gesture calls
 * no writer, and the report line names what happened.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  automationWriteArmed,
  automationWriteReport,
  beginAutomationWrite,
  closeAutomationWrite,
  endAutomationWrite,
  isAutomationWriteActive,
  openAutomationWrite,
  sampleAutomationWrite,
  isAutomationWriting,
  pollAutomationWrites,
  resetAutomationWriteForTests,
  setAutomationWriteArmed,
  toggleAutomationWriteArm,
  writeAutomationOnce,
  writeAutomationSample,
  type AutomationWriteSurface,
} from "../stores/automationWrite";
import { resetGridDivision, setGridDivision } from "../stores/grid";
import { i18n } from "../stores/i18n";
import type { AutomationPosition } from "../stores/synthAutomation";
import {
  automationPositionAt,
  sampleAutomationPoints,
  writeGridLabel,
} from "../utils/automationSampler";

interface Recorder {
  surface: AutomationWriteSurface;
  opened: number;
  closed: number;
  points: { position: AutomationPosition; value: number }[];
  setValue(value: number): void;
}

/** A stand-in surface that records every call the writer makes. */
function recorder(label = "filter.cutoff", initial = 0): Recorder {
  let current = initial;
  const record: Recorder = {
    surface: {
      key: label,
      label,
      value: () => current,
      position: (quarter) => automationPositionAt(quarter, 4),
      open: async () => {
        record.opened += 1;
      },
      point: async (position, value) => {
        record.points.push({ position, value });
      },
      close: async () => {
        record.closed += 1;
      },
    },
    opened: 0,
    closed: 0,
    points: [],
    setValue: (value: number) => {
      current = value;
    },
  };
  return record;
}

/** Let the writer's serialised backend queue drain. */
async function settle(): Promise<void> {
  await vi.waitFor(() => {
    expect(automationWriteReport()).not.toBeNull();
  });
}

beforeEach(() => {
  i18n.setLocale("en");
  setGridDivision(4);
  resetAutomationWriteForTests();
});

afterEach(() => {
  resetAutomationWriteForTests();
  resetGridDivision();
});

describe("grid sampling", () => {
  it("keeps the last value of every grid cell and drops the rest", () => {
    const points = sampleAutomationPoints(4, [
      { position: 0, value: 0.1 },
      { position: 0.06, value: 0.2 },
      { position: 0.25, value: 0.3 },
      { position: 0.51, value: 0.4 },
      { position: 0.49, value: 0.45 },
    ]);
    expect(points).toEqual([
      { quarter: 0, value: 0.2 },
      { quarter: 0.25, value: 0.3 },
      { quarter: 0.5, value: 0.45 },
    ]);
  });

  it("records a still finger as a flat segment", () => {
    const quarters = [0, 0.25, 0.5, 0.75];
    const points = sampleAutomationPoints(
      4,
      quarters.map((position) => ({ position, value: 0.5 })),
    );
    expect(points).toHaveLength(4);
    expect(points.map((point) => point.value)).toEqual([0.5, 0.5, 0.5, 0.5]);
    expect(points.map((point) => point.quarter)).toEqual(quarters);
  });

  it("merges ticks when the grid is coarser than the poll", () => {
    // A quarter-note grid: four 1/16 ticks land in two cells.
    const points = sampleAutomationPoints(1, [
      { position: 0, value: 0.1 },
      { position: 0.25, value: 0.2 },
      { position: 0.5, value: 0.3 },
      { position: 0.75, value: 0.4 },
    ]);
    expect(points).toEqual([
      { quarter: 0, value: 0.2 },
      { quarter: 1, value: 0.4 },
    ]);
  });

  it("never proposes a finer step than a 16th, and names what it uses", () => {
    // A 1/32 grid cannot be written as `bar:beat:subdivision`, so it snaps to 1/16.
    const points = sampleAutomationPoints(8, [
      { position: 0, value: 0.1 },
      { position: 0.06, value: 0.2 },
    ]);
    expect(points).toEqual([{ quarter: 0, value: 0.2 }]);
    expect(writeGridLabel(8)).toBe("1/16");
    expect(writeGridLabel(4)).toBe("1/16");
    expect(writeGridLabel(2)).toBe("1/8");
    expect(writeGridLabel(1)).toBe("1/4");
  });
});

describe("one gesture", () => {
  it("refuses stale close and sample handles after a new same-key interaction opens", async () => {
    setAutomationWriteArmed(true);
    const first = recorder("same-parameter", 0.2);
    const second = recorder("same-parameter", 0.8);
    const a = openAutomationWrite(first.surface)!;
    sampleAutomationWrite(a, 0);
    await closeAutomationWrite(a);
    const b = openAutomationWrite(second.surface)!;
    first.setValue(0.99);
    expect(sampleAutomationWrite(a, 1)).toBe(false);
    await closeAutomationWrite(a);
    expect(isAutomationWriteActive(b)).toBe(true);
    expect(second.points).toEqual([]);
    expect(sampleAutomationWrite(b, 2)).toBe(true);
    await closeAutomationWrite(b);
    expect(first.points.map((point) => point.value)).toEqual([0.2]);
    expect(second.points.map((point) => point.value)).toEqual([0.8]);
    expect(first.closed).toBe(1);
    expect(second.closed).toBe(1);
  });

  it("awaits an already-closing writer before opening the next transaction", async () => {
    setAutomationWriteArmed(true);
    const first = recorder("first", 0.2);
    const second = recorder("second", 0.4);
    let releaseClose!: () => void;
    const gate = new Promise<void>((resolve) => { releaseClose = resolve; });
    const order: string[] = [];
    first.surface.close = async () => {
      order.push("first-close-start");
      await gate;
      order.push("first-close-end");
    };
    second.surface.open = async () => { order.push("second-open"); };
    beginAutomationWrite(first.surface);
    writeAutomationSample(0);
    const finished = endAutomationWrite(first.surface);
    await vi.waitFor(() => expect(order).toEqual(["first-close-start"]));
    // Even after the public active session clears, this is the same completion.
    let settled = false;
    void endAutomationWrite().then(() => { settled = true; });
    beginAutomationWrite(second.surface);
    writeAutomationSample(1);
    const secondFinished = endAutomationWrite(second.surface);
    await Promise.resolve();
    expect(settled).toBe(false);
    expect(order).toEqual(["first-close-start"]);
    releaseClose();
    await finished;
    await secondFinished;
    expect(order).toEqual(["first-close-start", "first-close-end", "second-open"]);
  });

  it("opens and closes the surface exactly once and reports the points", async () => {
    const source = recorder("filter.cutoff");
    setAutomationWriteArmed(true);
    source.setValue(0.2);
    expect(beginAutomationWrite(source.surface)).toBe(true);
    for (const value of [0.2, 0.4, 0.6]) {
      source.setValue(value);
      writeAutomationSample(value === 0.2 ? 0 : value === 0.4 ? 0.25 : 0.5);
    }
    endAutomationWrite(source.surface);
    await settle();

    expect(source.opened).toBe(1);
    expect(source.closed).toBe(1);
    expect(source.points.map((point) => point.value)).toEqual([0.2, 0.4, 0.6]);
    expect(source.points[1].position).toEqual({ bar: 1, beat: 1, subdivision: 1 });
    expect(automationWriteReport()).toBe("Recorded 3 points on filter.cutoff (1/16 grid).");
  });

  it("writes nothing and records nothing when the drag produced no point", async () => {
    const source = recorder();
    setAutomationWriteArmed(true);
    beginAutomationWrite(source.surface);
    endAutomationWrite(source.surface);
    await Promise.resolve();

    expect(source.opened).toBe(0);
    expect(source.points).toHaveLength(0);
    expect(source.closed).toBe(0);
    expect(automationWriteReport()).toBeNull();
  });

  it("samples the same cell again only when the value moved", async () => {
    const source = recorder();
    setAutomationWriteArmed(true);
    beginAutomationWrite(source.surface);
    source.setValue(0.2);
    writeAutomationSample(0);
    writeAutomationSample(0.05);
    source.setValue(0.3);
    writeAutomationSample(0.1);
    endAutomationWrite(source.surface);
    await settle();

    // One cell, two distinct values: the second sample rewrote it.
    expect(source.points).toHaveLength(2);
    expect(source.points[1].value).toBe(0.3);
    expect(automationWriteReport()).toBe("Recorded 1 point at 1:1:0.");
  });

  it("writes one point at the playhead from a single press", async () => {
    const source = recorder();
    setAutomationWriteArmed(true);
    writeAutomationOnce(source.surface, 0.75, 2.5);
    await settle();

    expect(source.opened).toBe(1);
    expect(source.points).toHaveLength(1);
    expect(source.points[0].position).toEqual({ bar: 1, beat: 3, subdivision: 2 });
    expect(source.closed).toBe(1);
    expect(automationWriteReport()).toBe("Recorded 1 point at 1:3:2.");
  });

  it("does not let a stale surface close a newer gesture", async () => {
    const first = recorder("filter.cutoff");
    const second = recorder("filter.resonance");
    setAutomationWriteArmed(true);
    beginAutomationWrite(first.surface);
    writeAutomationSample(0);
    beginAutomationWrite(second.surface);
    writeAutomationSample(1);
    expect(isAutomationWriting(second.surface)).toBe(true);
    endAutomationWrite(first.surface);
    expect(isAutomationWriting(second.surface)).toBe(true);
    endAutomationWrite(second.surface);
    await vi.waitFor(() => expect(second.closed).toBe(1));
    expect(first.closed).toBe(1);
    expect(first.points).toHaveLength(1);
    expect(second.points).toHaveLength(1);
  });
});

describe("the arm", () => {
  it("is one signal: toggling it flips what every surface reads", () => {
    expect(automationWriteArmed()).toBe(false);
    toggleAutomationWriteArm();
    expect(automationWriteArmed()).toBe(true);
    setAutomationWriteArmed(false);
    expect(automationWriteArmed()).toBe(false);
  });

  it("refuses a write while off, and says so", () => {
    const source = recorder();
    expect(beginAutomationWrite(source.surface)).toBe(false);
    writeAutomationSample(0);
    endAutomationWrite(source.surface);

    expect(source.opened).toBe(0);
    expect(source.points).toHaveLength(0);
    expect(automationWriteReport()).toBe("Nothing was recorded: automation write is off.");
  });

  it("closes an open gesture when the arm is switched off", async () => {
    const source = recorder();
    setAutomationWriteArmed(true);
    beginAutomationWrite(source.surface);
    writeAutomationSample(0);
    setAutomationWriteArmed(false);
    await vi.waitFor(() => expect(source.closed).toBe(1));
    // The point was recorded before the arm went off.
    expect(source.points).toHaveLength(1);
    expect(automationWriteReport()).toBe("Recorded 1 point at 1:1:0.");
  });
});

describe("the transport poll", () => {
  it("samples only while the transport rolls", async () => {
    const source = recorder();
    setAutomationWriteArmed(true);
    beginAutomationWrite(source.surface);
    source.setValue(0.5);
    pollAutomationWrites(0, false);
    pollAutomationWrites(0.25, true);
    pollAutomationWrites(0.5, true);
    endAutomationWrite(source.surface);
    await settle();

    expect(source.points.map((point) => point.value)).toEqual([0.5, 0.5]);
    expect(automationWriteReport()).toBe("Recorded 2 points on filter.cutoff (1/16 grid).");
  });
});

describe("owned parameter surface admission and publication", () => {
  it("closes its exact late begin token without dispatching a point to a replacement source", async () => {
    let current = true;
    let release!: (token: import("../stores/api").UndoGestureToken) => void;
    const begin = new Promise<import("../stores/api").UndoGestureToken>(resolve => { release = resolve; });
    const point = vi.fn();
    const end = vi.fn(async () => ({ closed: false, snapshot: {} as import("../types/project").ProjectSnapshot }));
    const apply = vi.fn();
    const surface = ownedParameterSurface({ key: "effect:track:0:0:mix", label: "mix", value: () => 0.5,
      position: () => ({ bar: 1, beat: 1, subdivision: 0 }), isCurrent: () => current,
      expectedStamp: "fixture-parameter-target" as import("../types/project").ParameterTargetStamp,
      begin: () => begin, point, end, apply });
    const opening = surface.open();
    current = false;
    release("late-owner" as import("../stores/api").UndoGestureToken);
    await expect(opening).rejects.toThrow();
    await expect(surface.point({ bar: 1, beat: 1, subdivision: 0 }, 0.7)).rejects.toThrow();
    await surface.close();
    await surface.close();
    expect(point).not.toHaveBeenCalled();
    expect(end).toHaveBeenCalledTimes(1);
    expect(end).toHaveBeenCalledWith("late-owner");
    expect(apply).not.toHaveBeenCalled();
  });

  it("ignores a delayed close snapshot after the document changes while ending", async () => {
    let current = true;
    let finish!: (reply: import("../stores/api").OwnedParameterGestureEnd) => void;
    const ending = new Promise<import("../stores/api").OwnedParameterGestureEnd>(resolve => { finish = resolve; });
    const apply = vi.fn();
    const surface = ownedParameterSurface({ key: "plugin\0Pad\0Gain", label: "Gain", value: () => 0.5,
      position: () => ({ bar: 1, beat: 1, subdivision: 0 }), isCurrent: () => current,
      expectedStamp: "fixture-parameter-target" as import("../types/project").ParameterTargetStamp,
      begin: async () => "current-owner" as import("../stores/api").UndoGestureToken,
      point: vi.fn(), end: () => ending, apply });
    await surface.open();
    const closed = surface.close();
    current = false;
    finish({ closed: true, snapshot: {} as import("../types/project").ProjectSnapshot });
    await closed;
    expect(apply).not.toHaveBeenCalled();
  });

  it("retains independent same-key tokens and applies only an acknowledged current close", async () => {
    const ends: string[] = [];
    const applies = vi.fn();
    let next = 0;
    const make = () => ownedParameterSurface({ key: "same-key", label: "Gain", value: () => 0.5,
      position: () => ({ bar: 1, beat: 1, subdivision: 0 }), isCurrent: () => true,
      expectedStamp: "fixture-parameter-target" as import("../types/project").ParameterTargetStamp,
      begin: async () => `owner-${++next}` as import("../stores/api").UndoGestureToken,
      point: vi.fn(), end: async token => { ends.push(token); return { closed: token === "owner-2", snapshot: {} as import("../types/project").ProjectSnapshot }; }, apply: applies });
    const first = make(); const second = make();
    await first.open(); await second.open();
    await first.close(); await first.close(); await second.close();
    expect(ends).toEqual(["owner-1", "owner-2"]);
    expect(applies).toHaveBeenCalledTimes(1);
  });
});


it("retains captured target admission and never points or closes after begin refusal", async () => {
  const original = "before-topology-change" as import("../types/project").ParameterTargetStamp;
  const begin = vi.fn(async (_stamp: import("../types/project").ParameterTargetStamp) => {
    throw new Error("Target changed before admission");
  });
  const point = vi.fn(); const end = vi.fn();
  const surface = ownedParameterSurface({
    key: "effect:track:0:0:factor", label: "factor", value: () => 0.2,
    position: () => ({ bar: 1, beat: 1, subdivision: 0 }), isCurrent: () => true,
    expectedStamp: original, begin, point, end, apply: vi.fn(),
  });
  await expect(surface.open()).rejects.toThrow("Target changed before admission");
  await expect(surface.point({ bar: 1, beat: 1, subdivision: 0 }, 0.2)).rejects.toThrow();
  await surface.close();
  expect(begin).toHaveBeenCalledWith(original);
  expect(point).not.toHaveBeenCalled(); expect(end).not.toHaveBeenCalled();
});

it("fails closed without an opaque stamp instead of admitting through a legacy begin", async () => {
  const begin = vi.fn();
  const surface = ownedParameterSurface({
    key: "missing-stamp", label: "factor", value: () => 0.2,
    position: () => ({ bar: 1, beat: 1, subdivision: 0 }), isCurrent: () => true,
    expectedStamp: null, begin, point: vi.fn(), end: vi.fn(), apply: vi.fn(),
  });
  await expect(surface.open()).rejects.toThrow();
  expect(begin).not.toHaveBeenCalled();
});
