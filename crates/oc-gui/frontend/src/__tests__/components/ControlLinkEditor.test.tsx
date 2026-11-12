/**
 * Control-link editor: fan-out rows, formula/smoothing edits, the MIDI learn
 * arm and the last-tweaked shortcut, all rendered as compact rows.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";

import { ControlLinkEditor } from "../../components/ControlLinkEditor";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import {
  learnLastTweaked,
  rememberTweaked,
  resetControlLinkState,
} from "../../stores/controlLinks";
import { store } from "../../stores/project";
import type { ControlLink, Project, ProjectSnapshot } from "../../types/project";

interface Activity {
  running: boolean;
  events: number;
  last: {
    kind: string;
    channel: number;
    note: number | null;
    velocity: number | null;
    controller: number | null;
    value: number | null;
    millis: number;
  } | null;
  error: string | null;
  audition_active: boolean;
}

let project: Project;
let activity: { value: () => Activity; set: (next: Activity) => void };

const IDLE: Activity = { running: true, events: 0, last: null, error: null, audition_active: false };

function link(): ControlLink {
  return {
    name: "cc74",
    source: { kind: "midi_cc", channel: 1, controller: 74 },
    targets: [
      { track: "Lead", param: "filter.cutoff", formula: "midi_unit", smoothing_ms: 0 },
      { track: "Lead", param: "output.gain", formula: "exponential", smoothing_ms: 8 },
    ],
  };
}

function snapshot(): ProjectSnapshot {
  return makeSnapshot(structuredClone(project));
}

function installBackend() {
  project = makeSnapshot({
    tracks: [{ ...makeTrack("Lead", 0), preset: "Lead" }],
    presets: [{
      name: "Lead", source: "builtin", plugin: "builtin.open_synth", program: null,
      patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
    }],
  }).project;
  project.properties.control_links = [link()];
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "update_control_link_target") {
      const target = project.properties.control_links?.[0].targets.find((entry) => entry.param === args?.param);
      if (target) {
        target.formula = args?.formula as typeof target.formula;
        target.smoothing_ms = Number(args?.smoothingMs);
      }
      return snapshot();
    }
    if (cmd === "add_control_link_target") {
      const target = args?.target as ControlLink["targets"][number];
      const links = project.properties.control_links ?? (project.properties.control_links = []);
      let entry = links.find((candidate) => candidate.source.kind === "midi_cc");
      if (!entry) {
        entry = { name: "cc74", source: { kind: "midi_cc", channel: 1, controller: 74 }, targets: [] };
        links.push(entry);
      }
      if (!entry.targets.some((row) => row.param === target.param)) entry.targets.push(target);
      return snapshot();
    }
    if (cmd === "remove_control_link_target") {
      const links = project.properties.control_links ?? [];
      for (const entry of links) entry.targets = entry.targets.filter((row) => row.param !== args?.param);
      project.properties.control_links = links.filter((entry) => entry.targets.length > 0);
      return snapshot();
    }
    return snapshot();
  });
}

function renderEditor() {
  const [activityValue, setActivity] = createSignal<Activity>(IDLE);
  activity = { value: activityValue, set: setActivity };
  return render(() => <ControlLinkEditor activity={() => activityValue()} />);
}

function expand(container: HTMLElement) {
  fireEvent.click(container.querySelector(".control-link-toggle") as HTMLElement);
}

beforeEach(async () => {
  cleanup();
  resetControlLinkState();
  installBackend();
  await store.initProject();
});

describe("ControlLinkEditor", () => {
  it("shows one compact row per fan-out target", () => {
    const { container } = renderEditor();
    expect(container.querySelectorAll(".control-link-row")).toHaveLength(0);
    expand(container);
    const rows = [...container.querySelectorAll(".control-link-row")];
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("Lead · Filter Cutoff");
    expect(rows[1].textContent).toContain("Lead · Amp Gain");
    expect(container.querySelector(".control-link-source")?.textContent).toBe("CC 74");
  });

  it("retunes one row's formula and smoothing without touching the other", async () => {
    const { container } = renderEditor();
    expand(container);
    const rows = [...container.querySelectorAll(".control-link-row")];
    const formula = rows[1].querySelector("select") as HTMLSelectElement;
    fireEvent.change(formula, { target: { value: "logarithmic" } });
    await waitFor(() => {
      expect(store.project.properties.control_links?.[0].targets[1].formula).toBe("logarithmic");
      expect(store.project.properties.control_links?.[0].targets[0].formula).toBe("midi_unit");
    });

    const smoothing = rows[0].querySelector("input[type=number]") as HTMLInputElement;
    fireEvent.change(smoothing, { target: { value: "25" } });
    await waitFor(() => {
      expect(store.project.properties.control_links?.[0].targets[0].smoothing_ms).toBe(25);
    });
    // The update command carries the camelCase Tauri argument.
    const updates = invokeMock.mock.calls.filter(([cmd]) => cmd === "update_control_link_target");
    const update = updates[updates.length - 1];
    expect(update?.[1]).toMatchObject({ param: "filter.cutoff", smoothingMs: 25 });
  });

  it("learns a CC for the last tweaked parameter and renders the new row", async () => {
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "amp.attack", label: "Attack" });
    const { container } = renderEditor();
    expand(container);
    expect(learnLastTweaked()).toBe(true);
    // The component effect consumes the next live control change.
    activity.set({
      ...IDLE,
      last: {
        kind: "control_change",
        channel: 1,
        note: null,
        velocity: null,
        controller: 74,
        value: 100,
        millis: 4000,
      },
    });
    await waitFor(() => {
      const rows = [...container.querySelectorAll(".control-link-row")];
      expect(rows).toHaveLength(3);
      expect(rows.some((row) => row.textContent?.includes("Lead · Amp Attack"))).toBe(true);
    });
  });

  it("binds the remembered parameter to an existing control in one click", async () => {
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "filter.resonance", label: "Resonance" });
    const { container } = renderEditor();
    expand(container);
    fireEvent.click(container.querySelector(".control-link-bind") as HTMLElement);
    await waitFor(() => {
      expect(container.querySelectorAll(".control-link-row")).toHaveLength(3);
      expect(container.textContent).toContain("Lead · Filter Resonance");
      expect(store.project.properties.control_links?.[0].targets[2]).toMatchObject({
        param: "filter.resonance",
        formula: "midi_unit",
      });
    });
  });
});
