/**
 * Network preferences: the proxy every outbound client follows.
 *
 * The backend owns the value (`gui-settings.yaml`); this store is the page's
 * view and the connection probe's result.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

export type ProxyMode = "system" | "manual" | "none";

export interface NetworkSettings {
  proxyMode: ProxyMode;
  proxyUrl: string;
}

export interface ProbeResult {
  ok: boolean;
  status?: number;
  ms: number;
  bytes?: number;
  error?: string;
}

const [settings, setSettings] = createSignal<NetworkSettings>({
  proxyMode: "system",
  proxyUrl: "",
});
const [probe, setProbe] = createSignal<ProbeResult | null>(null);
const [probing, setProbing] = createSignal(false);
const [suggestion, setSuggestion] = createSignal<string | null>(null);
let detected = false;

const DISMISSED_KEY = "network.proxySuggestionDismissed";

async function load(): Promise<void> {
  try {
    setSettings(await invoke<NetworkSettings>("network_settings"));
  } catch {
    // A preview without Tauri keeps the default.
  }
}

async function save(next: NetworkSettings): Promise<void> {
  setSettings(await invoke<NetworkSettings>("network_save", { settings: next }));
}

async function test(mode: ProxyMode, url: string): Promise<void> {
  setProbing(true);
  setProbe(null);
  try {
    setProbe(await invoke<ProbeResult>("network_test", { proxyMode: mode, proxyUrl: url }));
  } catch (error) {
    setProbe({ ok: false, ms: 0, error: String(error) });
  } finally {
    setProbing(false);
  }
}

/**
 * Offer a local proxy once per session when the machine runs in UTC+8.
 *
 * The scan is deliberately lazy: a preview build, a non-UTC+8 machine or a
 * dismissed suggestion never touches a port.
 */
async function detect(): Promise<void> {
  if (detected) return;
  if (new Date().getTimezoneOffset() !== -480) return;
  detected = true;
  await load();
  const url = await invoke<string | null>("detect_local_proxy").catch(() => null);
  if (!url) return;
  const current = settings();
  if (current.proxyMode === "manual" && current.proxyUrl === url) return;
  if (globalThis.localStorage?.getItem(DISMISSED_KEY) === url) return;
  setSuggestion(url);
}

async function useSuggestion(): Promise<void> {
  const url = suggestion();
  if (!url) return;
  await save({ proxyMode: "manual", proxyUrl: url });
  setSuggestion(null);
}

/** Let a later session or a test try again after the zone check. */
function resetDetection(): void {
  detected = false;
}

function dismiss(): void {
  const url = suggestion();
  if (url) globalThis.localStorage?.setItem(DISMISSED_KEY, url);
  setSuggestion(null);
}

export const networkStore = {
  settings, probe, probing, suggestion,
  load, save, test, detect, useSuggestion, dismiss, resetDetection,
};
