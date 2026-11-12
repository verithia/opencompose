/**
 * The audio export dialog: options before the file name, the project block it
 * writes, the overwrite prompt, the checkpoint cancel and the finished summary.
 */
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Mock } from "vitest";
import { listen } from "@tauri-apps/api/event";
import { ExportAudioDialog } from "../components/ExportAudioDialog";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { installAuditBackend, settle } from "./ui-audit-support";
import {
  getLastCallArgs,
  invokeMock,
  makeSnapshot,
  makeTrack,
  openDialogMock,
  saveDialogMock,
} from "./setup";
import type { ExportSettings, Project, ProjectSnapshot, Track } from "../types/project";
import type { ExportPhase, RenderAudioResult } from "../types/export";
import { MP3_SAMPLE_RATES } from "../types/export";

// jsdom does not expose Web Storage in this environment; the dialog only needs a map.
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

type Handler = (args: Record<string, unknown>) => unknown;

const MIX_PATH = "/tmp/mix.wav";

function result(overrides: Partial<RenderAudioResult> = {}): RenderAudioResult {
  return {
    scope: "mix",
    path: MIX_PATH,
    files: [MIX_PATH],
    stems: 0,
    bytes: 3_100_000,
    frames: 441_000,
    sample_rate: 44_100,
    container: "wav",
    depth: "i16",
    dither: false,
    peak_dbfs: -0.5,
    true_peak_dbfs: -0.4,
    above_full_scale: false,
    clipped: false,
    ...overrides,
  };
}

interface SnapshotOptions {
  duration?: number;
  exportBlock?: ExportSettings;
  tracks?: Track[];
  buses?: Project["buses"];
  master?: Project["master"];
}

function snapshot(options: SnapshotOptions = {}): ProjectSnapshot {
  const base = makeSnapshot({
    tracks: options.tracks ?? [],
    buses: options.buses ?? [],
    master: options.master ?? null,
  });
  base.duration_seconds = options.duration ?? 222;
  if (options.exportBlock) base.project.properties.export = options.exportBlock;
  return base;
}

/** Answer the dialog's own commands; anything else falls through to the audit backend. */
function backend(handlers: Record<string, Handler>, project: ProjectSnapshot) {
  invokeMock.mockImplementation(async (command: string, raw?: unknown) => {
    const args = (raw ?? {}) as Record<string, unknown>;
    const handler = handlers[command];
    if (handler) return handler(args);
    if (command === "export_capabilities") return { mp3: true };
    if (command === "export_target_exists") return false;
    if (command === "cancel_export" || command === "reveal_export") return undefined;
    if (command === "set_export_settings") {
      // Keep the rest of the project, including a declared loudness target.
      const applied = snapshot({
        duration: project.duration_seconds,
        master: project.project.master,
        tracks: project.project.tracks,
        buses: project.project.buses,
      });
      applied.project.properties.export = args.settings as ExportSettings;
      return applied;
    }
    if (command === "get_undo_state") {
      return {
        can_undo: false,
        can_redo: false,
        undo_label: null,
        redo_label: null,
        undo_labels: [],
        redo_labels: [],
      };
    }
    if (command === "get_undo_history") return [];
    if (command === "get_transport_state" || command === "transport_stop") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: project.duration_seconds };
    }
    return project;
  });
}

async function load(project: ProjectSnapshot) {
  installAuditBackend((command) => (command === "get_project" ? project : undefined));
  i18n.setLocale("en");
  await store.initProject();
  invokeMock.mockClear();
  saveDialogMock.mockClear();
  openDialogMock.mockClear();
  backend({}, project);
}

async function mountDialog(scope: "mix" | "stems" = "mix") {
  const view = render(() => <ExportAudioDialog scope={scope} onClose={() => {}} />);
  await settle();
  return view;
}

function depthButton(container: HTMLElement, depth: string) {
  return container.querySelector<HTMLButtonElement>(`.export-audio-depth[data-depth="${depth}"]`)!;
}

function formatButton(container: HTMLElement, format: string) {
  return container.querySelector<HTMLButtonElement>(`.export-audio-format[data-format="${format}"]`)!;
}

function rateSelect(container: HTMLElement) {
  return container.querySelector<HTMLSelectElement>(".export-audio-rate select")!;
}

function ditherCheckbox(container: HTMLElement) {
  return container.querySelector<HTMLInputElement>(".export-audio-dither input")!;
}

function summaryLine(container: HTMLElement) {
  return container.querySelector(".export-audio-summary-line")?.textContent ?? "";
}

beforeEach(() => {
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("audio export dialog", () => {
  it("renders every control this build can act on, and hides the ones it cannot", async () => {
    await load(snapshot());
    const view = await mountDialog();
    const container = view.container;

    expect(container.querySelectorAll(".export-audio-scope")).toHaveLength(2);
    expect(container.querySelectorAll(".export-audio-format")).toHaveLength(3);
    expect(container.querySelectorAll(".export-audio-depth")).toHaveLength(3);
    expect(ditherCheckbox(container)).toBeTruthy();
    expect(rateSelect(container)).toBeTruthy();
    expect(container.querySelector(".export-audio-resample select")).toBeTruthy();
    expect(container.querySelector(".export-audio-normalise input")).toBeTruthy();
    expect(container.querySelector(".export-audio-summary")).toBeTruthy();
    expect(container.querySelector(".export-audio-confirm")).toBeTruthy();

    // Bitrate and FLAC compression do not exist for WAV, so they are absent
    // rather than greyed out.
    const text = container.textContent ?? "";
    expect(text).not.toMatch(/Bitrate/i);
    expect(text).not.toMatch(/Compression/i);
  });

  it("offers every format this build encodes and names none as missing", async () => {
    await load(snapshot());
    const view = await mountDialog();
    const formats = [...view.container.querySelectorAll<HTMLButtonElement>(".export-audio-format")];
    expect(formats.map((button) => button.dataset.format)).toEqual(["wav", "flac", "mp3"]);
    expect(formats[0].disabled).toBe(false);
    expect(formats[1].disabled).toBe(false);
    expect(formats[2].disabled).toBe(false);
    expect(formats[0].getAttribute("aria-checked")).toBe("true");
    expect(
      view.container.querySelector('.export-audio-reason[data-format="flac"]'),
    ).toBeNull();
    expect(
      view.container.querySelector('.export-audio-reason[data-format="mp3"]'),
    ).toBeNull();
    expect(view.container.querySelector(".export-audio-format-hint")?.textContent).toBe(
      i18n.t("export.audio.formatHint"),
    );
  });

  it("lists MP3 disabled with the stable reason when the encoder is not compiled in", async () => {
    const project = snapshot();
    await load(project);
    // A build without the `mp3` feature answers the capability query false.
    backend({ export_capabilities: () => ({ mp3: false }) }, project);
    const view = await mountDialog();

    const mp3 = formatButton(view.container, "mp3");
    expect(mp3.disabled).toBe(true);
    expect(mp3.getAttribute("aria-checked")).toBe("false");
    expect(
      view.container.querySelector('.export-audio-reason[data-format="mp3"]')?.textContent,
    ).toBe("This build has no MP3 encoder");
    expect(i18n.t("export.audio.mp3Unavailable")).toBe("This build has no MP3 encoder");

    // Clicking it does nothing: the format is never offered and then failed.
    fireEvent.click(mp3);
    await settle();
    expect(formatButton(view.container, "wav").getAttribute("aria-checked")).toBe("true");
    expect(view.container.querySelector(".export-audio-mp3-bitrate")).toBeNull();
  });

  it("shows the FLAC depth, compression and dither rules and hides bitrate", async () => {
    await load(snapshot());
    const view = await mountDialog();

    fireEvent.click(formatButton(view.container, "flac"));
    await settle();

    expect(formatButton(view.container, "flac").getAttribute("aria-checked")).toBe("true");
    // FLAC stores integers only, so 32-bit float disappears.
    const depths = [...view.container.querySelectorAll<HTMLButtonElement>(".export-audio-depth")];
    expect(depths.map((button) => button.dataset.depth)).toEqual(["i16", "i24"]);
    // The depth a WAV project chose carries over; FLAC only drops f32.
    expect(depthButton(view.container, "i16").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(depthButton(view.container, "i24"));
    await settle();

    const level = view.container.querySelector<HTMLInputElement>(
      ".export-audio-flac-level input",
    )!;
    expect(level.value).toBe("5");
    expect(view.container.textContent).toContain(
      i18n.t("export.audio.flacLevelValue", { level: 5 }),
    );
    expect(view.container.querySelector(".export-audio-flac-note")?.textContent).toBe(
      i18n.t("export.audio.flacNote"),
    );

    // Dither is a WAV option: FLAC stores what it is given.
    expect(ditherCheckbox(view.container).disabled).toBe(true);
    expect(ditherCheckbox(view.container).checked).toBe(false);
    expect(view.container.querySelector(".export-audio-dither-reason")?.textContent).toBe(
      i18n.t("export.audio.ditherFlac"),
    );

    const text = view.container.textContent ?? "";
    expect(text).not.toMatch(/Bitrate/i);
    expect(summaryLine(view.container)).toContain("test-project.flac");
    expect(summaryLine(view.container)).toContain("24-bit / 44.1 kHz");

    // The compression level moves the estimate but not the audio description.
    const fastEstimate = summaryLine(view.container);
    fireEvent.input(level, { target: { value: "0" } });
    await settle();
    expect(summaryLine(view.container)).not.toBe(fastEstimate);
    expect(summaryLine(view.container)).toContain("24-bit / 44.1 kHz");
  });

  it("renders FLAC with the chosen level and reports a FLAC failure", async () => {
    await load(snapshot());
    const FLAC_PATH = "/tmp/mix.flac";
    saveDialogMock.mockResolvedValue(FLAC_PATH);
    const view = await mountDialog();

    fireEvent.click(formatButton(view.container, "flac"));
    await settle();
    fireEvent.click(depthButton(view.container, "i24"));
    await settle();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();
    fireEvent.input(
      view.container.querySelector<HTMLInputElement>(".export-audio-flac-level input")!,
      { target: { value: "2" } },
    );
    await settle();

    let reject: (error: unknown) => void = () => {};
    backend(
      {
        render_audio: () =>
          new Promise((_resolve, rejectRender) => {
            reject = rejectRender;
          }),
      },
      snapshot(),
    );
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    const request = getLastCallArgs("render_audio")?.request as {
      options: { container: string; depth: string; dither: boolean; flac_level?: number };
    };
    expect(request.options.container).toBe("flac");
    expect(request.options.depth).toBe("i24");
    expect(request.options.dither).toBe(false);
    expect(request.options.flac_level).toBe(2);
    expect(getLastCallArgs("set_export_settings")?.settings).toEqual({
      container: "flac",
      wav: { depth: "i24", dither: false },
      flac: { depth: "i24", level: 2 },
      sample_rate: 44100,
      resample: null,
    });

    const handler = (listen as Mock).mock.calls.at(-1)?.[1] as
      | ((event: { payload: { phase: ExportPhase } }) => void)
      | undefined;
    handler!({ payload: { phase: "encoding" } });
    await settle();
    expect(view.container.querySelector(".export-audio-phase")?.textContent).toBe(
      i18n.t("export.audio.phaseEncodingFlac"),
    );

    reject(new Error("FLAC encoding failed: block size"));
    await settle();
    expect(view.container.querySelector(".export-audio-error")?.textContent).toBe(
      i18n.t("export.audio.flacFailed", { error: "FLAC encoding failed: block size" }),
    );
  });

  it("takes an MP3 bitrate, hides the depth and dither, and estimates the CBR size", async () => {
    await load(snapshot());
    const view = await mountDialog();

    fireEvent.click(formatButton(view.container, "mp3"));
    await settle();

    expect(formatButton(view.container, "mp3").getAttribute("aria-checked")).toBe("true");
    // Layer III takes 16-bit PCM, so depth and dither are absent, not greyed.
    expect(view.container.querySelectorAll(".export-audio-depth")).toHaveLength(0);
    expect(
      view.container.querySelector(`input[aria-label="${i18n.t("export.audio.dither")}"]`),
    ).toBeNull();
    expect(view.container.querySelector(".export-audio-mp3-note")?.textContent).toBe(
      i18n.t("export.audio.mp3Note"),
    );

    const bitrate = view.container.querySelector<HTMLSelectElement>(
      ".export-audio-mp3-bitrate select",
    )!;
    expect(bitrate.value).toBe("192");
    expect(bitrate.disabled).toBe(false);
    expect(bitrate.options).toHaveLength(14);

    const estimate = () =>
      view.container.querySelector(".export-audio-mp3-estimate")?.textContent ?? "";
    expect(summaryLine(view.container)).toContain("test-project.mp3");
    expect(summaryLine(view.container)).toContain("192 kbps CBR / 44.1 kHz");
    expect(estimate()).toContain("CBR at 192 kbps");

    const fastSize = estimate();
    fireEvent.change(bitrate, { target: { value: "320" } });
    await settle();
    expect(bitrate.value).toBe("320");
    expect(estimate()).not.toBe(fastSize);
    expect(estimate()).toContain("CBR at 320 kbps");
    expect(summaryLine(view.container)).toContain("320 kbps CBR / 44.1 kHz");

    // The dialog offers every rate the encoder writes, MPEG-2 and 2.5 included.
    expect([...rateSelect(view.container).options].map((option) => option.value)).toEqual([
      "11025",
      "12000",
      "16000",
      "22050",
      "24000",
      "44100",
      "48000",
    ]);
    fireEvent.change(rateSelect(view.container), { target: { value: "22050" } });
    await settle();
    // 320 kbps is not in the MPEG-2 table, so the bitrate moves with the rate.
    expect(bitrate.value).toBe("160");
    expect(bitrate.options).toHaveLength(14);
    expect(summaryLine(view.container)).toContain("160 kbps CBR / 22.05 kHz");

    fireEvent.change(rateSelect(view.container), { target: { value: "48000" } });
    await settle();
    expect(summaryLine(view.container)).toContain("160 kbps CBR / 48 kHz");
  });

  it("switches MP3 to VBR and says the CBR bitrate no longer applies", async () => {
    await load(snapshot());
    const view = await mountDialog();
    fireEvent.click(formatButton(view.container, "mp3"));
    await settle();

    const vbr = view.container.querySelector<HTMLSelectElement>(
      ".export-audio-mp3-vbr select",
    )!;
    expect(vbr.value).toBe("off");
    expect(vbr.options).toHaveLength(11);
    expect(view.container.querySelector(".export-audio-mp3-vbr-reason")).toBeNull();

    fireEvent.change(vbr, { target: { value: "2" } });
    await settle();

    const bitrate = view.container.querySelector<HTMLSelectElement>(
      ".export-audio-mp3-bitrate select",
    )!;
    expect(bitrate.disabled).toBe(true);
    expect(view.container.querySelector(".export-audio-mp3-vbr-reason")?.textContent).toBe(
      i18n.t("export.audio.mp3VbrNote"),
    );
    expect(summaryLine(view.container)).toContain("VBR q2 / 44.1 kHz");
    // The size stays the CBR estimate and says so.
    expect(
      view.container.querySelector(".export-audio-mp3-estimate")?.textContent,
    ).toContain("CBR at 192 kbps");
  });

  it("renders MP3 with the chosen bitrate and stores the choice", async () => {
    const project = snapshot();
    await load(project);
    const MP3_PATH = "/tmp/mix.mp3";
    saveDialogMock.mockResolvedValue(MP3_PATH);
    const view = await mountDialog();

    fireEvent.click(formatButton(view.container, "mp3"));
    await settle();
    fireEvent.change(
      view.container.querySelector<HTMLSelectElement>(".export-audio-mp3-bitrate select")!,
      { target: { value: "256" } },
    );
    await settle();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    backend(
      {
        render_audio: () => result({ container: "mp3", path: MP3_PATH, depth: "i16" }),
      },
      project,
    );
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    const request = getLastCallArgs("render_audio")?.request as {
      options: {
        container: string;
        depth: string;
        dither: boolean;
        mp3_bitrate?: number;
        mp3_vbr?: number;
        flac_level?: number;
      };
    };
    expect(request.options.container).toBe("mp3");
    expect(request.options.mp3_bitrate).toBe(256);
    expect(request.options.mp3_vbr).toBeUndefined();
    expect(request.options.flac_level).toBeUndefined();
    expect(request.options.dither).toBe(false);
    expect(getLastCallArgs("set_export_settings")?.settings).toEqual({
      container: "mp3",
      wav: { depth: "i16", dither: false },
      mp3: { bitrate: 256 },
      sample_rate: 44100,
      resample: null,
    });
  });

  it("disables dither at 32-bit float and says why", async () => {
    await load(snapshot());
    const view = await mountDialog();

    expect(ditherCheckbox(view.container).disabled).toBe(false);
    fireEvent.click(ditherCheckbox(view.container));
    expect(ditherCheckbox(view.container).checked).toBe(true);

    fireEvent.click(depthButton(view.container, "f32"));
    await settle();
    expect(ditherCheckbox(view.container).disabled).toBe(true);
    expect(ditherCheckbox(view.container).checked).toBe(false);
    expect(view.container.querySelector(".export-audio-dither-reason")?.textContent).toBe(
      i18n.t("export.audio.ditherDisabled"),
    );

    fireEvent.click(depthButton(view.container, "i16"));
    await settle();
    expect(ditherCheckbox(view.container).disabled).toBe(false);
    expect(view.container.querySelector(".export-audio-dither-reason")).toBeNull();
  });

  it("takes its defaults from the project's export block", async () => {
    await load(
      snapshot({
        exportBlock: {
          container: "wav",
          wav: { depth: "i24", dither: true },
          sample_rate: 48000,
          resample: 3,
        },
      }),
    );
    const view = await mountDialog();

    expect(depthButton(view.container, "i24").getAttribute("aria-checked")).toBe("true");
    expect(ditherCheckbox(view.container).checked).toBe(true);
    expect(rateSelect(view.container).value).toBe("48000");
    expect(view.container.querySelector<HTMLSelectElement>(".export-audio-resample select")!.value).toBe("3");
  });

  it("uses the last-used choices only when the project declares none", async () => {
    window.localStorage.setItem(
      "opencompose.export.last",
      JSON.stringify({ depth: "f32", dither: false, sampleRate: 88200, resample: 5 }),
    );
    await load(snapshot());
    const bare = await mountDialog();
    expect(depthButton(bare.container, "f32").getAttribute("aria-checked")).toBe("true");
    expect(rateSelect(bare.container).value).toBe("88200");
    bare.unmount();

    // A project that declares a depth outranks the stored choice.
    await load(
      snapshot({ exportBlock: { container: "wav", wav: { depth: "i16", dither: false } } }),
    );
    const declared = await mountDialog();
    expect(depthButton(declared.container, "i16").getAttribute("aria-checked")).toBe("true");
    expect(depthButton(declared.container, "f32").getAttribute("aria-checked")).toBe("false");
  });

  it("writes the chosen values into the project block, and reopens with them", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();

    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    fireEvent.click(depthButton(view.container, "i24"));
    fireEvent.click(ditherCheckbox(view.container));
    fireEvent.change(rateSelect(view.container), { target: { value: "48000" } });
    await settle();

    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    const saved = getLastCallArgs("set_export_settings");
    expect(saved?.settings).toEqual({
      container: "wav",
      wav: { depth: "i24", dither: true },
      sample_rate: 48000,
      resample: null,
    });
    expect(store.project.properties.export).toEqual(saved?.settings);
    // The same choices are remembered for the next dialog on this device.
    expect(JSON.parse(window.localStorage.getItem("opencompose.export.last")!)).toEqual({
      container: "wav",
      depth: "i24",
      dither: true,
      flacLevel: 5,
      mp3Bitrate: 192,
      mp3Vbr: null,
      sampleRate: 48000,
      resample: null,
      normalise: false,
    });

    view.unmount();
    const reopened = await mountDialog();
    expect(depthButton(reopened.container, "i24").getAttribute("aria-checked")).toBe("true");
    expect(ditherCheckbox(reopened.container).checked).toBe(true);
    expect(rateSelect(reopened.container).value).toBe("48000");
  });

  it("asks before replacing a file, and only when the file exists", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    backend({ render_audio: () => result() }, snapshot());
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();
    expect(view.container.querySelector(".export-audio-overwrite")).toBeNull();
    expect(invokeMock.mock.calls.filter(([command]) => command === "render_audio")).toHaveLength(1);
    view.unmount();

    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const again = await mountDialog();
    fireEvent.click(again.container.querySelector(".export-audio-choose")!);
    await settle();

    const calls: string[] = [];
    backend(
      {
        export_target_exists: () => true,
        render_audio: () => {
          calls.push("render");
          return result();
        },
      },
      snapshot(),
    );
    fireEvent.click(again.container.querySelector(".export-audio-confirm")!);
    await settle();

    expect(again.container.querySelector(".export-audio-overwrite")).toBeTruthy();
    expect(again.container.querySelector(".export-audio-overwrite-title")?.textContent).toBe(
      i18n.t("export.audio.overwriteTitle", { file: "mix.wav" }),
    );
    expect(calls).toEqual([]);
    expect(getLastCallArgs("set_export_settings")).toBeUndefined();

    fireEvent.click(again.container.querySelector(".export-audio-replace")!);
    await settle();
    expect(calls).toEqual(["render"]);
  });

  it("keeps the pre-render estimate in step with the depth and the rate", async () => {
    await load(snapshot({ duration: 222 }));
    const view = await mountDialog();
    const first = summaryLine(view.container);
    expect(first).toContain("3:42");
    expect(first).toContain("16-bit / 44.1 kHz");
    expect(first).toContain("≈");

    fireEvent.click(depthButton(view.container, "i24"));
    await settle();
    const deeper = summaryLine(view.container);
    expect(deeper).toContain("24-bit / 44.1 kHz");
    expect(deeper).not.toBe(first);

    fireEvent.change(rateSelect(view.container), { target: { value: "96000" } });
    await settle();
    const faster = summaryLine(view.container);
    expect(faster).toContain("24-bit / 96 kHz");
    expect(faster).not.toBe(deeper);
  });

  it("reports the arrangement length in bars as well as H:M:S", async () => {
    // 128 BPM, 4/4: one bar is 1.875 s, so 7.5 s covers exactly four bars.
    await load(snapshot({ duration: 7.5 }));
    const view = await mountDialog();
    expect(summaryLine(view.container)).toContain("4 bars");
    expect(summaryLine(view.container)).toContain("0:08");
  });

  it("names the render checkpoints and cancels at the next one", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    let reject = (_error: unknown) => {};
    backend(
      {
        render_audio: () =>
          new Promise((_resolve, rejectRender) => {
            reject = rejectRender;
          }),
      },
      snapshot(),
    );

    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();
    expect(view.container.querySelector(".export-audio-phase")?.textContent).toBe(
      i18n.t("export.audio.phaseRendering"),
    );
    expect(view.container.querySelector(".export-audio-cancel-note")?.textContent).toBe(
      i18n.t("export.audio.cancelHint"),
    );

    const handler = (listen as Mock).mock.calls.at(-1)?.[1] as
      | ((event: { payload: { phase: ExportPhase } }) => void)
      | undefined;
    expect(handler).toBeTruthy();
    handler!({ payload: { phase: "encoding" } });
    await settle();
    expect(view.container.querySelector(".export-audio-phase")?.textContent).toBe(
      i18n.t("export.audio.phaseEncoding"),
    );

    const cancel = view.container.querySelector<HTMLButtonElement>(".export-audio-cancel")!;
    fireEvent.click(cancel);
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "cancel_export")).toHaveLength(1);
    expect(cancel.textContent).toBe(i18n.t("export.audio.cancelling"));
    expect(cancel.disabled).toBe(true);
    expect(view.container.querySelector(".export-audio-cancel-note")?.textContent).toBe(
      i18n.t("export.audio.cancelling"),
    );

    reject(new Error("Export cancelled at the encoding checkpoint; nothing was published"));
    await settle();
    expect(view.container.querySelector(".export-audio-error")?.textContent).toBe(
      "Export cancelled at the encoding checkpoint; nothing was published",
    );
    // Back on the options page: nothing was published, so there is no summary.
    expect(view.container.querySelector(".export-audio-confirm")).toBeTruthy();
    expect(view.container.querySelector(".export-audio-finished")).toBeNull();
  });

  it("closes only after a running render has stopped", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const onClose = vi.fn();
    const view = render(() => <ExportAudioDialog scope="mix" onClose={onClose} />);
    await settle();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    let reject = (_error: unknown) => {};
    backend(
      {
        render_audio: () =>
          new Promise((_resolve, rejectRender) => {
            reject = rejectRender;
          }),
      },
      snapshot(),
    );
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.click(view.container.querySelector(".export-audio-close")!);
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "cancel_export")).toHaveLength(1);
    // The window stays open until the worker answers.
    expect(onClose).not.toHaveBeenCalled();

    reject(new Error("Export cancelled at the rendering checkpoint; nothing was published"));
    await settle();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(
      view.container.querySelector<HTMLElement>(".export-audio-error")?.dataset.cancelled,
    ).toBe("true");
  });

  it("reports the finished file, its size, its peak and the clipped flag", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    backend({ render_audio: () => result({ clipped: true, peak_dbfs: 0.25 }) }, snapshot());
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    expect(view.container.querySelector(".export-audio-finished-path")?.textContent).toBe(MIX_PATH);
    const text = view.container.textContent ?? "";
    expect(text).toContain("3.1 MB");
    expect(text).toContain("0.25 dBFS");
    const clipped = view.container.querySelector(".export-audio-clipped")!;
    expect(clipped.getAttribute("data-clipped")).toBe("true");
    expect(clipped.textContent).toBe(i18n.t("export.audio.finishedClipped"));

    fireEvent.click(view.container.querySelector(".export-audio-show-file")!);
    await settle();
    const revealed = getLastCallArgs("reveal_export");
    expect(revealed?.path).toBe(MIX_PATH);
  });

  it("reports a clean mix as unclipped", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();
    backend({ render_audio: () => result() }, snapshot());
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    const clipped = view.container.querySelector(".export-audio-clipped")!;
    expect(clipped.getAttribute("data-clipped")).toBe("false");
    expect(clipped.textContent).toBe(i18n.t("export.audio.finishedClean"));
  });

  it("refuses to start without a target and names the missing choice", async () => {
    await load(snapshot());
    const view = await mountDialog();
    const confirm = view.container.querySelector<HTMLButtonElement>(".export-audio-confirm")!;
    expect(confirm.disabled).toBe(true);
    expect(view.container.querySelector(".export-audio-validation")?.textContent).toBe(
      i18n.t("export.audio.noPath"),
    );
  });

  it("opens with the stems scope preset and previews the set's file names", async () => {
    await load(
      snapshot({
        tracks: [makeTrack("Lead Guitar", 0), makeTrack("Drums", 1)],
        buses: [{ name: "Reverb Bus", routes: [], effects: [] }],
      }),
    );
    const view = await mountDialog("stems");

    expect(
      view.container.querySelector('.export-audio-scope[data-scope="stems"]')?.getAttribute("aria-checked"),
    ).toBe("true");
    expect(view.container.querySelector(".export-audio-path")?.textContent).toBe(
      i18n.t("export.audio.noFolder"),
    );
    const name = view.container.querySelector<HTMLInputElement>(".export-audio-stem-name input")!;
    expect(name.value).toBe("Test Project");
    const preview = [...view.container.querySelectorAll(".export-audio-stem-preview .midi-chip")].map(
      (chip) => chip.textContent,
    );
    expect(preview).toEqual([
      "test-project-mix.wav",
      "test-project-lead-guitar.wav",
      "test-project-drums.wav",
      "+1 more",
    ]);
    expect(view.container.querySelector<HTMLButtonElement>(".export-audio-confirm")!.disabled).toBe(true);
    expect(view.container.querySelector(".export-audio-validation")?.textContent).toBe(
      i18n.t("export.audio.noFolder"),
    );
  });

  it("reports a float export above full scale as peaks kept, not clipping", async () => {
    await load(snapshot());
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();
    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();

    backend(
      {
        render_audio: () =>
          result({
            depth: "f32",
            peak_dbfs: 5.39,
            above_full_scale: true,
            clipped: false,
          }),
      },
      snapshot(),
    );
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();

    const status = view.container.querySelector(".export-audio-clipped")!;
    expect(status.getAttribute("data-clipped")).toBe("false");
    expect(status.getAttribute("data-above-full-scale")).toBe("true");
    expect(status.textContent).toBe(i18n.t("export.audio.finishedAboveFullScale"));
  });

  it("starts a declared loudness target enabled even when this device remembered off", async () => {
    window.localStorage.setItem("opencompose.export.last", JSON.stringify({ normalise: false }));
    const project = snapshot({
      master: { effects: [], loudness_target: -14, volume_db: 0 },
    });
    await load(project);
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();

    const checkbox = view.container.querySelector<HTMLInputElement>(
      ".export-audio-normalise input",
    )!;
    expect(checkbox.checked).toBe(true);
    expect(view.container.textContent).toContain("-14 LUFS");
    expect(view.container.querySelector(".export-audio-normalise-source")?.textContent).toBe(
      i18n.t("export.audio.normaliseProject"),
    );

    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();
    backend({ render_audio: () => result() }, project);
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();
    const request = getLastCallArgs("render_audio")?.request as {
      options: { normalise?: boolean };
    };
    expect(request.options.normalise).toBe(true);
  });

  it("lets the per-export switch turn a declared target off without touching the project", async () => {
    window.localStorage.setItem("opencompose.export.last", JSON.stringify({ normalise: false }));
    const project = snapshot({
      master: { effects: [], loudness_target: -14, volume_db: 0 },
    });
    await load(project);
    saveDialogMock.mockResolvedValue(MIX_PATH);
    const view = await mountDialog();

    fireEvent.click(view.container.querySelector(".export-audio-normalise input")!);
    await settle();
    expect(view.container.querySelector(".export-audio-normalise-source")?.textContent).toBe(
      i18n.t("export.audio.normaliseOffForExport", { target: -14 }),
    );

    fireEvent.click(view.container.querySelector(".export-audio-choose")!);
    await settle();
    backend({ render_audio: () => result() }, project);
    fireEvent.click(view.container.querySelector(".export-audio-confirm")!);
    await settle();
    const request = getLastCallArgs("render_audio")?.request as {
      options: { normalise?: boolean };
    };
    expect(request.options.normalise).toBe(false);
  });

  it("refuses an MP3 rate the container cannot carry, before rendering", async () => {
    const project = snapshot({
      exportBlock: {
        container: "mp3",
        wav: { depth: "i16", dither: false },
        sample_rate: 96000,
      },
    });
    await load(project);
    const view = await mountDialog();

    expect(view.container.querySelector<HTMLButtonElement>(".export-audio-confirm")!.disabled).toBe(
      true,
    );
    expect(view.container.querySelector(".export-audio-validation")?.textContent).toBe(
      i18n.t("export.audio.mp3RateUnsupported", {
        rate: 96000,
        rates: MP3_SAMPLE_RATES.join(", "),
      }),
    );
    expect(invokeMock.mock.calls.filter(([command]) => command === "render_audio")).toHaveLength(0);
  });
});
