import { fireEvent } from "@solidjs/testing-library";

/** Resolve both inline faders and the dial's stable, portalled native range. */
export function synthRangeInput(root: HTMLElement): HTMLInputElement {
  const local = root.querySelector<HTMLInputElement>('input[type="range"]');
  if (local) return local;
  const input = document.getElementById(root.dataset.rangeInput ?? "");
  if (!(input instanceof HTMLInputElement) || input.type !== "range") {
    throw new Error(`No synth range for ${root.dataset.param ?? root.tagName}`);
  }
  return input;
}

export function synthKnobTrigger(root: HTMLElement): HTMLButtonElement {
  const trigger = root.querySelector<HTMLButtonElement>(".synth-param-button");
  if (!trigger) throw new Error(`No synth dial for ${root.dataset.param ?? root.tagName}`);
  return trigger;
}

/** Enter through the visible dial before simulating native slider input. */
export function openSynthRange(root: HTMLElement): HTMLInputElement {
  const trigger = root.querySelector<HTMLButtonElement>(".synth-param-button");
  if (trigger && trigger.getAttribute("aria-expanded") !== "true") fireEvent.click(trigger);
  return synthRangeInput(root);
}

/** jsdom has no native PointerEvent constructor; retain real mouse fields and pointer identity. */
export function synthPointer(target: Element | Window | Document, type: string, pointerId: number, clientY: number, extra: MouseEventInit = {}) {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, clientY, ...extra });
  Object.defineProperties(event, { pointerId: { value: pointerId }, isPrimary: { value: true } });
  fireEvent(target, event);
  return event;
}
