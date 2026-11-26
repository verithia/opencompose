import { onCleanup, onMount } from "solid-js";
import { scheduleFrameWork } from "./frameWorkQueue";

const HORIZONTAL_LOOKAHEAD_PX = 384;
const SCROLL_SETTLE_MS = 36;
const FAST_SCROLL_STEP_PX = 96;

interface ObservedRow {
  visible: boolean;
  shown: boolean;
  pointer: boolean;
  focused: boolean;
  deferHorizontal: boolean;
  cancelWork?: () => void;
  update: (visible: boolean) => void;
}

/** One observer for the whole timeline. Row shells retain their dimensions;
 * expensive content mounts only near the viewport. A pointer/focus owner stays
 * mounted through release, including a drag that scrolls outside the viewport. */
export function createTimelineViewport(root: () => HTMLElement | undefined) {
  const supported = typeof IntersectionObserver !== "undefined";
  const rows = new Map<HTMLElement, ObservedRow>();
  const deferred = new Set<ObservedRow>();
  let observer: IntersectionObserver | undefined;
  let scrollSettle: ReturnType<typeof setTimeout> | undefined;
  let fastScroll = false;
  const update = (row: ObservedRow) => {
    deferred.delete(row);
    const shown = row.visible || row.pointer || row.focused;
    if (shown === row.shown) return;
    row.shown = shown;
    row.update(shown);
  };
  const schedule = (row: ObservedRow) => {
    row.cancelWork ??= scheduleFrameWork(() => {
      row.cancelWork = undefined;
      if (fastScroll && !row.pointer && !row.focused) return;
      update(row);
    });
  };
  const publish = (row: ObservedRow) => {
    if (row.deferHorizontal && scrollSettle !== undefined && !row.pointer && !row.focused) {
      if (row.shown === row.visible) {
        deferred.delete(row);
        row.cancelWork?.();
        row.cancelWork = undefined;
        return;
      }
      deferred.add(row);
      if (!fastScroll) schedule(row);
      return;
    }
    update(row);
  };
  const enclosing = (target: EventTarget | null): ObservedRow[] => {
    const found: ObservedRow[] = [];
    let element = target instanceof Element ? target : null;
    while (element && element !== root()) {
      const row = rows.get(element as HTMLElement);
      if (row) found.push(row);
      element = element.parentElement;
    }
    return found;
  };
  const down = (event: PointerEvent) => {
    for (const row of enclosing(event.target)) { row.pointer = true; publish(row); }
  };
  const release = () => {
    for (const row of rows.values()) {
      if (row.pointer) { row.pointer = false; publish(row); }
    }
  };
  const focusIn = (event: FocusEvent) => {
    for (const row of enclosing(event.target)) { row.focused = true; publish(row); }
  };
  const focusOut = (event: FocusEvent) => {
    const retained = new Set(enclosing(event.relatedTarget));
    for (const row of enclosing(event.target)) {
      row.focused = retained.has(row); publish(row);
    }
  };

  onMount(() => {
    const element = root();
    if (!supported || !element) return;
    let scrollLeft = element.scrollLeft;
    const scroll = () => {
      const next = element.scrollLeft;
      if (scrollLeft === next) return;
      fastScroll = Math.abs(next - scrollLeft) >= FAST_SCROLL_STEP_PX;
      scrollLeft = next;
      if (!fastScroll) for (const row of deferred) schedule(row);
      clearTimeout(scrollSettle);
      scrollSettle = setTimeout(() => {
        scrollSettle = undefined;
        fastScroll = false;
        for (const row of deferred) schedule(row);
      }, SCROLL_SETTLE_MS);
    };
    observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const row = rows.get(entry.target as HTMLElement);
        if (row) { row.visible = entry.isIntersecting; publish(row); }
      }
    }, { root: element, rootMargin: `256px ${HORIZONTAL_LOOKAHEAD_PX}px` });
    for (const element of rows.keys()) observer.observe(element);
    element.addEventListener("scroll", scroll, { passive: true });
    element.addEventListener("pointerdown", down, true);
    element.addEventListener("focusin", focusIn);
    element.addEventListener("focusout", focusOut);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    window.addEventListener("blur", release);
    onCleanup(() => {
      observer?.disconnect();
      clearTimeout(scrollSettle);
      element.removeEventListener("scroll", scroll);
      element.removeEventListener("pointerdown", down, true);
      element.removeEventListener("focusin", focusIn);
      element.removeEventListener("focusout", focusOut);
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
      window.removeEventListener("blur", release);
    });
  });
  return {
    supported,
    observe(element: HTMLElement, update: (visible: boolean) => void, options?: { deferHorizontal: boolean }) {
      if (!supported) { update(true); return; }
      const row: ObservedRow = {
        visible: false, shown: false, pointer: false, focused: false,
        deferHorizontal: options?.deferHorizontal ?? false, update,
      };
      rows.set(element, row);
      observer?.observe(element);
      onCleanup(() => {
        observer?.unobserve(element);
        deferred.delete(row);
        row.cancelWork?.();
        rows.delete(element);
      });
    },
  };
}

export type TimelineViewport = ReturnType<typeof createTimelineViewport>;
