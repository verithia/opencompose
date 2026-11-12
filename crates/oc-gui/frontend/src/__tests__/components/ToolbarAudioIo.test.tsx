/**
 * Toolbar audio-IO preferences — the four controls, their disabled reasons,
 * pending labels, snap-back after a failed open, the missing-device line and
 * the latency readout.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { adoptRecord } from "../../stores/record";
import { preferencesUi } from "../../stores/preferencesUi";
import { audioIoStore, type AudioIoStatus, type AudioIoView } from "../../stores/audioIo";
import { Toolbar } from "../../components/Toolbar";
import { audioLatencyValue } from "../../stores/audioIoText";

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

const SELECTION = {
  output_device: null,
  output_device_name: null,
  sample_rate: null,
  buffer_frames: null,
};

const INPUTS = [
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

const INPUT_SELECTION = {
  input_device: null,
  input_device_name: null,
  input_channels: "mono" as const,
  input_monitor: false,
  input_monitor_gain_db: 0,
};

const INPUT_VIEW = {
  open: true,
  device: "Built-in Microphone",
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
    inputs: INPUTS,
    default_input: "coreaudio:mic",
    opened_input: {
      key: "coreaudio:mic",
      name: "Built-in Microphone",
      sample_rate: 48000,
      channels: 1,
    },
    missing_input: false,
    input_notices: [],
    input_failure: null,
    ...overrides,
  };
}

function audioView(overrides: Partial<AudioIoView> = {}): AudioIoView {
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

interface MockOptions {
  view?: AudioIoView;
  setAudio?: (selection: { output_device: string | null }) => AudioIoView | Promise<AudioIoView>;
}

function mockCommands(options: MockOptions = {}) {
  const current = () => options.view ?? audioView();
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") {
      return {
        can_undo: false,
        can_redo: false,
        undo_label: null,
        redo_label: null,
        undo_labels: [],
        redo_labels: [],
      };
    }
    if (cmd === "get_midi_config") {
      return { input: null, monitor: false, active: false, audition_active: false, error: null };
    }
    if (cmd === "list_midi_ports") return { supported: true, inputs: [], outputs: [], error: null };
    if (cmd === "get_midi_activity") {
      return { running: false, events: 0, last: null, error: null, audition_active: false };
    }
    if (cmd === "get_audio_io" || cmd === "list_audio_io") return current();
    if (cmd === "set_input_io") {
      const selection = (args as { selection: typeof INPUT_SELECTION }).selection;
      const base = current();
      return {
        ...base,
        status: { ...base.status, input: { ...base.status.input, ...selection } },
        input: { ...base.input, monitoring: selection.input_monitor },
      };
    }
    if (cmd === "set_audio_io") {
      const selection = (args as { selection: typeof SELECTION }).selection;
      if (options.setAudio) return options.setAudio(selection);
      const base = current();
      const device = base.status.devices.find((entry) => entry.key === selection.output_device);
      return {
        ...base,
        status: {
          ...base.status,
          selection: { ...base.status.selection, ...selection },
          opened: device
            ? {
                key: device.key,
                name: device.name,
                sample_rate: 48000,
                buffer_frames: null,
                buffer_fixed: false,
              }
            : base.status.opened,
        },
      };
    }
    return makeSnapshot();
  });
}

async function openPreferences(container: HTMLElement) {
  fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
  await vi.waitFor(() => expect(container.querySelector(".preferences-modal")).toBeTruthy());
  fireEvent.click(container.querySelector('[data-page="audio"]') as HTMLElement);
  await vi.waitFor(() => expect(container.querySelector(".audio-output-select")).toBeTruthy());
}

beforeEach(async () => {
  preferencesUi.close();
  i18n.setLocale("en");
  audioIoStore.dispose();
  invokeMock.mockReset();
  mockCommands();
  await store.initProject();
});

afterEach(() => {
  audioIoStore.dispose();
  adoptRecord({ recording: false });
});

describe("Toolbar audio-IO preferences", () => {
  it("renders the device, rate, buffer and latency rows with the opened format", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    const output = container.querySelector<HTMLSelectElement>(".audio-output-select")!;
    expect(Array.from(output.options).map((option) => option.textContent)).toEqual([
      i18n.t("toolbar.audioSystemDefault"),
      "Speakers",
      "USB Interface",
    ]);
    expect(output.value).toBe("");

    const rate = container.querySelector<HTMLSelectElement>(".audio-rate-select")!;
    expect(rate.value).toBe("48000");
    expect(Array.from(rate.options).map((option) => option.value)).toEqual([
      "",
      "44100",
      "48000",
    ]);

    const buffer = container.querySelector<HTMLSelectElement>(".audio-buffer-select")!;
    expect(buffer.value).toBe("");
    expect(container.querySelector(".audio-latency")?.textContent).toBe(
      "10.7 ms (512 samples)",
    );
    expect(container.querySelector(".audio-status-line")?.textContent).toContain("Speakers");
  });

  it("states why the device picker cannot act when none are connected", async () => {
    mockCommands({ view: audioView({ status: status({ devices: [], opened: null, stream_open: false }) }) });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    expect(container.querySelector<HTMLSelectElement>(".audio-output-select")!.disabled).toBe(true);
    expect(container.querySelector<HTMLSelectElement>(".audio-rate-select")!.disabled).toBe(true);
    expect(container.querySelector<HTMLSelectElement>(".audio-buffer-select")!.disabled).toBe(true);
    const texts = [...container.querySelectorAll(".midi-hint")].map((node) => node.textContent);
    expect(texts).toContain(i18n.t("toolbar.audioNoDevices"));
    expect(texts).toContain(i18n.t("toolbar.audioOpenFirst"));
    expect(container.querySelector(".audio-latency")?.textContent).toBe("—");
    expect(texts).toContain(i18n.t("toolbar.audioNoStream"));
  });

  it("disables the rate and buffer with the device's own reasons", async () => {
    mockCommands({
      view: audioView({
        status: status({
          devices: [
            {
              key: "coreaudio:speakers",
              name: "Speakers",
              is_default: true,
              channels: 2,
              sample_rates: [48000],
              default_rate: 48000,
              buffer_frames: null,
            },
          ],
        }),
      }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    expect(container.querySelector<HTMLSelectElement>(".audio-rate-select")!.disabled).toBe(true);
    expect(container.querySelector<HTMLSelectElement>(".audio-buffer-select")!.disabled).toBe(true);
    const texts = [...container.querySelectorAll(".midi-hint")].map((node) => node.textContent);
    expect(texts).toContain(i18n.t("toolbar.audioOneRate", { rate: 48000 }));
    expect(texts).toContain(i18n.t("toolbar.audioDeviceBuffer"));
  });

  it("dims the section and shows a pending label while a switch is applied", async () => {
    let release: ((view: AudioIoView) => void) | null = null;
    mockCommands({
      setAudio: () =>
        new Promise<AudioIoView>((resolve) => {
          release = resolve;
        }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.change(container.querySelector(".audio-output-select")!, {
      target: { value: "coreaudio:usb" },
    });
    await vi.waitFor(() =>
      expect(container.querySelector(".audio-io-pending")?.textContent).toBe(
        i18n.t("toolbar.audioSwitching", { name: "USB Interface" }),
      ),
    );
    expect(container.querySelector(".audio-io-section")!.classList.contains("audio-io-busy")).toBe(
      true,
    );
    expect(container.querySelector<HTMLSelectElement>(".audio-output-select")!.disabled).toBe(true);

    release!(audioView());
    await vi.waitFor(() =>
      expect(container.querySelector(".audio-io-pending")).toBeNull(),
    );
  });

  it("snaps the picker back and shows the reason when the open fails", async () => {
    mockCommands({
      setAudio: () =>
        audioView({
          status: status({
            failure: {
              kind: "open_failed",
              device: "USB Interface",
              error: "the device is in use",
              fallback: "Speakers",
            },
          }),
        }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.change(container.querySelector(".audio-output-select")!, {
      target: { value: "coreaudio:usb" },
    });
    await vi.waitFor(() =>
      expect(container.querySelector(".audio-io-failure")?.textContent).toBe(
        i18n.t("toolbar.audioOpenFailed", {
          device: "USB Interface",
          error: "the device is in use",
          fallback: "Speakers",
        }),
      ),
    );
    expect(container.querySelector<HTMLSelectElement>(".audio-output-select")!.value).toBe("");
  });

  it("shows the missing-device line with the fallback in use", async () => {
    mockCommands({
      view: audioView({
        status: status({
          missing_device: true,
          selection: {
            output_device: "coreaudio:gone",
            output_device_name: "Studio Box",
            sample_rate: null,
            buffer_frames: null,
          },
          notices: [
            { kind: "missing_device", key: "coreaudio:gone", name: "Studio Box" },
          ],
        }),
      }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    expect(container.querySelector(".audio-io-missing")?.textContent).toBe(
      i18n.t("toolbar.audioMissingDevice", { name: "Studio Box", fallback: "Speakers" }),
    );
    // The missing-device notice is the banner, not a second line.
    expect(container.querySelectorAll(".audio-io-notice")).toHaveLength(0);
  });

  it("formats the latency readout in milliseconds and samples", () => {
    i18n.setLocale("en");
    expect(audioLatencyValue(512 / 48000, 512)).toBe("10.7 ms (512 samples)");
    expect(audioLatencyValue(0.0116, 556)).toBe("11.6 ms (556 samples)");
    i18n.setLocale("zh-Hans");
    expect(audioLatencyValue(0.0116, 556)).toBe("11.6 ms（556 个采样）");
  });

  it("translates every label of the section", async () => {
    i18n.setLocale("zh-Hans");
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    const labels = [...container.querySelectorAll(".audio-io-section .theme-select-label span")].map(
      (node) => node.textContent,
    );
    expect(labels).toEqual([
      i18n.t("toolbar.audioOutputDevice"),
      i18n.t("toolbar.audioSampleRate"),
      i18n.t("toolbar.audioBufferSize"),
      i18n.t("toolbar.audioInputDevice"),
      i18n.t("toolbar.audioInputChannels"),
    ]);
    expect(container.querySelector(".audio-io-section .keymap-title")?.textContent).toBe(
      i18n.t("toolbar.audioIo"),
    );
    expect(container.querySelector(".audio-io-label")?.textContent).toBe(
      i18n.t("toolbar.audioLatency"),
    );
    expect(
      [...container.querySelectorAll(".audio-io-label")].map((node) => node.textContent),
    ).toEqual([i18n.t("toolbar.audioLatency"), i18n.t("toolbar.audioInputLevel")]);
    expect(i18n.t("toolbar.audioIo")).not.toBe("Audio");
  });
});

describe("Toolbar capture preferences", () => {
  it("shows an output failure separately from the running input meter", async () => {
    const error = "headphones disconnected";
    mockCommands({
      view: audioView({ input: { ...INPUT_VIEW, monitor_error: error, monitoring: true } }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    expect(container.textContent).toContain(i18n.t("toolbar.audioMonitorOutputFailed", { error }));
    expect(container.querySelector(".audio-input-closed")).toBeNull();
  });

  it("locks device and format pickers during an audio take but leaves monitoring live", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    const pickers = [
      ".audio-output-select", ".audio-rate-select", ".audio-buffer-select",
      ".audio-input-select", ".audio-input-channels",
    ].map(selector => container.querySelector<HTMLSelectElement>(selector)!);
    expect(pickers.every(picker => !picker.disabled)).toBe(true);

    adoptRecord({ recording: true, mode: "audio" });
    expect(pickers.every(picker => picker.disabled)).toBe(true);
    expect(container.textContent).toContain(i18n.t("toolbar.audioRecordingActive"));
    expect(container.querySelector<HTMLInputElement>(".audio-monitor-toggle")!.disabled).toBe(false);

    adoptRecord({ recording: false });
    expect(pickers.every(picker => !picker.disabled)).toBe(true);
  });

  it("renders the input device, channel layout, monitor switch and level", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    const input = container.querySelector<HTMLSelectElement>(".audio-input-select")!;
    expect(Array.from(input.options).map((option) => option.textContent)).toEqual([
      i18n.t("toolbar.audioSystemDefault"),
      "Built-in Microphone",
      "USB Interface Input",
    ]);
    expect(input.value).toBe("");

    const channels = container.querySelector<HTMLSelectElement>(".audio-input-channels")!;
    expect(channels.value).toBe("mono");
    expect(Array.from(channels.options).map((option) => option.textContent)).toEqual([
      i18n.t("toolbar.audioInputMono"),
      i18n.t("toolbar.audioInputStereo"),
    ]);

    const monitor = container.querySelector<HTMLInputElement>(".audio-monitor-toggle")!;
    expect(monitor.checked).toBe(false);
    expect(container.querySelector(".audio-monitor-hint")).toBeNull();
    expect(container.querySelector(".audio-input-level")?.textContent).toBe("-∞ dBFS");
    expect(container.querySelector(".audio-input-clip")).toBeNull();
    expect(container.querySelector(".audio-input-closed")).toBeNull();
  });

  it("sends the chosen capture device with its display name", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    fireEvent.change(container.querySelector(".audio-input-select")!, {
      target: { value: "coreaudio:usb-in" },
    });
    await vi.waitFor(() => expect(audioIoStore.status().input.input_device).toBe("coreaudio:usb-in"));
  });

  it("sends the channel layout and the monitor switch", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    fireEvent.change(container.querySelector(".audio-input-channels")!, {
      target: { value: "stereo" },
    });
    await vi.waitFor(() => expect(audioIoStore.inputChannels()).toBe("stereo"));
    fireEvent.click(container.querySelector(".audio-monitor-toggle")!);
    await vi.waitFor(() => expect(audioIoStore.monitoring()).toBe(true));
  });

  it("states why the capture controls cannot act without a device", async () => {
    invokeMock.mockReset();
    mockCommands({ view: audioView({ status: status({ inputs: [], opened_input: null }) }) });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    expect(container.querySelector<HTMLSelectElement>(".audio-input-select")!.disabled).toBe(true);
    expect(container.querySelector<HTMLSelectElement>(".audio-input-channels")!.disabled).toBe(true);
    expect(container.querySelector<HTMLInputElement>(".audio-monitor-toggle")!.disabled).toBe(true);
    expect(container.querySelector(".audio-input-reason")?.textContent).toBe(
      i18n.t("toolbar.audioInputNoDevicesReason"),
    );
    expect(container.querySelector(".audio-monitor-reason")?.textContent).toBe(
      i18n.t("toolbar.audioInputNoDevicesReason"),
    );
  });

  it("names the saved capture device that is not connected", async () => {
    invokeMock.mockReset();
    mockCommands({
      view: audioView({
        status: status({
          input: { ...INPUT_SELECTION, input_device: "coreaudio:usb-in", input_device_name: "USB Interface Input" },
          missing_input: true,
        }),
      }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    const line = container.querySelector(".audio-io-missing")?.textContent ?? "";
    expect(line).toContain("USB Interface Input");
    expect(line).toContain("Built-in Microphone");
  });

  it("shows the capture open failure and the closed state", async () => {
    invokeMock.mockReset();
    mockCommands({
      view: audioView({
        input: {
          ...INPUT_VIEW,
          open: false,
          error: "Built-in Microphone: the device is busy",
        },
      }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    expect(container.querySelector(".audio-input-closed")?.textContent).toBe(
      i18n.t("toolbar.audioInputClosed"),
    );
    expect(container.querySelector(".audio-io-failure")?.textContent).toBe(
      "Built-in Microphone: the device is busy",
    );
    expect(container.querySelector(".audio-input-level")?.textContent).toBe(
      i18n.t("preferences.audio.notCapturing"),
    );
  });

  it("fills the level readout from the peak and shows a clip", async () => {
    invokeMock.mockReset();
    mockCommands({
      view: audioView({
        input: { ...INPUT_VIEW, level_peak: 0.5 },
      }),
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);
    expect(container.querySelector(".audio-input-level")?.textContent).toBe("-6.0 dBFS");
    preferencesUi.close();

    invokeMock.mockReset();
    mockCommands({
      view: audioView({
        input: { ...INPUT_VIEW, level_peak: 1.0, clipped: true },
      }),
    });
    const second = render(() => <Toolbar />);
    await openPreferences(second.baseElement);
    expect(second.baseElement.querySelector(".audio-input-clip")?.textContent).toBe(
      i18n.t("toolbar.audioInputClip"),
    );
  });
});
