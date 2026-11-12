/**
 * Recovery-copy state: the list the File menu and the dialogs read, and the
 * pending flags for checking, restoring and discarding.
 *
 * Restoring the project belongs to the project store (it applies the snapshot);
 * this store only tracks what the dialog shows.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import { i18n } from "./i18n";
import type { RecoveryEntry } from "../types/project";

const [entries, setEntries] = createSignal<RecoveryEntry[]>([]);
const [open, setOpen] = createSignal(false);
const [promptOpen, setPromptOpen] = createSignal(false);
const [checking, setChecking] = createSignal(false);
const [restoring, setRestoring] = createSignal(false);
const [discarding, setDiscarding] = createSignal<string | null>(null);
const [error, setError] = createSignal<string | null>(null);
/** The startup prompt is shown at most once per run. */
let prompted = false;

/** Human-readable time for a copy; the row and the prompt share it. */
export function formatSavedAt(savedAtMs: number): string {
  return new Date(savedAtMs).toLocaleString();
}

/**
 * Why Restore is disabled for one row, or null when it can be restored.
 * A previous version is restorable whenever its bytes parse, even though it is
 * older than the file; an unreadable copy carries its own reason from the
 * backend, so a parse error reaches the row.
 */
export function restoreDisabledReason(entry: RecoveryEntry): string | null {
  if (entry.kind === "backup") return entry.restore_disabled_reason;
  if (entry.restore_disabled_reason) {
    return entry.newer_than_disk
      ? i18n.t("recovery.unreadable")
      : i18n.t("recovery.older");
  }
  return entry.newer_than_disk ? null : i18n.t("recovery.older");
}

async function refresh(): Promise<RecoveryEntry[]> {
  try {
    const reply = await api.listRecoveries();
    // A test double or an older backend may answer with a snapshot; no list
    // means no recoverable copies, never a crash in the file menu.
    const list = Array.isArray(reply) ? reply : [];
    setEntries(list);
    return list;
  } catch (cause) {
    setError(String(cause));
    return [];
  }
}

function newer(): RecoveryEntry | null {
  return entries().find((entry) => entry.newer_than_disk) ?? null;
}

/**
 * The row the File menu opens the dialog for: a previous version exists even
 * when no autosave copy is newer than the file.
 */
function available(): RecoveryEntry | null {
  return entries().find((entry) => entry.kind === "backup") ?? newer();
}

async function openDialog() {
  setOpen(true);
  setPromptOpen(false);
  setError(null);
  setChecking(true);
  await refresh();
  setChecking(false);
}

/** Called after the startup project loads; never prompts twice. */
async function checkOnStartup() {
  await refresh();
  if (prompted) return;
  if (!newer()) return;
  prompted = true;
  setPromptOpen(true);
}

/**
 * Called after every successful load: refresh and offer any copy newer than the
 * file on disk. `restored` names the copy a restore just applied, so it is not
 * offered again.
 */
async function checkAfterLoad(restored?: string) {
  await refresh();
  const offer = entries().find(
    (entry) => entry.newer_than_disk && entry.path !== restored,
  );
  if (!offer) return;
  setPromptOpen(true);
}

function close() {
  setOpen(false);
  setPromptOpen(false);
}

async function discard(entry: RecoveryEntry) {
  setError(null);
  setDiscarding(entry.path);
  try {
    await api.discardRecovery(entry.path);
    await refresh();
  } catch (cause) {
    setError(String(cause));
  } finally {
    setDiscarding(null);
  }
}

function beginRestore() {
  setError(null);
  setRestoring(true);
}

function endRestore() {
  setRestoring(false);
}

export const recoveryStore = {
  entries,
  open,
  promptOpen,
  checking,
  restoring,
  discarding,
  error,
  newer,
  available,
  openDialog,
  checkOnStartup,
  checkAfterLoad,
  close,
  refresh,
  discard,
  beginRestore,
  endRestore,
  setError,
};
