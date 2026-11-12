/** Help navigation shared by the command palette and Preferences. */
import { createSignal } from "solid-js";
import { preferencesUi } from "./preferencesUi";

const [manualOpen, setManualOpen] = createSignal(false);
export const helpUi = {
  manualOpen,
  setManualOpen,
  openManual() {
    setManualOpen(true);
    preferencesUi.openOn("help");
  },
};
