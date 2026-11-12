import { For, Show, createEffect, createMemo, createSignal, onCleanup, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { useTooltip } from "./Tooltip";

export type DropdownValue = string | number;

export interface DropdownOption<T extends DropdownValue = DropdownValue> {
  value: T;
  label: string;
  /** A short state word shown after the label, such as a deprecation badge. */
  badge?: string;
  /** A quiet second line under the label, such as a model's quota. */
  note?: string;
  /** Hover text for the option, such as the date a model is removed. */
  hint?: string;
}

export interface DropdownProps<T extends DropdownValue = DropdownValue> {
  /** Accessible name and the visible label unless `hideLabel` is set. */
  label: string;
  value: T;
  options: readonly DropdownOption<T>[];
  onSelect: (value: T) => void;
  /** Capture control admission when a real pointer, key or AX action opens the menu. */
  onOpen?: () => void;
  /** Release the menu's captured interaction after selection or cancellation. */
  onClose?: () => void;
  /** Stack the label above the trigger instead of beside it. */
  stack?: boolean;
  /** Keep the label as the accessible name only. */
  hideLabel?: boolean;
  /** Host hook such as `data-param` that tests and automation read. */
  dataParam?: string;
  /** Extra data attributes for the trigger, for hosts with their own hooks. */
  triggerAttrs?: Record<string, string>;
  /** Extra trigger attributes for host hooks such as a control tooltip. */
  triggerProps?: JSX.HTMLAttributes<HTMLButtonElement>;
  /** Host action such as resetting to the default on double click. */
  onDblClick?: (event: MouseEvent) => void;
  /** A state word the selected row carries, shown on the trigger as well. */
  badge?: string;
  /** One line above the list, when the list is not the one the host expected. */
  notice?: string;
  class?: string;
  disabled?: boolean;
}

export interface DropdownPlacement {
  x: number;
  y: number;
  width: number;
  placement: "above" | "below";
}

const VIEWPORT_MARGIN = 6;
const TRIGGER_GAP = 2;

/** Place the list under the trigger, flipping above when it would overflow. */
export function placeDropdown(
  trigger: { left: number; top: number; width: number; height: number },
  menu: { width: number; height: number },
  viewport: { width: number; height: number },
  margin = VIEWPORT_MARGIN,
): DropdownPlacement {
  const width = Math.min(
    Math.max(menu.width, trigger.width),
    Math.max(0, viewport.width - margin * 2),
  );
  const below = trigger.top + trigger.height + TRIGGER_GAP;
  const above = trigger.top - TRIGGER_GAP - menu.height;
  const fitsBelow = below + menu.height + margin <= viewport.height;
  const placement = !fitsBelow && above >= margin ? "above" : "below";
  const preferred = placement === "above" ? above : below;
  return {
    x: Math.max(margin, Math.min(viewport.width - margin - width, trigger.left)),
    y: Math.max(margin, Math.min(viewport.height - margin - menu.height, preferred)),
    width,
    placement,
  };
}

let nextDropdownId = 0;

/**
 * Accessible listbox dropdown: a button trigger plus a portalled popover list.
 * Focus stays on the listbox itself and moves with aria-activedescendant.
 */
export function Dropdown<T extends DropdownValue>(props: DropdownProps<T>): JSX.Element {
  const baseId = `oc-dropdown-${++nextDropdownId}`;
  const labelId = `${baseId}-label`;
  const listId = `${baseId}-list`;
  const optionId = (index: number) => `${baseId}-option-${index}`;

  const [open, setOpen] = createSignal(false);
  const [active, setActive] = createSignal(-1);
  const [placement, setPlacement] = createSignal<DropdownPlacement | null>(null);
  let trigger: HTMLButtonElement | undefined;
  let menu: HTMLDivElement | undefined;
  let menuOwner: object | null = null;
  let disposed = false;

  // Call sites build option arrays inline; memoize so `For` keeps its nodes.
  const optionList = createMemo(() => props.options);
  const selectedIndex = createMemo(() =>
    optionList().findIndex((option) => String(option.value) === String(props.value))
  );
  const isSelected = (option: DropdownOption<T>) => String(option.value) === String(props.value);
  const selectedLabel = () => optionList()[selectedIndex()]?.label ?? String(props.value);

  const position = () => {
    if (!trigger || !menu) return;
    setPlacement(placeDropdown(
      trigger.getBoundingClientRect(),
      menu.getBoundingClientRect(),
      { width: window.innerWidth || 1024, height: window.innerHeight || 768 },
    ));
  };

  const closeMenu = (restoreFocus = false) => {
    if (disposed || !open()) return;
    setOpen(false);
    menuOwner = null;
    props.onClose?.();
    if (!disposed && restoreFocus) trigger?.focus({ preventScroll: true });
  };

  const openMenu = () => {
    if (disposed || props.disabled || open() || menuOwner !== null) return;
    const owner = {};
    menuOwner = owner;
    props.onOpen?.();
    if (disposed || menuOwner !== owner) return;
    setPlacement(null);
    setActive(Math.max(0, selectedIndex()));
    setOpen(true);
  };

  const selectIndex = (index: number, owner = menuOwner) => {
    const option = optionList()[index];
    if (disposed || !open() || owner === null || owner !== menuOwner || !option) return;
    setOpen(false);
    menuOwner = null;
    trigger?.focus({ preventScroll: true });
    try { if (!disposed) props.onSelect(option.value); }
    finally { if (menuOwner === null) props.onClose?.(); }
  };

  onCleanup(() => {
    disposed = true;
    const hadOwner = menuOwner !== null;
    menuOwner = null;
    setOpen(false);
    if (hadOwner) props.onClose?.();
  });

  const moveActive = (delta: number) => {
    const total = optionList().length;
    if (total === 0) return;
    const current = active();
    const next = current < 0
      ? (delta > 0 ? 0 : total - 1)
      : (current + delta + total) % total;
    setActive(next);
  };

  createEffect(() => {
    if (!open()) return;

    // The list mounts before this effect runs; measure once it is styled.
    queueMicrotask(() => {
      if (!open()) return;
      position();
      menu?.focus({ preventScroll: true });
    });

    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (trigger?.contains(target) || menu?.contains(target)) return;
      closeMenu();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeMenu(true);
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        event.stopPropagation();
        moveActive(event.key === "ArrowDown" ? 1 : -1);
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        event.stopPropagation();
        setActive(event.key === "Home" ? 0 : optionList().length - 1);
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        event.stopPropagation();
        selectIndex(active());
      } else if (event.key === "Tab") {
        closeMenu();
      } else {
        // No typeahead yet; keep app shortcuts from firing behind the list.
        event.stopPropagation();
      }
    };
    const onScroll = () => {
      if (!trigger) return;
      const rect = trigger.getBoundingClientRect();
      if (rect.bottom < 0 || rect.top > (window.innerHeight || 768)) {
        closeMenu();
        return;
      }
      position();
    };
    const onResize = () => position();
    const onWindowBlur = () => closeMenu();

    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    window.addEventListener("blur", onWindowBlur);
    onCleanup(() => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
      window.removeEventListener("blur", onWindowBlur);
    });
  });

  createEffect(() => {
    if (!open()) return;
    const index = active();
    if (index < 0 || !menu) return;
    const option = menu.querySelector<HTMLElement>(`[data-option-index="${index}"]`);
    if (option && typeof option.scrollIntoView === "function") {
      option.scrollIntoView({ block: "nearest" });
    }
  });

  return (
    <div
      class={`oc-dropdown${props.stack ? " oc-dropdown-stack" : ""}${props.hideLabel ? " oc-dropdown-bare" : ""}${props.class ? ` ${props.class}` : ""}`}
      data-param={props.dataParam}
    >
      <Show when={!props.hideLabel}>
        <span class="oc-dropdown-label" id={labelId}>{props.label}</span>
      </Show>
      <button
        ref={(element) => { trigger = element; }}
        type="button"
        class="oc-dropdown-trigger"
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-controls={open() ? listId : undefined}
        aria-labelledby={props.hideLabel ? undefined : labelId}
        aria-label={props.hideLabel ? props.label : undefined}
        disabled={props.disabled}
        {...props.triggerAttrs}
        {...props.triggerProps}
        onClick={() => (open() ? closeMenu() : openMenu())}
        onDblClick={props.onDblClick}
        on:keydown={(event) => {
          if (open()) return;
          if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            event.stopPropagation();
            openMenu();
          }
        }}
      >
        <span class="oc-dropdown-value">{selectedLabel()}</span>
        <Show when={props.badge}>
          <span class="oc-dropdown-badge">{props.badge}</span>
        </Show>
        <span class="oc-dropdown-chevron" aria-hidden="true">{"\u25BE"}</span>
      </button>
      <Show when={open()}>
        <Portal>
          <div
            ref={(element) => { menu = element; }}
            id={listId}
            class="oc-dropdown-menu"
            role="listbox"
            aria-labelledby={props.hideLabel ? undefined : labelId}
            aria-label={props.hideLabel ? props.label : undefined}
            aria-activedescendant={active() >= 0 ? optionId(active()) : undefined}
            tabIndex={-1}
            data-placement={placement()?.placement ?? "below"}
            style={{
              position: "fixed",
              left: `${placement()?.x ?? 0}px`,
              top: `${placement()?.y ?? 0}px`,
              width: placement() ? `${placement()!.width}px` : undefined,
              visibility: placement() ? "visible" : "hidden",
            }}
          >
            {/* The list says why it is not the one the host expected, so a
                fallback roster is never silently swapped in. */}
            <Show when={props.notice}>
              <div class="oc-dropdown-notice" role="note">{props.notice}</div>
            </Show>
            <For each={optionList()}>
              {(option, index) => {
                const owner = menuOwner;
                const tip = useTooltip(() => option.hint);
                return (
                  <button
                    type="button"
                    role="option"
                    id={optionId(index())}
                    data-option-index={index()}
                    data-value={String(option.value)}
                    class="oc-dropdown-option"
                    classList={{
                      "oc-dropdown-active": active() === index(),
                      "oc-dropdown-has-note": !!option.note,
                    }}
                    aria-selected={isSelected(option)}
                    aria-describedby={option.hint ? tip["aria-describedby"] : undefined}
                    tabIndex={-1}
                    onMouseEnter={(event) => { setActive(index()); tip.onMouseEnter(event); }}
                    onMouseLeave={tip.onMouseLeave}
                    onFocusIn={tip.onFocusIn}
                    onFocusOut={tip.onFocusOut}
                    onClick={() => selectIndex(index(), owner)}
                  >
                    <span class="oc-dropdown-check" aria-hidden="true">{isSelected(option) ? "\u2713" : ""}</span>
                    <span class="oc-dropdown-option-label">{option.label}</span>
                    <Show when={option.badge}>
                      <span class="oc-dropdown-badge">{option.badge}</span>
                    </Show>
                    <Show when={option.note}>
                      <span class="oc-dropdown-option-note">{option.note}</span>
                    </Show>
                  </button>
                );
              }}
            </For>
          </div>
        </Portal>
      </Show>
    </div>
  );
}
