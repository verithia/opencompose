/** Slice passive timeline work across frames. Pointer gestures stay immediate. */
const FRAME_BUDGET_MS = 3;
const pending = new Set<() => void>();
let frame: number | undefined;

function requestPaintFrame() {
  if (frame !== undefined || pending.size === 0) return;
  frame = requestAnimationFrame(() => {
    frame = undefined;
    const deadline = performance.now() + FRAME_BUDGET_MS;
    for (const paint of pending) {
      pending.delete(paint);
      paint();
      if (performance.now() >= deadline) break;
    }
    requestPaintFrame();
  });
}

export function scheduleFrameWork(paint: () => void): () => void {
  pending.add(paint);
  requestPaintFrame();
  return () => {
    pending.delete(paint);
    if (pending.size === 0 && frame !== undefined) {
      cancelAnimationFrame(frame);
      frame = undefined;
    }
  };
}
