/**
 * MIDI import dialog — mapping table, options, summary, progress and cancel.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import {
  DEFAULT_IMPORT_OPTIONS,
  isMidiImportCancelled,
  midiImportErrorMessage,
  previewMidiImport,
  startMidiImport,
  type MidiImportJob,
  type MidiImportOptions,
  type MidiImportProgress,
  type MidiImportResult,
  type MidiImportSummary,
  type MidiTrackMapping,
  type MidiTrackSummary,
} from "../stores/midi";

export interface MidiImportDialogProps {
  path: string;
  preferredTrack?: number | null;
  onClose: () => void;
  onImported: (result: MidiImportResult, firstNewTrack: number | null) => void;
}

interface MappingRow {
  key: string;
  sourceTrack: number;
  channel: number | null;
  source: MidiTrackSummary;
  target: MidiImportTarget;
}

type MidiImportTarget =
  | { kind: "new"; name: string }
  | { kind: "existing"; index: number };

const CHANNEL_DRUMS = 10;

function rowKey(track: number, channel: number | null) {
  return `${track}:${channel ?? "all"}`;
}

function uniqueName(base: string, taken: Set<string>) {
  const trimmed = base.trim() || "Track";
  if (!taken.has(trimmed.toLowerCase())) return trimmed;
  let suffix = 2;
  while (taken.has(`${trimmed} (${suffix})`.toLowerCase())) suffix += 1;
  return `${trimmed} (${suffix})`;
}

function defaultTargetName(track: MidiTrackSummary, taken: Set<string>) {
  const base = track.name.trim() || (track.channel === CHANNEL_DRUMS ? "Drums" : `Track ${track.source_track + 1}`);
  return uniqueName(base, taken);
}

function formatTickError(value: number) {
  return Number.isInteger(value) ? String(value) : value.toFixed(2);
}

export function MidiImportDialog(props: MidiImportDialogProps) {
  const [options, setOptions] = createSignal<MidiImportOptions>({ ...DEFAULT_IMPORT_OPTIONS });
  const [preview, setPreview] = createSignal<MidiImportSummary | null>(null);
  const [rows, setRows] = createSignal<MappingRow[]>([]);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [loading, setLoading] = createSignal(true);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [applying, setApplying] = createSignal(false);
  const [cancelling, setCancelling] = createSignal(false);
  const [cancelled, setCancelled] = createSignal(false);
  const [progress, setProgress] = createSignal<MidiImportProgress | null>(null);
  const [applyError, setApplyError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<{ summary: MidiImportResult["summary"]; created: string[] } | null>(null);

  let job: MidiImportJob | null = null;
  let loadSeq = 0;
  let selectionInitialized = false;
  let preferredApplied = false;

  const buildRows = (next: MidiImportSummary, previous: MappingRow[]) => {
    const previousByKey = new Map(previous.map((row) => [row.key, row]));
    const taken = new Set(store.project.tracks.map((track) => track.name.trim().toLowerCase()));
    const draft: MappingRow[] = [];
    for (const track of next.tracks) {
      if (track.dropped || (options().drop_empty && track.note_count === 0)) continue;
      const key = rowKey(track.source_track, track.channel);
      const prev = previousByKey.get(key);
      const preferred = !preferredApplied && props.preferredTrack !== null && props.preferredTrack !== undefined
        && props.preferredTrack >= 0 && props.preferredTrack < store.project.tracks.length;
      const target: MidiImportTarget = prev?.target
        ?? (preferred
          ? { kind: "existing", index: props.preferredTrack as number }
          : { kind: "new", name: defaultTargetName(track, taken) });
      if (preferred) preferredApplied = true;
      if (target.kind === "new") taken.add(target.name.trim().toLowerCase());
      draft.push({ key, sourceTrack: track.source_track, channel: track.channel, source: track, target });
    }
    // Re-resolve new-name collisions between rows that survived a summary change.
    const seen = new Set<string>();
    for (const row of draft) {
      if (row.target.kind !== "new") continue;
      let name = row.target.name;
      while (seen.has(name.trim().toLowerCase())) name = uniqueName(name, seen);
      seen.add(name.trim().toLowerCase());
      row.target = { kind: "new", name };
    }
    setRows(draft);
    setSelected((current) => {
      if (!selectionInitialized) {
        selectionInitialized = true;
        return new Set(draft.map((row) => row.key));
      }
      return new Set(draft.filter((row) => current.has(row.key)).map((row) => row.key));
    });
  };

  const load = async (opts: MidiImportOptions) => {
    const seq = ++loadSeq;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await previewMidiImport(props.path, opts);
      if (seq !== loadSeq) return;
      setPreview(next);
      buildRows(next, rows());
    } catch (error) {
      if (seq !== loadSeq) return;
      setPreview(null);
      setRows([]);
      setSelected(new Set<string>());
      selectionInitialized = false;
      setLoadError(String(error));
    } finally {
      if (seq === loadSeq) setLoading(false);
    }
  };

  createEffect(() => {
    const opts = options();
    void load(opts);
  });

  const assignedProjectName = () => new Set(store.project.tracks.map((track) => track.name.trim().toLowerCase()));
  const assignedRowNames = createMemo(() => {
    const names = new Map<string, number>();
    for (const row of rows()) {
      if (!selected().has(row.key) || row.target.kind !== "new") continue;
      const name = row.target.name.trim().toLowerCase();
      if (!name) continue;
      names.set(name, (names.get(name) ?? 0) + 1);
    }
    return names;
  });

  const collisions = createMemo(() => {
    const projectNames = assignedProjectName();
    const counts = assignedRowNames();
    const problems = new Set<string>();
    for (const row of rows()) {
      if (!selected().has(row.key)) continue;
      if (row.target.kind === "existing") continue;
      const name = row.target.name.trim().toLowerCase();
      if (!name) problems.add(row.key);
      else if (projectNames.has(name) || (counts.get(name) ?? 0) > 1) problems.add(row.key);
    }
    return problems;
  });

  const canConfirm = createMemo(() =>
    !loading() && !applying() && !result() && preview() !== null && selected().size > 0 && collisions().size === 0,
  );

  const ignoredTypes = createMemo(() => {
    const counts = new Map<string, number>();
    for (const track of preview()?.tracks ?? []) {
      for (const ignored of track.ignored) {
        counts.set(ignored.kind, (counts.get(ignored.kind) ?? 0) + ignored.count);
      }
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1]);
  });

  const selectedRows = createMemo(() => rows().filter((row) => selected().has(row.key)));

  const toggleOption = (key: keyof MidiImportOptions) => {
    setOptions((current) => ({ ...current, [key]: !current[key] }));
  };

  const toggleRow = (key: string, checked: boolean) => {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(key); else next.delete(key);
      return next;
    });
  };

  const setRowTarget = (key: string, target: MidiImportTarget) => {
    setRows((current) => current.map((row) => row.key === key ? { ...row, target } : row));
  };

  const selectAll = (checked: boolean) => {
    setSelected(checked ? new Set(rows().map((row) => row.key)) : new Set<string>());
  };

  const confirm = async () => {
    if (!canConfirm() || !preview()) return;
    const mappings: MidiTrackMapping[] = selectedRows().map((row) => ({
      source_track: row.sourceTrack,
      channel: row.channel,
      target: row.target.kind === "new" ? "NewTrack" : { ExistingTrack: row.target.index },
      name: row.target.kind === "new" ? row.target.name.trim() : null,
    }));
    const projectTrackCount = store.project.tracks.length;
    setApplying(true);
    setApplyError(null);
    setCancelled(false);
    setProgress(null);
    job = startMidiImport(props.path, { ...options(), mappings }, setProgress);
    try {
      const next = await job.result;
      const created = next.summary.tracks
        .filter((track) => track.target_track !== null)
        .map((track) => track.name);
      const newIndices = next.summary.tracks
        .map((track) => track.target_track)
        .filter((index): index is number => index !== null && index >= projectTrackCount);
      setResult({ summary: next.summary, created });
      props.onImported(next, newIndices.length > 0 ? Math.min(...newIndices) : null);
    } catch (error) {
      if (cancelled() || isMidiImportCancelled(error)) setCancelled(true);
      else setApplyError(midiImportErrorMessage(error));
    } finally {
      setApplying(false);
      setCancelling(false);
      job = null;
    }
  };

  const cancel = async () => {
    if (!job) return;
    setCancelling(true);
    setCancelled(true);
    try { await job.cancel(); } catch (error) { setApplyError(String(error)); }
  };

  const close = () => {
    if (applying()) return;
    props.onClose();
  };

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !applying()) props.onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  const progressPct = () => {
    const value = progress();
    if (!value) return null;
    return Math.max(0, Math.min(100, Math.round(value.fraction * 100)));
  };

  const stageLabel = () => {
    const stage = progress()?.stage;
    if (!stage) return i18n.t("midi.import.applying");
    return i18n.t(`midi.import.stage.${stage}` as Parameters<typeof i18n.t>[0]) || stage;
  };

  return (
    <div class="midi-overlay" onClick={close}>
      <div
        class="midi-dialog midi-import-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("midi.import.title")}
        onClick={(event) => event.stopPropagation()}
      >
        <header class="midi-dialog-header">
          <div>
            <div class="midi-dialog-title">{i18n.t("midi.import.title")}</div>
            <div class="midi-dialog-subtitle" {...useTooltip(() => props.path)}>{props.path.split(/[\\/]/).pop()}</div>
          </div>
          <button class="midi-icon-btn" onClick={close} disabled={applying()} {...useTooltip(() => i18n.t("midi.close"))}>×</button>
        </header>

        <Show when={!result()}>
        <div class="midi-options" role="group" aria-label={i18n.t("midi.import.options")}>
          <label class="midi-check">
            <input
              type="checkbox"
              checked={options().split_channels}
              onChange={() => toggleOption("split_channels")}
            />
            <span>{i18n.t("midi.import.splitByChannel")}</span>
          </label>
          <label class="midi-check">
            <input
              type="checkbox"
              checked={options().drop_empty}
              onChange={() => toggleOption("drop_empty")}
            />
            <span>{i18n.t("midi.import.dropEmpty")}</span>
          </label>
          <label class="midi-check">
            <input
              type="checkbox"
              checked={options().expand_sustain}
              onChange={() => toggleOption("expand_sustain")}
            />
            <span>{i18n.t("midi.import.expandPedal")}</span>
          </label>
          <label class="midi-check">
            <input
              type="checkbox"
              checked={options().import_clock}
              onChange={() => toggleOption("import_clock")}
            />
            <span>{i18n.t("midi.import.applyTempoMap")}</span>
          </label>
        </div>
        </Show>

        <Show when={!result()}>
          <div class="midi-dialog-body midi-import-body">
            <section class="midi-mapping-panel">
              <div class="midi-panel-head">
                <span>{i18n.t("midi.import.mapping")}</span>
                <div class="midi-select-actions">
                  <button class="midi-link-btn" onClick={() => selectAll(true)}>{i18n.t("midi.import.selectAll")}</button>
                  <span class="midi-sep">·</span>
                  <button class="midi-link-btn" onClick={() => selectAll(false)}>{i18n.t("midi.import.selectNone")}</button>
                </div>
              </div>
              <div class="midi-table-wrap">
                <table class="midi-table">
                  <thead>
                    <tr>
                      <th class="midi-col-check">
                        <input
                          type="checkbox"
                          aria-label={i18n.t("midi.import.selectAll")}
                          checked={rows().length > 0 && selected().size === rows().length}
                          onChange={(event) => selectAll(event.currentTarget.checked)}
                        />
                      </th>
                      <th>{i18n.t("midi.import.sourceTrack")}</th>
                      <th>{i18n.t("midi.import.channel")}</th>
                      <th>{i18n.t("midi.import.program")}</th>
                      <th class="midi-col-num">{i18n.t("midi.import.notes")}</th>
                      <th>{i18n.t("midi.import.lanes")}</th>
                      <th>{i18n.t("midi.import.target")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    <Show when={!loading() && rows().length === 0}>
                      <tr><td colSpan={7} class="midi-empty">{i18n.t("midi.import.noTracks")}</td></tr>
                    </Show>
                    <For each={rows()}>
                      {(row) => {
                        const checked = () => selected().has(row.key);
                        const collision = () => collisions().has(row.key);
                        return (
                          <tr classList={{ "midi-row-selected": checked(), "midi-row-invalid": collision() }}>
                            <td class="midi-col-check">
                              <input
                                type="checkbox"
                                checked={checked()}
                                aria-label={`${i18n.t("midi.import.selectRow")} ${row.source.name}`}
                                onChange={(event) => toggleRow(row.key, event.currentTarget.checked)}
                              />
                            </td>
                            <td class="midi-source-cell" {...useTooltip(() => `${row.source.name} · ${i18n.t("midi.import.bars")} ${row.source.first_bar ?? "—"}–${row.source.last_bar ?? "—"}`)}>
                              <span class="midi-source-index">#{row.sourceTrack + 1}</span>
                              <span class="midi-source-name">{row.source.name || i18n.t("midi.import.untitled")}</span>
                            </td>
                            <td>
                              <span classList={{ "midi-badge-drum": row.channel === CHANNEL_DRUMS }} class="midi-badge">
                                {row.channel === null
                                  ? i18n.t("midi.import.allChannels")
                                  : row.channel === CHANNEL_DRUMS ? i18n.t("midi.import.drums") : row.channel}
                              </span>
                            </td>
                            <td class="midi-program">{row.source.program_name ?? (row.source.program === null ? "—" : `GM ${row.source.program + 1}`)}</td>
                            <td class="midi-col-num">{row.source.note_count}</td>
                            <td class="midi-lanes" {...useTooltip(() => row.source.lanes.join(", "))}>
                              {row.source.lanes.length ? row.source.lanes.join(", ") : "—"}
                            </td>
                            <td class="midi-target-cell">
                              <select
                                class="midi-target-select"
                                aria-label={`${i18n.t("midi.import.target")} ${row.source.name}`}
                                value={row.target.kind === "existing" ? `existing:${row.target.index}` : "new"}
                                onChange={(event) => {
                                  const value = event.currentTarget.value;
                                  if (value === "new") {
                                    setRowTarget(row.key, { kind: "new", name: defaultTargetName(row.source, new Set()) });
                                  } else {
                                    setRowTarget(row.key, { kind: "existing", index: Number(value.split(":")[1]) });
                                  }
                                }}
                              >
                                <option value="new">{i18n.t("midi.import.newTrack")}</option>
                                <For each={store.project.tracks}>
                                  {(track, index) => (
                                    <option value={`existing:${index()}`}>{i18n.t("midi.import.mapTo", { track: track.name })}</option>
                                  )}
                                </For>
                              </select>
                              <Show when={row.target.kind === "new"}>
                                <input
                                  class="midi-name-input"
                                  classList={{ "midi-input-invalid": collision() }}
                                  aria-label={i18n.t("midi.import.trackName")}
                                  value={row.target.kind === "new" ? row.target.name : ""}
                                  onInput={(event) => setRowTarget(row.key, { kind: "new", name: event.currentTarget.value })}
                                />
                              </Show>
                            </td>
                          </tr>
                        );
                      }}
                    </For>
                  </tbody>
                </table>
                <Show when={loading()}>
                  <div class="midi-table-loading">{i18n.t("midi.import.loading")}</div>
                </Show>
              </div>
            </section>

            <aside class="midi-summary-panel">
              <div class="midi-panel-head">{i18n.t("midi.import.summary")}</div>
              <Show when={preview()}>
                {(value) => (
                  <div class="midi-summary-content">
                    <div class="midi-summary-line">
                      <span>{i18n.t("midi.import.sourceFormat")}</span>
                      <strong>{value().format} · {value().source_ppq} → {value().target_ppq} PPQ</strong>
                    </div>
                    <div class="midi-summary-line">
                      <span>{i18n.t("midi.import.tempoChanges")}</span>
                      <strong>{value().tempo_changes}</strong>
                    </div>
                    <div class="midi-summary-line">
                      <span>{i18n.t("midi.import.meterChanges")}</span>
                      <strong>{value().meter_changes}</strong>
                    </div>
                    <div class="midi-summary-line">
                      <span>{i18n.t("midi.import.maxTickError")}</span>
                      <strong classList={{ "midi-warn-text": value().max_tick_error > 0 }}>
                        {formatTickError(value().max_tick_error)} {i18n.t("midi.import.ticks")}
                      </strong>
                    </div>
                    <Show when={value().ticks_shifted > 0}>
                      <div class="midi-summary-line">
                        <span>{i18n.t("midi.import.ticksShifted")}</span>
                        <strong>{value().ticks_shifted}</strong>
                      </div>
                    </Show>
                    <Show when={value().sustain_expanded_notes > 0}>
                      <div class="midi-summary-line">
                        <span>{i18n.t("midi.import.sustainExpanded")}</span>
                        <strong>{value().sustain_expanded_notes}</strong>
                      </div>
                    </Show>
                    <Show when={value().overlapping_notes > 0}>
                      <div class="midi-summary-line">
                        <span>{i18n.t("midi.import.overlaps")}</span>
                        <strong>{value().overlapping_notes}</strong>
                      </div>
                    </Show>
                    <Show when={value().zero_length_notes > 0}>
                      <div class="midi-summary-line">
                        <span>{i18n.t("midi.import.zeroLength")}</span>
                        <strong>{value().zero_length_notes}</strong>
                      </div>
                    </Show>
                    <Show when={value().unterminated_notes > 0}>
                      <div class="midi-summary-line">
                        <span>{i18n.t("midi.import.unterminated")}</span>
                        <strong>{value().unterminated_notes}</strong>
                      </div>
                    </Show>
                    <div class="midi-summary-divider" />
                    <div class="midi-summary-label">{i18n.t("midi.import.ignored")}</div>
                    <Show
                      when={ignoredTypes().length > 0}
                      fallback={<div class="midi-muted">{i18n.t("midi.import.noIgnored")}</div>}
                    >
                      <div class="midi-chips">
                        <For each={ignoredTypes()}>
                          {([kind, count]) => <span class="midi-chip midi-chip-warn" {...useTooltip(() => kind)}>{kind} ×{count}</span>}
                        </For>
                      </div>
                    </Show>
                    <div class="midi-summary-divider" />
                    <div class="midi-summary-line">
                      <span>{i18n.t("midi.import.selectedCount")}</span>
                      <strong>{selected().size} / {rows().length}</strong>
                    </div>
                  </div>
                )}
              </Show>
            </aside>
          </div>
        </Show>

        <Show when={result()}>
          {(value) => (
            <div class="midi-dialog-body midi-result-body selectable">
              <div class="midi-result-title">{i18n.t("midi.import.created", { count: value().created.length })}</div>
              <ul class="midi-result-list">
                <For each={value().created}>{(name) => <li>{name}</li>}</For>
              </ul>
              <div class="midi-summary-content">
                <div class="midi-summary-line">
                  <span>{i18n.t("midi.import.tempoChanges")}</span>
                  <strong>{value().summary.tempo_changes}</strong>
                </div>
                <div class="midi-summary-line">
                  <span>{i18n.t("midi.import.meterChanges")}</span>
                  <strong>{value().summary.meter_changes}</strong>
                </div>
                <div class="midi-summary-line">
                  <span>{i18n.t("midi.import.maxTickError")}</span>
                  <strong>{formatTickError(value().summary.max_tick_error)} {i18n.t("midi.import.ticks")}</strong>
                </div>
                <Show when={ignoredTypes().length > 0}>
                  <div class="midi-summary-label">{i18n.t("midi.import.ignored")}</div>
                  <div class="midi-chips">
                    <For each={ignoredTypes()}>
                      {([kind, count]) => <span class="midi-chip midi-chip-warn">{kind} ×{count}</span>}
                    </For>
                  </div>
                </Show>
              </div>
            </div>
          )}
        </Show>

        <footer class="midi-dialog-footer">
          <div class="midi-footer-status">
            <Show when={loadError()}>
              <span class="midi-error-text selectable">{i18n.t("midi.import.previewFailed", { error: loadError()! })}</span>
            </Show>
            <Show when={collisions().size > 0}>
              <span class="midi-error-text selectable">{i18n.t("midi.import.collision", { count: collisions().size })}</span>
            </Show>
            <Show when={cancelled() && !applying()}>
              <span class="midi-muted">{i18n.t("midi.import.cancelled")}</span>
            </Show>
            <Show when={applyError()}>
              <span class="midi-error-text selectable">{applyError()}</span>
            </Show>
            <Show when={applying()}>
              <div class="midi-progress">
                <div class="midi-progress-track">
                  <div class="midi-progress-fill" style={{ width: progressPct() === null ? "35%" : `${progressPct()}%` }} />
                </div>
                <span class="midi-progress-label">
                  {stageLabel()} {progressPct() === null ? "" : `· ${progressPct()}%`}
                </span>
              </div>
            </Show>
          </div>
          <div class="midi-footer-actions">
            <Show when={!result()}>
              <button class="midi-btn" onClick={close} disabled={applying()}>{i18n.t("midi.cancel")}</button>
              <Show when={applying()}>
                <button class="midi-btn midi-btn-danger" onClick={cancel} disabled={cancelling()}>
                  {cancelling() ? i18n.t("midi.import.cancelling") : i18n.t("midi.import.cancelImport")}
                </button>
              </Show>
              <Show when={!applying()}>
                <button
                  class="midi-btn midi-btn-primary midi-import-confirm"
                  onClick={confirm}
                  disabled={!canConfirm()}
                >
                  {i18n.t("midi.import.confirm", { count: selected().size })}
                </button>
              </Show>
            </Show>
            <Show when={result()}>
              <button class="midi-btn midi-btn-primary" onClick={close}>{i18n.t("midi.done")}</button>
            </Show>
          </div>
        </footer>
      </div>
    </div>
  );
}
