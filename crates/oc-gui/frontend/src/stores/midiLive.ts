/**
 * Live MIDI preferences adapter — ports, device selection, monitor stream and
 * activity. Shapes mirror `src-tauri/src/midi_live.rs`; this is the only
 * frontend module that talks to the live MIDI commands.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { startNativeAudio } from "./webAudioFocus";

export interface MidiPortEntry {
  index: number;
  name: string;
  id: string;
}

export interface MidiPorts {
  supported: boolean;
  inputs: MidiPortEntry[];
  outputs: MidiPortEntry[];
  error: string | null;
}

interface MidiMpeConfig {
  lower?: { first: number; last: number };
  upper?: { first: number; last: number };
  bend_range?: number;
}

export interface MidiConfig {
  input: string | null;
  monitor: boolean;
  mpe?: MidiMpeConfig | null;
  /** A backend MIDI thread is polling the selected input. */
  active: boolean;
  /** An OpenSynth audition is open, so input is audible. */
  audition_active: boolean;
  error: string | null;
}

export interface MidiActivityEvent {
  kind: string;
  channel: number;
  note: number | null;
  velocity: number | null;
  controller: number | null;
  value: number | null;
  millis: number;
}

export interface MidiActivity {
  running: boolean;
  events: number;
  last: MidiActivityEvent | null;
  error: string | null;
  audition_active: boolean;
}

const EMPTY_PORTS: MidiPorts = { supported: false, inputs: [], outputs: [], error: null };
const EMPTY_CONFIG: MidiConfig = {
  input: null,
  monitor: false,
  active: false,
  audition_active: false,
  error: null,
};
const EMPTY_ACTIVITY: MidiActivity = {
  running: false,
  events: 0,
  last: null,
  error: null,
  audition_active: false,
};

const POLL_MS = 250;

const [ports, setPorts] = createSignal<MidiPorts>(EMPTY_PORTS);
const [config, setConfig] = createSignal<MidiConfig>(EMPTY_CONFIG);
const [activity, setActivity] = createSignal<MidiActivity>(EMPTY_ACTIVITY);
const [busy, setBusy] = createSignal(false);

let pollTimer: ReturnType<typeof setInterval> | undefined;

function stopPolling() {
  if (pollTimer !== undefined) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }
  setActivity(EMPTY_ACTIVITY);
}

function refreshActivity() {
  void invoke<MidiActivity>("get_midi_activity")
    .then((next) => {
      setActivity(next);
      if (!next.running && pollTimer !== undefined) {
        // Keep the last error visible while stopping the 4 Hz poll.
        clearInterval(pollTimer);
        pollTimer = undefined;
      }
    })
    .catch(() => {});
}

function startPolling() {
  if (pollTimer !== undefined) return;
  refreshActivity();
  pollTimer = setInterval(refreshActivity, POLL_MS);
}

function applyView(view: MidiConfig) {
  setConfig(view);
  if (view.active) startPolling();
  else stopPolling();
}

async function applySelection(input: string | null, monitor: boolean): Promise<MidiConfig | null> {
  setBusy(true);
  try {
    const mpe = config().mpe;
    const selectInput = () => invoke<MidiConfig>("set_midi_config", {
      input, monitor, ...(mpe ? { mpe } : {}),
    });
    const view = await (input && monitor ? startNativeAudio(selectInput) : selectInput());
    applyView(view);
    return view;
  } catch (error) {
    setConfig({ ...config(), monitor: false, active: false, error: String(error) });
    stopPolling();
    return null;
  } finally {
    setBusy(false);
  }
}

async function init(): Promise<void> {
  try {
    const [saved, available] = await Promise.all([
      invoke<MidiConfig>("get_midi_config"),
      invoke<MidiPorts>("list_midi_ports"),
    ]);
    setPorts(available);
    setConfig(saved);
    if (saved.monitor && saved.input) {
      // Resume a persisted monitor stream against the devices present now.
      await applySelection(saved.input, true);
    } else if (!saved.active) {
      stopPolling();
    }
  } catch (error) {
    setPorts(EMPTY_PORTS);
    setConfig({ ...EMPTY_CONFIG, error: String(error) });
    stopPolling();
  }
}

async function refreshPorts(): Promise<void> {
  try {
    setPorts(await invoke<MidiPorts>("list_midi_ports"));
  } catch (error) {
    setPorts({ supported: false, inputs: [], outputs: [], error: String(error) });
  }
}

/** Re-read state when the preferences popover opens; never starts a stream. */
async function openPreferences(): Promise<void> {
  await Promise.all([
    refreshPorts(),
    invoke<MidiConfig>("get_midi_config")
      .then((view) => setConfig(view))
      .catch(() => {}),
  ]);
}

async function selectInput(name: string | null): Promise<MidiConfig | null> {
  const input = name && name.trim() ? name : null;
  const monitor = input ? config().monitor : false;
  return applySelection(input, monitor);
}

async function setMonitor(enabled: boolean): Promise<MidiConfig | null> {
  const input = config().input;
  if (enabled && !input) return config();
  return applySelection(input, enabled);
}

function dispose() {
  stopPolling();
}

const missingSelection = () => {
  const input = config().input;
  return input !== null && !ports().inputs.some((entry) => entry.name === input);
};

const auditionActive = () => config().audition_active || activity().audition_active;

const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** MIDI note number to scientific pitch name, e.g. 60 → C4. */
export function midiNoteName(note: number): string {
  const value = Math.trunc(note);
  return `${NOTE_NAMES[((value % 12) + 12) % 12]}${Math.floor(value / 12) - 1}`;
}

export const midiStore = {
  ports,
  config,
  activity,
  busy,
  init,
  openPreferences,
  refreshPorts,
  selectInput,
  setMonitor,
  dispose,
  missingSelection,
  auditionActive,
};
