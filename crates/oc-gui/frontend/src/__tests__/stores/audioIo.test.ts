/**
 * Output-device store — the opened device, pending state, snap-back, errors.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock } from "../setup";
import {
  audioIoStore,
  type AudioIoStatus,
  type AudioIoView,
  type AudioSelection,
  type InputSelection,
  type InputView,
} from "../../stores/audioIo";

const DEVICES = [
  {
    key: "coreaudio:speakers",
    name: "Speakers",
    is_default: true,
    channels: 2,
    sample_rates: [44100, 48000],
    default_rate: 48000,
    buffer_frames: [64, 1024] as [number, number],
  },
  {
    key: "coreaudio:usb",
    name: "USB Interface",
    is_default: false,
    channels: 2,
    sample_rates: [44100, 48000, 96000],
    default_rate: 48000,
    buffer_frames: [32, 512] as [number, number],
  },
];

const SELECTION: AudioSelection = {
  output_device: null,
  output_device_name: null,
  sample_rate: null,
  buffer_frames: null,
};

const INPUT_SELECTION: InputSelection = {
  input_device: null,
  input_device_name: null,
  input_channels: "mono",
  input_monitor: false,
  input_monitor_gain_db: 0,
};

const INPUT_VIEW: InputView = {
  open: false,
  device: null,
  channels: 1,
  sample_rate: 48000,
  level_peak: 0,
  clipped: false,
  monitoring: false,
  monitor_gain_db: 0,
  error: null,
};

function status(overrides: Partial<AudioIoStatus> = {}): AudioIoStatus {
  return {
    selection: SELECTION,
    opened: {
      key: "coreaudio:speakers",
      name: "Speakers",
      sample_rate: 48000,
      buffer_frames: null,
      buffer_fixed: false,
    },
    devices: DEVICES,
    default_output: "coreaudio:speakers",
    missing_device: false,
    notices: [],
    failure: null,
    stream_open: true,
    input: INPUT_SELECTION,
    inputs: [],
    default_input: null,
    opened_input: null,
    missing_input: false,
    input_notices: [],
    input_failure: null,
    ...overrides,
  };
}

function view(overrides: Partial<AudioIoView> = {}): AudioIoView {
  return {
    status: status(),
    callback_frames: 512,
    latency_frames: 512,
    latency_seconds: 512 / 48000,
    underruns: 0,
    input: INPUT_VIEW,
    input_latency_frames: 512,
    ...overrides,
  };
}

beforeEach(() => {
  audioIoStore.dispose();
  invokeMock.mockReset();
});

afterEach(() => {
  audioIoStore.dispose();
});

describe("audioIoStore", () => {
  it("loads the opened device and format from the backend", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") return view();
      return {};
    });
    await audioIoStore.openPreferences();
    expect(audioIoStore.opened()?.name).toBe("Speakers");
    expect(audioIoStore.rateValue()).toBe(48000);
    expect(audioIoStore.callbackFrames()).toBe(512);
    expect(audioIoStore.missing()).toBe(false);
  });

  it("sends the whole selection and adopts the opened result", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "get_audio_io") return view();
      if (cmd === "set_audio_io") {
        const selection = (args as { selection: AudioSelection }).selection;
        expect(selection.output_device).toBe("coreaudio:usb");
        return view({
          status: status({
            selection,
            opened: {
              key: "coreaudio:usb",
              name: "USB Interface",
              sample_rate: 96000,
              buffer_frames: 256,
              buffer_fixed: true,
            },
          }),
        });
      }
      return {};
    });
    await audioIoStore.openPreferences();
    await audioIoStore.selectOutput("coreaudio:usb");
    expect(audioIoStore.outputValue()).toBe("coreaudio:usb");
    expect(audioIoStore.opened()?.name).toBe("USB Interface");
    expect(audioIoStore.rateValue()).toBe(96000);
    expect(audioIoStore.bufferValue()).toBe(256);
  });

  it("keeps the opened device when the backend rejects the selection", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") return view();
      if (cmd === "set_audio_io") {
        return view({
          status: status({
            failure: {
              kind: "open_failed",
              device: "Interface",
              error: "the device is in use",
              fallback: "Speakers",
            },
          }),
        });
      }
      return {};
    });
    await audioIoStore.openPreferences();
    const rejected = await audioIoStore.selectOutput("coreaudio:usb");
    expect(rejected?.status.failure?.kind).toBe("open_failed");
    // The picker snaps back to the device that is still open.
    expect(audioIoStore.outputValue()).toBe("");
    expect(audioIoStore.pending()).toBeNull();
  });

  it("shows a pending label until the switch returns", async () => {
    let release: ((value: AudioIoView) => void) | null = null;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") return view();
      if (cmd === "set_audio_io") {
        return new Promise<AudioIoView>((resolve) => {
          release = resolve;
        });
      }
      return {};
    });
    await audioIoStore.openPreferences();
    const switching = audioIoStore.selectOutput("coreaudio:usb");
    await vi.waitFor(() => expect(audioIoStore.pending()).not.toBeNull());
    expect(audioIoStore.pending()).toEqual({ kind: "output", detail: "USB Interface" });
    release!(view());
    await switching;
    expect(audioIoStore.pending()).toBeNull();
  });

  it("surfaces a command failure instead of pretending it changed", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") return view();
      if (cmd === "set_audio_io") throw new Error("no audio device");
      return {};
    });
    await audioIoStore.openPreferences();
    const result = await audioIoStore.selectRate(96000);
    expect(result).toBeNull();
    expect(audioIoStore.error()).toContain("no audio device");
    // The readout keeps the opened rate rather than the rejected request.
    expect(audioIoStore.rateValue()).toBe(48000);
  });

  it("re-enumerates devices on refresh", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") return view();
      if (cmd === "list_audio_io") {
        return view({ status: status({ missing_device: true }) });
      }
      return {};
    });
    await audioIoStore.openPreferences();
    await audioIoStore.refresh();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "list_audio_io")).toBe(true);
    expect(audioIoStore.missing()).toBe(true);
  });
});

describe("audioIoStore capture", () => {
  const INPUT_DEVICES = [
    {
      key: "coreaudio:mic",
      name: "Built-in Microphone",
      is_default: true,
      channels: 1,
      sample_rates: [44100, 48000],
      default_rate: 48000,
    },
    {
      key: "coreaudio:usb-in",
      name: "USB Interface Input",
      is_default: false,
      channels: 2,
      sample_rates: [44100, 48000],
      default_rate: 48000,
    },
  ];

  function captureView(): AudioIoView {
    return view({
      status: status({
        inputs: INPUT_DEVICES,
        default_input: "coreaudio:mic",
        opened_input: {
          key: "coreaudio:mic",
          name: "Built-in Microphone",
          sample_rate: 48000,
          channels: 1,
        },
      }),
      input: { ...INPUT_VIEW, open: true, device: "Built-in Microphone" },
    });
  }

  function mockCapture(onSet: (selection: InputSelection) => AudioIoView) {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "get_audio_io") return captureView();
      if (cmd === "set_input_io") {
        return onSet((args as { selection: InputSelection }).selection);
      }
      return {};
    });
  }

  it("reads the input list, the chosen device and the level", async () => {
    mockCapture(() => captureView());
    await audioIoStore.openPreferences();
    expect(audioIoStore.inputs().map((device) => device.name)).toEqual([
      "Built-in Microphone",
      "USB Interface Input",
    ]);
    expect(audioIoStore.inputValue()).toBe("");
    expect(audioIoStore.inputChannels()).toBe("mono");
    expect(audioIoStore.monitoring()).toBe(false);
    expect(audioIoStore.inputView().open).toBe(true);
    expect(audioIoStore.inputLevel()).toBe(0);
  });

  it("sends the device name with the key and keeps the rest of the selection", async () => {
    let sent: InputSelection | null = null;
    mockCapture((selection) => {
      sent = selection;
      return captureView();
    });
    await audioIoStore.openPreferences();
    await audioIoStore.selectInput("coreaudio:usb-in");
    expect(sent).not.toBeNull();
    expect(sent!.input_device).toBe("coreaudio:usb-in");
    expect(sent!.input_device_name).toBe("USB Interface Input");
    expect(sent!.input_channels).toBe("mono");
  });

  it("sends the channel layout and the monitor switch", async () => {
    const sent: InputSelection[] = [];
    mockCapture((selection) => {
      sent.push(selection);
      return captureView();
    });
    await audioIoStore.openPreferences();
    await audioIoStore.selectInputChannels("stereo");
    await audioIoStore.setInputMonitor(true);
    await audioIoStore.setInputMonitorGain(-6);
    expect(sent[0].input_channels).toBe("stereo");
    expect(sent[1].input_monitor).toBe(true);
    expect(sent[2].input_monitor_gain_db).toBe(-6);
  });

  it("reports a pending label while the capture choice is applied", async () => {
    let release: (value: AudioIoView) => void = () => {};
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "get_audio_io") return captureView();
      if (cmd === "set_input_io") {
        const selection = (args as { selection: InputSelection }).selection;
        return new Promise<AudioIoView>((resolve) => {
          release = () => resolve(captureView());
          void selection;
        });
      }
      return {};
    });
    await audioIoStore.openPreferences();
    const inFlight = audioIoStore.selectInput("coreaudio:usb-in");
    expect(audioIoStore.pending()).toEqual({ kind: "input", detail: "USB Interface Input" });
    release(captureView());
    await inFlight;
    expect(audioIoStore.pending()).toBeNull();
  });

  it("keeps the failure the backend reports for a refused capture choice", async () => {
    mockCapture(() =>
      view({
        status: status({
          inputs: INPUT_DEVICES,
          input_failure: {
            kind: "input_open_failed",
            device: "USB Interface Input",
            error: "the device is busy",
            fallback: "Built-in Microphone",
          },
        }),
      }),
    );
    await audioIoStore.openPreferences();
    const result = await audioIoStore.selectInput("coreaudio:usb-in");
    expect(result?.status.input_failure).toEqual({
      kind: "input_open_failed",
      device: "USB Interface Input",
      error: "the device is busy",
      fallback: "Built-in Microphone",
    });
    expect(audioIoStore.status().input_failure).not.toBeNull();
  });

  it("survives a backend that predates the capture fields", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_audio_io") {
        const legacy = view() as unknown as Record<string, unknown>;
        const status = { ...(legacy.status as Record<string, unknown>) };
        for (const key of [
          "input",
          "inputs",
          "opened_input",
          "missing_input",
          "input_notices",
          "input_failure",
        ]) {
          delete status[key];
        }
        return { ...legacy, status, input: undefined };
      }
      return {};
    });
    await audioIoStore.openPreferences();
    expect(audioIoStore.inputs()).toEqual([]);
    // A backend without the capture fields falls back to the current default.
    expect(audioIoStore.inputChannels()).toBe("stereo");
    expect(audioIoStore.monitoring()).toBe(false);
    expect(audioIoStore.inputView().open).toBe(false);
  });
});
