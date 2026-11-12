/**
 * Non-modal clip gain editor.
 *
 * One window edits one clip at a time. The Basic tab owns the clip-wide fades
 * and crossfade; the Envelope tab owns the optional point envelope relative to
 * the clip gain. The window floats above the arrangement and never blocks it.
 */
import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import { AudioClipPlaybackTab } from "./AudioClipPlaybackTab";
import { Portal } from "solid-js/web";
import { gainEnvelopeWindow, type GainEnvelopeTab } from "../stores/gainEnvelopeWindow";
import { openContextMenu } from "./ContextMenu";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { withUndoGesture } from "../stores/gesture";
import { beginPointerDrag, endPointerDrag } from "../utils/pointerDrag";
import { audioClipBounds } from "../utils/audioClipPlacement";
import { projectClock } from "../utils/projectClock";
import { fileName } from "../utils/clipContent";
import {
  MIN_FADE_SECONDS,
  formatClipGain,
  formatClipSeconds,
} from "../utils/waveform";
import type { AudioClip, GainEnvelopePoint } from "../types/project";

const ENVELOPE_MIN_DB = -60;
const ENVELOPE_MAX_DB = 24;
const ENVELOPE_SPAN_DB = ENVELOPE_MAX_DB - ENVELOPE_MIN_DB;
const MIN_POINT_GAP = 0.005;
const GRID_DB = [12, 0, -12, -24, -48];

function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(high, value));
}

function dbToPercent(db: number): number {
  return ((ENVELOPE_MAX_DB - clamp(db, ENVELOPE_MIN_DB, ENVELOPE_MAX_DB)) / ENVELOPE_SPAN_DB) * 100;
}

function percentToDb(percent: number): number {
  return ENVELOPE_MAX_DB - (clamp(percent, 0, 100) / 100) * ENVELOPE_SPAN_DB;
}

function gainAt(points: GainEnvelopePoint[], position: number): number {
  if (points.length === 0) return 0;
  if (position <= points[0].position) return points[0].gain_db;
  for (let index = 1; index < points.length; index++) {
    const previous = points[index - 1];
    const next = points[index];
    if (position <= next.position) {
      const span = next.position - previous.position;
      if (span <= 0) return next.gain_db;
      const t = (position - previous.position) / span;
      return previous.gain_db + (next.gain_db - previous.gain_db) * t;
    }
  }
  return points[points.length - 1].gain_db;
}

function ParamSlider(props: {
  label: string;
  value: number;
  max: number;
  step: number;
  disabled?: boolean;
  onCommit: (value: number) => void;
}) {
  const [value, setValue] = createSignal(props.value);
  createEffect(() => setValue(props.value));
  const fill = () => {
    const max = Math.max(props.max, 0.001);
    return `${Math.max(0, Math.min(100, (value() / max) * 100))}%`;
  };
  return (
    <label class="gain-env-param">
      <span class="gain-env-param-head">
        <span>{props.label}</span>
        <output>{formatClipSeconds(value())}</output>
      </span>
      <input
        type="range"
        min="0"
        max={Math.max(props.max, 0.02)}
        step={props.step}
        value={value()}
        disabled={props.disabled}
        style={{ "--slider-fill": fill() }}
        onInput={(event) => setValue(Number(event.currentTarget.value))}
        onChange={(event) => props.onCommit(Number(event.currentTarget.value))}
      />
    </label>
  );
}

function BasicTab(props: { clip: AudioClip; seconds: number }) {
  const [start, setStart] = createSignal("0");
  const [nudge, setNudge] = createSignal("1");
  const [placing, setPlacing] = createSignal(false);
  createEffect(() => setStart(String(Number(audioClipBounds(
    projectClock(store.project.properties), props.clip,
  ).startSeconds.toFixed(6)))));
  const validStart = () => start().trim() !== "" && Number.isFinite(Number(start())) && Number(start()) >= 0;
  const validNudge = () => nudge().trim() !== "" && Number.isFinite(Number(nudge())) && Number(nudge()) > 0;
  const place = async (placement: { start_seconds?: number; nudge_ms?: number }) => {
    const index = gainEnvelopeWindow.state()?.clipIndex;
    if (index === undefined || placing()) return;
    setPlacing(true);
    try {
      await withUndoGesture("project", () => store.setAudioClipPlacement(index, placement));
    } finally {
      setPlacing(false);
    }
  };
  const apply = (values: {
    fade_in_seconds?: number;
    fade_out_seconds?: number;
    crossfade_seconds?: number;
  }) => {
    const index = gainEnvelopeWindow.state()?.clipIndex;
    if (index === undefined) return;
    void withUndoGesture("project", () => store.setAudioClipEnvelope(index, values));
  };
  const disabled = () => props.seconds < MIN_FADE_SECONDS;
  const fadeValue = (value: number) => value === 0 ? 0 : Math.max(MIN_FADE_SECONDS, value);
  return (
    <div class="gain-env-basic">
      <div class="gain-env-placement">
        <label>
          {i18n.t("clipPlacement.startSeconds")}
          <input type="number" min="0" step="0.001" value={start()}
            onInput={(event) => setStart(event.currentTarget.value)} disabled={placing()} />
        </label>
        <button type="button" class="gain-env-add" disabled={placing() || !validStart()} onClick={() => void place({ start_seconds: Number(start()) })}>
          {i18n.t("clipPlacement.applyStart")}
        </button>
        <label>
          {i18n.t("clipPlacement.nudgeMs")}
          <input type="number" min="0.001" step="0.1" value={nudge()}
            onInput={(event) => setNudge(event.currentTarget.value)} disabled={placing()} />
        </label>
        <div class="gain-env-placement-actions">
          <button type="button" class="gain-env-add" disabled={placing() || !validNudge()} onClick={() => void place({ nudge_ms: -Number(nudge()) })}>
            {i18n.t("clipPlacement.earlier")}
          </button>
          <button type="button" class="gain-env-add" disabled={placing() || !validNudge()} onClick={() => void place({ nudge_ms: Number(nudge()) })}>
            {i18n.t("clipPlacement.later")}
          </button>
        </div>
        <p class="gain-env-hint">{i18n.t("clipPlacement.hint")}</p>
      </div>
      <p class="gain-env-hint">{i18n.t("gainEnvelope.basicHint")}</p>
      <ParamSlider
        label={i18n.t("gainEnvelope.fadeIn")}
        value={props.clip.fade_in_seconds}
        max={props.seconds}
        step={0.001}
        disabled={disabled()}
        onCommit={(value) => apply({ fade_in_seconds: fadeValue(value) })}
      />
      <ParamSlider
        label={i18n.t("gainEnvelope.fadeOut")}
        value={props.clip.fade_out_seconds}
        max={props.seconds}
        step={0.001}
        disabled={disabled()}
        onCommit={(value) => apply({ fade_out_seconds: fadeValue(value) })}
      />
      <ParamSlider
        label={i18n.t("gainEnvelope.crossfade")}
        value={props.clip.crossfade_seconds ?? 0}
        max={props.seconds}
        step={0.001}
        disabled={disabled()}
        onCommit={(value) => apply({ crossfade_seconds: fadeValue(value) })}
      />
    </div>
  );
}

function EnvelopeTab(props: { clip: AudioClip }) {
  let plot: HTMLDivElement | undefined;
  let stopDrag: (() => void) | undefined;
  onCleanup(() => stopDrag?.());
  const [draft, setDraft] = createSignal<GainEnvelopePoint[]>([]);
  const [dragging, setDragging] = createSignal(false);
  const [activePoint, setActivePoint] = createSignal<number | null>(null);

  const points = () => props.clip.gain_envelope ?? [];

  createEffect(() => {
    const current = points();
    if (dragging()) return;
    setDraft(current.map((point) => ({ ...point })));
  });

  const commit = async (
    next: GainEnvelopePoint[],
    index = gainEnvelopeWindow.state()?.clipIndex,
    revision = store.projectRevision,
  ) => {
    if (index === undefined) return;
    setDragging(true);
    try {
      await withUndoGesture("project", () =>
        store.setAudioClipEnvelope(index, { gain_envelope: next }, revision),
      );
    } finally {
      setDragging(false);
    }
  };

  const startPointDrag = (event: PointerEvent, index: number) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const bounds = plot?.getBoundingClientRect();
    if (!bounds || bounds.width <= 0 || bounds.height <= 0) return;
    const clipIndex = gainEnvelopeWindow.state()?.clipIndex;
    if (clipIndex === undefined) return;
    const revision = store.projectRevision;
    const clipName = props.clip.name;
    stopDrag?.();
    setActivePoint(index);
    setDragging(true);
    stopDrag = beginPointerDrag({
      element: event.currentTarget as Element,
      pointerId: event.pointerId,
      cursor: "grabbing",
      onMove: (clientX, clientY) => {
        const next = draft().map((point) => ({ ...point }));
        const point = next[index];
        if (!point) return;
        const rawPosition = clamp((clientX - bounds.left) / bounds.width, 0, 1);
        const gain = clamp(percentToDb(((clientY - bounds.top) / bounds.height) * 100), ENVELOPE_MIN_DB, ENVELOPE_MAX_DB);
        if (index === 0) point.position = 0;
        else if (index === next.length - 1) point.position = 1;
        else {
          const low = next[index - 1].position + MIN_POINT_GAP;
          const high = next[index + 1].position - MIN_POINT_GAP;
          point.position = clamp(rawPosition, low, high);
        }
        point.gain_db = Math.round(gain * 10) / 10;
        setDraft(next);
      },
      onEnd: (committed) => {
        stopDrag = undefined;
        setActivePoint(null);
        if (!committed || props.clip.name !== clipName) {
          setDragging(false);
          return;
        }
        void commit(draft(), clipIndex, revision);
      },
    });
  };

  const addPoint = (event: MouseEvent) => {
    if ((event.target as Element | null)?.closest(".gain-env-point")) return;
    const bounds = plot?.getBoundingClientRect();
    if (!bounds || bounds.width <= 0 || bounds.height <= 0) return;
    const position = clamp((event.clientX - bounds.left) / bounds.width, 0.01, 0.99);
    const next = [
      ...draft(),
      { position, gain_db: Math.round(gainAt(draft(), position) * 10) / 10 },
    ].sort((left, right) => left.position - right.position);
    setDraft(next);
    void commit(next);
  };

  const removePoint = (index: number) => {
    if (index <= 0 || index >= draft().length - 1) return;
    const next = draft().filter((_, at) => at !== index);
    setDraft(next);
    void commit(next);
  };

  const addEnvelope = () => {
    const next = [
      { position: 0, gain_db: 0 },
      { position: 1, gain_db: 0 },
    ];
    setDraft(next);
    void commit(next);
  };

  const polyline = () =>
    draft().map((point) => `${point.position * 100},${dbToPercent(point.gain_db)}`).join(" ");

  return (
    <Show
      when={points().length > 0}
      fallback={
        <div class="gain-env-empty">
          <p>{i18n.t("gainEnvelope.empty")}</p>
          <p class="gain-env-hint">{i18n.t("gainEnvelope.addHint")}</p>
          <button type="button" class="gain-env-add" onClick={addEnvelope}>
            {i18n.t("gainEnvelope.add")}
          </button>
        </div>
      }
    >
      <div class="gain-env-env">
        <p class="gain-env-hint">
          {i18n.t("gainEnvelope.relative")} · {i18n.t("gainEnvelope.envHint")}
        </p>
        <div
          class="gain-env-plot"
          ref={plot}
          onDblClick={addPoint}
          onContextMenu={(event) => event.preventDefault()}
        >
          <svg class="gain-env-grid" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            <For each={GRID_DB}>
              {(db) => (
                <line
                  x1="0"
                  x2="100"
                  y1={dbToPercent(db)}
                  y2={dbToPercent(db)}
                  classList={{ "gain-env-zero": db === 0 }}
                />
              )}
            </For>
          </svg>
          <svg class="gain-env-line" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            <polyline points={polyline()} />
          </svg>
          <Index each={draft()}>
            {(point, index) => (
              <button
                type="button"
                class="gain-env-point"
                data-envelope-point
                classList={{ active: activePoint() === index }}
                style={{
                  left: `${point().position * 100}%`,
                  top: `${dbToPercent(point().gain_db)}%`,
                }}
                aria-label={i18n.t("gainEnvelope.point", {
                  index: index + 1,
                  gain: formatClipGain(point().gain_db),
                  position: Math.round(point().position * 100),
                })}
                onPointerDown={(event) => startPointDrag(event, index)}
                onDblClick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  endPointerDrag();
                  removePoint(index);
                }}
                onContextMenu={(event) => {
                  // A right-click opens the app's own menu instead of deleting
                  // the point outright, so the gesture names itself.
                  event.preventDefault();
                  event.stopPropagation();
                  openContextMenu({
                    x: event.clientX,
                    y: event.clientY,
                    items: [
                      { type: "heading", label: i18n.t("gainEnvelope.pointTitle", { index: index + 1 }) },
                      {
                        label: i18n.t("gainEnvelope.deletePoint"),
                        danger: true,
                        onSelect: () => removePoint(index),
                      },
                    ],
                  });
                }}
              />
            )}
          </Index>
          <div class="gain-env-scale" aria-hidden="true">
            <For each={GRID_DB}>
              {(db) => (
                <span style={{ top: `${dbToPercent(db)}%` }}>
                  {db > 0 ? `+${db}` : db}
                </span>
              )}
            </For>
          </div>
        </div>
      </div>
    </Show>
  );
}

export function GainEnvelopeWindow() {
  const state = () => gainEnvelopeWindow.state();
  const clip = createMemo(() => {
    const current = state();
    if (!current) return undefined;
    return store.project.properties.audio_clips?.[current.clipIndex];
  });
  const seconds = createMemo(() => {
    const current = clip();
    if (!current) return 0;
    const clock = projectClock(store.project.properties);
    return Math.max(0, audioClipBounds(clock, current).durationSeconds);
  });

  createEffect(() => {
    if (state() && !clip()) gainEnvelopeWindow.close();
  });

  createEffect(() => {
    if (!state()) return;
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      gainEnvelopeWindow.close();
    };
    window.addEventListener("keydown", escape);
    onCleanup(() => window.removeEventListener("keydown", escape));
  });

  const startMove = (event: PointerEvent) => {
    if (event.button !== 0) return;
    if ((event.target as Element | null)?.closest("button")) return;
    event.preventDefault();
    event.stopPropagation();
    const current = state();
    if (!current) return;
    const offsetX = event.clientX - current.x;
    const offsetY = event.clientY - current.y;
    beginPointerDrag({
      element: event.currentTarget as Element,
      pointerId: event.pointerId,
      cursor: "move",
      onMove: (clientX, clientY) => gainEnvelopeWindow.move(clientX - offsetX, clientY - offsetY),
    });
  };

  const selectTab = (tab: GainEnvelopeTab) => gainEnvelopeWindow.setTab(tab);

  return (
    <Portal>
      <Show when={state()}>
        {(current) => (
          <Show when={clip()}>
            {(currentClip) => (
              <section
                class="gain-envelope-window"
                role="dialog"
                aria-label={i18n.t(current().tab === "playback" ? "clipPlayback.open" : "gainEnvelope.title")}
                data-gain-envelope-window
                style={{
                  left: `${current().x}px`,
                  top: `${current().y}px`,
                  width: `${gainEnvelopeWindow.width}px`,
                  height: `${gainEnvelopeWindow.height}px`,
                }}
                onPointerDown={(event) => event.stopPropagation()}
              >
                <header class="gain-envelope-title" onPointerDown={startMove}>
                  <div class="gain-envelope-heading">
                    <strong>{i18n.t(current().tab === "playback" ? "clipPlayback.open" : "gainEnvelope.title")}</strong>
                    <span>{currentClip().name || fileName(currentClip().file)}</span>
                  </div>
                  <button
                    type="button"
                    class="gain-envelope-close"
                    aria-label={i18n.t("gainEnvelope.close")}
                    onClick={() => gainEnvelopeWindow.close()}
                  >
                    ×
                  </button>
                </header>
                <nav class="gain-envelope-tabs" aria-label={i18n.t(current().tab === "playback" ? "clipPlayback.open" : "gainEnvelope.title")}>
                  <button
                    type="button"
                    data-tab="basic"
                    classList={{ active: current().tab === "basic" }}
                    onClick={() => selectTab("basic")}
                  >
                    {i18n.t("gainEnvelope.basic")}
                  </button>
                  <button
                    type="button"
                    data-tab="env"
                    classList={{ active: current().tab === "env" }}
                    onClick={() => selectTab("env")}
                  >
                    {i18n.t("gainEnvelope.env")}
                  </button>
                  <button
                    type="button"
                    data-tab="playback"
                    classList={{ active: current().tab === "playback" }}
                    onClick={() => selectTab("playback")}
                  >
                    {i18n.t("clipPlayback.tab")}
                  </button>
                </nav>
                <div class="gain-envelope-body">
                  <Show when={current().tab === "basic"}>
                    <BasicTab clip={currentClip()} seconds={seconds()} />
                  </Show>
                  <Show when={current().tab === "env"}>
                    <EnvelopeTab clip={currentClip()} />
                  </Show>
                  <Show when={current().tab === "playback"}>
                    <AudioClipPlaybackTab clip={currentClip()} index={current().clipIndex} />
                  </Show>
                </div>
              </section>
            )}
          </Show>
        )}
      </Show>
    </Portal>
  );
}
