/** Keyboard focus ownership for a modal, including nested modal handoffs. */
import { onCleanup, onMount } from "solid-js";

export function useDialogFocus(root: () => HTMLElement | undefined, active: () => boolean) {
  onMount(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusable = () => [...(root()?.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"], summary',
    ) ?? [])].filter(element => !element.closest('[hidden], [inert], [aria-hidden="true"]')
      && element.tabIndex >= 0 && getComputedStyle(element).display !== "none" && getComputedStyle(element).visibility !== "hidden")
      .sort((a, b) => a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1);
    queueMicrotask(() => {
      if (active() && root()?.isConnected && !root()?.contains(document.activeElement)) {
        (focusable()[0] ?? root())?.focus({ preventScroll: true });
      }
    });
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || event.defaultPrevented || !active()) return;
      const controls = focusable();
      const first = controls[0], last = controls[controls.length - 1];
      const current = document.activeElement;
      if (!first || !root()?.contains(current) || (event.shiftKey ? current === first : current === last)) {
        event.preventDefault();
        (event.shiftKey ? last ?? root() : first ?? root())?.focus({ preventScroll: true });
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => {
      document.removeEventListener("keydown", onKeyDown, true);
      if (opener?.isConnected && (root()?.contains(document.activeElement) || document.activeElement === document.body)) {
        opener.focus({ preventScroll: true });
      }
    });
  });
}
