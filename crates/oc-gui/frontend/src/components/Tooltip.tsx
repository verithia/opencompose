import { For, Show, createEffect, createSignal, onCleanup, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { blockGesture } from "../stores/gesture";
import { isContextMenuOpen } from "./ContextMenu";
import { dialogOpen } from "./modalEscape";

export interface TooltipRow {
  label?: string;
  value: string;
  /** Numeric values render in the mono face so positions line up. */
  mono?: boolean;
}

export type TooltipContent = string | {
  title: string;
  /** Context line: block kind and track, muted under the title. */
  subtitle?: string;
  rows: TooltipRow[];
};

export interface TooltipAttachment {
  "aria-describedby": string;
  onMouseEnter: (event: MouseEvent & { currentTarget: Element }) => void;
  onMouseLeave: () => void;
  onFocusIn: (event: FocusEvent & { currentTarget: Element }) => void;
  onFocusOut: () => void;
}

interface TooltipState {
  id: string;
  content: TooltipContent;
  anchor: Pick<DOMRect, "left" | "top" | "right" | "bottom" | "width" | "height">;
}

interface TooltipPosition extends TooltipPlacement {
  id: string;
}

export interface TooltipPlacement {
  x: number;
  y: number;
  side: "top" | "bottom";
  /** Set only when neither side fits at full height; the card scrolls instead. */
  maxHeight: number | null;
}

const DEFAULT_DELAY = 400;
const GAP = 8;
const MARGIN = 8;

/**
 * Hover cards stand down while an overlay owns the pointer: a block gesture or
 * an open context menu. One switch keeps both from drifting into two mechanisms.
 */
export function tooltipSuppressed(): boolean {
  return blockGesture() !== null || isContextMenuOpen();
}

let nextTooltipId = 0;
let showTimer: ReturnType<typeof setTimeout> | null = null;
const [activeTooltip, setActiveTooltip] = createSignal<TooltipState | null>(null);

function clearShowTimer() {
  if (showTimer !== null) {
    clearTimeout(showTimer);
    showTimer = null;
  }
}

function showTooltip(state: TooltipState, delay: number) {
  // A gesture or menu owns the pointer; the hover card would cover its target.
  if (tooltipSuppressed()) return;
  if (delay <= 0) {
    clearShowTimer();
    setActiveTooltip(state);
    return;
  }
  // Re-entering the same anchor keeps the current tooltip without a new delay.
  if (activeTooltip()?.id === state.id) {
    clearShowTimer();
    setActiveTooltip(state);
    return;
  }
  clearShowTimer();
  showTimer = setTimeout(() => {
    showTimer = null;
    setActiveTooltip(state);
  }, delay);
}

function hideTooltip(id: string) {
  clearShowTimer();
  if (activeTooltip()?.id === id) setActiveTooltip(null);
}

/** Attach to any element that should show a tooltip on hover or focus. */
export function useTooltip(
  content: () => TooltipContent | null | undefined,
  options?: { delay?: number },
): TooltipAttachment {
  const id = `oc-tooltip-${++nextTooltipId}`;
  const delay = options?.delay ?? DEFAULT_DELAY;

  const show = (event: MouseEvent & { currentTarget: Element }) => {
    const value = content();
    if (!value) return;
    showTooltip({ id, content: value, anchor: event.currentTarget.getBoundingClientRect() }, delay);
  };
  const showFocused = (event: FocusEvent & { currentTarget: Element }) => {
    const value = content();
    if (!value) return;
    showTooltip({ id, content: value, anchor: event.currentTarget.getBoundingClientRect() }, 0);
  };

  onCleanup(() => hideTooltip(id));

  return {
    "aria-describedby": id,
    onMouseEnter: show,
    onMouseLeave: () => hideTooltip(id),
    onFocusIn: showFocused,
    onFocusOut: () => hideTooltip(id),
  };
}

/**
 * Pure placement helper: prefer below the anchor, flip above near the bottom
 * edge, and when neither side holds the whole card, keep it on the roomier
 * side with a max height so it scrolls instead of leaving the viewport.
 */
export function placeTooltip(
  anchor: Pick<DOMRect, "left" | "top" | "right" | "bottom" | "width" | "height">,
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  gap = GAP,
  margin = MARGIN,
): TooltipPlacement {
  const belowY = anchor.bottom + gap;
  const belowSpace = Math.max(0, viewport.height - margin - belowY);
  const aboveSpace = Math.max(0, anchor.top - gap - margin);
  let side: "top" | "bottom";
  let y: number;
  let maxHeight: number | null = null;
  if (size.height <= belowSpace) {
    side = "bottom";
    y = belowY;
  } else if (size.height <= aboveSpace) {
    side = "top";
    y = anchor.top - gap - size.height;
  } else if (belowSpace >= aboveSpace) {
    side = "bottom";
    y = belowY;
    maxHeight = belowSpace;
  } else {
    side = "top";
    y = margin;
    maxHeight = aboveSpace;
  }
  const clampedY = Math.max(margin, y);
  const centered = anchor.left + anchor.width / 2 - size.width / 2;
  const maxX = Math.max(margin, viewport.width - margin - size.width);
  return { x: Math.max(margin, Math.min(maxX, centered)), y: clampedY, side, maxHeight };
}

/** rAF for real layout; jsdom has no paint loop, so it falls back to a task. */
function nextFrame(callback: () => void): number {
  return typeof requestAnimationFrame === "function"
    ? requestAnimationFrame(callback)
    : (setTimeout(callback, 0) as unknown as number);
}

function cancelFrame(handle: number): void {
  if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(handle);
  else clearTimeout(handle);
}

/** Single global tooltip layer; mount once near the app root. */
export function TooltipLayer(): JSX.Element {
  let element: HTMLDivElement | undefined;
  const [position, setPosition] = createSignal<TooltipPosition | null>(null);

  // A gesture or an open menu dismisses whatever hover card is already up.
  createEffect(() => {
    if (!tooltipSuppressed()) return;
    clearShowTimer();
    setActiveTooltip(null);
  });

  createEffect(() => {
    const state = activeTooltip();
    setPosition(null);
    if (!state) return;
    let live = true;
    let observer: ResizeObserver | null = null;

    // The card's height changes when its rows and fonts finish laying out, so
    // measure after the first paint and keep re-placing while it settles.
    const apply = () => {
      if (!live || !element || !activeTooltip()) return;
      const rect = element.getBoundingClientRect();
      const border = Math.max(0, rect.height - element.clientHeight);
      const size = {
        width: rect.width,
        // scrollHeight is the unclamped content, even after a max-height.
        height: element.scrollHeight + border,
      };
      const viewport = {
        width: window.innerWidth || 1024,
        height: window.innerHeight || 768,
      };
      const placement = placeTooltip(activeTooltip()!.anchor, size, viewport);
      setPosition({ id: activeTooltip()!.id, ...placement });
    };
    const frame = nextFrame(() => {
      if (!live || !element) return;
      apply();
      if (typeof ResizeObserver === "function") {
        observer = new ResizeObserver(apply);
        observer.observe(element);
      }
    });
    const onViewportResize = () => apply();
    window.addEventListener("resize", onViewportResize);
    onCleanup(() => {
      live = false;
      cancelFrame(frame);
      observer?.disconnect();
      window.removeEventListener("resize", onViewportResize);
    });
  });

  return (
    <Portal>
      <Show when={tooltipSuppressed() ? null : activeTooltip()}>
        {(state) => {
          const placed = () => (position()?.id === state().id ? position() : null);
          return (
            <div
              ref={(el) => { element = el; }}
              id={state().id}
              class="oc-tooltip"
              classList={{
                "oc-tooltip-clamped": placed()?.maxHeight != null,
                "oc-tooltip-over-modal": dialogOpen(),
              }}
              role="tooltip"
              data-placement={placed()?.side ?? "bottom"}
              style={{
                left: `${placed()?.x ?? -9999}px`,
                top: `${placed()?.y ?? -9999}px`,
                ...(placed()?.maxHeight != null
                  ? { "max-height": `${placed()!.maxHeight}px` }
                  : {}),
              }}
            >
              <Show
                when={typeof state().content === "string"}
                fallback={
                  <>
                    <div class="oc-tooltip-title">{(state().content as { title: string }).title}</div>
                    <Show when={(state().content as { subtitle?: string }).subtitle}>
                      {(subtitle) => <div class="oc-tooltip-subtitle">{subtitle()}</div>}
                    </Show>
                    <div class="oc-tooltip-rows">
                      <For each={(state().content as { rows: TooltipRow[] }).rows}>
                        {(row) => (
                          <div class="oc-tooltip-row">
                            <Show when={row.label}>
                              <span class="oc-tooltip-label">{row.label}</span>
                            </Show>
                            <span class="oc-tooltip-value" classList={{ mono: row.mono, hint: !row.label }}>{row.value}</span>
                          </div>
                        )}
                      </For>
                    </div>
                  </>
                }
              >
                <div class="oc-tooltip-text">{state().content as string}</div>
              </Show>
            </div>
          );
        }}
      </Show>
    </Portal>
  );
}
