/**
 * One floating gain-envelope window at a time.
 *
 * The window is deliberately not modal: the timeline stays usable while it is
 * open, and opening another clip re-points the same window instead of stacking
 * a second one.
 */
import { createSignal } from "solid-js";

export type GainEnvelopeTab = "basic" | "env" | "playback";

export interface GainEnvelopeWindowState {
  clipIndex: number;
  tab: GainEnvelopeTab;
  x: number;
  y: number;
}

export const GAIN_ENVELOPE_WINDOW_WIDTH = 560;
export const GAIN_ENVELOPE_WINDOW_HEIGHT = 430;

const MARGIN = 12;

function clampPosition(x: number, y: number): { x: number; y: number } {
  const width = typeof window === "undefined" ? 1280 : window.innerWidth;
  const height = typeof window === "undefined" ? 800 : window.innerHeight;
  return {
    x: Math.max(MARGIN, Math.min(x, width - GAIN_ENVELOPE_WINDOW_WIDTH - MARGIN)),
    y: Math.max(MARGIN, Math.min(y, height - GAIN_ENVELOPE_WINDOW_HEIGHT - MARGIN)),
  };
}

function initialPosition(): { x: number; y: number } {
  const width = typeof window === "undefined" ? 1280 : window.innerWidth;
  const height = typeof window === "undefined" ? 800 : window.innerHeight;
  return clampPosition(
    (width - GAIN_ENVELOPE_WINDOW_WIDTH) / 2,
    Math.max(MARGIN, (height - GAIN_ENVELOPE_WINDOW_HEIGHT) / 2 - 20),
  );
}

const [state, setState] = createSignal<GainEnvelopeWindowState | null>(null);

function open(clipIndex: number, tab: GainEnvelopeTab = "basic") {
  setState((current) => current
    ? { ...current, clipIndex, tab }
    : { clipIndex, tab, ...initialPosition() });
}

function close() {
  setState(null);
}

function move(x: number, y: number) {
  const next = clampPosition(x, y);
  setState((current) => current ? { ...current, ...next } : current);
}

function setTab(tab: GainEnvelopeTab) {
  setState((current) => current ? { ...current, tab } : current);
}

export const gainEnvelopeWindow = {
  state,
  open,
  close,
  move,
  setTab,
  width: GAIN_ENVELOPE_WINDOW_WIDTH,
  height: GAIN_ENVELOPE_WINDOW_HEIGHT,
};
