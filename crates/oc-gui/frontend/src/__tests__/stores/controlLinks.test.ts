/**
 * Control-link store: MIDI learn writes one record per target, the last-tweaked
 * shortcut binds the remembered parameter, and edits reach the right commands.
 */
import { beforeEach, describe, expect, it } from "vitest";

import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import {
  addControlTarget,
  bindLastTweaked,
  cancelLearn,
  controlLinks,
  defaultFormula,
  lastTweaked,
  learnFromMidiActivity,
  learnLastTweaked,
  learning,
  rememberTweaked,
  resetControlLinkState,
  updateControlTarget,
} from "../../stores/controlLinks";
import { store } from "../../stores/project";
import type { ControlLinkSource, ControlLinkTarget, Project, ProjectSnapshot } from "../../types/project";

const EVENT = {
  kind: "control_change",
  channel: 1,
  note: null,
  velocity: null,
  controller: 74,
  value: 100,
  millis: 1000,
};

let project: Project;

function sameSource(a: { kind: string }, b: { kind: string }): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Minimal backend: learn keeps one row per `(track, param)` like the command. */
function installBackend() {
  project = makeSnapshot({
    tracks: [{ ...makeTrack("Lead", 0), preset: "Lead" }],
    presets: [{
      name: "Lead", source: "builtin", plugin: "builtin.open_synth", program: null,
      patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
    }],
  }).project;
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "get_project") return snapshot();
    if (cmd === "add_control_link_target") {
      const source = args?.source as ControlLinkSource;
      const target = args?.target as ControlLinkTarget;
      const links = project.properties.control_links ?? (project.properties.control_links = []);
      let link = links.find((entry) => sameSource(entry.source, source));
      if (!link) {
        link = { name: source.kind === "midi_cc" ? `cc${source.controller}` : "link", source, targets: [] };
        links.push(link);
      }
      if (!link.targets.some((entry) => entry.track === target.track && entry.param === target.param)) {
        link.targets.push(target);
      }
      return snapshot();
    }
    if (cmd === "update_control_link_target") {
      const link = (project.properties.control_links ?? []).find((entry) => (
        entry.source.kind === "midi_cc" && entry.source.controller === (args?.source as { controller: number }).controller
      ));
      const target = link?.targets.find((entry) => entry.param === args?.param);
      if (target) {
        target.formula = args?.formula as typeof target.formula;
        target.smoothing_ms = Number(args?.smoothingMs);
      }
      return snapshot();
    }
    if (cmd === "remove_control_link_target") {
      const links = project.properties.control_links ?? [];
      for (const link of links) {
        link.targets = link.targets.filter((entry) => entry.param !== args?.param);
      }
      project.properties.control_links = links.filter((link) => link.targets.length > 0);
      return snapshot();
    }
    return snapshot();
  });
}

function snapshot(): ProjectSnapshot {
  return makeSnapshot(structuredClone(project));
}

beforeEach(async () => {
  resetControlLinkState();
  installBackend();
  await store.initProject();
});

describe("control link store", () => {
  it("MIDI learn writes exactly one record per target", async () => {
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "filter.cutoff", label: "Filter Cutoff" });
    expect(learnLastTweaked()).toBe(true);
    expect(learning()).not.toBeNull();

    expect(await learnFromMidiActivity(EVENT)).toBe(true);
    // The arm disarms after the first CC, so a held controller writes no more.
    expect(await learnFromMidiActivity(EVENT)).toBe(false);
    const links = controlLinks();
    expect(links).toHaveLength(1);
    expect(links[0].source).toEqual({ kind: "midi_cc", channel: 1, controller: 74 });
    expect(links[0].targets).toHaveLength(1);
    expect(links[0].targets[0].param).toBe("filter.cutoff");

    // Learning the same target under the same CC again still keeps one row.
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "filter.cutoff", label: "Filter Cutoff" });
    learnLastTweaked();
    expect(await learnFromMidiActivity({ ...EVENT, millis: 2000 })).toBe(true);
    expect(controlLinks()[0].targets).toHaveLength(1);

    // A second target under the same control is one more row.
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "output.gain", label: "Gain" });
    learnLastTweaked();
    expect(await learnFromMidiActivity({ ...EVENT, millis: 3000 })).toBe(true);
    expect(controlLinks()[0].targets).toHaveLength(2);
  });

  it("bind last tweaked adds the remembered parameter in one action", async () => {
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "output.gain", label: "Gain" });
    expect(await bindLastTweaked({ kind: "macro", index: 2 })).toBe(true);
    expect(controlLinks()[0].source).toEqual({ kind: "macro", index: 2 });
    expect(controlLinks()[0].targets[0]).toEqual({
      track: "Lead",
      param: "output.gain",
      formula: "linear",
      smoothing_ms: 0,
    });

    // A CC source defaults to the 0-127 scaler.
    rememberTweaked({ track: "Lead", trackIndex: 0, param: "filter.cutoff", label: "Filter Cutoff" });
    expect(await bindLastTweaked({ kind: "midi_cc", channel: 1, controller: 20 })).toBe(true);
    expect(defaultFormula({ kind: "midi_cc", channel: 1, controller: 20 })).toBe("midi_unit");
    expect(controlLinks().find((link) => link.source.kind === "midi_cc")?.targets[0].formula)
      .toBe("midi_unit");
  });

  it("reports a missing last-tweaked parameter instead of binding nothing", async () => {
    expect(lastTweaked()).toBeNull();
    expect(learnLastTweaked()).toBe(false);
    expect(await bindLastTweaked({ kind: "macro", index: 1 })).toBe(false);
    expect(controlLinks()).toHaveLength(0);
  });

  it("retunes a row and can be cancelled while learning", async () => {
    await addControlTarget(
      { kind: "midi_cc", channel: 1, controller: 7 },
      { track: "Lead", param: "filter.cutoff", formula: "midi_unit", smoothing_ms: 0 },
    );
    const link = controlLinks()[0];
    await updateControlTarget(link.source, link.targets[0], "exponential", 12);
    expect(controlLinks()[0].targets[0].formula).toBe("exponential");
    expect(controlLinks()[0].targets[0].smoothing_ms).toBe(12);

    rememberTweaked({ track: "Lead", trackIndex: 0, param: "output.gain", label: "Gain" });
    learnLastTweaked();
    cancelLearn();
    expect(learning()).toBeNull();
    expect(await learnFromMidiActivity(EVENT)).toBe(false);
  });
});
