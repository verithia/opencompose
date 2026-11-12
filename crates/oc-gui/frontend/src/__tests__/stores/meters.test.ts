import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock } from "../setup";
import {
  clearMeterHistory,
  masterHistory,
  meterSnapshot,
  pollMeters,
  resetMeterClip,
  selectChannelMeter,
  selectMasterMonitor,
  selectMeter,
  setMonitorMono,
  startMeterPolling,
} from "../../stores/meters";
import { METER_HISTORY_CAPACITY } from "../../utils/meterHistory";

function snapshot() {
  return {
    available: true,
    playing: true,
    master: {
      gain_reduction_db: 2,
      output_peak_db: -6,
      effects: [2],
      effects_max_gain_reduction_db: [6],
      effects_ceiling_margin_db: [0.4],
    },
    tracks: [{
      index: 0,
      gain_reduction_db: 4,
      input_peak_db: -12,
      output_peak_db: -18,
      effect_bands: [[-3.5, 0, 1.25]],
      effects: [1, 3.5],
      effects_max_gain_reduction_db: [1, 5],
      effects_ceiling_margin_db: [null, -1.5],
    }],
    buses: [{ name: "fx", gain_reduction_db: 1.5, effects: [1.5] }],
    spectrum: { bands: [-60, Number.NaN, 3] },
  };
}

beforeEach(() => {
  invokeMock.mockReset();
  clearMeterHistory();
});

describe("meter store", () => {
  it("normalizes a snapshot and resolves track, bus, and master rows", async () => {
    invokeMock.mockImplementation(async (command) =>
      command === "meter_snapshot" ? snapshot() : {},
    );

    await pollMeters();

    const meters = meterSnapshot();
    expect(meters?.available).toBe(true);
    expect(meters?.playing).toBe(true);
    expect(selectMeter(meters, "track:0", 1)).toEqual({
      available: true,
      playing: true,
      gainReductionDb: 3.5,
      maxGainReductionDb: 5,
      ceilingMarginDb: -1.5,
      inputPeakDb: -12,
      outputPeakDb: -18,
    });
    // Missing effect rows fall back to the chain total.
    expect(selectMeter(meters, "track:0", 9)?.gainReductionDb).toBe(4);
    expect(selectMeter(meters, "track:0", 9)?.maxGainReductionDb).toBe(4);
    expect(selectMeter(meters, "track:0", 9)?.ceilingMarginDb).toBeNull();
    expect(selectMeter(meters, "bus:fx", 0)?.gainReductionDb).toBe(1.5);
    expect(selectMeter(meters, "master", 0)?.outputPeakDb).toBe(-6);
    expect(selectMeter(meters, "master", 0)?.maxGainReductionDb).toBe(6);
    expect(selectMeter(meters, "master", 0)?.ceilingMarginDb).toBe(0.4);
    expect(selectMeter(meters, "track:7", 0)).toBeNull();
    expect(meters?.tracks[0].effect_bands).toEqual([[-3.5, 0, 1.25]]);
    expect(meters?.spectrum?.bands).toEqual([-60, -160, 0]);
  });

  it("clamps non-finite values and clears bad shapes", async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command !== "meter_snapshot") return {};
      return {
        available: true,
        playing: false,
        master: { gain_reduction_db: Number.NaN, effects: [Number.POSITIVE_INFINITY] },
        tracks: [{ index: 0, gain_reduction_db: 1e9, input_peak_db: -1e9, effects: [] }],
        buses: [],
      };
    });

    await pollMeters();
    expect(selectMeter(meterSnapshot(), "master", 0)?.gainReductionDb).toBe(0);
    expect(selectMeter(meterSnapshot(), "track:0", 0)?.gainReductionDb).toBe(100);
    // The engine clamps a level at -120 dBFS; the store keeps that clamp.
    expect(selectMeter(meterSnapshot(), "track:0", 0)?.inputPeakDb).toBe(-120);

    invokeMock.mockImplementation(async () => ({ devices: [] }));
    await pollMeters();
    expect(meterSnapshot()).toBeNull();
  });

  it("polls on an interval and stops after the last release", async () => {
    vi.useFakeTimers();
    invokeMock.mockImplementation(async (command) =>
      command === "meter_snapshot" ? snapshot() : {},
    );

    const release = startMeterPolling();
    await vi.advanceTimersByTimeAsync(260);
    const polls = () =>
      invokeMock.mock.calls.filter(([command]) => command === "meter_snapshot").length;
    const before = polls();
    expect(before).toBeGreaterThanOrEqual(3);

    release();
    await vi.advanceTimersByTimeAsync(500);
    expect(polls()).toBe(before);
    vi.useRealTimers();
  });

  it("resolves peak, RMS and the clip hold for a channel", async () => {
    invokeMock.mockImplementation(async (command) =>
      command === "meter_snapshot"
        ? {
            available: true,
            playing: true,
            master_channels: 2,
            mono: { requested: false, applied: false },
            master: {
              gain_reduction_db: 0,
              output_peak_db: 3,
              output_rms_db: -9.5,
              clipped: true,
              effects: [],
              short_term_lufs: -14.2,
              short_term_window_seconds: 3,
              correlation: -0.4,
              goniometer: [-0.5, 0.5],
            },
            tracks: [],
            buses: [],
          }
        : {},
    );
    await pollMeters();
    const row = selectChannelMeter(meterSnapshot(), "master");
    expect(row).toEqual({
      available: true,
      playing: true,
      peakDb: 3,
      rmsDb: -9.5,
      clipped: true,
      reason: null,
    });
    // A level outside the representable range is clamped, not dropped.
    expect(selectChannelMeter({
      ...meterSnapshot()!,
      master: { ...meterSnapshot()!.master, output_peak_db: 99 },
    }, "master")?.peakDb).toBe(12);
    expect(selectChannelMeter(null, "master")).toBeNull();
    expect(selectChannelMeter(meterSnapshot(), "bus:missing")).toBeNull();
  });

  it("resolves the master monitor row and its pending mono request", async () => {
    invokeMock.mockImplementation(async (command) =>
      command === "meter_snapshot"
        ? {
            available: true,
            playing: true,
            master_channels: 2,
            mono: { requested: true, applied: false },
            master: {
              gain_reduction_db: 0,
              output_peak_db: -6,
              output_rms_db: -12,
              clipped: false,
              effects: [],
              short_term_lufs: -14.24,
              short_term_window_seconds: 3,
              correlation: 1.4,
              goniometer: [0.5, 0.5],
            },
            tracks: [],
            buses: [],
          }
        : {},
    );
    await pollMeters();
    const monitor = selectMasterMonitor(meterSnapshot())!;
    expect(monitor.lufs).toBe(-14.24);
    expect(monitor.windowSeconds).toBe(3);
    // Correlation clamps to its defined range.
    expect(monitor.correlation).toBe(1);
    expect(monitor.monoPending).toBe(true);
    expect(monitor.masterChannels).toBe(2);
    expect(selectMasterMonitor(null)).toBeNull();
  });

  it("keeps ten seconds of master peak history and a gap when stopped", async () => {
    let playing = true;
    invokeMock.mockImplementation(async (command) =>
      command === "meter_snapshot"
        ? {
            available: true,
            playing,
            master_channels: 2,
            mono: { requested: false, applied: false },
            master: {
              gain_reduction_db: 0,
              output_peak_db: -6,
              output_rms_db: -12,
              clipped: false,
              effects: [],
            },
            tracks: [],
            buses: [],
          }
        : {},
    );
    for (let index = 0; index < METER_HISTORY_CAPACITY + 20; index += 1) {
      await pollMeters();
    }
    expect(masterHistory()).toHaveLength(METER_HISTORY_CAPACITY);
    expect(masterHistory().every((value) => value === -6)).toBe(true);

    playing = false;
    await pollMeters();
    expect(masterHistory()[masterHistory().length - 1]).toBeNull();
  });

  it("sends the clip reset and the mono request through their commands", async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === "meter_snapshot") {
        return {
          available: true,
          playing: true,
          master_channels: 2,
          mono: { requested: false, applied: false },
          master: { gain_reduction_db: 0, clipped: false, effects: [] },
          tracks: [],
          buses: [],
        };
      }
      return { requested: true, applied: false };
    });
    await resetMeterClip();
    expect(invokeMock.mock.calls.some(([command]) => command === "reset_meter_clip")).toBe(true);
    const mono = await setMonitorMono(true);
    expect(mono.requested).toBe(true);
    const call = invokeMock.mock.calls.find(([command]) => command === "set_monitor_mono");
    expect(call?.[1]).toEqual({ enabled: true });
  });
});
