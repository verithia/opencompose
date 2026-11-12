import { createMemo, createSignal } from "solid-js";
import type { BarBeat, Effect, Project, ProjectSnapshot } from "../types/project";
import { createProjectClock } from "../utils/projectClock";
import * as api from "./api";
import type { AbSnapshot, AuditionSnapshot, TransportSnapshot } from "./api";
import { pollAutomationReadouts } from "./automationReadout";
import { pollAutomationWrites } from "./automationWrite";
import { log } from "./log";
import { i18n } from "./i18n";
import { pauseWebAudio } from "./webAudioFocus";

/** The lower panel's temporary playback; it never owns the timeline cursor. */
export type AuditionKind = "preset" | "track" | "pattern" | "clip";

/** The take auditioned through the live render thread; never part of the DSL. */
export interface TakeSoloState {
  track: string;
  take: number;
}

export interface AbCandidateInput {
  label: string;
  params: [string, string | null][];
  effects: Effect[];
}

/** Observers of each raw poll reply; the record store keeps its clock in sync. */
const transportObservers = new Set<(snapshot: TransportSnapshot) => void>();

/**
 * Observe every transport reply the poll adopts. Returns the unsubscribe
 * function.
 */
export function onTransportSnapshot(
  listener: (snapshot: TransportSnapshot) => void,
): () => void {
  transportObservers.add(listener);
  return () => transportObservers.delete(listener);
}

export function createPlaybackStore(
  project: Project,
  applyWithUndo: (snapshot: ProjectSnapshot) => void,
  setLoading: (loading: boolean) => void,
  setError: (message: string) => void,
) {
  const [audition, setAudition] = createSignal<AuditionSnapshot | null>(null);
  /** Last level-matched A/B pair; the readout keeps its measured numbers. */
  const [abPair, setAbPair] = createSignal<AbSnapshot | null>(null);
  const [abBusy, setAbBusy] = createSignal(false);
  const [abError, setAbError] = createSignal<string | null>(null);

  let auditionPollId: ReturnType<typeof setInterval> | null = null;

  const [auditionActive, setAuditionActive] = createSignal(false);
  const [auditionKind, setAuditionKind] = createSignal<AuditionKind | null>(null);
  /** Whether the timeline was playing when the current audition started. */
  let auditionResume = false;
  const [auditionTarget, setAuditionTarget] = createSignal<string | null>(null);
  let auditionTogglePending = false;
  let auditionDesiredPaused = false;
  type PendingPreview = { epoch: number; target: string; paused: boolean };
  let pendingPreview: PendingPreview | null = null;

  function startingPreview() {
    return pendingPreview?.epoch === playbackEpoch ? pendingPreview : null;
  }

  function toggleStartingPreview() {
    const pending = startingPreview();
    if (!pending) return false;
    pending.paused = !pending.paused;
    setAudition(current => current ? { ...current, paused: pending.paused } : current);
    return true;
  }

  const [transport, setTransport] = createSignal<TransportSnapshot>({
    playing: false,
    position_beats: 0,
    position_seconds: 0,
    duration_seconds: 0,
  });

  /** Absolute beat painted while a ruler drag previews a seek. */
  const [scrubPosition, setScrubPosition] = createSignal<number | null>(null);

  const [takeSolo, setTakeSolo] = createSignal<TakeSoloState | null>(null);

  // Transport polling interval
  let transportPollId: ReturnType<typeof setInterval> | null = null;
  // Smooth cursor interpolation via requestAnimationFrame
  let lastPollTime = 0;
  let lastPollBeats = 0;
  let rafId: number | null = null;
  const [smoothPositionBeats, setSmoothPositionBeats] = createSignal(0);
  // Track whether playback has ever been triggered — cursor stays visible once shown
  const [cursorActive, setCursorActive] = createSignal(false);
  /** Beat painted by the preview cursor while a track preview runs; null when idle. */
  const [previewCursorBeats, setPreviewCursorBeats] = createSignal<number | null>(null);
  /** Where the previewed content starts, in project beats; null means the
   * preview plays pattern-local time with no arrangement position. */
  const [auditionOriginBeats, setAuditionOriginBeats] = createSignal<number | null>(null);

  const transportClock = createMemo(() => {
    try { return createProjectClock(project.properties); }
    catch { return null; } // Invalid authored clocks are rejected by the backend.
  });

  function startCursorRaf() {
    if (rafId !== null) return;
    const tick = () => {
      const t = transport();
      // A track preview plays the solo arrangement from its start; the
      // arrangement playhead stays where the user left it.
      if (t.playing && !previewAnchor) {
        const now = performance.now();
        const elapsed = (now - lastPollTime) / 1000;
        const clock = transportClock();
        const interpolated = clock
          ? clock.quarterAtSeconds(clock.secondsAt(lastPollBeats) + Math.max(0, elapsed))
          : lastPollBeats;
        setSmoothPositionBeats(interpolated);
      }
      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
  }

  function stopCursorRaf() {
    if (rafId !== null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  }

  // Playhead position held while a track preview runs. The preview renders the
  // solo arrangement from its own start, so its transport frames must not move
  // the arrangement cursor.
  let previewAnchor: { beats: number; seconds: number; duration: number } | null = null;
  const [previewTrackIndex, setPreviewTrackIndex] = createSignal<number | null>(null);

  /** An explicit transport action takes the playhead back from a running preview. */
  function releasePlayheadHold() {
    previewAnchor = null;
    setPreviewCursorBeats(null);
  }

  function holdPlayhead(state: TransportSnapshot): TransportSnapshot {
    if (!previewAnchor) return state;
    return {
      ...state,
      position_beats: previewAnchor.beats,
      position_seconds: previewAnchor.seconds,
    };
  }

  function startTransportPoll() {
    if (transportPollId) return;
    const epoch = playbackEpoch;
    let pollCount = 0;
    let consecutiveStoppedPolls = 0;
    transportPollId = setInterval(async () => {
      try {
        const raw = await api.getTransportState();
        if (raw.project) applyWithUndo(raw.project);
        if (epoch !== playbackEpoch) return;
        const state = holdPlayhead(raw);
        pollCount++;
        // The preview cursor follows the polled preview position, never the anchor.
        if (previewAnchor) {
          setPreviewCursorBeats(raw.playing || raw.track_preview_rebuild ? raw.position_beats : null);
        }
        // Update interpolation anchor on each poll
        lastPollTime = performance.now();
        lastPollBeats = state.position_beats;
        setTransport(state);
        for (const observer of transportObservers) observer(raw);
        pollAutomationReadouts(state.position_beats, state.playing);
        pollAutomationWrites(state.position_beats, state.playing);
        if (!state.warming && !state.starting) setLoading(false);
        if (state.status) {
          if (state.status !== lastTransportStatus) {
            lastTransportStatus = state.status;
            setError(state.status);
          }
        } else {
          lastTransportStatus = null;
        }
        // Also update smooth position directly (in case RAF isn't running)
        setSmoothPositionBeats(state.position_beats);
        // Recording needs the poll even after the arrangement stops. Outside
        // a take, allow for transient playing=false during a seek.
        if (!state.playing && !raw.record?.recording) {
          consecutiveStoppedPolls++;
          if (raw.track_preview_rebuild) {
            setCursorActive(false);
            stopTransportPoll();
          } else if (consecutiveStoppedPolls >= 3 && transportPollId) {
            const anchor = previewAnchor;
            stopTransportPoll();
            if (anchor) {
              ++playbackEpoch;
              void stopTrackPreview(anchor, true);
            }
          }
        } else {
          consecutiveStoppedPolls = 0;
        }
      } catch (_) {
        // ignore poll errors
      }
    }, 50); // 20Hz update rate
    startCursorRaf();
  }

  function stopTransportPoll() {
    if (transportPollId) {
      clearInterval(transportPollId);
      transportPollId = null;
    }
    stopCursorRaf();
    // Snap smooth position to final transport state
    setSmoothPositionBeats(transport().position_beats);
  }

  /** Adopt a snapshot a transport command returned, before the poll ticks. */
  function adoptTransport(snapshot: TransportSnapshot) {
    const state = holdPlayhead(snapshot);
    setTransport(state);
    lastPollTime = performance.now();
    lastPollBeats = state.position_beats;
    setSmoothPositionBeats(state.position_beats);
  }

  let playPending = false;
  let playbackEpoch = 0;
  let pendingSeek: { epoch: number; operation: Promise<boolean>; toggle: boolean } | null = null;
  let pendingTrack: { epoch: number; index: number; paused: boolean } | null = null;
  let lastTransportStatus: string | null = null;

  function takeBlocksPlayback(): boolean {
    if (!transport().record?.recording) return false;
    setError(i18n.t("toolbar.recordingPlaybackLocked"));
    return true;
  }

  async function play() {
    if (takeBlocksPlayback()) return;
    if (playPending && !auditionActive() && !pendingTrack) return;
    const epoch = ++playbackEpoch;
    playPending = true;
    stopTransportPoll();
    try {
      if (auditionActive() || pendingPreview) await stopAuditionForTimeline();
      if (epoch !== playbackEpoch) return;
      releasePlayheadHold();
      setLoading(true);
      lastTransportStatus = null;
      // Optimistically show cursor at current position while render starts
      const currentPos = transport().position_beats;
      setCursorActive(true);
      setTransport({ ...transport(), playing: true });
      setSmoothPositionBeats(currentPos);
      lastPollTime = performance.now();
      lastPollBeats = currentPos;
      // Start RAF for smooth cursor (but NOT the backend poll yet —
      // polling before backend is ready would overwrite our optimistic state)
      startCursorRaf();

      let state = await api.transportPlay();
      if (epoch !== playbackEpoch) return;

      // If backend returned playing=false with did_render=true, the render
      // likely produced an empty buffer (resources not yet loaded). Retry once.
      if (!state.playing && state.did_render && state.duration_seconds === 0) {
        await new Promise(r => setTimeout(r, 200));
        if (epoch !== playbackEpoch) return;
        state = await api.transportPlay();
        if (epoch !== playbackEpoch) return;
      }

      setTransport(state);
      // Re-anchor interpolation to actual backend state
      lastPollTime = performance.now();
      lastPollBeats = state.position_beats;
      setSmoothPositionBeats(state.position_beats);
      // A press during a background build returns at once; keep the chip up until
      // the worker reports audio instead of pretending the transport is ready.
      setLoading(Boolean(state.warming || state.starting));
      // NOW start polling — backend is confirmed playing
      if (state.playing) {
        startTransportPoll();
      } else {
        stopCursorRaf();
        setCursorActive(false);
      }
      if (state.did_render) {
        log.info("transport", "Project load queued for streaming");
      } else {
        log.info("transport", "Resumed from pause (no render)");
      }
      if (state.warnings && state.warnings.length > 0) {
        for (const w of state.warnings) {
          log.warn("plugin", w);
        }
        setError(state.warnings.join("; "));
      }
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      setError(String(e)); setLoading(false);
      stopCursorRaf();
      setCursorActive(false);
      setTransport({ ...transport(), playing: false });
    } finally {
      if (epoch === playbackEpoch) playPending = false;
    }
  }

  async function pause() {
    if (takeBlocksPlayback()) return;
    const wasPlaying = transport().playing;
    const epoch = ++playbackEpoch;
    playPending = false;
    setLoading(false);
    stopTransportPoll();
    if (auditionActive() || pendingPreview) await stopAuditionForTimeline();
    if (epoch !== playbackEpoch) return;
    releasePlayheadHold();
    try {
      const state = await api.transportPause();
      if (epoch !== playbackEpoch) return;
      setTransport(state);
      setSmoothPositionBeats(state.position_beats);
      if (state.record?.recording) startTransportPoll();
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      setError(String(e));
      if (wasPlaying || transport().record?.recording) startTransportPoll();
    }
  }

  async function stop() {
    const arrangementDuration = previewAnchor?.duration ?? transport().duration_seconds;
    const epoch = ++playbackEpoch;
    playPending = false;
    setLoading(false);
    stopTransportPoll();
    if (auditionActive() || abPair() || pendingPreview) {
      pendingPreview = null;
      auditionResume = false;
      clearAuditionState();
      clearAbPair();
      try { await api.stopAudition(); } catch (e) { if (epoch === playbackEpoch) setError(String(e)); }
    }
    if (epoch !== playbackEpoch) return;
    releasePlayheadHold();
    try {
      const state = await api.transportStop();
      if (epoch !== playbackEpoch) return;
      setTransport(arrangementDuration === undefined ? state : { ...state, duration_seconds: arrangementDuration });
      // A stop that committed a take reports the project it wrote.
      if (state.project) applyWithUndo(state.project);
      setSmoothPositionBeats(state.position_beats);
      setCursorActive(false);
      stopTransportPoll();
    } catch (e) { if (epoch === playbackEpoch) setError(String(e)); }
  }

  function seek(beat: number): Promise<boolean> {
    const operation = seekTo(beat);
    const pending = { epoch: playbackEpoch, operation, toggle: false };
    pendingSeek = pending;
    return operation.then((applied) => {
      if (pendingSeek === pending) pendingSeek = null;
      return applied;
    });
  }

  async function seekTo(beat: number) {
    if (takeBlocksPlayback()) return false;
    const wasPlaying = transport().playing;
    const arrangementDuration = previewAnchor?.duration ?? transport().duration_seconds;
    const epoch = ++playbackEpoch;
    playPending = false;
    setLoading(false);
    stopTransportPoll();
    if (auditionActive() || pendingPreview) await stopAuditionForTimeline();
    if (epoch !== playbackEpoch) return false;
    releasePlayheadHold();
    try {
      const state = await api.transportSeek(beat);
      if (epoch !== playbackEpoch) return false;
      setTransport(arrangementDuration === undefined ? state : { ...state, duration_seconds: arrangementDuration });
      // Update interpolation anchor
      lastPollTime = performance.now();
      lastPollBeats = state.position_beats;
      setSmoothPositionBeats(state.position_beats);
      if (state.playing) startTransportPoll();
      return true;
    } catch (e) {
      if (epoch === playbackEpoch) {
        setError(String(e));
        if (wasPlaying || transport().record?.recording) startTransportPoll();
      }
      return false;
    }
  }

  /** Jump the playhead to a marker and show the cursor. */
  async function seekToMarker(position: BarBeat) {
    const clock = createProjectClock(project.properties);
    setCursorActive(true);
    await seek(clock.quarterAt(position));
  }

  async function togglePlayPause() {
    const seek = pendingSeek;
    if (seek?.epoch === playbackEpoch) {
      seek.toggle = !seek.toggle;
      const applied = await seek.operation;
      if (!applied || seek.epoch !== playbackEpoch || !seek.toggle) return;
      seek.toggle = false;
    }
    if (toggleStartingPreview()) return;
    if (pendingTrack?.epoch === playbackEpoch) { await toggleTrackPreview(); return; }
    if (auditionActive()) { await toggleAudition(); return; }
    // Pause is an intent even while the initial playback request is preparing.
    if (playPending) { await pause(); return; }
    if (transport().playing) {
      await pause();
    } else {
      await play();
    }
  }

  function previewDrumPad(presetName: string, note: number, velocity?: number) {
    const target = JSON.stringify(["pad", presetName, note]);
    return startPrivatePreview("preset", target, presetName,
      () => api.previewDrumPad(presetName, note, velocity, true), true);
  }

  function samplePreviewTarget(owner: string, provider: string, id: string) {
    return JSON.stringify(["sample", owner, provider, id]);
  }

  function previewMarketSample(owner: string, provider: string, id: string, label: string, onError: (error: unknown) => void) {
    return startPrivatePreview("preset", samplePreviewTarget(owner, provider, id), label, async () => {
      const preview = await api.samplePreview(provider, id, label, true);
      if (preview.status === "cancelled") throw new Error("Preview was cancelled");
    }, false, onError);
  }

  function marketSampleAction(owner: string, provider: string, id: string) {
    return auditionAction(samplePreviewTarget(owner, provider, id));
  }

  function stopMarketSample(owner: string, provider: string, id: string) {
    if (marketSampleAction(owner, provider, id)) return stopAudition();
  }

  /** Audition a take. The DSL is never written; `null` clears the audition. */
  async function auditionTake(track: string | null, take: number | null) {
    try {
      const next = await api.setTakeSolo(track, take);
      setTakeSolo(next.track !== null && next.take !== null
        ? { track: next.track, take: next.take }
        : null);
    } catch (e) { setError(String(e)); }
  }

  function stopAuditionPoll() {
    if (auditionPollId) {
      clearInterval(auditionPollId);
      auditionPollId = null;
    }
  }

  function clearAuditionState(preserveAbReadout = false) {
    if (!preserveAbReadout) { retainedAbRequest = null; stopAbPoll(); }
    setAbBusy(false);
    pendingTrack = null;
    setAuditionTarget(null);
    auditionTogglePending = false;
    setPreviewCursorBeats(null);
    setAuditionOriginBeats(null);
    setPreviewTrackIndex(null);
    stopAuditionPoll();
    setAuditionActive(false);
    setAuditionKind(null);
    setAudition(null);
  }

  /**
   * Park the timeline before temporary playback. The cursor does not move, so a
   * stopped audition returns to the transport state it interrupted.
   */
  async function beginAudition(kind: AuditionKind, epoch = playbackEpoch, preserveAbPair = false) {
    if (!preserveAbPair && abPair()) clearAbPair();
    retainedAbRequest = null;
    stopAbPoll();
    const replacingTrack = !!previewAnchor;
    const resumeArrangement = auditionResume;
    if (previewAnchor) await stopTrackPreview(previewAnchor, false);
    if (epoch !== playbackEpoch) return;
    if (!auditionActive()) {
      const wasPlaying = transport().playing;
      auditionResume = replacingTrack ? resumeArrangement : wasPlaying;
      if (wasPlaying) {
        stopTransportPoll();
        try {
          const state = await api.transportPause();
          if (epoch !== playbackEpoch) return;
          setTransport(state);
          setSmoothPositionBeats(state.position_beats);
          setCursorActive(false);
        } catch (e) {
          if (epoch !== playbackEpoch) return;
          auditionResume = false;
          throw e;
        }
      }
    }
    setAuditionKind(kind);
    setAuditionActive(true);
  }

  async function finishAudition(resume: boolean, preserveAbReadout = false) {
    ++playbackEpoch;
    const shouldResume = resume && auditionResume;
    auditionResume = false;
    clearAuditionState(preserveAbReadout && !shouldResume);
    if (shouldResume) {
      // Main transport takes the native stream back; a discarded pair cannot replay.
      clearAbPair();
      await play();
    } else if (transport().playing || transport().record?.recording) {
      stopTransportPoll();
      startTransportPoll();
    }
  }

  function startAuditionPoll() {
    if (auditionPollId) return;
    const epoch = playbackEpoch;
    let sawActive = false;
    let idlePolls = 0;
    auditionPollId = setInterval(async () => {
      try {
        const state = await api.getTransportState();
        if (state.project) applyWithUndo(state.project);
        if (epoch !== playbackEpoch) return;
        const snapshot = state.audition;
        const audible = snapshot && (snapshot.status !== "idle" || snapshot.paused || state.audition_active);
        setAudition(current => audible ? { ...snapshot, preset: snapshot.preset ?? current?.preset ?? null } : null);
        if (state.audition?.status === "error") {
          const message = state.audition.error ?? "Audition failed";
          setError(message);
          await finishAudition(true);
          return;
        }
        if (state.audition?.finished) {
          await finishAudition(true, auditionTarget() === `ab:${abRequest}`);
          return;
        }
        if (state.audition?.paused) return;
        if (state.audition_active) {
          sawActive = true;
          idlePolls = 0;
          return;
        }
        if (state.audition?.status === "loading") return;
        idlePolls++;
        if (sawActive || state.audition?.status === "ready" || idlePolls >= 6) {
          await finishAudition(true, auditionTarget() === `ab:${abRequest}`);
        }
      } catch (_) {
        // A transient poll failure leaves the loading indicator in place.
      }
    }, 50);
  }

  function markAuditionLoading(label: string) {
    setAudition({
      preset: label,
      status: "loading",
      ready_ms: null,
      paused: false,
      position_seconds: 0,
      instances: audition()?.instances ?? 0,
      loads: audition()?.loads ?? 0,
      error: null,
    });
  }

  function previewPreset(presetName: string) {
    return startPrivatePreview("preset", `preset:${presetName}`, presetName, () => api.previewPreset(presetName, true));
  }

  /** Audition a factory candidate without loading it into the project. */
  function auditionFactoryPatch(
    label: string,
    params: [string, string | null][],
    effects: Effect[] = [],
  ) {
    const target = JSON.stringify(["factory", label, params, effects]);
    return startPrivatePreview("preset", target, label, () => api.auditionFactoryPatch(params, label, effects, true));
  }

  /** A new compare or audition supersedes any older render still in flight. */
  let abRequest = 0;

  let retainedAbRequest: number | null = null;
  let abPollId: ReturnType<typeof setInterval> | null = null;
  function stopAbPoll() {
    if (abPollId) clearInterval(abPollId);
    abPollId = null;
  }

  function startAbPoll(request: number) {
    stopAbPoll();
    if (!abPair()?.measurement_pending) return;
    let reading = false;
    abPollId = setInterval(async () => {
      if (request !== abRequest || retainedAbRequest !== request) {
        stopAbPoll();
        return;
      }
      if (reading) return;
      reading = true;
      try {
        const snapshot = await api.getAbAuditionState();
        if (request !== abRequest || retainedAbRequest !== request) return;
        if (!snapshot) { stopAbPoll(); return; }
        const current = abPair();
        if (!current || snapshot.a_label !== current.a_label || snapshot.b_label !== current.b_label) return;
        setAbPair(snapshot);
        if (!snapshot.measurement_pending) stopAbPoll();
      } catch (e) {
        if (request === abRequest && retainedAbRequest === request) setAbError(String(e));
      } finally { reading = false; }
    }, 200);
  }

  /** Start streamed comparison; measurement arrives asynchronously for this pair. */
  async function compareFactoryPatches(a: AbCandidateInput, b: AbCandidateInput) {
    if (takeBlocksPlayback()) return null;
    const epoch = ++playbackEpoch;
    const request = ++abRequest;
    const target = `ab:${request}`;
    const pending = { epoch, target, paused: false };
    pendingPreview = pending;
    playPending = false;
    auditionTogglePending = false;
    stopAuditionPoll();
    stopAbPoll();
    setAbBusy(true);
    setAbError(null);
    setAbPair(null);
    markAuditionLoading(b.label);
    try {
      await beginAudition("preset", epoch);
      if (epoch !== playbackEpoch || request !== abRequest) return null;
      setAbBusy(true);
      setAuditionTarget(target);
      retainedAbRequest = request;
      markAuditionLoading(b.label);
      setAudition(current => current ? { ...current, paused: pending.paused } : current);
      const snapshot = await api.compareFactoryPatches(a, b, true);
      if (epoch !== playbackEpoch || request !== abRequest || retainedAbRequest !== request) return null;
      if (!await applyStartingPreviewIntent(pending)) return null;
      if (request !== abRequest || retainedAbRequest !== request) return null;
      setAbPair(snapshot);
      startAuditionPoll();
      startAbPoll(request);
      return snapshot;
    } catch (e) {
      if (epoch !== playbackEpoch || request !== abRequest) return null;
      setAbError(String(e));
      await finishAudition(true);
      return null;
    } finally {
      if (pendingPreview === pending) pendingPreview = null;
      if (request === abRequest) setAbBusy(false);
    }
  }

  /** Flip the pair; a finished pass starts again from the top on the new side. */
  async function switchAb() {
    if (!abPair()) return null;
    const request = abRequest;
    const epoch = playbackEpoch;
    setAbError(null);
    try {
      const snapshot = await api.switchAb();
      if (request !== abRequest || epoch !== playbackEpoch) return null;
      setAbPair(snapshot);
      if (!auditionActive()) {
        await beginAudition("preset", epoch, true);
        if (epoch !== playbackEpoch) return null;
      }
      setAuditionTarget(`ab:${request}`);
      retainedAbRequest = request;
      startAuditionPoll();
      startAbPoll(request);
      return snapshot;
    } catch (e) {
      if (request === abRequest && epoch === playbackEpoch) setAbError(String(e));
      return null;
    }
  }

  /** Forget the pair when the browser closes or another audition takes over. */
  function clearAbPair() {
    retainedAbRequest = null;
    stopAbPoll();
    abRequest += 1;
    setAbPair(null);
    setAbError(null);
  }

  function previewPattern(patternName: string) {
    // The timeline draws the dashed preview cursor where the pattern sits, so
    // the preview needs the position of its first placement.
    setAuditionOriginBeats(patternPlacementBeats(patternName));
    return startPrivatePreview("pattern", `pattern:${patternName}`, patternName, () => api.previewPattern(patternName, true));
  }

  /** Project beats of the first clip or inline pattern that plays `patternName`. */
  function patternPlacementBeats(patternName: string): number | null {
    let clock: ReturnType<typeof createProjectClock>;
    try {
      clock = createProjectClock(project.properties);
    } catch {
      return null;
    }
    for (const track of project.tracks) {
      const clip = track.clips?.find((entry) => entry.pattern === patternName);
      if (clip?.time_range) return clock.quarterAt(clip.time_range.start);
      const inline = track.patterns?.find((entry) => entry.name === patternName);
      if (inline?.time_range) return clock.quarterAt(inline.time_range.start);
    }
    return null;
  }

  /** Pause/resume the current private audition without moving the arrangement cursor. */
  async function toggleAudition(): Promise<{ restartEpoch: number } | void> {
    if (toggleStartingPreview()) return;
    if (auditionKind() === "track") { await toggleTrackPreview(); return; }
    if (!auditionActive()) return;
    if (auditionTogglePending) {
      auditionDesiredPaused = !auditionDesiredPaused;
      return;
    }
    auditionTogglePending = true;
    auditionDesiredPaused = audition()?.paused !== true;
    const epoch = ++playbackEpoch;
    stopAuditionPoll();
    try {
      for (;;) {
        const paused = auditionDesiredPaused;
        const state = paused ? await api.pauseAudition() : await api.resumeAudition();
        if (epoch !== playbackEpoch) return;
        if (state.audition?.finished) {
          // Explicit pause/Space never resumes the parked arrangement. Only the
          // same-preview button consumes this restart result on a resume race.
          const restartRequested = !auditionDesiredPaused;
          const retirement = finishAudition(false);
          const retiredEpoch = playbackEpoch;
          await retirement;
          if (restartRequested && retiredEpoch === playbackEpoch) return { restartEpoch: retiredEpoch };
          return;
        }
        if (state.audition) setAudition(state.audition);
        else setAudition(current => current ? {...current, paused} : current);
        if (paused === auditionDesiredPaused) break;
      }
      startAuditionPoll();
    } catch (error) {
      if (epoch === playbackEpoch) setError(String(error));
    } finally {
      if (epoch === playbackEpoch) auditionTogglePending = false;
    }
  }

  function auditionAction(target: string): "pause" | "resume" | null {
    const snapshot = audition();
    const pending = startingPreview();
    if (pending) return pending.target === target ? (snapshot?.paused ? "resume" : "pause") : null;
    return auditionActive() && auditionTarget() === target ? (playbackPlaying() ? "pause" : "resume") : null;
  }

  /** The audible owner, including a parked private audition. */
  function playbackPlaying() {
    const pending = startingPreview();
    if (pending) {
      // The snapshot signal updates whenever the startup intent changes.
      return audition()?.paused !== true;
    }
    return auditionActive() && auditionKind() !== "track" ? audition()?.paused !== true : transport().playing;
  }
  function auditionPositionSeconds(): number | null {
    return auditionActive() && auditionKind() !== "track" ? audition()?.position_seconds ?? 0 : null;
  }

  /** The private preview's playhead in beats from the previewed content's start.
   * A preset audition has no place on the timeline, so it reports null. */
  const auditionLocalBeats = createMemo<number | null>(() => {
    const seconds = auditionPositionSeconds();
    const kind = auditionKind();
    if (seconds === null || (kind !== "clip" && kind !== "pattern")) return null;
    return createProjectClock(project.properties).quarterAtSeconds(seconds);
  });

  /** Project beats where the previewed content starts; null for a pattern
   * preview, which the piano roll draws in its own local time. */
  const auditionStartBeats = createMemo<number | null>(() =>
    auditionKind() === "clip" ? auditionOriginBeats() : null,
  );

  /** Where the previewed content sits on the arrangement, so the timeline can
   * draw its cursor. A pattern preview knows the position of its first
   * placement even though the piano roll draws it locally. */
  const auditionArrangementBeats = createMemo<number | null>(() => {
    const local = auditionLocalBeats();
    const origin = auditionOriginBeats();
    if (local === null || origin === null) return null;
    const kind = auditionKind();
    if (kind !== "clip" && kind !== "pattern") return null;
    return origin + local;
  });

  /** Stop the current audition and restore the transport it interrupted. */
  async function stopAudition() {
    if (!auditionActive() && !abPair() && !startingPreview()) return;
    pendingPreview = null;
    clearAbPair();
    const epoch = ++playbackEpoch;
    playPending = false;
    stopAuditionPoll();
    if (auditionKind() === "track") {
      const anchor = previewAnchor;
      if (anchor) {
        await stopTrackPreview(anchor, true);
        return;
      }
    }
    try {
      await api.stopAudition();
    } catch (e) {
      setError(String(e));
    }
    if (epoch === playbackEpoch) await finishAudition(true);
  }

  /** Stop the audition so an explicit timeline action can take over. */
  async function stopAuditionForTimeline() {
    pendingPreview = null;
    if (!auditionActive()) {
      auditionResume = false;
      clearAuditionState();
      return;
    }
    const epoch = playbackEpoch;
    if (auditionKind() === "track") {
      const anchor = previewAnchor;
      if (anchor) {
        await stopTrackPreview(anchor, false);
        return;
      }
    }
    try {
      await api.stopAudition();
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      setError(String(e));
    }
    if (epoch !== playbackEpoch) return;
    auditionResume = false;
    clearAuditionState();
  }

  let dryKeyboardParking: Promise<boolean> | undefined;

  function parkPreviewForDryKeyboard(): Promise<boolean> | undefined {
    if (dryKeyboardParking) return dryKeyboardParking;
    if (!auditionActive() && !startingPreview() && !abPair() && !pendingTrack) return;
    const epoch = ++playbackEpoch;
    playPending = false;
    stopTransportPoll();
    stopAuditionPoll();
    clearAbPair();
    const parking = stopAuditionForTimeline().then(() => epoch === playbackEpoch);
    dryKeyboardParking = parking;
    const clear = () => {
      if (dryKeyboardParking === parking) dryKeyboardParking = undefined;
    };
    void parking.then(clear, clear);
    return parking;
  }

  async function playDryKey(preset: string, note: number) {
    if (takeBlocksPlayback()) return;
    const requestedEpoch = playbackEpoch;
    let epoch = requestedEpoch;
    const state = await api.openSynthNoteOn(preset, note, 108, () => {
      if (requestedEpoch !== playbackEpoch) return Promise.resolve(false);
      const parking = parkPreviewForDryKeyboard();
      epoch = playbackEpoch;
      return parking;
    });
    if (state && epoch === playbackEpoch) {
      stopTransportPoll();
      setCursorActive(false);
      // A retired solo graph can still report its preview range.
      adoptTransport({ ...state, duration_seconds: transport().duration_seconds });
    }
  }

  /**
   * Stop the main-transport track preview and put the timeline cursor back where
   * it was. The preview pass has its own start, so its position is never allowed
   * to become the timeline position.
   */
  async function stopTrackPreview(
    anchor: { beats: number; seconds: number; duration: number },
    resume: boolean,
  ) {
    const epoch = playbackEpoch;
    previewAnchor = null;
    setPreviewCursorBeats(null);
    stopTransportPoll();
    try {
      await api.transportPause();
      if (epoch !== playbackEpoch) return;
      const state = await api.transportSeek(anchor.beats);
      if (epoch !== playbackEpoch) return;
      setTransport({ ...state, playing: false, duration_seconds: anchor.duration });
      setSmoothPositionBeats(anchor.beats);
      setCursorActive(false);
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      setError(String(e));
    }
    if (epoch !== playbackEpoch) return;
    const shouldResume = resume && auditionResume;
    auditionResume = false;
    clearAuditionState();
    if (shouldResume) {
      // Main transport takes the native stream back; a discarded pair cannot replay.
      clearAbPair();
      await play();
    }
  }

  /** Pause/resume the retained solo graph without returning to arrangement playback. */
  async function toggleTrackPreview() {
    const trackIndex = previewTrackIndex();
    if (pendingTrack?.epoch === playbackEpoch) {
      pendingTrack.paused = !pendingTrack.paused;
      setTransport({ ...transport(), playing: !pendingTrack.paused });
      return;
    }
    if (trackIndex === null || !previewAnchor || playPending) return;
    const epoch = ++playbackEpoch;
    const pending = { epoch, index: trackIndex, paused: transport().playing };
    pendingTrack = pending;
    playPending = true;
    stopTransportPoll();
    let requestedPaused = pending.paused;
    try {
      let state: TransportSnapshot;
      do {
        requestedPaused = pending.paused;
        state = requestedPaused ? await api.transportPause() : await api.resumeTrackPreview(trackIndex);
        if (epoch !== playbackEpoch) return;
      } while (requestedPaused !== pending.paused);
      setTransport(holdPlayhead(state));
      setPreviewCursorBeats(state.position_beats);
      setCursorActive(state.playing);
      if (state.playing) startTransportPoll();
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      if (!requestedPaused && previewAnchor) await stopTrackPreview(previewAnchor, false);
      else if (requestedPaused) startTransportPoll();
      setError(String(e));
    } finally {
      if (pendingTrack === pending) pendingTrack = null;
      if (epoch === playbackEpoch) playPending = false;
    }
  }

  async function previewTrack(trackIndex: number) {
    if (takeBlocksPlayback()) return;
    if ((pendingTrack?.epoch === playbackEpoch && pendingTrack.index === trackIndex) || (previewTrackIndex() === trackIndex && previewAnchor)) {
      await toggleTrackPreview();
      return;
    }
    const epoch = ++playbackEpoch;
    playPending = true;
    const pending = { epoch, index: trackIndex, paused: false };
    pendingTrack = pending;
    try {
      await beginAudition("track", epoch);
      if (epoch !== playbackEpoch) return;
      pendingTrack = pending;
      // The preview starts at the track's first content; keep the arrangement
      // playhead at the position the user left it.
      const anchor = {
        beats: transport().position_beats,
        seconds: transport().position_seconds,
        duration: transport().duration_seconds,
      };
      previewAnchor = anchor;
      setPreviewTrackIndex(trackIndex);
      const started = await api.previewTrack(trackIndex, true);
      if (epoch !== playbackEpoch) return;
      let snap = started;
      // Reconcile Space with the native owner only after the solo is installed.
      // The initial device gate is closed even if parking or loading took time.
      let appliedPaused = !snap.playing;
      while (appliedPaused !== pending.paused) {
        appliedPaused = pending.paused;
        snap = appliedPaused ? await api.transportPause() : await api.resumeTrackPreview(trackIndex);
        if (epoch !== playbackEpoch) return;
      }
      pendingTrack = null;
      if (started.warnings?.length) setError(started.warnings.join("; "));
      if (!snap.playing && !(pending.paused && (started.did_render || snap.duration_seconds > 0))) {
        previewAnchor = null;
        setPreviewCursorBeats(null);
        setCursorActive(false);
        stopCursorRaf();
        playPending = false;
        await finishAudition(true);
        return;
      }
      const playing = !pending.paused && snap.playing;
      setTransport(holdPlayhead({ ...transport(), playing, duration_seconds: snap.duration_seconds }));
      setCursorActive(playing);
      setPreviewCursorBeats(snap.position_beats);
      lastPollTime = performance.now();
      lastPollBeats = anchor.beats;
      setSmoothPositionBeats(anchor.beats);
      if (playing) startTransportPoll();
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      setError(String(e));
      const anchor = previewAnchor;
      previewAnchor = null;
      setPreviewCursorBeats(null);
      stopTransportPoll();
      auditionResume = false;
      clearAuditionState();
      if (anchor) {
        try {
          const state = await api.transportSeek(anchor.beats);
          setTransport(state);
          setSmoothPositionBeats(anchor.beats);
        } catch (_) {
          setCursorActive(false);
          stopCursorRaf();
        }
      }
      if (transport().playing || transport().record?.recording) startTransportPoll();
    } finally {
      if (epoch === playbackEpoch) playPending = false;
    }
  }

  /** Audition a placed block while keeping the arrangement cursor parked. */
  function previewBlock(trackIndex: number, start: BarBeat, end: BarBeat, audioClipName?: string) {
    const target = JSON.stringify(["clip", trackIndex, start, end, audioClipName]);
    const label = audioClipName ?? `Track ${trackIndex + 1}`;
    // The preview stream reports time from its window start, so the cursor needs
    // the block's own position to draw it on the arrangement.
    setAuditionOriginBeats(createProjectClock(project.properties).quarterAt(start));
    return startPrivatePreview("clip", target, label, () => api.previewBlock(trackIndex, start, end, true, audioClipName));
  }

  async function startPrivatePreview(
    kind: "preset" | "pattern" | "clip",
    target: string,
    label: string,
    start: () => Promise<void | string[]>,
    retrigger = false,
    onError = (error: unknown) => setError(String(error)),
  ) {
    if (takeBlocksPlayback()) return;
    const activeTarget = startingPreview()?.target ?? (auditionActive() ? auditionTarget() : null);
    if (!retrigger && activeTarget === target) {
      const restart = await toggleAudition();
      if (!restart || restart.restartEpoch !== playbackEpoch) return;
    }
    const epoch = ++playbackEpoch;
    const pending = { epoch, target, paused: false };
    pendingPreview = pending;
    playPending = false;
    auditionTogglePending = false;
    stopAuditionPoll();
    markAuditionLoading(label);
    try {
      await beginAudition(kind, epoch);
      if (epoch !== playbackEpoch) return;
      setAuditionTarget(target);
      markAuditionLoading(label);
      setAudition(current => current ? { ...current, paused: pending.paused } : current);
      // An installed but suspended stream cannot emit a block before a Space
      // press received during parking or loading has been applied.
      const warnings = await start();
      if (epoch !== playbackEpoch) return;
      if (warnings?.length) setError(warnings.join("; "));
      if (!await applyStartingPreviewIntent(pending)) return;
      startAuditionPoll();
    } catch (e) {
      if (epoch !== playbackEpoch) return;
      onError(e);
      await finishAudition(true);
    } finally {
      if (pendingPreview === pending) pendingPreview = null;
    }
  }

  async function applyStartingPreviewIntent(pending: PendingPreview) {
    let appliedPaused = true;
    while (pending.paused !== appliedPaused) {
      appliedPaused = pending.paused;
      const snapshot = appliedPaused ? await api.pauseAudition() : await api.resumeAudition();
      if (pending.epoch !== playbackEpoch) return false;
      if (snapshot.audition) setAudition({ ...snapshot.audition, paused: pending.paused });
    }
    return pending.epoch === playbackEpoch;
  }

  /** Invalidate any in-flight playback reply from the previous document. */
  function endPlaybackForProjectReplacement() {
    pauseWebAudio();
    if (auditionActive() || startingPreview() || abPair() || playPending || transport().playing) {
      return stop();
    }
    ++playbackEpoch;
    stopTransportPoll();
    auditionResume = false;
    clearAuditionState();
    releasePlayheadHold();
  }

  function resetPosition() {
    setTransport({ ...transport(), playing: false, position_beats: 0, position_seconds: 0 });
    setSmoothPositionBeats(0);
    setCursorActive(false);
  }

  function adoptArrangementDuration(duration: number | undefined) {
    if (typeof duration !== "number") return;
    if (previewAnchor) previewAnchor.duration = duration;
    // Arrangement length arrives with the snapshot, so no render probe is needed.
    setTransport(current => ({ ...current, duration_seconds: duration }));
  }

  return {
    audition,
    abPair,
    abBusy,
    abError,
    auditionActive,
    auditionKind,
    transport,
    scrubPosition,
    setScrubPosition,
    takeSolo,
    smoothPositionBeats,
    cursorActive,
    previewCursorBeats,
    previewTrackIndex,
    adoptTransport,
    startTransportPoll,
    play,
    pause,
    stop,
    seek,
    seekToMarker,
    togglePlayPause,
    previewDrumPad,
    previewMarketSample,
    stopMarketSample,
    marketSampleAction,
    auditionTake,
    previewPreset,
    auditionFactoryPatch,
    compareFactoryPatches,
    switchAb,
    clearAbPair,
    previewPattern,
    toggleAudition,
    auditionAction,
    playbackPlaying,
    auditionPositionSeconds,
    auditionLocalBeats,
    auditionStartBeats,
    auditionArrangementBeats,
    stopAudition,
    playDryKey,
    toggleTrackPreview,
    previewTrack,
    previewBlock,
    endPlaybackForProjectReplacement,
    resetPosition,
    adoptArrangementDuration,
  };
}
