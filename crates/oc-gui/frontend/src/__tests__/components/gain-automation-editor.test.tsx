import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { GainAutomationEditor } from "../../components/GainAutomationEditor";
import { Timeline } from "../../components/Timeline";
import { Mixer } from "../../components/Mixer";
import { gainAutomationUi } from "../../stores/gainAutomationUi";
import { panelStore } from "../../stores/panels";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { gainAutomationView } from "../../utils/gainAutomationView";
import {
  lanePolyline,
  mappedAutomationValue,
  unmappedAutomationValue,
} from "../../utils/automationRow";
import { AUTOMATION_CURVES } from "../../data/automationCurves.gen";
import type { GainAutomation } from "../../types/project";
import { getLastCallArgs, invokeMock, makeSnapshot } from "../setup";
const curve = (overrides: Partial<GainAutomation> = {}): GainAutomation => ({
  interpolation: "Segments",
  ppq: 96000,
  time_range: {
    start: { bar: 1, beat: 1, subdivision: 0 },
    end: { bar: 2, beat: 1, subdivision: 0 },
  },
  points: [
    {
      position: { bar: 1, beat: 1, subdivision: 0 },
      value: { Number: 0 },
      curve: "exponential",
      curvature: -2,
    },
    { position: { bar: 2, beat: 1, subdivision: 0 }, value: { Number: 1 } },
  ],
  ...overrides,
});
const fixture = (envelope = curve()) =>
  makeSnapshot({
    buses: [
      {
        name: "gain owner",
        routes: ["master"],
        effects: [],
        gain_automation: [envelope],
      },
    ],
  });
beforeEach(() => {
  gainAutomationUi.clear();
  i18n.setLocale("en");
  store.applyAiSnapshot(fixture());
});
afterEach(() => cleanup());

it("keeps literal legacy Step/Linear geometry and opts into segment curves only explicitly", () => {
  const p = (position: { bar: number; beat: number; subdivision: number }) =>
    (position.bar - 1) * 4 + position.beat - 1;
  const bounds = { min: 0, max: 1 };
  expect(
    lanePolyline(
      gainAutomationView(curve({ interpolation: "Linear" })),
      p,
      10,
      bounds,
      38,
    ).split(" "),
  ).toHaveLength(2);
  const held = lanePolyline(
    gainAutomationView(curve({ interpolation: "Step" })),
    p,
    10,
    bounds,
    38,
  ).split(" ");
  expect(held).toHaveLength(3);
  expect(held[0].split(",")[1]).toBe(held[1].split(",")[1]);
  expect(
    lanePolyline(gainAutomationView(curve()), p, 10, bounds, 38).split(" "),
  ).toHaveLength(17);
});

it("applies quantization before the value mapping and refuses a fictitious inverse", () => {
  const map = {
    curve: "exponential" as const,
    curvature: 2,
    scale: 2,
    input_quantization_steps: 4,
  };
  expect(mappedAutomationValue(0.49, map)).toBe(
    mappedAutomationValue(0.25, {
      ...map,
      input_quantization_steps: undefined,
    }),
  );
  expect(unmappedAutomationValue(0.5, map)).toBeNull();
  expect(
    mappedAutomationValue(-0.01, {
      curve: "linear",
      input_quantization_steps: 4,
    }),
  ).toBe(-0.25);
  for (const pair of [
    [0, 1],
    [1, 0],
  ]) {
    const c = curve({
      points: [
        {
          position: { bar: 1, beat: 1, subdivision: 0 },
          value: { Number: pair[0] },
        },
        {
          position: { bar: 2, beat: 1, subdivision: 0 },
          value: { Number: pair[1] },
        },
      ],
      value_mapping: { curve: "linear", input_quantization_steps: 4 },
    });
    const path = lanePolyline(
      gainAutomationView(c),
      (p) => (p.bar - 1) * 4 + p.beat - 1,
      25,
      { min: 0, max: 1 },
      38,
    )
      .split(" ")
      .map((v) => v.split(",").map(Number));
    for (let i = 1; i < path.length; i++)
      expect(
        path[i][0] === path[i - 1][0] || path[i][1] === path[i - 1][1],
      ).toBe(true);
    expect(c.points).toHaveLength(2);
  }
});

it("exposes quantized raw anchor editing and sends owner/stamp/revision guarded replacement", async () => {
  const c = curve({
    value_mapping: {
      curve: "exponential",
      curvature: 2,
      input_quantization_steps: 4,
    },
  });
  const snapshot = fixture(c);
  store.applyAiSnapshot(snapshot);
  const base = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (command, args) =>
    command === "replace_gain_automation" ? snapshot : base?.(command, args),
  );
  invokeMock.mockClear();
  gainAutomationUi.open("bus:gain owner", 0);
  const view = render(() => <GainAutomationEditor target="bus:gain owner" />);
  expect(view.container.textContent).toContain(
    "Quantized output has no unique inverse",
  );
  const input = view.getByRole("spinbutton", { name: "Raw anchor 1" });
  fireEvent.change(input, { target: { value: "0.5" } });
  expect(
    invokeMock.mock.calls.some(
      ([command]) => command === "replace_gain_automation",
    ),
  ).toBe(false);
  fireEvent.click(view.getByRole("button", { name: "Apply" }));
  await vi.waitFor(() =>
    expect(getLastCallArgs("replace_gain_automation")).toMatchObject({
      request: {
        target: "bus:gain owner",
        index: 0,
        expected_stamp: snapshot.parameter_target_stamp,
        expected_revision: snapshot.revision,
        expected: c,
        replacement: {
          points: [{ value: { Number: 0.5 } }, { value: { Number: 1 } }],
          value_mapping: c.value_mapping,
        },
      },
    }),
  );
  expect(c.points[0].value).toEqual({ Number: 0 });
  expect(
    view.getByLabelText("Quantization steps (blank: off)"),
  ).toHaveAttribute(
    "max",
    String(AUTOMATION_CURVES.inputQuantizationSteps.max),
  );
});

it("shows gain envelopes in the actual Timeline and Edit selects the correct rack owner", () => {
  const snapshot = fixture();
  store.applyAiSnapshot(snapshot);
  const view = render(() => (
    <>
      <Timeline />
      <Mixer />
    </>
  ));
  expect(
    view.container.querySelector('[data-mixer-node="bus:gain owner"]'),
  ).toBeNull();
  const preview = view.container.querySelector<HTMLElement>(
    '.gain-automation-preview[data-gain-owner="bus:gain owner"]',
  )!;
  expect(preview).not.toBeNull();
  expect(preview.querySelectorAll(".gain-envelope-anchor")).toHaveLength(2);
  expect(preview.querySelector(".gain-envelope-anchor")).toHaveAttribute("title", "Gain −∞ dB");
  expect(preview.querySelectorAll(".gain-envelope-anchor")[1]).toHaveAttribute("title", "Gain 0 dB");
  fireEvent.click(preview.querySelector(".gain-envelope-edit")!);
  expect(gainAutomationUi.request()).toMatchObject({
    target: "bus:gain owner",
    index: 0,
  });
  expect(panelStore.bottomMode()).toBe("mixer");
  expect(
    view.container.querySelector('[data-mixer-node="bus:gain owner"]'),
  ).not.toBeNull();
  const editor = view.container.querySelector(
    '[data-gain-editor-owner="bus:gain owner"]',
  )!;
  expect(editor.querySelector(".gain-envelope-summary")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});

it("keeps outside bracket anchors visible as context instead of dropping them", () => {
  const c = curve({
    time_range: {
      start: { bar: 1, beat: 2, subdivision: 0 },
      end: { bar: 1, beat: 3, subdivision: 0 },
    },
  });
  store.applyAiSnapshot(fixture(c));
  const view = render(() => <Timeline />);
  const preview = view.container.querySelector(".gain-automation-preview")!;
  expect(preview.querySelectorAll(".gain-envelope-context")).toHaveLength(2);
  expect(preview.querySelectorAll(".gain-envelope-anchor")).toHaveLength(2);
  expect(preview.querySelector("clipPath rect")?.getAttribute("width")).toBe(
    "24",
  );
});

it("clears exponential-only mapping curvature when switching to linear", async () => {
  const c = curve({
      value_mapping: { curve: "exponential", curvature: 2, scale: 2 },
    }),
    snapshot = fixture(c);
  store.applyAiSnapshot(snapshot);
  const base = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (command, args) =>
    command === "replace_gain_automation" ? snapshot : base?.(command, args),
  );
  invokeMock.mockClear();
  gainAutomationUi.open("bus:gain owner", 0);
  const view = render(() => <GainAutomationEditor target="bus:gain owner" />);
  expect(
    view.getByRole("spinbutton", { name: "Curvature" }),
  ).toBeInTheDocument();
  fireEvent.change(view.getByRole("combobox", { name: "Mapped values" }), {
    target: { value: "linear" },
  });
  expect(view.queryByRole("spinbutton", { name: "Curvature" })).toBeNull();
  fireEvent.click(view.getByRole("button", { name: "Apply" }));
  await vi.waitFor(() =>
    expect(getLastCallArgs("replace_gain_automation")).toMatchObject({
      request: {
        replacement: { value_mapping: { curve: "linear", scale: 2 } },
      },
    }),
  );
  expect(
    (
      getLastCallArgs("replace_gain_automation") as {
        request: { replacement: GainAutomation };
      }
    ).request.replacement.value_mapping,
  ).not.toHaveProperty("curvature");
});

it("does not mark a newer draft saved when an earlier Apply finishes", async () => {
  const snapshot = fixture(),
    posted = fixture();
  posted.project.buses[0].gain_automation![0].points[0].value = {
    Number: 0.25,
  };
  posted.revision = snapshot.revision + 1;
  store.applyAiSnapshot(snapshot);
  let finish!: (value: typeof snapshot) => void;
  const pending = new Promise<typeof snapshot>((resolve) => {
    finish = resolve;
  });
  const base = invokeMock.getMockImplementation();
  invokeMock.mockImplementation((command, args) =>
    command === "replace_gain_automation" ? pending : base?.(command, args),
  );
  gainAutomationUi.open("bus:gain owner", 0);
  const view = render(() => <GainAutomationEditor target="bus:gain owner" />);
  fireEvent.change(view.getByRole("spinbutton", { name: "Raw anchor 1" }), {
    target: { value: "0.25" },
  });
  fireEvent.click(view.getByRole("button", { name: "Apply" }));
  fireEvent.change(view.getByRole("spinbutton", { name: "Raw anchor 1" }), {
    target: { value: "0.5" },
  });
  finish(posted);
  await vi.waitFor(() =>
    expect(view.getByRole("button", { name: "Apply" })).not.toBeDisabled(),
  );
  expect(view.getByRole("spinbutton", { name: "Raw anchor 1" })).toHaveValue(
    0.5,
  );
  fireEvent.click(view.getByRole("button", { name: "Reload" }));
  expect(view.getByRole("spinbutton", { name: "Raw anchor 1" })).toHaveValue(
    0.25,
  );
  expect(view.getByRole("button", { name: "Apply" })).toBeDisabled();
});
