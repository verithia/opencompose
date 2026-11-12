import { store } from "../../stores/project";
import { makeSnapshot } from "../setup";
import { openSynthRange, synthRangeInput, synthKnobTrigger, synthPointer } from "../helpers/synthRange";
import { afterEach, describe, expect, it } from "vitest";
import { createSignal } from "solid-js";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { i18n } from "../../stores/i18n";
import { paramLabel, specForConcreteKey } from "../../data/synthCatalog";
import { openDialogMock } from "../setup";
import { uiScaleStore } from "../../stores/uiScale";
import type { PresetDef } from "../../types/project";

const PRESET: PresetDef = {
  name: "Catalog Test",
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

function setup(refreshPreset = false) {
  store.applyAiSnapshot(makeSnapshot());
  const calls: [string, string][] = [];
  const [preset, setPreset] = createSignal(PRESET);
  render(() => (
    <OpenSynthEditor
      preset={preset()}
      setParam={async (name, value) => {
        calls.push([name, value]);
        if (refreshPreset) setPreset(current => ({
          ...current,
          params: [...current.params.filter(([key]) => key !== name), [name, { Number: Number(value) }]],
        }));
        return null;
      }}
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
  return calls;
}

function control(key: string): HTMLElement | null {
  return document.querySelector(`[data-param="${key}"]`);
}

/** The filter controls live on the FILTER sub-page of the sound view. */
function openFilterPage() {
  const tab = Array.from(document.querySelectorAll(".synth-subtabs button"))
    .find((button) => button.textContent === "FILTER");
  expect(tab, "FILTER subtab").toBeTruthy();
  fireEvent.click(tab!);
}

afterEach(() => { cleanup(); i18n.setLocale("en"); uiScaleStore.set(1); openDialogMock.mockReset(); });

describe("catalog-driven synth controls", () => {
  it("commits a relative knob drag released outside the control", async () => {
    const calls = setup(true);
    const row = control("osc.a.level")!;
    const knob = synthKnobTrigger(row);
    synthPointer(knob, "pointerdown", 41, 200);
    expect(knob.getAttribute("aria-expanded")).toBe("true");
    synthPointer(window, "pointermove", 41, 184);
    synthPointer(window, "pointerup", 41, 184);
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.88"]));
    expect(knob.getAttribute("aria-expanded")).toBe("false");
    expect(Number(row.dataset.automationValue)).toBeCloseTo(0.88);
  });

  it("accepts fresh accessibility input after pointer and keyboard edits on the same mounted range", async () => {
    const calls = setup(true);
    const input = openSynthRange(control("osc.a.level")!);
    fireEvent.pointerDown(input);
    fireEvent.input(input, { target: { value: "0.3" } });
    fireEvent.pointerUp(input);
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.3"]));
    fireEvent.input(input, { target: { value: "0.4" } });
    fireEvent.change(input, { target: { value: "0.4" } });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.4"]));
    fireEvent.keyDown(input, { key: "ArrowRight" });
    await waitFor(() => {
      expect(calls.at(-1)?.[0]).toBe("osc.a.level");
      expect(Number(calls.at(-1)?.[1])).toBeCloseTo(0.41, 12);
    });
    fireEvent.input(input, { target: { value: "0.5" } });
    fireEvent.change(input, { target: { value: "0.5" } });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.5"]));
    expect(synthRangeInput(control("osc.a.level")!)).toBe(input);
  });

  it("reverts an interrupted draft and ignores its late change event", async () => {
    const calls = setup(true);
    const row = control("osc.a.level")!;
    const input = openSynthRange(row);
    const before = Number(row.dataset.automationValue);
    fireEvent.input(input, { target: { value: "1" } });
    expect(Number(row.dataset.automationValue)).not.toBe(before);
    fireEvent.pointerCancel(input);
    fireEvent.change(input, { target: { value: "1" } });
    await Promise.resolve();
    expect(Number(row.dataset.automationValue)).toBe(before);
    expect(calls).toEqual([]);
  });

  it("clears an uncommitted draft on window blur", () => {
    const calls = setup();
    const row = control("osc.a.level")!;
    const before = Number(row.dataset.automationValue);
    fireEvent.input(openSynthRange(row), { target: { value: "1" } });
    fireEvent.blur(window);
    expect(Number(row.dataset.automationValue)).toBe(before);
    expect(calls).toEqual([]);
  });

  it("recovers step-slider focus after native compatibility blur and uses canonical key steps", async () => {
    const calls = setup();
    fireEvent.click(screen.getByRole("button", { name: "MOD" }));
    const input = control("step.1")!.querySelector<HTMLInputElement>("input")!;
    fireEvent.pointerDown(input);
    input.blur();
    fireEvent.pointerUp(input);
    fireEvent.click(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "End" });
    await waitFor(() => expect(calls).toContainEqual(["step.1", String(specForConcreteKey("step.1")!.max)]));
  });

  it("restores range focus after the native mouse compatibility phase", async () => {
    const calls = setup();
    const input = openSynthRange(control("osc.a.level")!);
    fireEvent.pointerDown(input);
    fireEvent.mouseDown(input);
    // A WebKit native range gesture can clear programmatic pointerdown focus
    // during its mouse compatibility phase. Keyboard input follows release.
    input.blur();
    fireEvent.pointerUp(input);
    fireEvent.mouseUp(input);
    fireEvent.click(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "ArrowDown" });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.77"]));
  });

  it("keeps the focused control mounted across committed preset snapshots", async () => {
    const calls = setup(true);
    const input = openSynthRange(control("osc.a.level")!);
    fireEvent.pointerDown(input);
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.79"]));
    expect(synthRangeInput(control("osc.a.level")!)).toBe(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", "0.8"]));
  });

  it("focuses a touched range so keyboard edits reach the same control", async () => {
    const calls = setup();
    const input = openSynthRange(control("osc.a.level")!);
    fireEvent.pointerDown(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    await waitFor(() => expect(calls).toContainEqual([
      "osc.a.level", String(specForConcreteKey("osc.a.level")!.max),
    ]));
  });

  it.each([["en", 0.7], ["zh-Hans", 1.5], ["zh-Hant", 1]] as const)(
    "uses canonical envelope endpoints and resets at locale %s and UI scale %s", async (locale, scale) => {
    i18n.setLocale(locale);
    uiScaleStore.set(scale);
    const calls = setup();
    const tab = Array.from(document.querySelectorAll(".synth-subtabs button"))
      .find((button) => button.textContent === i18n.t("synth.ui.ampTab").toUpperCase())!;
    fireEvent.click(tab);
    for (const key of ["amp.attack", "amp.release", "filter_env.attack", "filter_env.release", "lfo.rate"]) {
      const spec = specForConcreteKey(key)!;
      const row = control(key)!;
      const input = openSynthRange(row);
      expect(synthKnobTrigger(row)).toHaveAccessibleName(paramLabel(key));
      expect(Number(row.dataset.paramMin)).toBe(spec.min);
      expect(Number(row.dataset.paramMax)).toBe(spec.max);
      expect(Number(row.dataset.paramStep)).toBe(spec.step);
      fireEvent.input(input, { target: { value: "1" } });
      fireEvent.change(input, { target: { value: "1" } });
      await waitFor(() => expect(calls).toContainEqual([key, String(spec.max)]));
      fireEvent.keyDown(input, { key: "Home" });
      await waitFor(() => expect(calls).toContainEqual([key, String(spec.min)]));
      fireEvent.dblClick(input);
      await waitFor(() => expect(calls).toContainEqual([key, String(spec.default)]));
    }
  });

  it("keeps draft pointer updates separate from the committed schema value", async () => {
    const calls = setup();
    const input = openSynthRange(control("osc.a.level")!);
    const spec = specForConcreteKey("osc.a.level")!;
    fireEvent.input(input, { target: { value: "1" } });
    expect(calls).toEqual([]);
    fireEvent.change(input, { target: { value: "1" } });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.level", String(spec.max)]));
  });

  it("shows concrete oscillator defaults and uses the step-family metadata", async () => {
    const calls = setup();
    for (const key of ["osc.a.level", "osc.b.level", "osc.c.level"]) {
      expect(Number(control(key)!.dataset.automationValue)).toBe(specForConcreteKey(key)!.default);
    }
    fireEvent.click(screen.getByRole("button", { name: i18n.t("synth.ui.mod").toUpperCase() }));
    const tab = Array.from(document.querySelectorAll(".synth-module-tabs button"))
      .find((button) => button.textContent === i18n.t("synth.ui.steps").toUpperCase())!;
    fireEvent.click(tab);
    const spec = specForConcreteKey("step.16")!;
    const input = control("step.16")!.querySelector<HTMLInputElement>('input[type="range"]')!;
    expect(Number(input.min)).toBe(spec.min);
    expect(Number(input.max)).toBe(spec.max);
    expect(Number(input.step)).toBe(spec.step);
    fireEvent.input(input, { target: { value: String(spec.max) } });
    fireEvent.change(input, { target: { value: String(spec.max) } });
    await waitFor(() => expect(calls).toContainEqual(["step.16", String(spec.max)]));
    fireEvent.dblClick(input);
    await waitFor(() => expect(calls).toContainEqual(["step.16", String(spec.default)]));
  });

  it("renders the previously missing controls", () => {
    setup();
    for (const key of [
      "osc.a.warp_var",
      "osc.b.warp_var",
      "osc.c.warp_var",
      "osc.a.warp2_mode",
      "osc.a.warp2_amount",
      "osc.a.warp2_var",
      "osc.a.pan",
    ]) {
      expect(control(key), key).not.toBeNull();
    }
    openFilterPage();
    for (const key of ["filter.wet", "filter2.wet"]) {
      expect(control(key), key).not.toBeNull();
    }
  });

  it("commits a warp variation from the catalog range", async () => {
    const calls = setup();
    const input = openSynthRange(control("osc.a.warp_var")!);
    expect(input).toBeTruthy();
    fireEvent.input(input!, { target: { value: "0.5" } });
    fireEvent.change(input!, { target: { value: "0.5" } });
    await waitFor(() => expect(calls).toContainEqual(["osc.a.warp_var", "0.5"]));
  });

  it("offers all twelve filter models", () => {
    setup();
    openFilterPage();
    fireEvent.click(control("filter.model")!.querySelector("button")!);
    expect(document.querySelectorAll('[role="option"]')).toHaveLength(12);
  });
});

describe("localized OpenSynth controls", () => {
  it("switches visible tabs and effects labels in all supported languages", () => {
    setup();
    const workbench = document.querySelector(".synth-workbench")!;
    for (const locale of i18n.locales) {
      i18n.setLocale(locale);
      fireEvent.click(screen.getByRole("button", { name: i18n.t("synth.ui.voice").toUpperCase() }));
      expect(within(workbench as HTMLElement).getByText(i18n.t("synth.ui.voicePolyphony").toUpperCase())).toBeVisible();
      expect(workbench).not.toHaveTextContent(/deterministic release|POLYBLEP|PER-VOICE SVF/);
      fireEvent.click(screen.getByRole("button", { name: i18n.t("synth.ui.fx").toUpperCase() }));
      expect(screen.getByRole("button", { name: i18n.t("synth.ui.fxAdd") })).toBeVisible();
      expect(workbench).not.toHaveTextContent(/voice sum|output clip/);
    }
  });

  it("uses localized choices without changing the oscillator value written", async () => {
    i18n.setLocale("zh-Hans");
    const calls = setup();
    fireEvent.click(control("osc.a.waveform")!.querySelector("button")!);
    fireEvent.click(await screen.findByRole("option", { name: "正弦波" }));
    await waitFor(() => expect(calls).toContainEqual(["osc.a.waveform", "0"]));
    expect(screen.getByText("Catalog Test")).toBeVisible();
  });

  it("translates the patch menu and empty-name validation", async () => {
    i18n.setLocale("zh-Hans");
    setup();
    fireEvent.click(screen.getByRole("button", { name: "音色操作" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "重命名…" }));
    const dialog = screen.getByRole("dialog", { name: "重命名音色" });
    fireEvent.input(within(dialog).getByRole("textbox"), { target: { value: "" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "重命名" }));
    expect(within(dialog).getByRole("alert")).toHaveTextContent("请输入名称");
  });

  it("shows one localized import error when the file picker is unavailable", async () => {
    i18n.setLocale("zh-Hans");
    openDialogMock.mockRejectedValueOnce(new Error("Test picker failure"));
    setup();
    fireEvent.click(screen.getByText("加载", { selector: ".synth-load-button" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("无法导入预设：");
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });

  it("does not show internal write targets in the LFO bank", () => {
    i18n.setLocale("zh-Hans");
    setup();
    fireEvent.click(screen.getByRole("button", { name: "调制" }));
    fireEvent.click(screen.getByRole("button", { name: "低频振荡器" }));
    const bank = screen.getByLabelText("低频振荡器编辑器");
    expect(bank).not.toHaveTextContent(/writes lfo|retrigger mirror/);
    expect(within(bank).getByText("低频振荡器组")).toBeVisible();
  });
});
