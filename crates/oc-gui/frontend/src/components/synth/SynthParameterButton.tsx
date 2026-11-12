import { createEffect, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

export interface SynthParameterButtonProps {
  id?: string;
  label: string;
  valueText: string;
  position: number;
  disabled: boolean;
  editable: boolean;
  decoration?: JSX.Element;
  onStep: (key: string) => void;
  onReset: (event: MouseEvent) => void;
  onClose?: () => void;
  onDragStart?: (event: PointerEvent) => boolean;
  onDragPosition?: (position: number) => void;
  onDragCancel?: (event: PointerEvent) => void;
  children: JSX.Element;
}

let nextParameterId = 0;
let closeActiveParameter: (() => void) | null = null;

/** The native range stays mounted so opening the dial never replaces a gesture target. */
export function SynthParameterButton(props: SynthParameterButtonProps): JSX.Element {
  const popupId = `synth-parameter-popup-${++nextParameterId}`;
  const [open, setOpen] = createSignal(false);
  const [dragging, setDragging] = createSignal(false);
  const [placement, setPlacement] = createSignal({ left: 0, top: 0 });
  let trigger!: HTMLButtonElement;
  let popup!: HTMLDivElement;
  let disposed = false;
  let suppressPointerClick = false;
  let drag: { event: PointerEvent; lastY: number; position: number; detach: () => void; cancel?: (event: PointerEvent) => void } | null = null;
  const canEdit = () => !disposed && !props.disabled && props.editable;
  const retireDrag = (cancelEvent?: PointerEvent) => {
    const owned = drag;
    if (!owned) return;
    drag = null;
    setDragging(false);
    owned.detach();
    try { if (trigger.hasPointerCapture?.(owned.event.pointerId)) trigger.releasePointerCapture(owned.event.pointerId); } catch { /* The native window can already have released capture. */ }
    if (cancelEvent) owned.cancel?.(cancelEvent);
  };

  const close = (restoreFocus = false, interrupt = true) => {
    if (!open()) return;
    if (drag) retireDrag(drag.event);
    setOpen(false);
    if (closeActiveParameter === closeWithoutFocus) closeActiveParameter = null;
    if (interrupt) props.onClose?.();
    if (restoreFocus && !disposed && trigger.isConnected && !props.disabled) trigger.focus();
  };
  const closeWithoutFocus = () => close();
  const place = (side = false) => {
    const bounds = trigger.getBoundingClientRect();
    const width = 64;
    const height = 132;
    const below = bounds.bottom + 4;
    const top = side ? bounds.top + (32 - height) / 2
      : below + height <= window.innerHeight - 6 ? below : bounds.top - height - 4;
    const left = side ? (bounds.right + width + 10 <= window.innerWidth ? bounds.right + 4 : bounds.left - width - 4)
      : bounds.left + (bounds.width - width) / 2;
    setPlacement({
      left: Math.max(6, Math.min(window.innerWidth - width - 6, left)),
      top: Math.max(6, Math.min(window.innerHeight - height - 6, top)),
    });
  };
  const toggle = () => {
    if (!canEdit()) return;
    if (open()) { close(); return; }
    closeActiveParameter?.();
    if (!canEdit()) return;
    closeActiveParameter = closeWithoutFocus;
    place();
    setOpen(true);
    queueMicrotask(() => {
      if (!disposed && open() && !dragging()) popup.querySelector<HTMLInputElement>('input[type="range"]')?.focus();
    });
  };
  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0 || event.ctrlKey || event.metaKey || event.altKey || event.isPrimary === false
      || !canEdit() || drag || !props.onDragStart || !props.onDragPosition) return;
    suppressPointerClick = true;
    if (open()) close();
    if (!canEdit()) return;
    closeActiveParameter?.();
    if (!canEdit()) return;
    const start = props.onDragStart;
    const canceled = props.onDragCancel;
    if (!start || !props.onDragPosition) return;
    // The parent registers its owning release handler first, so it commits
    // before this feedback-only owner retires the pointer interaction.
    if (!start(event)) return;
    if (!canEdit()) { canceled?.(event); return; }
    event.preventDefault();
    trigger.focus();
    if (!canEdit()) { canceled?.(event); return; }
    const pointerId = event.pointerId;
    const move = (next: PointerEvent) => {
      if (drag !== owned || next.pointerId !== pointerId) return;
      const sensitivity = next.shiftKey ? 0.1 : 1;
      drag.position = Math.max(0, Math.min(1, drag.position + (drag.lastY - next.clientY) / 160 * sensitivity));
      drag.lastY = next.clientY;
      props.onDragPosition?.(drag.position);
    };
    const finish = (next: PointerEvent) => {
      if (drag !== owned || next.pointerId !== pointerId) return;
      retireDrag();
      close(true, false);
    };
    const cancel = (next: PointerEvent) => {
      if (drag !== owned || next.pointerId !== pointerId) return;
      retireDrag(next);
      close(true, false);
    };
    const detach = () => {
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", finish, true);
      window.removeEventListener("pointercancel", cancel, true);
      trigger.removeEventListener("lostpointercapture", cancel);
    };
    const owned = { event, lastY: event.clientY, position: props.position, detach, cancel: canceled };
    drag = owned;
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", finish, true);
    window.addEventListener("pointercancel", cancel, true);
    trigger.addEventListener("lostpointercapture", cancel);
    try { trigger.setPointerCapture?.(pointerId); } catch { /* Window listeners retain ownership when capture is unavailable. */ }
    if (drag !== owned) return;
    if (!canEdit()) { retireDrag(event); return; }
    closeActiveParameter = closeWithoutFocus;
    place(true);
    setDragging(true);
    setOpen(true);
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (disposed || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      if (!event.repeat && !dragging()) toggle();
    } else if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      if (canEdit() && !dragging()) props.onStep(event.key);
    }
  };
  const onWheel = (event: WheelEvent) => {
    if (event.ctrlKey || !event.deltaY || !canEdit() || dragging()) return;
    event.preventDefault();
    event.stopPropagation();
    props.onStep(event.deltaY < 0 ? "ArrowUp" : "ArrowDown");
  };
  const onOutsidePointer = (event: PointerEvent) => {
    if (dragging()) return;
    const target = event.target;
    if (target instanceof Node && !trigger.contains(target) && !popup.contains(target)) close();
  };
  const onEscape = (event: KeyboardEvent) => {
    if (event.key !== "Escape" || !open()) return;
    event.preventDefault();
    event.stopPropagation();
    close(true);
  };
  const onFocusOut = (event: FocusEvent) => {
    const target = event.relatedTarget;
    // WebKit can temporarily focus the document while a native range receives
    // a pointer press. Release restores range focus; this is not a Tab exit.
    if (!(target instanceof Node) || target === document.body || target === document.documentElement) return;
    if (target instanceof Node && (trigger.contains(target) || popup.contains(target))) return;
    close();
  };
  onMount(() => {
    trigger.addEventListener("wheel", onWheel, { passive: false });
  });
  createEffect(() => {
    if (props.disabled || !props.editable) close();
  });
  createEffect(() => {
    if (!open()) return;
    document.addEventListener("pointerdown", onOutsidePointer, true);
    document.addEventListener("keydown", onEscape, true);
    window.addEventListener("resize", closeWithoutFocus);
    window.addEventListener("scroll", closeWithoutFocus, true);
    window.addEventListener("blur", closeWithoutFocus);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onOutsidePointer, true);
      document.removeEventListener("keydown", onEscape, true);
      window.removeEventListener("resize", closeWithoutFocus);
      window.removeEventListener("scroll", closeWithoutFocus, true);
      window.removeEventListener("blur", closeWithoutFocus);
    });
  });
  onCleanup(() => {
    disposed = true;
    trigger.removeEventListener("wheel", onWheel);
    close();
  });

  return <div class="synth-param-dial" style={{ "--dial-position": String(Math.max(0, Math.min(1, props.position))) }}>
    <button
      ref={trigger}
      id={props.id}
      type="button"
      class="synth-param-button"
      aria-label={props.label}
      aria-expanded={open()}
      aria-controls={popupId}
      aria-describedby={`${popupId}-value`}
      disabled={props.disabled}
      onPointerDown={onPointerDown}
      onFocusOut={onFocusOut}
      onKeyDown={onKeyDown}
      onKeyUp={(event) => { if (event.key === " " || event.key === "Enter") event.stopPropagation(); }}
      onClick={(event) => {
        if (disposed) return;
        const compatibilityClick = suppressPointerClick && event.detail > 0;
        suppressPointerClick = false;
        if (compatibilityClick) { event.preventDefault(); return; }
        if (!event.ctrlKey && event.button === 0) toggle();
      }}
      onDblClick={(event) => { if (canEdit() && !event.ctrlKey && event.button === 0) props.onReset(event); }}
    >
      <span class="synth-param-dial-face" aria-hidden="true"><span class="synth-param-dial-marker" /></span>
      <span class="synth-param-caption">{props.label}{props.decoration}</span>
      <span id={`${popupId}-value`} class="synth-param-value">{props.valueText}</span>
    </button>
    <Portal>
      <div
        ref={popup}
        id={popupId}
        class="synth-param-popover"
        role="group"
        aria-label={props.label}
        hidden={!open()}
        inert={dragging()}
        aria-hidden={dragging() ? true : undefined}
        data-dragging={dragging() ? "true" : undefined}
        onFocusOut={onFocusOut}
        onKeyDown={(event) => { if (event.key === " " || event.key === "Enter") event.stopPropagation(); }}
        style={{ left: `${placement().left}px`, top: `${placement().top}px` }}
      >
        {props.children}
        <output class="synth-param-popup-value">{props.valueText}</output>
      </div>
    </Portal>
  </div>;
}
