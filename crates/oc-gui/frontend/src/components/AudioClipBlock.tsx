import { createEffect, createMemo, createSignal, For, Index, onCleanup, Show } from "solid-js";
import type { AudioClip, BarBeat } from "../types/project";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { themeStore } from "../stores/theme";
import { loadPeaks, peaksFor, peaksMissing, peaksVersion } from "../stores/audioPeaks";
import { blockStepQuarters, gridDivision, snapBlockQuarter } from "../stores/grid";
import { audioClipBounds } from "../utils/audioClipPlacement";
import { projectClock } from "../utils/projectClock";
import {
  CLIP_GAIN_MAX_DB,
  CLIP_GAIN_MIN_DB,
  MIN_FADE_SECONDS,
  formatClipGain,
  formatClipSeconds,
  gainAtY,
  gainLineY,
  loopedColumnRanges,
  paintWaveform,
} from "../utils/waveform";
import type { AudioClipEnvelope } from "../stores/api";
import { useTooltip, type TooltipContent } from "./Tooltip";
import { beginBlockGesture, endBlockGesture, isSecondaryClick } from "../stores/gesture";
import { beginPointerDrag } from "../utils/pointerDrag";

const MIN_QUARTERS = 0.25;
const LABEL_HEIGHT = 15;
/// Backing-store cap: a five-minute clip at maximum zoom is 72k CSS px wide.
/** Parse a computed `rgb(...)`/`rgba(...)` colour into channels. */
function colorChannels(value: string): [number, number, number] | null {
  const match = value.match(/rgba?\(([^)]+)\)/i);
  if (!match) return null;
  const parts = match[1].split(/[\s,/]+/).filter(Boolean).map(Number);
  if (parts.length < 3 || parts.some((part) => !Number.isFinite(part))) return null;
  return [parts[0], parts[1], parts[2]];
}

/** Resolve a CSS colour value on the canvas to plain channels. */
function readCanvasColor(canvas: HTMLCanvasElement, value: string): [number, number, number] | null {
  canvas.style.color = value;
  const channels = colorChannels(getComputedStyle(canvas).color);
  canvas.style.color = "";
  return channels;
}

/**
 * Ink for the waveform and its envelope: blend the track accent toward the
 * theme text colour. Dark themes get a light waveform, light themes a dark
 * one; the accent only supplies a hue. No color-mix() reaches Canvas 2D.
 */
function waveformColors(canvas: HTMLCanvasElement): {
  waveform: string;
  envelope: string;
  halo: string | undefined;
} {
  const inkChannels = readCanvasColor(canvas, "var(--text-primary)");
  const haloChannels = readCanvasColor(canvas, "var(--bg-void)");
  const halo = haloChannels
    ? `rgb(${haloChannels[0]}, ${haloChannels[1]}, ${haloChannels[2]})`
    : undefined;
  if (!inkChannels) return { waveform: "#d8c7b3", envelope: "#d8c7b3", halo };
  const ink = `rgb(${inkChannels[0]}, ${inkChannels[1]}, ${inkChannels[2]})`;
  const accentChannels = readCanvasColor(canvas, "var(--clip-accent)");
  if (!accentChannels) return { waveform: ink, envelope: ink, halo };
  const mix = (weight: number) => {
    const channels = accentChannels.map((channel, index) =>
      Math.round(channel * weight + inkChannels[index] * (1 - weight)),
    );
    return `rgb(${channels[0]}, ${channels[1]}, ${channels[2]})`;
  };
  // The waveform sits back; the envelope carries more of the track hue so it
  // reads as a control line rather than a black stroke over a tinted clip.
  return { waveform: mix(0.30), envelope: mix(0.55), halo };
}

const MAX_CANVAS_WIDTH = 8192;
/// Pointer travel that turns a press into a drag.
const DRAG_THRESHOLD = 3;
/// Gain step written to the project so the DSL keeps one decimal.
const GAIN_STEP = 0.1;
/// Fade step written to the project, in seconds.
const FADE_STEP = 0.001;
/// Slip step written to the project, in seconds.
const SLIP_STEP = 0.001;
/// Shortest loop the block accepts, matching the backend's refusal.
const MIN_LOOP_SECONDS = 0.1;

type DragMode = "move" | "trim-start" | "trim-end" | "stretch-end" | "gain" | "fade-in" | "fade-out" | "slip" | "loop";

interface DragSession {
  mode: DragMode;
  pointerId: number;
  clipIndex: number;
  revision: number;
  startClientX: number;
  startClientY: number;
  nominalStartQuarter: number;
  nominalEndQuarter: number;
  movedStartQuarter: number;
  originStartQuarter: number;
  originEndQuarter: number;
  originOffsetSeconds: number;
  originGainDb: number;
  originFadeIn: number;
  originFadeOut: number;
  originLoopSeconds: number;
  loopSeconds: number;
  gainDb: number;
  fadeIn: number;
  fadeOut: number;
  readoutX: number;
  grabOffsetQuarters: number;
  startQuarter: number;
  endQuarter: number;
  offsetSeconds: number;
  targetTrack: string | null;
  active: boolean;
  gesture: number;
}

export interface AudioClipBlockProps {
  clip: AudioClip;
  index: number;
  color: string;
  height: number;
  pxPerBeat: number;
  selected: boolean;
  tooltipContent: () => TooltipContent | null;
  beatFromClientX: (clientX: number) => number;
  trackAtClientY: (clientY: number) => string | null;
  onSelect: (index: number) => void;
  onEditPlayback?: (index: number) => void;
  onStretch?: (index: number, end: BarBeat, revision: number) => void;
  onWarp?: (index: number, markers: NonNullable<AudioClip["warp_markers"]>, revision: number) => void;
  onMove: (index: number, targetTrack: string, start: BarBeat) => void;
  onTrim: (index: number, start: BarBeat, end: BarBeat, edge: "start" | "end") => void;
  onEnvelope: (index: number, envelope: AudioClipEnvelope, revision: number) => void;
  onSlip: (index: number, offsetSeconds: number, revision: number) => void;
  onLoop: (index: number, loopSeconds: number, revision: number) => void;
  onError: (message: string) => void;
  onContextMenu: (event: MouseEvent) => void;
}

function basename(file: string): string {
  const parts = file.split(/[\\/]/);
  return parts[parts.length - 1] || file;
}

function formatSpan(seconds: number): string {
  if (seconds >= 1) return `${seconds.toFixed(1)}s`;
  return `${Math.round(seconds * 1000)}ms`;
}

function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(high, value));
}

/** Snap a fade to the stored step so the project text keeps three decimals. */
function roundFade(seconds: number): number {
  return Math.round(seconds / FADE_STEP) * FADE_STEP;
}

/** Snap a slip offset to the stored step so the project text keeps three decimals. */
function roundSlip(seconds: number): number {
  return Math.round(seconds / SLIP_STEP) * SLIP_STEP;
}

export function AudioClipBlock(props: AudioClipBlockProps) {
  let canvasRef: HTMLCanvasElement | undefined;
  let blockRef: HTMLDivElement | undefined;
  const [session, setSession] = createSignal<DragSession | null>(null);
  const tooltip = useTooltip(() => props.tooltipContent());

  const clock = () => projectClock(store.project.properties);
  const bounds = () => audioClipBounds(clock(), props.clip);
  const startQuarter = () => bounds().startQuarter;
  const endQuarter = () => bounds().endQuarter;

  const preview = createMemo(() => {
    const drag = session();
    if (!drag) {
      return {
        startQuarter: startQuarter(),
        endQuarter: endQuarter(),
        offsetSeconds: props.clip.offset_seconds,
        targetTrack: props.clip.track,
        gainDb: props.clip.gain_db,
        fadeIn: props.clip.fade_in_seconds,
        fadeOut: props.clip.fade_out_seconds,
        loopSeconds: props.clip.loop_seconds ?? 0,
      };
    }
    return {
      startQuarter: drag.startQuarter,
      endQuarter: drag.endQuarter,
      offsetSeconds: drag.offsetSeconds,
      targetTrack: drag.targetTrack,
      gainDb: drag.gainDb,
      fadeIn: drag.fadeIn,
      fadeOut: drag.fadeOut,
      loopSeconds: drag.loopSeconds,
    };
  });

  const leftPx = () => preview().startQuarter * props.pxPerBeat;
  const widthPx = () =>
    Math.max(2, (preview().endQuarter - preview().startQuarter) * props.pxPerBeat);
  const durationSeconds = () => {
    const range = preview();
    return Math.max(0, clock().secondsAt(range.endQuarter) - clock().secondsAt(range.startQuarter));
  };
  const clipDurationSeconds = () =>
    Math.max(0, clock().secondsAt(endQuarter()) - clock().secondsAt(startQuarter()));
  const gainDb = () => preview().gainDb;
  const fadeIn = () => preview().fadeIn;
  const fadeOut = () => preview().fadeOut;
  const waveHeight = () => Math.max(1, Math.round(props.height - LABEL_HEIGHT));
  const gainY = () => LABEL_HEIGHT + gainLineY(gainDb(), waveHeight());

  // A clip whose media is gone has no waveform to draw the ramp on, so its
  // envelope handles stay inert and say why.
  const mediaMissing = () => peaksMissing(props.clip.file);
  const gainDisabledReason = () => (mediaMissing() ? i18n.t("clip.noMedia") : null);
  const fadeDisabledReason = () =>
    mediaMissing()
      ? i18n.t("clip.noMedia")
      : clipDurationSeconds() < MIN_FADE_SECONDS
        ? i18n.t("clip.fadeNeedsLength")
        : null;

  const processingBypassed = () => props.clip.bypass_processing || props.clip.stretch_mode === "off";
  const durationRatio = (mediaSeconds: number) => {
    if (processingBypassed()) return 1;
    if (props.clip.tempo_follow && mediaSeconds > 0) {
      return clipDurationSeconds() / mediaSeconds;
    }
    return props.clip.stretch ?? 1;
  };
  const warpMarkers = () => processingBypassed() ? [] : props.clip.warp_markers ?? [];
  const stretchPreview = () => session()?.mode === "stretch-end" && !props.clip.bypass_processing && clipDurationSeconds() > 0
    ? durationSeconds() / clipDurationSeconds() : 1;
  const [warpPreview, setWarpPreview] = createSignal<{ index: number; target: number } | null>(null);
  const displayedWarpMarkers = () => {
    const active = warpPreview();
    return warpMarkers().map((marker, index) => index === active?.index
      ? { ...marker, target_seconds: active.target } : marker);
  };
  const markerPositions = () => displayedWarpMarkers().flatMap((marker, index) => {
    const range = preview();
    const elapsed = (marker.target_seconds - range.offsetSeconds) * stretchPreview();
    if (elapsed <= 0 || elapsed >= durationSeconds()) return [];
    const absolute = clock().secondsAt(range.startQuarter) + elapsed;
    return [{ index, x: (clock().quarterAtSeconds(absolute) - range.startQuarter) * props.pxPerBeat }];
  });

  /// Stretched media length in seconds: the span a slip can move through.
  const contentSeconds = () => {
    peaksVersion();
    const pyramid = peaksFor(props.clip.file);
    if (!pyramid) return 0;
    const media = pyramid.frames / pyramid.sample_rate;
    const last = displayedWarpMarkers().at(-1);
    return last
      ? last.target_seconds + Math.max(0, media - last.source_seconds) * durationRatio(media)
      : Math.max(0, media * durationRatio(media));
  };
  /// Seconds of media hidden behind the clip's tail; zero means nothing to slip.
  const maxSlipSeconds = () =>
    Math.max(0, contentSeconds() - clipDurationSeconds());
  const slipDisabledReason = () =>
    mediaMissing()
      ? i18n.t("clip.noMedia")
      : maxSlipSeconds() <= 1e-6 && (props.clip.offset_seconds ?? 0) <= 1e-6
        ? i18n.t("clip.nothingToSlip")
        : null;

  const loopSeconds = () => preview().loopSeconds;
  /// Match the engine's loop window, clamped to the media after the offset.
  const loopPeriod = () => Math.min(loopSeconds(), Math.max(0, contentSeconds() - preview().offsetSeconds));
  const loopPosition = (elapsed: number) => {
    const start = preview().startQuarter;
    return (clock().quarterAtSeconds(clock().secondsAt(start) + elapsed * stretchPreview()) - start) * props.pxPerBeat;
  };
  const loopPx = () => loopPosition(loopPeriod());
  const loopSeams = () => {
    const period = loopPeriod();
    if (!(period > 0) || loopPx() < 4) return [];
    const seams: number[] = [];
    for (let repeat = 1; repeat <= 256; repeat++) {
      const x = loopPosition(repeat * period);
      if (x >= widthPx() - 1) break;
      seams.push(x);
    }
    return seams;
  };
  /// Seconds a loop may repeat: media left after the offset, capped at the clip.
  /// Keep one grid step before the clip's end so a repeat fits.
  const gridStepSeconds = () => clock().secondsAt(endQuarter())
    - clock().secondsAt(Math.max(startQuarter(), endQuarter() - blockStepQuarters(gridDivision())));
  const loopCeiling = () =>
    Math.max(0, Math.min(
      Math.max(0, contentSeconds() - (props.clip.offset_seconds ?? 0)),
      clipDurationSeconds() - gridStepSeconds(),
    ));
  const loopDisabledReason = () => {
    if (mediaMissing()) return i18n.t("clip.loopNeedsMedia");
    if (contentSeconds() < MIN_LOOP_SECONDS) return i18n.t("clip.loopMediaTooShort");
    if (clipDurationSeconds() < MIN_LOOP_SECONDS) return i18n.t("clip.loopNeedsLength");
    if (loopCeiling() < MIN_LOOP_SECONDS) return i18n.t("clip.loopNeedsContent");
    return null;
  };

  const readout = createMemo(() => {
    const drag = session();
    if (!drag || !drag.active) return null;
    if (drag.mode === "slip") {
      const limit =
        drag.offsetSeconds <= 1e-9
          ? i18n.t("clip.slipStart")
          : maxSlipSeconds() - drag.offsetSeconds <= 1e-9
            ? i18n.t("clip.slipEnd")
            : null;
      return {
        x: drag.readoutX,
        text: i18n.t("clip.slipReadout", { value: formatClipSeconds(drag.offsetSeconds) }),
        limit,
      };
    }
    if (drag.mode === "gain") {
      const limit =
        drag.gainDb <= CLIP_GAIN_MIN_DB + 1e-9
          ? CLIP_GAIN_MIN_DB
          : drag.gainDb >= CLIP_GAIN_MAX_DB - 1e-9
            ? CLIP_GAIN_MAX_DB
            : null;
      return {
        x: drag.readoutX,
        text: i18n.t("clip.gainReadout", { value: formatClipGain(drag.gainDb) }),
        limit: limit === null ? null : i18n.t("clip.gainLimit", { limit: formatClipGain(limit) }),
      };
    }
    if (drag.mode === "loop") {
      const limit =
        drag.loopSeconds <= MIN_LOOP_SECONDS + 1e-9
          ? i18n.t("clip.loopLimit", { limit: formatClipSeconds(MIN_LOOP_SECONDS) })
          : loopCeiling() - drag.loopSeconds <= 1e-9
            ? i18n.t("clip.loopLimit", { limit: formatClipSeconds(loopCeiling()) })
            : null;
      return {
        x: drag.readoutX,
        text: i18n.t("clip.loopReadout", { value: formatClipSeconds(drag.loopSeconds) }),
        limit,
      };
    }
    if (drag.mode === "fade-in") {
      return {
        x: drag.readoutX,
        text: i18n.t("clip.fadeInReadout", { value: formatClipSeconds(drag.fadeIn) }),
        limit: null,
      };
    }
    return {
      x: drag.readoutX,
      text: i18n.t("clip.fadeOutReadout", { value: formatClipSeconds(drag.fadeOut) }),
      limit: null,
    };
  });

  createEffect(() => {
    const file = props.clip.file;
    if (!peaksFor(file)) void loadPeaks(file);
  });

  // Repaint on zoom, width, peak arrival, envelope changes and trim previews.
  createEffect(() => {
    // The waveform ink comes from theme tokens, so a theme switch must repaint.
    themeStore.cssTheme();
    const canvas = canvasRef;
    if (!canvas) return;
    const width = Math.max(1, Math.min(MAX_CANVAS_WIDTH, Math.round(widthPx())));
    const height = waveHeight();
    const dpr = window.devicePixelRatio || 1;
    const pixelWidth = Math.round(width * dpr);
    const pixelHeight = Math.round(height * dpr);
    if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
    if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
    const context = canvas.getContext("2d");
    if (!context) return;
    peaksVersion();
    const pyramid = peaksFor(props.clip.file);
    const range = preview();
    const duration = durationSeconds();
    const offset = range.offsetSeconds;
    // Peak data is in source time, while clip edits and fades use output time.
    const mediaSeconds = pyramid ? pyramid.frames / pyramid.sample_rate : 0;
    const ratio = durationRatio(mediaSeconds);
    const markers = displayedWarpMarkers();
    const scale = stretchPreview();
    const startSeconds = clock().secondsAt(range.startQuarter);
    const elapsedAt = (fraction: number) => {
      const quarter = range.startQuarter + fraction * (range.endQuarter - range.startQuarter);
      return Math.max(0, clock().secondsAt(quarter) - startSeconds) / scale;
    };
    const frameAt = (target: number) => {
      let previousSource = 0;
      let previousTarget = 0;
      for (const marker of markers) {
        if (target <= marker.target_seconds) {
          const position = (target - previousTarget) / (marker.target_seconds - previousTarget);
          return (previousSource + position * (marker.source_seconds - previousSource)) * (pyramid?.sample_rate ?? 0);
        }
        previousSource = marker.source_seconds;
        previousTarget = marker.target_seconds;
      }
      return (previousSource + (target - previousTarget) / ratio) * (pyramid?.sample_rate ?? 0);
    };
    const period = loopPeriod();
    const mediaStart = frameAt(offset);
    const mediaEnd = frameAt(offset + elapsedAt(1));
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    // Canvas 2D never sees a color-mix() expression: resolve the theme ink and
    // the track accent to rgb() first, then blend them.
    const { waveform: waveformColor, envelope: envelopeColor, halo } = waveformColors(canvas);
    paintWaveform(context, {
      pyramid,
      startFrame: mediaStart,
      endFrame: mediaEnd,
      rangesAt: (start, end) => period > 0
        ? loopedColumnRanges(elapsedAt(start), elapsedAt(end), offset, period, frameAt)
        : [[frameAt(offset + elapsedAt(start)), frameAt(offset + elapsedAt(end))]],
      durationSeconds: duration,
      offsetSeconds: offset,
      gainDb: gainDb(),
      fadeInSeconds: fadeIn(),
      fadeOutSeconds: fadeOut(),
      crossfadeSeconds: props.clip.crossfade_seconds ?? 0,
      gainEnvelope: props.clip.gain_envelope ?? [],
      width,
      height,
      color: waveformColor,
      envelopeColor,
      envelopeHalo: halo,
    });
  });

  let stopDrag: (() => void) | null = null;

  const beginWarpDrag = (event: PointerEvent, index: number) => {
    if (event.button !== 0 || isSecondaryClick(event) || !props.onWarp) return;
    event.preventDefault();
    event.stopPropagation();
    props.onSelect(props.index);
    const clipIndex = props.index;
    const revision = store.projectRevision;
    const original = warpMarkers();
    const marker = original[index];
    const offset = props.clip.offset_seconds;
    const startSeconds = clock().secondsAt(startQuarter());
    const markerQuarter = clock().quarterAtSeconds(startSeconds + marker.target_seconds - offset);
    const startClientX = event.clientX;
    const grabOffset = props.beatFromClientX(startClientX) - markerQuarter;
    const low = Math.max(offset + 0.001, (original[index - 1]?.target_seconds ?? 0) + 0.001);
    const high = Math.min(offset + clipDurationSeconds() - 0.001, (original[index + 1]?.target_seconds ?? Infinity) - 0.001);
    if (high <= low) return;
    stopDrag = beginPointerDrag({
      element: event.currentTarget as HTMLElement,
      pointerId: event.pointerId,
      cursor: "ew-resize",
      onMove: (x) => {
        if (Math.abs(x - startClientX) < DRAG_THRESHOLD) {
          setWarpPreview(null);
          return;
        }
        const quarter = snapBlockQuarter(Math.max(0, props.beatFromClientX(x) - grabOffset), gridDivision());
        setWarpPreview({ index, target: clamp(offset + clock().secondsAt(quarter) - startSeconds, low, high) });
      },
      onEnd: (committed) => {
        const current = warpPreview();
        stopDrag = null;
        setWarpPreview(null);
        if (!committed || !current || Math.abs(current.target - marker.target_seconds) < 1e-9) return;
        props.onWarp?.(clipIndex, original.map((point, i) => i === index
          ? { ...point, target_seconds: current.target } : point), revision);
      },
    });
  };

  const beginDrag = (event: PointerEvent, mode: DragMode) => {
    if (event.button !== 0 || isSecondaryClick(event)) return;
    // Alt-drag on the body slips the content under the clip instead of moving it.
    if (mode === "move" && event.altKey) mode = "slip";
    if (mode === "trim-end" && event.altKey && props.onStretch) mode = "stretch-end";
    event.preventDefault();
    event.stopPropagation();
    if (
      mode === "gain" ||
      mode === "fade-in" ||
      mode === "fade-out" ||
      mode === "slip" ||
      mode === "loop"
    ) {
      const reason =
        mode === "gain"
          ? gainDisabledReason()
          : mode === "slip"
            ? slipDisabledReason()
            : mode === "loop"
              ? loopDisabledReason()
              : fadeDisabledReason();
      if (reason) {
        props.onError(reason);
        return;
      }
    }
    props.onSelect(props.index);
    const originStart = startQuarter();
    const originEnd = endQuarter();
    const drag: DragSession = {
      mode,
      pointerId: event.pointerId,
      clipIndex: props.index,
      revision: store.projectRevision,
      startClientX: event.clientX,
      startClientY: event.clientY,
      nominalStartQuarter: clock().quarterAt(props.clip.time_range.start),
      nominalEndQuarter: clock().quarterAt(props.clip.time_range.end),
      movedStartQuarter: clock().quarterAt(props.clip.time_range.start),
      originStartQuarter: originStart,
      originEndQuarter: originEnd,
      originOffsetSeconds: props.clip.offset_seconds,
      originGainDb: props.clip.gain_db,
      originFadeIn: props.clip.fade_in_seconds,
      originFadeOut: props.clip.fade_out_seconds,
      originLoopSeconds: props.clip.loop_seconds ?? 0,
      loopSeconds: props.clip.loop_seconds ?? 0,
      gainDb: props.clip.gain_db,
      fadeIn: props.clip.fade_in_seconds,
      fadeOut: props.clip.fade_out_seconds,
      readoutX: 0,
      grabOffsetQuarters: props.beatFromClientX(event.clientX) - originStart,
      startQuarter: originStart,
      endQuarter: originEnd,
      offsetSeconds: props.clip.offset_seconds,
      targetTrack: props.clip.track,
      active: false,
      gesture: beginBlockGesture(
        mode === "move" ? "move" : mode === "slip" || mode === "loop" ? "slip" : "resize",
      ),
    };
    // Take the pointer before publishing the session: the helper cancels any
    // drag still running, and that drag must see its own state, not this one.
    stopDrag = beginPointerDrag({
      element: event.currentTarget as HTMLElement,
      pointerId: event.pointerId,
      onMove: handleMove,
      onEnd: (committed) => {
        stopDrag = null;
        finishDrag(committed);
      },
    });
    setSession(drag);
  };

  const handleMove = (clientX: number, clientY: number) => {
    const drag = session();
    if (!drag) return;
    if (!drag.active) {
      const dx = Math.abs(clientX - drag.startClientX);
      const dy = Math.abs(clientY - drag.startClientY);
      if (dx < DRAG_THRESHOLD && dy < DRAG_THRESHOLD) return;
      drag.active = true;
    }
    const clockNow = clock();
    if (drag.mode === "gain") {
      const lineY = LABEL_HEIGHT + gainLineY(drag.originGainDb, waveHeight());
      const pointerY = lineY + (clientY - drag.startClientY);
      drag.gainDb = clamp(
        Math.round(gainAtY(pointerY - LABEL_HEIGHT, waveHeight()) / GAIN_STEP) * GAIN_STEP,
        CLIP_GAIN_MIN_DB,
        CLIP_GAIN_MAX_DB,
      );
    } else if (drag.mode === "fade-in") {
      const quarter =
        drag.originStartQuarter + (clientX - drag.startClientX) / props.pxPerBeat;
      const seconds =
        clockNow.secondsAt(quarter) - clockNow.secondsAt(drag.originStartQuarter);
      drag.fadeIn = roundFade(clamp(seconds, 0, durationSeconds()));
    } else if (drag.mode === "fade-out") {
      const quarter =
        drag.originEndQuarter + (clientX - drag.startClientX) / props.pxPerBeat;
      const seconds =
        clockNow.secondsAt(drag.originEndQuarter) - clockNow.secondsAt(quarter);
      drag.fadeOut = roundFade(clamp(seconds, 0, durationSeconds()));
    } else if (drag.mode === "move") {
      const raw = props.beatFromClientX(clientX) - drag.grabOffsetQuarters;
      const nominalRaw = drag.nominalStartQuarter + raw - drag.originStartQuarter;
      const start = snapBlockQuarter(Math.max(0, nominalRaw), gridDivision());
      const end = start + drag.nominalEndQuarter - drag.nominalStartQuarter;
      drag.movedStartQuarter = start;
      drag.startQuarter = clockNow.quarterAtSeconds(Math.max(0,
        clockNow.secondsAt(start) + (props.clip.placement?.start_offset_seconds ?? 0)));
      drag.endQuarter = clockNow.quarterAtSeconds(Math.max(0,
        clockNow.secondsAt(end) + (props.clip.placement?.end_offset_seconds ?? 0)));
      // Null while the pointer is outside every lane: the drop is refused.
      drag.targetTrack = props.trackAtClientY(clientY);
    } else if (drag.mode === "slip") {
      // Dragging right moves the content right, revealing the hidden head.
      // The local seconds per quarter keeps the mapping safe at bar 1.
      const deltaQuarters = (clientX - drag.startClientX) / props.pxPerBeat;
      const secondsPerQuarter =
        clockNow.secondsAt(drag.originStartQuarter + 1) -
        clockNow.secondsAt(drag.originStartQuarter);
      drag.offsetSeconds = roundSlip(
        clamp(
          drag.originOffsetSeconds - deltaQuarters * secondsPerQuarter,
          0,
          maxSlipSeconds(),
        ),
      );
    } else if (drag.mode === "loop") {
      // The seam under the pointer is the loop's end, measured from the head.
      const startSeconds = clockNow.secondsAt(drag.originStartQuarter);
      const seam = clockNow.quarterAtSeconds(startSeconds + drag.originLoopSeconds);
      const delta = (clientX - drag.startClientX) / props.pxPerBeat;
      const seconds = clockNow.secondsAt(seam + delta) - startSeconds;
      drag.loopSeconds = roundSlip(clamp(seconds, MIN_LOOP_SECONDS, loopCeiling()));
    } else if (drag.mode === "trim-start") {
      const raw = snapBlockQuarter(props.beatFromClientX(clientX), gridDivision());
      const start = Math.max(0, Math.min(raw, drag.endQuarter - MIN_QUARTERS));
      drag.startQuarter = start;
      const deltaSeconds = clockNow.secondsAt(start) - clockNow.secondsAt(drag.originStartQuarter);
      drag.offsetSeconds = Math.max(0, drag.originOffsetSeconds + deltaSeconds);
    } else {
      const pointer = props.beatFromClientX(clientX);
      const grabOffset = drag.mode === "stretch-end"
        ? drag.originStartQuarter + drag.grabOffsetQuarters - drag.originEndQuarter
        : 0;
      const raw = snapBlockQuarter(pointer - grabOffset, gridDivision());
      drag.endQuarter = Math.max(drag.startQuarter + MIN_QUARTERS, raw);
    }
    const blockLeft = blockRef?.getBoundingClientRect().left ?? 0;
    drag.readoutX = clamp(clientX - blockLeft, 0, widthPx());
    setSession({ ...drag });
  };

  /** Ends the drag; only a release writes, so a cancelled one is dropped. */
  const finishDrag = (committed: boolean) => {
    const drag = session();
    if (!drag) return;
    setSession(null);
    endBlockGesture(drag.gesture);
    if (!committed) return;
    if (!drag.active) {
      if (blockRef) blockRef.focus({ preventScroll: true });
      return;
    }
    swallowTrailingClick();
    if (drag.mode === "slip") {
      if (Math.abs(drag.offsetSeconds - drag.originOffsetSeconds) < 1e-9) return;
      props.onSlip(drag.clipIndex, drag.offsetSeconds, drag.revision);
      return;
    }
    if (drag.mode === "loop") {
      if (Math.abs(drag.loopSeconds - drag.originLoopSeconds) < 1e-9) return;
      props.onLoop(drag.clipIndex, drag.loopSeconds, drag.revision);
      return;
    }
    if (drag.mode === "gain") {
      if (Math.abs(drag.gainDb - drag.originGainDb) < 1e-9) return;
      props.onEnvelope(drag.clipIndex, { gain_db: drag.gainDb }, drag.revision);
      return;
    }
    if (drag.mode === "fade-in" || drag.mode === "fade-out") {
      const value = drag.mode === "fade-in" ? drag.fadeIn : drag.fadeOut;
      const origin = drag.mode === "fade-in" ? drag.originFadeIn : drag.originFadeOut;
      // A ramp below the click threshold is refused rather than written; the
      // block snaps back because the project kept its old value.
      if (value > 0 && value < MIN_FADE_SECONDS) {
        props.onError(i18n.t("clip.fadeTooShort"));
        return;
      }
      if (Math.abs(value - origin) < 1e-9) return;
      props.onEnvelope(
        drag.clipIndex,
        drag.mode === "fade-in" ? { fade_in_seconds: value } : { fade_out_seconds: value },
        drag.revision,
      );
      return;
    }
    const clockNow = clock();
    if (drag.mode === "move") {
      // A drop outside every lane names no track and is refused.
      if (drag.targetTrack === null) return;
      if (
        Math.abs(drag.startQuarter - drag.originStartQuarter) < 1e-9 &&
        drag.targetTrack === props.clip.track
      ) {
        return;
      }
      props.onMove(props.index, drag.targetTrack, clockNow.barBeatAt(drag.movedStartQuarter));
      return;
    }
    const changed =
      Math.abs(drag.startQuarter - drag.originStartQuarter) > 1e-9 ||
      Math.abs(drag.endQuarter - drag.originEndQuarter) > 1e-9;
    if (!changed) return;
    if (drag.mode === "stretch-end") {
      props.onStretch?.(drag.clipIndex, clockNow.barBeatAt(drag.endQuarter), drag.revision);
      return;
    }
    props.onTrim(
      props.index,
      drag.mode === "trim-start" ? clockNow.barBeatAt(drag.startQuarter) : props.clip.time_range.start,
      drag.mode === "trim-end" ? clockNow.barBeatAt(drag.endQuarter) : props.clip.time_range.end,
      drag.mode === "trim-start" ? "start" : "end",
    );
  };

  const swallowTrailingClick = () => {
    const swallow = (event: MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      document.removeEventListener("click", swallow, true);
    };
    document.addEventListener("click", swallow, true);
    setTimeout(() => document.removeEventListener("click", swallow, true), 0);
  };

  onCleanup(() => stopDrag?.());

  return (
    <div
      ref={blockRef}
      class="phrase-block clip-block audio-clip-block"
      classList={{
        selected: props.selected,
        dragging: session() !== null,
        "audio-clip-retarget":
          preview().targetTrack !== null && preview().targetTrack !== props.clip.track,
      }}
      style={`left: ${leftPx()}px; width: ${widthPx()}px; --clip-accent: ${props.color};`}
      tabIndex={0}
      role="button"
      aria-label={i18n.t("clip.audioLabel", { name: props.clip.name })}
      {...tooltip}
      onClick={(event) => {
        event.stopPropagation();
        props.onSelect(props.index);
      }}
      onDblClick={(event) => {
        if ((event.target as Element | null)?.closest("button")) return;
        event.stopPropagation();
        props.onEditPlayback?.(props.index);
      }}
      onPointerDown={(event) => beginDrag(event, "move")}
      onContextMenu={(event) => props.onContextMenu(event)}
    >
      <span class="clip-accent-flag" aria-hidden="true" />
      <canvas ref={canvasRef} class="audio-clip-waveform" aria-hidden="true" />
      <div class="clip-label-chip">
        <span class="phrase-block-name">{basename(props.clip.file)}</span>
        <span class="clip-meta">
          {preview().offsetSeconds > 0
            ? i18n.t("clip.slipChip", { value: formatClipSeconds(preview().offsetSeconds) }) + " "
            : ""}
          {loopSeconds() > 0
            ? i18n.t("clip.loopChip", { value: formatClipSeconds(loopSeconds()) }) + " "
            : ""}
          {props.clip.gain_db !== 0 ? `${props.clip.gain_db > 0 ? "+" : ""}${props.clip.gain_db}dB ` : ""}
          {fadeIn() > 0 ? i18n.t("clip.fadeIn", { span: formatSpan(fadeIn()) }) + " " : ""}
          {fadeOut() > 0 ? i18n.t("clip.fadeOut", { span: formatSpan(fadeOut()) }) : ""}
        </span>
      </div>
      <div
        class="audio-clip-gain"
        classList={{ disabled: gainDisabledReason() !== null }}
        style={{ top: `${gainY()}px` }}
        aria-label={i18n.t("clip.gainHandle", { min: CLIP_GAIN_MIN_DB, max: CLIP_GAIN_MAX_DB })}
        aria-disabled={gainDisabledReason() ? "true" : undefined}
        title={gainDisabledReason() ?? undefined}
        onPointerDown={(event) => beginDrag(event, "gain")}
      />
      <div
        class="audio-clip-fade in"
        classList={{ disabled: fadeDisabledReason() !== null }}
        aria-label={i18n.t("clip.fadeInHandle")}
        aria-disabled={fadeDisabledReason() ? "true" : undefined}
        title={fadeDisabledReason() ?? undefined}
        onPointerDown={(event) => beginDrag(event, "fade-in")}
      />
      <div
        class="audio-clip-fade out"
        classList={{ disabled: fadeDisabledReason() !== null }}
        aria-label={i18n.t("clip.fadeOutHandle")}
        aria-disabled={fadeDisabledReason() ? "true" : undefined}
        title={fadeDisabledReason() ?? undefined}
        onPointerDown={(event) => beginDrag(event, "fade-out")}
      />
      <Index each={markerPositions()}>
        {(marker) => (
          <span class="audio-clip-warp-marker" style={{ left: `${marker().x}px` }}
            title={i18n.t("clipPlayback.warp")}
            onPointerDown={(event) => beginWarpDrag(event, marker().index)} />
        )}
      </Index>
      <For each={loopSeams()}>
        {(x) => (
          <span
            class="audio-clip-loop-seam"
            style={{ left: `${x}px` }}
            aria-hidden="true"
          />
        )}
      </For>
      <Show when={loopSeconds() > 0}>
        <div
          class="audio-clip-loop-handle"
          style={{ left: `${loopPx()}px` }}
          aria-label={i18n.t("clip.loopHandle")}
          title={i18n.t("clip.loopHandle")}
          onPointerDown={(event) => beginDrag(event, "loop")}
        />
      </Show>
      <div
        class="audio-clip-trim left"
        aria-label={i18n.t("clip.trimStart")}
        onPointerDown={(event) => beginDrag(event, "trim-start")}
      />
      <div
        class="audio-clip-trim right"
        aria-label={i18n.t("clip.trimEnd")}
        title={i18n.t("clipPlayback.stretchHandle")}
        onPointerDown={(event) => beginDrag(event, "trim-end")}
      />
      <Show when={props.selected && props.onStretch}>
        <button
          type="button"
          class="audio-clip-stretch-handle"
          aria-label={i18n.t("clipPlayback.dragToStretch")}
          title={i18n.t("clipPlayback.dragToStretch")}
          onPointerDown={(event) => beginDrag(event, "stretch-end")}
          onClick={(event) => {
            event.stopPropagation();
            props.onEditPlayback?.(props.index);
          }}
        >↔</button>
      </Show>
      <Show when={readout()}>
        {(content) => (
          <div class="clip-envelope-readout" style={{ left: `${content().x}px` }}>
            <span>{content().text}</span>
            <Show when={content().limit}>
              {(limit) => <span class="clip-envelope-limit">{limit()}</span>}
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}
