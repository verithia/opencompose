/**
 * The Keymap page: the shortcut editor surface, embedded rather than opened as
 * a second dialog. Search, per-row reset and restore-defaults all come with it.
 */
import { i18n } from "../../stores/i18n";
import { ShortcutEditorPanel } from "../ShortcutEditor";

export function KeymapPage() {
  return (
    <>
      <p class="preferences-hint">{i18n.t("preferences.keymap.hint")}</p>
      <ShortcutEditorPanel />
    </>
  );
}
