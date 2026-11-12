/**
 * The record store: arming, the take, refusals and the recording clock.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { recordStore, type RecordState } from "../../stores/record";

function state(overrides: Partial<RecordState> = {}): RecordState {
  return {
    armed: null,
    recording: false,
    seconds: 0,
    report: null,
    disabled_reason: null,
    count_in_remaining_frames: 0,
    mode: "audio",
    midi: { target: null, quantise: "off", overdub: false, input: null },
    audio_device: null,
    ...overrides,
  };
}

beforeEach(() => {
  recordStore.dispose();
  invokeMock.mockReset();
});

afterEach(() => {
  recordStore.dispose();
});

describe("recordStore", () => {
  it("starts unarmed and not recording", () => {
    expect(recordStore.armed()).toBeNull();
    expect(recordStore.recording()).toBe(false);
    expect(recordStore.recordingLabel()).toBe("00:00");
  });

  it("adopts the record fields of a transport snapshot", () => {
    recordStore.adoptRecord(state({ armed: "vox", recording: true, seconds: 63.4 }));
    expect(recordStore.armed()).toBe("vox");
    expect(recordStore.recording()).toBe(true);
    expect(recordStore.recordingLabel()).toBe("01:03");
  });

  it("arms a track and disarms it again", async () => {
    const sent: (string | null)[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "arm_track") {
        const track = (args as { track: string | null }).track;
        sent.push(track);
        return { record: state({ armed: track }) };
      }
      return {};
    });
    await recordStore.toggleArm("vox");
    expect(sent[0]).toBe("vox");
    expect(recordStore.armed()).toBe("vox");
    await recordStore.toggleArm("vox");
    expect(sent[1]).toBeNull();
    expect(recordStore.armed()).toBeNull();
  });

  it("shows the backend's refusal verbatim and keeps the project", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") {
        throw "Recording could not start: No track is armed for recording. The project is unchanged.";
      }
      return {};
    });
    const result = await recordStore.start();
    expect(result).toBeNull();
    expect(recordStore.error()).toContain("No track is armed for recording");
    expect(recordStore.recording()).toBe(false);
  });

  it("adopts the project the stop returns, so the take becomes a clip", async () => {
    const snapshot = {
      ...TRANSPORT_STOPPED(),
      project: makeSnapshot({ name: "with take" }),
      record: state({ report: "Recorded vox take 1" }),
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "stop_recording") return snapshot;
      return {};
    });
    await recordStore.stop();
    expect(store.project.name).toBe("with take");
    expect(recordStore.report()).toBe("Recorded vox take 1");
    expect(recordStore.recording()).toBe(false);
    expect(store.transport().record?.recording).toBe(false);
  });

  it("formats the recording clock past a minute", () => {
    recordStore.adoptRecord(state({ recording: true, seconds: 125 }));
    expect(recordStore.recordingLabel()).toBe("02:05");
  });
});

/** A transport reply as `start_recording` and `get_transport_state` return it. */
function transportReply(overrides: Record<string, unknown> = {}) {
  return {
    playing: true,
    position_beats: 0,
    position_seconds: 0,
    duration_seconds: 30,
    record: state({ armed: "vox", recording: true }),
    ...overrides,
  };
}

function transportPolls(): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "get_transport_state").length;
}

describe("record take pump", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    recordStore.dispose();
    invokeMock.mockReset();
  });

  afterEach(async () => {
    // Stop through the public action so the poll interval is gone before the
    // fake clock is replaced.
    await store.stop();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("a record press starts the poll and the take clock follows it", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") return transportReply({ position_beats: 2 });
      if (cmd === "get_transport_state") {
        return transportReply({
          position_beats: 4,
          record: state({
            armed: "vox",
            recording: true,
            seconds: 1.5,
            count_in_remaining_frames: 48000,
            report: "Counting in",
          }),
        });
      }
      return {};
    });

    await recordStore.start();
    // The reply is adopted at once, before the first 50 ms tick.
    expect(store.transport().playing).toBe(true);
    expect(store.transport().position_beats).toBe(2);
    expect(recordStore.seconds()).toBe(0);

    await vi.advanceTimersByTimeAsync(50);

    expect(transportPolls()).toBe(1);
    expect(recordStore.seconds()).toBe(1.5);
    expect(recordStore.state().count_in_remaining_frames).toBe(48000);
    expect(recordStore.report()).toBe("Counting in");
    expect(store.transport().position_beats).toBe(4);
  });

  it("keeps the take clock when a poll reply carries no record", async () => {
    let withRecord = true;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") return transportReply();
      if (cmd === "get_transport_state") {
        return withRecord
          ? transportReply({ record: state({ armed: "vox", recording: true, seconds: 1.5 }) })
          : transportReply({ record: null });
      }
      return {};
    });

    await recordStore.start();
    await vi.advanceTimersByTimeAsync(50);
    expect(recordStore.seconds()).toBe(1.5);

    withRecord = false;
    await vi.advanceTimersByTimeAsync(50);
    expect(recordStore.seconds()).toBe(1.5);
    expect(recordStore.recording()).toBe(true);
  });

  it("a second press while the poll runs creates no second timer", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") return transportReply();
      if (cmd === "get_transport_state") return transportReply();
      return {};
    });

    await recordStore.start();
    const timers = vi.getTimerCount();
    await recordStore.start();
    expect(vi.getTimerCount()).toBe(timers);

    await vi.advanceTimersByTimeAsync(50);
    expect(transportPolls()).toBe(1);
  });

  it("a record press while already playing joins the running poll", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return transportReply({ position_beats: 8 });
      if (cmd === "start_recording") return transportReply({ position_beats: 8 });
      if (cmd === "get_transport_state") return transportReply({ position_beats: 9 });
      return {};
    });

    await store.play();
    const timers = vi.getTimerCount();
    await recordStore.start();
    expect(vi.getTimerCount()).toBe(timers);

    await vi.advanceTimersByTimeAsync(50);
    expect(transportPolls()).toBe(1);
    expect(store.transport().position_beats).toBe(9);
  });

  it("a refused press leaves no poll and no interval behind", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") throw "Recording could not start: no input device";
      if (cmd === "get_transport_state") return transportReply();
      return {};
    });

    await recordStore.start();
    expect(recordStore.error()).toContain("no input device");
    await vi.advanceTimersByTimeAsync(200);
    expect(transportPolls()).toBe(0);
  });

  it("stops the poll once a stopped transport has reported three times", async () => {
    let polled: Record<string, unknown> = transportReply();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "start_recording") return transportReply();
      if (cmd === "stop_recording") {
        return transportReply({
          playing: false,
          record: state({ armed: "vox", recording: false, report: "Recorded vox take 1" }),
        });
      }
      if (cmd === "get_transport_state") return polled;
      return {};
    });

    await recordStore.start();
    await vi.advanceTimersByTimeAsync(50);
    expect(transportPolls()).toBe(1);

    await recordStore.stop();
    polled = transportReply({
      playing: false,
      record: state({ armed: "vox", recording: false, report: "Recorded vox take 1" }),
    });
    await vi.advanceTimersByTimeAsync(150);
    const settled = transportPolls();
    expect(settled).toBe(4);
    expect(store.transport().playing).toBe(false);

    await vi.advanceTimersByTimeAsync(200);
    expect(transportPolls()).toBe(settled);
  });
});
