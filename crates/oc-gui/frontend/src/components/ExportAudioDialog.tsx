/**
 * Audio export dialog — options, then progress, then the finished summary.
 *
 * Options come before the file name because the extension follows the format.
 * The dialog owns the export choices; the project's `export:` block is written
 * when Export is pressed, so a saved project reloads with the same choices.
 */
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import {
  exportAudioStore,
  exportDefaults,
  exportSettingsFor,
  saveLastUsedExport,
  type ExportDefaults,
} from "../stores/exportAudio";
import * as api from "../stores/api";
import { createProjectClock } from "../utils/projectClock";
import type { ExportDepth } from "../types/project";
import {
  EXPORT_RESAMPLE_STEPS,
  EXPORT_SAMPLE_RATES,
  MP3_SAMPLE_RATES,
  MP3_VBR_QUALITIES,
  exportContainers,
  mp3BitrateFor,
  mp3BitratesFor,
  type ExportPhase,
  type ExportScope,
  type RenderAudioResult,
  type WritableContainer,
} from "../types/export";
import {
  estimateExportBytes,
  formatBytes,
  formatDuration,
  formatMp3Description,
  formatSampleDescription,
  stemFileNames,
  stemSlug,
} from "../utils/exportEstimate";
import { useTooltip } from "./Tooltip";

export interface ExportAudioDialogProps {
  scope: ExportScope;
  onClose: () => void;
}

type Page = "options" | "rendering" | "finished";

const PROGRESS_EVENT = "export-progress";

/** The stem-set marker file; its presence means a previous set is there. */
const STEM_MANIFEST = "stems.json";

/** The dialog's own failure sentence; a backend cancellation speaks for itself. */
function describeFailure(cause: unknown, container: WritableContainer): string {
  const message = cause instanceof Error ? cause.message : String(cause);
  if (message.includes("cancelled")) return message;
  if (container === "flac" && /flac|encod/i.test(message)) {
    return i18n.t("export.audio.flacFailed", { error: message });
  }
  if (container === "mp3" && /mp3|encod/i.test(message)) {
    return i18n.t("export.audio.mp3Failed", { error: message });
  }
  return i18n.t("export.audio.failed", { error: message });
}

export function ExportAudioDialog(props: ExportAudioDialogProps) {
  const defaults = exportDefaults(store.project);
  const [scope, setScope] = createSignal<ExportScope>(props.scope);
  const [container, setContainer] = createSignal<WritableContainer>(defaults.container);
  const [depth, setDepth] = createSignal<ExportDepth>(defaults.depth);
  const [dither, setDither] = createSignal(defaults.dither);
  const [flacLevel, setFlacLevel] = createSignal<number>(defaults.flacLevel);
  const [mp3Bitrate, setMp3Bitrate] = createSignal<number>(defaults.mp3Bitrate);
  const [mp3Vbr, setMp3Vbr] = createSignal<number | null>(defaults.mp3Vbr);
  // The encoder is a build-time feature: until the backend says it is compiled
  // in, MP3 stays listed but disabled rather than offered and failing later.
  const [mp3Available, setMp3Available] = createSignal(false);
  // The encoder's rate table, replaced by the backend's answer when it arrives.
  const [mp3Rates, setMp3Rates] = createSignal<readonly number[]>(MP3_SAMPLE_RATES);
  const [sampleRate, setSampleRate] = createSignal<number>(defaults.sampleRate);
  const [resample, setResample] = createSignal<number | null>(defaults.resample);
  const loudnessTarget = () => store.project.master?.loudness_target ?? null;
  const [normalise, setNormalise] = createSignal(defaults.normalise ?? loudnessTarget() !== null);
  const [path, setPath] = createSignal<string | null>(null);
  const [folder, setFolder] = createSignal<string | null>(exportAudioStore.stemFolder());
  const [stemName, setStemName] = createSignal(store.project.name ?? "");
  const [page, setPage] = createSignal<Page>("options");
  const [phase, setPhase] = createSignal<ExportPhase | null>(null);
  const [fraction, setFraction] = createSignal(0);
  const [cancelling, setCancelling] = createSignal(false);
  const [cancelled, setCancelled] = createSignal(false);
  const [result, setResult] = createSignal<RenderAudioResult | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [confirmReplace, setConfirmReplace] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  // A close during a render asks the worker to stop, then closes once it has.
  let closeWhenStopped = false;

  const running = () => page() === "rendering";
  const song = () => stemSlug(store.project.name ?? "", "untitled");

  /** Extension the chosen container writes. */
  const extension = () => container();

  /** The one sentence a build without a container's encoder shows. */
  const disabledReason = (id: "wav" | "flac" | "mp3"): string =>
    id === "mp3"
      ? i18n.t("export.audio.mp3Unavailable")
      : i18n.t("export.audio.formatDisabled", { format: id.toUpperCase() });

  const formats = () => exportContainers(mp3Available());

  const disabledFormats = () => formats().filter((option) => !option.writable);

  /** True when the chosen container carries no bit depth at all. */
  const depthless = () => container() === "mp3";

  /** Depth choices that exist for the chosen container. */
  const depthOptions = (): ExportDepth[] =>
    container() === "flac" ? ["i16", "i24"] : ["i16", "i24", "f32"];

  /** Sample rates this container can write; MP3 has its own MPEG table. */
  const rateChoices = (): number[] =>
    depthless()
      ? // The current rate stays listed so an unsupported declared rate shows
        // as itself and can be refused, rather than disappearing.
        Array.from(new Set([...mp3Rates(), sampleRate()])).sort((a, b) => a - b)
      : rateOptions();

  /** CBR bitrates the MPEG version at the chosen rate writes. */
  const bitrateChoices = (): readonly number[] => mp3BitratesFor(sampleRate());

  /** Change the rate and keep the bitrate inside the new MPEG table. */
  const chooseRate = (rate: number) => {
    setSampleRate(rate);
    if (container() === "mp3" && !mp3BitratesFor(rate).includes(mp3Bitrate())) {
      setMp3Bitrate(mp3BitrateFor(rate));
    }
  };

  /** Default target a mix export suggests before the user picks a file. */
  const suggestedName = () => `${song()}.${extension()}`;

  const fileName = () => {
    const target = path();
    if (!target) return suggestedName();
    const parts = target.split(/[\\/]/);
    return parts[parts.length - 1] || suggestedName();
  };

  const stemManifestPath = () => {
    const directory = folder();
    if (!directory) return null;
    const separator = directory.endsWith("/") || directory.endsWith("\\") ? "" : "/";
    return `${directory}${separator}${STEM_MANIFEST}`;
  };

  const destination = () => (scope() === "stems" ? stemManifestPath() : path());

  const seconds = () => store.transport().duration_seconds;

  /** Bars the arrangement covers, read on the project clock. */
  const bars = createMemo(() => {
    const length = seconds();
    if (!Number.isFinite(length) || length <= 0) return null;
    try {
      const clock = createProjectClock(store.project.properties);
      const end = clock.barBeatAt(clock.quarterAtSeconds(length));
      // A length that lands exactly on a bar line covers the bars before it.
      const onBarLine = end.beat === 1 && end.subdivision === 0;
      return Math.max(1, onBarLine ? end.bar - 1 : end.bar);
    } catch {
      return null;
    }
  });

  const rateOptions = createMemo(() =>
    Array.from(new Set([...EXPORT_SAMPLE_RATES, sampleRate()])).sort((a, b) => a - b),
  );

  const perFileBytes = () =>
    estimateExportBytes({
      container: container(),
      seconds: seconds(),
      sampleRate: sampleRate(),
      depth: depth(),
      flacLevel: flacLevel(),
      bitrateKbps: mp3Bitrate(),
    });

  const stemFiles = () =>
    stemFileNames(
      {
        song: stemName() || store.project.name || "",
        tracks: store.project.tracks.map((track) => track.name),
        buses: store.project.buses.map((bus) => bus.name),
      },
      extension(),
    );

  /** FLAC size at this level as a percentage of the same-depth WAV. */
  const flacPercent = () => {
    const flac = estimateExportBytes({
      container: "flac",
      seconds: seconds(),
      sampleRate: sampleRate(),
      depth: depth(),
      flacLevel: flacLevel(),
    });
    const wav = estimateExportBytes({
      container: "wav",
      seconds: seconds(),
      sampleRate: sampleRate(),
      depth: depth(),
    });
    if (flac === null || wav === null || wav === 0) return null;
    return Math.round((flac / wav) * 100);
  };

  const estimatedBytes = () => {
    const one = perFileBytes();
    if (one === null) return null;
    return scope() === "stems" ? one * stemFiles().length : one;
  };

  /** The summary line the user reads before committing. */
  const summary = () => {
    const size = estimatedBytes();
    const length = seconds();
    if (size === null || length <= 0) return null;
    return {
      file: scope() === "stems" ? i18n.t("export.audio.files", { count: stemFiles().length }) : fileName(),
      duration: formatDuration(length),
      bars: bars(),
      sample: depthless()
        ? formatMp3Description(mp3Bitrate(), sampleRate(), mp3Vbr())
        : formatSampleDescription(depth(), sampleRate()),
      size: formatBytes(size),
    };
  };

  // Dither acts on WAV integer output only: FLAC stores it forever and float
  // already carries the mix precision.
  const ditherEnabled = () => container() === "wav" && depth() !== "f32";
  const ditherReason = () => {
    if (container() === "flac") return i18n.t("export.audio.ditherFlac");
    if (container() === "mp3") return i18n.t("export.audio.mp3NoDither");
    return i18n.t("export.audio.ditherDisabled");
  };
  const normaliseEnabled = () => loudnessTarget() !== null;

  /** The switch is meaningful only while the project declares a target. */
  const normaliseHint = () => {
    if (!normaliseEnabled()) return i18n.t("export.audio.normaliseDisabled");
    if (normalise()) return i18n.t("export.audio.normaliseProject");
    return i18n.t("export.audio.normaliseOffForExport", { target: loudnessTarget()! });
  };

  /** Select a container and drop choices that cannot apply to it. */
  const chooseContainer = (next: WritableContainer) => {
    if (next === container()) return;
    if (next === "mp3" && !mp3Available()) return;
    setContainer(next);
    if (next === "flac") {
      if (depth() === "f32") setDepth("i24");
      setDither(false);
    }
    if (next === "mp3") {
      // MP3 has no depth and no dither. An unsupported rate stays selected so
      // the dialog can name it; only the bitrate moves into the MPEG table.
      setDither(false);
      if (!mp3BitratesFor(sampleRate()).includes(mp3Bitrate())) {
        setMp3Bitrate(mp3BitrateFor(sampleRate()));
      }
    }
    setPath(null);
    setConfirmReplace(false);
  };

  const currentDefaults = (): ExportDefaults => ({
    container: container(),
    depth: depth(),
    dither: ditherEnabled() ? dither() : false,
    flacLevel: flacLevel(),
    mp3Bitrate: mp3Bitrate(),
    mp3Vbr: mp3Vbr(),
    sampleRate: sampleRate(),
    resample: resample(),
    normalise: normalise() && normaliseEnabled(),
  });

  const validation = createMemo(() => {
    if (running()) return null;
    // Refuse an unwritable rate here, before the render starts, and name the
    // table the container carries.
    if (depthless() && !mp3Rates().includes(sampleRate())) {
      return i18n.t("export.audio.mp3RateUnsupported", {
        rate: sampleRate(),
        rates: mp3Rates().join(", "),
      });
    }
    if (scope() === "stems" && !folder()) return i18n.t("export.audio.noFolder");
    if (scope() === "mix" && !path()) return i18n.t("export.audio.noPath");
    return null;
  });

  const canExport = () => !busy() && !running() && validation() === null;

  const chooseFile = async () => {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const chosen = await save({
        defaultPath: suggestedName(),
        filters: [
          {
            name:
              container() === "flac"
                ? "FLAC audio"
                : container() === "mp3"
                  ? "MP3 audio"
                  : "WAV audio",
            extensions: [extension()],
          },
        ],
      });
      if (typeof chosen === "string" && chosen) {
        setPath(chosen);
        setConfirmReplace(false);
        setError(null);
      }
    } catch (cause) {
      setError(i18n.t("export.audio.chooseFailed", { error: String(cause) }));
    }
  };

  const chooseFolder = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const chosen = await open({ directory: true, multiple: false });
      if (typeof chosen === "string" && chosen) {
        setFolder(chosen);
        exportAudioStore.setStemFolder(chosen);
        setConfirmReplace(false);
        setError(null);
      }
    } catch (cause) {
      setError(i18n.t("export.audio.chooseFailed", { error: String(cause) }));
    }
  };

  /** Write the choices where they survive a reload: the project and this device. */
  const persist = async () => {
    const chosen = currentDefaults();
    saveLastUsedExport(chosen);
    try {
      await store.setExportSettings(exportSettingsFor(chosen));
    } catch (cause) {
      throw i18n.t("export.audio.settingsFailed", { error: String(cause) });
    }
  };

  const run = async () => {
    setConfirmReplace(false);
    setBusy(true);
    setResult(null);
    setError(null);
    setCancelled(false);
    setPhase("rendering");
    setFraction(0);
    setCancelling(false);
    setPage("rendering");
    try {
      const chosen = currentDefaults();
      const rendered = await store.renderAudio({
        scope: scope(),
        output_path: scope() === "mix" ? (path() ?? undefined) : undefined,
        directory: scope() === "stems" ? (folder() ?? undefined) : undefined,
        stem_name: scope() === "stems" ? stemName() || undefined : undefined,
        options: {
          container: chosen.container,
          depth: chosen.depth,
          dither: chosen.dither,
          flac_level: chosen.container === "flac" ? chosen.flacLevel : undefined,
          mp3_bitrate: chosen.container === "mp3" ? chosen.mp3Bitrate : undefined,
          mp3_vbr: chosen.container === "mp3" ? (chosen.mp3Vbr ?? undefined) : undefined,
          sample_rate: chosen.sampleRate,
          resample: chosen.resample,
          normalise: normalise() && normaliseEnabled(),
        },
      });
      setResult(rendered);
      setPage("finished");
    } catch (cause) {
      const message = describeFailure(cause, container());
      setError(message);
      setCancelled(message.toLowerCase().includes("cancelled"));
      setPage("options");
    } finally {
      setPhase(null);
      setCancelling(false);
      setBusy(false);
      if (closeWhenStopped) {
        closeWhenStopped = false;
        props.onClose();
      }
    }
  };

  const start = async () => {
    if (!canExport()) return;
    const chosenTarget = destination();
    if (!chosenTarget) return;
    setError(null);
    setBusy(true);
    try {
      const exists = await api.exportTargetExists(chosenTarget);
      if (exists) {
        setConfirmReplace(true);
        return;
      }
      await persist();
      await run();
    } catch (cause) {
      setError(describeFailure(cause, container()));
    } finally {
      setBusy(false);
    }
  };

  const replace = async () => {
    if (busy()) return;
    setBusy(true);
    try {
      await persist();
      await run();
    } catch (cause) {
      setError(describeFailure(cause, container()));
      setPage("options");
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (!running() || cancelling()) return;
    setCancelling(true);
    try {
      await api.cancelExport();
    } catch (cause) {
      setError(describeFailure(cause, container()));
    }
  };

  const showFile = async () => {
    const published = result();
    if (!published) return;
    try {
      await api.revealExport(published.path);
    } catch (cause) {
      setError(i18n.t("export.audio.revealFailed", { error: String(cause) }));
    }
  };

  const close = () => {
    if (running()) {
      // Closing during a render is a cancel: stop the worker first, then close
      // when its invoke settles.
      closeWhenStopped = true;
      if (!cancelling()) void cancel();
      return;
    }
    props.onClose();
  };

  const phaseLabel = () => {
    switch (phase()) {
      case "encoding":
        if (container() === "flac") return i18n.t("export.audio.phaseEncodingFlac");
        if (container() === "mp3") return i18n.t("export.audio.phaseEncodingMp3");
        return i18n.t("export.audio.phaseEncoding");
      case "publishing":
        return i18n.t("export.audio.phasePublishing");
      case "done":
        return i18n.t("export.audio.phaseDone");
      case "cancelled":
        return i18n.t("export.audio.phaseCancelled");
      default:
        return i18n.t("export.audio.phaseRendering");
    }
  };

  onMount(() => {
    void api
      .exportCapabilities()
      .then((capabilities) => {
        setMp3Available(capabilities?.mp3 === true);
        const rates = capabilities?.mp3_sample_rates;
        if (Array.isArray(rates) && rates.length > 0) setMp3Rates(rates);
      })
      .catch(() => setMp3Available(false));
  });

  let stopListening: (() => void) | null = null;
  onMount(() => {
    void listen(PROGRESS_EVENT, (event) => {
      const payload = event.payload as {
        phase?: ExportPhase;
        fraction?: number;
      } | null;
      if (payload?.phase) {
        setPhase(payload.phase);
        setFraction(payload.phase === "cancelled" ? 0 : (payload.fraction ?? 0));
        if (payload.phase === "cancelled") setCancelled(true);
      }
    }).then((unlisten) => {
      stopListening = unlisten;
    });
    onCleanup(() => {
      stopListening?.();
      stopListening = null;
    });
  });

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div class="midi-overlay" onClick={close}>
      <div
        class="midi-dialog export-audio-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("export.audio.title")}
        onClick={(event) => event.stopPropagation()}
      >
        <header class="midi-dialog-header">
          <div>
            <div class="midi-dialog-title">{i18n.t("export.audio.title")}</div>
            <div class="midi-dialog-subtitle">{i18n.t("export.audio.subtitle")}</div>
          </div>
          <button
            class="midi-icon-btn export-audio-close"
            onClick={close}
            {...useTooltip(() => i18n.t("export.audio.close"))}
          >
            ×
          </button>
        </header>

        <div class="midi-dialog-body export-audio-body">
          <Show when={page() === "options"}>
            <section class="export-audio-section">
              <div class="midi-field-label">{i18n.t("export.audio.scope")}</div>
              <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("export.audio.scope")}>
                <For each={["mix", "stems"] as ExportScope[]}>
                  {(kind) => (
                    <button
                      class="midi-segment export-audio-scope"
                      classList={{ active: scope() === kind }}
                      data-scope={kind}
                      role="radio"
                      aria-checked={scope() === kind}
                      onClick={() => {
                        setScope(kind);
                        setConfirmReplace(false);
                      }}
                    >
                      {kind === "mix"
                        ? i18n.t("export.audio.scopeMix")
                        : i18n.t("export.audio.scopeStems")}
                    </button>
                  )}
                </For>
              </div>
            </section>

            <section class="export-audio-section">
              <div class="midi-panel-head">{i18n.t("export.audio.file")}</div>
              <Show
                when={scope() === "mix"}
                fallback={
                  <>
                    <div class="midi-field-row">
                      <span class="export-audio-path selectable">
                        {folder() ?? i18n.t("export.audio.noFolder")}
                      </span>
                      <button class="midi-btn export-audio-choose" onClick={chooseFolder}>
                        {i18n.t("export.audio.choose")}
                      </button>
                    </div>
                    <label class="midi-inline-field export-audio-stem-name">
                      <span>{i18n.t("export.audio.stemName")}</span>
                      <input
                        value={stemName()}
                        onInput={(event) => setStemName(event.currentTarget.value)}
                        placeholder={song()}
                      />
                    </label>
                    <div class="export-audio-stem-preview">
                      <For each={stemFiles().slice(0, 3)}>
                        {(name) => <span class="midi-chip">{name}</span>}
                      </For>
                      <Show when={stemFiles().length > 3}>
                        <span class="midi-chip">
                          {i18n.t("export.audio.stemPreviewMore", { count: stemFiles().length - 3 })}
                        </span>
                      </Show>
                    </div>
                  </>
                }
              >
                <div class="midi-field-row">
                  <span class="export-audio-path selectable">
                    {path() ?? i18n.t("export.audio.noPath")}
                  </span>
                  <button class="midi-btn export-audio-choose" onClick={chooseFile}>
                    {i18n.t("export.audio.choose")}
                  </button>
                </div>
              </Show>
            </section>

            <section class="export-audio-section">
              <div class="midi-panel-head">{i18n.t("export.audio.format")}</div>
              <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("export.audio.format")}>
                <For each={formats()}>
                  {(option) => (
                    <button
                      class="midi-segment export-audio-format"
                      classList={{ active: option.id === container() }}
                      data-format={option.id}
                      role="radio"
                      aria-checked={option.id === container()}
                      disabled={!option.writable}
                      onClick={() => {
                        if (option.writable) chooseContainer(option.id as WritableContainer);
                      }}
                      {...useTooltip(() =>
                        option.writable ? option.label : disabledReason(option.id),
                      )}
                    >
                      {option.label}
                    </button>
                  )}
                </For>
              </div>
              <p class="midi-hint export-audio-format-hint">{i18n.t("export.audio.formatHint")}</p>
              <For each={disabledFormats()}>
                {(option) => (
                  <p class="export-audio-reason" data-format={option.id}>
                    {disabledReason(option.id)}
                  </p>
                )}
              </For>
              <Show when={container() === "flac"}>
                <p class="export-audio-reason export-audio-flac-note">
                  {i18n.t("export.audio.flacNote")}
                </p>
              </Show>

              <Show
                when={!depthless()}
                fallback={
                  <p class="export-audio-reason export-audio-mp3-note">
                    {i18n.t("export.audio.mp3Note")}
                  </p>
                }
              >
              <div class="midi-options-row">
                <div class="midi-field">
                  <span class="midi-field-label">{i18n.t("export.audio.depth")}</span>
                  <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("export.audio.depth")}>
                    <For each={depthOptions()}>
                      {(value) => (
                        <button
                          class="midi-segment export-audio-depth"
                          classList={{ active: depth() === value }}
                          data-depth={value}
                          role="radio"
                          aria-checked={depth() === value}
                          onClick={() => {
                            setDepth(value);
                            if (value === "f32") setDither(false);
                          }}
                        >
                          {value === "i16" ? "16-bit" : value === "i24" ? "24-bit" : "32-bit float"}
                        </button>
                      )}
                    </For>
                  </div>
                </div>

                <div class="midi-field">
                  <label class="export-audio-dither">
                    <input
                      type="checkbox"
                      aria-label={i18n.t("export.audio.dither")}
                      checked={dither() && ditherEnabled()}
                      disabled={!ditherEnabled()}
                      onChange={(event) => setDither(event.currentTarget.checked)}
                    />
                    <span>{i18n.t("export.audio.dither")}</span>
                  </label>
                  <Show when={!ditherEnabled()}>
                    <p class="export-audio-reason export-audio-dither-reason">
                      {ditherReason()}
                    </p>
                  </Show>
                </div>
              </div>
              </Show>

              <Show when={container() === "mp3"}>
                <div class="midi-options-row">
                  <label class="midi-inline-field export-audio-mp3-bitrate">
                    <span>{i18n.t("export.audio.mp3Bitrate")}</span>
                    <select
                      value={mp3Bitrate()}
                      disabled={mp3Vbr() !== null}
                      onChange={(event) => setMp3Bitrate(Number(event.currentTarget.value))}
                    >
                      <For each={bitrateChoices()}>
                        {(kbps) => <option value={kbps}>{kbps} kbps</option>}
                      </For>
                    </select>
                  </label>

                  <label class="midi-inline-field export-audio-mp3-vbr">
                    <span>{i18n.t("export.audio.mp3Vbr")}</span>
                    <select
                      value={mp3Vbr() === null ? "off" : String(mp3Vbr())}
                      onChange={(event) => {
                        const value = event.currentTarget.value;
                        setMp3Vbr(value === "off" ? null : Number(value));
                      }}
                    >
                      <option value="off">{i18n.t("export.audio.mp3VbrOff")}</option>
                      <For each={MP3_VBR_QUALITIES}>
                        {(quality) => (
                          <option value={quality}>
                            {i18n.t("export.audio.mp3VbrValue", { quality })}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                </div>
                <Show when={mp3Vbr() !== null}>
                  <p class="export-audio-reason export-audio-mp3-vbr-reason">
                    {i18n.t("export.audio.mp3VbrNote")}
                  </p>
                </Show>
                <p class="midi-hint export-audio-mp3-estimate">
                  {i18n.t("export.audio.mp3Estimate", {
                    kbps: mp3Bitrate(),
                    size: formatBytes(perFileBytes() ?? 0),
                  })}
                </p>
              </Show>

              <Show when={container() === "flac"}>
                <label class="midi-inline-field export-audio-flac-level">
                  <span>{i18n.t("export.audio.flacLevel")}</span>
                  <input
                    type="range"
                    min="0"
                    max="8"
                    step="1"
                    value={flacLevel()}
                    aria-label={i18n.t("export.audio.flacLevel")}
                    onInput={(event) => setFlacLevel(Number(event.currentTarget.value))}
                  />
                  <span class="export-audio-flac-level-value">
                    {i18n.t("export.audio.flacLevelValue", { level: flacLevel() })}
                  </span>
                </label>
                <p class="midi-hint export-audio-flac-estimate">
                  {i18n.t("export.audio.flacEstimate", { percent: flacPercent() ?? "—" })}
                </p>
              </Show>
            </section>

            <section class="export-audio-section">
              <div class="midi-options-row">
                <label class="midi-inline-field export-audio-rate">
                  <span>{i18n.t("export.audio.sampleRate")}</span>
                  <select
                    value={sampleRate()}
                    onChange={(event) => chooseRate(Number(event.currentTarget.value))}
                  >
                    <For each={rateChoices()}>
                      {(rate) => <option value={rate}>{rate} Hz</option>}
                    </For>
                  </select>
                </label>

                <label class="midi-inline-field export-audio-resample">
                  <span>{i18n.t("export.audio.resample")}</span>
                  <select
                    value={resample() === null ? "default" : String(resample())}
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setResample(value === "default" ? null : Number(value));
                    }}
                  >
                    <option value="default">{i18n.t("export.audio.resampleDefault")}</option>
                    <For each={EXPORT_RESAMPLE_STEPS}>
                      {(step) => (
                        <option value={step}>{i18n.t("export.audio.resampleStep", { step })}</option>
                      )}
                    </For>
                  </select>
                </label>
              </div>

              <div class="export-audio-normalise">
                <label class="export-audio-dither">
                  <input
                    type="checkbox"
                    aria-label={i18n.t("export.audio.normaliseLabel")}
                    checked={normalise() && normaliseEnabled()}
                    disabled={!normaliseEnabled()}
                    onChange={(event) => setNormalise(event.currentTarget.checked)}
                  />
                  <span>
                    {loudnessTarget() !== null
                      ? i18n.t("export.audio.normalise", { target: loudnessTarget()! })
                      : i18n.t("export.audio.normaliseLabel")}
                  </span>
                </label>
                <Show when={!normaliseEnabled()}>
                  <p class="export-audio-reason export-audio-normalise-reason">
                    {i18n.t("export.audio.normaliseDisabled")}
                  </p>
                </Show>
                <Show when={normaliseEnabled()}>
                  <p class="midi-hint export-audio-normalise-source">{normaliseHint()}</p>
                </Show>
              </div>
            </section>

            <section class="export-audio-summary">
              <div class="midi-panel-head">{i18n.t("export.audio.summary")}</div>
              <Show
                when={summary()}
                fallback={
                  <div class="midi-muted export-audio-summary-empty">
                    {i18n.t("export.audio.summaryUnknown")}
                  </div>
                }
              >
                {(value) => (
                  <div class="export-audio-summary-line selectable">
                    {i18n.t("export.audio.summaryLine", {
                      file: value().file,
                      duration: value().duration,
                      bars: value().bars ?? "—",
                      sample: value().sample,
                      size: value().size,
                    })}
                  </div>
                )}
              </Show>
              <Show when={scope() === "stems"}>
                <div class="export-audio-summary-line export-audio-file-count">
                  {i18n.t("export.audio.files", { count: stemFiles().length })}
                </div>
              </Show>
            </section>

            <Show when={confirmReplace()}>
              <section class="export-audio-overwrite" role="alertdialog">
                <div class="export-audio-overwrite-title">
                  {i18n.t("export.audio.overwriteTitle", {
                    file: scope() === "stems" ? (folder() ?? "") : fileName(),
                  })}
                </div>
                <div class="midi-muted">{i18n.t("export.audio.overwriteBody")}</div>
                <div class="export-audio-overwrite-actions">
                  <button class="midi-btn export-audio-keep" onClick={() => setConfirmReplace(false)}>
                    {i18n.t("export.audio.cancel")}
                  </button>
                  <button class="midi-btn midi-btn-primary export-audio-replace" onClick={replace}>
                    {i18n.t("export.audio.replace")}
                  </button>
                </div>
              </section>
            </Show>
          </Show>

          <Show when={page() === "rendering"}>
            <section class="export-audio-progress" aria-live="polite">
              <div class="export-audio-phase">{phaseLabel()}</div>
              <div class="export-audio-track">
                <div
                  class="export-audio-track-fill"
                  classList={{ "export-audio-track-known": fraction() > 0 }}
                  style={fraction() > 0 ? { width: `${Math.round(fraction() * 100)}%` } : undefined}
                />
              </div>
              <p class="midi-muted export-audio-cancel-note">
                {cancelling()
                  ? i18n.t("export.audio.cancelling")
                  : i18n.t("export.audio.cancelHint")}
              </p>
            </section>
          </Show>

          <Show when={page() === "finished"}>
            <section class="export-audio-finished">
              <div class="midi-panel-head">{i18n.t("export.audio.finished")}</div>
              <Show when={result()}>
                {(value) => (
                  <>
                    <div class="export-audio-finished-row">
                      <span>{i18n.t("export.audio.finishedFile")}</span>
                      <strong class="selectable export-audio-finished-path">{value().path}</strong>
                    </div>
                    <div class="export-audio-finished-row">
                      <span>{i18n.t("export.audio.finishedSize")}</span>
                      <strong>{formatBytes(value().bytes)}</strong>
                    </div>
                    <div class="export-audio-finished-row">
                      <span>{i18n.t("export.audio.finishedPeak")}</span>
                      <strong>{value().peak_dbfs.toFixed(2)} dBFS</strong>
                    </div>
                    <div class="export-audio-finished-row">
                      <span>{i18n.t("export.audio.finishedSample")}</span>
                      <strong>{formatSampleDescription(value().depth, value().sample_rate)}</strong>
                    </div>
                    <Show when={value().stems > 0}>
                      <div class="export-audio-finished-row">
                        <span>{i18n.t("export.audio.finishedStems")}</span>
                        <strong>{i18n.t("export.audio.files", { count: value().files.length })}</strong>
                      </div>
                    </Show>
                    <p
                      class="export-audio-clipped"
                      classList={{
                        "midi-warn-text": value().clipped || value().above_full_scale,
                      }}
                      data-clipped={value().clipped ? "true" : "false"}
                      data-above-full-scale={value().above_full_scale ? "true" : "false"}
                    >
                      {value().clipped
                        ? i18n.t("export.audio.finishedClipped")
                        : value().above_full_scale
                          ? i18n.t("export.audio.finishedAboveFullScale")
                          : i18n.t("export.audio.finishedClean")}
                    </p>
                  </>
                )}
              </Show>
            </section>
          </Show>
        </div>

        <footer class="midi-dialog-footer">
          <div class="midi-footer-status">
            <Show when={error()}>
              <span
                class="midi-error-text selectable export-audio-error"
                classList={{ "export-audio-cancelled": cancelled() }}
                data-cancelled={cancelled() ? "true" : "false"}
              >
                {error()}
              </span>
            </Show>
            <Show when={!error() && page() === "options" && validation()}>
              <span class="midi-warn-text export-audio-validation">{validation()}</span>
            </Show>
          </div>
          <div class="midi-footer-actions">
            <Show when={page() === "options"}>
              <button class="midi-btn" onClick={close}>
                {i18n.t("export.audio.cancel")}
              </button>
              <button
                class="midi-btn midi-btn-primary export-audio-confirm"
                onClick={start}
                disabled={!canExport()}
                {...useTooltip(() => validation() ?? i18n.t("export.audio.export"))}
              >
                {busy() ? i18n.t("export.audio.exporting") : i18n.t("export.audio.export")}
              </button>
            </Show>
            <Show when={page() === "rendering"}>
              <button
                class="midi-btn export-audio-cancel"
                onClick={cancel}
                disabled={cancelling()}
                {...useTooltip(() => i18n.t("export.audio.cancelHint"))}
              >
                {cancelling() ? i18n.t("export.audio.cancelling") : i18n.t("export.audio.cancel")}
              </button>
            </Show>
            <Show when={page() === "finished"}>
              <button class="midi-btn" onClick={close}>
                {i18n.t("export.audio.close")}
              </button>
              <button class="midi-btn midi-btn-primary export-audio-show-file" onClick={showFile}>
                {i18n.t("export.audio.showFile")}
              </button>
            </Show>
          </div>
        </footer>
      </div>
    </div>
  );
}
