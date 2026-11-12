import { createSignal } from "solid-js";
import { isMac } from "./platform";
import * as api from "./api";
import type { UndoScope } from "./api";

/** The kind of pointer gesture the timeline currently owns. */
export type BlockGesture = "move" | "resize" | "marquee" | "slip";

/** Open a backend undo gesture; edits until `endUndoGesture` merge into one entry. */
export function beginUndoGesture(scope: UndoScope): void {
  void api.beginUndoGesture(scope).catch(() => {});
}

export function endUndoGesture(): void {
  void api.endUndoGesture().catch(() => {});
}

/**
 * Run a drag's commands as one undo entry. The begin token is dispatched first
 * but never awaited, so the first edit keeps the same latency as before.
 */
export async function withUndoGesture<T>(
  scope: UndoScope,
  action: () => Promise<T> | T,
): Promise<T> {
  beginUndoGesture(scope);
  try {
    return await action();
  } finally {
    endUndoGesture();
  }
}

interface ActiveGesture {
  kind: BlockGesture;
  owner: number;
}

let nextOwner = 0;
const [blockGesture, setBlockGesture] = createSignal<ActiveGesture | null>(null);

export { blockGesture };

/** Elements whose text is a copy target even while a gesture runs. */
const COPY_TARGET =
  ".selectable, .ai-message-content, .ai-tool-detail, input, textarea, [contenteditable='true']";

function blockSelectStart(event: Event): void {
  // A drag on the timeline must not select the interface, but it must never
  // take copy away from a surface that exists to be copied: a selection made
  // in the transcript stays the webview's.
  const target = event.target as Element | null;
  if (target?.closest?.(COPY_TARGET)) return;
  event.preventDefault();
}

/** Ends the gesture that locked selection, even if its own pointer stream is lost. */
let releaseLock: (() => void) | null = null;

/** A gesture owns the pointer, so no text may be selected while it runs. */
function lockTextSelection(owner: number): void {
  window.getSelection()?.removeAllRanges();
  document.addEventListener("selectstart", blockSelectStart, true);
  const release = () => {
    releaseLock = null;
    window.removeEventListener("pointerup", release, true);
    window.removeEventListener("pointercancel", release, true);
    window.removeEventListener("blur", release, true);
    endBlockGesture(owner);
  };
  releaseLock = release;
  window.addEventListener("pointerup", release, true);
  window.addEventListener("pointercancel", release, true);
  window.addEventListener("blur", release, true);
}

function unlockTextSelection(): void {
  document.removeEventListener("selectstart", blockSelectStart, true);
  if (releaseLock) {
    const release = releaseLock;
    releaseLock = null;
    window.removeEventListener("pointerup", release, true);
    window.removeEventListener("pointercancel", release, true);
    window.removeEventListener("blur", release, true);
  }
}

/** Begin a gesture; the returned token is required to end that same gesture. */
export function beginBlockGesture(kind: BlockGesture): number {
  const owner = ++nextOwner;
  setBlockGesture({ kind, owner });
  lockTextSelection(owner);
  return owner;
}

/** End the gesture this token started; a stale token cannot clear a newer one. */
export function endBlockGesture(owner: number): void {
  if (blockGesture()?.owner !== owner) return;
  setBlockGesture(null);
  unlockTextSelection();
}

/** Secondary click, including the macOS ctrl+click that reports the primary button. */
export function isSecondaryClick(event: MouseEvent): boolean {
  return event.button === 2 || (isMac() && event.ctrlKey);
}
