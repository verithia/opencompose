/**
 * One pointer drag at a time, from the press to the release.
 *
 * A drag ends on the release, on a cancelled pointer, on the loss of its
 * capture, on Escape, or when the window loses focus. The last four are the
 * cases where no release ever arrives: the button came up outside the window,
 * over the native title bar, or the pointer was taken away. A drag that ends
 * without a release is cancelled, and a cancelled drag writes nothing: it
 * reports `committed: false` so the surface drops its preview instead of
 * applying it.
 *
 * The listeners sit on the window in the capture phase, so a move is seen
 * wherever the pointer goes, whether or not the platform supports pointer
 * capture. A running drag owns Escape, so the surface that started it does not
 * listen for the key itself.
 *
 * Only one drag owns the pointer. Starting another one cancels the running
 * drag first, so a press whose release was lost cannot leave a second listener
 * set behind and apply every move twice.
 */

export interface PointerDragOptions {
  /** Element that takes the pointer capture, when the platform has one. */
  element: Element;
  pointerId: number;
  onMove: (clientX: number, clientY: number) => void;
  /**
   * Ends the drag. `committed` is true only for the release; a cancelled
   * pointer, a lost capture, a blur, Escape and a superseding press all end it
   * with `false`.
   */
  onEnd?: (committed: boolean) => void;
  /** Cursor for the whole drag; the previous one comes back when it ends. */
  cursor?: string;
}

interface ActiveDrag {
  owner: number;
  end: (committed: boolean) => void;
}

let nextOwner = 0;
let active: ActiveDrag | null = null;

/** End the drag that is running, if any, as cancelled; safe when none is. */
export function endPointerDrag(): void {
  active?.end(false);
}

/** Start a drag; it owns the pointer until it ends. Returns its own stopper. */
export function beginPointerDrag(options: PointerDragOptions): () => void {
  endPointerDrag();

  const owner = ++nextOwner;
  const { element, pointerId, onMove, onEnd, cursor } = options;
  const body = document.body;
  const previousCursor = body.style.cursor;

  const move = (event: PointerEvent) => {
    if (event.pointerId !== pointerId) return;
    onMove(event.clientX, event.clientY);
  };

  // A blur of a focused child reaches this listener through the capture phase;
  // only the window itself losing focus ends the drag. The window reports its
  // own document, a node in it does not.
  const windowBlur = (event: Event) => {
    if ((event.target as Window | null)?.document === document) finish(false);
  };

  const release = (event: PointerEvent) => {
    if (event.pointerId === pointerId) finish(true);
  };

  const cancel = (event: PointerEvent) => {
    if (event.pointerId === pointerId) finish(false);
  };

  const escape = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    finish(false);
  };

  const finish = (committed: boolean) => {
    if (active?.owner !== owner) return;
    active = null;
    window.removeEventListener("pointermove", move, true);
    window.removeEventListener("pointerup", release, true);
    window.removeEventListener("pointercancel", cancel, true);
    window.removeEventListener("lostpointercapture", cancel, true);
    window.removeEventListener("blur", windowBlur, true);
    window.removeEventListener("keydown", escape, true);
    try {
      element.releasePointerCapture?.(pointerId);
    } catch {
      // The pointer was never captured (no support, or already released).
    }
    body.style.cursor = previousCursor;
    onEnd?.(committed);
  };

  window.addEventListener("pointermove", move, true);
  window.addEventListener("pointerup", release, true);
  window.addEventListener("pointercancel", cancel, true);
  window.addEventListener("lostpointercapture", cancel, true);
  window.addEventListener("blur", windowBlur, true);
  window.addEventListener("keydown", escape, true);
  active = { owner, end: finish };
  if (cursor) body.style.cursor = cursor;
  try {
    element.setPointerCapture?.(pointerId);
  } catch {
    // Not supported here; the window listeners still see every move.
  }

  // A surface that unmounts mid-drag ends its own drag, never another's.
  return () => {
    if (active?.owner === owner) finish(false);
  };
}
