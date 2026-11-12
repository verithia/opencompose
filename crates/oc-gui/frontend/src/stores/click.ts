/**
 * The audition click: toggle, level and count-in choice. Shapes mirror
 * `oc_audio::click`; the transport polling keeps it in step with the backend.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

export type CountIn = "off" | "one_bar" | "two_bars";

export interface ClickSettings {
  enabled: boolean;
  level_db: number;
  count_in: CountIn;
}

export interface ClickView extends ClickSettings {
  /** Count-in is offered only while a track is armed for recording. */
  count_in_available: boolean;
}

const DEFAULTS: ClickView = {
  enabled: false,
  level_db: -6,
  count_in: "off",
  count_in_available: false,
};

const [view, setView] = createSignal<ClickView>(DEFAULTS);
const [pending, setPending] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

/** Adopt a transport or click snapshot; ignores one without the fields. */
export function adoptClick(next: Partial<ClickView> | null | undefined): void {
  if (!next || typeof next.enabled !== "boolean" || typeof next.level_db !== "number") return;
  setView({
    enabled: next.enabled,
    level_db: next.level_db,
    count_in: (next.count_in as CountIn) ?? "off",
    count_in_available: next.count_in_available ?? false,
  });
}

async function apply(next: ClickSettings): Promise<ClickView | null> {
  setPending(true);
  try {
    const result = await invoke<ClickView>("set_click", { settings: next });
    adoptClick(result);
    setError(null);
    return result;
  } catch (failure) {
    setError(String(failure));
    return null;
  } finally {
    setPending(false);
  }
}

const settings = () => view();
const enabled = () => view().enabled;
const levelDb = () => view().level_db;
const countIn = () => view().count_in;
const countInAvailable = () => view().count_in_available;

function toSettings(next: Partial<ClickSettings>): ClickSettings {
  const current = view();
  return {
    enabled: next.enabled ?? current.enabled,
    level_db: next.level_db ?? current.level_db,
    count_in: next.count_in ?? current.count_in,
  };
}

async function setEnabled(on: boolean): Promise<ClickView | null> {
  return apply(toSettings({ enabled: on }));
}

async function setLevelDb(db: number): Promise<ClickView | null> {
  if (!Number.isFinite(db)) return null;
  return apply(toSettings({ level_db: Math.max(-60, Math.min(6, db)) }));
}

async function setCountIn(next: CountIn): Promise<ClickView | null> {
  // The menu is disabled until a track is armed; refuse here too.
  if (!view().count_in_available) return null;
  return apply(toSettings({ count_in: next }));
}

function dispose(): void {
  setView(DEFAULTS);
  setPending(false);
  setError(null);
}

export const clickStore = {
  settings,
  enabled,
  levelDb,
  countIn,
  countInAvailable,
  pending,
  error,
  adoptClick,
  setEnabled,
  setLevelDb,
  setCountIn,
  dispose,
};
