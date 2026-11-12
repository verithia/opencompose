/* @refresh reload */
import { render } from "solid-js/web";
import "../src/styles/daw.css";
import "../src/styles/catppuccin.css";
import "../src/styles/audio-io.css";
import "../src/styles/midi.css";
import "../src/styles/export-audio.css";
import "../src/styles/preferences.css";
import App from "./App";
import { i18n } from "./stores/i18n";
import { themeStore } from "./stores/theme";
import { applyPlatform } from "./stores/platform";

applyPlatform();
themeStore.initTheme();
i18n.initLocale();

// The app draws its own menus. The webview's native one (Reload / Inspect
// Element) is always suppressed, including the Vite-served frontend, because it
// would hide the app's own menus.
document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
});

/// Copy the selection from the interface itself.
///
/// A webview that never receives Mod+C leaves every surface unable to copy, so
/// the key is taken in the capture phase and the text is written directly.
async function copySelection(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    try {
      document.execCommand?.("copy");
    } catch {
      // Nothing else can write the clipboard; the selection stays as it was.
    }
  }
}

document.addEventListener(
  "keydown",
  (event) => {
    if (!(event.metaKey || event.ctrlKey) || event.shiftKey || event.altKey) return;
    if (event.key.toLowerCase() !== "c") return;
    const selected = document.getSelection()?.toString() ?? "";
    if (selected.length === 0) return;
    event.preventDefault();
    event.stopPropagation();
    void copySelection(selected);
  },
  true,
);

const root = document.getElementById("root");
render(() => <App />, root!);
