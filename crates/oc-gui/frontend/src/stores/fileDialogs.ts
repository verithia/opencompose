/** Native project file dialogs shared by the toolbar, the menus and commands. */
import { store } from "./project";
import { importAlignedStems } from "./api";
import { i18n } from "./i18n";

let stemImportPending = false;

export async function openAlignedStemsDialog() {
  if (stemImportPending) return;
  if (!store.filePath() || store.project.tracks.length || store.project.sections?.length) {
    store.setError(i18n.t("handoff.destination"));
    return;
  }
  const revision = store.projectRevision;
  stemImportPending = true;
  try {
    const { ask, message, open } = await import("@tauri-apps/plugin-dialog");
    const paths = await open({
      multiple: true,
      filters: [{ name: "WAV / MIDI", extensions: ["wav", "mid", "midi"] }],
    });
    if (!paths?.length) return;
    const title = i18n.t("command.file.importAlignedStems");
    if (!await ask(i18n.t("handoff.confirm"), { title })) return;
    const imported = await importAlignedStems(paths, revision);
    if (!store.applySampleSnapshot(imported.snapshot)) return;
    store.setSelectedTrackIndex(imported.midi_track_count);
    const summary = i18n.t("handoff.summary", {
      stems: imported.stem_count,
      midi: imported.midi_track_count,
    });
    const origin = imported.timestamped_stem_count === imported.stem_count
      ? i18n.t("handoff.originMatched")
      : i18n.t("handoff.originUnverified");
    const warnings = imported.midi?.warnings ?? [];
    await message([summary, origin, ...warnings.slice(0, 8)].join("\n"), { title });
  } catch (error) {
    store.setError(String(error));
  } finally {
    stemImportPending = false;
  }
}

export async function openFileDialog() {
  // Ask before the picker: Cancel must not even open it.
  if (!await store.confirmDiscardChanges()) return;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const path = await open({
    filters: [{ name: "OpenCompose", extensions: ["ocp"] }],
  });
  if (path) void store.openFile(path as string);
}

/** File ▸ New and the toolbar's New: the same guard as Open. */
export async function newProjectDialog() {
  if (!await store.confirmDiscardChanges()) return;
  await store.newProject();
}

export async function saveFileDialog() {
  if (store.filePath()) {
    return store.saveFile();
  } else {
    return saveAsFileDialog();
  }
}

export async function saveAsFileDialog() {
  try {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: store.filePath() ?? undefined,
      filters: [{ name: "OpenCompose", extensions: ["ocp"] }],
    });
    return path ? await store.saveFile(path) : false;
  } catch (error) {
    store.setError(String(error));
    return false;
  }
}
