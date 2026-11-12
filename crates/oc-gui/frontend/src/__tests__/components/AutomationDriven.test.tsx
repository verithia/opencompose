import { openSynthRange } from "../helpers/synthRange";
/**
 * A knob shows the value a lane drives into it, keeps a driven badge while it
 * does, and hands the control back to the operator on a drag. With no lane, a
 * muted owner or a stopped transport the stored value stays and the tooltip
 * says why.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";

import { DevicePanel } from "../../components/DevicePanel";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { TooltipLayer } from "../../components/Tooltip";
import { store } from "../../stores/project";
import { automationReadout, unwatchAutomation } from "../../stores/automationReadout";
import {
  automationWriteReport,
  resetAutomationWriteForTests,
  setAutomationWriteArmed,
} from "../../stores/automationWrite";
import type { AutomationReadout } from "../../stores/api";
import type { AutomationLane, PresetDef, Project } from "../../types/project";
import type { DeviceDef } from "../../types/devices";
import { invokeMock, makeEffect, makeSnapshot, makeTrack, TRANSPORT_PLAYING } from "../setup";

const DEVICE: DeviceDef = {
  id: "drive",
  name: "Drive",
  category: "utility",
  params: [
    {
      id: "threshold", name: "Threshold", unit: "dB", min: -60, max: 0,
      default: -18, curve: "linear", steps: null,
    },
  ],
};

/** `fx1.threshold` addresses the first effect of the owner's chain. */
const THRESHOLD_LANE: AutomationLane = {
  param: "fx1.threshold",
  points: [
    { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -6 } },
    { position: { bar: 1, beat: 3, subdivision: 0 }, value: { Db: -30 } },
  ],
};

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

const CUTOFF_LANE: AutomationLane = {
  param: "pluginparam_66696c7465722e6375746f6666",
  points: [
    { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Number: 0.2 } },
    { position: { bar: 1, beat: 3, subdivision: 0 }, value: { Number: 0.8 } },
  ],
};

let loaded: Project;
/** Answer the read-out command returns; each test sets its own. */
let answer: AutomationReadout;
let readoutCalls: { target: string; lane: string; beat: number }[];
/** Backend calls the effect write transaction made, in order. */
let writeCalls: { command: string; args: Record<string, unknown> }[];

/** Store one point of `fx1.threshold`, replacing the point at that position. */
function writeThresholdPoint(position: { bar: number; beat: number; subdivision: number }, text: string) {
  const track = loaded.tracks[0];
  const lane = track.automation.find((entry) => entry.param === "fx1.threshold")
    ?? { param: "fx1.threshold", points: [] };
  if (!track.automation.includes(lane)) track.automation.push(lane);
  const point = { position, value: { Db: Number(text.replace("dB", "")) } };
  const index = lane.points.findIndex((entry) => (
    entry.position.bar === position.bar
    && entry.position.beat === position.beat
    && entry.position.subdivision === position.subdivision
  ));
  if (index >= 0) lane.points[index] = point;
  else lane.points.push(point);
}

function installBackend() {
  invokeMock.mockReset();
  writeCalls = [];
  invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command.endsWith("_effect_param_write")) {
      writeCalls.push({ command, args: args ?? {} });
      if (command === "begin_guarded_effect_param_write") return `effect-session-${writeCalls.length}`;
      if (command === "update_owned_effect_param_write") {
        writeThresholdPoint(
          args?.position as { bar: number; beat: number; subdivision: number },
          String(args?.value),
        );
        return makeSnapshot(structuredClone(loaded));
      }
      return { closed: true, snapshot: makeSnapshot(structuredClone(loaded)) };
    }
    if (command === "get_project") return makeSnapshot(loaded);
    if (command === "get_transport_state") return TRANSPORT_PLAYING();
    if (command === "transport_seek") {
      return { ...TRANSPORT_PLAYING(), position_beats: Number(args?.beat) };
    }
    if (command === "automation_value_at") {
      readoutCalls.push({
        target: String(args?.target),
        lane: String(args?.laneParam),
        beat: Number(args?.positionBeats),
      });
      return answer;
    }
    if (command === "begin_guarded_effect_param_touch") return null;
    if (command === "get_undo_state") {
      return {
        can_undo: false, can_redo: false,
        undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
      };
    }
    return makeSnapshot(loaded);
  });
}

/** Load one project into the store, with the backend answering for it. */
async function loadProject(project: Project) {
  loaded = project;
  installBackend();
  await store.initProject();
  await store.seek(1.5);
}

function effect(thresholdDb: number) {
  return makeEffect("Gain", [["threshold", { Db: thresholdDb }]]);
}

/** A one-track project whose first effect carries `automation`. */
function pianoProject(automation: AutomationLane[]): Project {
  return makeSnapshot({
    tracks: [{ ...makeTrack("Piano", 0), effects: [effect(-18)], automation }],
  }).project;
}

beforeEach(async () => {
  resetAutomationWriteForTests();
  answer = { status: "not_driven", reason: "no automation lane targets 'threshold'" };
  readoutCalls = [];
  unwatchAutomation("track:0", THRESHOLD_LANE.param);
  unwatchAutomation("track:0", CUTOFF_LANE.param);
  await loadProject(pianoProject([THRESHOLD_LANE]));
});

afterEach(cleanup);

function panel(project: Project) {
  return (
    <DevicePanel
      target="track:0"
      index={0}
      count={1}
      effect={project.tracks[0].effects[0]}
      device={DEVICE}
      onClose={() => {}}
    />
  );
}

function readoutText(container: HTMLElement): string | null | undefined {
  return container.querySelector('[data-readout-param="threshold"]')?.textContent;
}

describe("driven knobs", () => {
  it("shows the driven value with a badge, and a drag takes over", async () => {
    answer = { status: "driven", value: -12.5, domain: "device", smoothing_ms: 0 };
    const { container } = render(() => panel(loaded));

    await waitFor(() => expect(readoutText(container)).toBe("-12.5 dB"));
    const row = () => container.querySelector('[data-param-row="threshold"]')!;
    expect(row().querySelector('[data-driven-badge="true"]')).not.toBeNull();
    expect(row().classList.contains("driven")).toBe(true);
    expect(readoutCalls[0]).toEqual({ target: "track:0", lane: "fx1.threshold", beat: 1.5 });

    // The hand owns the knob while it moves: the lane value and badge go away.
    const knob = container.querySelector('[data-param-id="threshold"]')!;
    fireEvent.mouseDown(knob, { clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 60 });
    expect(row().querySelector('[data-driven-badge="true"]')).toBeNull();
    expect(readoutText(container)).toBe("0.0 dB");
    fireEvent.mouseUp(document);
  });

  it("keeps the stored value and shows no badge without a lane", async () => {
    await loadProject(pianoProject([]));
    const { container } = render(() => panel(pianoProject([])));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(readoutText(container)).toBe("-18.0 dB");
    expect(container.querySelector('[data-driven-badge="true"]')).toBeNull();
    expect(container.querySelector(".device-control.driven")).toBeNull();
    expect(readoutCalls).toHaveLength(0);
  });

  it("shows the stored value with the reason in the tooltip when the lane is off", async () => {
    answer = { status: "not_driven", reason: "track 'Piano' is muted" };
    const { container } = render(() => (
      <>
        {panel(loaded)}
        <TooltipLayer />
      </>
    ));

    await waitFor(() =>
      expect(automationReadout("track:0", "fx1.threshold")?.status).toBe("not_driven"),
    );
    expect(readoutText(container)).toBe("-18.0 dB");
    expect(container.querySelector('[data-driven-badge="true"]')).toBeNull();

    fireEvent.mouseEnter(container.querySelector(".device-control-name")!);
    await waitFor(
      () => expect(document.querySelector(".oc-tooltip")?.textContent).toContain("track 'Piano' is muted"),
      { timeout: 2000 },
    );
  });
});

describe("the synth knob", () => {
  it("shows the normalized lane value as a physical read-out, then a drag takes over", async () => {
    const project = makeSnapshot({
      presets: [PRESET],
      tracks: [{ ...makeTrack("Synth", 0), preset: "Lead", automation: [CUTOFF_LANE] }],
    }).project;
    await loadProject(project);
    answer = { status: "driven", value: 0.5, domain: "normalized", smoothing_ms: 0 };

    const { container } = render(() => (
      <OpenSynthEditor
        preset={PRESET}
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
    ));

    const tab = Array.from(container.querySelectorAll(".synth-subtabs button"))
      .find((button) => button.textContent === "FILTER");
    fireEvent.click(tab!);

    const control = () => container.querySelector<HTMLElement>('[data-param="filter.cutoff"]')!;
    await waitFor(() => expect(control().querySelector(".synth-param-value")?.textContent).toBe("632.5Hz"));
    expect(control().querySelector('[data-driven-badge="true"]')).not.toBeNull();
    expect(control().classList.contains("driven")).toBe(true);

    const input = openSynthRange(control());
    fireEvent.input(input, { target: { value: "0.6" } });
    expect(control().querySelector('[data-driven-badge="true"]')).toBeNull();
    // 20 Hz * 1000^0.6 = 1261.9, rounded to the control's 1 Hz step.
    expect(control().querySelector(".synth-param-value")?.textContent).toBe("1262.0Hz");
    fireEvent.change(input, { target: { value: "0.6" } });
  });
});

describe("writing an effect parameter", () => {
  it("keeps a newer device write open after the previous knob's captured mouseup", async () => {
    await loadProject(pianoProject([]));
    setAutomationWriteArmed(true);
    const { container } = render(() => panel(pianoProject([])));
    const listeners = vi.spyOn(document, "addEventListener");
    const knob = container.querySelector('[data-param-id="threshold"]')!;
    fireEvent.mouseDown(knob, { clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 80 });
    const oldRelease = listeners.mock.calls.filter(([type]) => type === "mouseup").at(-1)![1] as EventListener;
    fireEvent.mouseUp(document);
    await waitFor(() => expect(writeCalls.filter((call) => call.command === "end_owned_effect_param_write")).toHaveLength(1));
    fireEvent.mouseDown(knob, { clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 60 });
    oldRelease(new Event("mouseup"));
    await Promise.resolve();
    expect(writeCalls.filter((call) => call.command === "end_owned_effect_param_write")).toHaveLength(1);
    fireEvent.mouseUp(document);
    await waitFor(() => expect(writeCalls.filter((call) => call.command === "end_owned_effect_param_write")).toHaveLength(2));
    listeners.mockRestore();
  });

  it("creates the lane from an armed drag as one undo entry", async () => {
    await loadProject(pianoProject([]));
    setAutomationWriteArmed(true);
    const { container } = render(() => panel(pianoProject([])));

    const knob = container.querySelector('[data-param-id="threshold"]')!;
    fireEvent.mouseDown(knob, { clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 40 });
    fireEvent.mouseUp(document);

    await waitFor(() =>
      expect(writeCalls.filter((call) => call.command === "end_owned_effect_param_write")).toHaveLength(1),
    );
    expect(
      writeCalls.filter((call) => call.command === "begin_guarded_effect_param_write"),
    ).toHaveLength(1);
    const updates = writeCalls.filter((call) => call.command === "update_owned_effect_param_write");
    expect(updates.length).toBeGreaterThan(0);
    // The project was loaded at beat 1.5, which is a 1/16 position.
    expect(updates[0].args.position).toEqual({ bar: 1, beat: 2, subdivision: 2 });

    const lane = loaded.tracks[0].automation.find((entry) => entry.param === "fx1.threshold");
    expect(lane?.points).toHaveLength(1);
    // One cell: the last value of the drag is the point that stays.
    expect(lane?.points[0].value).toEqual({ Db: 0 });
    expect(automationWriteReport()).toBe("Recorded 1 point at 1:2:2.");
  });

  it("leaves an unarmed drag to the ordinary edit and creates no lane", async () => {
    await loadProject(pianoProject([]));
    const { container } = render(() => panel(pianoProject([])));

    const knob = container.querySelector('[data-param-id="threshold"]')!;
    fireEvent.mouseDown(knob, { clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 40 });
    fireEvent.mouseUp(document);
    await Promise.resolve();

    expect(writeCalls).toHaveLength(0);
    expect(loaded.tracks[0].automation).toHaveLength(0);
    expect(automationWriteReport()).toBeNull();
  });
});
