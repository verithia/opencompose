/**
 * The two command surfaces that sit above the editor: the palette and the
 * shortcut editor. Their open state is shared so a command, the toolbar and a
 * keyboard binding can all reach them.
 */
import { createSignal } from "solid-js";

const [paletteOpen, setPaletteOpen] = createSignal(false);
const [shortcutEditorOpen, setShortcutEditorOpen] = createSignal(false);

function openPalette() {
  setShortcutEditorOpen(false);
  setPaletteOpen(true);
}

function closePalette() {
  setPaletteOpen(false);
}

function openShortcutEditor() {
  setPaletteOpen(false);
  setShortcutEditorOpen(true);
}

function closeShortcutEditor() {
  setShortcutEditorOpen(false);
}

export const overlayStore = {
  paletteOpen,
  openPalette,
  closePalette,
  shortcutEditorOpen,
  openShortcutEditor,
  closeShortcutEditor,
};
