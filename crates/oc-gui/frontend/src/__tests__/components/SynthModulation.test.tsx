import { store } from "../../stores/project";
import { makeSnapshot } from "../setup";
import { openSynthRange, synthKnobTrigger } from "../helpers/synthRange";
/**
 * Modulation surface: per-route source attributes, macro learn and the reverse
 * lookup from a pushed control back to its routes.
 *
 * Every control is driven here, including the disabled
 * auxiliary-invert state, the keyboard learn path and the empty matrix.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { closeContextMenu } from "../../components/ContextMenu";
import { TooltipLayer } from "../../components/Tooltip";
import type { MacroLearnResult } from "../../stores/api";
import type { ParamValue, PresetDef } from "../../types/project";

function num(key: string, value: number): [string, ParamValue] {
  return [key, { Number: value }];
}

function preset(params: [string, ParamValue][] = []): PresetDef {
  return {
    name: "Modulation Test",
    source: "builtin",
    plugin: "builtin.open_synth",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params,
    effects: [],
  };
}

type LearnReply = { error: string | null; result: MacroLearnResult | null };

function renderEditor(options: {
  params?: [string, ParamValue][];
  learn?: (macroIndex: number, control: string) => Promise<LearnReply>;
} = {}) {
  store.applyAiSnapshot(makeSnapshot());
  const paramCalls: [string, string][] = [];
  const learnCalls: [number, string][] = [];
  const learn = options.learn ?? (async () => ({
    error: null,
    result: { status: "bound", slot: 2, destination: 0, source: 4 } as MacroLearnResult,
  }));
  render(() => (
    <>
      <OpenSynthEditor
        preset={preset(options.params)}
        setParam={async (name, value) => {
          paramCalls.push([name, value]);
          return null;
        }}
        setParams={async () => null}
        macroLearn={(macroIndex, control) => {
          learnCalls.push([macroIndex, control]);
          return learn(macroIndex, control);
        }}
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
      <TooltipLayer />
    </>
  ));
  return { paramCalls, learnCalls };
}

function control(key: string): HTMLElement {
  const element = document.querySelector<HTMLElement>(`[data-param="${key}"]`);
  expect(element, key).toBeTruthy();
  return element!;
}

function openView(label: "SOUND" | "MOD" | "FX" | "VOICE") {
  const button = [...document.querySelectorAll(".synth-view-tabs button")]
    .find((candidate) => candidate.textContent === label);
  expect(button, label).toBeTruthy();
  fireEvent.click(button!);
}

function openFilterPage() {
  const tab = [...document.querySelectorAll(".synth-subtabs button")]
    .find((candidate) => candidate.textContent === "FILTER");
  expect(tab, "FILTER subtab").toBeTruthy();
  fireEvent.click(tab!);
}

function selectRoute(slot: number) {
  const edge = document.querySelector(`.synth-route-edge[data-slot="${slot}"]`);
  expect(edge, `route edge ${slot}`).toBeTruthy();
  fireEvent.click(edge!);
}

function learnToggle(): HTMLButtonElement {
  return document.querySelector<HTMLButtonElement>(".synth-learn-toggle")!;
}

function learnStatus(): string {
  return document.querySelector(".synth-learn-status")?.textContent ?? "";
}

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function tooltipText(): string {
  return document.querySelector(".oc-tooltip")?.textContent ?? "";
}

function activeRouteParams(): [string, ParamValue][] {
  return [
    num("filter.cutoff", 800),
    num("mod.1.source", 4), // Macro 2
    num("mod.1.destination", 0), // Filter Cutoff
    num("mod.1.amount", 0.5),
  ];
}

afterEach(() => {
  closeContextMenu(false);
  cleanup();
  vi.useRealTimers();
});

describe("per-route modulation source attributes", () => {
  it("renders every attribute with its fallback when the patch omits them", () => {
    renderEditor({ params: activeRouteParams() });
    openView("MOD");
    // The slot row is the other way into the route inspector.
    fireEvent.click(document.querySelector('.synth-matrix-row[data-slot="1"]')!);
    for (const key of [
      "mod.1.bipolar",
      "mod.1.curve",
      "mod.1.slew_up",
      "mod.1.slew_down",
      "mod.1.aux",
      "mod.1.aux_invert",
    ]) {
      expect(control(key), key).toBeTruthy();
    }
    // Missing storage means the engine defaults: 0 curve/slew and aux off.
    expect(control("mod.1.bipolar").querySelector("input")?.checked).toBe(false);
    expect(control("mod.1.curve").querySelector(".synth-param-value")?.textContent).toBe("0.00");
    expect(control("mod.1.slew_up").querySelector(".synth-param-value")?.textContent).toBe("0.000s");
    expect(control("mod.1.aux").querySelector(".oc-dropdown-trigger")?.textContent).toContain("None");
  });

  it("commits bipolar, curve, both slews and the auxiliary source", async () => {
    const { paramCalls } = renderEditor({ params: activeRouteParams() });
    openView("MOD");
    selectRoute(1);

    fireEvent.click(control("mod.1.bipolar").querySelector("input")!);
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.bipolar", "1"]));

    const curve = openSynthRange(control("mod.1.curve"));
    fireEvent.input(curve, { target: { value: "0.75" } });
    fireEvent.change(curve, { target: { value: "0.75" } });
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.curve", "0.5"]));

    const slewUp = openSynthRange(control("mod.1.slew_up"));
    fireEvent.input(slewUp, { target: { value: "0.75" } });
    fireEvent.change(slewUp, { target: { value: "0.75" } });
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.slew_up", "0.211"]));

    const slewDown = openSynthRange(control("mod.1.slew_down"));
    fireEvent.input(slewDown, { target: { value: "0.5" } });
    fireEvent.change(slewDown, { target: { value: "0.5" } });
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.slew_down", "0.063"]));

    fireEvent.click(control("mod.1.aux").querySelector(".oc-dropdown-trigger")!);
    const macroTwo = [...document.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("Macro 2"));
    expect(macroTwo, "Macro 2 option").toBeTruthy();
    fireEvent.click(macroTwo!);
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.aux", "5"]));
  });

  it("disables aux invert with a visible, focusable reason until an aux is chosen", async () => {
    renderEditor({ params: activeRouteParams() });
    openView("MOD");
    selectRoute(1);
    const off = control("mod.1.aux_invert");
    const input = off.querySelector<HTMLInputElement>("input")!;
    expect(input.disabled).toBe(true);
    expect(off.getAttribute("tabindex")).toBe("0");
    expect(off.getAttribute("title")).toBe("Choose an auxiliary source first.");
    expect(off.querySelector(".synth-toggle-reason")).toBeTruthy();
    // Keyboard focus on the wrapper exposes the same reason as a tooltip.
    vi.useFakeTimers();
    off.focus();
    vi.advanceTimersByTime(0);
    expect(tooltipText()).toContain("Choose an auxiliary source first.");
    vi.useRealTimers();

    cleanup();
    const { paramCalls } = renderEditor({
      params: [...activeRouteParams(), num("mod.1.aux", 5)],
    });
    openView("MOD");
    selectRoute(1);
    const on = control("mod.1.aux_invert").querySelector<HTMLInputElement>("input")!;
    expect(on.disabled).toBe(false);
    on.focus();
    expect(document.activeElement).toBe(on);
    fireEvent.click(on);
    await waitFor(() => expect(paramCalls).toContainEqual(["mod.1.aux_invert", "1"]));
  });

  it("exposes an accessible name and a hint tooltip on each attribute", () => {
    renderEditor({ params: activeRouteParams() });
    openView("MOD");
    selectRoute(1);
    expect(control("mod.1.bipolar").querySelector("input")?.getAttribute("aria-label")).toBe("BIP");
    expect(control("mod.1.aux_invert").querySelector("input")?.getAttribute("aria-label")).toBe("INV");
    expect(synthKnobTrigger(control("mod.1.curve"))).toHaveAccessibleName("CURVE");
    expect(control("mod.1.aux").querySelector(".oc-dropdown-label")?.textContent).toBe("AUX");

    vi.useFakeTimers();
    fireEvent.mouseEnter(control("mod.1.curve"));
    vi.advanceTimersByTime(400);
    expect(tooltipText()).toContain("Shape the route signal");
  });
});

describe("macro learn", () => {
  it("keeps secondary activation and double-click reset from mutating learn targets", async () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    openView("SOUND");
    openFilterPage();
    const slider = synthKnobTrigger(control("filter.cutoff"));
    fireEvent.pointerDown(slider, { button: 2 });
    fireEvent.contextMenu(slider, { button: 2 });
    fireEvent.dblClick(slider);
    await flush();
    expect(learnCalls).toEqual([]);
    expect(paramCalls).toEqual([]);
  });

  it("leaves learn armed during arrow navigation without editing values or opening dropdowns", async () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    openView("SOUND");
    openFilterPage();
    const slider = synthKnobTrigger(control("filter.cutoff"));
    fireEvent.keyDown(slider, { key: "ArrowRight" });
    const trigger = control("filter.model").querySelector("button")!;
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    await flush();
    expect(document.querySelector('[role="listbox"]')).toBeNull();
    expect(learnCalls).toEqual([]);
    expect(paramCalls).toEqual([]);
  });

  it("arms, binds a control and reports the slot and destination", async () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    expect(learnToggle().getAttribute("aria-pressed")).toBe("true");
    expect(learnStatus()).toContain("Learn armed");

    openView("SOUND");
    openFilterPage();
    fireEvent.pointerDown(synthKnobTrigger(control("filter.cutoff")));
    await flush();

    expect(learnCalls).toEqual([[1, "filter.cutoff"]]);
    expect(paramCalls).toEqual([]);
    expect(synthKnobTrigger(control("filter.cutoff"))).toHaveAttribute("aria-expanded", "false");
    expect(learnStatus()).toContain("Bound Macro 1 → Filter Cutoff on route 02");
  });

  it("keeps pointer learning focus through compatibility blur so Escape cannot close the workspace", async () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    openView("SOUND");
    openFilterPage();
    const slider = synthKnobTrigger(control("filter.cutoff"));
    const workspaceShortcut = vi.fn();
    window.addEventListener("keydown", workspaceShortcut);
    try {
      fireEvent.pointerDown(slider, { button: 0 });
      slider.blur();
      fireEvent.click(slider, { detail: 1 });
      expect(slider).toHaveFocus();
      await flush();
      expect(learnCalls).toEqual([[1, "filter.cutoff"]]);
    expect(paramCalls).toEqual([]);
    expect(synthKnobTrigger(control("filter.cutoff"))).toHaveAttribute("aria-expanded", "false");
      fireEvent.keyDown(document.activeElement!, { key: "Escape" });
      expect(learnToggle().getAttribute("aria-pressed")).toBe("false");
      expect(workspaceShortcut).not.toHaveBeenCalled();
      expect(paramCalls).toEqual([]);
    } finally {
      window.removeEventListener("keydown", workspaceShortcut);
    }
  });

  it("learns from the keyboard and disarms with Escape", async () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    openView("SOUND");
    openFilterPage();
    const slider = synthKnobTrigger(control("filter.cutoff"));
    slider.focus();
    fireEvent.keyDown(slider, { key: "Enter" });
    await flush();
    expect(learnCalls).toEqual([[1, "filter.cutoff"]]);
    expect(paramCalls).toEqual([]);
    expect(synthKnobTrigger(control("filter.cutoff"))).toHaveAttribute("aria-expanded", "false");

    fireEvent.keyDown(document.querySelector(".synth-workbench")!, { key: "Escape" });
    expect(learnToggle().getAttribute("aria-pressed")).toBe("false");
  });

  it("says a control has no destination instead of writing a route", async () => {
    const { learnCalls, paramCalls } = renderEditor({
      params: activeRouteParams(),
      learn: async () => ({
        error: null,
        result: { status: "unsupported", slot: null, destination: null, source: null },
      }),
    });
    fireEvent.click(learnToggle());
    openView("VOICE");
    fireEvent.pointerDown(synthKnobTrigger(control("unison.width")));
    await flush();
    expect(learnCalls).toEqual([[1, "unison.width"]]);
    expect(paramCalls).toEqual([]);
    expect(synthKnobTrigger(control("unison.width"))).toHaveAttribute("aria-expanded", "false");
    expect(learnStatus()).toContain("unison.width");
    expect(learnStatus()).toContain("no modulation destination");
  });

  it("reports a full matrix and an unbind without inventing a slot", async () => {
    const replies: MacroLearnResult[] = [
      { status: "full", slot: null, destination: 0, source: 4 },
      { status: "unbound", slot: 1, destination: 0, source: 4 },
    ];
    const { learnCalls, paramCalls } = renderEditor({
      params: activeRouteParams(),
      learn: async () => ({ error: null, result: replies.shift() ?? null }),
    });
    fireEvent.click(learnToggle());
    openView("SOUND");
    openFilterPage();
    fireEvent.pointerDown(synthKnobTrigger(control("filter.cutoff")));
    await flush();
    expect(learnStatus()).toContain("All 64 modulation routes are in use");
    fireEvent.pointerDown(synthKnobTrigger(control("filter.cutoff")));
    await flush();
    expect(learnStatus()).toContain("Unbound Macro 1 → Filter Cutoff on route 01");
    expect(learnCalls).toHaveLength(2);
    expect(paramCalls).toEqual([]);
  });

  it("leaves the learn bar alone so a second click can disarm", () => {
    const { learnCalls, paramCalls } = renderEditor({ params: activeRouteParams() });
    fireEvent.click(learnToggle());
    fireEvent.click(learnToggle());
    expect(learnToggle().getAttribute("aria-pressed")).toBe("false");
    expect(learnCalls).toEqual([]);
    expect(paramCalls).toEqual([]);
  });
});

describe("reverse lookup on a pushed control", () => {
  it("marks the control, names its routes in a tooltip and lights the edge", () => {
    renderEditor({ params: activeRouteParams() });
    openView("SOUND");
    openFilterPage();
    const cutoff = control("filter.cutoff");
    expect(cutoff.classList.contains("synth-modulated")).toBe(true);
    expect(cutoff.querySelector(".synth-modulated-dot")).toBeTruthy();

    vi.useFakeTimers();
    fireEvent.mouseEnter(cutoff);
    vi.advanceTimersByTime(400);
    expect(tooltipText()).toContain("Modulated by");
    expect(tooltipText()).toContain("Filter Cutoff");
    expect(tooltipText()).toContain("Macro 2");
    expect(tooltipText()).toContain("+50%");
    // The single-view layout shows the routing graph as a hover preview.
    expect(document.querySelector(".synth-mod-preview .synth-route-edge.highlighted")).toBeTruthy();

    fireEvent.mouseLeave(cutoff);
    expect(document.querySelector(".synth-mod-preview")).toBeNull();
  });

  it("leaves an unmodulated control unmarked", () => {
    renderEditor({ params: activeRouteParams() });
    openView("SOUND");
    openFilterPage();
    expect(control("filter.resonance").classList.contains("synth-modulated")).toBe(false);
    expect(control("filter.resonance").querySelector(".synth-modulated-dot")).toBeNull();
  });

  it("does not cover an unmodulated knob editor with an empty routing tooltip", () => {
    renderEditor();
    openView("SOUND");
    openFilterPage();
    const cutoff = control("filter.cutoff");
    vi.useFakeTimers();
    fireEvent.mouseEnter(cutoff);
    vi.advanceTimersByTime(400);
    expect(tooltipText()).not.toContain("No modulation routes yet");
    expect(document.querySelector(".synth-mod-preview")).toBeNull();
    const input = openSynthRange(cutoff);
    fireEvent.focusIn(input);
    vi.advanceTimersByTime(400);
    expect(tooltipText()).not.toContain("No modulation routes yet");
    expect(document.querySelector(".synth-mod-preview")).toBeNull();
    expect(synthKnobTrigger(cutoff).getAttribute("aria-expanded")).toBe("true");
  });

  it("shows the empty matrix state when no route is active", () => {
    renderEditor();
    openView("MOD");
    expect(document.querySelector(".synth-route-empty")?.textContent).toContain("No modulation routes yet.");
    expect(document.querySelector(".synth-route-inspector")).toBeNull();
  });
});
