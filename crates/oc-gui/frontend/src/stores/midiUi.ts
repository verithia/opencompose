/**
 * Dialog requests for MIDI import/export, shared by the toolbar and track menu.
 */
import { createSignal } from "solid-js";
import { store } from "./project";
import { i18n } from "./i18n";

export interface MidiImportRequest {
  path: string;
  preferredTrack: number | null;
}

export interface MidiExportRequest {
  trackIndex: number | null;
}

const [importRequest, setImportRequest] = createSignal<MidiImportRequest | null>(null);
const [exportRequest, setExportRequest] = createSignal<MidiExportRequest | null>(null);

export async function openMidiImportDialog(preferredTrack: number | null = null) {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [{ name: "MIDI", extensions: ["mid", "midi"] }],
    });
    if (typeof path === "string" && path) setImportRequest({ path, preferredTrack });
  } catch (error) {
    store.setError(i18n.t("midi.import.pickFailed", { error: String(error) }));
  }
}

export function openMidiExportDialog(trackIndex: number | null = null) {
  setExportRequest({ trackIndex });
}

export const midiDialogStore = {
  importRequest,
  exportRequest,
  closeImport: () => setImportRequest(null),
  closeExport: () => setExportRequest(null),
};
