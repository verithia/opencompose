/**
 * Recording: the armed track, what a press will record, the take in progress,
 * and why a press is refused. Shapes mirror `record::RecordState` in the
 * backend.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { startRecording, stopRecording, type Quantise } from "./api";
import { i18n } from "./i18n";
import { onTransportSnapshot, store } from "./project";

/** What the armed track records: MIDI from its instrument, or the input. */
export type RecordMode = "audio" | "midi";

export interface MidiRecordState {
  target: string | null;
  quantise: Quantise;
  overdub: boolean;
  input: string | null;
}

export interface RecordState {
  armed: string | null;
  recording: boolean;
  seconds: number;
  report: string | null;
  disabled_reason: string | null;
  count_in_remaining_frames: number;
  mode: RecordMode;
  midi: MidiRecordState;
  audio_device: string | null;
}

const EMPTY_MIDI: MidiRecordState = {
  target: null,
  quantise: "off",
  overdub: false,
  input: null,
};

const DEFAULTS: RecordState = {
  armed: null,
  recording: false,
  seconds: 0,
  report: null,
  disabled_reason: null,
  count_in_remaining_frames: 0,
  mode: "audio",
  midi: EMPTY_MIDI,
  audio_device: null,
};

const [state, setState] = createSignal<RecordState>(DEFAULTS);
const [pending, setPending] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

/** Adopt a transport snapshot's record fields. */
export function adoptRecord(next: Partial<RecordState> | null | undefined): void {
  if (!next || typeof next.recording !== "boolean") return;
  setState({
    armed: next.armed ?? null,
    recording: next.recording,
    seconds: next.seconds ?? 0,
    report: next.report ?? null,
    disabled_reason: next.disabled_reason ?? null,
    count_in_remaining_frames: next.count_in_remaining_frames ?? 0,
    mode: next.mode === "midi" ? "midi" : "audio",
    midi: {
      target: next.midi?.target ?? null,
      quantise: next.midi?.quantise ?? "off",
      overdub: next.midi?.overdub === true,
      input: next.midi?.input ?? null,
    },
    audio_device: next.audio_device ?? null,
  });
}

// The take is drained by `get_transport_state`, so the 20 Hz poll is the
// record clock: every reply that carries `record` updates this store.
onTransportSnapshot((snapshot) => {
  if (snapshot.record) adoptRecord(snapshot.record);
});

async function arm(track: string | null): Promise<void> {
  setPending(true);
  try {
    const snapshot = await invoke<{ record: RecordState }>("arm_track", { track });
    adoptRecord(snapshot.record);
    setError(null);
  } catch (failure) {
    setError(String(failure));
  } finally {
    setPending(false);
  }
}

async function toggleArm(track: string): Promise<void> {
  await arm(state().armed === track ? null : track);
}

async function start(): Promise<unknown> {
  setPending(true);
  setError(null);
  try {
    const snapshot = await startRecording();
    adoptRecord(snapshot.record);
    // The take ring is drained only by the transport poll, so a record press
    // must start it; the poll adopts the running transport from here on.
    store.adoptTransport(snapshot);
    if (snapshot.playing || snapshot.record?.recording) store.startTransportPoll();
    return snapshot;
  } catch (failure) {
    // The backend words the refusal; show it verbatim.
    setError(String(failure));
    return null;
  } finally {
    setPending(false);
  }
}

async function stop(): Promise<unknown> {
  setPending(true);
  try {
    const snapshot = await stopRecording();
    adoptRecord(snapshot.record);
    store.adoptTransport(snapshot);
    // A committed take changes the project: adopt it so the piano roll and the
    // clip lane show the new notes at once, and open the pattern it wrote.
    if (snapshot.project) {
      store.applyTakeSnapshot(snapshot.project);
      const target = state().midi.target;
      if (state().mode === "midi" && target) store.setSelectedPatternName(target);
    }
    setError(null);
    return snapshot;
  } catch (failure) {
    setError(String(failure));
    return null;
  } finally {
    setPending(false);
  }
}

/** Change the MIDI input quantise; the reply is the settings now in force. */
async function setQuantise(quantise: Quantise): Promise<void> {
  setPending(true);
  setError(null);
  try {
    adoptMidi(await invoke<{ target: string | null; quantise: Quantise; overdub: boolean; input: string | null }>(
      "set_midi_record_settings",
      { quantise, overdub: null },
    ));
  } catch (failure) {
    setError(String(failure));
  } finally {
    setPending(false);
  }
}

/** Keep the notes already in the pattern instead of replacing the span. */
async function setOverdub(overdub: boolean): Promise<void> {
  setPending(true);
  setError(null);
  try {
    adoptMidi(await invoke<{ target: string | null; quantise: Quantise; overdub: boolean; input: string | null }>(
      "set_midi_record_settings",
      { quantise: null, overdub },
    ));
  } catch (failure) {
    setError(String(failure));
  } finally {
    setPending(false);
  }
}

/** Adopt the settings reply without touching the armed track or the take. */
function adoptMidi(next: {
  target: string | null;
  quantise: Quantise;
  overdub: boolean;
  input: string | null;
}): void {
  if (!next || typeof next.overdub !== "boolean") return;
  setState((current) => ({ ...current, midi: { ...current.midi, ...next } }));
}

const armed = () => state().armed;
const recording = () => state().recording;
const seconds = () => state().seconds;
const report = () => state().report;
const disabledReason = () => state().disabled_reason;
const mode = () => state().mode;
const midi = () => state().midi;
const quantise = () => state().midi.quantise;
const overdub = () => state().midi.overdub;

/**
 * What a record press will do, as the arm chip says it: the pattern a MIDI take
 * lands in, or the device an audio take captures. `null` while nothing is armed.
 */
const armLine = (): string | null => {
  const current = state();
  if (!current.armed || current.recording) return null;
  if (current.mode === "midi") {
    return current.midi.target
      ? i18n.t("toolbar.armChipMidi", { pattern: current.midi.target })
      : i18n.t("toolbar.armChipMidiNew", { track: current.armed });
  }
  return current.audio_device
    ? i18n.t("toolbar.armChipAudio", { device: current.audio_device })
    : i18n.t("toolbar.armChipAudioUnknown");
};

/** `Recording 00:03` — the transport's pulsing status text. */
const recordingLabel = (): string => {
  const total = Math.max(0, Math.floor(state().seconds));
  const minutes = Math.floor(total / 60);
  const rest = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
};

function dispose(): void {
  setState(DEFAULTS);
  setPending(false);
  setError(null);
}

export const recordStore = {
  state,
  armed,
  recording,
  seconds,
  report,
  disabledReason,
  mode,
  midi,
  quantise,
  overdub,
  armLine,
  recordingLabel,
  pending,
  error,
  adoptRecord,
  arm,
  toggleArm,
  start,
  stop,
  setQuantise,
  setOverdub,
  dispose,
};
