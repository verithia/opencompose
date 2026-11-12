import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";

import { EffectAutomation } from "../../components/EffectAutomation";
import { expectInvoked, getLastCallArgs, invokeMock, makeEffect, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import type { AutomationLane } from "../../types/project";

const LANE: AutomationLane = {
  param: "fx1.threshold",
  smoothing_ms: 0,
  points: [
    { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -24 } },
    {
      position: { bar: 2, beat: 1, subdivision: 0 },
      value: { Db: -6 },
      curve: "smooth",
      tension: 0.5,
    },
  ],
};

const LFO_LANE: AutomationLane = {
  param: "fx1.threshold",
  mode: "lfo",
  lfo: {
    speed: { unit: "hz", hz: 2 },
    mult: 2,
    pulse_width: 0.25,
    skew: -0.5,
    tension: 0.3,
    amount: 0.75,
  },
  points: [
    { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -24 } },
    { position: { bar: 1, beat: 2, subdivision: 0 }, value: { Db: -6 } },
    { position: { bar: 1, beat: 3, subdivision: 0 }, value: { Db: -24 } },
    { position: { bar: 1, beat: 4, subdivision: 0 }, value: { Db: -12 } },
  ],
};

function seed(automation: AutomationLane[] = [structuredClone(LANE)]) {
  store.applyAiSnapshot(
    makeSnapshot({
      tracks: [
        {
          ...makeTrack("Drums", 0),
          effects: [makeEffect("Compression")],
          automation,
        },
      ],
    }),
  );
}

function renderEditor() {
  return render(() => <EffectAutomation target="track:0" />);
}

function laneRow(container: HTMLElement): HTMLElement {
  const row = container.querySelector<HTMLElement>('[data-lane="fx1.threshold"]');
  if (!row) throw new Error("no lane row");
  return row;
}

function control<T extends HTMLElement>(container: HTMLElement, selector: string): T {
  const element = laneRow(container).querySelector<T>(selector);
  if (!element) throw new Error(`no ${selector}`);
  return element;
}

function lfoCallCount(): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "set_automation_lfo").length;
}

beforeEach(() => {
  store.applyAiSnapshot(makeSnapshot());
});

afterEach(() => {
  cleanup();
});

describe("effect lane editor", () => {
  it("lists the lane's smoothing and each point's value and curve", () => {
    seed();
    const { container } = renderEditor();
    const row = laneRow(container);
    expect(row.querySelector(".effect-automation-name")?.textContent).toBe("fx1.threshold");
    const smoothing = row.querySelector<HTMLInputElement>(".effect-automation-smoothing");
    expect(smoothing?.value).toBe("0");
    const points = [...row.querySelectorAll<HTMLElement>(".effect-automation-point")];
    expect(points).toHaveLength(2);
    expect(points[0].querySelector(".effect-automation-pos")?.textContent).toBe("1:1");
    expect(points[0].querySelector(".effect-automation-value")?.textContent).toBe("-24 dB");
    expect(points[0].querySelector<HTMLSelectElement>("select")?.value).toBe("linear");
    expect(points[1].querySelector<HTMLSelectElement>("select")?.value).toBe("smooth");
    // The tension field only shows for a smooth point.
    expect(points[0].querySelector(".effect-automation-tension")).toBeNull();
    expect(points[1].querySelector<HTMLInputElement>(".effect-automation-tension")?.value).toBe(
      "0.5",
    );
  });

  it("commits a new smoothing time in milliseconds", async () => {
    seed();
    const { container } = renderEditor();
    const smoothing = control<HTMLInputElement>(container, ".effect-automation-smoothing");
    fireEvent.change(smoothing, { target: { value: "25" } });
    await waitFor(() =>
      expectInvoked("set_automation_smoothing", {
        target: "track:0",
        laneParam: "fx1.threshold",
        smoothingMs: 25,
      }),
    );
  });

  it("commits the per-point curve and drops the tension of a non-smooth point", async () => {
    seed();
    const { container } = renderEditor();
    const select = laneRow(container).querySelectorAll<HTMLSelectElement>(
      ".effect-automation-curve",
    )[1];
    fireEvent.change(select, { target: { value: "stairs" } });
    await waitFor(() =>
      expectInvoked("set_automation_point_curve", {
        target: "track:0",
        laneParam: "fx1.threshold",
        pointIndex: 1,
        curve: "stairs",
        tension: null,
      }),
    );
  });

  it("keeps the tension together with the smooth curve", async () => {
    seed([
      {
        param: "fx1.threshold",
        points: [
          { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -24 }, curve: "smooth" },
        ],
      },
    ]);
    const { container } = renderEditor();
    const tension = control<HTMLInputElement>(container, ".effect-automation-tension");
    // Without an authored tension the editor shows the curve's full smoothstep.
    expect(tension.value).toBe("1");
    fireEvent.change(tension, { target: { value: "0.25" } });
    await waitFor(() =>
      expectInvoked("set_automation_point_curve", {
        target: "track:0",
        laneParam: "fx1.threshold",
        pointIndex: 0,
        curve: "smooth",
        tension: 0.25,
      }),
    );
  });

  it("lists a control lane and names its target kind", () => {
    seed([
      {
        param: "brightness",
        points: [{ position: { bar: 1, beat: 1, subdivision: 0 }, value: { Percentage: 50 } }],
      },
    ]);
    const { container } = renderEditor();
    expect(container.querySelector("[data-effect-automation]")).toBeTruthy();
    const row = container.querySelector<HTMLElement>('[data-lane="brightness"]');
    expect(row, "a whole-timeline control lane is editable here").toBeTruthy();
    expect(row!.querySelector(".effect-automation-kind")?.textContent).toBe(
      i18n.t("timeline.automation.kind.control"),
    );
  });

  it("decodes a patch lane's name so it is readable", () => {
    const lane = `pluginparam_${Array.from(new TextEncoder().encode("filter.cutoff"))
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join("")}`;
    seed([
      {
        param: lane,
        points: [{ position: { bar: 1, beat: 1, subdivision: 0 }, value: { Number: 0.5 } }],
      },
    ]);
    const { container } = renderEditor();
    const row = container.querySelector<HTMLElement>(`[data-lane="${lane}"]`);
    expect(row, "a synth lane is no longer hidden").toBeTruthy();
    expect(row!.querySelector(".effect-automation-name")?.textContent).toContain("Cutoff");
    expect(row!.querySelector(".effect-automation-kind")?.textContent).toBe(
      i18n.t("timeline.automation.kind.patch"),
    );
  });

  it("removes a lane, whole-timeline or clip, from its row", async () => {
    seed([
      {
        param: "fx1.threshold",
        points: [{ position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -18 } }],
      },
    ]);
    const { container } = renderEditor();
    const remove = control<HTMLButtonElement>(container, ".effect-automation-delete");
    expect(remove.getAttribute("aria-label")).toBe(i18n.t("timeline.automation.deleteLane"));

    fireEvent.click(remove);

    await waitFor(() => expectInvoked("remove_automation_lane"));
    expect(getLastCallArgs("remove_automation_lane")).toMatchObject({
      owner: "track:0",
      laneParam: "fx1.threshold",
      laneIndex: 0,
    });
  });
});

describe("lane LFO mode", () => {
  it("marks the row as an LFO and shows every cycle control", () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    const row = laneRow(container);
    expect(row.dataset.laneMode).toBe("lfo");

    const mode = control<HTMLSelectElement>(container, ".effect-automation-mode");
    expect(mode.value).toBe("lfo");
    const group = control<HTMLElement>(container, ".effect-automation-lfo");
    expect(group.getAttribute("role")).toBe("group");
    expect(group.getAttribute("aria-label")).toBeTruthy();
    expect(group.title).toBeTruthy();

    expect(control<HTMLInputElement>(container, ".effect-automation-speed").value).toBe("2");
    expect(control<HTMLSelectElement>(container, ".effect-automation-speed-unit").value).toBe("hz");
    expect(control<HTMLInputElement>(container, ".effect-automation-mult").value).toBe("2");
    expect(control<HTMLInputElement>(container, ".effect-automation-pw").value).toBe("0.25");
    expect(control<HTMLInputElement>(container, ".effect-automation-skew").value).toBe("-0.5");
    expect(control<HTMLInputElement>(container, ".effect-automation-lfo-tension").value).toBe("0.3");
    expect(control<HTMLInputElement>(container, ".effect-automation-amount").value).toBe("0.75");
    for (const selector of [
      ".effect-automation-speed",
      ".effect-automation-speed-unit",
      ".effect-automation-mult",
      ".effect-automation-pw",
      ".effect-automation-skew",
      ".effect-automation-lfo-tension",
      ".effect-automation-amount",
    ]) {
      expect(control<HTMLInputElement>(container, selector).disabled).toBe(false);
    }
  });

  it("hides the cycle controls in envelope mode", () => {
    seed();
    const { container } = renderEditor();
    expect(laneRow(container).dataset.laneMode).toBe("envelope");
    expect(container.querySelector(".effect-automation-lfo")).toBeNull();
  });

  it("commits a mode switch and leaves the cycle controls untouched", async () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    const mode = control<HTMLSelectElement>(container, ".effect-automation-mode");
    fireEvent.change(mode, { target: { value: "envelope" } });
    await waitFor(() =>
      expectInvoked("set_automation_mode", {
        target: "track:0",
        laneParam: "fx1.threshold",
        mode: "envelope",
      }),
    );
    // The mode command is the only edit: no cycle field is rewritten.
    expect(lfoCallCount()).toBe(0);
  });

  it("commits an absolute speed and keeps the rest of the cycle", async () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    fireEvent.change(control<HTMLInputElement>(container, ".effect-automation-speed"), {
      target: { value: "4" },
    });
    await waitFor(() => expectInvoked("set_automation_lfo"));
    const args = getLastCallArgs("set_automation_lfo");
    expect(args?.target).toBe("track:0");
    expect(args?.laneParam).toBe("fx1.threshold");
    expect(args?.lfo).toEqual({
      speed: { unit: "hz", hz: 4 },
      mult: 2,
      pulse_width: 0.25,
      skew: -0.5,
      tension: 0.3,
      amount: 0.75,
    });
  });

  it("commits a note fraction", async () => {
    seed([
      {
        ...structuredClone(LFO_LANE),
        lfo: { ...structuredClone(LFO_LANE.lfo), speed: { unit: "note", numerator: 1, denominator: 4 } },
      },
    ]);
    const { container } = renderEditor();
    const speed = control<HTMLInputElement>(container, ".effect-automation-speed");
    expect(speed.value).toBe("1/4");
    fireEvent.change(speed, { target: { value: "3/8" } });
    await waitFor(() => expectInvoked("set_automation_lfo"));
    expect(getLastCallArgs("set_automation_lfo")?.lfo).toMatchObject({
      speed: { unit: "note", numerator: 3, denominator: 8 },
    });
  });

  it("starts a unit switch from that unit's own default", async () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    fireEvent.change(control<HTMLSelectElement>(container, ".effect-automation-speed-unit"), {
      target: { value: "note" },
    });
    await waitFor(() => expectInvoked("set_automation_lfo"));
    expect(getLastCallArgs("set_automation_lfo")?.lfo).toMatchObject({
      speed: { unit: "note", numerator: 1, denominator: 4 },
    });
  });

  it("rejects an unusable speed with an alert and no commit", async () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    const speed = control<HTMLInputElement>(container, ".effect-automation-speed");
    const before = lfoCallCount();
    fireEvent.change(speed, { target: { value: "0" } });
    await waitFor(() => expect(speed.getAttribute("aria-invalid")).toBe("true"));
    expect(lfoCallCount()).toBe(before);
    expect(container.querySelector(".effect-automation-error")?.textContent).toBeTruthy();
  });

  it("rejects a note fraction with a zero term", async () => {
    seed([
      {
        ...structuredClone(LFO_LANE),
        lfo: { ...structuredClone(LFO_LANE.lfo), speed: { unit: "note", numerator: 1, denominator: 4 } },
      },
    ]);
    const { container } = renderEditor();
    const speed = control<HTMLInputElement>(container, ".effect-automation-speed");
    const before = lfoCallCount();
    fireEvent.change(speed, { target: { value: "1/0" } });
    await waitFor(() => expect(speed.getAttribute("aria-invalid")).toBe("true"));
    expect(lfoCallCount()).toBe(before);
  });

  it("disables the cycle controls until the lane has two points", () => {
    seed([
      {
        ...structuredClone(LFO_LANE),
        points: [{ position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: -24 } }],
      },
    ]);
    let { container } = renderEditor();
    expect(control<HTMLInputElement>(container, ".effect-automation-speed").disabled).toBe(true);
    expect(control<HTMLInputElement>(container, ".effect-automation-mult").disabled).toBe(true);
    expect(control<HTMLInputElement>(container, ".effect-automation-pw").disabled).toBe(true);
    expect(control<HTMLInputElement>(container, ".effect-automation-skew").disabled).toBe(true);
    expect(control<HTMLInputElement>(container, ".effect-automation-lfo-tension").disabled).toBe(true);
    expect(control<HTMLInputElement>(container, ".effect-automation-amount").disabled).toBe(true);
    expect(container.querySelector(".effect-automation-lfo-hint")?.textContent).toBeTruthy();
    cleanup();

    seed([
      {
        ...structuredClone(LFO_LANE),
        points: [],
      },
    ]);
    ({ container } = renderEditor());
    expect(control<HTMLInputElement>(container, ".effect-automation-speed").disabled).toBe(true);
    expect(container.querySelector(".effect-automation-lfo-hint")?.textContent).toBeTruthy();
  });

  it("shows the cycle defaults when an LFO lane has no stored controls", () => {
    seed([
      {
        param: "fx1.threshold",
        mode: "lfo",
        points: structuredClone(LFO_LANE.points),
      },
    ]);
    const { container } = renderEditor();
    expect(control<HTMLInputElement>(container, ".effect-automation-speed").value).toBe("1");
    expect(control<HTMLInputElement>(container, ".effect-automation-mult").value).toBe("1");
    expect(control<HTMLInputElement>(container, ".effect-automation-pw").value).toBe("0.5");
    expect(control<HTMLInputElement>(container, ".effect-automation-skew").value).toBe("0");
    expect(control<HTMLInputElement>(container, ".effect-automation-lfo-tension").value).toBe("0");
    expect(control<HTMLInputElement>(container, ".effect-automation-amount").value).toBe("1");
  });

  it("gives every cycle control a name, a tooltip and a focus stop", () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    for (const selector of [
      ".effect-automation-mode",
      ".effect-automation-speed",
      ".effect-automation-speed-unit",
      ".effect-automation-mult",
      ".effect-automation-pw",
      ".effect-automation-skew",
      ".effect-automation-lfo-tension",
      ".effect-automation-amount",
    ]) {
      const element = control<HTMLElement>(container, selector);
      expect(element.getAttribute("aria-label"), selector).toBeTruthy();
      expect(element.getAttribute("title"), selector).toBeTruthy();
      expect(element.tabIndex, selector).toBeGreaterThanOrEqual(0);
      element.focus();
      expect(document.activeElement, selector).toBe(element);
    }
  });

  it("still edits each point's curve while the lane is a waveform", async () => {
    seed([structuredClone(LFO_LANE)]);
    const { container } = renderEditor();
    const select = laneRow(container).querySelectorAll<HTMLSelectElement>(
      ".effect-automation-curve",
    )[2];
    fireEvent.change(select, { target: { value: "half_sine" } });
    await waitFor(() =>
      expectInvoked("set_automation_point_curve", {
        target: "track:0",
        laneParam: "fx1.threshold",
        pointIndex: 2,
        curve: "half_sine",
        tension: null,
      }),
    );
  });
});
