/**
 * Audio page: the output device and format, the capture device and layout,
 * monitoring with its gain, the capture rule, and the "not capturing" meter
 * before a stream opens.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { AudioPage } from "../components/preferences/AudioPage";
import { audioIoStore, type AudioIoView } from "../stores/audioIo";
import { i18n } from "../stores/i18n";

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
];

const INPUTS = [
  {
    key: "coreaudio:usb-in",
    name: "USB Interface Input",
    is_default: true,
    channels: 2,
    sample_rates: [44100, 48000],
    default_rate: 48000,
  },
];

function view(): AudioIoView {
  return {
    status: {
      selection: {
        output_device: "coreaudio:speakers",
        output_device_name: "Speakers",
        sample_rate: 48000,
        buffer_frames: null,
      },
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
      input: {
        input_device: "coreaudio:usb-in",
        input_device_name: "USB Interface Input",
        // The backend default: a fresh install captures stereo.
        input_channels: "stereo",
        input_monitor: false,
        input_monitor_gain_db: 0,
      },
      inputs: INPUTS,
      default_input: "coreaudio:usb-in",
      opened_input: null,
      missing_input: false,
      input_notices: [],
      input_failure: null,
    },
    callback_frames: 256,
    latency_frames: 256,
    latency_seconds: 256 / 48000,
    underruns: 0,
    input: {
      open: false,
      device: null,
      channels: 2,
      sample_rate: 0,
      level_peak: 0,
      clipped: false,
      monitoring: false,
      monitor_gain_db: 0,
      error: null,
    },
    input_latency_frames: 0,
  };
}

async function renderPage() {
  const rendered = render(() => <AudioPage />);
  await vi.waitFor(() =>
    expect(rendered.container.querySelector(".audio-output-select")).toBeTruthy(),
  );
  return rendered;
}

beforeEach(() => {
  i18n.setLocale("en");
  audioIoStore.dispose();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_audio_io" || cmd === "list_audio_io") return view();
    if (cmd === "get_midi_config") {
      return { input: null, monitor: false, active: false, audition_active: false, error: null };
    }
    if (cmd === "list_midi_ports") return { supported: true, inputs: [], outputs: [], error: null };
    if (cmd === "get_midi_activity") {
      return { running: false, events: 0, last: null, error: null, audition_active: false };
    }
    return undefined;
  });
});

afterEach(() => {
  cleanup();
  audioIoStore.dispose();
});

describe("the Audio page", () => {
  it("shows the opened output format and the capture rule", async () => {
    const { container } = await renderPage();

    expect((container.querySelector(".audio-output-select") as HTMLSelectElement).value).toBe(
      "coreaudio:speakers",
    );
    expect(container.querySelector(".audio-latency")?.textContent).toBe("5.3 ms (256 samples)");
    // The capture rule is a backend contract, not UI copy.
    expect(container.querySelector(".preferences-rule")).toBeNull();
  });

  it("defaults the capture layout to stereo and says it is not capturing", async () => {
    const { container } = await renderPage();

    const channels = container.querySelector(".audio-input-channels") as HTMLSelectElement;
    expect(channels.value).toBe("stereo");
    expect(container.querySelector(".audio-input-level")?.textContent).toBe(
      i18n.t("preferences.audio.notCapturing"),
    );
  });

  it("sends the monitoring switch and the gain", async () => {
    let selection: { input_monitor?: boolean; input_monitor_gain_db?: number } = {};
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "set_input_io") {
        selection = (args as { selection: typeof selection }).selection;
        const base = view();
        return {
          ...base,
          status: { ...base.status, input: { ...base.status.input, ...selection } },
          input: {
            ...base.input,
            monitoring: Boolean(selection.input_monitor),
            monitor_gain_db: selection.input_monitor_gain_db ?? 0,
          },
        };
      }
      if (cmd === "get_audio_io") return view();
      if (cmd === "get_midi_config") {
        return { input: null, monitor: false, active: false, audition_active: false, error: null };
      }
      if (cmd === "list_midi_ports") return { supported: true, inputs: [], outputs: [], error: null };
      if (cmd === "get_midi_activity") {
        return { running: false, events: 0, last: null, error: null, audition_active: false };
      }
      return undefined;
    });
    const { container } = await renderPage();

    fireEvent.click(container.querySelector(".audio-monitor-toggle")!);
    await vi.waitFor(() => expect(audioIoStore.monitoring()).toBe(true));

    const gain = container.querySelector(".audio-monitor-gain") as HTMLInputElement;
    fireEvent.change(gain, { target: { value: "-6" } });
    await vi.waitFor(() => expect(selection.input_monitor_gain_db).toBe(-6));
    await vi.waitFor(() =>
      expect(container.querySelector(".preferences-gain-value")?.textContent).toBe(
        i18n.t("preferences.audio.gainValue", { db: "-6.0" }),
      ),
    );
  });
});
