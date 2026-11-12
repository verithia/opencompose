import { loadPluginPanel, pluginPanel } from "../../stores/pluginPanel";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { PluginParamPanel } from "../../components/PluginParamPanel";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { unwatchAutomation } from "../../stores/automationReadout";
import {
  automationWriteArmed,
  resetAutomationWriteForTests,
  setAutomationWriteArmed,
} from "../../stores/automationWrite";
import type { AutomationLane, PresetDef, Project } from "../../types/project";
import type { AutomationReadout, PluginPanelParam, PluginPanelSurface } from "../../stores/api";

const PRESET: PresetDef = {
  name: "Panel",
  source: "vst2",
  plugin: "vst2:fixture",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  params: [],
  effects: [],
};

const TRANSPORT = { playing: false, position_beats: 2, position_seconds: 1, duration_seconds: 30 };

let project: Project;
let history: Project[];
let pendingOld: Project | null;
let calls: Record<string, unknown>[];
let surface: PluginPanelSurface;
/** Answer the read-out command returns; a test sets it before mounting. */
let readoutAnswer: AutomationReadout;

function laneParam(name: string): string {
  return `pluginparam_${Array.from(new TextEncoder().encode(name))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")}`;
}

function upsertPoint(
  name: string,
  position: { bar: number; beat: number; subdivision: number },
  normalized: number,
) {
  const lane: AutomationLane = project.tracks[0].automation.find(
    (entry) => entry.param === laneParam(name),
  ) ?? { param: laneParam(name), points: [] };
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

/** A fake VST2/LV2-style plugin: twenty parameters, some grouped and some stepped. */
function twentyParams(): PluginPanelParam[] {
  return Array.from({ length: 20 }, (_, index) => ({
    index,
    name: `Param ${index}`,
    unit: index % 3 === 0 ? "dB" : "",
    min: 0,
    max: 10,
    current: index / 2,
    display: index % 3 === 0 ? `${index / 2} dB` : "",
    normalized: index / 20,
    default: index === 1 ? 5 : null,
    steps: index === 2 ? 11 : null,
    automatable: index !== 7,
    by_passing: index === 9,
    group: index < 3 ? "Filter" : null,
  }));
}

function installBackend(
  panelSurface: PluginPanelSurface,
  format = "vst2",
  automation: AutomationLane[] = [],
) {
  project = makeSnapshot({
    presets: [PRESET],
    tracks: [{ ...makeTrack("Synth", 0), preset: "Panel", automation }],
  }).project;
  history = [];
  pendingOld = null;
  calls = [];
  surface = panelSurface;
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, ...(args ?? {}) });
    switch (cmd) {
      case "begin_parameter_gesture": return `preset-session-${calls.length}`;
      case "end_owned_undo_gesture": return true;
      case "get_transport_state":
      case "transport_seek":
        return TRANSPORT;
      case "plugin_parameter_panel":
        return { preset: String(args?.presetName), format, surface };
      case "automation_value_at":
        return readoutAnswer;
      case "begin_guarded_plugin_param_gesture":
        pendingOld = structuredClone(project);
        return `plugin-session-${calls.length}`;
      case "update_owned_plugin_param_gesture":
        upsertPoint(
          String(args?.paramName),
          args?.position as { bar: number; beat: number; subdivision: number },
          Number(args?.value),
        );
        return makeSnapshot(structuredClone(project));
      case "end_owned_plugin_param_gesture":
        if (pendingOld) history.push(pendingOld);
        pendingOld = null;
        return { closed: true, snapshot: makeSnapshot(structuredClone(project)) };
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

function renderPanel() {
  return render(() => (
    <>
      <PluginParamPanel preset="Panel" />
      <ContextMenuLayer />
    </>
  ));
}

function rows(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-param]"));
}

function slider(name: string): HTMLInputElement {
  const row = document.querySelector<HTMLElement>(`[data-param="${name}"]`);
  if (!row) throw new Error(`no row for ${name}`);
  const input = row.querySelector<HTMLInputElement>('input[type="range"]');
  if (!input) throw new Error(`no slider for ${name}`);
  return input;
}

function menuButton(label: string): HTMLElement {
  const button = Array.from(document.querySelectorAll<HTMLElement>(".context-menu-item"))
    .find((item) => item.textContent?.includes(label));
  if (!button) throw new Error(`no menu item ${label}`);
  return button;
}

function gestureCalls(cmd: string) {
  return calls.filter((call) => call.cmd === cmd);
}

beforeEach(async () => {
  resetAutomationWriteForTests();
  readoutAnswer = { status: "not_driven", reason: "no automation lane targets 'Param 5'" };
  unwatchAutomation("track:0", laneParam("Param 5"));
  installBackend({ kind: "parameters", parameters: twentyParams() });
  await store.initProject();
  await store.seek(2);
});

afterEach(cleanup);

describe("generated plugin parameter panel", () => {
  it("shows the value a lane drives and hands the slider back on a drag", async () => {
    readoutAnswer = { status: "driven", value: 0.25, domain: "normalized", smoothing_ms: 0 };
    installBackend(
      { kind: "parameters", parameters: twentyParams() },
      "vst2",
      [{
        param: laneParam("Param 5"),
        points: [
          { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Number: 0.1 } },
          { position: { bar: 1, beat: 3, subdivision: 0 }, value: { Number: 0.4 } },
        ],
      }],
    );
    await store.initProject();
    await store.seek(2);
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    const row = () => document.querySelector<HTMLElement>('[data-param="Param 5"]')!;
    // The plugin's domain is 0..10, so a 0.25 position reads as 2.5.
    await waitFor(() => expect(row().querySelector(".plugin-panel-value")?.textContent).toBe("2.5"));
    expect(row().classList.contains("driven")).toBe(true);
    expect(row().querySelector('[data-driven-badge="true"]')).not.toBeNull();
    expect(slider("Param 5").value).toBe("0.25");

    fireEvent.pointerDown(slider("Param 5"));
    fireEvent.input(slider("Param 5"), { target: { value: "0.4" } });
    expect(row().querySelector('[data-driven-badge="true"]')).toBeNull();
    expect(slider("Param 5").value).toBe("0.4");
    fireEvent.pointerUp(slider("Param 5"));
  });

  it("renders one control per parameter of a twenty parameter plugin", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    // Groups come from the plugin, and the rest keeps enum order.
    const groups = Array.from(document.querySelectorAll(".plugin-panel-group-name"))
      .map((node) => node.textContent);
    expect(groups).toEqual(["Filter", "General"]);
    expect(rows().map((row) => row.getAttribute("data-param"))).toEqual(
      Array.from({ length: 20 }, (_, index) => `Param ${index}`),
    );
    // The read-out is the plugin's own value and unit.
    expect(rows()[0].querySelector(".plugin-panel-value")?.textContent).toBe("0 dB");
    expect(rows()[1].querySelector(".plugin-panel-value")?.textContent).toBe("0.5");
    expect(rows()[9].classList.contains("bypassing")).toBe(true);
    expect(rows()[9].querySelector(".plugin-panel-bypass")).not.toBeNull();
  });

  it("writes the normalized drag value into the plugin gesture command", async () => {
    setAutomationWriteArmed(true);
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    const input = slider("Param 5");
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.42" } });
    fireEvent.pointerUp(input);

    await waitFor(() => expect(gestureCalls("update_owned_plugin_param_gesture")).toHaveLength(1));
    const updates = gestureCalls("update_owned_plugin_param_gesture");
    expect(updates[0].paramName).toBe("Param 5");
    expect(Number(updates[0].value)).toBeCloseTo(0.42, 6);
    expect(updates[0].position).toEqual({ bar: 1, beat: 3, subdivision: 0 });
    // The read-out follows the drag in the plugin's own units.
    await waitFor(() => expect(rows()[5].querySelector(".plugin-panel-value")?.textContent).toBe("4.2"));
  });

  it("shows the plugin's own formatted value until the control is moved", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    // The plugin formats this one itself ("1.5 dB" for 1.5 on a 0..10 range).
    expect(rows()[3].querySelector(".plugin-panel-value")?.textContent).toBe("1.5 dB");

    const input = slider("Param 3");
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.5" } });
    fireEvent.pointerUp(input);

    // A moved value has no plugin-formatted string yet, so the host renders it.
    await waitFor(() => expect(rows()[3].querySelector(".plugin-panel-value")?.textContent).toBe("5 dB"));
  });

  it("keeps a whole drag inside one undo entry", async () => {
    setAutomationWriteArmed(true);
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    const input = slider("Param 4");
    fireEvent.pointerDown(input);
    for (const value of ["0.1", "0.2", "0.3"]) {
      fireEvent.input(input, { target: { value } });
    }
    fireEvent.pointerUp(input);
    fireEvent.change(input, { target: { value: "0.3" } });

    await waitFor(() => expect(history).toHaveLength(1));
    expect(gestureCalls("begin_guarded_plugin_param_gesture")).toHaveLength(1);
    expect(gestureCalls("end_owned_plugin_param_gesture")).toHaveLength(1);
    expect(gestureCalls("update_owned_plugin_param_gesture")).toHaveLength(3);
    expect(project.tracks[0].automation).toHaveLength(1);
    expect(project.tracks[0].automation[0].param).toBe(laneParam("Param 4"));
    expect(project.tracks[0].automation[0].points).toHaveLength(1);

    await store.undo();
    expect(project.tracks[0].automation).toHaveLength(0);
  });

  it("creates no lane from an unarmed drag", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    const input = slider("Param 5");
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.42" } });
    fireEvent.pointerUp(input);

    await waitFor(() => expect(calls.some((call) => call.cmd === "set_preset_param_owned")).toBe(true));
    expect(gestureCalls("begin_guarded_plugin_param_gesture")).toHaveLength(0);
    expect(gestureCalls("update_owned_plugin_param_gesture")).toHaveLength(0);
    expect(project.tracks[0].automation).toHaveLength(0);
    expect(automationWriteArmed()).toBe(false);
  });

  it("does not let a captured old release close a newer drag of the same hosted parameter", async () => {
    setAutomationWriteArmed(true);
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20));
    const listeners = vi.spyOn(window, "addEventListener");
    const input = slider("Param 5");
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.3" } });
    const oldRelease = listeners.mock.calls.filter(([type]) => type === "pointerup").at(-1)![1] as EventListener;
    fireEvent.pointerUp(input);
    await waitFor(() => expect(gestureCalls("end_owned_plugin_param_gesture")).toHaveLength(1));
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.7" } });
    oldRelease(new Event("pointerup"));
    fireEvent.change(input, { target: { value: "0.1" } });
    await Promise.resolve();
    expect(gestureCalls("end_owned_plugin_param_gesture")).toHaveLength(1);
    fireEvent.pointerUp(input);
    await waitFor(() => expect(gestureCalls("end_owned_plugin_param_gesture")).toHaveLength(2));
    expect(gestureCalls("update_owned_plugin_param_gesture").at(-1)?.value).toBe(0.7);
    listeners.mockRestore();
  });

  it("routes an armed drag through the shared writer", async () => {
    setAutomationWriteArmed(true);
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    const input = slider("Param 5");
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.42" } });
    fireEvent.input(input, { target: { value: "0.44" } });
    fireEvent.pointerUp(input);

    await waitFor(() => expect(gestureCalls("end_owned_plugin_param_gesture")).toHaveLength(1));
    expect(gestureCalls("begin_guarded_plugin_param_gesture")).toHaveLength(1);
    expect(gestureCalls("update_owned_plugin_param_gesture").length).toBeGreaterThan(0);
    expect(project.tracks[0].automation).toHaveLength(1);
    expect(project.tracks[0].automation[0].param).toBe(laneParam("Param 5"));
    expect(project.tracks[0].automation[0].points).toHaveLength(1);
    expect(calls.some((call) => call.cmd === "set_preset_param_owned")).toBe(false);
  });

  it("creates an automation lane from the context menu", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    fireEvent.contextMenu(rows()[3]);
    await waitFor(() => expect(document.querySelector(".context-menu-heading")?.textContent).toBe("Param 3"));
    fireEvent.click(menuButton("Automate"));

    await waitFor(() => expect(project.tracks[0].automation).toHaveLength(1));
    expect(project.tracks[0].automation[0].param).toBe(laneParam("Param 3"));
    expect(project.tracks[0].automation[0].points).toHaveLength(1);
    const stored = project.tracks[0].automation[0].points[0].value;
    expect("Number" in stored ? stored.Number : NaN).toBeCloseTo(3 / 20, 6);
    expect(history).toHaveLength(1);
  });

  it("disables automation for a parameter the plugin will not automate", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    fireEvent.contextMenu(rows()[7]);
    await waitFor(() => expect(document.querySelector(".context-menu-heading")?.textContent).toBe("Param 7"));
    const automate = menuButton("Automate");
    expect(automate.getAttribute("aria-disabled")).toBe("true");
    expect(automate.textContent).toContain("does not accept automation");
    fireEvent.click(automate);
    expect(project.tracks[0].automation).toHaveLength(0);
  });

  it("resets a parameter to the plugin default on double click", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    // Param 1 reports a default of 5 on a 0..10 range.
    fireEvent.doubleClick(rows()[1]);
    await waitFor(() => expect(gestureCalls("update_owned_plugin_param_gesture")).toHaveLength(1));
    const update = gestureCalls("update_owned_plugin_param_gesture")[0];
    expect(Number(update.value)).toBeCloseTo(0.5, 6);
    expect(history).toHaveLength(1);
  });

  it("falls back to the value the panel opened with when there is no default", async () => {
    renderPanel();
    await waitFor(() => expect(rows()).toHaveLength(20), { timeout: 3000 });

    // Param 4 has no default; it opened at 2.0 on a 0..10 range.
    fireEvent.doubleClick(rows()[4]);
    await waitFor(() => expect(gestureCalls("update_owned_plugin_param_gesture")).toHaveLength(1));
    const update = gestureCalls("update_owned_plugin_param_gesture")[0];
    expect(Number(update.value)).toBeCloseTo(0.2, 6);
  });

  it("shows a typed no-parameter state instead of an empty box", async () => {
    installBackend({ kind: "unavailable", issue: { kind: "plugin_reports_no_parameters" } });
    await store.initProject();
    renderPanel();

    await waitFor(
      () => expect(document.querySelector(".plugin-panel-unavailable")).not.toBeNull(),
      { timeout: 3000 },
    );
    expect(rows()).toHaveLength(0);
    expect(document.querySelector(".plugin-panel") ).toBeNull();
    expect(document.querySelector(".plugin-panel-unavailable-text")?.textContent)
      .toContain("reports no parameters");
  });

  it("names the format when it has no parameter list", async () => {
    installBackend(
      { kind: "unavailable", issue: { kind: "format_has_no_parameters", format: "soundfont" } },
      "soundfont",
    );
    await store.initProject();
    renderPanel();

    await waitFor(
      () => expect(document.querySelector(".plugin-panel-unavailable")).not.toBeNull(),
      { timeout: 3000 },
    );
    expect(document.querySelector(".plugin-panel-unavailable-format")?.textContent).toBe("SOUNDFONT");
    expect(document.querySelector(".plugin-panel-unavailable-text")?.textContent)
      .toContain("no parameter list");
  });

  it("reports a plugin the host could not query", async () => {
    installBackend({
      kind: "unavailable",
      issue: { kind: "query_failed", message: "no component named Fixture" },
    });
    await store.initProject();
    renderPanel();

    await waitFor(
      () => expect(document.querySelector(".plugin-panel-unavailable")).not.toBeNull(),
      { timeout: 3000 },
    );
    expect(document.querySelector(".plugin-panel-unavailable-text")?.textContent)
      .toContain("no component named Fixture");
  });
});


it("ignores an older same-name parameter inventory after a newer panel load", async () => {
  let finish!: (response: unknown) => void;
  const old = new Promise<unknown>(resolve => { finish = resolve; });
  let queries = 0;
  invokeMock.mockImplementation(async command => {
    if (command === "plugin_parameter_panel") {
      if (++queries === 1) return old;
      return { preset: "Panel", format: "vst2", surface: { kind: "parameters", parameters: [{ ...twentyParams()[0], name: "New Inventory" }] } };
    }
    return makeSnapshot(project);
  });
  const earlier = loadPluginPanel("Panel");
  await loadPluginPanel("Panel");
  finish({ preset: "Panel", format: "vst2", surface: { kind: "parameters", parameters: twentyParams() } });
  await earlier;
  expect(pluginPanel().params.map(param => param.name)).toEqual(["New Inventory"]);
});
