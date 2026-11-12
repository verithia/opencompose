/**
 * Automation rows under the track lanes.
 *
 * A row draws one lane: its polyline, its clip block when the lane has a span,
 * the points, a mute button and the empty-state help. The header side repeats
 * the lane name and its mute state so the lane list reads like the track list.
 *
 * Point gestures are pointer-driven: double-click adds a point at the snapped
 * position, dragging moves one with a ghost, and a right-click offers the four
 * curves plus tension. The commit happens on release; Escape, blur and unmount
 * abandon the drag. A muted lane refuses every edit and says why.
 *
 * A lane with a span also draws a block with its own gestures: drag it to move,
 * drag either edge to resize, Alt-drag to copy, and right-click for copy,
 * duplicate, split at the playhead and delete. The commit happens on release;
 * Escape cancels.
 */
import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";

import type { AutomationCurve, AutomationLane, BarBeat } from "../types/project";
import { store } from "../stores/project";
import { i18n, type I18nKey } from "../stores/i18n";
import { BLOCK_POSITION_QUARTERS, gridDivision, snapBlockQuarter } from "../stores/grid";
import { beginBlockGesture, endBlockGesture, isSecondaryClick } from "../stores/gesture";
import { createProjectClock } from "../utils/projectClock";
import { ownerEffects } from "../utils/automationTarget";
import {
  AUTOMATION_ROW_HEIGHT,
  automationLaneLabel,
  automationTargetKind,
  laneDomain,
  lanePolyline,
  laneSpan,
  pointXY,
  yToValue,
} from "../utils/automationRow";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";

const CURVES: AutomationCurve[] = ["linear", "stairs", "half_sine", "smooth"];

/** Height of the strip that names a bus or master group. */
export const AUTOMATION_OWNER_STRIP_HEIGHT = 18;

/** Height of the help line a track with no lanes shows. */
export const AUTOMATION_EMPTY_HEIGHT = 24;

/** Height a lane group occupies, in both the header and the lane column. */
export function automationAreaHeight(lanes: AutomationLane[]): number {
  return lanes.length > 0
    ? lanes.length * AUTOMATION_ROW_HEIGHT
    : AUTOMATION_EMPTY_HEIGHT;
}

/** `bar:beat`, with the subdivision only when the point sits off the beat. */
export function formatLanePosition(position: BarBeat): string {
  const subdivision = position.subdivision > 0 ? `:${position.subdivision}` : "";
  return `${position.bar}:${position.beat}${subdivision}`;
}

function curveLabel(curve: AutomationCurve): string {
  return i18n.t(`device.automation.curve.${curve}` as I18nKey);
}

/** The lane label and its target kind, as one line. */
function laneTitle(lane: AutomationLane): string {
  return `${automationLaneLabel(lane)} — ${i18n.t(`timeline.automation.kind.${automationTargetKind(lane)}` as I18nKey)}`;
}

/** Header cell of one lane: its name and a mute button. */
export function AutomationLaneHeader(props: {
  lane: AutomationLane;
  onToggleMute: () => void;
}) {
  return (
    <div class="automation-lane-header" classList={{ muted: Boolean(props.lane.muted) }}>
      <span class="automation-lane-name" title={laneTitle(props.lane)}>
        {automationLaneLabel(props.lane)}
      </span>
      <span class="automation-lane-kind">
        {i18n.t(`timeline.automation.kind.${automationTargetKind(props.lane)}` as I18nKey)}
      </span>
      <button
        type="button"
        class="track-btn automation-mute-btn"
        classList={{ "mute-active": Boolean(props.lane.muted) }}
        aria-label={
          props.lane.muted
            ? i18n.t("timeline.automation.unmute")
            : i18n.t("timeline.automation.mute")
        }
        aria-pressed={Boolean(props.lane.muted)}
        title={
          props.lane.muted
            ? i18n.t("timeline.automation.unmute")
            : i18n.t("timeline.automation.mute")
        }
        onClick={(event) => {
          event.stopPropagation();
          props.onToggleMute();
        }}
      >M</button>
    </div>
  );
}

/** Header side of a track's automation area that has no lanes. */
export function AutomationEmptyHeader() {
  return (
    <div class="automation-empty-header" style={{ height: `${AUTOMATION_EMPTY_HEIGHT}px` }}>
      {i18n.t("timeline.automation.title")}
    </div>
  );
}

/** Lane side of a track's automation area that has no lanes. */
export function AutomationEmptyLane() {
  return (
    <div
      class="automation-empty-lane"
      style={{ height: `${AUTOMATION_EMPTY_HEIGHT}px` }}
      role="note"
    >
      {i18n.t("timeline.automation.empty")}
    </div>
  );
}

/** Strip naming a bus or master group, drawn in both columns. */
export function AutomationOwnerStrip(props: { label: string }) {
  return (
    <div class="automation-owner-strip" style={{ height: `${AUTOMATION_OWNER_STRIP_HEIGHT}px` }}>
      {props.label}
    </div>
  );
}

export interface AutomationLaneRowProps {
  lane: AutomationLane;
  laneIndex: number;
  /** Panel target that owns the lane, so its effects can name a range. */
  owner?: string;
  pxPerBeat: number;
  width: number;
  /** Index of the selected point inside this lane, or null. */
  selectedPoint: number | null;
  onSelectPoint: (pointIndex: number | null) => void;
  onAddPoint: (positionBeats: number, value: number) => void;
  onMovePoint: (pointIndex: number, positionBeats: number, value: number) => void;
  onDeletePoint: (pointIndex: number) => void;
  onCurve: (pointIndex: number, curve: AutomationCurve, tension: number | null) => void;
  /** Ask for a tension value; the dialog lives with the timeline. */
  onTension: (pointIndex: number) => void;
  onToggleMute: () => void;
  /** True when this block is the timeline's selected automation clip. */
  blockSelected: boolean;
  onSelectBlock: () => void;
  onMoveBlock: (start: BarBeat) => void;
  onResizeBlock: (edge: "start" | "end", to: BarBeat) => void;
  onDuplicateBlock: (at: BarBeat) => void;
  onCopyBlock: () => void;
  onSplitBlock: () => void;
  onDeleteBlock: () => void;
  /** Whole menu for this block, supplied by the timeline from the clip pool. */
  blockMenuItems?: () => ContextMenuItem[];
}

/** One lane's drawing and point gestures. */
export function AutomationLaneRow(props: AutomationLaneRowProps) {
  let element: HTMLDivElement | undefined;
  const [ghost, setGhost] = createSignal<{ index: number; x: number; y: number } | null>(null);

  const height = AUTOMATION_ROW_HEIGHT;
  // One clock per project revision; every x on the row goes through it, so a
  // point sits over the same beat the clips use.
  const clock = createMemo(() => createProjectClock(store.project.properties));
  const quarterAt = (position: BarBeat): number => clock().quarterAt(position);
  // The target's own range, so a point can be authored anywhere the target
  // accepts; the owner names the chain that holds the device.
  const domain = createMemo(() =>
    laneDomain(
      props.lane,
      props.owner ? ownerEffects(store.project, props.owner) : undefined,
    ),
  );
  const points = createMemo(() =>
    props.lane.points.map((point) => pointXY(point, quarterAt, props.pxPerBeat, domain(), height)),
  );
  const span = createMemo(() => laneSpan(props.lane, quarterAt, props.pxPerBeat));
  const muted = () => Boolean(props.lane.muted);

  // ── Clip block gestures ──
  // The block and its two edge handles share one session. The ghost follows the
  // snapped pointer and the command goes out once, on release.
  const [blockGhost, setBlockGhost] = createSignal<
    { left: number; width: number; copy: boolean } | null
  >(null);
  let blockSession: {
    pointerId: number;
    mode: "move" | "start" | "end";
    gesture: number;
    grabOffset: number;
    startQuarter: number;
    endQuarter: number;
    startClientX: number;
    active: boolean;
    copy: boolean;
  } | null = null;

  /** The quarter under a client x, snapped to the shared block grid. */
  function blockQuarterAt(clientX: number): number {
    const left = element?.getBoundingClientRect().left ?? 0;
    return snapBlockQuarter(Math.max(0, (clientX - left) / props.pxPerBeat), gridDivision());
  }

  function endBlockDrag() {
    const session = blockSession;
    if (!session) return;
    document.removeEventListener("pointermove", handleBlockMove);
    document.removeEventListener("pointerup", handleBlockUp);
    document.removeEventListener("pointercancel", handleBlockCancel);
    document.removeEventListener("keydown", handleBlockKey, true);
    window.removeEventListener("blur", endBlockDrag);
    blockSession = null;
    endBlockGesture(session.gesture);
    setBlockGhost(null);
  }

  const handleBlockMove = (event: PointerEvent) => {
    const session = blockSession;
    if (!session || event.pointerId !== session.pointerId) return;
    if (!session.active) {
      if (Math.abs(event.clientX - session.startClientX) < 4) return;
      session.active = true;
    }
    event.preventDefault();
    const quarter = blockQuarterAt(event.clientX);
    const duration = session.endQuarter - session.startQuarter;
    if (session.mode === "move") {
      const start = Math.max(0, quarter - session.grabOffset);
      session.copy = event.altKey;
      setBlockGhost({
        left: start * props.pxPerBeat,
        width: duration * props.pxPerBeat,
        copy: session.copy,
      });
      return;
    }
    if (session.mode === "start") {
      const start = Math.min(quarter, session.endQuarter - BLOCK_POSITION_QUARTERS);
      setBlockGhost({
        left: start * props.pxPerBeat,
        width: (session.endQuarter - start) * props.pxPerBeat,
        copy: false,
      });
      return;
    }
    const end = Math.max(quarter, session.startQuarter + BLOCK_POSITION_QUARTERS);
    setBlockGhost({
      left: session.startQuarter * props.pxPerBeat,
      width: (end - session.startQuarter) * props.pxPerBeat,
      copy: false,
    });
  };

  const handleBlockUp = (event: PointerEvent) => {
    const session = blockSession;
    if (!session || event.pointerId !== session.pointerId) return;
    const { active, mode, copy, grabOffset, startQuarter, endQuarter } = session;
    endBlockDrag();
    if (!active) return;
    const quarter = blockQuarterAt(event.clientX);
    if (mode === "move") {
      const start = Math.max(0, quarter - grabOffset);
      const at = clock().barBeatAt(start);
      if (copy) props.onDuplicateBlock(at);
      else props.onMoveBlock(at);
      return;
    }
    if (mode === "start") {
      const start = Math.min(quarter, endQuarter - BLOCK_POSITION_QUARTERS);
      props.onResizeBlock("start", clock().barBeatAt(start));
      return;
    }
    const end = Math.max(quarter, startQuarter + BLOCK_POSITION_QUARTERS);
    props.onResizeBlock("end", clock().barBeatAt(end));
  };

  const handleBlockCancel = (event: PointerEvent) => {
    if (blockSession && event.pointerId !== blockSession.pointerId) return;
    endBlockDrag();
  };

  const handleBlockKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    endBlockDrag();
  };

  function beginBlockDrag(event: PointerEvent, mode: "move" | "start" | "end") {
    // A whole-timeline lane has no block, so there is nothing to grab.
    if (event.button !== 0 || isSecondaryClick(event) || !props.lane.time_range) return;
    event.preventDefault();
    event.stopPropagation();
    endBlockDrag();
    const startQuarter = quarterAt(props.lane.time_range.start);
    const endQuarter = quarterAt(props.lane.time_range.end);
    if (!(endQuarter > startQuarter)) return;
    props.onSelectBlock();
    blockSession = {
      pointerId: event.pointerId,
      mode,
      gesture: beginBlockGesture(mode === "move" ? "move" : "resize"),
      grabOffset: blockQuarterAt(event.clientX) - startQuarter,
      startQuarter,
      endQuarter,
      startClientX: event.clientX,
      active: false,
      copy: false,
    };
    document.addEventListener("pointermove", handleBlockMove);
    document.addEventListener("pointerup", handleBlockUp);
    document.addEventListener("pointercancel", handleBlockCancel);
    document.addEventListener("keydown", handleBlockKey, true);
    window.addEventListener("blur", endBlockDrag);
  }

  onCleanup(() => endBlockDrag());

  /** The block's own menu, and the row background's, since both address it. */
  function openBlockMenu(event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    props.onSelectBlock();
    const items = props.blockMenuItems?.() ?? [];
    if (items.length === 0) return;
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items,
    });
  }

  /** The pointer position in content coordinates, clamped to the row. */
  function pointerAt(event: PointerEvent | MouseEvent): { x: number; y: number } {
    const rect = element?.getBoundingClientRect();
    const left = rect ? rect.left : 0;
    const top = rect ? rect.top : 0;
    return {
      x: Math.max(0, event.clientX - left),
      y: Math.min(height - 1, Math.max(0, event.clientY - top)),
    };
  }

  /** A snapped quarter-note position and the value under a y coordinate. */
  function positionAt(x: number, y: number): { quarter: number; value: number } {
    return {
      quarter: snapBlockQuarter(x / props.pxPerBeat, gridDivision()),
      value: yToValue(y, domain(), height),
    };
  }

  // ── Point drag ──
  // One release path: pointer up commits the ghost's position, while cancel,
  // Escape, blur and unmount abandon the drag and write nothing.
  let pointSession: {
    pointerId: number;
    index: number;
    gesture: number;
    quarter: number;
    y: number;
    moved: boolean;
  } | null = null;

  /** Drop the point drag and its listeners; idempotent, never writes. */
  function endPointDrag() {
    const session = pointSession;
    if (!session) return;
    document.removeEventListener("pointermove", handlePointMove);
    document.removeEventListener("pointerup", handlePointUp);
    document.removeEventListener("pointercancel", handlePointCancel);
    document.removeEventListener("keydown", handlePointKey, true);
    window.removeEventListener("blur", endPointDrag);
    pointSession = null;
    endBlockGesture(session.gesture);
    setGhost(null);
  }

  const handlePointMove = (event: PointerEvent) => {
    const session = pointSession;
    if (!session || event.pointerId !== session.pointerId) return;
    session.moved = true;
    const at = pointerAt(event);
    session.quarter = positionAt(at.x, at.y).quarter;
    session.y = at.y;
    setGhost({ index: session.index, x: session.quarter * props.pxPerBeat, y: at.y });
  };

  const handlePointUp = (event: PointerEvent) => {
    const session = pointSession;
    if (!session || event.pointerId !== session.pointerId) return;
    const { index, moved, quarter, y } = session;
    endPointDrag();
    if (!moved) return;
    props.onMovePoint(index, quarter, yToValue(y, domain(), height));
  };

  const handlePointCancel = (event: PointerEvent) => {
    if (pointSession && event.pointerId !== pointSession.pointerId) return;
    endPointDrag();
  };

  const handlePointKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    endPointDrag();
  };

  function beginDrag(event: PointerEvent, index: number) {
    if (event.button !== 0 || isSecondaryClick(event)) return;
    event.preventDefault();
    event.stopPropagation();
    endPointDrag();
    props.onSelectPoint(index);
    if (muted()) return;
    const at = pointerAt(event);
    pointSession = {
      pointerId: event.pointerId,
      index,
      gesture: beginBlockGesture("move"),
      quarter: positionAt(at.x, at.y).quarter,
      y: at.y,
      moved: false,
    };
    document.addEventListener("pointermove", handlePointMove);
    document.addEventListener("pointerup", handlePointUp);
    document.addEventListener("pointercancel", handlePointCancel);
    document.addEventListener("keydown", handlePointKey, true);
    window.addEventListener("blur", endPointDrag);
  }

  onCleanup(() => endPointDrag());

  function openPointMenu(event: MouseEvent, index: number) {
    event.preventDefault();
    event.stopPropagation();
    props.onSelectPoint(index);
    const point = props.lane.points[index];
    if (!point) return;
    const curve = point.curve ?? "linear";
    const refusal = muted() ? i18n.t("timeline.automation.muted") : undefined;
    const items: ContextMenuItem[] = [
      {
        type: "heading",
        label: `${automationLaneLabel(props.lane)} · ${formatLanePosition(point.position)}`,
      },
      ...CURVES.map((name): ContextMenuItem => ({
        label: curveLabel(name),
        checked: curve === name,
        disabled: muted(),
        hint: refusal,
        onSelect: () =>
          props.onCurve(index, name, name === "smooth" ? (point.tension ?? 1) : null),
      })),
      { type: "separator" },
      {
        label: i18n.t("timeline.automation.tension"),
        disabled: muted(),
        hint: refusal,
        onSelect: () => props.onTension(index),
      },
      { type: "separator" },
      {
        label: i18n.t("timeline.automation.deletePoint"),
        danger: true,
        disabled: muted(),
        hint: refusal,
        onSelect: () => props.onDeletePoint(index),
      },
    ];
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "automation-point-menu",
      ariaLabel: i18n.t("menu.actions"),
      items,
    });
  }

  return (
    <div
      ref={element}
      class="automation-lane"
      classList={{ "whole-timeline": span() === null, muted: muted() }}
      data-lane={props.lane.param}
      data-lane-index={props.laneIndex}
      data-span={span() === null ? "whole" : "clip"}
      style={{ height: `${height}px`, width: `${props.width}px` }}
      onDblClick={(event) => {
        const target = event.target as HTMLElement;
        if (target.closest(".automation-point, .automation-resize-handle")) return;
        if (muted()) return;
        const at = pointerAt(event);
        const { quarter, value } = positionAt(at.x, at.y);
        props.onAddPoint(quarter, value);
      }}
      onPointerDown={(event) => {
        const target = event.target as HTMLElement;
        if (!target.closest(".automation-point")) props.onSelectPoint(null);
      }}
      onContextMenu={(event) => {
        const target = event.target as HTMLElement;
        // A point has its own menu; the block and its handles open the clip menu.
        if (target.closest(".automation-point")) return;
        openBlockMenu(event);
      }}
    >
      <Show when={span()}>
        {(block) => (
          <div
            class="automation-span"
            classList={{
              selected: props.blockSelected,
              dragging: blockGhost() !== null,
            }}
            data-automation-span
            style={{ left: `${block().left}px`, width: `${block().width}px` }}
            title={i18n.t("timeline.automation.blockHint")}
            onPointerDown={(event) => beginBlockDrag(event, "move")}
            onContextMenu={openBlockMenu}
          >
            <span
              class="automation-resize-handle start"
              aria-hidden="true"
              onPointerDown={(event) => beginBlockDrag(event, "start")}
            />
            <span
              class="automation-resize-handle end"
              aria-hidden="true"
              onPointerDown={(event) => beginBlockDrag(event, "end")}
            />
          </div>
        )}
      </Show>
      <Show when={blockGhost()}>
        {(preview) => (
          <div
            class="automation-block-ghost"
            classList={{ copy: preview().copy }}
            data-automation-block-ghost
            style={{ left: `${preview().left}px`, width: `${preview().width}px` }}
            aria-hidden="true"
          />
        )}
      </Show>
      <svg class="automation-curve" width={props.width} height={height} aria-hidden="true">
        <polyline class="automation-polyline" points={lanePolyline(props.lane, quarterAt, props.pxPerBeat, domain(), height)} />
      </svg>
      <span class="automation-row-name" title={laneTitle(props.lane)}>
        {automationLaneLabel(props.lane)}
      </span>
      <Show when={muted()}>
        <span class="automation-muted-note" role="note">
          {i18n.t("timeline.automation.muted")}
        </span>
      </Show>
      <For each={points()}>
        {(at, index) => (
          <button
            type="button"
            class="automation-point"
            classList={{ selected: props.selectedPoint === index() }}
            aria-label={`${automationLaneLabel(props.lane)} ${formatLanePosition(props.lane.points[index()].position)}`}
            style={{ left: `${at.x}px`, top: `${at.y}px` }}
            onPointerDown={(event) => beginDrag(event, index())}
            onDblClick={(event) => { event.stopPropagation(); }}
            onContextMenu={(event) => openPointMenu(event, index())}
            onKeyDown={(event) => {
              if (event.key === "Delete" || event.key === "Backspace") {
                event.preventDefault();
                event.stopPropagation();
                if (!muted()) props.onDeletePoint(index());
              }
            }}
          />
        )}
      </For>
      <Show when={ghost()}>
        {(preview) => (
          <span
            class="automation-ghost"
            data-automation-ghost
            style={{ left: `${preview().x}px`, top: `${preview().y}px` }}
            aria-hidden="true"
          />
        )}
      </Show>
    </div>
  );
}
