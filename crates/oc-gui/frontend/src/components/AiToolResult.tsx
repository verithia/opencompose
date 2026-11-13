/**
 * Rich views for tool results. Every view keeps the raw payload behind one
 * toggle, so a shape this file does not know still reads as JSON.
 */
import { For, Show, createMemo, createResource, createSignal, type JSX } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { toolResultLabelKeys } from "../stores/locales/toolResultCopy";
import { aiApi } from "../stores/api";
import { store } from "../stores/project";
import { previewAudioFile, type PreviewAudio } from "../stores/api";
import { pcmPreviewWaveform } from "../utils/pcmPreviewWaveform";
import { AiToolContent, CodeBlock } from "./AiToolContent";
import { PreviewAudioPlayer } from "./PreviewAudioPlayer";

type RecordValue = Record<string, unknown>;
type Scalar = string | number | boolean | null;

type Column = { key: string; label?: string; numeric?: boolean };

const MAX_ROWS = 80;

/** A listening delivery opens itself; a render result stays folded like the rest. */
export function autoOpenResult(tool: string): boolean {
  return tool === "audition_range";
}

function record(value: unknown): RecordValue | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as RecordValue
    : null;
}

function rowsOf(value: unknown): RecordValue[] | null {
  if (!Array.isArray(value)) return null;
  const rows = value.filter((row): row is RecordValue => record(row) !== null);
  return rows.length === value.length && rows.length > 0 ? rows : null;
}

function string(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function number(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function scalar(value: unknown): value is Scalar {
  return value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean";
}

function formatNumber(value: number): string {
  if (Number.isInteger(value)) return String(value);
  if (Math.abs(value) >= 100) return value.toFixed(1);
  return value.toFixed(2).replace(/\.?0+$/, "");
}

function cell(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "number") return Number.isFinite(value) ? formatNumber(value) : "—";
  if (typeof value === "boolean") return yesNo(value);
  if (typeof value === "string") {
    const one = value.replace(/\s+/g, " ").trim();
    return one.length > 120 ? `${one.slice(0, 117)}…` : one;
  }
  if (Array.isArray(value)) {
    if (value.every((item) => Array.isArray(item) && item.length === 2)) {
      return value.map(([key, item]) => `${cell(key)}=${cell(item)}`).join(", ").slice(0, 160);
    }
    return value.map(cell).join(", ").slice(0, 120);
  }
  const param = record(value);
  if (param && Object.keys(param).length === 1 && "Number" in param) return cell(param.Number);
  try {
    return JSON.stringify(value).slice(0, 120);
  } catch {
    return "—";
  }
}

const fieldLabels: Readonly<Record<string, string>> = {
  track_count: "tracks",
  volume_db: "vol dB",
  file_path: "file",
  start_s: "start s",
  end_s: "end s",
  duration_s: "duration s",
  duration_seconds: "duration",
  tempo_bpm: "tempo",
  time_signature: "meter",
  peak_dbfs: "peak dB",
  rms_dbfs: "RMS dB",
  integrated_lufs: "integrated",
  true_peak_dbfs: "true peak",
  duration_beats: "beats",
  dsl_source: "DSL",
};

function label(key: string): string {
  const readable = key.replace(/_/g, " ");
  const field = Object.hasOwn(fieldLabels, key) ? fieldLabels[key] : readable;
  if (Object.hasOwn(toolResultLabelKeys, field)) {
    return i18n.t(toolResultLabelKeys[field as keyof typeof toolResultLabelKeys]);
  }
  return readable.replace(/\b(s|ms|db|hz|bpm)\b/gi, (unit) => unit.toLowerCase());
}

const statusKeys: Readonly<Record<string, I18nKey>> = {
  ok: "ai.result.succeeded", success: "ai.result.succeeded", completed: "ai.result.succeeded",
  failed: "ai.activity.failed", error: "ai.activity.failed", refused: "ai.result.refused",
  ready: "ai.result.ready", loading: "ai.result.loading", installed: "ai.result.installed",
  opened: "ai.result.opened", not_found: "ai.result.notFound", unsupported: "ai.result.unsupported",
  needs_confirmation: "ai.result.confirmation", playing: "ai.result.playing", paused: "ai.result.paused",
  stopped: "ai.result.stopped", idle: "ai.result.idle", prepared: "ai.result.prepared",
  warming: "ai.result.warming", cancelled: "ai.result.cancelled", canceled: "ai.result.cancelled",
  high: "ai.result.high", medium: "ai.result.medium", low: "ai.result.low",
  valid: "ai.result.valid", invalid: "ai.result.invalid",
};

function statusLabel(value: string): string {
  const key = value.toLowerCase();
  return Object.hasOwn(statusKeys, key) ? i18n.t(statusKeys[key]) : value;
}

function displayCell(key: string, value: unknown): string {
  return (key === "status" || key === "confidence") && typeof value === "string"
    ? statusLabel(value) : cell(value);
}

function yesNo(value: boolean): string {
  return i18n.t(value ? "ai.result.yes" : "ai.result.no");
}

function isCodeKey(key: string): boolean {
  return /^(dsl|dsl_source|new_dsl|old_dsl|parsed_dsl|context|instructions|content|text|markdown|skill_content|state_hex|base64)$/i.test(key);
}

/** Columns that read well in a table: scalar fields first, nested ones last. */
function autoColumns(rows: RecordValue[]): Column[] {
  const seen = new Map<string, { count: number; numeric: number }>();
  for (const row of rows.slice(0, 40)) {
    for (const [key, value] of Object.entries(row)) {
      if (isCodeKey(key) || key === "sha256") continue;
      const entry = seen.get(key) ?? { count: 0, numeric: 0 };
      entry.count += 1;
      if (typeof value === "number") entry.numeric += 1;
      seen.set(key, entry);
    }
  }
  return [...seen.entries()]
    .sort((left, right) => right[1].count - left[1].count)
    .slice(0, 9)
    .map(([key, stats]) => ({
      key,
      label: label(key),
      numeric: stats.numeric > 0 && stats.numeric * 2 >= stats.count,
    }));
}

function DataTable(props: { rows: RecordValue[]; columns?: Column[]; empty?: string; max?: number; onRowClick?: (row: RecordValue) => void }) {
  const columns = createMemo(() => props.columns ?? autoColumns(props.rows));
  const shown = createMemo(() => props.rows.slice(0, props.max ?? MAX_ROWS));
  return (
    <Show when={props.rows.length > 0} fallback={<div class="ai-tool-empty">{props.empty ?? i18n.t("ai.tool.empty")}</div>}>
      <div class="ai-tool-table-wrap">
        <table class="ai-tool-table">
          <thead>
            <tr><For each={columns()}>{column => <th classList={{ numeric: !!column.numeric }}>{column.label ?? label(column.key)}</th>}</For></tr>
          </thead>
          <tbody>
            <For each={shown()}>{row => (
              <tr classList={{ clickable: !!props.onRowClick }} onClick={() => props.onRowClick?.(row)}>
                <For each={columns()}>{column => {
                  const value = row[column.key];
                  return <td classList={{ numeric: !!column.numeric }} title={displayCell(column.key, value)}>{displayCell(column.key, value)}</td>;
                }}</For>
              </tr>
            )}</For>
          </tbody>
        </table>
        <Show when={props.rows.length > shown().length}>
          <div class="ai-tool-table-note">
            {i18n.t("ai.tool.moreRows", { count: shown().length })}
            <span> · {i18n.t("ai.tool.rows", { count: props.rows.length })}</span>
          </div>
        </Show>
      </div>
    </Show>
  );
}

function Stats(props: { items: { label: string; value: string; hint?: string }[] }) {
  return (
    <div class="ai-tool-stats">
      <For each={props.items}>{(item) => (
        <div class="ai-tool-stat" title={item.hint ?? item.value}>
          <span class="ai-tool-stat-label">{item.label}</span>
          <span class="ai-tool-stat-value">{item.value}</span>
        </div>
      )}</For>
    </div>
  );
}

function Section(props: { title: string; count?: number; children: JSX.Element }) {
  return (
    <section class="ai-tool-section">
      <div class="ai-tool-section-head">
        <span>{props.title}</span>
        <Show when={props.count !== undefined}><span class="ai-tool-section-count">{props.count}</span></Show>
      </div>
      {props.children}
    </section>
  );
}

function Status(props: { value: RecordValue }) {
  const error = () => string(props.value.error);
  const valid = () => typeof props.value.valid === "boolean" ? props.value.valid : null;
  const ok = () => typeof props.value.ok === "boolean" ? props.value.ok : null;
  const status = () => string(props.value.status);
  const failed = () => error().length > 0
    || valid() === false
    || (ok() === false && status() !== "refused")
    || /^(failed|error|refused)$/i.test(status());
  return (
    <Show when={error() || valid() !== null || ok() !== null || status()}>
      <div class="ai-tool-status" classList={{ error: failed(), ok: !failed() && (ok() === true || valid() === true || status() === "ok") }}>
        <span class="ai-tool-status-dot" />
        <span>{i18n.t(failed() ? "ai.activity.failed" : "ai.tool.status")}</span>
        <Show when={status()}><span class="ai-tool-status-note">{statusLabel(status())}</span></Show>
        <Show when={error()}><span class="ai-tool-status-note">{error()}</span></Show>
        <Show when={!error() && string(props.value.message)}><span class="ai-tool-status-note">{string(props.value.message)}</span></Show>
      </div>
    </Show>
  );
}

function Chips(props: { values: unknown[] }) {
  return <div class="ai-tool-chips"><For each={props.values}>{value => <span class="ai-tool-chip">{cell(value)}</span>}</For></div>;
}

/** The fallback for a shape no dedicated view claims: scalars, tables, sections. */
function AutoView(props: { value: unknown; depth?: number }) {
  const depth = () => props.depth ?? 0;
  const object = createMemo(() => record(props.value));
  const scalars = createMemo(() => {
    const value = object();
    if (!value) return [];
    return Object.entries(value)
      .filter(([key, item]) => scalar(item) && !isCodeKey(key) && string(item).length <= 240)
      .slice(0, 16);
  });
  const arrays = createMemo(() => {
    const value = object();
    if (!value) return [];
    return Object.entries(value).filter(([, item]) => Array.isArray(item) && item.length > 0) as [string, unknown[]][];
  });
  const nested = createMemo(() => {
    const value = object();
    if (!value) return [];
    return Object.entries(value)
      .filter(([, item]) => record(item) !== null && Object.keys(record(item)!).length > 0 && !Array.isArray(item))
      .slice(0, 8) as [string, RecordValue][];
  });
  const strings = createMemo(() => {
    const value = object();
    if (!value) return [];
    return Object.entries(value)
      .filter(([key, item]) => typeof item === "string" && isCodeKey(key) && String(item).trim())
      .slice(0, 3) as [string, string][];
  });
  return (
    <Show when={object()} fallback={
      <Show when={rowsOf(props.value)} fallback={<div class="ai-tool-json"><CodeBlock language="JSON" text={JSON.stringify(props.value, null, 2)} /></div>}>
        <DataTable rows={rowsOf(props.value)!} />
      </Show>
    }>
      <Status value={object()!} />
      <Show when={scalars().length}>
        <Stats items={scalars().map(([key, value]) => ({ label: label(key), value: displayCell(key, value) }))} />
      </Show>
      <For each={strings()}>{([key, value]) => (
        <Section title={label(key)}><CodeBlock language={/dsl/i.test(key) ? "DSL" : "Text"} text={value} /></Section>
      )}</For>
      <For each={arrays()}>{([key, values]) => (
        <Section title={label(key)} count={values.length}>
          <Show when={rowsOf(values)} fallback={<Chips values={values} />}>
            <DataTable rows={rowsOf(values)!} />
          </Show>
        </Section>
      )}</For>
      <Show when={depth() < 2}>
        <For each={nested()}>{([key, value]) => (
          <details class="ai-tool-nested">
            <summary>{label(key)}</summary>
            <Show when={isSnapshot(value)} fallback={<AutoView value={value} depth={depth() + 1} />}>
              <SnapshotReceipt value={value} />
            </Show>
          </details>
        )}</For>
      </Show>
    </Show>
  );
}

function TrackTable(props: { tracks: RecordValue[] }) {
  const rows = createMemo(() => props.tracks.map((track, index) => {
    const instrument = record(track.instrument);
    const mix = record(track.mix);
    return {
      index: number(track.index) ?? index,
      name: string(track.name),
      preset: string(track.preset) || "—",
      instrument: instrument
        ? `${string(instrument.category)}/${string(instrument.name)}`
        : string(track.instrument) || "—",
      volume_db: mix ? number(mix.volume_db) ?? number(track.volume_db) : number(track.volume_db),
      pan: mix ? number(mix.pan) ?? number(track.pan) : number(track.pan),
      flags: [
        (mix ? mix.mute : track.mute) ? "M" : "",
        (mix ? mix.solo : track.solo) ? "S" : "",
      ].filter(Boolean).join("+") || "—",
    };
  }));
  return <DataTable rows={rows()} onRowClick={(row) => store.setSelectedTrackIndex(number(row.index) ?? 0)} columns={[
    { key: "index", label: "#", numeric: true },
    { key: "name", label: label("track") },
    { key: "preset", label: label("preset") },
    { key: "volume_db", label: label("vol dB"), numeric: true },
    { key: "pan", label: label("pan"), numeric: true },
    { key: "flags", label: label("M/S") },
  ]} />;
}

function barBeat(value: unknown): number | null {
  if (typeof value === "number") return value;
  const match = /^(\d+)(?::(\d+))?/.exec(string(value));
  return match ? Number(match[1]) + (match[2] ? Number(match[2]) / 16 : 0) : null;
}

function sectionRows(sections: RecordValue[]): RecordValue[] {
  return sections.map((section, index) => {
    const range = record(section.time_range);
    return {
      index: number(section.index) ?? index,
      name: string(section.name),
      start: range ? string(record(range.start)?.bar) || string(range.start) : string(section.start),
      end: range ? string(record(range.end)?.bar) || string(range.end) : string(section.end),
      bars: number(section.bars),
      repeat: number(section.repeat) ?? 1,
      label: string(section.label),
    };
  });
}

function SectionTimeline(props: { sections: RecordValue[] }) {
  const spans = createMemo(() => {
    const rows = props.sections.map((section, index) => {
      const range = record(section.time_range);
      const start = range ? barBeat(record(range.start)?.bar ?? range.start) : barBeat(section.start);
      const end = range ? barBeat(record(range.end)?.bar ?? range.end) : barBeat(section.end);
      return {
        name: string(section.name) || `#${index + 1}`,
        start: start ?? index * 4,
        end: end ?? (start ?? index * 4) + 4,
        repeat: number(section.repeat) ?? 1,
      };
    }).filter((span) => span.end > span.start);
    if (rows.length === 0) return [];
    const start = Math.min(...rows.map((row) => row.start));
    const end = Math.max(...rows.map((row) => row.end));
    return rows.map((row) => ({
      ...row,
      left: ((row.start - start) / (end - start)) * 100,
      width: Math.max(2, ((row.end - row.start) / (end - start)) * 100),
    }));
  });
  return (
    <Show when={spans().length > 0}>
      <div class="ai-tool-timeline" aria-label={i18n.t("ai.tool.timeline")}>
        <For each={spans()}>{span => (
          <div class="ai-tool-timeline-span" style={{ left: `${span.left}%`, width: `${span.width}%` }} title={`${span.name} · ${span.start}–${span.end}`}>
            <span>{span.name}</span>
            <Show when={span.repeat > 1}><small>×{span.repeat}</small></Show>
          </div>
        )}</For>
      </div>
    </Show>
  );
}

function timeSegments(segments: RecordValue[]) {
  const spans = segments.map((segment) => ({
    name: number(segment.rms_dbfs) !== null ? `${formatNumber(number(segment.rms_dbfs) ?? 0)} dB` : "",
    start: number(segment.start_s) ?? 0,
    end: number(segment.end_s) ?? 0,
  })).filter((span) => span.end > span.start);
  if (spans.length === 0) return [];
  const start = Math.min(...spans.map((span) => span.start));
  const end = Math.max(...spans.map((span) => span.end));
  return spans.map((span) => ({
    ...span,
    left: ((span.start - start) / (end - start)) * 100,
    width: Math.max(1, ((span.end - span.start) / (end - start)) * 100),
  }));
}

function TimeSegments(props: { segments: RecordValue[] }) {
  const spans = createMemo(() => timeSegments(props.segments));
  return (
    <Show when={spans().length > 0}>
      <div class="ai-tool-timeline">
        <For each={spans()}>{span => (
          <div class="ai-tool-timeline-span audio" style={{ left: `${span.left}%`, width: `${span.width}%` }} title={`${span.start.toFixed(2)}–${span.end.toFixed(2)} s`}>
            <Show when={span.name}><span>{span.name}</span></Show>
          </div>
        )}</For>
      </div>
    </Show>
  );
}

function ProjectOverview(props: { value: RecordValue }) {
  const project = createMemo(() => record(props.value.project));
  const properties = createMemo(() => project() ? record(project()!.properties) : null);
  const tracks = createMemo(() => {
    const list = project()?.tracks;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const sections = createMemo(() => {
    const list = project()?.sections;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const presets = createMemo(() => {
    const list = project()?.presets;
    return Array.isArray(list) ? list : [];
  });
  const meter = createMemo(() => {
    const signature = record(properties()?.time_signature);
    return signature ? `${number(signature.numerator) ?? 4}/${number(signature.denominator) ?? 4}` : "4/4";
  });
  return (
    <Show when={project()} fallback={<AutoView value={props.value} />}>
      <Stats items={[
        { label: label("project"), value: string(project()!.name) || "—" },
        { label: label("tempo"), value: `${number(properties()?.tempo) ?? "—"} BPM` },
        { label: label("meter"), value: meter() },
        { label: label("tracks"), value: String(tracks().length) },
        { label: label("sections"), value: String(sections().length) },
        { label: label("presets"), value: String(presets().length) },
        { label: label("duration"), value: `${formatNumber(number(props.value.duration_seconds) ?? 0)} s` },
        { label: label("revision"), value: String(number(props.value.revision) ?? "—") },
        { label: label("dirty"), value: yesNo(!!props.value.dirty) },
      ]} />
      <Show when={string(props.value.file_path)}>
        <div class="ai-tool-path">{string(props.value.file_path)}</div>
      </Show>
      <Show when={tracks().length}>
        <Section title={label("tracks")} count={tracks().length}>
          <TrackTable tracks={tracks().map((track, index) => {
            const mix = record(track.mix);
            const instrument = record(track.instrument);
            return {
              index,
              name: string(track.name),
              preset: string(track.preset),
              instrument: instrument ? `${string(instrument.category)}/${string(instrument.name)}` : "",
              ...(mix ?? {}),
              output: string(mix?.output) || "master",
            };
          })} />
        </Section>
      </Show>
      <Show when={sections().length}>
        <Section title={label("sections")} count={sections().length}>
          <SectionTimeline sections={sections()} />
          <DataTable rows={sectionRows(sections())} columns={[
            { key: "index", label: "#", numeric: true },
            { key: "name", label: label("name") },
            { key: "start", label: label("start") },
            { key: "end", label: label("end") },
            { key: "bars", label: label("bars"), numeric: true },
            { key: "repeat", label: label("repeat"), numeric: true },
            { key: "label", label: label("label") },
          ]} />
        </Section>
      </Show>
    </Show>
  );
}

function SectionsView(props: { value: RecordValue }) {
  const sections = createMemo(() => {
    const list = props.value.sections;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return (
    <>
      <Stats items={[
        { label: label("sections"), value: String(number(props.value.count) ?? sections().length) },
        { label: label("rendered end"), value: string(props.value.rendered_end) || "—" },
        { label: label("rendered bars"), value: formatNumber(number(props.value.rendered_bars) ?? 0) },
        { label: label("rendered seconds"), value: formatNumber(number(props.value.rendered_seconds) ?? 0) },
      ]} />
      <Show when={sections().length}>
        <SectionTimeline sections={sections()} />
        <DataTable rows={sectionRows(sections())} columns={[
          { key: "index", label: "#", numeric: true },
          { key: "name", label: label("name") },
          { key: "start", label: label("start") },
          { key: "end", label: label("end") },
          { key: "bars", label: label("bars"), numeric: true },
          { key: "repeat", label: label("repeat"), numeric: true },
          { key: "label", label: label("label") },
        ]} />
      </Show>
    </>
  );
}

type SourceWindow = { start: number; duration: number };

function sourceWindow(start: unknown, end: unknown): SourceWindow | undefined {
  const from = number(start);
  const to = number(end);
  if (from === null || from < 0 || to === null || to <= from) return undefined;
  return { start: from, duration: to - from };
}

function AudioCard(props: { path: string; base64?: string; mime?: string; duration?: number; window?: SourceWindow }) {
  const [loaded, setLoaded] = createSignal<Awaited<ReturnType<typeof previewAudioFile>>>();
  const [failed, setFailed] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [startText, setStartText] = createSignal(String(props.window?.start ?? 0));
  const adjustable = () => !props.window || props.window.duration > 30.000001;
  const start = () => adjustable() ? Number(startText()) : props.window!.start;
  const duration = () => props.window
    ? Math.min(30, props.window.start + props.window.duration - start())
    : undefined;
  const canLoad = () => !loading() && startText().trim() !== "" && Number.isFinite(start())
    && start() >= (props.window?.start ?? 0) && (duration() ?? 30) > 0;
  const audio = createMemo<PreviewAudio | undefined>(() => props.base64
    ? { mime: props.mime || "audio/wav", base64: props.base64 }
    : loaded());
  const waveform = createMemo(() => audio()?.mime === "audio/wav"
    ? pcmPreviewWaveform(audio()!.base64)
    : null);

  const load = async () => {
    if (!canLoad()) return;
    setFailed(false);
    setLoading(true);
    setLoaded(undefined);
    try {
      const preview = await previewAudioFile(props.path, start(), duration());
      if (!preview?.base64) throw new Error("Empty audio preview");
      setLoaded(preview);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div class="ai-tool-audio">
      <Show when={!props.base64 && (adjustable() || !audio())}>
        <div class="ai-tool-audio-note ai-tool-audio-request">
          <Show when={adjustable()}>
            <label class="ai-tool-audio-offset">
              <span>{i18n.t("ai.tool.audioSourceStart")}</span>
              <input type="number" min={props.window?.start ?? 0} step="any" value={startText()} disabled={loading()}
                onInput={(event) => setStartText(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === " ") event.stopPropagation();
                  if (event.key === "Enter") { event.preventDefault(); void load(); }
                }} />
            </label>
          </Show>
          <button type="button" class="ai-tool-raw-toggle" disabled={!canLoad()}
            onKeyDown={(event) => { if (event.key === " ") event.stopPropagation(); }}
            onClick={() => void load()}>{i18n.t(loading() ? "ai.tool.audioPreparing" : "ai.tool.audioLoad")}</button>
          <Show when={failed()}><span role="status">{i18n.t("ai.tool.audioFailed")}</span></Show>
        </div>
      </Show>
      <Show when={audio()} keyed>
        {(preview) => <>
          <Show when={waveform()}>{(shape) =>
            <svg class="ai-tool-waveform" viewBox={`0 -1 ${shape().columns} 2`}
              preserveAspectRatio="none" aria-hidden="true">
              <path d={shape().path} fill="none" stroke="var(--accent)"
                stroke-width="1" vector-effect="non-scaling-stroke" />
            </svg>
          }</Show>
          <Show when={loaded()}>
            {(excerpt) => <div class="ai-tool-audio-note">{i18n.t("ai.tool.audioExcerptRange", {
              from: excerpt().start_s.toFixed(3), to: excerpt().end_s.toFixed(3),
            })}</div>}
          </Show>
          <Show when={props.window && loaded()?.truncated}>
            <div class="ai-tool-warning">{i18n.t("ai.result.clipShort")}</div>
          </Show>
          <PreviewAudioPlayer src={`data:${preview.mime};base64,${preview.base64}`}
            duration={waveform()?.duration ?? loaded()?.clip_duration_s ?? props.duration} />
        </>}
      </Show>
    </div>
  );
}

function AudioResult(props: { value: RecordValue }) {
  const audio = createMemo(() => record(props.value.audio));
  const path = () => string(props.value.path);
  return (
    <>
      <Show when={path()} keyed fallback={<AutoView value={props.value} />}>
        {(file) => <AudioCard path={file} base64={string(audio()?.base64)} mime={string(audio()?.mime)} duration={number(props.value.duration_s) ?? undefined} />}
      </Show>
      <Stats items={[
        { label: label("path"), value: path() || "—" },
        { label: label("start"), value: `${formatNumber(number(props.value.start_s) ?? 0)} s` },
        { label: label("end"), value: `${formatNumber(number(props.value.end_s) ?? 0)} s` },
        { label: label("duration"), value: `${formatNumber(number(props.value.duration_s) ?? 0)} s` },
        { label: label("tracks"), value: Array.isArray(props.value.tracks) ? props.value.tracks.map(String).join(", ") : "—" },
        { label: label("cache"), value: typeof props.value.cache_hit === "boolean" ? i18n.t(props.value.cache_hit ? "ai.result.cacheReused" : "ai.result.cacheNew") : "—" },
      ]} />
      <Show when={props.value.truncated}><div class="ai-tool-warning">{i18n.t("ai.result.clipShort")}</div></Show>
    </>
  );
}

function Spectrum(props: { value: RecordValue }) {
  const centers = createMemo(() => Array.isArray(props.value.centers_hz) ? props.value.centers_hz.map(Number) : []);
  const energy = createMemo(() => Array.isArray(props.value.energy_db) ? props.value.energy_db.map(Number) : []);
  const bounds = createMemo(() => {
    const values = energy().filter(Number.isFinite);
    if (values.length === 0) return { min: 0, max: 1 };
    const min = Math.min(...values);
    const max = Math.max(...values);
    return { min, max: max === min ? min + 1 : max };
  });
  return (
    <Show when={energy().length > 0}>
      <div class="ai-tool-spectrum" aria-label={i18n.t("ai.tool.spectrum")}>
        <For each={energy()}>{(value, index) => {
          const normalized = () => (value - bounds().min) / (bounds().max - bounds().min);
          return (
            <div class="ai-tool-spectrum-bar" title={`${formatNumber(centers()[index()] ?? 0)} Hz · ${formatNumber(value)} dB`}>
              <span style={{ height: `${Math.max(2, normalized() * 100)}%` }} />
            </div>
          );
        }}</For>
      </div>
      <div class="ai-tool-spectrum-scale">
        <For each={centers()}>{(center) => <span>{center >= 1000 ? `${formatNumber(center / 1000)}k` : formatNumber(center)}</span>}</For>
      </div>
    </Show>
  );
}

function BeatEvidenceView(props: { path: string; grid: RecordValue }) {
  const start = () => number(props.grid.start_s) ?? 0;
  const end = () => number(props.grid.end_s) ?? start();
  const candidates = () => rowsOf(props.grid.candidates) ?? [];
  return (
    <>
      <Stats items={[
        { label: label("file"), value: props.path },
        { label: label("window"), value: `${start().toFixed(3)}–${end().toFixed(3)} s` },
        { label: i18n.t("ai.result.beatAttacks"), value: cell(props.grid.attack_count) },
      ]} />
      <Show when={props.path ? sourceWindow(start(), end()) : undefined} keyed>
        {(window) => <AudioCard path={props.path} window={window} />}
      </Show>
      <Section title={i18n.t("ai.result.beatCandidates")} count={candidates().length}>
        <Show when={candidates().length} fallback={<div class="ai-tool-empty">{i18n.t("ai.result.beatInsufficient")}</div>}>
          <For each={candidates()}>{(candidate) => {
            const anchors = () => Array.isArray(candidate.anchors_s)
              ? candidate.anchors_s.map(number).filter((time): time is number => time !== null)
              : [];
            return <div class="ai-tool-beat-candidate">
              <Stats items={[
                { label: label("tempo"), value: `${cell(candidate.tempo_bpm)} BPM` },
                { label: i18n.t("ai.result.beatSupport"), value: cell(candidate.score) },
                { label: i18n.t("ai.result.beatOrigin"), value: `${number(candidate.pulse_origin_s)?.toFixed(3) ?? "—"} s` },
              ]} />
              <Show when={anchors().length > 0 && end() > start()}>
                <svg class="ai-tool-beat-anchors" viewBox="0 0 100 10" preserveAspectRatio="none"
                  role="img" aria-label={i18n.t("ai.result.beatAnchors")}>
                  <For each={anchors()}>{(time) => {
                    const x = 100 * (time - start()) / (end() - start());
                    return <line x1={x} x2={x} y1="1" y2="9"><title>{time.toFixed(3)} s</title></line>;
                  }}</For>
                </svg>
              </Show>
            </div>;
          }}</For>
        </Show>
      </Section>
      <div class="ai-tool-warning">{i18n.t("ai.result.beatUnconfirmed")}</div>
    </>
  );
}

function TransientEvidenceView(props: { path: string; evidence: RecordValue }) {
  const start = () => number(props.evidence.start_s) ?? 0;
  const end = () => number(props.evidence.end_s) ?? start();
  const attacks = () => (rowsOf(props.evidence.attacks) ?? []).map((attack) => ({
    file_seconds: number(attack.file_seconds)?.toFixed(4),
    strength: number(attack.strength)?.toFixed(4),
  }));
  const resolutionMs = () => {
    const seconds = number(props.evidence.resolution_seconds);
    return seconds === null ? "—" : `${(seconds * 1000).toFixed(2)} ms`;
  };
  const gapMs = () => {
    const gap = number(props.evidence.effective_gap_ms);
    return gap === null ? "—" : `${gap.toFixed(2)} ms`;
  };
  return (
    <>
      <Stats items={[
        { label: label("file"), value: props.path },
        { label: label("window"), value: `${start().toFixed(3)}–${end().toFixed(3)} s` },
        { label: i18n.t("ai.result.attackResolution"), value: resolutionMs() },
        { label: i18n.t("ai.result.attackGap"), value: gapMs() },
      ]} />
      <Show when={props.path ? sourceWindow(start(), end()) : undefined} keyed>
        {(window) => <AudioCard path={props.path} window={window} />}
      </Show>
      <Section title={i18n.t("ai.result.beatAttacks")} count={attacks().length}>
        <DataTable rows={attacks()} max={128} columns={[
          { key: "file_seconds", label: i18n.t("ai.result.attackFileTime"), numeric: true },
          { key: "strength", label: i18n.t("ai.result.attackStrength"), numeric: true },
        ]} />
      </Section>
    </>
  );
}

function LevelWindowView(props: { value: RecordValue; window: RecordValue }) {
  const level = () => record(props.value.level) ?? {};
  return (
    <>
      <Stats items={[
        { label: label("file"), value: cell(props.value.path) },
        { label: label("window"), value: `${number(props.window.start_s)?.toFixed(3)}–${number(props.window.end_s)?.toFixed(3)} s` },
        { label: label("peak"), value: `${cell(level().peak_dbfs)} dBFS` },
        { label: "RMS", value: `${cell(level().rms_dbfs)} dBFS` },
        { label: label("crest"), value: `${cell(level().crest_db)} dB` },
      ]} />
      <Warnings value={props.value.warnings} />
    </>
  );
}

function AnalysisView(props: { value: RecordValue }) {
  const level = createMemo(() => record(props.value.level) ?? {});
  const stereo = createMemo(() => record(props.value.stereo) ?? {});
  const octave = createMemo(() => record(props.value.octave) ?? {});
  const peaks = createMemo(() => {
    const list = record(props.value.spectral_peaks)?.peaks;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const silence = createMemo(() => {
    const list = props.value.silence;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const sections = createMemo(() => {
    const list = props.value.sections;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return (
    <>
      <Stats items={[
        { label: label("file"), value: string(props.value.path) || "—" },
        { label: label("duration"), value: `${formatNumber(number(props.value.duration_s) ?? 0)} s` },
        { label: label("sample rate"), value: `${number(props.value.sample_rate) ?? "—"} Hz` },
        { label: label("channels"), value: String(number(props.value.channels) ?? "—") },
        { label: label("peak"), value: `${formatNumber(number(level().peak_dbfs) ?? 0)} dBFS` },
        { label: label("true peak"), value: `${formatNumber(number(props.value.true_peak_dbfs) ?? 0)} dBFS` },
        { label: "RMS", value: `${formatNumber(number(level().rms_dbfs) ?? 0)} dBFS` },
        { label: label("crest"), value: `${formatNumber(number(level().crest_db) ?? 0)} dB` },
        { label: label("integrated"), value: number(level().integrated_lufs) === null ? "—" : `${formatNumber(number(level().integrated_lufs) ?? 0)} LUFS` },
        { label: label("stereo corr"), value: number(stereo().correlation) === null ? "—" : formatNumber(number(stereo().correlation) ?? 0) },
        { label: label("mid/side"), value: number(stereo().mid_side_ratio_db) === null ? "—" : `${formatNumber(number(stereo().mid_side_ratio_db) ?? 0)} dB` },
      ]} />
      <Show when={string(props.value.path)} keyed>
        {(path) => <AudioCard path={path} />}
      </Show>
      <Section title={i18n.t("ai.tool.spectrum")}><Spectrum value={octave()} /></Section>
      <Show when={peaks().length}>
        <Section title={label("spectral peaks")} count={peaks().length}>
          <DataTable rows={peaks()} columns={[
            { key: "frequency_hz", label: "Hz", numeric: true },
            { key: "level_db", label: "dB", numeric: true },
            { key: "relative_db", label: label("relative dB"), numeric: true },
          ]} />
        </Section>
      </Show>
      <Show when={sections().length}>
        <Section title={label("sections")} count={sections().length}>
          <TimeSegments segments={sections()} />
          <DataTable rows={sections()} columns={[
            { key: "start_s", label: label("start s"), numeric: true },
            { key: "end_s", label: label("end s"), numeric: true },
            { key: "peak_dbfs", label: label("peak dB"), numeric: true },
            { key: "rms_dbfs", label: label("RMS dB"), numeric: true },
          ]} />
        </Section>
      </Show>
      <Show when={silence().length}>
        <Section title={label("silence")} count={silence().length}>
          <DataTable rows={silence()} columns={[
            { key: "start_s", label: label("start s"), numeric: true },
            { key: "end_s", label: label("end s"), numeric: true },
            { key: "duration_s", label: label("duration s"), numeric: true },
          ]} />
        </Section>
      </Show>
      <Warnings value={props.value.warnings} />
    </>
  );
}

function Warnings(props: { value: unknown }) {
  const warnings = () => Array.isArray(props.value) ? props.value.map(String) : [];
  return <Show when={warnings().length}><div class="ai-tool-warnings"><For each={warnings()}>{text => <div>{text}</div>}</For></div></Show>;
}

function NoteRoll(props: { value: RecordValue }) {
  const notes = createMemo(() => {
    const list = props.value.notes;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const pitch = createMemo(() => {
    const values = notes().map((note) => number(note.midi)).filter((value): value is number => value !== null);
    if (values.length === 0) return { min: 48, max: 72 };
    const min = Math.min(...values);
    const max = Math.max(...values);
    return { min: min - 1, max: max + 1 };
  });
  const end = createMemo(() => Math.max(1, ...notes().map((note) => number(note.end_s) ?? 0)));
  const names = createMemo(() => [...new Set(notes().map((note) => string(note.name)).filter(Boolean))].slice(0, 12));
  return (
    <Show when={notes().length} fallback={<div class="ai-tool-empty">{i18n.t("ai.tool.empty")}</div>}>
      <svg class="ai-tool-roll" viewBox={`0 0 100 100`} preserveAspectRatio="none" role="img" aria-label={i18n.t("ai.tool.notes", { count: notes().length })}>
        <For each={notes()}>{(note) => {
          const start = number(note.start_s) ?? 0;
          const finish = number(note.end_s) ?? start;
          const midi = number(note.midi) ?? pitch().min;
          const top = 100 - ((midi - pitch().min) / (pitch().max - pitch().min)) * 100;
          const height = Math.max(1.5, 100 / (pitch().max - pitch().min));
          const left = (start / end()) * 100;
          const width = Math.max(0.4, ((finish - start) / end()) * 100);
          const confidence = number(note.confidence) ?? 1;
          return <rect class="ai-tool-roll-note" x={left} y={Math.max(0, top - height)} width={width} height={height} rx="0.5" style={{ opacity: String(0.35 + confidence * 0.65) }}>
            <title>{`${string(note.name)} · ${formatNumber(confidence)}`}</title>
          </rect>;
        }}</For>
      </svg>
      <div class="ai-tool-roll-caption">
        <span>{i18n.t("ai.tool.notes", { count: notes().length })}{names().length ? ` · ${names().join(" ")}` : ""}</span>
        <Show when={props.value.polyphonic}><span class="ai-tool-warning">{i18n.t("ai.result.polyphonic")}</span></Show>
      </div>
    </Show>
  );
}

function NotesView(props: { value: RecordValue }) {
  const notes = createMemo(() => {
    const list = props.value.notes;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return (
    <>
      <Stats items={[
        { label: label("file"), value: string(props.value.path) || "—" },
        { label: label("window"), value: `${formatNumber(number(props.value.start_s) ?? 0)}–${formatNumber(number(props.value.end_s) ?? 0)} s` },
        { label: label("tempo"), value: `${formatNumber(number(props.value.tempo_bpm) ?? 0)} BPM` },
        { label: label("meter"), value: string(props.value.time_signature) || "—" },
        { label: label("notes"), value: String(notes().length) },
      ]} />
      <Show when={string(props.value.path) ? sourceWindow(props.value.start_s, props.value.end_s) : undefined} keyed>
        {(window) => <AudioCard path={string(props.value.path)} window={window} />}
      </Show>
      <NoteRoll value={props.value} />
      <Show when={notes().length}>
        <details class="ai-tool-nested">
          <summary>{i18n.t("ai.tool.notes", { count: notes().length })}</summary>
          <DataTable rows={notes()} columns={[
            { key: "name", label: label("note") },
            { key: "start_s", label: label("start s"), numeric: true },
            { key: "end_s", label: label("end s"), numeric: true },
            { key: "duration_beats", label: label("beats"), numeric: true },
            { key: "confidence", label: label("confidence"), numeric: true },
            { key: "bar", label: label("bar"), numeric: true },
          ]} />
        </details>
      </Show>
      <Warnings value={props.value.warnings} />
    </>
  );
}

function ScoreCard(props: { value: RecordValue }) {
  const score = createMemo(() => record(props.value.score) ?? {});
  const value = createMemo(() => number(score().value) ?? 0);
  const components = createMemo(() => Object.entries(score())
    .filter(([, item]) => record(item) !== null && number(record(item)!.term) !== null)
    .map(([key, item]) => {
      const component = record(item)!;
      return {
        term: label(key),
        term_value: number(component.term),
        weight: number(component.weight),
        contribution: number(component.contribution),
      };
    }));
  return (
    <>
      <div class="ai-tool-score">
        <div class="ai-tool-score-value">{formatNumber(value() * 100)}%</div>
        <div class="ai-tool-score-track"><span style={{ width: `${Math.max(0, Math.min(1, value())) * 100}%` }} /></div>
        <div class="ai-tool-score-label">{string(score().label) || i18n.t("ai.tool.score")}</div>
      </div>
      <Show when={components().length}>
        <Section title={label("score components")} count={components().length}>
          <DataTable rows={components()} columns={[
            { key: "term", label: label("term") },
            { key: "term_value", label: label("value"), numeric: true },
            { key: "weight", label: label("weight"), numeric: true },
            { key: "contribution", label: label("contribution"), numeric: true },
          ]} />
        </Section>
      </Show>
    </>
  );
}

function CompareView(props: { value: RecordValue }) {
  const alignment = createMemo(() => record(props.value.alignment) ?? {});
  const level = createMemo(() => record(props.value.level) ?? {});
  const warnings = () => props.value.warnings;
  const listening = createMemo(() => record(props.value.listening));
  const passage = (side: "reference" | "candidate") => {
    const window = record(props.value.window);
    const path = string(record(props.value[side])?.path);
    const range = sourceWindow(window?.[`${side}_start_s`], window?.[`${side}_end_s`]);
    return path && range ? { path, range } : null;
  };
  const source = (side: "reference" | "candidate") => {
    const path = string(record(props.value[side])?.path) || "—";
    const window = record(props.value.window);
    const start = number(window?.[`${side}_start_s`]);
    const end = number(window?.[`${side}_end_s`]);
    return start === null || end === null ? path : `${path} · ${formatNumber(start)}–${formatNumber(end)} s`;
  };
  return (
    <>
      <Show when={props.value.listening_only !== true}><ScoreCard value={props.value} /></Show>
      <Stats items={[
        { label: label("reference"), value: source("reference") },
        { label: label("candidate"), value: source("candidate") },
        ...(props.value.listening_only === true ? [] : [
          { label: label("lag"), value: `${formatNumber(number(alignment().lag_ms) ?? 0)} ms` },
          { label: label("alignment"), value: statusLabel(string(alignment().confidence)) || "—" },
          { label: label("spectral corr"), value: formatNumber(number(alignment().spectral_corr) ?? 0) },
          { label: label("aligned"), value: `${formatNumber(number(props.value.aligned_duration_s) ?? 0)} s` },
          { label: label("ref LUFS"), value: `${formatNumber(number(record(level().reference)?.integrated_lufs) ?? 0)}` },
          { label: label("cand LUFS"), value: `${formatNumber(number(record(level().candidate)?.integrated_lufs) ?? 0)}` },
        ]),
      ]} />
      <Show when={passage("reference") && passage("candidate")}>
        <Section title={i18n.t("ai.tool.audioSourcePreviews")}>
          <For each={["reference", "candidate"] as const}>{(side) =>
            <Show when={passage(side)} keyed>
              {(item) => <div class="ai-tool-audio-source">
                <span>{label(side)}</span>
                <AudioCard path={item.path} window={item.range} />
              </div>}
            </Show>
          }</For>
        </Section>
      </Show>
      <Show when={listening()}>
        {(pair) => <Section title={i18n.t("ai.result.listeningPair")}>
          <div class="ai-tool-warning">{i18n.t(pair().status === "matched" ? "ai.result.listeningMatched"
            : pair().status === "unsupported" ? "ai.result.listeningUnsupported" : "ai.result.listeningUnverified")}</div>
          <Show when={number(pair().delta_lu) !== null}>
            <Stats items={[
              { label: label("ref LUFS"), value: cell(pair().reference_lufs) },
              { label: label("cand LUFS"), value: cell(pair().candidate_lufs) },
              { label: i18n.t("ai.result.listeningDelta"), value: `${number(pair().delta_lu)!.toFixed(3)} LU` },
            ]} />
          </Show>
        </Section>}
      </Show>
      <AutoView value={Object.fromEntries(Object.entries(props.value).filter(([key]) => !["score", "alignment", "reference", "candidate", "level", "window", "listening", "listening_only"].includes(key)))} />
      <Warnings value={warnings()} />
    </>
  );
}

function DiffView(props: { value: RecordValue }) {
  const diff = createMemo(() => record(props.value.diff) ?? props.value);
  const hunks = createMemo(() => {
    const list = diff().hunks;
    return Array.isArray(list) ? list : [];
  });
  return (
    <>
      <Show when={record(props.value.snapshot)}>
        <SnapshotStats value={record(props.value.snapshot)!} />
      </Show>
      <Show when={hunks().length} fallback={<AutoView value={props.value} />}>
        <div class="ai-tool-diff">
          <For each={hunks()}>{(hunk) => {
            const lines = Array.isArray(record(hunk)?.lines) ? record(hunk)!.lines as unknown[] : [];
            return <For each={lines}>{(line) => {
              const item = record(line) ?? {};
              const kind = string(item.kind);
              return <div class="ai-diff-line" classList={{ added: kind === "Added", removed: kind === "Removed" }}>
                {kind === "Added" ? "+ " : kind === "Removed" ? "- " : "  "}{string(item.text)}
              </div>;
            }}</For>;
          }}</For>
        </div>
      </Show>
    </>
  );
}

function SnapshotStats(props: { value: RecordValue }) {
  return (
    <Stats items={[
      { label: label("tracks"), value: String(number(props.value.track_count) ?? "—") },
      { label: label("revision"), value: String(number(props.value.revision) ?? "—") },
      { label: label("dirty"), value: yesNo(!!props.value.dirty) },
      { label: label("file"), value: string(props.value.file_path) || "—" },
      { label: label("duration"), value: `${formatNumber(number(props.value.duration_seconds) ?? 0)} s` },
    ]} />
  );
}

function ValidationView(props: { value: RecordValue }) {
  const errors = () => Array.isArray(props.value.errors) ? props.value.errors.map(String) : [];
  return (
    <>
      <div class="ai-tool-status" classList={{ error: props.value.valid === false, ok: props.value.valid === true }}>
        <span class="ai-tool-status-dot" />
        <span>{props.value.valid === false ? i18n.t("ai.tool.errors") : i18n.t("ai.tool.status")}</span>
      </div>
      <Show when={errors().length}>
        <Section title={i18n.t("ai.tool.errors")} count={errors().length}>
          <ul class="ai-tool-errors"><For each={errors()}>{error => <li>{error}</li>}</For></ul>
        </Section>
      </Show>
      <Show when={string(props.value.parsed_dsl)}>
        <Section title={label("parsed DSL")}><CodeBlock language="DSL" text={string(props.value.parsed_dsl)} /></Section>
      </Show>
    </>
  );
}

function CodeView(props: { value: RecordValue }) {
  const blocks = createMemo(() => Object.entries(props.value)
    .filter(([key, value]) => typeof value === "string" && value.trim() && isCodeKey(key))
    .slice(0, 4) as [string, string][]);
  return (
    <>
      <Show when={blocks().length} fallback={<AutoView value={props.value} />}>
        <For each={blocks()}>{([key, text]) => (
          <Section title={label(key)}><CodeBlock language={/dsl/i.test(key) ? "DSL" : "Text"} text={text} /></Section>
        )}</For>
      </Show>
    </>
  );
}

function TableValue(props: { value: RecordValue; key: string; columns?: Column[] }) {
  const rows = createMemo(() => {
    const list = props.value[props.key];
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return <Show when={rows().length} fallback={<AutoView value={props.value} />}><DataTable rows={rows()} columns={props.columns} /></Show>;
}

function PluginTable(props: { value: RecordValue }) {
  const rows = createMemo(() => {
    const list = props.value.entries ?? props.value.plugins ?? props.value.installed;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return (
    <>
      <Stats items={[
        { label: label("source"), value: string(props.value.source) || "—" },
        { label: label("total"), value: String(number(props.value.total) ?? rows().length) },
      ]} />
      <Show when={rows().length} fallback={<TableValue value={props.value} key="entries" />}>
        <DataTable rows={rows()} columns={[
          { key: "name", label: label("name") },
          { key: "vendor", label: label("vendor") },
          { key: "installed", label: label("installed") },
          { key: "installable", label: label("installable") },
          { key: "mode", label: label("mode") },
          { key: "tags", label: label("tags") },
        ]} />
      </Show>
    </>
  );
}

function PresetsView(props: { value: RecordValue }) {
  const presets = createMemo(() => {
    const list = props.value.presets;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const rows = createMemo(() => presets().map((preset) => ({
    name: string(preset.name),
    source: string(preset.source),
    plugin: string(preset.plugin),
    program: number(preset.program),
    patch: string(preset.patch),
    file: string(preset.file),
    params: Array.isArray(preset.params) ? preset.params.length : 0,
  })));
  const soundfonts = createMemo(() => Array.isArray(props.value.soundfonts) ? props.value.soundfonts : []);
  return (
    <>
      <DataTable rows={rows()} columns={[
        { key: "name", label: label("name") },
        { key: "source", label: label("source") },
        { key: "program", label: label("program"), numeric: true },
        { key: "patch", label: label("patch") },
      ]} />
      <Show when={soundfonts().length}>
        <Section title={label("soundfonts")} count={soundfonts().length}><Chips values={soundfonts()} /></Section>
      </Show>
      <Show when={string(props.value.soundfonts_dir)}>
        <div class="ai-tool-path">{string(props.value.soundfonts_dir)}</div>
      </Show>
    </>
  );
}

function schemaRow(entry: RecordValue): RecordValue {
  const schema = record(entry.schema) ?? {};
  return {
    type: string(entry.type),
    id: string(schema.id) || string(schema.prefix),
    name: string(schema.name) || string(schema.id),
    group: string(schema.group),
    canonical: string(schema.canonical),
    status: string(schema.status),
    default: schema.default,
    min: schema.min,
    max: schema.max,
    unit: string(schema.unit),
  };
}

function DevicePresetTable(props: { value: RecordValue }) {
  const rows = createMemo(() => {
    const list = props.value.presets;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null).map((preset) => ({
      name: string(preset.name),
      device: string(preset.device),
      category: string(preset.category),
      source: string(preset.source),
      intent: string(preset.intent),
      params: record(preset.params) ? Object.keys(record(preset.params)!).length : 0,
    })) : [];
  });
  return (
    <>
      <DataTable rows={rows()} columns={[
        { key: "name", label: label("name") },
        { key: "device", label: label("device") },
        { key: "category", label: label("category") },
        { key: "source", label: label("source") },
        { key: "params", label: label("params"), numeric: true },
        { key: "intent", label: label("intent") },
      ]} />
      <Warnings value={props.value.warnings} />
    </>
  );
}

function DeviceCatalogView(props: { value: RecordValue; args?: string }) {
  const instrument = createMemo(() => {
    try {
      return string(record(JSON.parse(props.args ?? "{}"))?.instrument);
    } catch {
      return "";
    }
  });
  const devices = createMemo(() => {
    const list = props.value.devices;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  const entries = createMemo(() => {
    const list = props.value.entries;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null).map(schemaRow) : [];
  });
  return (
    <Show when={instrument()} fallback={
      <>
        <Stats items={[
          { label: label("instrument"), value: string(props.value.instrument) || "mixer" },
          { label: label("version"), value: string(props.value.version) || "—" },
          { label: label("devices"), value: String(devices().length) },
        ]} />
        <DataTable rows={devices().map((device) => ({
          id: string(device.id),
          name: string(device.name),
          category: string(device.category),
          params: Array.isArray(device.params) ? device.params.length : 0,
        }))} columns={[
          { key: "name", label: label("name") },
          { key: "category", label: label("category") },
          { key: "params", label: label("params"), numeric: true },
        ]} />
      </>
    }>
      <Stats items={[
        { label: label("instrument"), value: instrument() || "—" },
        { label: label("version"), value: string(props.value.version) || "—" },
        { label: label("entries"), value: String(entries().length) },
        { label: label("total"), value: String(number(props.value.total) ?? entries().length) },
        { label: label("offset"), value: String(number(props.value.offset) ?? 0) },
      ]} />
      <DataTable rows={entries()} columns={[
        { key: "type", label: label("type") },
        { key: "id", label: label("id") },
        { key: "name", label: label("name") },
        { key: "status", label: label("status") },
      ]} />
    </Show>
  );
}

function ParamTable(props: { value: RecordValue }) {
  const rows = createMemo(() => {
    const list = props.value.params ?? props.value.parameters ?? props.value;
    return Array.isArray(list) ? list.filter((item): item is RecordValue => record(item) !== null) : [];
  });
  return (
    <Show when={rows().length} fallback={<AutoView value={props.value} />}>
      <DataTable rows={rows()} columns={[
        { key: "id", label: label("id") },
        { key: "name", label: label("name") },
        { key: "display", label: label("value") },
        { key: "current", label: label("current"), numeric: true },
        { key: "min", label: label("min"), numeric: true },
        { key: "max", label: label("max"), numeric: true },
        { key: "default", label: label("default"), numeric: true },
        { key: "unit", label: label("unit") },
      ]} />
    </Show>
  );
}

function UndoView(props: { value: RecordValue | RecordValue[] }) {
  const rows = createMemo(() => Array.isArray(props.value) ? props.value : [props.value]);
  return <DataTable rows={rows()} columns={[
    { key: "index", label: "#", numeric: true },
    { key: "name", label: label("edit") },
    { key: "applied", label: label("applied") },
    { key: "timestamp", label: label("time") },
  ]} />;
}

/** A project snapshot, or its metadata-only form after the model wire drops the AST. */
function isSnapshot(value: RecordValue): boolean {
  return record(value.project) !== null
    || value.project_ast_omitted === true
    || (typeof value.track_count === "number" && typeof value.revision === "number"
      && (typeof value.dirty === "boolean" || typeof value.dsl_source === "string"));
}

function TransportView(props: { value: RecordValue }) {
  const project = createMemo(() => record(props.value.project));
  return (
    <>
      <Stats items={[
        { label: label("playing"), value: yesNo(!!props.value.playing) },
        { label: label("position"), value: `${formatNumber(number(props.value.position_beats) ?? 0)} ${i18n.t("tooltip.beatsUnit")}` },
        { label: label("seconds"), value: `${formatNumber(number(props.value.position_seconds) ?? 0)} s` },
        { label: label("duration"), value: `${formatNumber(number(props.value.duration_seconds) ?? 0)} s` },
        { label: label("prepared"), value: yesNo(!!props.value.prepared) },
        { label: label("warming"), value: yesNo(!!props.value.warming) },
        { label: label("status"), value: statusLabel(string(props.value.status)) || "—" },
      ]} />
      <Show when={project()}>
        <details class="ai-tool-nested">
          <summary>{label("project")}</summary>
          <SnapshotReceipt value={project()!} />
        </details>
      </Show>
      <Warnings value={props.value.warnings} />
    </>
  );
}

function SnapshotReceipt(props: { value: RecordValue }) {
  const dsl = () => string(props.value.dsl_source);
  return (
    <>
      <Show when={record(props.value.project)} fallback={
        <>
          <Stats items={[
            { label: label("tracks"), value: String(number(props.value.track_count) ?? "—") },
            { label: label("revision"), value: String(number(props.value.revision) ?? "—") },
            { label: label("dirty"), value: yesNo(!!props.value.dirty) },
            { label: label("duration"), value: `${formatNumber(number(props.value.duration_seconds) ?? 0)} s` },
          ]} />
        </>
      }>
        <ProjectOverview value={props.value} />
      </Show>
      <Show when={dsl()}>
        <details class="ai-tool-nested">
          <summary>{label("DSL")}</summary>
          <CodeBlock language="DSL" text={dsl()} />
        </details>
      </Show>
    </>
  );
}

/**
 * A result the loop bounded to 8 000 characters. The backend keeps the full
 * payload in memory for the conversation that produced it, so the view asks
 * for it; after a restart only the summary and the preview remain.
 */
function TruncatedResult(props: { tool: string; value: RecordValue; args?: string; sessionId?: string }) {
  const [full] = createResource(
    () => {
      const id = string(props.value.tool_call_id);
      return id && props.sessionId ? { session: props.sessionId, id } : null;
    },
    ({ session, id }) => aiApi.toolResult(session, id).catch(() => null),
  );
  const parsed = createMemo<unknown>(() => {
    const text = full();
    if (!text) return undefined;
    try {
      return JSON.parse(text) as unknown;
    } catch {
      return undefined;
    }
  });
  const recovered = createMemo(() => record(parsed()));
  const recoveredRows = createMemo(() => rowsOf(parsed()));
  return (
    <Show when={parsed() !== undefined} fallback={
      <>
        <Stats items={[
          { label: i18n.t("ai.tool.status"), value: props.value.tool_failed ? i18n.t("ai.activity.failed") : i18n.t("ai.tool.status") },
          { label: label("characters"), value: String(number(props.value.original_characters) ?? "—") },
          { label: label("tracks"), value: String(number(props.value.track_count) ?? "—") },
          { label: label("revision"), value: String(number(props.value.revision) ?? "—") },
          { label: label("dirty"), value: yesNo(!!props.value.dirty) },
        ]} />
        <div class="ai-tool-warning">{i18n.t("ai.tool.truncated")}</div>
        <Show when={string(props.value.preview)}>
          <details class="ai-tool-nested" open>
            <summary>{label("preview")}</summary>
            <AiToolContent content={string(props.value.preview)} />
          </details>
        </Show>
      </>
    }>
      <Show when={recovered()} fallback={
        <Show when={recoveredRows()} fallback={<PrimitiveResult value={parsed()} />}>
          <ArrayTool tool={props.tool} rows={recoveredRows()!} />
        </Show>
      }>
        <GenericTool name={props.tool} value={recovered()!} args={props.args} />
      </Show>
    </Show>
  );
}

function PrimitiveResult(props: { value: unknown }) {
  return (
    <div class="ai-tool-status" classList={{ ok: props.value === true || (typeof props.value === "string" && props.value.length > 0), error: props.value === false }}>
      <span class="ai-tool-status-dot" />
      <span>{cell(props.value)}</span>
    </div>
  );
}

function GenericTool(props: { name: string; value: RecordValue; args?: string }) {
  switch (props.name) {
    case "project_get": return <ProjectOverview value={props.value} />;
    case "sections_list":
    case "sections_add":
    case "sections_set":
    case "sections_remove": return <SectionsView value={props.value} />;
    case "tracks_list": {
      const tracks = Array.isArray(props.value.tracks) ? props.value.tracks.filter((item): item is RecordValue => record(item) !== null) : [];
      return <><Stats items={[{ label: label("tracks"), value: String(number(props.value.count) ?? tracks.length) }]} /><TrackTable tracks={tracks} /></>;
    }
    case "project_dsl":
    case "project_extract":
    case "project_context":
    case "project_generate_context":
    case "read_skill": return <CodeView value={props.value} />;
    case "project_edit":
      if (props.value.valid === false || (Array.isArray(props.value.errors) && props.value.errors.length > 0)) {
        return <ValidationView value={props.value} />;
      }
      return <DiffView value={props.value} />;
    case "project_diff": return <DiffView value={props.value} />;
    case "project_validate": return <ValidationView value={props.value} />;
    case "import_aligned_stems": return (
      <>
        <Stats items={[
          { label: label("stems"), value: cell(props.value.stem_count) },
          { label: i18n.t("ai.result.handoffMidiTracks"), value: cell(props.value.midi_track_count) },
        ]} />
        <Show when={number(props.value.timestamped_stem_count) !== null}>
          <div class="ai-tool-warning">
            {number(props.value.timestamped_stem_count) === number(props.value.stem_count)
              ? i18n.t("handoff.originMatched")
              : i18n.t("handoff.originUnverified")}
          </div>
        </Show>
        <Warnings value={record(props.value.midi)?.warnings} />
        <Show when={string(props.value.project_file_error)}>
          <div class="ai-tool-warning">{string(props.value.project_file_error)}</div>
        </Show>
      </>
    );
    case "audition_range": return <AudioResult value={props.value} />;
    case "render_range":
    case "render_offline": return <AudioResult value={props.value} />;
    case "render_stems": return <StemsView value={props.value} />;
    case "audio_analyze": return <Show when={record(props.value.transients)} fallback={
      <Show when={record(props.value.beat_grid)} fallback={
        <Show when={record(props.value.level_window)} fallback={<AnalysisView value={props.value} />}>
          {(window) => <LevelWindowView value={props.value} window={window()} />}
        </Show>
      }>
        {(grid) => <BeatEvidenceView path={string(props.value.path)} grid={grid()} />}
      </Show>
    }>
      {(evidence) => <TransientEvidenceView path={string(props.value.path)} evidence={evidence()} />}
    </Show>;
    case "audio_compare": return <CompareView value={props.value} />;
    case "analyze_notes": return <NotesView value={props.value} />;
    case "separate_stems": return <AutoView value={props.value} />;
    case "plugin_search":
    case "plugin_pull":
    case "plugins_scan":
    case "plugins_list": return <PluginTable value={props.value} />;
    case "plugins_params": return <ParamTable value={props.value} />;
    case "plugins_programs": return <AutoView value={props.value} />;
    case "presets_list": return <PresetsView value={props.value} />;
    case "presets_rename":
    case "presets_duplicate": return <AutoView value={props.value} />;
    case "sample_search":
    case "sample_pull": return <AutoView value={props.value} />;
    case "device_presets_list": return <DevicePresetTable value={props.value} />;
    case "devices_catalog": return <DeviceCatalogView value={props.value} args={props.args} />;
    case "sessions_list": return <AutoView value={props.value} />;
    case "undo_history": return <UndoView value={props.value} />;
    case "undo_state": return <AutoView value={props.value} />;
    case "transport_state": return <TransportView value={props.value} />;
    case "system_capabilities": return <AutoView value={props.value} />;
    case "audio_devices": return <AutoView value={props.value} />;
    case "audio_config": return <AutoView value={props.value} />;
    case "render_compat_report": return <AutoView value={props.value} />;
    case "midi_import_preview": return <AutoView value={props.value} />;
    case "midi_import_apply": return <AutoView value={props.value} />;
    case "midi_export": return <AutoView value={props.value} />;
    case "list_skills":
    case "buses_routes":
    case "sampler_assets": return <AutoView value={props.value} />;
    default: return isSnapshot(props.value) ? <SnapshotReceipt value={props.value} /> : <AutoView value={props.value} />;
  }
}

export function AiToolArguments(props: { content: string }) {
  const [raw, setRaw] = createSignal(false);
  const object = createMemo(() => {
    try {
      return record(JSON.parse(props.content));
    } catch {
      return null;
    }
  });
  return (
    <Show when={object()} fallback={<AiToolContent content={props.content} />}>
      <Show when={!raw()} fallback={<AiToolContent content={props.content} />}>
        <div class="ai-tool-args">
          <For each={Object.entries(object()!).filter(([key]) => !isCodeKey(key))}>{(entry) => (
            <div class="ai-tool-arg">
              <span class="ai-tool-arg-key">{label(entry[0])}</span>
              <span class="ai-tool-arg-value">{cell(entry[1])}</span>
            </div>
          )}</For>
        </div>
        <For each={Object.entries(object()!).filter(([key, value]) => isCodeKey(key) && typeof value === "string" && value.trim())}>{([key, value]) => (
          <CodeBlock language={/dsl/i.test(key) ? "DSL" : "Text"} text={String(value)} />
        )}</For>
      </Show>
      <button type="button" class="ai-tool-raw-toggle" onClick={() => setRaw(!raw())}>
        {i18n.t(raw() ? "ai.tool.details" : "ai.tool.raw")}
      </button>
    </Show>
  );
}

function StemsView(props: { value: RecordValue }) {
  const files = createMemo(() => Array.isArray(props.value.files) ? props.value.files.map(String) : []);
  return (
    <>
      <Stats items={[
        { label: label("directory"), value: string(props.value.directory) || "—" },
        { label: label("stems"), value: String(number(props.value.stems) ?? "—") },
        { label: label("frames"), value: String(number(props.value.frames) ?? "—") },
        { label: label("manifest"), value: string(props.value.manifest) || "—" },
      ]} />
      <Show when={files().length}>
        <Section title={label("files")} count={files().length}><Chips values={files()} /></Section>
      </Show>
      <Show when={record(props.value.loudness)}>
        <Section title={label("loudness")}><AutoView value={props.value.loudness} /></Section>
      </Show>
    </>
  );
}

function ArrayTool(props: { tool: string; rows: RecordValue[] }) {
  switch (props.tool) {
    case "plugins_params": return <ParamTable value={{ params: props.rows }} />;
    case "undo_history": return <UndoView value={props.rows} />;
    default: return <DataTable rows={props.rows} />;
  }
}

export function AiToolResult(props: { tool: string; content: string; args?: string; sessionId?: string }) {
  const [raw, setRaw] = createSignal(false);
  const value = createMemo(() => {
    try {
      return JSON.parse(props.content) as unknown;
    } catch {
      return undefined;
    }
  });
  const object = createMemo(() => record(value()));
  const rows = createMemo(() => rowsOf(value()));
  const rich = createMemo(() => object() !== null || rows() !== null || value() !== undefined);
  return (
    <div class="ai-tool-rich">
      <Show when={rich()} fallback={<AiToolContent content={props.content} />}>
        <Show when={!raw()} fallback={<AiToolContent content={props.content} />}>
          <Show when={object()} fallback={
            <Show when={rows()} fallback={<PrimitiveResult value={value()} />}>
              <ArrayTool tool={props.tool} rows={rows()!} />
            </Show>
          }>
            <Show when={object()!.result_truncated === true} fallback={
              <GenericTool name={props.tool} value={object()!} args={props.args} />
            }>
              <TruncatedResult tool={props.tool} value={object()!} args={props.args} sessionId={props.sessionId} />
            </Show>
          </Show>
        </Show>
        <button type="button" class="ai-tool-raw-toggle" onClick={() => setRaw(!raw())}>
          {i18n.t(raw() ? "ai.tool.details" : "ai.tool.raw")}
        </button>
      </Show>
    </div>
  );
}
