/**
 * Output-device preferences adapter — the device list, the opened device, the
 * negotiated rate and period, and the health notices. Shapes mirror
 * `crates/oc-audio/src/audio_io.rs`; this is the only frontend module that
 * talks to the audio-IO commands.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { startNativeAudio } from "./webAudioFocus";

export interface OutputDeviceInfo {
  key: string;
  name: string;
  is_default: boolean;
  channels: number;
  sample_rates: number[];
  default_rate: number;
  buffer_frames: [number, number] | null;
}

export interface InputDeviceInfo {
  key: string;
  name: string;
  is_default: boolean;
  channels: number;
  sample_rates: number[];
  default_rate: number;
}

export type InputChannels = "mono" | "stereo";

export interface InputSelection {
  input_device: string | null;
  input_device_name: string | null;
  input_channels: InputChannels;
  input_monitor: boolean;
  input_monitor_gain_db: number;
}

export interface OpenedInput {
  key: string | null;
  name: string;
  sample_rate: number;
  channels: number;
}

export interface InputView {
  open: boolean;
  device: string | null;
  channels: number;
  sample_rate: number;
  level_peak: number;
  clipped: boolean;
  monitoring: boolean;
  monitor_gain_db: number;
  monitor_error?: string | null;
  error: string | null;
}

export interface AudioSelection {
  output_device: string | null;
  output_device_name: string | null;
  sample_rate: number | null;
  buffer_frames: number | null;
}

export interface OpenedOutput {
  key: string | null;
  name: string;
  sample_rate: number;
  buffer_frames: number | null;
  buffer_fixed: boolean;
}

export type AudioNotice =
  | { kind: "missing_device"; key: string; name: string }
  | { kind: "rate_substituted"; requested: number; chosen: number }
  | { kind: "buffer_unsupported"; requested: number }
  | { kind: "buffer_clamped"; requested: number; chosen: number; min: number; max: number }
  | { kind: "buffer_rejected"; requested: number; error: string }
  | { kind: "settings_unreadable"; error: string }
  | { kind: "settings_unwritable"; error: string }
  | { kind: "stream_failed"; role: string; error: string }
  | { kind: "missing_input_device"; key: string; name: string }
  | { kind: "input_rate_substituted"; requested: number; chosen: number }
  | { kind: "input_channels_reduced"; requested: number; chosen: number }
  | { kind: "input_failed"; error: string };

export type AudioFailure =
  | { kind: "recording_active" }
  | { kind: "no_devices" }
  | { kind: "list_failed"; error: string }
  | { kind: "open_failed"; device: string; error: string; fallback: string | null }
  | { kind: "stream_lost"; device: string; error: string }
  | { kind: "no_input_devices" }
  | { kind: "input_open_failed"; device: string; error: string; fallback: string | null };

export interface AudioIoStatus {
  selection: AudioSelection;
  opened: OpenedOutput | null;
  devices: OutputDeviceInfo[];
  default_output: string | null;
  missing_device: boolean;
  notices: AudioNotice[];
  failure: AudioFailure | null;
  stream_open: boolean;
  input: InputSelection;
  inputs: InputDeviceInfo[];
  default_input: string | null;
  opened_input: OpenedInput | null;
  missing_input: boolean;
  input_notices: AudioNotice[];
  input_failure: AudioFailure | null;
}

export interface AudioIoView {
  status: AudioIoStatus;
  callback_frames: number;
  latency_frames: number;
  latency_seconds: number;
  underruns: number;
  input: InputView;
  input_latency_frames: number;
}

/** What the section is waiting for, so every control can show a pending label. */
export type AudioPending =
  | { kind: "output"; detail: string }
  | { kind: "rate"; detail: number }
  | { kind: "buffer"; detail: number }
  | { kind: "input"; detail: string }
  | { kind: "channels"; detail: InputChannels }
  | { kind: "monitor"; detail: boolean }
  | { kind: "refresh" }
  | null;

const EMPTY_SELECTION: AudioSelection = {
  output_device: null,
  output_device_name: null,
  sample_rate: null,
  buffer_frames: null,
};

const EMPTY_INPUT_SELECTION: InputSelection = {
  input_device: null,
  input_device_name: null,
  // Matches the backend default: a fresh install captures stereo.
  input_channels: "stereo",
  input_monitor: false,
  input_monitor_gain_db: 0,
};

const EMPTY_INPUT: InputView = {
  open: false,
  device: null,
  channels: 1,
  sample_rate: 0,
  level_peak: 0,
  clipped: false,
  monitoring: false,
  monitor_gain_db: 0,
  error: null,
};

const EMPTY_STATUS: AudioIoStatus = {
  selection: EMPTY_SELECTION,
  opened: null,
  devices: [],
  default_output: null,
  missing_device: false,
  notices: [],
  failure: null,
  stream_open: false,
  input: EMPTY_INPUT_SELECTION,
  inputs: [],
  default_input: null,
  opened_input: null,
  missing_input: false,
  input_notices: [],
  input_failure: null,
};

const EMPTY_VIEW: AudioIoView = {
  status: EMPTY_STATUS,
  callback_frames: 0,
  latency_frames: 0,
  latency_seconds: 0,
  underruns: 0,
  input: EMPTY_INPUT,
  input_latency_frames: 0,
};

const [view, setView] = createSignal<AudioIoView>(EMPTY_VIEW);
const [pending, setPending] = createSignal<AudioPending>(null);
const [error, setError] = createSignal<string | null>(null);

/** Ignore a response that is not an audio-IO snapshot (older backend, stub test). */
function applyView(next: AudioIoView | null | undefined): AudioIoView | null {
  if (!next || !next.status || !Array.isArray(next.status.devices)) return null;
  // A backend older than the capture fields answers without them.
  const normalized: AudioIoView = {
    ...next,
    status: {
      ...next.status,
      input: next.status.input ?? EMPTY_INPUT_SELECTION,
      inputs: next.status.inputs ?? [],
      input_notices: next.status.input_notices ?? [],
      input_failure: next.status.input_failure ?? null,
      opened_input: next.status.opened_input ?? null,
      default_input: next.status.default_input ?? null,
      missing_input: next.status.missing_input ?? false,
    },
    input: next.input ?? EMPTY_INPUT,
    input_latency_frames: next.input_latency_frames ?? 0,
  };
  setView(normalized);
  return normalized;
}

async function readStatus(): Promise<AudioIoView | null> {
  try {
    const next = applyView(await invoke<AudioIoView>("get_audio_io"));
    if (next) setError(null);
    return next;
  } catch (failure) {
    setError(String(failure));
    return null;
  }
}

/** Re-enumerate and re-read; the preferences header's ↻ button. */
async function refresh(): Promise<void> {
  setPending({ kind: "refresh" });
  try {
    applyView(await invoke<AudioIoView>("list_audio_io"));
    setError(null);
  } catch (failure) {
    setError(String(failure));
  } finally {
    setPending(null);
  }
}

/** Re-read state when the preferences popover opens; never opens a stream. */
async function openPreferences(): Promise<void> {
  await readStatus();
}

async function apply(selection: AudioSelection, label: AudioPending): Promise<AudioIoView | null> {
  setPending(label);
  try {
    const next = applyView(await invoke<AudioIoView>("set_audio_io", { selection }));
    if (next) setError(null);
    return next;
  } catch (failure) {
    setError(String(failure));
    return null;
  } finally {
    setPending(null);
  }
}

async function selectOutput(key: string | null): Promise<AudioIoView | null> {
  const status = view().status;
  const device = status.devices.find((entry) => entry.key === key);
  const selection: AudioSelection = {
    ...status.selection,
    output_device: key,
    output_device_name: device?.name ?? null,
  };
  return apply(selection, {
    kind: "output",
    detail: device?.name ?? status.selection.output_device_name ?? "",
  });
}

async function selectRate(rate: number | null): Promise<AudioIoView | null> {
  const status = view().status;
  return apply(
    { ...status.selection, sample_rate: rate },
    { kind: "rate", detail: rate ?? 0 },
  );
}

async function setBuffer(frames: number | null): Promise<AudioIoView | null> {
  const status = view().status;
  return apply(
    { ...status.selection, buffer_frames: frames },
    { kind: "buffer", detail: frames ?? 0 },
  );
}

async function applyInput(
  selection: InputSelection,
  label: AudioPending,
): Promise<AudioIoView | null> {
  setPending(label);
  try {
    const setInput = () => invoke<AudioIoView>("set_input_io", { selection });
    const next = applyView(await (selection.input_monitor ? startNativeAudio(setInput) : setInput()));
    if (next) setError(null);
    return next;
  } catch (failure) {
    setError(String(failure));
    return null;
  } finally {
    setPending(null);
  }
}

async function selectInput(key: string | null): Promise<AudioIoView | null> {
  const status = view().status;
  const device = status.inputs.find((entry) => entry.key === key);
  return applyInput(
    {
      ...status.input,
      input_device: key,
      input_device_name: device?.name ?? null,
    },
    { kind: "input", detail: device?.name ?? status.input.input_device_name ?? "" },
  );
}

async function selectInputChannels(next: InputChannels): Promise<AudioIoView | null> {
  const status = view().status;
  return applyInput(
    { ...status.input, input_channels: next },
    { kind: "channels", detail: next },
  );
}

async function setInputMonitor(on: boolean): Promise<AudioIoView | null> {
  const status = view().status;
  return applyInput({ ...status.input, input_monitor: on }, { kind: "monitor", detail: on });
}

async function setInputMonitorGain(db: number): Promise<AudioIoView | null> {
  const status = view().status;
  return applyInput(
    { ...status.input, input_monitor_gain_db: db },
    { kind: "monitor", detail: status.input.input_monitor },
  );
}

/** The device the pickers must show; `null` while no device is enumerated. */
const devices = () => view().status.devices;
const status = () => view().status;
const opened = () => view().status.opened;
const missing = () => view().status.missing_device;

/** Value of the output picker: the persisted choice, `""` for system default. */
const outputValue = () => view().status.selection.output_device ?? "";

/** Rate shown by the rate picker: what was opened, not what was requested. */
const rateValue = (): number | null => opened()?.sample_rate ?? view().status.selection.sample_rate;

/** Period shown by the buffer picker: what was opened, `null` for the default. */
const bufferValue = (): number | null => {
  const current = opened();
  return current === null ? view().status.selection.buffer_frames : current.buffer_frames;
};

/** Frames of the last measured device callback, 0 while nothing reports one. */
const callbackFrames = () => view().callback_frames;

const latencyFrames = () => view().latency_frames;
const latencySeconds = () => view().latency_seconds;
const streamOpen = () => view().status.stream_open;
const inputs = () => view().status.inputs;
const inputStatus = () => view().status.input;
const inputView = () => view().input;
const openedInput = () => view().status.opened_input;
const missingInput = () => view().status.missing_input;
const inputValue = () => view().status.input.input_device ?? "";
const inputChannels = () => view().status.input.input_channels;
const monitoring = () => view().status.input.input_monitor;
const monitorGainDb = () => view().status.input.input_monitor_gain_db;
/** Peak of the last capture block, 0..1. */
const inputLevel = () => view().input.level_peak;
const inputClipped = () => view().input.clipped;
const inputLatencyFrames = () => view().input_latency_frames;
const underruns = () => view().underruns;

function dispose(): void {
  setView(EMPTY_VIEW);
  setPending(null);
  setError(null);
}

export const audioIoStore = {
  view,
  devices,
  status,
  opened,
  missing,
  outputValue,
  rateValue,
  bufferValue,
  callbackFrames,
  latencyFrames,
  latencySeconds,
  streamOpen,
  underruns,
  pending,
  error,
  openPreferences,
  refresh,
  readStatus,
  inputs,
  inputStatus,
  inputView,
  openedInput,
  missingInput,
  inputValue,
  inputChannels,
  monitoring,
  monitorGainDb,
  inputLevel,
  inputClipped,
  inputLatencyFrames,
  selectOutput,
  selectRate,
  setBuffer,
  selectInput,
  selectInputChannels,
  setInputMonitor,
  setInputMonitorGain,
  dispose,
};
