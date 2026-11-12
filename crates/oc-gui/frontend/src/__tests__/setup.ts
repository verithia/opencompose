/**
 * Test setup — mocks Tauri APIs, provides fake data factories and assertion helpers.
 */
import "@testing-library/jest-dom/vitest";
import { fireEvent } from "@solidjs/testing-library";
import { vi, type Mock } from "vitest";
import type { ProjectSnapshot, Project, Track, PatternDef, NoteEvent, Effect, AudioClip } from "../types/project";
import type { saveFile, TransportSnapshot } from "../stores/api";
import { DEFAULT_PROVIDERS } from "../ai/config";

// Existing suites exercise the DAW, not the first-run guide. Individual
// onboarding tests remove this marker before mounting the component. jsdom on
// this Node build can leave `window.localStorage` undefined; the store treats
// that as "do not trap the operator", so the guard is enough.
try {
  window.localStorage?.setItem("opencompose.onboarding.v1", JSON.stringify({ completed_at: "test" }));
} catch (_) {}


// ── AI config fixtures ──────────────────────────────────────────────────────

/** A saved provider in the snake_case shape `save_ai_config` reads back. */
export function rustAiProvider(provider: import("../ai/config").AiProvider) {
  return {
    id: provider.id,
    name: provider.name,
    kind: provider.kind,
    ...(provider.wire ? { wire: provider.wire } : {}),
    auth: provider.auth,
    ...(provider.authProvider ? { auth_provider: provider.authProvider } : {}),
    ...(provider.oauthAllowedHosts?.length ? { oauth_allowed_hosts: provider.oauthAllowedHosts } : {}),
    endpoint: provider.endpoint,
    model: provider.model,
    ...(provider.models?.length ? { models: provider.models } : {}),
    context_length: provider.contextLength,
    max_tokens: provider.maxTokens,
    temperature: provider.temperature,
    ...(provider.thinkingLevel ? { thinking_level: provider.thinkingLevel } : {}),
    capabilities: {
      text: provider.capabilities.text,
      audio_in: provider.capabilities.audioIn,
      audio_out: provider.capabilities.audioOut,
      native_tool_calling: provider.capabilities.nativeToolCalling,
      streaming: provider.capabilities.streaming,
    },
  };
}

/** A saved file holding the catalogue as the user's own rows. Suites that need
 * a configured install seed this; the product itself saves nothing on first run. */
export function aiConfigWire(
  providers: import("../ai/config").AiProvider[] = DEFAULT_PROVIDERS,
) {
  return {
    version: 2,
    active_provider: providers[0]?.id ?? "",
    providers: providers.map(rustAiProvider),
  };
}

// ── Fake data factories ─────────────────────────────────────────────────────

export function TRANSPORT_STOPPED() {
  return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
}
export function TRANSPORT_PLAYING() {
  return { playing: true, position_beats: 4.5, position_seconds: 2.1, duration_seconds: 30, did_render: true, warnings: [] };
}

export function makeNote(overrides?: Partial<NoteEvent>): NoteEvent {
  return {
    start_beat: 0, explicit_position: false,
    pitch: { note: "C", accidental: "Natural", octave: 4 },
    duration: { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] },
    dynamics: "MF", velocity: null, articulation: null, vibrato: null,
    expression: null, pitch_bend: null, muted: false, grace: false,
    text: null, modifiers: [],
    ...overrides,
  };
}

/** Empty audio-IO snapshot; the Toolbar's audio section renders its empty state. */
export const AUDIO_IO_EMPTY = {
  status: {
    selection: {
      output_device: null,
      output_device_name: null,
      sample_rate: null,
      buffer_frames: null,
    },
    opened: null,
    devices: [] as unknown[],
    default_output: null,
    missing_device: false,
    notices: [] as unknown[],
    failure: null,
    stream_open: false,
  },
  callback_frames: 0,
  latency_frames: 0,
  latency_seconds: 0,
  underruns: 0,
};

export const TEST_PARAMETER_TARGET_STAMP = "fixture-parameter-target" as import("../types/project").ParameterTargetStamp;

export function makeSnapshot(overrides?: Partial<Project>): ProjectSnapshot {
  const proj: Project = {
    name: "Test Project",
    properties: { tempo: 128, time_signature: { numerator: 4, denominator: 4 }, key: null, swing: null, tempo_map: [], time_sig_map: [], key_map: [] },
    presets: [{ name: "Piano 1", source: "soundfont", plugin: null, program: 0, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
    patterns: [],
    tracks: [],
    buses: [],
    master: null,
    sections: [],
    ...overrides,
  };
  return {
    project: proj,
    dsl_source: '@project "Test Project"\n  tempo: 128\n  time_signature: 4/4\n\n',
    file_path: null,
    dirty: false,
    track_count: proj.tracks.length,
    // Fixtures share one revision, so a test that feeds two responses in a row
    // is not refused by the order it happens to run in.
    revision: 0,
    parameter_target_stamp: TEST_PARAMETER_TARGET_STAMP,
    duration_seconds: 0,
  };
}

/** The save command returns its path alongside any repointed project snapshot. */
export function makeSavedFile(path: string, snapshot: ProjectSnapshot | null = null): Awaited<ReturnType<typeof saveFile>> {
  return { path, snapshot };
}

/** A native audition reports its state through the transport snapshot. */
export function makeAuditionTransport(preset: string, paused = false): TransportSnapshot {
  return {
    ...TRANSPORT_STOPPED(),
    audition_active: !paused,
    audition: { preset, status: "ready", ready_ms: 1, instances: 1, loads: 1, error: null, paused, position_seconds: 0 },
  };
}

/** Click a preset in the left list; the synth header repeats the same name. */
/** Open every collapsed step group so a test can inspect its tool rows. */
export function openStepGroups(root: ParentNode = document) {
  for (const group of root.querySelectorAll<HTMLDetailsElement>(".ai-step-group:not([open])")) {
    group.open = true;
    group.dispatchEvent(new Event("toggle"));
  }
}

export function selectPreset(container: HTMLElement, name: string) {
  const node = [...container.querySelectorAll(".preset-list-item-name")]
    .find((element) => element.textContent === name);
  if (!node) throw new Error(`no preset list row for ${name}`);
  fireEvent.click(node);
}

export function makeTrack(name: string, idx: number): Track {
  return {
    name,
    instrument: { category: "piano", name: "acoustic_grand" },
    preset: idx === 0 ? "Piano 1" : null,
    engine: null,
    mix: { volume_db: 0, pan: 0, sends: [], mute: false, solo: false },
    effects: [],
    phrases: [{
      name: "melody",
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
      events: [makeNote()],
    }],
    patterns: [],
    inline_patterns: [],
    clips: [],
    automation: [],
    engine_params: null,
  };
}

export function makePattern(name: string): PatternDef {
  return {
    name,
    instrument: { category: "drums", name: "acoustic_kit" },
    preset: null, engine: null, transpose: null, swing: null,
    effects: [], color: null,
    events: [],
    hits: [
      { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
      { drum: "snare", position: { beat: 3, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
    ],
  };
}

export function makeAudioClip(overrides?: Partial<AudioClip>): AudioClip {
  return {
    name: "hit",
    track: "tape",
    file: "media/hit.wav",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    },
    offset_seconds: 0,
    gain_db: 0,
    fade_in_seconds: 0,
    fade_out_seconds: 0,
    ...overrides,
  };
}

export function makeEffect(type: string, params: [string, unknown][] = []): Effect {
  return {
    effect_type: type as Effect["effect_type"],
    params: params as Effect["params"],
  };
}

let nextOwnedFixture = 0;
/** Explicit ownership replies for component IPC fixtures; unknown commands stay strict. */
export function createOwnedUndoFixture(targetStamp: () => import("../types/project").ParameterTargetStamp | null = () => TEST_PARAMETER_TARGET_STAMP) {
  const prefix = `fixture-${++nextOwnedFixture}`;
  let next = 0;
  const active = new Set<string>();
  const scopes = new Set(["project", "mixer_routing", "mixer_levels", "preset", "automation", "plugin_state", "transport"]);
  const requireToken = (args: unknown) => {
    const token = (args as { token?: unknown } | undefined)?.token;
    if (typeof token !== "string" || !active.has(token)) throw new Error("Owned mutation requires its live fixture token");
    return token;
  };
  return {
    route(command: string, args?: unknown, observe?: (phase: "begin" | "end") => void): { value: string | boolean } | null {
      if (command === "begin_owned_undo_gesture" || command === "begin_parameter_gesture") {
        if (command === "begin_parameter_gesture" && (typeof (args as { expectedStamp?: unknown } | undefined)?.expectedStamp !== "string"
          || (args as { expectedStamp?: unknown } | undefined)?.expectedStamp !== targetStamp())) {
          throw new Error("Parameter fixture target admission changed");
        }
        const scope = (args as { scope?: unknown } | undefined)?.scope;
        if (typeof scope !== "string" || !scopes.has(scope)) throw new Error("Unsupported fixture undo scope");
        const token = `${prefix}-${++next}`;
        active.add(token);
        observe?.("begin");
        return { value: token };
      }
      if (command === "end_owned_undo_gesture") {
        const token = (args as { token?: unknown } | undefined)?.token;
        if (typeof token !== "string") throw new Error("Fixture undo close requires a string token");
        observe?.("end");
        return { value: active.delete(token) };
      }
      return null;
    },
    mutation(command: string, args?: unknown): string {
      const legacyCommands: Record<string, string> = {
        set_preset_param_owned: "set_preset_param",
        set_preset_params_owned: "set_preset_params",
        randomize_preset_owned: "randomize_preset",
        synth_macro_learn_owned: "synth_macro_learn",
      };
      const legacy = legacyCommands[command];
      if (!legacy) return command;
      requireToken(args);
      return legacy;
    },
  };
}

// ── Mock @tauri-apps/api/core ──────────────────────────────────────────────

vi.mock("@tauri-apps/api/core", () => {
  let nextGesture = 0;
  const ownedGestures = new Set<string>();
  const undoScopes = new Set(["project", "mixer_routing", "mixer_levels", "preset", "automation", "plugin_state", "transport"]);
  const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
    if (cmd === "claim_web_audio") return true;
    if (cmd === "release_web_audio") return undefined;
    if (cmd === "scan_plugins") return [];
    if (cmd === "plugin_catalog") return { platform: "macos-aarch64", entries: [], installed: [] };
    if (cmd === "plugin_store_open") return undefined;
    if (cmd === "export_capabilities") return { mp3: true };
    if (cmd === "plugin_store_install") return { status: "installed", path: "", message: "" };
    if (cmd === "take_startup_project") return null;
    if (cmd === "get_project") return makeSnapshot();
    // The panel asks once, when it mounts; no test previews unless it mocks this.
    if (cmd === "preview_state") return null;
    // No stranded batch unless a test opens one.
    if (cmd === "ai_batch_recover") return null;
    if (cmd === "list_recoveries") return [];
    if (cmd === "discard_recovery") return undefined;
    if (cmd === "get_undo_history") return [];
    if (cmd === "begin_undo_gesture" || cmd === "end_undo_gesture") return undefined;
    if (cmd === "begin_owned_undo_gesture" || cmd === "begin_parameter_gesture") {
      if (cmd === "begin_parameter_gesture" && (_args as { expectedStamp?: unknown } | undefined)?.expectedStamp !== TEST_PARAMETER_TARGET_STAMP) {
        throw new Error("Parameter target admission changed");
      }
      const scope = (_args as { scope?: unknown } | undefined)?.scope;
      if (typeof scope !== "string" || !undoScopes.has(scope)) {
        throw new Error("begin_owned_undo_gesture requires a supported scope");
      }
      const token = `test-undo-owner-${++nextGesture}`;
      ownedGestures.add(token);
      return token;
    }
    if (cmd === "end_owned_undo_gesture") {
      const token = (_args as { token?: unknown } | undefined)?.token;
      if (typeof token !== "string") throw new Error("end_owned_undo_gesture requires a string token");
      return ownedGestures.delete(token);
    }
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_midi_config") return { input: null, monitor: false, active: false, audition_active: false, error: null };
    if (cmd === "list_audio_io" || cmd === "get_audio_io") return AUDIO_IO_EMPTY;
    if (cmd === "set_audio_io") {
      const args = (_args ?? {}) as { selection?: Record<string, unknown> };
      return {
        ...AUDIO_IO_EMPTY,
        status: {
          ...AUDIO_IO_EMPTY.status,
          selection: { ...AUDIO_IO_EMPTY.status.selection, ...(args.selection ?? {}) },
        },
      };
    }
    if (cmd === "list_midi_ports") return { supported: true, inputs: [], outputs: [], error: null };
    if (cmd === "get_midi_activity") return { running: false, events: 0, last: null, error: null, audition_active: false };
    if (cmd === "set_midi_config") {
      const args = (_args ?? {}) as { input?: string | null; monitor?: boolean };
      return {
        input: args.input ?? null,
        monitor: Boolean(args.monitor),
        active: Boolean(args.monitor && args.input),
        audition_active: false,
        error: null,
      };
    }
    throw new Error(`Unexpected Tauri command in test: ${cmd}. Supply an explicit mock handler.`);
  }) as ReturnType<typeof vi.fn>;
  // AiPanel streams through a Channel; the real one needs __TAURI_INTERNALS__.
  class Channel {
    id = 0;
    onmessage: (message: unknown) => void;
    constructor(onmessage?: (message: unknown) => void) {
      this.onmessage = onmessage ?? (() => {});
    }
    cleanupCallback() {}
  }
  return { invoke: invokeMock, Channel, isTauri: () => false,
    convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}` };
});

// ── Other Tauri mocks ─────────────────────────────────────────────────────

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    label: "main",
    setTitle: vi.fn(),
    close: vi.fn(),
    onCloseRequested: vi.fn(async () => () => {}),
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
  save: vi.fn(),
  ask: vi.fn(),
  message: vi.fn(),
}));

// The AI stats store subscribes to one backend event; jsdom has no event bridge.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

// ── Export the mocked invoke for assertions ───────────────────────────────

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask, message, open, save } from "@tauri-apps/plugin-dialog";
export const invokeMock = invoke as Mock;
export const listenMock = listen as Mock;
export const openDialogMock = open as Mock;
export const saveDialogMock = save as Mock;
export const askDialogMock = ask as Mock;
export const messageDialogMock = message as Mock;

/**
 * Assert a specific command was invoked with specific args.
 * Uses vitest's built-in call tracking on the mock function.
 */
export function expectInvoked(cmd: string, args?: Record<string, unknown>) {
  const allCalls = invokeMock.mock.calls as [string, unknown][];
  const calls = allCalls.filter(([c]) => c === cmd);
  if (calls.length === 0) {
    const actualCmds = allCalls.map(([c]) => c).join(", ");
    throw new Error(`Expected invoke("${cmd}") to be called, but it was not.\nActual calls: ${actualCmds}`);
  }
  if (args !== undefined) {
    const lastArgs = calls[calls.length - 1][1] as Record<string, unknown> | undefined;
    for (const [key, value] of Object.entries(args)) {
      const actual = lastArgs?.[key];
      if (actual !== value) {
        throw new Error(
          `Expected invoke("${cmd}") arg "${key}" to be ${JSON.stringify(value)}, got ${JSON.stringify(actual)}\nFull args: ${JSON.stringify(lastArgs)}`
        );
      }
    }
  }
}

/**
 * Get the last call args for a specific command. Useful for complex assertions.
 */
export function getLastCallArgs(cmd: string): Record<string, unknown> | undefined {
  const allCalls = invokeMock.mock.calls as [string, unknown][];
  for (let i = allCalls.length - 1; i >= 0; i--) {
    if (allCalls[i][0] === cmd) return allCalls[i][1] as Record<string, unknown>;
  }
  return undefined;
}

/** The listbox a trigger actually opened: its own, never another panel's. */
function openedListbox(trigger: HTMLElement): HTMLElement | null {
  const id = trigger.getAttribute("aria-controls");
  if (id) {
    const own = document.getElementById(id);
    if (own) return own;
  }
  const lists = document.querySelectorAll<HTMLElement>('[role="listbox"]');
  return lists.length ? lists[lists.length - 1] : null;
}

/** Open a dropdown trigger and click the option with this exact label. */
export function chooseDropdown(trigger: HTMLElement, optionLabel: string) {
  fireEvent.click(trigger);
  const options = [...(openedListbox(trigger)?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];
  const option = options.find(
    (element) => element.querySelector(".oc-dropdown-option-label")?.textContent === optionLabel,
  );
  if (!option) {
    const labels = options.map((element) => element.querySelector(".oc-dropdown-option-label")?.textContent);
    throw new Error(`Dropdown option "${optionLabel}" not found. Options: ${labels.join(", ")}`);
  }
  fireEvent.click(option);
}

/** Open a dropdown, read its option values in order, then dismiss it. */
export function dropdownOptionValues(trigger: HTMLElement): string[] {
  fireEvent.click(trigger);
  const values = [...(openedListbox(trigger)?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])]
    .map((element) => element.dataset.value ?? "");
  fireEvent.pointerDown(document.body);
  return values;
}
