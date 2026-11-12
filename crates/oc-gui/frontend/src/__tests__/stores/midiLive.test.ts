/**
 * Live MIDI store — device selection, persisted resume, and activity polling.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock } from "../setup";
import { midiNoteName, midiStore } from "../../stores/midiLive";

const PORTS = {
  supported: true,
  inputs: [
    { index: 0, name: "Keys", id: "in-0" },
    { index: 1, name: "Pads", id: "in-1" },
  ],
  outputs: [],
  error: null,
};

const IDLE = { input: null, monitor: false, active: false, audition_active: false, error: null };

const ACTIVITY = {
  running: true,
  events: 1,
  last: {
    kind: "note_on",
    channel: 0,
    note: 60,
    velocity: 100,
    controller: null,
    value: null,
    millis: 12,
  },
  error: null,
  audition_active: false,
};

function mockCommands(overrides: Record<string, (args?: unknown) => unknown> = {}) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (overrides[cmd]) return overrides[cmd](args);
    if (cmd === "get_midi_config") return IDLE;
    if (cmd === "list_midi_ports") return PORTS;
    if (cmd === "get_midi_activity") return { ...ACTIVITY, running: false, events: 0, last: null };
    if (cmd === "set_midi_config") {
      const request = (args ?? {}) as { input?: string | null; monitor?: boolean };
      return {
        input: request.input ?? null,
        monitor: Boolean(request.monitor),
        active: Boolean(request.monitor && request.input),
        audition_active: false,
        error: null,
      };
    }
    return {};
  });
}

function setCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "set_midi_config");
}

beforeEach(() => {
  midiStore.dispose();
  invokeMock.mockReset();
  mockCommands();
});

afterEach(() => {
  midiStore.dispose();
});

describe("midiStore", () => {
  it("loads the saved selection and resumes the monitor stream", async () => {
    const mpe = { lower: { first: 2, last: 12 }, bend_range: 48 };
    mockCommands({
      get_midi_config: () => ({
        input: "Keys",
        monitor: true,
        mpe,
        active: false,
        audition_active: false,
        error: null,
      }),
      set_midi_config: () => ({
        input: "Keys",
        monitor: true,
        mpe,
        active: true,
        audition_active: false,
        error: null,
      }),
      get_midi_activity: () => ACTIVITY,
    });

    await midiStore.init();

    expect(midiStore.config().input).toBe("Keys");
    expect(midiStore.config().monitor).toBe(true);
    expect(midiStore.config().active).toBe(true);
    expect(setCalls()).toHaveLength(1);
    expect(setCalls()[0][1]).toEqual({ input: "Keys", monitor: true, mpe });
    await vi.waitFor(() => expect(midiStore.activity().last?.note).toBe(60));
  });

  it("restores a saved selection without starting a stream", async () => {
    mockCommands({
      get_midi_config: () => ({
        input: "Keys",
        monitor: false,
        active: false,
        audition_active: false,
        error: null,
      }),
    });

    await midiStore.init();

    expect(midiStore.config().input).toBe("Keys");
    expect(midiStore.config().monitor).toBe(false);
    expect(setCalls()).toHaveLength(0);
  });

  it("reports an absent backend without throwing or enabling the monitor", async () => {
    mockCommands({
      list_midi_ports: () => ({
        supported: false,
        inputs: [],
        outputs: [],
        error: "MIDI support is unavailable: no backend",
      }),
    });

    await midiStore.init();

    expect(midiStore.ports().supported).toBe(false);
    expect(midiStore.ports().error).toContain("unavailable");
    expect(midiStore.config().monitor).toBe(false);
    expect(setCalls()).toHaveLength(0);
  });

  it("persists a device selection and only starts monitoring when enabled", async () => {
    await midiStore.init();

    await midiStore.selectInput("Keys");
    expect(setCalls()[0][1]).toEqual({ input: "Keys", monitor: false });
    expect(midiStore.config().input).toBe("Keys");
    expect(midiStore.config().active).toBe(false);

    await midiStore.setMonitor(true);
    expect(setCalls()[1][1]).toEqual({ input: "Keys", monitor: true });
    expect(midiStore.config().monitor).toBe(true);
    expect(midiStore.config().active).toBe(true);
  });

  it("clears the monitor when the selection is removed", async () => {
    await midiStore.selectInput("Keys");
    await midiStore.setMonitor(true);

    await midiStore.selectInput(null);

    expect(setCalls().at(-1)?.[1]).toEqual({ input: null, monitor: false });
    expect(midiStore.config().input).toBeNull();
    expect(midiStore.config().monitor).toBe(false);
  });

  it("surfaces a device error returned by the backend", async () => {
    mockCommands({
      set_midi_config: () => ({
        input: "Gone",
        monitor: true,
        active: false,
        audition_active: false,
        error: "MIDI input 'Gone': MIDI input port 'Gone' was not found",
      }),
    });

    await midiStore.init();
    await midiStore.selectInput("Gone");

    expect(midiStore.config().monitor).toBe(true);
    expect(midiStore.config().active).toBe(false);
    expect(midiStore.config().error).toContain("not found");
  });

  it("flags a saved device that is no longer connected", async () => {
    mockCommands({
      get_midi_config: () => ({
        input: "Gone",
        monitor: false,
        active: false,
        audition_active: false,
        error: null,
      }),
    });

    await midiStore.init();

    expect(midiStore.missingSelection()).toBe(true);
    await midiStore.selectInput("Pads");
    expect(midiStore.missingSelection()).toBe(false);
  });

  it("formats MIDI note numbers", () => {
    expect(midiNoteName(60)).toBe("C4");
    expect(midiNoteName(61)).toBe("C#4");
    expect(midiNoteName(0)).toBe("C-1");
    expect(midiNoteName(127)).toBe("G9");
  });
});
