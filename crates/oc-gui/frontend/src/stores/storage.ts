/**
 * Storage facts: what the build keeps on disk, and the two actions the page
 * offers. Counts come from the backend, so the page never guesses a size.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import * as api from "./api";
import type { StorageReport } from "./api";

const [report, setReport] = createSignal<StorageReport | null>(null);
const [loading, setLoading] = createSignal(false);
const [clearing, setClearing] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);
/** The block currently being scanned, so each card can show its own spinner. */
const [scanning, setScanning] = createSignal<string | null>(null);
const [status, setStatus] = createSignal<string | null>(null);

/** A backend older than the page answers without the counts. */
function isReport(value: unknown): value is StorageReport {
  const report = value as StorageReport | null;
  return Boolean(
    report
    && report.soundfonts
    && report.plugins
    && report.samples
    && typeof report.preferences === "string",
  );
}

async function load(): Promise<void> {
  setLoading(true);
  try {
    const next = await api.storageReport();
    if (!isReport(next)) throw new Error("the backend answered without storage counts");
    setReport(next);
    setError(null);
  } catch (failure) {
    setError(String(failure));
  } finally {
    setLoading(false);
  }
}

async function clearClipPcmCache(): Promise<void> {
  setClearing(true);
  try {
    const clip_pcm = await api.clearClipPcmCache();
    setReport((previous) => (previous ? { ...previous, clip_pcm } : previous));
    setError(null);
    setStatus("clip-cache-cleared");
  } catch (failure) {
    setError(String(failure));
  } finally {
    setClearing(false);
  }
}

/** Delete the open project's sample cache and adopt the new counts. */
async function clearSamples(): Promise<boolean> {
  setClearing(true);
  try {
    const next = await api.clearSampleCache();
    if (!isReport(next)) throw new Error("the backend answered without storage counts");
    setReport(next);
    setError(null);
    setStatus("cleared");
    return true;
  } catch (failure) {
    setError(String(failure));
    return false;
  } finally {
    setClearing(false);
  }
}

/** Delete every archived audition file and adopt the new counts. */
async function clearAuditions(): Promise<boolean> {
  setClearing(true);
  try {
    const next = await api.clearAuditionArchive();
    if (!isReport(next)) throw new Error("the backend answered without storage counts");
    setReport(next);
    setError(null);
    setStatus("auditions-cleared");
    return true;
  } catch (failure) {
    setError(String(failure));
    return false;
  } finally {
    setClearing(false);
  }
}

async function openFolder(path: string): Promise<void> {
  try {
    await api.openStorageFolder(path);
    setError(null);
  } catch (failure) {
    setError(String(failure));
  }
}

function reset(): void {
  setReport(null);
  setLoading(false);
  setClearing(false);
  setError(null);
  setStatus(null);
}


/** Container session audio is archived in. */
export type SessionAudioFormat = "wav" | "mp3";

export interface SessionAudioSettings {
  format: SessionAudioFormat;
  mp3Kbps: number;
}

const [sessionAudio, setSessionAudio] = createSignal<SessionAudioSettings>({
  format: "wav",
  mp3Kbps: 192,
});

async function loadSessionAudio(): Promise<void> {
  try {
    const loaded = await invoke<SessionAudioSettings>("session_audio_settings");
    if (loaded && typeof loaded.format === "string") setSessionAudio(loaded);
  } catch {
    // A preview without Tauri keeps the default.
  }
}

async function saveSessionAudio(next: SessionAudioSettings): Promise<void> {
  const saved = await invoke<SessionAudioSettings>("session_audio_save", {
    format: next.format,
    mp3Kbps: next.mp3Kbps,
  });
  if (saved && typeof saved.format === "string") setSessionAudio(saved);
}

export type StorageSection = "sessions" | "soundfonts" | "plugins" | "samples" | "clip_pcm";

/** Re-scan one block without walking the other sections. */
async function scan(section: StorageSection): Promise<void> {
  setScanning(section);
  try {
    const value = await invoke<Record<string, unknown>>("storage_scan", { section });
    setReport((previous) => (previous ? { ...previous, [section]: value } : previous));
    setError(null);
  } catch (cause) {
    setError(String(cause));
  } finally {
    setScanning(null);
  }
}

export const storageStore = {
  scan,
  scanning,
  sessionAudio,
  loadSessionAudio,
  saveSessionAudio,
  report,
  loading,
  clearing,
  error,
  status,
  load,
  clearSamples,
  clearClipPcmCache,
  clearAuditions,
  openFolder,
  reset,
};
