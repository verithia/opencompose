import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";

import type { ContextMenuItem } from "../types/contextMenu";
export type { ContextMenuItem } from "../types/contextMenu";

export interface ContextMenuOptions {
  x: number;
  y: number;
  items: ContextMenuItem[];
  className?: string;
  ariaLabel?: string;
}

interface OpenMenu extends ContextMenuOptions {
  opener: HTMLElement | null;
  id: number;
}

interface MenuPosition {
  id: number;
  x: number;
  y: number;
}

const MARGIN = 8;
let nextMenuId = 0;
const [openMenu, setOpenMenu] = createSignal<OpenMenu | null>(null);

export function openContextMenu(options: ContextMenuOptions) {
  const active = document.activeElement;
  const opener = active instanceof HTMLElement && active.closest(".context-menu")
    ? openMenu()?.opener ?? null
    : active instanceof HTMLElement ? active : null;
  setOpenMenu({ ...options, opener, id: ++nextMenuId });
}

export function closeContextMenu(restoreFocus = true) {
  const opener = openMenu()?.opener;
  const ownedFocus = Boolean(document.activeElement?.closest(".context-menu"));
  setOpenMenu(null);
  // Restore synchronously, before an action focuses a new dialog. Outside
  // clicks and window blur must leave their destination in charge of focus.
  if (restoreFocus && ownedFocus && opener?.isConnected) opener.focus({ preventScroll: true });
}

export function isContextMenuOpen(): boolean {
  return openMenu() !== null;
}

/** Clamp a menu rectangle to the viewport, flipping back over the pointer on overflow. */
export function placeContextMenu(
  point: { x: number; y: number },
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  margin = MARGIN,
): { x: number; y: number } {
  const maxX = Math.max(margin, viewport.width - margin - size.width);
  const maxY = Math.max(margin, viewport.height - margin - size.height);
  return {
    x: Math.max(margin, Math.min(maxX, point.x)),
    y: Math.max(margin, Math.min(maxY, point.y)),
  };
}

type ActionItem = Extract<ContextMenuItem, { onSelect: () => void | Promise<void> }>;

function isSelectable(item: ContextMenuItem | undefined): item is ActionItem {
  return Boolean(item && "onSelect" in item && !item.disabled);
}

/** A group child is always an action: a nested group or a slider is not a row. */
function isActionItem(item: ContextMenuItem): item is ActionItem {
  return "onSelect" in item;
}

/** The colour row: chips that set a colour at once, plus a picker button. */
function ContextMenuPalette(props: { item: Extract<ContextMenuItem, { type: "palette" }> }) {
  let picker: HTMLInputElement | undefined;
  return (
    <div class="context-menu-palette" role="group" aria-label={props.item.label}>
      <div class="context-menu-palette-row">
        <span class="context-menu-palette-label">{props.item.label}</span>
        <div class="context-menu-palette-chips">
          <For each={props.item.colours}>
            {(colour) => (
              <button
                type="button"
                class="context-menu-chip"
                classList={{ current: props.item.value === colour.hex }}
                style={{ background: colour.hex }}
                aria-label={colour.label}
                aria-pressed={props.item.value === colour.hex}
                {...useTooltip(() => colour.label)}
                onClick={() => props.item.onPick(colour.hex)}
              />
            )}
          </For>
        </div>
      </div>
      <Show when={props.item.onCustom || props.item.onReset}>
        <div class="context-menu-palette-actions">
          <Show when={props.item.onCustom != null}>
            <button
                type="button"
                class="context-menu-palette-button"
                onClick={() => picker?.click()}
              >
                <Show when={props.item.value}>
                  {(value) => (
                    <span
                      class="context-menu-palette-preview"
                      aria-hidden="true"
                      style={{ background: value() }}
                    />
                  )}
                </Show>
                {i18n.t("menu.colour.custom")}
              </button>
          </Show>
          <Show when={props.item.onReset}>
            {(reset) => (
              <button
                type="button"
                class="context-menu-palette-button"
                disabled={props.item.resetDisabled}
                onClick={() => reset()()}
              >{props.item.resetLabel}</button>
            )}
          </Show>
        </div>
      </Show>
      <input
        ref={picker}
        type="color"
        class="context-menu-palette-input"
        value={props.item.value ?? "#6bc0e8"}
        aria-label={i18n.t("menu.colour.custom")}
        onChange={(event) => props.item.onCustom?.(event.currentTarget.value)}
      />
    </div>
  );
}

/** A range control inside a menu: live readout, one commit on release. */
function ContextMenuSlider(props: {
  item: Extract<ContextMenuItem, { type: "slider" }>;
}) {
  const [value, setValue] = createSignal(props.item.value);
  createEffect(() => setValue(props.item.value));
  const shown = () => props.item.format?.(value()) ?? String(value());
  const fill = () => {
    const span = props.item.max - props.item.min;
    if (span <= 0) return "0%";
    return `${Math.max(0, Math.min(100, ((value() - props.item.min) / span) * 100))}%`;
  };
  return (
    <div class="context-menu-slider" role="group" aria-label={props.item.label}>
      <div class="context-menu-slider-head">
        <span>{props.item.label}</span>
        <output>{shown()}</output>
      </div>
      <input
        type="range"
        min={props.item.min}
        max={props.item.max}
        step={props.item.step}
        value={value()}
        disabled={props.item.disabled}
        style={{ "--slider-fill": fill() }}
        aria-label={props.item.label}
        onInput={(event) => setValue(Number(event.currentTarget.value))}
        onChange={(event) => props.item.onCommit(Number(event.currentTarget.value))}
      />
    </div>
  );
}

/** Single global context-menu layer; mount once near the app root. */
export function ContextMenuLayer(): JSX.Element {
  let element: HTMLDivElement | undefined;
  const [position, setPosition] = createSignal<MenuPosition | null>(null);
  const [activeKey, setActiveKey] = createSignal<string | null>(null);

  const items = () => openMenu()?.items ?? [];

  /** Expanded groups, by top-level index; a group starts closed. */
  const [expanded, setExpanded] = createSignal<Set<number>>(new Set());

  interface Row {
    /** Stable identity for focus and aria-activedescendant. */
    key: string;
    item: ContextMenuItem;
    depth: number;
    /** Top-level index of the group this row belongs to. */
    top: number;
  }

  /** The rows a reader can reach: top-level entries plus the children of any
   * expanded group. */
  const rows = createMemo<Row[]>(() => {
    const open = expanded();
    const out: Row[] = [];
    items().forEach((item, index) => {
      out.push({ key: `i${index}`, item, depth: 0, top: index });
      if (item.type === "group" && open.has(index)) {
        item.items.forEach((child, childIndex) => {
          out.push({ key: `i${index}c${childIndex}`, item: child, depth: 1, top: index });
        });
      }
    });
    return out;
  });

  const rowSelectable = (row: Row) =>
    row.item.type === "group" ? !row.item.disabled : isSelectable(row.item);

  const selectableKeys = () => rows().filter(rowSelectable).map((row) => row.key);

  const firstSelectable = () => selectableKeys()[0] ?? null;

  const moveActive = (delta: number) => {
    const keys = selectableKeys();
    if (keys.length === 0) return;
    const current = keys.indexOf(activeKey() ?? "");
    const next = current < 0
      ? (delta > 0 ? 0 : keys.length - 1)
      : (current + delta + keys.length) % keys.length;
    setActiveKey(keys[next]);
  };

  const toggleGroup = (top: number) => {
    setExpanded((open) => {
      const next = new Set(open);
      if (next.has(top)) next.delete(top);
      else next.add(top);
      return next;
    });
    setActiveKey(`i${top}`);
  };

  const activateRow = (row: Row | undefined) => {
    if (!row || !rowSelectable(row)) return;
    if (row.item.type === "group") {
      toggleGroup(row.top);
      return;
    }
    if (!isSelectable(row.item)) return;
    closeContextMenu();
    void row.item.onSelect();
  };

  // A new menu starts with every group closed. This runs before the effect
  // below reads the row list, so the two never write and read each other.
  createEffect(on(() => openMenu()?.id, () => setExpanded(new Set<number>())));

  createEffect(() => {
    const state = openMenu();
    setActiveKey(state ? firstSelectable() : null);
    if (!state) {
      setPosition(null);
      return;
    }
    setPosition(null);
    queueMicrotask(() => {
      if (!element || openMenu()?.id !== state.id) return;
      const rect = element.getBoundingClientRect();
      const viewport = {
        width: window.innerWidth || 1024,
        height: window.innerHeight || 768,
      };
      setPosition({ id: state.id, ...placeContextMenu(state, rect, viewport) });
      element.focus({ preventScroll: true });
    });

    const onPointerDown = (event: PointerEvent) => {
      if (element && event.target instanceof Node && element.contains(event.target)) return;
      closeContextMenu(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
        const selection = window.getSelection();
        if (selection?.toString() && selection.anchorNode && element?.contains(selection.anchorNode)) {
          event.stopPropagation(); // Let native copy keep the selected provenance text.
          return;
        }
      }
      if (event.target instanceof HTMLInputElement && event.key !== "Escape") return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeContextMenu();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        event.stopPropagation();
        moveActive(1);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        event.stopPropagation();
        moveActive(-1);
      } else if (event.key === "Home") {
        event.preventDefault();
        event.stopPropagation();
        setActiveKey(firstSelectable());
      } else if (event.key === "End") {
        event.preventDefault();
        event.stopPropagation();
        setActiveKey(selectableKeys().at(-1) ?? null);
      } else if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
        event.preventDefault();
        event.stopPropagation();
        const row = rows().find((entry) => entry.key === activeKey());
        if (row?.item.type === "group") {
          setExpanded((open) => {
            const next = new Set(open);
            if (event.key === "ArrowRight") next.add(row.top);
            else next.delete(row.top);
            return next;
          });
        }
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        event.stopPropagation();
        activateRow(rows().find((entry) => entry.key === activeKey()));
      } else if (event.key === "Tab") {
        closeContextMenu();
      } else {
        // The menu is a keyboard boundary: an unhandled key must not reach the
        // document shortcuts behind it, where Delete would act on whatever was
        // selected before the right-click.
        event.stopPropagation();
      }
    };
    const close = () => closeContextMenu(false);

    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    onCleanup(() => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    });
  });

  return (
    <Portal>
      <Show when={openMenu()}>
        {(state) => {
          const placed = () => (position()?.id === state().id ? position() : null);
          return (
            <div
              ref={(el) => { element = el; }}
              class={`pianoroll-context-menu context-menu ${state().className ?? ""}`}
              role="menu"
              aria-label={state().ariaLabel ?? i18n.t("menu.contextDefault")}
              aria-activedescendant={activeKey() ? `oc-context-row-${activeKey()}` : undefined}
              tabIndex={-1}
              data-menu={state().id}
              style={{
                position: "fixed",
                left: `${placed()?.x ?? state().x}px`,
                top: `${placed()?.y ?? state().y}px`,
                "z-index": "1000",
              }}
              onContextMenu={(event) => event.preventDefault()}
            >
              <For each={state().items}>
                {(item, index) => {
                  const top = index();
                  const rowKey = `i${top}`;
                  return item.type === "separator" ? (
                    <div class="context-menu-divider" role="separator" />
                  ) : item.type === "heading" ? (
                    <div class="context-menu-heading" role="presentation">{item.label}</div>
                  ) : item.type === "info" ? (
                    <dl class="context-menu-info" role="note">
                      <For each={item.rows}>{row => <div><dt>{row.label}</dt><dd classList={{ "context-menu-path": row.path }}>{row.value}</dd></div>}</For>
                    </dl>
                  ) : item.type === "slider" ? (
                    <ContextMenuSlider item={item} />
                  ) : item.type === "palette" ? (
                    <ContextMenuPalette item={item} />
                  ) : item.type === "group" ? (
                    <>
                      <button
                        type="button"
                        role="menuitem"
                        id={`oc-context-row-${rowKey}`}
                        class="context-menu-item context-menu-group"
                        classList={{ "context-menu-active": activeKey() === rowKey }}
                        aria-disabled={item.disabled ? "true" : undefined}
                        aria-expanded={expanded().has(top) ? "true" : "false"}
                        tabIndex={-1}
                        onMouseEnter={() => setActiveKey(rowKey)}
                        onClick={() => toggleGroup(top)}
                      >
                        <span class="context-menu-group-caret" aria-hidden="true">
                          {expanded().has(top) ? "\u25BE" : "\u25B8"}
                        </span>
                        <span class="context-menu-label">{item.label}</span>
                      </button>
                      <Show when={expanded().has(top)}>
                        <For each={item.items.filter(isActionItem)}>
                          {(child, childIndex) => {
                            const childKey = `i${top}c${childIndex()}`;
                            return (
                              <button
                                type="button"
                                role={child.checked === undefined ? "menuitem" : child.checkType === "checkbox" ? "menuitemcheckbox" : "menuitemradio"}
                                id={`oc-context-row-${childKey}`}
                                class="context-menu-item context-menu-subitem"
                                classList={{
                                  "context-menu-danger": Boolean(child.danger),
                                  "context-menu-active": activeKey() === childKey,
                                }}
                                aria-disabled={child.disabled ? "true" : undefined}
                                aria-checked={child.checked === undefined ? undefined : child.checked}
                                tabIndex={-1}
                                onMouseEnter={() => setActiveKey(childKey)}
                                onClick={() => {
                                  if (child.disabled) return;
                                  closeContextMenu();
                                  void child.onSelect();
                                }}
                              >
                                <Show when={child.checked !== undefined}>
                                  <span class="context-menu-check" aria-hidden="true">{child.checked ? "\u2713" : ""}</span>
                                </Show>
                                <span class="context-menu-label">{child.label}</span>
                                <Show when={child.hint}>
                                  <span class="context-menu-hint">{child.hint}</span>
                                </Show>
                                <Show when={child.shortcut}>
                                  <span class="context-menu-shortcut">{child.shortcut}</span>
                                </Show>
                              </button>
                            );
                          }}
                        </For>
                      </Show>
                    </>
                  ) : (
                    <button
                      type="button"
                      role={item.checked === undefined ? "menuitem" : item.checkType === "checkbox" ? "menuitemcheckbox" : "menuitemradio"}
                      id={`oc-context-row-${rowKey}`}
                      class="context-menu-item"
                      classList={{
                        "context-menu-danger": Boolean(item.danger),
                        "context-menu-active": activeKey() === rowKey,
                      }}
                      aria-disabled={item.disabled ? "true" : undefined}
                      aria-checked={item.checked === undefined ? undefined : item.checked}
                      tabIndex={-1}
                      onMouseEnter={() => setActiveKey(rowKey)}
                      onClick={() => activateRow(rows().find((entry) => entry.key === rowKey))}
                    >
                      <Show when={item.checked !== undefined}>
                        <span class="context-menu-check" aria-hidden="true">{item.checked ? "\u2713" : ""}</span>
                      </Show>
                      <span class="context-menu-label">{item.label}</span>
                      <Show when={item.hint}>
                        <span class="context-menu-hint">{item.hint}</span>
                      </Show>
                      <Show when={item.shortcut}>
                        <span class="context-menu-shortcut">{item.shortcut}</span>
                      </Show>
                    </button>
                  );
                }}
              </For>
            </div>
          );
        }}
      </Show>
    </Portal>
  );
}
