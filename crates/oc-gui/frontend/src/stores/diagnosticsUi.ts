/**
 * The diagnostics archive: one support file the Help page and the app shortcut
 * both produce. It lives in a store so the shortcut keeps working while the
 * preferences surface is closed.
 */
import { createSignal } from "solid-js";
import { i18n } from "./i18n";
import { log } from "./log";
import { exportDiagnostics as exportArchive, revealDiagnostics } from "./api";

const [busy, setBusy] = createSignal(false);
const [status, setStatus] = createSignal("");

/** Ask where to write the archive, then write and reveal it. */
async function exportBundle(): Promise<void> {
  setStatus("");
  setBusy(true);
  try {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const path = await save({
      defaultPath: `opencompose-diagnostics-${stamp}.tar.gz`,
      filters: [{ name: "Diagnostics archive", extensions: ["tar.gz", "tgz"] }],
    });
    if (!path) return;
    const logs = log.entries().map(({ time, level, source, message }) => ({
      time,
      level,
      source,
      message,
    }));
    const saved = await exportArchive(path as string, logs);
    setStatus(i18n.t("toolbar.diagnosticsSaved", { path: saved }));
    try {
      await revealDiagnostics(saved);
    } catch (error) {
      log.warn("diagnostics", `archive written but not revealed: ${String(error)}`);
    }
  } catch (error) {
    setStatus(String(error));
  } finally {
    setBusy(false);
  }
}

function reset(): void {
  setBusy(false);
  setStatus("");
}

export const diagnosticsStore = { busy, status, exportBundle, reset };
