import { createSignal } from "solid-js";
import * as api from "./api";
import type { LoopState, PunchState } from "./api";

export function createTransportRanges(filePath: () => string | null) {
  /**
   * The session loop region.
   *
   * Playback state, not project data: it never reaches the DSL, the renderer or
   * an export. It is kept in the webview's local storage per project path, so a
   * reload restores exactly what the transport will loop instead of showing a
   * band that does nothing.
   */
  const [loop, setLoop] = createSignal<LoopState>({
    enabled: false,
    start_beats: null,
    end_beats: null,
  });
  const [loopPending, setLoopPending] = createSignal(false);
  const [loopError, setLoopError] = createSignal<string | null>(null);

  /**
   * The punch range a take keeps notes inside.
   *
   * Session state like the loop region: it never reaches the DSL. The backend
   * owns it, so the transport bar and the ruler band read the same range.
   */
  const [punch, setPunch] = createSignal<PunchState>({
    enabled: false,
    start_beats: null,
    end_beats: null,
  });

  const [punchPending, setPunchPending] = createSignal(false);
  const [punchError, setPunchError] = createSignal<string | null>(null);

  /** Where the region is remembered: UI-local, keyed by the project path. */
  function loopStorageKey(path: string | null): string {
    return `opencompose.loop:${path ?? ""}`;
  }

  function readStoredLoop(path: string | null): { start: number; end: number; enabled: boolean } | null {
    try {
      const raw = localStorage.getItem(loopStorageKey(path));
      if (!raw) return null;
      const value = JSON.parse(raw) as { start?: unknown; end?: unknown; enabled?: unknown };
      if (typeof value.start !== "number" || typeof value.end !== "number") return null;
      return { start: value.start, end: value.end, enabled: value.enabled === true };
    } catch {
      return null;
    }
  }

  function writeStoredLoop(
    path: string | null,
    region: { start: number; end: number; enabled: boolean } | null,
  ): void {
    try {
      if (region) localStorage.setItem(loopStorageKey(path), JSON.stringify(region));
      else localStorage.removeItem(loopStorageKey(path));
    } catch {
      // Storage is optional; the transport still holds the region for this run.
    }
  }

  /** A reply is only a region when it carries two numbers; anything else clears. */
  function asLoopState(state: LoopState): LoopState {
    const start = typeof state?.start_beats === "number" ? state.start_beats : null;
    const end = typeof state?.end_beats === "number" ? state.end_beats : null;
    const armed = start !== null && end !== null;
    return {
      enabled: armed && state?.enabled === true,
      start_beats: armed ? start : null,
      end_beats: armed ? end : null,
      clamped: armed && state?.clamped === true,
    };
  }

  /** Adopt a transport reply and persist it, so a reload tells the truth. */
  function adoptLoop(state: LoopState): void {
    const normalized = asLoopState(state);
    setLoop(normalized);
    const region = normalized.start_beats !== null && normalized.end_beats !== null
      ? {
        start: normalized.start_beats,
        end: normalized.end_beats,
        enabled: normalized.enabled,
      }
      : null;
    writeStoredLoop(filePath(), region);
  }

  /** The region in beats, or null when none is set. */
  function loopRegion(): { start: number; end: number } | null {
    const current = loop();
    if (typeof current.start_beats !== "number" || typeof current.end_beats !== "number") {
      return null;
    }
    return { start: current.start_beats, end: current.end_beats };
  }

  /** Restore this project's region after a load, or clear a stale one loudly. */
  async function restoreLoopForProject(path: string | null): Promise<void> {
    const stored = readStoredLoop(path);
    setLoopError(null);
    try {
      adoptLoop(await api.setLoopRegion(stored?.start ?? null, stored?.end ?? null, stored?.enabled ?? false));
    } catch (e) {
      // A region saved against another timeline cannot apply here. Dropping it
      // and saying so beats leaving a band that would never loop.
      writeStoredLoop(path, null);
      setLoop({ enabled: false, start_beats: null, end_beats: null });
      setLoopError(String(e));
      try { await api.setLoopRegion(null, null, false); } catch { /* already clear */ }
    }
  }

  /** Keep the stored region on the new path after a first save. */
  function rekeyStoredLoop(from: string | null, to: string | null): void {
    const stored = readStoredLoop(from);
    if (!stored) return;
    writeStoredLoop(from, null);
    writeStoredLoop(to, stored);
  }

  /** Set the region drawn in the ruler; Loop keeps its current arm state. */
  async function setLoopRegion(start: number, end: number): Promise<boolean> {
    setLoopPending(true);
    setLoopError(null);
    try {
      adoptLoop(await api.setLoopRegion(start, end, loop().enabled));
      return true;
    } catch (e) {
      setLoopError(String(e));
      return false;
    } finally {
      setLoopPending(false);
    }
  }

  /** Arm or disarm Loop. A missing region refuses with the control's reason. */
  async function toggleLoop(): Promise<boolean> {
    const current = loop();
    if (current.start_beats === null || current.end_beats === null) {
      setLoopError("Drag in the ruler to set a loop region first.");
      return false;
    }
    if (loopPending()) return false;
    setLoopPending(true);
    setLoopError(null);
    try {
      adoptLoop(await api.setLoopRegion(current.start_beats, current.end_beats, !current.enabled));
      return true;
    } catch (e) {
      setLoopError(String(e));
      return false;
    } finally {
      setLoopPending(false);
    }
  }

  /** Clear the region; the backend also disarms Loop, so no toggle can lie. */
  async function clearLoopRegion(): Promise<void> {
    setLoopPending(true);
    setLoopError(null);
    try {
      adoptLoop(await api.setLoopRegion(null, null, false));
    } catch (e) {
      setLoopError(String(e));
    } finally {
      setLoopPending(false);
    }
  }

  /** A reply is only a range when it carries two numbers; anything else clears. */
  function asPunchState(state: PunchState): PunchState {
    const start = typeof state?.start_beats === "number" ? state.start_beats : null;
    const end = typeof state?.end_beats === "number" ? state.end_beats : null;
    const armed = start !== null && end !== null;
    return {
      enabled: armed && state?.enabled === true,
      start_beats: armed ? start : null,
      end_beats: armed ? end : null,
    };
  }

  /** Adopt the punch range a transport snapshot reports. */
  function adoptPunch(state: PunchState | null | undefined): void {
    if (!state || typeof state !== "object") return;
    setPunch(asPunchState(state));
  }

  /** The range in beats, or null when none is set. */
  function punchRegion(): { start: number; end: number } | null {
    const current = punch();
    if (typeof current.start_beats !== "number" || typeof current.end_beats !== "number") {
      return null;
    }
    return { start: current.start_beats, end: current.end_beats };
  }

  /** Set or clear the punch range; the backend validates it and reports why not. */
  async function setPunchRegion(
    start: number | null,
    end: number | null,
    enabled: boolean,
  ): Promise<boolean> {
    setPunchPending(true);
    setPunchError(null);
    try {
      setPunch(asPunchState(await api.setPunchRegion(start, end, enabled)));
      return true;
    } catch (e) {
      setPunchError(String(e));
      return false;
    } finally {
      setPunchPending(false);
    }
  }

  return {
    loop,
    loopPending,
    loopError,
    punch,
    punchPending,
    punchError,
    restoreLoopForProject,
    rekeyStoredLoop,
    setLoopRegion,
    toggleLoop,
    clearLoopRegion,
    loopRegion,
    adoptPunch,
    punchRegion,
    setPunchRegion,
  };
}
