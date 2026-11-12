import { Show, type JSX } from "solid-js";
import { panelStore, PANEL_LABELS, type PanelId } from "../stores/panels";
import { useTooltip } from "./Tooltip";
import { beginPointerDrag } from "../utils/pointerDrag";

interface FloatingPanelProps {
  id: PanelId;
  mode: "floating";
  x: number;
  y: number;
  width: number;
  height: number;
  minimized: boolean;
  zIndex: number;
}

/**
 * A floating, draggable, resizable panel.
 * Title bar: drag to move, double-click to re-dock, minimize button.
 * Bottom-right corner: drag to resize.
 */
export function FloatingPanel(props: {
  panel: FloatingPanelProps;
  children: JSX.Element;
}) {
  const p = () => props.panel;

  // Drag move handler
  const handleTitlePointerDown = (e: PointerEvent & { currentTarget: HTMLDivElement }) => {
    if (e.button !== 0) return;
    e.preventDefault();
    panelStore.bringToFront(p().id);

    const startX = e.clientX - p().x;
    const startY = e.clientY - p().y;

    // The store clamps the position, so the title bar stays reachable.
    beginPointerDrag({
      element: e.currentTarget,
      pointerId: e.pointerId,
      cursor: "move",
      onMove: (clientX, clientY) => {
        panelStore.movePanel(p().id, clientX - startX, clientY - startY);
      },
    });
  };

  // Resize handler (bottom-right corner)
  const handleResizePointerDown = (e: PointerEvent & { currentTarget: HTMLDivElement }) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    panelStore.bringToFront(p().id);

    const startX = e.clientX;
    const startY = e.clientY;
    const startW = p().width;
    const startH = p().height;

    beginPointerDrag({
      element: e.currentTarget,
      pointerId: e.pointerId,
      cursor: "nwse-resize",
      onMove: (clientX, clientY) => {
        panelStore.resizePanel(
          p().id,
          startW + (clientX - startX),
          startH + (clientY - startY),
        );
      },
    });
  };

  // Double-click title to re-dock
  const handleTitleDblClick = () => {
    panelStore.dockPanel(p().id);
  };

  return (
    <div
      class="floating-panel"
      style={{
        position: "absolute",
        left: `${p().x}px`,
        top: `${p().y}px`,
        width: `${p().width}px`,
        height: p().minimized ? "28px" : `${p().height}px`,
        "z-index": p().zIndex,
      }}
      onMouseDown={() => panelStore.bringToFront(p().id)}
    >
      {/* Title bar */}
      <div
        class="floating-panel-title"
        onPointerDown={handleTitlePointerDown}
        onDblClick={handleTitleDblClick}
      >
        <span class="floating-panel-label">{PANEL_LABELS[p().id as PanelId] ?? p().id}</span>
        <div class="floating-panel-btns">
          <button
            class="floating-panel-btn"
            onClick={(e) => { e.stopPropagation(); panelStore.toggleMinimize(p().id); }}
            {...useTooltip(() => p().minimized ? "Expand" : "Minimize")}
          >
            {p().minimized ? "+" : "\u2013"}
          </button>
          <button
            class="floating-panel-btn"
            onClick={(e) => { e.stopPropagation(); panelStore.dockPanel(p().id); }}
            {...useTooltip(() => "Dock back to tabs")}
          >
            &#x2B73;
          </button>
        </div>
      </div>

      {/* Content */}
      <Show when={!p().minimized}>
        <div class="floating-panel-content">
          {props.children}
        </div>

        {/* Resize handle */}
        <div
          class="floating-panel-resize"
          onPointerDown={handleResizePointerDown}
        />
      </Show>
    </div>
  );
}
