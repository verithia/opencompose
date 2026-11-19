/**
 * Starter sound packs from the signed catalog and the one install that may run
 * at a time. The state lives at module level so an install keeps going, and
 * keeps its result, when the guide moves to another step or closes.
 */
import { createSignal } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import * as api from "./api";
import type { SoundPackItem, SoundPackProgress } from "./api";
import { localScan } from "./localScan";

export type PackInstallState =
  | { status: "installing"; progress: SoundPackProgress | null; cancelError: string | null }
  | { status: "installed" }
  | { status: "failed"; error: string }
  | { status: "cancelled" };

const [items, setItems] = createSignal<SoundPackItem[]>([]);
const [loaded, setLoaded] = createSignal(false);
const [states, setStates] = createSignal<Record<string, PackInstallState>>({});
const [active, setActive] = createSignal<string | null>(null);

/** A reset drops every reply from the runs before it. */
let generation = 0;
/** Ids whose cancel the user asked for, so the rejection that follows reads as a cancel. */
const cancelling = new Set<string>();

const setState = (id: string, state: PackInstallState) =>
  setStates((previous) => ({ ...previous, [id]: state }));

async function load(): Promise<void> {
  const run = generation;
  try {
    const listing = await api.soundPacksList();
    if (run !== generation) return;
    setItems(Array.isArray(listing?.items) ? listing.items : []);
  } catch {
    // An older catalog or a failed read leaves only the add-folder action.
    if (run === generation) setItems([]);
  } finally {
    if (run === generation) setLoaded(true);
  }
}

async function install(id: string): Promise<void> {
  if (active() !== null) return;
  const run = generation;
  cancelling.delete(id);
  setActive(id);
  setState(id, { status: "installing", progress: null, cancelError: null });
  let stop: (() => void) | undefined;
  try {
    stop = await listen<SoundPackProgress>(api.SOUND_PACK_PROGRESS_EVENT, (event) => {
      const payload = event.payload;
      if (run !== generation || payload?.id !== id) return;
      const current = states()[id];
      if (current?.status !== "installing") return;
      setState(id, { ...current, progress: payload });
    });
  } catch {
    // Without events the card shows an indeterminate bar until the install returns.
  }
  try {
    await api.soundPackInstall(id);
    if (run !== generation) return;
    setState(id, { status: "installed" });
    // The pack's folder is searched from now on; a fresh count shows it.
    void localScan.start();
  } catch (cause) {
    if (run !== generation) return;
    setState(id, cancelling.has(id) ? { status: "cancelled" } : { status: "failed", error: String(cause) });
  } finally {
    stop?.();
    cancelling.delete(id);
    if (run === generation) setActive(null);
  }
}

async function cancel(id: string): Promise<void> {
  if (active() !== id) return;
  cancelling.add(id);
  try {
    await api.soundPackCancel(id);
  } catch (cause) {
    cancelling.delete(id);
    const current = states()[id];
    if (current?.status === "installing") setState(id, { ...current, cancelError: String(cause) });
  }
}

export const soundPacks = {
  /** Catalog rows, in catalog order; empty until `load` answers or when it fails. */
  items,
  loaded,
  /** The pack being installed, if any. */
  active,
  stateOf: (id: string): PackInstallState | undefined => states()[id],
  load,
  install,
  cancel,
  /** Forget everything; tests and a fresh guide use it. */
  reset: () => {
    generation += 1;
    cancelling.clear();
    setItems([]);
    setLoaded(false);
    setStates({});
    setActive(null);
  },
};
