/**
 * The export store action: every chosen option reaches the backend, and the
 * project's `export:` block is written through the same undoable edit path as
 * any other project change.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { store } from "../stores/project";
import { exportDefaults, exportSettingsFor, projectExportSettings } from "../stores/exportAudio";
import { installAuditBackend } from "./ui-audit-support";
import { getLastCallArgs, invokeMock, makeSnapshot } from "./setup";
import type { ExportSettings, ProjectSnapshot } from "../types/project";
import type { RenderAudioResult } from "../types/export";

// jsdom does not expose Web Storage here; the store only needs a map.
const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => {
        storage.set(key, String(value));
      },
      removeItem: (key: string) => {
        storage.delete(key);
      },
      clear: () => storage.clear(),
    },
  });
}

const RESULT: RenderAudioResult = {
  scope: "mix",
  path: "/tmp/mix.wav",
  files: ["/tmp/mix.wav"],
  stems: 0,
  bytes: 1_024,
  frames: 512,
  sample_rate: 48_000,
  container: "wav",
  depth: "i24",
  dither: true,
  peak_dbfs: -1,
  true_peak_dbfs: -0.9,
  above_full_scale: false,
  clipped: false,
};

function snapshot(exportBlock?: ExportSettings): ProjectSnapshot {
  const base = makeSnapshot();
  base.duration_seconds = 12;
  if (exportBlock) base.project.properties.export = exportBlock;
  return base;
}

beforeEach(() => {
  window.localStorage?.clear();
});

describe("export defaults", () => {
  it("reads the project block and fills the rest with the build defaults", () => {
    const project = snapshot({ container: "wav", wav: { depth: "i24", dither: true } }).project;
    const defaults = exportDefaults(project);
    expect(projectExportSettings(project)).not.toBeNull();
    expect(defaults).toEqual({
      container: "wav",
      depth: "i24",
      dither: true,
      flacLevel: 5,
      mp3Bitrate: 192,
      mp3Vbr: null,
      sampleRate: 44_100,
      resample: null,
    });
  });

  it("falls back to WAV, 16-bit, no dither and 44100 when nothing is declared", () => {
    const project = snapshot().project;
    expect(projectExportSettings(project)).toBeNull();
    expect(exportDefaults(project)).toEqual({
      container: "wav",
      depth: "i16",
      dither: false,
      flacLevel: 5,
      mp3Bitrate: 192,
      mp3Vbr: null,
      sampleRate: 44_100,
      resample: null,
    });
  });

  it("never selects dither on float output, even from a stale project block", () => {
    const project = snapshot({ container: "wav", wav: { depth: "f32", dither: true } }).project;
    expect(exportDefaults(project).dither).toBe(false);
  });

  it("maps a dialog state to the block the project stores", () => {
    expect(
      exportSettingsFor({
        container: "wav",
        depth: "i24",
        dither: true,
        flacLevel: 5,
        mp3Bitrate: 192,
        mp3Vbr: null,
        sampleRate: 48_000,
        resample: 4,
      }),
    ).toEqual({
      container: "wav",
      wav: { depth: "i24", dither: true },
      sample_rate: 48_000,
      resample: 4,
    });
  });

  it("reads an MP3 block and keeps a declared rate the container cannot write", () => {
    const project = snapshot({
      container: "mp3",
      wav: { depth: "i24", dither: true },
      mp3: { bitrate: 256, vbr_quality: 2 },
      sample_rate: 96000,
    }).project;
    const defaults = exportDefaults(project);
    expect(defaults.container).toBe("mp3");
    // MP3 has one sample format and no dither step.
    expect(defaults.depth).toBe("i16");
    expect(defaults.dither).toBe(false);
    expect(defaults.mp3Bitrate).toBe(256);
    expect(defaults.mp3Vbr).toBe(2);
    // 96 kHz has no MPEG form; the dialog keeps it so it can refuse it and
    // name the table, rather than rewriting the project's choice.
    expect(defaults.sampleRate).toBe(96000);
  });

  it("moves a declared bitrate into the MPEG-2 table at a low rate", () => {
    const project = snapshot({
      container: "mp3",
      wav: { depth: "i16", dither: false },
      mp3: { bitrate: 192 },
      sample_rate: 22050,
    }).project;
    expect(exportDefaults(project).sampleRate).toBe(22050);
    expect(exportDefaults(project).mp3Bitrate).toBe(160);
  });

  it("does not let a remembered normalisation off override a declared target", () => {
    window.localStorage.setItem(
      "opencompose.export.last",
      JSON.stringify({ normalise: false }),
    );
    const project = makeSnapshot({
      master: { effects: [], loudness_target: -14, volume_db: 0 },
    }).project;
    expect(exportDefaults(project).normalise).toBe(true);
  });

  it("stores an MP3 container with the bitrate it differs on", () => {
    expect(
      exportSettingsFor({
        container: "mp3",
        depth: "i16",
        dither: false,
        flacLevel: 5,
        mp3Bitrate: 256,
        mp3Vbr: null,
        sampleRate: 48_000,
        resample: null,
      }),
    ).toEqual({
      container: "mp3",
      wav: { depth: "i16", dither: false },
      mp3: { bitrate: 256 },
      sample_rate: 48_000,
      resample: null,
    });

    // A VBR choice is stored as the quality index, and the default CBR bitrate
    // never appears in the block.
    const vbr = exportSettingsFor({
      container: "mp3",
      depth: "i16",
      dither: false,
      flacLevel: 5,
      mp3Bitrate: 192,
      mp3Vbr: 2,
      sampleRate: 44_100,
      resample: null,
    });
    expect(vbr.mp3).toEqual({ bitrate: 192, vbr_quality: 2 });

    // A default MP3 choice is omitted, so WAV and FLAC projects keep their
    // historical spelling.
    const plain = exportSettingsFor({
      container: "mp3",
      depth: "i16",
      dither: false,
      flacLevel: 5,
      mp3Bitrate: 192,
      mp3Vbr: null,
      sampleRate: 44_100,
      resample: null,
    });
    expect(plain.mp3).toBeUndefined();
  });

  it("stores a FLAC container with its own depth and level", () => {
    const project = snapshot({
      container: "flac",
      wav: { depth: "i16", dither: false },
      flac: { depth: "i24", level: 8 },
    }).project;
    const defaults = exportDefaults(project);
    expect(defaults.container).toBe("flac");
    expect(defaults.depth).toBe("i24");
    expect(defaults.flacLevel).toBe(8);
    expect(defaults.dither).toBe(false);
    expect(exportSettingsFor(defaults)).toEqual({
      container: "flac",
      wav: { depth: "i24", dither: false },
      flac: { depth: "i24", level: 8 },
      sample_rate: 44_100,
      resample: null,
    });
  });
});

describe("renderAudio", () => {
  it("passes every option to the backend and returns its summary", async () => {
    installAuditBackend((command) => {
      if (command === "get_project") return snapshot();
      if (command === "render_audio") return RESULT;
      return undefined;
    });
    await store.initProject();
    invokeMock.mockClear();

    const result = await store.renderAudio({
      scope: "mix",
      output_path: "/tmp/mix.wav",
      options: {
        container: "wav",
        depth: "i24",
        dither: true,
        sample_rate: 48_000,
        resample: 3,
        normalise: false,
      },
    });

    const request = getLastCallArgs("render_audio")?.request as Record<string, unknown>;
    expect(request).toEqual({
      scope: "mix",
      output_path: "/tmp/mix.wav",
      options: {
        container: "wav",
        depth: "i24",
        dither: true,
        sample_rate: 48_000,
        resample: 3,
        normalise: false,
      },
    });
    expect(result).toEqual(RESULT);
  });

  it("sends the stem set's folder and name with the same options", async () => {
    installAuditBackend((command) => {
      if (command === "get_project") return snapshot();
      if (command === "render_audio") return { ...RESULT, scope: "stems", stems: 3 };
      return undefined;
    });
    await store.initProject();
    invokeMock.mockClear();

    await store.renderAudio({
      scope: "stems",
      directory: "/tmp/stems",
      stem_name: "take 2",
      options: { container: "wav", depth: "f32", dither: false, sample_rate: 96_000 },
    });

    const request = getLastCallArgs("render_audio")?.request as Record<string, unknown>;
    expect(request.scope).toBe("stems");
    expect(request.directory).toBe("/tmp/stems");
    expect(request.stem_name).toBe("take 2");
    expect((request.options as Record<string, unknown>).depth).toBe("f32");
    expect((request.options as Record<string, unknown>).sample_rate).toBe(96_000);
  });

  it("surfaces a failed render instead of swallowing it", async () => {
    installAuditBackend((command) => {
      if (command === "get_project") return snapshot();
      if (command === "render_audio") throw "Export cancelled at the encoding checkpoint; nothing was published";
      return undefined;
    });
    await store.initProject();

    await expect(
      store.renderAudio({
        scope: "mix",
        output_path: "/tmp/mix.wav",
        options: { container: "wav", depth: "i16", dither: false, sample_rate: 44_100 },
      }),
    ).rejects.toBe("Export cancelled at the encoding checkpoint; nothing was published");
  });
});

describe("setExportSettings", () => {
  it("writes the block into the project and its DSL", async () => {
    installAuditBackend((command, args) => {
      if (command === "get_project") return snapshot();
      if (command === "set_export_settings") {
        const applied = snapshot();
        applied.project.properties.export = args.settings as ExportSettings;
        applied.dsl_source =
          '@project "Test Project"\n  tempo: 128\n  export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000}\n';
        return applied;
      }
      return undefined;
    });
    await store.initProject();

    await store.setExportSettings({
      container: "wav",
      wav: { depth: "i24", dither: true },
      sample_rate: 48_000,
      resample: null,
    });

    expect(store.project.properties.export).toEqual({
      container: "wav",
      wav: { depth: "i24", dither: true },
      sample_rate: 48_000,
      resample: null,
    });
    expect(store.dslSource()).toContain("sample_rate: 48000");
  });
});
