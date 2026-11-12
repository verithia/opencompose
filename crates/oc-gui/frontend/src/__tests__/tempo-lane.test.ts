/**
 * Tempo/meter lane editing contract: snapped geometry, map validation, store
 * persistence and undo. The engine's checked clock is the reference.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { invokeMock, makeSnapshot, expectInvoked, getLastCallArgs } from "./setup";
import { store } from "../stores/project";
import type { ProjectProperties, TempoPoint, TimeSigPoint } from "../types/project";
import {
  barMarks,
  formatMeter,
  formatTempo,
  insertTempoPoint,
  insertTimeSigPoint,
  isValidMeter,
  isValidTempo,
  meterAtQuarter,
  moveTempoPoint,
  moveTimeSigPoint,
  removeTempoPoint,
  removeTimeSigPoint,
  setTempoPointBpm,
  setTimeSigPointValue,
  tempoAtQuarter,
} from "../utils/tempoLane";

const properties = (overrides: Partial<ProjectProperties> = {}): ProjectProperties => ({
  tempo: 120,
  time_signature: { numerator: 4, denominator: 4 },
  key: null,
  swing: null,
  tempo_map: [],
  time_sig_map: [],
  key_map: [],
  ...overrides,
});

const tempo = (bar: number, beat: number, bpm: number): TempoPoint => ({
  position: { bar, beat, subdivision: 0 },
  offset_quarters: 0,
  bpm,
});

const meter = (bar: number, numerator: number, denominator: number): TimeSigPoint => ({
  position: { bar, beat: 1, subdivision: 0 },
  numerator,
  denominator,
});

const baseSnapshot = (overrides: Partial<ProjectProperties> = {}) =>
  makeSnapshot({ properties: properties(overrides) });

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "get_undo_state") {
      return {
        can_undo: false, can_redo: false, undo_label: null, redo_label: null,
        undo_labels: [], redo_labels: [],
      };
    }
    return baseSnapshot();
  });
  await store.initProject();
});

describe("tempo lane geometry", () => {
  it("relabels bar starts from the meter map the way the engine counts", () => {
    const p = properties({
      time_signature: { numerator: 7, denominator: 8 },
      time_sig_map: [meter(3, 3, 4)],
    });
    const marks = barMarks(p, 6);
    expect(marks.map(({ bar, quarter }) => [bar, quarter])).toEqual([
      [1, 0],
      [2, 3.5],
      [3, 7],
      [4, 10],
      [5, 13],
      [6, 16],
    ]);
  });

  it("reads tempo and meter at a quarter through the maps", () => {
    const p = properties({
      time_signature: { numerator: 7, denominator: 8 },
      tempo: 120,
      tempo_map: [tempo(1, 3, 60)],
      time_sig_map: [meter(3, 3, 4)],
    });
    expect(tempoAtQuarter(p, 0.999)).toBe(120);
    expect(tempoAtQuarter(p, 1)).toBe(60);
    expect(tempoAtQuarter(p, 100)).toBe(60);
    expect(meterAtQuarter(p, 0)).toEqual({ numerator: 7, denominator: 8 });
    expect(meterAtQuarter(p, 8)).toEqual({ numerator: 3, denominator: 4 });
  });

  it("formats point labels without trailing zeros", () => {
    expect(formatTempo(128)).toBe("128");
    expect(formatTempo(128.5)).toBe("128.5");
    expect(formatTempo(128.05)).toBe("128.05");
    expect(formatMeter(3, 4)).toBe("3/4");
    expect(isValidTempo(20)).toBe(true);
    expect(isValidTempo(400)).toBe(true);
    expect(isValidTempo(19.9)).toBe(false);
    expect(isValidTempo(401)).toBe(false);
    expect(isValidMeter(32, 32)).toBe(true);
    expect(isValidMeter(0, 4)).toBe(false);
    expect(isValidMeter(4, 64)).toBe(false);
  });
});

describe("tempo point edits", () => {
  it("adds on the snap grid and rejects duplicates and out-of-range values", () => {
    const p = properties({ tempo_map: [tempo(1, 1, 120)] });
    const added = insertTempoPoint(p, 4.12, 140, 4);
    expect(added.ok).toBe(true);
    if (!added.ok) return;
    expect(added.points).toHaveLength(2);
    expect(added.points[1]).toEqual(tempo(2, 1, 140));
    expect(added.index).toBe(1);

    // A second point at the same quarter is ambiguous, not merged.
    expect(insertTempoPoint(p, 0, 90, 4)).toEqual({ ok: false, error: "duplicatePosition" });
    expect(insertTempoPoint(p, 4, 10, 4)).toEqual({ ok: false, error: "invalidTempo" });
    expect(insertTempoPoint(p, 4, 401, 4)).toEqual({ ok: false, error: "invalidTempo" });
  });

  it("drags with snap, reorders and keeps the value", () => {
    const p = properties({ tempo_map: [tempo(1, 1, 120), tempo(2, 1, 140)] });
    const moved = moveTempoPoint(p, 0, 6.1, 4);
    expect(moved.ok).toBe(true);
    if (!moved.ok) return;
    expect(moved.points.map((point) => point.bpm)).toEqual([140, 120]);
    expect(moved.points[1].position).toEqual({ bar: 2, beat: 3, subdivision: 0 });
    expect(moved.index).toBe(1);

    expect(moveTempoPoint(p, 0, 4.02, 4)).toEqual({ ok: false, error: "duplicatePosition" });
    expect(moveTempoPoint(p, 5, 4, 4)).toEqual({ ok: false, error: "outOfRange" });
  });

  it("edits the value and removes a point", () => {
    const p = properties({ tempo_map: [tempo(1, 1, 120), tempo(2, 1, 140)] });
    const value = setTempoPointBpm(p, 1, 151.5);
    expect(value.ok).toBe(true);
    if (!value.ok) return;
    expect(value.points[1].bpm).toBe(151.5);
    expect(setTempoPointBpm(p, 1, 999)).toEqual({ ok: false, error: "invalidTempo" });

    const removed = removeTempoPoint(p, 0);
    expect(removed).toEqual({ ok: true, points: [tempo(2, 1, 140)], index: 0 });
    const last = removeTempoPoint(properties({ tempo_map: [tempo(1, 1, 120)] }), 0);
    expect(last).toEqual({ ok: true, points: [], index: -1 });
    expect(removeTempoPoint(p, 4)).toEqual({ ok: false, error: "outOfRange" });
  });
});

describe("meter point edits", () => {
  it("adds a bar-start change and rejects duplicates and invalid meters", () => {
    const p = properties();
    const added = insertTimeSigPoint(p, 3, 3, 4);
    expect(added.ok).toBe(true);
    if (!added.ok) return;
    expect(added.points).toEqual([meter(3, 3, 4)]);
    const withPoint = properties({ time_sig_map: [meter(3, 3, 4)] });
    expect(insertTimeSigPoint(withPoint, 3, 7, 8)).toEqual({ ok: false, error: "duplicatePosition" });
    expect(insertTimeSigPoint(p, 0, 4, 4)).toEqual({ ok: false, error: "invalidPosition" });
    expect(insertTimeSigPoint(p, 3, 0, 4)).toEqual({ ok: false, error: "invalidMeter" });
    expect(insertTimeSigPoint(p, 3, 4, 5)).toEqual({ ok: false, error: "invalidMeter" });
  });

  it("drags a change to another bar and keeps its value", () => {
    const p = properties({ time_sig_map: [meter(3, 3, 4), meter(7, 5, 8)] });
    // 3/4 from bar 3: bar 4 starts at 11, bar 5 at 14, bar 7 at 20.
    const moved = moveTimeSigPoint(p, 0, 14);
    expect(moved.ok).toBe(true);
    if (!moved.ok) return;
    expect(moved.points.map((point) => point.position.bar)).toEqual([5, 7]);
    expect(moved.points[0]).toEqual(meter(5, 3, 4));
    expect(moved.index).toBe(0);
    expect(moveTimeSigPoint(p, 0, 20)).toEqual({ ok: false, error: "duplicatePosition" });
  });

  it("edits the meter, removes it, and validates the value", () => {
    const p = properties({ time_sig_map: [meter(3, 3, 4)] });
    const value = setTimeSigPointValue(p, 0, 7, 8);
    expect(value.ok).toBe(true);
    if (!value.ok) return;
    expect(value.points[0]).toEqual(meter(3, 7, 8));
    expect(setTimeSigPointValue(p, 0, 33, 4)).toEqual({ ok: false, error: "invalidMeter" });
    expect(setTimeSigPointValue(p, 0, 4, 64)).toEqual({ ok: false, error: "invalidMeter" });
    expect(setTimeSigPointValue(p, 9, 4, 4)).toEqual({ ok: false, error: "outOfRange" });
    expect(removeTimeSigPoint(p, 0)).toEqual({ ok: true, points: [], index: -1 });
  });
});

describe("tempo and meter persistence", () => {
  it("saves one map snapshot per operation and marks the project dirty once", async () => {
    const point = tempo(2, 1, 140);
    const saved = baseSnapshot({ tempo_map: [point] });
    saved.dirty = true;
    invokeMock.mockResolvedValueOnce(saved);

    const ok = await store.setTempoMap([point]);
    expect(ok).toBe(true);
    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_tempo_map");
    expect(calls).toHaveLength(1);
    expect(getLastCallArgs("set_tempo_map")).toEqual({ points: [point] });
    expect(store.project.properties.tempo_map).toEqual([point]);
    expect(store.dirty()).toBe(true);
  });

  it("undoes a tempo map edit through the existing undo command", async () => {
    const point = tempo(2, 1, 140);
    const saved = baseSnapshot({ tempo_map: [point] });
    saved.dirty = true;
    invokeMock.mockResolvedValueOnce(saved);
    await store.setTempoMap([point]);

    invokeMock.mockResolvedValueOnce(baseSnapshot()); // undo restores the empty map
    await store.undo();
    expectInvoked("undo");
    expect(store.project.properties.tempo_map).toEqual([]);
  });

  it("persists a meter map and survives a snapshot round-trip", async () => {
    const point = meter(3, 3, 4);
    const saved = baseSnapshot({ time_sig_map: [point] });
    saved.dirty = true;
    invokeMock.mockResolvedValueOnce(saved);
    const ok = await store.setTimeSigMap([point]);
    expect(ok).toBe(true);
    expect(getLastCallArgs("set_time_sig_map")).toEqual({ points: [point] });
    expect(store.project.properties.time_sig_map).toEqual([point]);

    // A reload applies the backend's parsed maps back into the store.
    invokeMock.mockResolvedValueOnce(baseSnapshot({ tempo_map: [tempo(5, 1, 90)], time_sig_map: [point] }));
    await store.initProject();
    expect(store.project.properties.tempo_map).toEqual([tempo(5, 1, 90)]);
    expect(store.project.properties.time_sig_map).toEqual([point]);
  });

  it("surfaces a rejected map and leaves the project untouched", async () => {
    invokeMock.mockRejectedValueOnce(new Error("Invalid tempo map: tempo map must have unique ascending positions and valid tempos"));
    const ok = await store.setTempoMap([tempo(2, 1, 140)]);
    expect(ok).toBe(false);
    expect(store.error()).toContain("Invalid tempo map");
    expect(store.project.properties.tempo_map).toEqual([]);
  });

  it("clears the map to return to the project BPM", async () => {
    const point = tempo(2, 1, 140);
    invokeMock.mockResolvedValueOnce(baseSnapshot({ tempo_map: [point] }));
    await store.setTempoMap([point]);
    invokeMock.mockResolvedValueOnce(baseSnapshot());
    await store.setTempoMap([]);
    expect(getLastCallArgs("set_tempo_map")).toEqual({ points: [] });
    expect(store.project.properties.tempo_map).toEqual([]);
    expect(store.project.properties.tempo).toBe(120);
  });
});
