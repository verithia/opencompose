/**
 * Interface scale — persisted browser-app zoom, independent of the timeline zoom.
 * Tauri's webview zoom keeps pointer coordinates in CSS pixels, so editing math
 * stays correct at every scale.
 */
import { createSignal } from "solid-js";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const STORAGE_KEY = "opencompose.uiScale";
export const UI_SCALE_BOUNDS = { min: 0.7, max: 1.5, step: 0.1 } as const;
const { min: MIN_SCALE, max: MAX_SCALE, step: STEP } = UI_SCALE_BOUNDS;

function clamp(scale: number): number {
  return Math.round(Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale)) * 10) / 10;
}

/** The OS already scales logical pixels. Fit only when the initial window is
 * too small to leave room for the editing surface and setup controls. */
export function recommendedUiScale(width: number, height: number): number {
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return 1;
  const fit = Math.min(1, width / 1024, height / 640);
  return clamp(Math.floor(fit * 10 + 1e-8) / 10);
}

function readStoredScale(): number | null {
  if (typeof window === "undefined") return null;
  try {
    const parsed = Number(window.localStorage.getItem(STORAGE_KEY));
    return Number.isFinite(parsed) && parsed > 0 ? clamp(parsed) : null;
  } catch {
    return null;
  }
}

const initialRecommendation = typeof window === "undefined" ? 1 : recommendedUiScale(window.innerWidth, window.innerHeight);
const [uiScale, setScale] = createSignal(readStoredScale() ?? initialRecommendation);

function persist(scale: number | null) {
  try {
    if (scale === null) window.localStorage.removeItem(STORAGE_KEY);
    else window.localStorage.setItem(STORAGE_KEY, String(scale));
  } catch {
    // Storage unavailable — keep the in-memory scale for this session.
  }
}

/** CSS cannot read the webview zoom, so publish it for the chrome insets. */
function publishScale(scale: number) {
  if (typeof document === "undefined") return;
  document.documentElement.style.setProperty("--ui-scale", String(scale));
}

function applyWebviewZoom(scale: number) {
  try {
    void getCurrentWebview().setZoom(scale).catch(() => {
      // Not running inside Tauri (tests, browser preview): nothing to apply.
    });
  } catch {
    // No Tauri webview API in this environment.
  }
}

function setUiScale(scale: number) {
  if (!Number.isFinite(scale)) return;
  const next = clamp(scale);
  setScale(next);
  persist(next);
  publishScale(next);
  applyWebviewZoom(next);
}

function adjustUiScale(steps: number) {
  setUiScale(uiScale() + steps * STEP);
}

function resetUiScale() {
  setScale(initialRecommendation);
  persist(null);
  publishScale(initialRecommendation);
  applyWebviewZoom(initialRecommendation);
}

function initUiScale() {
  publishScale(uiScale());
  applyWebviewZoom(uiScale());
}

export const uiScaleStore = {
  scale: uiScale,
  recommended: () => initialRecommendation,
  set: setUiScale,
  adjust: adjustUiScale,
  reset: resetUiScale,
  init: initUiScale,
};
