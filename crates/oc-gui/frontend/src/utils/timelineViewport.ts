import { onCleanup, onMount } from "solid-js";

interface ObservedRow {
  visible: boolean;
  pointer: boolean;
  focused: boolean;
  update: (visible: boolean) => void;
}

/** One observer for the whole timeline. Row shells retain their dimensions;
 * expensive content mounts only near the viewport. A pointer/focus owner stays
 * mounted through release, including a drag that scrolls outside the viewport. */
export function createTimelineViewport(root: () => HTMLElement | undefined) {
  const supported = typeof IntersectionObserver !== "undefined";
  const rows = new Map<HTMLElement, ObservedRow>();
  let observer: IntersectionObserver | undefined;
  const publish = (row: ObservedRow) => row.update(row.visible || row.pointer || row.focused);
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
    observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const row = rows.get(entry.target as HTMLElement);
        if (row) { row.visible = entry.isIntersecting; publish(row); }
      }
    }, { root: element, rootMargin: "256px" });
    for (const element of rows.keys()) observer.observe(element);
    element.addEventListener("pointerdown", down, true);
    element.addEventListener("focusin", focusIn);
    element.addEventListener("focusout", focusOut);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    window.addEventListener("blur", release);
    onCleanup(() => {
      observer?.disconnect();
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
    observe(element: HTMLElement, update: (visible: boolean) => void) {
      if (!supported) { update(true); return; }
      const row = { visible: false, pointer: false, focused: false, update };
      rows.set(element, row);
      observer?.observe(element);
      onCleanup(() => { observer?.unobserve(element); rows.delete(element); });
    },
  };
}

export type TimelineViewport = ReturnType<typeof createTimelineViewport>;
