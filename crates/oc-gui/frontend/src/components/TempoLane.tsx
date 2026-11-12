/**
 * Tempo and meter lane above the timeline lanes.
 *
 * Points are drawn at the bar/beat positions the engine uses. Pointer drags and
 * key moves produce a new map through `utils/tempoLane`; the project store
 * persists that map as one undoable command.
 */
import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import type { JSX } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { ask } from "@tauri-apps/plugin-dialog";
import { blockStepQuarters, gridDivision, snapBlockQuarter } from "../stores/grid";
import { openContextMenu } from "./ContextMenu";
import { useTooltip } from "./Tooltip";
import { withUndoGesture } from "../stores/gesture";
import {
  METER_DENOMINATORS,
  TEMPO_LANE_ROW_HEIGHT,
  TEMPO_MAX_BPM,
  TEMPO_MIN_BPM,
  formatMeter,
  formatTempo,
  insertTempoPoint,
  insertTimeSigPoint,
  meterAtBar,
  meterAtQuarter,
  moveTempoPoint,
  moveTimeSigPoint,
  removeTempoPoint,
  removeTimeSigPoint,
  setTempoPointBpm,
  setTimeSigPointValue,
  tempoAtQuarter,
  tempoPointQuarter,
  type LaneEdit,
  type LaneError,
} from "../utils/tempoLane";
import { projectClock } from "../utils/projectClock";
import { beginPointerDrag } from "../utils/pointerDrag";
import type { TempoPoint, TimeSigPoint } from "../types/project";

interface KindSelection {
  kind: "tempo" | "meter";
  index: number;
}

interface PointEditor {
  kind: "tempo" | "meter";
  index: number;
  bpm: number;
  numerator: number;
  denominator: number;
}

interface DragSession {
  pointerId: number;
  kind: "tempo" | "meter";
  index: number;
  rowLeft: number;
  startClientX: number;
  /** Raw quarter under the last move; the release commits this value. */
  quarter: number;
  moved: boolean;
}

interface DragPreview {
  kind: "tempo" | "meter";
  index: number;
  x: number;
}

interface TempoLaneProps {
  width: number;
  pxPerBeat: number;
}

const clock = () => projectClock(store.project.properties);
const tempoPoints = (): TempoPoint[] => store.project.properties.tempo_map ?? [];
const meterPoints = (): TimeSigPoint[] => store.project.properties.time_sig_map ?? [];

function laneErrorText(error: LaneError): string {
  switch (error) {
    case "invalidTempo":
      return i18n.t("tempoLane.invalidTempo", { min: TEMPO_MIN_BPM, max: TEMPO_MAX_BPM });
    case "invalidMeter":
      return i18n.t("tempoLane.invalidMeter");
    case "duplicatePosition":
      return i18n.t("tempoLane.duplicate");
    case "outOfRange":
      return i18n.t("tempoLane.outOfRange");
    case "invalidPosition":
      return i18n.t("tempoLane.invalidPosition");
  }
}

function formatPointPosition(point: TempoPoint): string {
  const { bar, beat, subdivision } = point.position;
  return `${bar}:${beat}${subdivision ? `.${subdivision}` : ""}`;
}

function tempoPointLabel(point: TempoPoint): string {
  return i18n.t("tempoLane.tempoPoint", {
    bpm: formatTempo(point.bpm),
    position: formatPointPosition(point),
  });
}

function meterPointLabel(point: TimeSigPoint): string {
  return i18n.t("tempoLane.meterPoint", {
    meter: formatMeter(point.numerator, point.denominator),
    bar: point.position.bar,
  });
}

/** Readout in the sticky header: the maps' values under the playhead. */
export function TempoLaneHeader(): JSX.Element {
  const playheadQuarter = () => {
    const scrub = store.scrubPosition();
    return Math.max(0, scrub ?? store.transport().position_beats);
  };
  const tempo = () => tempoAtQuarter(store.project.properties, playheadQuarter());
  const meter = () => meterAtQuarter(store.project.properties, playheadQuarter());
  return (
    <div class="tempo-lane-headers" style={{ height: `${TEMPO_LANE_ROW_HEIGHT * 2 + 1}px` }}>
      <div class="tempo-lane-head tempo-lane-head-tempo">
        <span class="tempo-lane-head-label">{i18n.t("tempoLane.tempo")}</span>
        <span class="tempo-lane-head-value">{formatTempo(tempo())}</span>
        <span class="tempo-lane-head-unit">BPM</span>
      </div>
      <div class="tempo-lane-head tempo-lane-head-meter">
        <span class="tempo-lane-head-label">{i18n.t("tempoLane.meter")}</span>
        <span class="tempo-lane-head-value">{formatMeter(meter().numerator, meter().denominator)}</span>
      </div>
    </div>
  );
}

export function TempoLane(props: TempoLaneProps): JSX.Element {
  const [selected, setSelected] = createSignal<KindSelection | null>(null);
  const [editing, setEditing] = createSignal<PointEditor | null>(null);
  const [editorError, setEditorError] = createSignal<LaneError | null>(null);
  const [dragPreview, setDragPreview] = createSignal<DragPreview | null>(null);
  let drag: DragSession | null = null;
  let tempoRow: HTMLDivElement | undefined;
  let meterRow: HTMLDivElement | undefined;

  const select = (kind: "tempo" | "meter", index: number) => setSelected({ kind, index });
  const isSelected = (kind: "tempo" | "meter", index: number) => {
    const current = selected();
    return current?.kind === kind && current.index === index;
  };

  const pointX = (kind: "tempo" | "meter", index: number): number => {
    if (kind === "tempo") {
      const point = tempoPoints()[index];
      return point ? tempoPointQuarter(clock(), point) * props.pxPerBeat : 0;
    }
    const point = meterPoints()[index];
    return point ? clock().barStartQuarter(point.position.bar) * props.pxPerBeat : 0;
  };

  const pointLeft = (kind: "tempo" | "meter", index: number): number => {
    const preview = dragPreview();
    return preview?.kind === kind && preview.index === index ? preview.x : pointX(kind, index);
  };

  createEffect(() => {
    const current = selected();
    if (!current) return;
    const count = current.kind === "tempo" ? tempoPoints().length : meterPoints().length;
    if (count === 0) setSelected(null);
    else if (current.index >= count) setSelected({ kind: current.kind, index: count - 1 });
  });

  createEffect(() => {
    const current = editing();
    if (!current) return;
    const count = current.kind === "tempo" ? tempoPoints().length : meterPoints().length;
    if (count <= current.index) {
      setEditing(null);
      setEditorError(null);
    }
  });

  const quarterAtClientX = (kind: "tempo" | "meter", clientX: number): number => {
    const row = kind === "tempo" ? tempoRow : meterRow;
    if (!row) return 0;
    const rect = row.getBoundingClientRect();
    return Math.max(0, (clientX - rect.left) / props.pxPerBeat);
  };

  const reportError = (error: LaneError) => {
    setEditorError(error);
    if (!editing()) store.setError(laneErrorText(error));
  };

  async function persistTempo(edit: LaneEdit<TempoPoint>): Promise<boolean> {
    if (!edit.ok) {
      reportError(edit.error);
      return false;
    }
    setEditorError(null);
    const saved = await withUndoGesture("transport", () => store.setTempoMap(edit.points));
    if (saved) setSelected({ kind: "tempo", index: edit.index });
    return saved;
  }

  async function persistMeter(edit: LaneEdit<TimeSigPoint>): Promise<boolean> {
    if (!edit.ok) {
      reportError(edit.error);
      return false;
    }
    setEditorError(null);
    const saved = await withUndoGesture("transport", () => store.setTimeSigMap(edit.points));
    if (saved) setSelected({ kind: "meter", index: edit.index });
    return saved;
  }

  const openEditor = (kind: "tempo" | "meter", index: number) => {
    if (kind === "tempo") {
      const point = tempoPoints()[index];
      if (!point) return;
      setEditing({ kind, index, bpm: point.bpm, numerator: 4, denominator: 4 });
    } else {
      const point = meterPoints()[index];
      if (!point) return;
      setEditing({ kind, index, bpm: 120, numerator: point.numerator, denominator: point.denominator });
    }
    setSelected({ kind, index });
    setEditorError(null);
  };

  const updateEditor = (patch: Partial<PointEditor>) => {
    const current = editing();
    if (!current) return;
    setEditing({ ...current, ...patch });
    setEditorError(null);
  };

  async function commitEditor(): Promise<boolean> {
    const current = editing();
    if (!current) return false;
    const properties = store.project.properties;
    if (current.kind === "tempo") {
      const saved = await persistTempo(setTempoPointBpm(properties, current.index, current.bpm));
      if (saved) setEditing(null);
      return saved;
    }
    const saved = await persistMeter(setTimeSigPointValue(
      properties,
      current.index,
      Math.trunc(current.numerator),
      Math.trunc(current.denominator),
    ));
    if (saved) setEditing(null);
    return saved;
  }

  const cancelEditor = () => {
    setEditing(null);
    setEditorError(null);
  };

  async function deletePoint(kind: "tempo" | "meter", index: number) {
    const properties = store.project.properties;
    setEditing(null);
    setEditorError(null);
    if (kind === "tempo") await persistTempo(removeTempoPoint(properties, index));
    else await persistMeter(removeTimeSigPoint(properties, index));
  }

  async function clearPoints(kind: "tempo" | "meter") {
    setEditing(null);
    setEditorError(null);
    setSelected(null);
    if (kind === "tempo") await store.setTempoMap([]);
    else await store.setTimeSigMap([]);
  }

  async function addPointAt(kind: "tempo" | "meter", quarter: number) {
    const properties = store.project.properties;
    if (kind === "tempo") {
      const result = insertTempoPoint(properties, quarter, tempoAtQuarter(properties, quarter), gridDivision());
      if (!result.ok && result.error === "duplicatePosition") {
        const existing = tempoPoints().findIndex((point) =>
          Math.abs(tempoPointQuarter(clock(), point) - quarter) < 1e-6);
        if (existing >= 0) openEditor("tempo", existing);
        return;
      }
      await persistTempo(result);
      return;
    }
    const bar = clock().barBeatAt(Math.max(0, quarter)).bar;
    const meter = meterAtBar(properties, bar);
    const result = insertTimeSigPoint(properties, bar, meter.numerator, meter.denominator);
    if (!result.ok && result.error === "duplicatePosition") {
      const existing = meterPoints().findIndex((point) => point.position.bar === bar);
      if (existing >= 0) openEditor("meter", existing);
      return;
    }
    await persistMeter(result);
  }

  let stopDrag: (() => void) | null = null;
  onCleanup(() => stopDrag?.());

  const beginDrag = (event: PointerEvent, kind: "tempo" | "meter", index: number) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    select(kind, index);
    setEditing(null);
    setEditorError(null);
    (event.currentTarget as HTMLElement).focus?.({ preventScroll: true });
    const row = kind === "tempo" ? tempoRow : meterRow;
    const session: DragSession = {
      pointerId: event.pointerId,
      kind,
      index,
      rowLeft: row?.getBoundingClientRect().left ?? 0,
      startClientX: event.clientX,
      quarter: 0,
      moved: false,
    };
    // Take the pointer before publishing the session: the helper cancels any
    // drag still running, and that drag must see its own state, not this one.
    stopDrag = beginPointerDrag({
      element: event.currentTarget as HTMLElement,
      pointerId: event.pointerId,
      onMove: handleDragMove,
      onEnd: (committed) => {
        stopDrag = null;
        void handleDragEnd(committed);
      },
    });
    drag = session;
  };

  function handleDragMove(clientX: number) {
    const session = drag;
    if (!session) return;
    // A press that barely moves is a click on the point, not a move.
    if (!session.moved && Math.abs(clientX - session.startClientX) < 3) return;
    session.moved = true;
    const quarter = Math.max(0, (clientX - session.rowLeft) / props.pxPerBeat);
    session.quarter = quarter;
    if (session.kind === "tempo") {
      const snapped = snapBlockQuarter(quarter, gridDivision());
      setDragPreview({ ...session, x: snapped * props.pxPerBeat });
    } else {
      const bar = clock().barBeatAt(quarter).bar;
      setDragPreview({ ...session, x: clock().barStartQuarter(bar) * props.pxPerBeat });
    }
  }

  /** Ends the drag; only a release writes, so a cancelled one is dropped. */
  async function handleDragEnd(committed: boolean) {
    const session = drag;
    if (!session) return;
    drag = null;
    setDragPreview(null);
    if (!committed || !session.moved) return;
    // The release click must not clear the selection the drag just committed.
    const swallow = (click: MouseEvent) => {
      click.preventDefault();
      click.stopPropagation();
      document.removeEventListener("click", swallow, true);
    };
    document.addEventListener("click", swallow, true);
    setTimeout(() => document.removeEventListener("click", swallow, true), 0);
    const quarter = session.quarter;
    if (session.kind === "tempo") {
      await persistTempo(moveTempoPoint(store.project.properties, session.index, quarter, gridDivision()));
    } else {
      await persistMeter(moveTimeSigPoint(store.project.properties, session.index, quarter));
    }
  }

  async function moveSelected(delta: number) {
    const current = selected();
    if (!current) return;
    const properties = store.project.properties;
    if (current.kind === "tempo") {
      const point = tempoPoints()[current.index];
      if (!point) return;
      const quarter = Math.max(0, tempoPointQuarter(clock(), point) + delta);
      await persistTempo(moveTempoPoint(properties, current.index, quarter, gridDivision()));
      return;
    }
    const point = meterPoints()[current.index];
    if (!point) return;
    const bar = point.position.bar + Math.sign(delta);
    if (bar < 1) return;
    await persistMeter(moveTimeSigPoint(properties, current.index, clock().barStartQuarter(bar)));
  }

  async function changeSelectedValue(delta: number) {
    const current = selected();
    if (!current) return;
    const properties = store.project.properties;
    if (current.kind === "tempo") {
      const point = tempoPoints()[current.index];
      if (!point) return;
      await persistTempo(setTempoPointBpm(properties, current.index, point.bpm + delta));
      return;
    }
    const point = meterPoints()[current.index];
    if (!point) return;
    await persistMeter(setTimeSigPointValue(properties, current.index, point.numerator + delta, point.denominator));
  }

  const onKeyDown = (event: KeyboardEvent) => {
    if (editing()) return;
    const current = selected();
    if (!current) {
      if (event.key === "Escape") setSelected(null);
      return;
    }
    if (event.key === "Delete" || event.key === "Backspace") {
      event.preventDefault();
      event.stopPropagation();
      void deletePoint(current.kind, current.index);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      openEditor(current.kind, current.index);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setSelected(null);
      return;
    }
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      event.stopPropagation();
      const step = blockStepQuarters(gridDivision());
      void moveSelected(event.key === "ArrowLeft" ? -step : step);
      return;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      event.stopPropagation();
      const amount = (event.shiftKey ? 10 : 1) * (event.key === "ArrowUp" ? 1 : -1);
      void changeSelectedValue(amount);
    }
  };

  const openPointMenu = (event: MouseEvent, kind: "tempo" | "meter", index: number) => {
    const heading = kind === "tempo"
      ? tempoPointLabel(tempoPoints()[index])
      : meterPointLabel(meterPoints()[index]);
    select(kind, index);
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "tempo-context-menu",
      ariaLabel: i18n.t("tempoLane.menu"),
      items: [
        { type: "heading", label: heading },
        { label: i18n.t("tempoLane.edit"), onSelect: () => openEditor(kind, index) },
        { type: "separator" },
        {
          // Clearing the whole map is a bulk destructive edit: name it as one
          // and ask before it runs.
          label: kind === "tempo" ? i18n.t("tempoLane.clearTempo") : i18n.t("tempoLane.clearMeter"),
          danger: true,
          onSelect: () => void confirmClearPoints(kind),
        },
        {
          label: i18n.t("tempoLane.delete"),
          shortcut: "Delete",
          danger: true,
          onSelect: () => void deletePoint(kind, index),
        },
      ],
    });
  };

  const confirmClearPoints = async (kind: "tempo" | "meter") => {
    const points = kind === "tempo" ? tempoPoints() : meterPoints();
    if (points.length === 0) return;
    const confirmed = await ask(i18n.t("tempoLane.clearConfirm", { count: points.length }), {
      title: i18n.t("tempoLane.clearTitle"),
      kind: "warning",
    });
    if (confirmed) await clearPoints(kind);
  };

  const openRowMenu = (event: MouseEvent, kind: "tempo" | "meter") => {
    const quarter = quarterAtClientX(kind, event.clientX);
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "tempo-context-menu",
      ariaLabel: i18n.t("tempoLane.menu"),
      items: [
        {
          label: kind === "tempo" ? i18n.t("tempoLane.addTempo") : i18n.t("tempoLane.addMeter"),
          onSelect: () => void addPointAt(kind, quarter),
        },
        { type: "separator" },
        {
          label: kind === "tempo" ? i18n.t("tempoLane.clearTempo") : i18n.t("tempoLane.clearMeter"),
          danger: true,
          disabled: (kind === "tempo" ? tempoPoints().length : meterPoints().length) === 0,
          onSelect: () => void confirmClearPoints(kind),
        },
      ],
    });
  };

  return (
    <div
      id="timeline-tempo-lane"
      data-own-keys
      class="tempo-lane"
      style={{ width: `${props.width}px` }}
      role="group"
      aria-label={i18n.t("tempoLane.region")}
      onKeyDown={onKeyDown}
    >
      <div
        class="tempo-lane-row tempo-lane-tempo"
        ref={tempoRow}
        role="group"
        tabIndex={0}
        aria-label={i18n.t("tempoLane.tempoRow")}
        onClick={(event) => {
          if (!(event.target as HTMLElement).closest(".tempo-point")) setSelected(null);
        }}
        onDblClick={(event) => {
          if ((event.target as HTMLElement).closest(".tempo-point")) return;
          void addPointAt("tempo", quarterAtClientX("tempo", event.clientX));
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          if ((event.target as HTMLElement).closest(".tempo-point")) return;
          openRowMenu(event, "tempo");
        }}
      >
        <For each={tempoPoints()}>
          {(point, index) => (
            <button
              type="button"
              class="tempo-point tempo-point-bpm"
              classList={{ selected: isSelected("tempo", index()) }}
              style={{ left: `${pointLeft("tempo", index())}px` }}
              aria-label={tempoPointLabel(point)}
              aria-pressed={isSelected("tempo", index())}
              onPointerDown={(event) => beginDrag(event, "tempo", index())}
              onDblClick={(event) => {
                event.stopPropagation();
                openEditor("tempo", index());
              }}
              onFocus={() => select("tempo", index())}
              onContextMenu={(event) => {
                event.preventDefault();
                event.stopPropagation();
                openPointMenu(event, "tempo", index());
              }}
              {...useTooltip(() => tempoPointLabel(point))}
            >
              {formatTempo(point.bpm)}
            </button>
          )}
        </For>
        <Show when={editing()?.kind === "tempo"}>
          <input
            class="tempo-point-input tempo-bpm-input"
            type="number"
            min={TEMPO_MIN_BPM}
            max={TEMPO_MAX_BPM}
            step="1"
            style={{ left: `${pointX("tempo", editing()!.index)}px` }}
            value={editing()!.bpm}
            aria-label={i18n.t("tempoLane.editTempo")}
            aria-invalid={editorError() !== null}
            ref={(element) => queueMicrotask(() => { element.focus(); element.select(); })}
            onInput={(event) => updateEditor({ bpm: Number(event.currentTarget.value) })}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                void commitEditor();
              } else if (event.key === "Escape") {
                event.preventDefault();
                cancelEditor();
              }
            }}
            onBlur={() => void commitEditor()}
          />
        </Show>
        <Show when={editorError() && editing()?.kind === "tempo"}>
          <span class="tempo-lane-error" role="alert">{laneErrorText(editorError()!)}</span>
        </Show>
      </div>

      <div
        class="tempo-lane-row tempo-lane-meter"
        ref={meterRow}
        role="group"
        tabIndex={0}
        aria-label={i18n.t("tempoLane.meterRow")}
        onClick={(event) => {
          if (!(event.target as HTMLElement).closest(".tempo-point")) setSelected(null);
        }}
        onDblClick={(event) => {
          if ((event.target as HTMLElement).closest(".tempo-point")) return;
          void addPointAt("meter", quarterAtClientX("meter", event.clientX));
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          if ((event.target as HTMLElement).closest(".tempo-point")) return;
          openRowMenu(event, "meter");
        }}
      >
        <Show when={!meterPoints().some((point) => point.position.bar === 1)}>
          <span
            class="tempo-point tempo-point-base"
            style={{ left: "0px" }}
            {...useTooltip(() =>
              i18n.t("tempoLane.baseMeter", {
                meter: formatMeter(
                  store.project.properties.time_signature.numerator,
                  store.project.properties.time_signature.denominator,
                ),
              }))}
          >
            {formatMeter(
              store.project.properties.time_signature.numerator,
              store.project.properties.time_signature.denominator,
            )}
          </span>
        </Show>
        <For each={meterPoints()}>
          {(point, index) => (
            <button
              type="button"
              class="tempo-point tempo-point-meter"
              classList={{ selected: isSelected("meter", index()) }}
              style={{ left: `${pointLeft("meter", index())}px` }}
              aria-label={meterPointLabel(point)}
              aria-pressed={isSelected("meter", index())}
              onPointerDown={(event) => beginDrag(event, "meter", index())}
              onDblClick={(event) => {
                event.stopPropagation();
                openEditor("meter", index());
              }}
              onFocus={() => select("meter", index())}
              onContextMenu={(event) => {
                event.preventDefault();
                event.stopPropagation();
                openPointMenu(event, "meter", index());
              }}
              {...useTooltip(() => meterPointLabel(point))}
            >
              {formatMeter(point.numerator, point.denominator)}
            </button>
          )}
        </For>
        <Show when={editing()?.kind === "meter"}>
          <span
            class="meter-editor"
            style={{ left: `${pointX("meter", editing()!.index)}px` }}
            onFocusOut={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node | null)) void commitEditor();
            }}
          >
            <input
              class="tempo-point-input meter-numerator-input"
              type="number"
              min="1"
              max="32"
              step="1"
              value={editing()!.numerator}
              aria-label={i18n.t("tempoLane.editNumerator")}
              aria-invalid={editorError() !== null}
              ref={(element) => queueMicrotask(() => { element.focus(); element.select(); })}
              onInput={(event) => updateEditor({ numerator: Number(event.currentTarget.value) })}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void commitEditor();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  cancelEditor();
                }
              }}
            />
            <span class="meter-editor-slash">/</span>
            <select
              class="meter-denominator-input"
              value={editing()!.denominator}
              aria-label={i18n.t("tempoLane.editDenominator")}
              onChange={(event) => updateEditor({ denominator: Number(event.currentTarget.value) })}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void commitEditor();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  cancelEditor();
                }
              }}
            >
              <For each={METER_DENOMINATORS}>{(value) => <option value={value}>{value}</option>}</For>
            </select>
          </span>
        </Show>
        <Show when={editorError() && editing()?.kind === "meter"}>
          <span class="tempo-lane-error" role="alert">{laneErrorText(editorError()!)}</span>
        </Show>
      </div>
    </div>
  );
}
