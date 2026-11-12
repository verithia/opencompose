/**
 * The preferences bundle store: what it reads out of the live stores, that it
 * never exports a credential, and that applying one leaf at a time reports the
 * ones a store refuses instead of dropping them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface FakeDevice {
  key: string;
  name: string;
  is_default: boolean;
  channels: number;
  sample_rates: number[];
  default_rate: number;
  buffer_frames: [number, number] | null;
}

const h = vi.hoisted(() => {
  const calls: string[] = [];
  const devices: FakeDevice[] = [
    {
      key: "coreaudio:builtin",
      name: "Built-in Output",
      is_default: true,
      channels: 2,
      sample_rates: [44100, 48000],
      default_rate: 48000,
      buffer_frames: [128, 1024],
    },
    {
      key: "coreaudio:usb",
      name: "USB Interface",
      is_default: false,
      channels: 2,
      sample_rates: [44100],
      default_rate: 44100,
      buffer_frames: [64, 512],
    },
    {
      // Reports no configuration at all: the rate and period pickers stay open.
      key: "coreaudio:mystery",
      name: "Mystery Interface",
      is_default: false,
      channels: 2,
      sample_rates: [],
      default_rate: 44100,
      buffer_frames: null,
    },
  ];
  const ports = [{ index: 0, name: "Keystation 49", id: "keystation" }];
  const state = {
    outputDevice: "coreaudio:builtin" as string | null,
    rate: 48000 as number | null,
    buffer: 256 as number | null,
    notices: [] as { kind: string; [key: string]: unknown }[],
  };
  return {
    calls,
    devices,
    ports,
    state,
    exportRecord: null as Record<string, unknown> | null,
    midi: { input: "Keystation 49" as string | null, monitor: true },
  };
});

function audioView(notices = h.state.notices) {
  const device = h.devices.find((entry) => entry.key === h.state.outputDevice) ?? null;
  return {
    status: {
      selection: {
        output_device: h.state.outputDevice,
        output_device_name: device?.name ?? null,
        sample_rate: h.state.rate,
        buffer_frames: h.state.buffer,
      },
      opened: device
        ? {
          key: device.key,
          name: device.name,
          sample_rate: h.state.rate ?? device.default_rate,
          buffer_frames: h.state.buffer,
          buffer_fixed: false,
        }
        : null,
      devices: h.devices,
      default_output: "coreaudio:builtin",
      missing_device: false,
      notices,
      failure: null,
      stream_open: false,
    },
    callback_frames: 0,
    latency_frames: 0,
    latency_seconds: 0,
    underruns: 0,
  };
}

vi.mock("../../stores/theme", () => ({
  themeStore: {
    catalog: [
      {
        id: "catppuccin",
        label: "Catppuccin",
        variants: [
          {
            id: "mocha",
            label: "Mocha",
            css: "catppuccin-mocha",
            colors: { bg: "#1e1e2e", surface: "#313244", accent: "#cba6f7", rail: "#89b4fa" },
          },
          {
            id: "latte",
            label: "Latte",
            css: "catppuccin-latte",
            colors: { bg: "#eff1f5", surface: "#ccd0da", accent: "#8839ef", rail: "#1e66f5" },
          },
        ],
      },
    ],
    family: () => ({ id: "catppuccin" }),
    variant: () => ({ id: "mocha" }),
    setTheme: (family: string, variant: string) => {
      h.calls.push(`theme:${family}/${variant}`);
    },
  },
}));

vi.mock("../../stores/i18n", () => ({
  LOCALES: ["en", "zh-Hans", "zh-Hant", "ja", "es"],
  i18n: {
    locale: () => "zh-Hans",
    setLocale: (locale: string) => {
      h.calls.push(`locale:${locale}`);
    },
  },
}));

vi.mock("../../stores/uiScale", () => ({
  uiScaleStore: {
    scale: () => 1.2,
    set: (scale: number) => {
      h.calls.push(`scale:${scale}`);
    },
  },
}));

vi.mock("../../stores/audioIo", () => ({
  audioIoStore: {
    status: () => audioView().status,
    devices: () => h.devices,
    error: () => null,
    readStatus: async () => audioView(),
    selectOutput: async (key: string) => {
      h.calls.push(`audio.output:${key}`);
      h.state.outputDevice = key;
      return audioView();
    },
    selectRate: async (rate: number) => {
      h.calls.push(`audio.rate:${rate}`);
      h.state.rate = rate;
      return audioView();
    },
    setBuffer: async (frames: number) => {
      h.calls.push(`audio.buffer:${frames}`);
      h.state.buffer = frames;
      return audioView();
    },
  },
}));

vi.mock("../../stores/midiLive", () => ({
  midiStore: {
    config: () => ({
      input: h.midi.input,
      monitor: h.midi.monitor,
      active: false,
      audition_active: false,
      error: null,
    }),
    ports: () => ({ supported: true, inputs: h.ports, outputs: [], error: null }),
    selectInput: async (name: string | null) => {
      h.calls.push(`midi.input:${name}`);
      h.midi.input = name;
      return { input: name, monitor: h.midi.monitor, active: false, audition_active: false, error: null };
    },
    setMonitor: async (enabled: boolean) => {
      h.calls.push(`midi.monitor:${enabled}`);
      h.midi.monitor = enabled;
      return { input: h.midi.input, monitor: enabled, active: false, audition_active: false, error: null };
    },
  },
}));

vi.mock("../../stores/grid", () => ({
  GRID_OPTIONS: [
    { value: 1, label: "1/4" },
    { value: 2, label: "1/8" },
    { value: 3, label: "1/8 triplet" },
    { value: 4, label: "1/16" },
    { value: 6, label: "1/16 triplet" },
    { value: 8, label: "1/32" },
  ],
  gridStore: {
    division: () => 6,
    setDivision: (division: number) => {
      h.calls.push(`grid:${division}`);
    },
  },
}));

vi.mock("../../stores/workspace", () => ({
  workspaceStore: {
    recents: () => [
      { name: "songs", path: "/Users/me/songs", opened: "2026-10-30T09:00:00Z" },
      { name: "albums", path: "/Users/me/albums", opened: "2026-10-29T09:00:00Z" },
    ],
  },
}));

vi.mock("../../stores/exportAudio", () => ({
  readLastUsedExport: () => h.exportRecord,
  mergeLastUsedExport: (patch: Record<string, unknown>) => {
    h.calls.push(`export:${JSON.stringify(patch)}`);
    h.exportRecord = { ...(h.exportRecord ?? {}), ...patch };
  },
}));

// jsdom does not expose Web Storage in this environment; the stores need a map.
const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      get length() {
        return storage.size;
      },
      key: (index: number) => Array.from(storage.keys())[index] ?? null,
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => {
        storage.set(key, String(value));
      },
      removeItem: (key: string) => {
        storage.delete(key);
      },
      clear: () => {
        storage.clear();
      },
    },
  });
}

import { invokeMock } from "../setup";
import {
  APP_VERSION,
  PREFERENCES_SCHEMA,
  applyPreferences,
  buildPreferencesBundle,
  countBundleSettings,
  importPreferencesFile,
  type PreferencesBundle,
} from "../../stores/preferencesBundle";

function bundle(overrides: Partial<PreferencesBundle> = {}): PreferencesBundle {
  return {
    schema: PREFERENCES_SCHEMA,
    exported_at: "2026-10-31T10:00:00.000Z",
    app_version: APP_VERSION,
    theme: { family: "catppuccin", variant: "latte" },
    locale: "ja",
    ui_scale: 1.0,
    audio: { output_device: "coreaudio:usb", sample_rate: 44100, buffer_size: 256 },
    midi: { input: "Keystation 49", channel: null, monitor: true },
    export: {
      format: "wav",
      bit_depth: 16,
      flac_level: 5,
      normalise: false,
      mp3_bitrate: null,
      mp3_vbr: null,
    },
    grid: { subdivision: 4 },
    keymap: { save: "Cmd+S" },
    workspace: { recent_directories: ["/Users/me/songs"] },
    ...overrides,
  };
}

beforeEach(() => {
  h.calls.length = 0;
  h.state.outputDevice = "coreaudio:builtin";
  h.state.rate = 48000;
  h.state.buffer = 256;
  h.state.notices = [];
  h.midi = { input: "Keystation 49", monitor: true };
  h.exportRecord = { container: "flac", depth: "i24", flacLevel: 7, normalise: true };
  window.localStorage?.clear();
  window.localStorage?.setItem("opencompose.keymap", JSON.stringify({ save: "Cmd+S" }));
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-31T10:00:00.000Z"));
});

afterEach(() => {
  vi.useRealTimers();
});

describe("building the bundle", () => {
  it("reads every setting out of the store that owns it", () => {
    expect(buildPreferencesBundle()).toEqual({
      schema: "opencompose.preferences.v1",
      exported_at: "2026-10-31T10:00:00.000Z",
      app_version: "0.1.0",
      theme: { family: "catppuccin", variant: "mocha" },
      locale: "zh-Hans",
      ui_scale: 1.2,
      audio: { output_device: "coreaudio:builtin", sample_rate: 48000, buffer_size: 256 },
      midi: { input: "Keystation 49", channel: null, monitor: true },
      export: {
        format: "flac",
        bit_depth: 24,
        flac_level: 7,
        normalise: true,
        mp3_bitrate: null,
        mp3_vbr: null,
      },
      grid: { subdivision: 6 },
      keymap: { save: "Cmd+S" },
      workspace: { recent_directories: ["/Users/me/songs", "/Users/me/albums"] },
    });
  });

  it("carries the keymap opaque and never a credential", () => {
    window.localStorage?.setItem(
      "opencompose.keymap",
      JSON.stringify({ save: "Cmd+S", "toggle key": "K" }),
    );
    const exported = buildPreferencesBundle();
    expect(exported.keymap).toEqual({ save: "Cmd+S", "toggle key": "K" });
    const keys = JSON.stringify(exported).match(/"[^"]+"(?=\s*:)/g) ?? [];
    const secret = keys
      .map((key) => key.replaceAll('"', "").toLowerCase())
      .filter((key) => /(^|[_\-.])(key|token|secret|password)s?($|[_\-.]|$)/.test(key));
    expect(secret, `exported names: ${keys.join(", ")}`).toEqual([]);
    expect(JSON.stringify(exported)).not.toMatch(/api[_-]?key|token|password/i);
  });

  it("counts the settings it carries", () => {
    expect(countBundleSettings(bundle())).toBe(15);
  });

  it("omits the export section when this device stored no choices", () => {
    h.exportRecord = null;
    expect(buildPreferencesBundle().export).toBeNull();
  });

  it("records the MP3 bitrate and VBR of the last export", () => {
    h.exportRecord = {
      container: "mp3",
      depth: "i16",
      mp3Bitrate: 320,
      mp3Vbr: 4,
      normalise: false,
    };
    const exported = buildPreferencesBundle();
    expect(exported.export).toEqual({
      format: "mp3",
      bit_depth: 16,
      flac_level: 5,
      normalise: false,
      mp3_bitrate: 320,
      mp3_vbr: 4,
    });
    expect(JSON.parse(JSON.stringify(exported)).export).toMatchObject({
      format: "mp3",
      mp3_bitrate: 320,
      mp3_vbr: 4,
    });
  });

  it("records CBR when the last MP3 export chose no VBR quality", () => {
    h.exportRecord = { container: "mp3", depth: "i16", mp3Bitrate: 192 };
    expect(buildPreferencesBundle().export).toMatchObject({
      format: "mp3",
      mp3_bitrate: 192,
      mp3_vbr: null,
    });
  });
});

describe("applying a bundle", () => {
  it("writes each leaf through its store, in order", async () => {
    const result = await applyPreferences(bundle());
    expect(h.calls).toEqual([
      "theme:catppuccin/latte",
      "locale:ja",
      "scale:1",
      "audio.output:coreaudio:usb",
      "audio.rate:44100",
      "audio.buffer:256",
      "midi.input:Keystation 49",
      "midi.monitor:true",
      'export:{"container":"wav","depth":"i16","flacLevel":5,"normalise":false}',
      "grid:4",
    ]);
    expect(result.applied).toEqual([
      "theme",
      "locale",
      "ui_scale",
      "audio.output_device",
      "audio.sample_rate",
      "audio.buffer_size",
      "midi.input",
      "midi.monitor",
      "export.format",
      "export.bit_depth",
      "export.flac_level",
      "export.normalise",
      "grid.subdivision",
      "keymap",
    ]);
    expect(result.skipped).toEqual([
      {
        field: "workspace.recent_directories",
        reason:
          "this build cannot write the recent-directory list; open 1 folder(s) once to re-add them",
      },
    ]);
    expect(window.localStorage?.getItem("opencompose.keymap")).toBe(
      JSON.stringify({ save: "Cmd+S" }),
    );
  });

  it("reports a theme this build does not ship", async () => {
    const result = await applyPreferences(
      bundle({ theme: { family: "solarized", variant: "dark" } }),
    );
    expect(result.applied).not.toContain("theme");
    expect(result.skipped[0]).toEqual({
      field: "theme",
      reason: "theme 'solarized/dark' is not in this build's catalog",
    });
    expect(h.calls).not.toContain("theme:solarized/dark");
  });

  it("reports a locale this build does not ship", async () => {
    const result = await applyPreferences(bundle({ locale: "klingon" }));
    expect(result.skipped[0].field).toBe("locale");
    expect(result.skipped[0].reason).toContain("klingon");
    expect(h.calls).not.toContain("locale:klingon");
  });

  it("reports a sample rate the device does not offer", async () => {
    const result = await applyPreferences(
      bundle({ audio: { output_device: "coreaudio:builtin", sample_rate: 96000, buffer_size: 256 } }),
    );
    expect(result.skipped).toContainEqual({
      field: "audio.sample_rate",
      reason: "96000 Hz is not offered by 'Built-in Output'",
    });
    expect(h.calls).not.toContain("audio.rate:96000");
  });

  it("reports a substituted rate and a clamped period", async () => {
    h.state.outputDevice = "coreaudio:mystery";
    h.state.notices = [{ kind: "rate_substituted", requested: 88200, chosen: 48000 }];
    const rate = await applyPreferences(
      bundle({ audio: { output_device: null, sample_rate: 88200, buffer_size: null } }),
    );
    expect(rate.skipped).toContainEqual({
      field: "audio.sample_rate",
      reason: "the device opened 48000 Hz instead",
    });

    h.state.notices = [
      { kind: "buffer_clamped", requested: 64, chosen: 128, min: 128, max: 1024 },
    ];
    const buffer = await applyPreferences(
      bundle({ audio: { output_device: null, sample_rate: null, buffer_size: 64 } }),
    );
    expect(buffer.skipped).toContainEqual({
      field: "audio.buffer_size",
      reason: "the device clamped the period to 128 frames",
    });
  });

  it("reports a device that is not connected", async () => {
    const result = await applyPreferences(
      bundle({ audio: { output_device: "coreaudio:absent", sample_rate: null, buffer_size: null } }),
    );
    expect(result.skipped[0]).toEqual({
      field: "audio.output_device",
      reason: "output device 'coreaudio:absent' is not connected",
    });
    expect(h.calls).not.toContain("audio.output:coreaudio:absent");
  });

  it("reports a MIDI input that is not connected", async () => {
    const result = await applyPreferences(
      bundle({ midi: { input: "Launchkey", channel: null, monitor: null } }),
    );
    expect(result.skipped).toContainEqual({
      field: "midi.input",
      reason: "MIDI input 'Launchkey' is not connected",
    });
    expect(h.calls).not.toContain("midi.input:Launchkey");
  });

  it("reports the MIDI channel and the recent directories as gaps", async () => {
    const result = await applyPreferences(
      bundle({ midi: { input: null, channel: 3, monitor: null } }),
    );
    expect(result.skipped).toContainEqual({
      field: "midi.channel",
      reason: "this build has no MIDI channel preference to write",
    });
    expect(result.skipped.some((entry) => entry.field === "workspace.recent_directories")).toBe(true);
  });

  it("reports an export choice the dialog cannot use", async () => {
    const result = await applyPreferences(
      bundle({
        export: {
          format: "ogg",
          bit_depth: 12,
          flac_level: 9,
          normalise: true,
          mp3_bitrate: null,
          mp3_vbr: null,
        },
      }),
    );
    expect(result.skipped.map((entry) => entry.field)).toEqual([
      "export.format",
      "export.bit_depth",
      "export.flac_level",
      "workspace.recent_directories",
    ]);
    expect(result.applied).toContain("export.normalise");
    expect(result.applied).not.toContain("export.format");
    expect(h.calls).toContain('export:{"normalise":true}');
  });

  it("applies an imported MP3 format, bitrate and VBR", async () => {
    const result = await applyPreferences(
      bundle({
        export: {
          format: "mp3",
          bit_depth: 16,
          flac_level: 5,
          normalise: false,
          mp3_bitrate: 320,
          mp3_vbr: 4,
        },
      }),
    );
    expect(h.calls).toContain(
      'export:{"container":"mp3","flacLevel":5,"normalise":false,"mp3Bitrate":320,"mp3Vbr":4}',
    );
    expect(result.applied).toEqual(expect.arrayContaining([
      "export.format",
      "export.mp3_bitrate",
      "export.mp3_vbr",
    ]));
    // MP3 consumes 16-bit PCM, so the carried depth carries no choice.
    expect(result.applied).not.toContain("export.bit_depth");
    expect(h.exportRecord).toMatchObject({
      container: "mp3",
      mp3Bitrate: 320,
      mp3Vbr: 4,
    });
  });

  it("skips an out-of-range MP3 bitrate or VBR with a reason", async () => {
    const result = await applyPreferences(
      bundle({
        export: {
          format: "mp3",
          bit_depth: null,
          flac_level: null,
          normalise: null,
          mp3_bitrate: 999,
          mp3_vbr: 12,
        },
      }),
    );
    expect(result.skipped).toEqual(expect.arrayContaining([
      { field: "export.mp3_bitrate", reason: "MP3 bitrate 999 kbps is outside 32–320" },
      { field: "export.mp3_vbr", reason: "MP3 VBR quality 12 is outside 0–9" },
    ]));
    expect(result.applied).toContain("export.format");
    expect(h.exportRecord).toMatchObject({ container: "mp3" });
    expect(h.exportRecord).not.toHaveProperty("mp3Bitrate");
    expect(h.exportRecord).not.toHaveProperty("mp3Vbr");
  });

  it("keeps the default bitrate for an MP3 bundle that carries none", async () => {
    const result = await applyPreferences(
      bundle({
        export: {
          format: "mp3",
          bit_depth: null,
          flac_level: null,
          normalise: null,
          mp3_bitrate: null,
          mp3_vbr: null,
        },
      }),
    );
    expect(result.applied).toEqual(expect.arrayContaining(["export.format", "export.mp3_vbr"]));
    expect(result.applied).not.toContain("export.mp3_bitrate");
    // No bitrate is written, so the dialog keeps its 192 kbps CBR default.
    expect(h.calls).toContain('export:{"container":"mp3","mp3Vbr":null}');
    expect(h.exportRecord).not.toHaveProperty("mp3Bitrate");
  });

  it("reports a grid division the timeline does not offer", async () => {
    const result = await applyPreferences(bundle({ grid: { subdivision: 5 } }));
    expect(result.skipped).toContainEqual({
      field: "grid.subdivision",
      reason: "subdivision 5 is not one of 1, 2, 3, 4, 6, 8",
    });
    expect(h.calls).not.toContain("grid:5");
  });

  it("leaves everything alone for a bundle that carries no settings", async () => {
    const empty = bundle({
      theme: null,
      locale: null,
      ui_scale: null,
      audio: null,
      midi: null,
      export: null,
      grid: null,
      keymap: null,
      workspace: null,
    });
    const result = await applyPreferences(empty);
    expect(result).toEqual({ applied: [], skipped: [] });
    expect(h.calls).toEqual([]);
  });
});

describe("the import command", () => {
  it("passes the chosen path to the backend validator", async () => {
    const preview = { bundle: bundle(), applied: [], skipped: [], errors: [] };
    invokeMock.mockResolvedValueOnce(preview);
    expect(await importPreferencesFile("/tmp/prefs.json")).toEqual(preview);
    expect(invokeMock).toHaveBeenCalledWith("preferences_import", { path: "/tmp/prefs.json" });
  });
});
