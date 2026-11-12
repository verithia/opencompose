/**
 * MIDI export dialog — scope, format, PPQ, channel map and the loss list.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import {
  reportMidiExport,
  writeMidiExport,
  type MidiChannelAssignment,
  type MidiExportOptions,
  type MidiExportReport,
  type MidiExportScope,
} from "../stores/midi";

export interface MidiExportDialogProps {
  trackIndex?: number | null;
  onClose: () => void;
}

type ScopeKind = "project" | "range" | "track";

const CHANNEL_DRUMS = 10;

function isDrumTrack(track: { instrument: { category: string } }) {
  return track.instrument.category.toLowerCase() === "drums";
}

const LOSS_I18N: Record<string, string> = {
  articulation: "midi.loss.articulation",
  keyswitch: "midi.loss.keyswitch",
  microtuning: "midi.loss.microtuning",
  plugin_parameter: "midi.loss.plugin_parameter",
  automation: "midi.loss.automation",
  audio_clip: "midi.loss.audio_clip",
  clip_envelope: "midi.loss.clip_envelope",
  clip_window: "midi.loss.clip_window",
  send_bus: "midi.loss.send_bus",
  mute_solo: "midi.loss.mute_solo",
  tempo_ramp: "midi.loss.tempo_ramp",
  program_change: "midi.loss.program_change",
};

function lossLabel(kind: string) {
  const key = LOSS_I18N[kind];
  return key ? i18n.t(key as Parameters<typeof i18n.t>[0]) : kind;
}

const MELODIC_CHANNELS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16];

// Round-robin over the 15 melodic channels; drums keep channel 10.
function defaultChannels(indices: number[]): Record<number, number> {
  const result: Record<number, number> = {};
  let next = 0;
  for (const index of indices) {
    const track = store.project.tracks[index];
    if (!track) continue;
    if (isDrumTrack(track)) {
      result[index] = CHANNEL_DRUMS;
    } else {
      result[index] = MELODIC_CHANNELS[next % MELODIC_CHANNELS.length];
      next += 1;
    }
  }
  return result;
}

export function MidiExportDialog(props: MidiExportDialogProps) {
  const tracks = () => store.project.tracks;
  const initialTrack = () =>
    props.trackIndex !== null && props.trackIndex !== undefined && props.trackIndex >= 0 && props.trackIndex < tracks().length
      ? props.trackIndex
      : null;

  const [scopeKind, setScopeKind] = createSignal<ScopeKind>(initialTrack() === null ? "project" : "track");
  const [rangeFrom, setRangeFrom] = createSignal(0);
  const [rangeTo, setRangeTo] = createSignal(Math.max(0, tracks().length - 1));
  const [singleTrack, setSingleTrack] = createSignal(initialTrack() ?? 0);
  const [format, setFormat] = createSignal<0 | 1>(1);
  const [ppq, setPpq] = createSignal<480 | 960>(480);
  const [channels, setChannels] = createSignal<Record<number, number>>({});
  const [report, setReport] = createSignal<MidiExportReport | null>(null);
  const [reportLoading, setReportLoading] = createSignal(false);
  const [reportError, setReportError] = createSignal<string | null>(null);
  const [writing, setWriting] = createSignal(false);
  const [writtenPath, setWrittenPath] = createSignal<string | null>(null);
  const [writeError, setWriteError] = createSignal<string | null>(null);

  let reportSeq = 0;
  let reportTimer: ReturnType<typeof setTimeout> | null = null;

  const includedIndices = createMemo(() => {
    const total = tracks().length;
    switch (scopeKind()) {
      case "project":
        return Array.from({ length: total }, (_, index) => index);
      case "range": {
        const from = Math.max(0, Math.min(rangeFrom(), rangeTo()));
        const to = Math.min(total - 1, Math.max(rangeFrom(), rangeTo()));
        return Array.from({ length: Math.max(0, to - from + 1) }, (_, index) => from + index);
      }
      case "track":
        return total === 0 ? [] : [Math.max(0, Math.min(singleTrack(), total - 1))];
    }
  });

  // Keep one channel assignment per included track; drums default to 10.
  createEffect(() => {
    const indices = includedIndices();
    const current = channels();
    const next: Record<number, number> = {};
    const defaults = defaultChannels(indices);
    for (const index of indices) next[index] = current[index] ?? defaults[index] ?? 1;
    if (Object.keys(next).length !== Object.keys(current).length || indices.some((index) => next[index] !== current[index])) {
      setChannels(next);
    }
  });

  const scope = (): MidiExportScope => {
    if (scopeKind() === "range") {
      return { Tracks: { start: Math.min(rangeFrom(), rangeTo()), end: Math.max(rangeFrom(), rangeTo()) + 1 } };
    }
    if (scopeKind() === "track") return { Track: singleTrack() };
    return "Project";
  };

  const channelAssignments = (): MidiChannelAssignment[] =>
    includedIndices().map((index) => ({ track: index, channel: channels()[index] ?? 1 }));

  const options = (): MidiExportOptions => ({
    scope: scope(),
    format: format(),
    ppq: ppq(),
    channels: channelAssignments(),
  });

  const tracksKey = createMemo(() =>
    JSON.stringify({ scope: scope(), format: format(), ppq: ppq(), channels: channelAssignments() }),
  );

  createEffect(() => {
    tracksKey();
    if (reportTimer) clearTimeout(reportTimer);
    if (includedIndices().length === 0) {
      setReport(null);
      setReportLoading(false);
      setReportError(null);
      return;
    }
    setReport(null);
    setReportError(null);
    setWrittenPath(null);
    setWriteError(null);
    setReportLoading(true);
    const seq = ++reportSeq;
    reportTimer = setTimeout(async () => {
      try {
        const next = await reportMidiExport(options());
        if (seq === reportSeq) setReport(next);
      } catch (error) {
        if (seq === reportSeq) setReportError(String(error));
      } finally {
        if (seq === reportSeq) setReportLoading(false);
      }
    }, 120);
  });

  onCleanup(() => {
    if (reportTimer) clearTimeout(reportTimer);
  });

  const scopeLabel = (kind: ScopeKind) => {
    if (kind === "project") return i18n.t("midi.export.scopeProject");
    if (kind === "range") return i18n.t("midi.export.scopeRange");
    return i18n.t("midi.export.scopeTrack");
  };

  const canExport = createMemo(() =>
    !writing() && !reportLoading() && reportError() === null && report() !== null && includedIndices().length > 0,
  );

  const save = async () => {
    if (!canExport()) return;
    let path: string | null = null;
    try {
      const { save: saveDialog } = await import("@tauri-apps/plugin-dialog");
      const base = (store.project.name || "Untitled").replace(/[\\/:*?"<>|]+/g, "-");
      path = await saveDialog({
        defaultPath: `${base}.mid`,
        filters: [{ name: "MIDI", extensions: ["mid", "midi"] }],
      });
    } catch (error) {
      setWriteError(String(error));
      return;
    }
    if (typeof path !== "string" || !path) return;
    setWriting(true);
    setWriteError(null);
    try {
      const written = await writeMidiExport(options(), path);
      setReport(written);
      setWrittenPath(path);
    } catch (error) {
      setWriteError(String(error));
    } finally {
      setWriting(false);
    }
  };

  const close = () => {
    if (!writing()) props.onClose();
  };

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !writing()) props.onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div class="midi-overlay" onClick={close}>
      <div
        class="midi-dialog midi-export-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("midi.export.title")}
        onClick={(event) => event.stopPropagation()}
      >
        <header class="midi-dialog-header">
          <div>
            <div class="midi-dialog-title">{i18n.t("midi.export.title")}</div>
            <div class="midi-dialog-subtitle">{i18n.t("midi.export.subtitle", { count: includedIndices().length })}</div>
          </div>
          <button class="midi-icon-btn" onClick={close} disabled={writing()} {...useTooltip(() => i18n.t("midi.close"))}>×</button>
        </header>

        <div class="midi-dialog-body midi-export-body">
          <section class="midi-export-options">
            <div class="midi-field">
              <span class="midi-field-label">{i18n.t("midi.export.scope")}</span>
              <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("midi.export.scope")}>
                <For each={["project", "range", "track"] as ScopeKind[]}>
                  {(kind) => (
                    <button
                      class="midi-segment"
                      classList={{ active: scopeKind() === kind }}
                      role="radio"
                      aria-checked={scopeKind() === kind}
                      disabled={tracks().length === 0}
                      onClick={() => setScopeKind(kind)}
                    >
                      {scopeLabel(kind)}
                    </button>
                  )}
                </For>
              </div>
              <Show when={scopeKind() === "range"}>
                <div class="midi-field-row">
                  <label class="midi-inline-field">
                    <span>{i18n.t("midi.export.rangeFrom")}</span>
                    <select value={rangeFrom()} onChange={(event) => setRangeFrom(Number(event.currentTarget.value))}>
                      <For each={tracks()}>{(track, index) => <option value={index()}>{index() + 1}. {track.name}</option>}</For>
                    </select>
                  </label>
                  <label class="midi-inline-field">
                    <span>{i18n.t("midi.export.rangeTo")}</span>
                    <select value={rangeTo()} onChange={(event) => setRangeTo(Number(event.currentTarget.value))}>
                      <For each={tracks()}>{(track, index) => <option value={index()}>{index() + 1}. {track.name}</option>}</For>
                    </select>
                  </label>
                </div>
              </Show>
              <Show when={scopeKind() === "track"}>
                <label class="midi-inline-field">
                  <span>{i18n.t("midi.export.singleTrack")}</span>
                  <select value={singleTrack()} onChange={(event) => setSingleTrack(Number(event.currentTarget.value))}>
                    <For each={tracks()}>{(track, index) => <option value={index()}>{index() + 1}. {track.name}</option>}</For>
                  </select>
                </label>
              </Show>
            </div>

            <div class="midi-options-row">
              <div class="midi-field">
                <span class="midi-field-label">{i18n.t("midi.export.format")}</span>
                <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("midi.export.format")}>
                  <button class="midi-segment" classList={{ active: format() === 1 }} role="radio" aria-checked={format() === 1} onClick={() => setFormat(1)}>{i18n.t("midi.export.format1")}</button>
                  <button class="midi-segment" classList={{ active: format() === 0 }} role="radio" aria-checked={format() === 0} onClick={() => setFormat(0)}>{i18n.t("midi.export.format0")}</button>
                </div>
                <span class="midi-hint">{format() === 1 ? i18n.t("midi.export.format1Hint") : i18n.t("midi.export.format0Hint")}</span>
              </div>
              <div class="midi-field">
                <span class="midi-field-label">{i18n.t("midi.export.ppq")}</span>
                <div class="midi-segmented" role="radiogroup" aria-label={i18n.t("midi.export.ppq")}>
                  <For each={[480, 960] as const}>
                    {(value) => (
                      <button class="midi-segment" classList={{ active: ppq() === value }} role="radio" aria-checked={ppq() === value} onClick={() => setPpq(value)}>{value}</button>
                    )}
                  </For>
                </div>
              </div>
            </div>
          </section>

          <section class="midi-channel-panel">
            <div class="midi-panel-head">{i18n.t("midi.export.channelMap")}</div>
            <div class="midi-channel-list">
              <For each={includedIndices()}>
                {(index) => {
                  const track = () => tracks()[index];
                  return (
                    <div class="midi-channel-row">
                      <span class="midi-channel-name" {...useTooltip(() => track()?.name ?? `#${index + 1}`)}>{track()?.name ?? `#${index + 1}`}</span>
                      <Show when={track() && isDrumTrack(track()!)}>
                        <span class="midi-chip midi-chip-drum">{i18n.t("midi.import.drums")}</span>
                      </Show>
                      <label class="midi-inline-field midi-channel-select">
                        <span>{i18n.t("midi.import.channel")}</span>
                        <select
                          aria-label={`${i18n.t("midi.import.channel")} ${track()?.name ?? index + 1}`}
                          value={channels()[index] ?? 1}
                          onChange={(event) => setChannels((current) => ({ ...current, [index]: Number(event.currentTarget.value) }))}
                        >
                          <For each={Array.from({ length: 16 }, (_, i) => i + 1)}>
                            {(channel) => <option value={channel}>{channel === CHANNEL_DRUMS ? `${channel} · ${i18n.t("midi.import.drums")}` : channel}</option>}
                          </For>
                        </select>
                      </label>
                    </div>
                  );
                }}
              </For>
              <Show when={includedIndices().length === 0}>
                <div class="midi-muted midi-pad">{i18n.t("midi.export.noTracks")}</div>
              </Show>
            </div>
          </section>

          <section class="midi-loss-panel">
            <div class="midi-panel-head">
              <span>{i18n.t("midi.export.lossTitle")}</span>
              <Show when={report()}>
                {(value) => (
                  <span classList={{ "midi-warn-text": value().losses.length > 0 }} class="midi-loss-count">
                    {value().losses.length === 0
                      ? i18n.t("midi.export.lossNone")
                      : i18n.t("midi.export.lossKinds", { count: value().losses.length })}
                  </span>
                )}
              </Show>
            </div>
            <div class="midi-loss-body selectable">
              <Show when={reportLoading()}>
                <div class="midi-muted midi-pad">{i18n.t("midi.export.reportLoading")}</div>
              </Show>
              <Show when={reportError()}>
                <div class="midi-error-text midi-pad selectable">{i18n.t("midi.export.reportFailed", { error: reportError()! })}</div>
              </Show>
              <Show when={report()}>
                {(value) => (
                  <>
                    <Show
                      when={value().losses.length > 0}
                      fallback={<div class="midi-muted midi-pad">{i18n.t("midi.export.lossEmpty")}</div>}
                    >
                      <ul class="midi-loss-list">
                        <For each={value().losses}>
                          {(loss) => (
                            <li class="midi-loss-item" data-kind={loss.kind}>
                              <span class="midi-loss-kind">{lossLabel(loss.kind)}</span>
                              <span class="midi-loss-count-badge">{loss.count}</span>
                              <span class="midi-loss-detail" {...useTooltip(() => loss.detail)}>{loss.detail}</span>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                    <div class="midi-loss-tracks">
                      <For each={value().written}>
                        {(track) => (
                          <span class="midi-chip" {...useTooltip(() => `SMF ${track.smf_track} · ${track.automation_events} automation`)}>
                            {track.name} · {i18n.t("midi.import.channel")} {track.channel} · {track.notes} {i18n.t("midi.import.notes")}
                          </span>
                        )}
                      </For>
                    </div>
                  </>
                )}
              </Show>
            </div>
          </section>
        </div>

        <footer class="midi-dialog-footer">
          <div class="midi-footer-status">
            <Show when={writtenPath()}>
              <span class="midi-success-text">{i18n.t("midi.export.saved", { path: writtenPath()!.split(/[\\/]/).pop() ?? "" })}</span>
            </Show>
            <Show when={writeError()}>
              <span class="midi-error-text selectable">{i18n.t("midi.export.writeFailed", { error: writeError()! })}</span>
            </Show>
            <Show when={!writtenPath() && !writeError() && report() && report()!.losses.length > 0}>
              <span class="midi-warn-text">{i18n.t("midi.export.lossWarning")}</span>
            </Show>
          </div>
          <div class="midi-footer-actions">
            <button class="midi-btn" onClick={close} disabled={writing()}>{i18n.t("midi.cancel")}</button>
            <button class="midi-btn midi-btn-primary midi-export-confirm" onClick={save} disabled={!canExport()}>
              {writing() ? i18n.t("midi.export.writing") : writtenPath() ? i18n.t("midi.export.rewrite") : i18n.t("midi.export.confirm")}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
