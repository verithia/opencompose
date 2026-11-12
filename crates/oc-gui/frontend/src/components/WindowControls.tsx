import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { i18n } from "../stores/i18n";
import { isWindows } from "../stores/platform";

/** True inside the Tauri webview; false in a plain browser or under jsdom. */
function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Caption buttons for the frameless Windows/Linux window: minimise, maximise
 * or restore, close. Absent on macOS, which keeps its native traffic lights.
 */
export function WindowControls() {
  const [maximized, setMaximized] = createSignal(false);

  onMount(() => {
    if (!inTauri() || !isWindows()) return;
    const win = getCurrentWindow();
    void win.isMaximized().then(setMaximized);
    void win
      .onResized(() => void win.isMaximized().then(setMaximized))
      .then((unlisten) => onCleanup(() => void unlisten()));
  });

  const run = (action: "minimize" | "toggleMaximize" | "close") => {
    void getCurrentWindow()[action]();
  };

  return (
    <Show when={inTauri() && isWindows()}>
      <div class="window-controls">
        <button
          class="window-control"
          aria-label={i18n.t("window.minimize")}
          onClick={() => run("minimize")}
        >
          <svg width="10" height="10" viewBox="0 0 10 10">
            <path d="M1 5.5h8" stroke="currentColor" stroke-width="1" />
          </svg>
        </button>
        <button
          class="window-control"
          aria-label={maximized() ? i18n.t("window.restore") : i18n.t("window.maximize")}
          onClick={() => run("toggleMaximize")}
        >
          <Show
            when={maximized()}
            fallback={
              <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
                <rect x="1.5" y="1.5" width="7" height="7" stroke="currentColor" stroke-width="1" />
              </svg>
            }
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <rect x="1" y="3" width="6" height="6" stroke="currentColor" stroke-width="1" />
              <path d="M3 3V1.5h5.5V7H7" stroke="currentColor" stroke-width="1" />
            </svg>
          </Show>
        </button>
        <button
          class="window-control close"
          aria-label={i18n.t("window.close")}
          onClick={() => run("close")}
        >
          <svg width="10" height="10" viewBox="0 0 10 10">
            <path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" stroke-width="1" />
          </svg>
        </button>
      </div>
    </Show>
  );
}

const EDGES: [string, string][] = [
  ["n", "North"],
  ["s", "South"],
  ["w", "West"],
  ["e", "East"],
  ["nw", "NorthWest"],
  ["ne", "NorthEast"],
  ["sw", "SouthWest"],
  ["se", "SouthEast"],
];

/** Resize grips for a frameless window; no-ops on macOS and in the browser. */
export function WindowResizeEdges() {
  const resize = (edge: string) => {
    void getCurrentWindow().startResizeDragging(
      edge as Parameters<ReturnType<typeof getCurrentWindow>["startResizeDragging"]>[0],
    );
  };

  return (
    <Show when={inTauri() && isWindows()}>
      <div class="window-resize-edges">
        {EDGES.map(([key, direction]) => (
          <div
            class={`window-resize-edge ${key}`}
            onPointerDown={(event) => {
              event.preventDefault();
              resize(direction);
            }}
          />
        ))}
      </div>
    </Show>
  );
}
