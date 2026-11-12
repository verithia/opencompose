/**
 * Live synth waveform: samples come from the backend, the debounce coalesces
 * slider bursts, and a failed preview falls back to the static drawing.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { invokeMock } from "../setup";
import type { ParamValue, PresetDef } from "../../types/project";
import {
  PREVIEW_DEBOUNCE_MS,
  SynthWaveDisplay,
  type WaveformPreview,
} from "../../components/synth/SynthWaveDisplay";

const PRESET: PresetDef = {
  name: "OpenSynth Init",
  source: "builtin",
  plugin: "builtin.open_synth",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  params: [["osc.a.waveform", { Number: 1 } as ParamValue]],
  effects: [],
};

function preview(overrides: Partial<WaveformPreview> = {}): WaveformPreview {
  return {
    // A flat offset no hand-drawn sine/saw/square would produce.
    mono: [0.5, 0.5, 0.5],
    traces: [
      [0.5, 0.5, 0.5],
      [0.5, 0.5, 0.5],
    ],
    more_voices: false,
    voices: 2,
    peak: 0.5,
    rms: 0.5,
    samples: 3,
    sample_rate: 48_000,
    note: 48,
    sync_resets: 0,
    ...overrides,
  };
}

function previewCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "synth_waveform_preview");
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "synth_waveform_preview") return preview();
    return undefined;
  });
});

describe("SynthWaveDisplay", () => {
  it("draws the polyline from the returned samples and overlays unison traces", async () => {
    const { container } = render(() => (
      <SynthWaveDisplay preset={PRESET} drafts={{}} oscillator={0} waveform={1} label="Oscillator A" />
    ));
    await waitFor(() => {
      expect(container.querySelector("svg[data-fallback='false']")).toBeTruthy();
    });
    const line = container.querySelector("polyline.synth-graph-line");
    expect(line?.getAttribute("points")).toBe("0.00,8.00 50.00,8.00 100.00,8.00");
    // The old hand-drawn saw at these three phases is 42, 25, 8, not flat.
    expect(line?.getAttribute("points")).not.toContain("0,42");
    expect(container.querySelectorAll("polyline.synth-wave-voice")).toHaveLength(2);
    expect(container.textContent).toContain("pk 0.50 rms 0.50");
  });

  it("falls back to the static drawing when the command errors", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "synth_waveform_preview") throw new Error("preview offline");
      return undefined;
    });
    const { container } = render(() => (
      <SynthWaveDisplay preset={PRESET} drafts={{}} oscillator={0} waveform={1} label="Oscillator A" />
    ));
    await waitFor(() => {
      expect(container.querySelector("svg[data-fallback='true']")).toBeTruthy();
      expect(container.textContent).toContain("static");
    });
    const line = container.querySelector("polyline.synth-graph-line");
    const points = line?.getAttribute("points") ?? "";
    expect(points.startsWith("0,42.00 ")).toBe(true);
    expect(points.endsWith(" 100,8.00")).toBe(true);
    expect(points).not.toBe("0.00,8.00 50.00,8.00 100.00,8.00");
  });

  it("issues one preview call for a burst of parameter changes", async () => {
    const [drafts, setDrafts] = createSignal<Record<string, number>>({});
    const { unmount } = render(() => (
      <SynthWaveDisplay preset={PRESET} drafts={drafts()} oscillator={0} waveform={1} label="Oscillator A" />
    ));
    await waitFor(() => expect(previewCalls()).toHaveLength(1));
    invokeMock.mockClear();

    for (let step = 1; step <= 4; step += 1) {
      setDrafts({ "osc.a.warp_amount": step / 10 });
      await new Promise((resolve) => setTimeout(resolve, 15));
    }
    await new Promise((resolve) => setTimeout(resolve, PREVIEW_DEBOUNCE_MS + 60));
    expect(previewCalls()).toHaveLength(1);
    unmount();
  });
});
