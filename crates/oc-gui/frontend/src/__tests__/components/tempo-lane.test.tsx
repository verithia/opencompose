/**
 * Timeline tempo/meter lane: rendering, folding, point add and key delete.
 * jsdom has no layout, so the row rect is stubbed for pointer geometry.
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { render, fireEvent, cleanup, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, getLastCallArgs } from "../setup";
import { store } from "../../stores/project";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { resetGridDivision } from "../../stores/grid";
import { i18n } from "../../stores/i18n";
import type { ProjectProperties, TempoPoint, TimeSigPoint } from "../../types/project";

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

const withMaps = () => makeSnapshot({
  properties: properties({
    tempo_map: [tempo(1, 1, 120), tempo(3, 1, 140)],
    time_sig_map: [meter(3, 3, 4)],
  }),
});

const rowRect = (row: HTMLElement) => {
  row.getBoundingClientRect = () => ({
    left: 0, top: 0, right: 1200, bottom: 17, width: 1200, height: 17,
    x: 0, y: 0, toJSON: () => ({}),
  });
};

beforeEach(async () => {
  i18n.setLocale("en");
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
    return withMaps();
  });
  await store.initProject();
});

afterEach(() => {
  cleanup();
  resetGridDivision();
});

describe("tempo and meter lane", () => {
  it("opens with maps and shows point values plus the playhead readout", () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const lane = container.querySelector("#timeline-tempo-lane") as HTMLElement;
    expect(lane).toBeTruthy();
    const tempos = [...container.querySelectorAll(".tempo-point-bpm")].map((node) => node.textContent);
    expect(tempos).toEqual(["120", "140"]);
    const meters = [...container.querySelectorAll(".tempo-point-meter")].map((node) => node.textContent);
    expect(meters).toEqual(["3/4"]);
    // No bar-1 map point, so the project meter is shown as the base chip.
    const base = container.querySelectorAll(".tempo-point-base");
    expect(base).toHaveLength(1);
    expect(base[0].textContent).toBe("4/4");

    const header = container.querySelector(".tempo-lane-head-tempo") as HTMLElement;
    expect(header.textContent).toContain("120");
    // Playhead is at 0 and bar 3 (meter change) starts at quarter 8.
    const meterHead = container.querySelector(".tempo-lane-head-meter") as HTMLElement;
    expect(meterHead.textContent).toContain("4/4");
  });

  it("folds away without leaving a spacer row and reopens from the ruler toggle", () => {
    const { container } = render(() => <Timeline />);
    const toggle = container.querySelector(".tempo-lane-toggle") as HTMLButtonElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(toggle);
    expect(container.querySelector("#timeline-tempo-lane")).toBeNull();
    expect(container.querySelector(".tempo-lane-headers")).toBeNull();
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(container.querySelector("#timeline-tempo-lane")).toBeTruthy();
    expect(container.querySelector(".tempo-lane-headers")).toBeTruthy();
  });

  it("adds a tempo point at the double-clicked bar with the effective tempo", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const row = container.querySelector(".tempo-lane-tempo") as HTMLElement;
    rowRect(row);
    const saved = makeSnapshot({
      properties: properties({
        tempo_map: [tempo(1, 1, 120), tempo(2, 1, 120), tempo(3, 1, 140)],
        time_sig_map: [meter(3, 3, 4)],
      }),
    });
    saved.dirty = true;
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
      if (cmd === "get_undo_history") return [];
      if (cmd === "set_tempo_map") return saved;
      return withMaps();
    });
    fireEvent.dblClick(row, { clientX: 96 }); // 96 px / 24 px-per-beat = bar 2
    await waitFor(() => expect(getLastCallArgs("set_tempo_map")).toBeTruthy());
    const points = (getLastCallArgs("set_tempo_map") as { points: TempoPoint[] }).points;
    expect(points.map((point) => point.position.bar)).toEqual([1, 2, 3]);
    expect(points[1].bpm).toBe(120);
    expect(store.project.properties.tempo_map.map((point) => point.position.bar)).toEqual([1, 2, 3]);
  });

  it("drops a point drag the window lost focus on, without writing a map", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const row = container.querySelector(".tempo-lane-tempo") as HTMLElement;
    rowRect(row);
    const point = container.querySelector(".tempo-point-bpm") as HTMLElement;

    fireEvent.pointerDown(point, { button: 0, pointerId: 1, clientX: 0 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 96 });
    // Cmd+Tab mid-drag: no release arrives, so the next one must not commit.
    fireEvent.blur(window);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 96 });

    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_tempo_map").length).toBe(0);
  });

  it("deletes a focused point with Delete as one map command", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const point = container.querySelector(".tempo-point-bpm") as HTMLButtonElement;
    point.focus();
    const saved = makeSnapshot({
      properties: properties({
        tempo_map: [tempo(3, 1, 140)],
        time_sig_map: [meter(3, 3, 4)],
      }),
    });
    saved.dirty = true;
    invokeMock.mockResolvedValueOnce(saved);
    fireEvent.keyDown(point, { key: "Delete" });
    await waitFor(() => expect(getLastCallArgs("set_tempo_map")).toBeTruthy());
    expect((getLastCallArgs("set_tempo_map") as { points: TempoPoint[] }).points)
      .toEqual([tempo(3, 1, 140)]);
  });

  it("relabels ruler bars from the meter map", () => {
    const { container } = render(() => <Timeline />);
    const marks = [...container.querySelectorAll<HTMLElement>(".timeline-ruler-mark")];
    expect(marks[1].style.left).toBe("96px"); // bar 2 = 4 quarters
    expect(marks[2].style.left).toBe("192px"); // bar 3 = 8 quarters
    expect(marks[3].textContent).toBe("4");
    expect(marks[3].style.left).toBe("264px"); // 3/4 at bar 3 ends bar 4 at quarter 11
  });

  it("selects a meter point and edits its value through the inline editor", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const meterPoint = container.querySelector(".tempo-point-meter") as HTMLButtonElement;
    fireEvent.dblClick(meterPoint);
    const numerator = container.querySelector(".meter-numerator-input") as HTMLInputElement;
    expect(numerator).toBeTruthy();
    fireEvent.input(numerator, { target: { value: "5" } });
    const saved = makeSnapshot({
      properties: properties({
        tempo_map: [tempo(1, 1, 120), tempo(3, 1, 140)],
        time_sig_map: [meter(3, 5, 4)],
      }),
    });
    saved.dirty = true;
    invokeMock.mockResolvedValueOnce(saved);
    fireEvent.keyDown(numerator, { key: "Enter" });
    await waitFor(() => expect(getLastCallArgs("set_time_sig_map")).toBeTruthy());
    expect((getLastCallArgs("set_time_sig_map") as { points: TimeSigPoint[] }).points)
      .toEqual([meter(3, 5, 4)]);
  });
});
