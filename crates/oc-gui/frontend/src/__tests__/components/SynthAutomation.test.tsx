import { openSynthRange } from "../helpers/synthRange";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { specForConcreteKey } from "../../data/synthCatalog";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import {
  automationWriteArmed,
  automationWriteReport,
  resetAutomationWriteForTests,
  setAutomationWriteArmed,
  toggleAutomationWriteArm,
} from "../../stores/automationWrite";
import type { AutomationLane, PresetDef, Project } from "../../types/project";

const PRESET: PresetDef = {
  name: "Lead",
  source: "builtin",
  plugin: "builtin.open_synth",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  params: [],
  effects: [],
};

const CUTOFF_LANE = "pluginparam_66696c7465722e6375746f6666";
const TRANSPORT = { playing: false, position_beats: 2, position_seconds: 1, duration_seconds: 30 };

let project: Project;
let history: Project[];
let pendingOld: Project | null;
let updates: Record<string, unknown>[];

function laneParam(key: string): string {
  return `pluginparam_${Array.from(new TextEncoder().encode(key))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")}`;
}

function upsertPoint(key: string, position: { bar: number; beat: number; subdivision: number }, normalized: number) {
  const laneParamName = laneParam(key);
  const lane: AutomationLane = project.tracks[0].automation.find((entry) => entry.param === laneParamName)
    ?? { param: laneParamName, points: [] };
  if (!project.tracks[0].automation.includes(lane)) project.tracks[0].automation.push(lane);
  const point = { position, value: { Number: normalized } };
  const index = lane.points.findIndex((entry) => (
    entry.position.bar === position.bar
    && entry.position.beat === position.beat
    && entry.position.subdivision === position.subdivision
  ));
  if (index >= 0) lane.points[index] = point;
  else lane.points.push(point);
}

/** Minimal stand-in for the backend's gesture transaction and undo stack. */
function installBackend() {
  invokeMock.mockReset();
  project = makeSnapshot({
    presets: [PRESET],
    tracks: [{ ...makeTrack("Synth", 0), preset: "Lead" }],
  }).project;
  history = [];
  pendingOld = null;
  updates = [];
  let nextOwner = 0;
  const owners = new Map<string, { preset: string; key: string }>();
  const owns = (args: Record<string, unknown> | undefined) => {
    const owner = typeof args?.token === "string" ? owners.get(args.token) : undefined;
    return owner && owner.preset === args?.presetName && owner.key === args?.paramKey;
  };
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "get_transport_state":
      case "transport_seek":
        return TRANSPORT;
      case "begin_guarded_synth_param_gesture": {
        pendingOld = structuredClone(project);
        const token = `synth-test-owner-${++nextOwner}`;
        owners.set(token, { preset: String(args?.presetName), key: String(args?.paramKey) });
        return token;
      }
      case "update_owned_synth_param_gesture": {
        if (!owns(args)) throw new Error("Stale synth fixture owner");
        updates.push(args ?? {});
        upsertPoint(String(args?.paramKey), args?.position as { bar: number; beat: number; subdivision: number }, Number(args?.normalized));
        return makeSnapshot(structuredClone(project));
      }
      case "end_owned_synth_param_gesture": {
        if (!owns(args)) return { closed: false, snapshot: makeSnapshot(structuredClone(project)) };
        owners.delete(String(args?.token));
        if (pendingOld) history.push(pendingOld);
        pendingOld = null;
        return { closed: true, snapshot: makeSnapshot(structuredClone(project)) };
      }
      case "begin_synth_param_gesture":
        pendingOld = structuredClone(project);
        return undefined;
      case "update_synth_param_gesture":
        updates.push(args ?? {});
        upsertPoint(
          String(args?.paramKey),
          args?.position as { bar: number; beat: number; subdivision: number },
          Number(args?.normalized),
        );
        return makeSnapshot(structuredClone(project));
      case "end_synth_param_gesture":
        if (pendingOld) history.push(pendingOld);
        pendingOld = null;
        return makeSnapshot(structuredClone(project));
      case "remove_synth_param_automation": {
        history.push(structuredClone(project));
        const lane = laneParam(String(args?.paramKey));
        project.tracks[0].automation = project.tracks[0].automation.filter((entry) => entry.param !== lane);
        return makeSnapshot(structuredClone(project));
      }
      case "undo": {
        const restored = history.pop();
        if (restored) project = restored;
        return makeSnapshot(structuredClone(project));
      }
      case "get_undo_state":
        return {
          can_undo: history.length > 0,
          can_redo: false,
          undo_label: null,
          redo_label: null,
          undo_labels: [],
          redo_labels: [],
        };
      default:
        return makeSnapshot(structuredClone(project));
    }
  });
}

function renderEditor(preset: PresetDef = PRESET) {
  return render(() => (
    <>
      <OpenSynthEditor
        preset={preset}
        setParam={async () => null}
        setParams={async () => null}
        macroLearn={async () => ({ error: null, result: null })}
        loadPatch={async () => null}
        noteOn={async () => {}}
        noteOff={async () => {}}
        auditionPatch={async () => {}}
        stopAudition={async () => {}}
        auditioning={() => null}
        auditionStatus={() => null}
        libraryPatches={() => []}
        libraryWarnings={() => []}
        refreshLibrary={async () => null}
        patchActions={{
          rename: async () => null,
          duplicate: async () => null,
          saveAs: async () => null,
          saveToLibrary: async () => null,
          randomize: async () => null,
        }}
      />
      <ContextMenuLayer />
    </>
  ));
}

function control(key: string): HTMLElement {
  const element = document.querySelector<HTMLElement>(`[data-param="${key}"]`);
  if (!element) throw new Error(`no control for ${key}`);
  return element;
}

function openFilterPage() {
  const tab = Array.from(document.querySelectorAll(".synth-subtabs button"))
    .find((button) => button.textContent === "FILTER");
  if (!tab) throw new Error("FILTER subtab");
  fireEvent.click(tab);
}

function menuButton(label: string): HTMLElement {
  const button = Array.from(document.querySelectorAll<HTMLElement>(".context-menu-item"))
    .find((item) => item.textContent?.includes(label));
  if (!button) throw new Error(`no menu item ${label}`);
  return button;
}

beforeEach(async () => {
  resetAutomationWriteForTests();
  installBackend();
  await store.initProject();
  await store.seek(2);
});

afterEach(cleanup);

describe("synth parameter automation", () => {
  it("refuses an unreachable physical LFO zero instead of writing a different sounding value", async () => {
    const preset = structuredClone(PRESET);
    preset.params = [["lfo.rate", { Number: 0 }]];
    renderEditor(preset);
    const tab = document.querySelector('[aria-label="Synthesizer views"] button:nth-child(2)');
    expect(tab).not.toBeNull();
    fireEvent.click(tab!);
    fireEvent.contextMenu(control("lfo.rate"));
    fireEvent.click(menuButton("Automate"));
    await waitFor(() => expect(document.body.textContent).toContain("Use a static value outside this range."));
    expect(updates).toHaveLength(0);
    expect(history).toHaveLength(0);
    expect(project.tracks[0].automation).toHaveLength(0);
  });

  it("automates a knob from the context menu and removes the lane again", async () => {
    renderEditor();
    openFilterPage();
    fireEvent.contextMenu(control("filter.cutoff"));

    await waitFor(() => expect(document.querySelector(".context-menu-heading")?.textContent).toBe("Filter Cutoff"));
    const automate = menuButton("Automate");
    expect(automate.getAttribute("aria-disabled")).not.toBe("true");
    expect(document.querySelectorAll(".context-menu-item")).toHaveLength(1);

    fireEvent.click(automate);
    await waitFor(() => expect(project.tracks[0].automation).toHaveLength(1));
    expect(project.tracks[0].automation[0].param).toBe(CUTOFF_LANE);
    expect(project.tracks[0].automation[0].points).toHaveLength(1);
    expect(updates).toHaveLength(1);
    expect(updates[0].paramKey).toBe("filter.cutoff");
    expect(updates[0].position).toEqual({ bar: 1, beat: 3, subdivision: 0 });
    // 12000 Hz on the engine's 20 Hz..20 kHz log domain.
    expect(Number(updates[0].normalized)).toBeCloseTo(Math.log10(600) / 3, 6);
    expect(history).toHaveLength(1);

    // The lane target shows as its human label in the header.
    await waitFor(() => expect(document.querySelector(".synth-automation-target")?.textContent).toBe("Filter Cutoff"));

    fireEvent.contextMenu(control("filter.cutoff"));
    await waitFor(() => expect(menuButton("Remove automation")).toBeTruthy());
    expect(document.querySelector(".context-menu-heading")?.textContent).toBe("Filter Cutoff");
    fireEvent.click(menuButton("Remove automation"));
    await waitFor(() => expect(project.tracks[0].automation).toHaveLength(0));
    expect(history).toHaveLength(2);
  });

  it("automates the widened filter model control", async () => {
    renderEditor();
    openFilterPage();
    fireEvent.contextMenu(control("filter.model"));

    await waitFor(() => expect(document.querySelector(".context-menu-heading")?.textContent).toBe("Filter Model"));
    const automate = menuButton("Automate");
    expect(automate.getAttribute("aria-disabled")).not.toBe("true");
    expect(automate.textContent).not.toContain("Automation covers");
    fireEvent.click(automate);
    await waitFor(() => expect(project.tracks[0].automation).toHaveLength(1));
    expect(project.tracks[0].automation[0].param).toBe(laneParam("filter.model"));
    expect(updates).toHaveLength(1);
    expect(Number(updates[0].normalized)).toBe(0);
    expect(history).toHaveLength(1);
  });

  it("keeps a newer same-key range write open when an old captured release arrives", async () => {
    renderEditor();
    openFilterPage();
    setAutomationWriteArmed(true);
    const listeners = vi.spyOn(window, "addEventListener");
    const input = openSynthRange(control("filter.cutoff"));
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.6" } });
    const oldRelease = listeners.mock.calls.filter(([type]) => type === "pointerup").at(-1)![1] as EventListener;
    fireEvent.pointerUp(input);
    await waitFor(() => expect(history).toHaveLength(1));
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.8" } });
    oldRelease(new Event("pointerup"));
    fireEvent.change(input, { target: { value: "0.1" } });
    await Promise.resolve();
    expect(history).toHaveLength(1);
    fireEvent.pointerUp(input);
    await waitFor(() => expect(history).toHaveLength(2));
    expect(Number(updates.at(-1)!.normalized)).toBeCloseTo(0.8, 4);
    listeners.mockRestore();
  });

  it("writes double-click reset through automation rather than an ordinary preset edit", async () => {
    renderEditor();
    openFilterPage();
    setAutomationWriteArmed(true);
    const input = openSynthRange(control("filter.cutoff"));
    fireEvent.input(input, { target: { value: "0.6" } });
    fireEvent.dblClick(input);
    await waitFor(() => expect(history).toHaveLength(1));
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_preset_param")).toHaveLength(0);
    expect(Number(updates.at(-1)!.physical)).toBe(specForConcreteKey("filter.cutoff")!.default);
  });

  it("finishes an interrupted write once and does not write again on a late change", async () => {
    renderEditor();
    openFilterPage();
    setAutomationWriteArmed(true);
    const input = openSynthRange(control("filter.cutoff"));
    fireEvent.input(input, { target: { value: "0.6" } });
    fireEvent.pointerCancel(input);
    fireEvent.change(input, { target: { value: "0.8" } });
    await waitFor(() => expect(history).toHaveLength(1));
    expect(invokeMock.mock.calls.filter(([command]) => command === "begin_guarded_synth_param_gesture")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([command]) => command === "end_owned_synth_param_gesture")).toHaveLength(1);
  });

  it("records one undo entry for a whole armed drag", async () => {
    renderEditor();
    openFilterPage();
    fireEvent.click(document.querySelector('[aria-label="Automation write mode"]')!);
    expect(document.querySelector(".synth-write-armed")).not.toBeNull();
    expect(automationWriteArmed()).toBe(true);

    const input = openSynthRange(control("filter.cutoff"));
    fireEvent.input(input, { target: { value: "0.6" } });
    fireEvent.input(input, { target: { value: "0.7" } });
    // While stopped the gesture writes one point at the playhead on release.
    fireEvent.change(input, { target: { value: "0.7" } });

    await waitFor(() => expect(history).toHaveLength(1));
    // Both samples land in one 1/16 cell: the last value wins, one point stays.
    expect(updates).toHaveLength(2);
    expect(updates[0].position).toEqual({ bar: 1, beat: 3, subdivision: 0 });
    expect(Number(updates[0].normalized)).toBeCloseTo(0.6, 4);
    expect(Number(updates[1].normalized)).toBeCloseTo(0.7, 4);
    expect(Number(updates[1].physical)).toBeGreaterThan(1000);
    // One begin/end pair: the preset write rides inside the same gesture.
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_param")).toHaveLength(0);
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "begin_guarded_synth_param_gesture"),
    ).toHaveLength(1);
    expect(project.tracks[0].automation[0].param).toBe(CUTOFF_LANE);
    expect(project.tracks[0].automation[0].points).toHaveLength(1);
    await waitFor(() => expect(automationWriteReport()).toBe("Recorded 1 point at 1:3:0."));

    // One undo step takes the lane away again.
    await store.undo();
    expect(project.tracks[0].automation).toHaveLength(0);
    await waitFor(() => expect(document.querySelector(".synth-automation-target")).toBeNull());
  });

  it("leaves an unarmed drag to the ordinary edit and creates no lane", async () => {
    renderEditor();
    openFilterPage();
    const input = openSynthRange(control("filter.cutoff"));
    fireEvent.input(input, { target: { value: "0.6" } });
    fireEvent.change(input, { target: { value: "0.6" } });
    await Promise.resolve();

    expect(project.tracks[0].automation).toHaveLength(0);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "begin_guarded_synth_param_gesture")).toHaveLength(0);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "update_synth_param_gesture")).toHaveLength(0);
  });

  it("shares one arm with the transport bar", async () => {
    renderEditor();
    openFilterPage();
    // The transport command toggles the same signal the header reads.
    toggleAutomationWriteArm();
    expect(automationWriteArmed()).toBe(true);
    await waitFor(() => expect(document.querySelector(".synth-write-armed")).not.toBeNull());

    setAutomationWriteArmed(false);
    await waitFor(() => expect(document.querySelector(".synth-write-armed")).toBeNull());
  });

  it("shows the A affordance on hover and writes the current value from it", async () => {
    renderEditor();
    openFilterPage();
    fireEvent.pointerMove(control("filter.cutoff"));
    const affordance = document.querySelector<HTMLElement>(".synth-automate-affordance");
    expect(affordance).not.toBeNull();
    expect(affordance!.getAttribute("aria-label")).toBe("Automate Filter Cutoff");

    fireEvent.click(affordance!);
    await waitFor(() => expect(project.tracks[0].automation).toHaveLength(1));
    expect(project.tracks[0].automation[0].param).toBe(CUTOFF_LANE);
    expect(history).toHaveLength(1);
  });

  it("offers an affordance on the widened model control", () => {
    renderEditor();
    openFilterPage();
    fireEvent.pointerMove(control("filter.model"));
    expect(document.querySelector(".synth-automate-affordance")?.getAttribute("aria-label")).toBe(
      "Automate Filter Model",
    );
  });
});
