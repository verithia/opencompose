/**
 * Mixer meters: peak/RMS bars, the clip hold, the stopped reason, and the
 * master's LUFS-S, correlation, goniometer, mono check and ten-second trace.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, expectInvoked } from "../setup";
import { store } from "../../stores/project";
import { clearMeterHistory, masterHistory } from "../../stores/meters";
import { ChannelStrips } from "../../components/ChannelStrips";
import { METER_HISTORY_CAPACITY, METER_HISTORY_SECONDS } from "../../utils/meterHistory";

function projectSnapshot() {
  return makeSnapshot({
    tracks: [makeTrack("Piano", 0)],
    buses: [],
    master: { effects: [], gain_automation: [], loudness_target: null, volume_db: 0 },
  });
}

interface MeterOptions {
  available?: boolean;
  playing?: boolean;
  reason?: string | null;
  masterChannels?: number;
  mono?: { requested: boolean; applied: boolean };
  trackPeak?: number | null;
  trackRms?: number | null;
  trackClipped?: boolean;
  lufs?: number | null;
  correlation?: number | null;
  goniometer?: number[];
}

function meterSnapshot(options: MeterOptions = {}) {
  return {
    available: options.available ?? true,
    reason: options.reason ?? null,
    playing: options.playing ?? true,
    master_channels: options.masterChannels ?? 2,
    mono: options.mono ?? { requested: false, applied: false },
    master: {
      gain_reduction_db: 0,
      output_peak_db: -3,
      output_rms_db: -9,
      clipped: false,
      effects: [],
      short_term_lufs: options.lufs === undefined ? -14.24 : options.lufs,
      short_term_window_seconds: 3,
      correlation: options.correlation === undefined ? 0.75 : options.correlation,
      goniometer: options.goniometer ?? [-0.5, 0.5, 0.25, 0.25],
    },
    tracks: [{
      index: 0,
      gain_reduction_db: 0,
      output_peak_db: options.trackPeak === undefined ? -6 : options.trackPeak,
      output_rms_db: options.trackRms === undefined ? -12 : options.trackRms,
      clipped: options.trackClipped ?? false,
      effects: [],
    }],
    buses: [],
  };
}

let current: MeterOptions = {};

function mockBackend() {
  invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "meter_snapshot") return meterSnapshot(current);
    if (command === "reset_meter_clip" || command === "set_monitor_mono") {
      return command === "set_monitor_mono"
        ? { requested: args?.enabled === true, applied: false }
        : undefined;
    }
    if (command === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (command === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    return projectSnapshot();
  });
}

beforeEach(async () => {
  current = {};
  clearMeterHistory();
  invokeMock.mockClear();
  mockBackend();
  await store.initProject();
});

const renderStrips = () =>
  render(() => <ChannelStrips selectedNode="track:0" onSelectNode={() => {}} />);

describe("channel meters", () => {
  it("renders peak and RMS bars for a track from the snapshot", async () => {
    const { container, getByText } = renderStrips();
    await vi.waitFor(() => {
      expect(getByText("Peak -6.0 dBFS")).toBeTruthy();
    });
    expect(getByText("RMS -12.0 dBFS")).toBeTruthy();
    const strip = container.querySelector('[data-channel-meter="track:0"]')!;
    expect(strip.getAttribute("data-meter-live")).toBe("true");
    const peakBar = strip.querySelector("[data-meter-peak-bar]") as HTMLElement;
    const rmsBar = strip.querySelector("[data-meter-rms-bar]") as HTMLElement;
    // -60 dBFS is empty and 0 dBFS is full, so -6 fills 90% and -12 fills 80%.
    expect(peakBar.style.width).toBe("90%");
    expect(rmsBar.style.width).toBe("80%");
  });

  it("shows a dash, never a level, before the first block arrives", async () => {
    current = { trackPeak: null, trackRms: null };
    const { container, getByText } = renderStrips();
    await vi.waitFor(() => {
      expect(getByText("Peak — dBFS")).toBeTruthy();
    });
    expect(getByText("RMS — dBFS")).toBeTruthy();
    const strip = container.querySelector('[data-channel-meter="track:0"]')!;
    // A silent tap is live metering with no reading, not a stopped meter.
    expect(strip.getAttribute("data-meter-live")).toBe("true");
  });

  it("says meters need playback when the transport is stopped", async () => {
    current = { playing: false };
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const strip = container.querySelector('[data-channel-meter="track:0"]')!;
      expect(strip.querySelector("[data-meter-note]")!.textContent).toBe(
        "Meters need playback.",
      );
    });
    const strip = container.querySelector('[data-channel-meter="track:0"]')!;
    expect(strip.getAttribute("data-meter-live")).toBe("false");
    expect(strip.querySelector("[data-meter-note]")).toBeTruthy();
  });

  it("reports the engine's reason when metering is unavailable", async () => {
    current = { available: false, reason: "audio output stopped" };
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const strip = container.querySelector('[data-channel-meter="track:0"]')!;
      expect(strip.querySelector("[data-meter-note]")!.textContent).toBe(
        "Metering unavailable: audio output stopped.",
      );
    });
  });

  it("holds a clip and clears it through the reset command", async () => {
    current = { trackClipped: true };
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const clip = container.querySelector(
        '[data-channel-meter="track:0"] [data-meter-clip]',
      )!;
      expect(clip.getAttribute("aria-pressed")).toBe("true");
      expect(clip.classList.contains("clip-held")).toBe(true);
    });
    const clip = container.querySelector(
      '[data-channel-meter="track:0"] [data-meter-clip]',
    )!;
    fireEvent.click(clip);
    await vi.waitFor(() => {
      expectInvoked("reset_meter_clip");
    });
  });
});

describe("master monitoring", () => {
  it("reads LUFS-S over the stated window", async () => {
    const { container, getByText } = renderStrips();
    await vi.waitFor(() => {
      expect(getByText("LUFS-S -14.2")).toBeTruthy();
    });
    const lufs = container.querySelector("[data-master-lufs]")!;
    expect(lufs.getAttribute("data-lufs-window")).toBe("3");
  });

  it("says the window is filling instead of showing a number", async () => {
    current = { lufs: null };
    const { getByText } = renderStrips();
    await vi.waitFor(() => {
      expect(getByText("Measuring — the 3 s window is still filling.")).toBeTruthy();
    });
  });

  it("formats the correlation readout", async () => {
    current = { correlation: -0.425 };
    const { container } = renderStrips();
    await vi.waitFor(() => {
      expect(container.querySelector("[data-master-correlation]")!.textContent).toContain(
        "Corr -0.42",
      );
    });
    current = { correlation: 1 };
    await vi.waitFor(() => {
      expect(container.querySelector("[data-master-correlation]")!.textContent).toContain(
        "Corr +1.00",
      );
    });
    current = { correlation: null };
    await vi.waitFor(() => {
      expect(container.querySelector("[data-master-correlation]")!.textContent).toContain(
        "Corr —",
      );
    });
  });

  it("draws the goniometer from the snapshot's stereo pairs", async () => {
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const scope = container.querySelector("[data-goniometer]")!;
      expect(scope.getAttribute("data-goniometer-points")).toBe("2");
      expect(scope.querySelectorAll("circle").length).toBe(2);
    });
  });

  it("disables MONO for a non-stereo master and says why", async () => {
    current = { masterChannels: 1 };
    const { container, getByText } = renderStrips();
    await vi.waitFor(() => {
      const button = container.querySelector("[data-mono-button]") as HTMLButtonElement;
      expect(button.disabled).toBe(true);
      expect(button.title).toBe("Mono check needs a stereo master.");
    });
    expect(getByText("Mono check needs a stereo master.")).toBeTruthy();
  });

  it("disables MONO while the transport is stopped", async () => {
    current = { playing: false };
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const button = container.querySelector("[data-mono-button]") as HTMLButtonElement;
      expect(button.disabled).toBe(true);
      expect(button.title).toBe("Mono check needs playback.");
    });
  });

  it("applies MONO and shows Applying… until a chunk lands", async () => {
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const button = container.querySelector("[data-mono-button]") as HTMLButtonElement;
      expect(button.disabled).toBe(false);
      expect(button.textContent).toBe("MONO");
    });
    fireEvent.click(container.querySelector("[data-mono-button]")!);
    await vi.waitFor(() => {
      expectInvoked("set_monitor_mono", { enabled: true });
    });
    // The engine has the request but no chunk has folded it yet.
    current = { mono: { requested: true, applied: false } };
    await vi.waitFor(() => {
      const button = container.querySelector("[data-mono-button]") as HTMLButtonElement;
      expect(button.textContent).toBe("Applying…");
      expect(button.getAttribute("aria-pressed")).toBe("true");
    });
    current = { mono: { requested: true, applied: true } };
    await vi.waitFor(() => {
      const button = container.querySelector("[data-mono-button]") as HTMLButtonElement;
      expect(button.textContent).toBe("MONO");
      expect(button.classList.contains("mono-active")).toBe(true);
    });
  });

  it("reports a refused mono check as a failure note", async () => {
    const { container, getByText } = renderStrips();
    await vi.waitFor(() => {
      expect(container.querySelector("[data-mono-button]")).toBeTruthy();
    });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "set_monitor_mono") throw new Error("Mono check needs a stereo master.");
      if (command === "meter_snapshot") return meterSnapshot(current);
      return projectSnapshot();
    });
    fireEvent.click(container.querySelector("[data-mono-button]")!);
    await vi.waitFor(() => {
      expect(getByText("Mono check failed: Mono check needs a stereo master.")).toBeTruthy();
    });
  });

  it("draws the last ten seconds of level history", async () => {
    const { container } = renderStrips();
    await vi.waitFor(() => {
      const trace = container.querySelector("[data-master-history]")!;
      expect(trace.getAttribute("data-history-capacity")).toBe(String(METER_HISTORY_CAPACITY));
      expect(trace.getAttribute("data-history-samples")).toBe(String(masterHistory().length));
      expect(masterHistory().length).toBeGreaterThan(0);
      expect(trace.querySelectorAll("polyline").length).toBeGreaterThan(0);
    });
    expect(METER_HISTORY_SECONDS).toBe(10);
    expect(container.querySelector("[data-history-window]")!.textContent).toContain("last 10 s");
  });
});
