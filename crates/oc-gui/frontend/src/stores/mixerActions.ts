import { isParameterRequestOwner, type ParameterRequestOwner } from "./parameterAdmission";
import type { AutomationMode, BarBeat, LaneLfo, PluginPortInfo, Project, ProjectSnapshot } from "../types/project";
import * as api from "./api";

export function createMixerActions(
  project: Project,
  applyWithUndo: (snapshot: ProjectSnapshot) => void,
  setError: (message: string) => void,
  requestOwner: () => ParameterRequestOwner,
) {
  async function updateMasterVolume(volumeDb: number) {
    try {
      const snap = await api.setMasterVolume(volumeDb);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function updateTrackMix(trackIndex: number, volumeDb?: number, pan?: number) {
    try {
      const snap = await api.updateTrackMix(trackIndex, volumeDb, pan);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function toggleMute(trackIndex: number) {
    const track = project.tracks[trackIndex];
    if (!track) return;
    try {
      const snap = await api.setTrackMute(trackIndex, !track.mix.mute);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function toggleSolo(trackIndex: number) {
    const track = project.tracks[trackIndex];
    if (!track) return;
    try {
      const snap = await api.setTrackSolo(trackIndex, !track.mix.solo);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function addControlRoute(owner: string, target: string, source: string): Promise<string | null> {
    try {
      applyWithUndo(await api.addControlRoute(owner, target, source));
      return null;
    } catch (e) {
      return String(e);
    }
  }

  /** Remove a control input; returns the refusal text when it is already gone. */
  async function removeControlRoute(owner: string, target: string): Promise<string | null> {
    try {
      applyWithUndo(await api.removeControlRoute(owner, target));
      return null;
    } catch (e) {
      return String(e);
    }
  }

  async function addSend(
    trackIndex: number,
    busName: string,
    amount: number,
    preFader = false,
    source?: { owner: string; output: number },
  ) {
    try {
      const snap = await api.addSend(
        trackIndex,
        busName,
        amount,
        preFader,
        source?.owner ?? null,
        source?.output ?? null,
      );
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setSendAmount(
    trackIndex: number,
    busName: string,
    amount: number,
    source?: { owner: string; output: number },
  ) {
    try {
      applyWithUndo(await api.setSend(
        trackIndex, busName, amount, null, null,
        source?.owner ?? null, source?.output ?? null,
      ));
    } catch (e) { setError(String(e)); }
  }

  async function setSendPreFader(
    trackIndex: number,
    busName: string,
    preFader: boolean,
    source?: { owner: string; output: number },
  ) {
    try {
      applyWithUndo(await api.setSend(
        trackIndex, busName, null, preFader, null,
        source?.owner ?? null, source?.output ?? null,
      ));
    } catch (e) { setError(String(e)); }
  }

  async function setSendPan(
    trackIndex: number,
    busName: string,
    pan: number,
    source?: { owner: string; output: number },
  ) {
    try {
      applyWithUndo(await api.setSend(
        trackIndex, busName, null, null, pan,
        source?.owner ?? null, source?.output ?? null,
      ));
    } catch (e) { setError(String(e)); }
  }

  /** Stereo output pairs of a preset's instrument; loads the plugin if needed. */
  async function presetOutputPairs(presetName: string): Promise<number> {
    try {
      return await api.presetOutputPairs(presetName);
    } catch (e) {
      setError(String(e));
      return 1;
    }
  }

  /** Declared output ports of a preset's instrument; never loads it by itself. */
  async function presetOutputPorts(
    presetName: string,
    load = false,
  ): Promise<PluginPortInfo[]> {
    try {
      return await api.presetOutputPorts(presetName, load);
    } catch (e) {
      setError(String(e));
      return [];
    }
  }

  async function setTrackOutputPort(trackIndex: number, port: number): Promise<boolean> {
    try {
      applyWithUndo(await api.setTrackOutputPort(trackIndex, port));
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  async function setTrackChannelOverrides(
    trackIndex: number,
    patch: {
      displayName?: string | null;
      color?: string | null;
      soloSafe?: boolean;
      polarityInverted?: boolean;
    },
  ) {
    try {
      applyWithUndo(await api.setTrackChannelOverrides(
        trackIndex,
        patch.displayName ?? null,
        patch.color ?? null,
        patch.soloSafe ?? null,
        patch.polarityInverted ?? null,
      ));
    } catch (e) { setError(String(e)); }
  }

  async function setTrackOutput(trackIndex: number, output: string): Promise<boolean> {
    try {
      applyWithUndo(await api.setTrackOutput(trackIndex, output));
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  /** Move one existing send to another bus without changing its level or tap. */
  async function setSendTarget(
    trackIndex: number,
    fromBus: string,
    toBus: string,
    source?: { owner: string; output: number },
  ): Promise<boolean> {
    try {
      applyWithUndo(await api.setSendTarget(
        trackIndex, fromBus, toBus, source?.owner ?? null, source?.output ?? null,
      ));
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  async function removeSend(
    trackIndex: number,
    busName: string,
    source?: { owner: string; output: number },
  ) {
    try {
      const snap = await api.removeSend(
        trackIndex, busName, source?.owner ?? null, source?.output ?? null,
      );
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  /** Insert one effect; returns the host's refusal text when it is not loadable. */
  async function addEffect(target: string, effectType: string): Promise<string | null> {
    try {
      const snap = await api.addEffect(target, effectType);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      setError(String(e));
      return String(e);
    }
  }

  async function removeEffect(target: string, effectIndex: number) {
    try {
      const snap = await api.removeEffect(target, effectIndex);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  /** Reorder one effect; reports whether the backend accepted the move. */
  async function moveEffect(target: string, effectIndex: number, newIndex: number): Promise<boolean> {
    try {
      const snap = await api.moveEffect(target, effectIndex, newIndex);
      applyWithUndo(snap);
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  /** Admission and response publication retain the same document through awaits. */
  async function effectMutation(mutate: (token: api.UndoGestureToken) => Promise<ProjectSnapshot>, isCurrent: () => boolean = () => true) {
    const ownsDocument = isParameterRequestOwner(isCurrent) ? isCurrent : requestOwner();
    const owns = () => ownsDocument() && isCurrent();
    let token: api.UndoGestureToken | null = null;
    try {
      if (!owns()) return;
      token = await api.beginParameterGesture("preset", ownsDocument.requireStamp());
      if (!owns()) return;
      const snapshot = await mutate(token);
      if (owns()) applyWithUndo(snapshot);
    } catch (error) {
      if (owns()) setError(String(error));
    } finally {
      if (token) {
        try { await api.endOwnedUndoGesture(token); }
        catch (error) { if (owns()) setError(String(error)); }
      }
    }
  }

  async function setEffectBypass(target: string, effectIndex: number, bypass: boolean) {
    await effectMutation(token => api.setEffectBypassOwned(token, target, effectIndex, bypass));
  }

  async function setEffectParam(target: string, effectIndex: number, paramName: string, paramValue: string, isCurrent?: () => boolean) {
    await effectMutation(token => api.setEffectParamOwned(token, target, effectIndex, paramName, paramValue), isCurrent);
  }

  async function setEffectParams(target: string, effectIndex: number, params: [string, string][], owner?: ParameterRequestOwner) {
    await effectMutation(token => api.setEffectParamsOwned(token, target, effectIndex, params), owner);
  }

  /**
   * Apply one device preset to a chain entry. The backend writes every parameter
   * in one step, so the edit lands as one undo entry; a refusal is thrown for the
   * panel to show instead of landing in the global error line.
   */
  async function applyDevicePreset(
    id: string,
    target: string,
    effectIndex: number,
    device: string,
  ): Promise<ProjectSnapshot> {
    const snap = await api.applyDevicePreset(id, target, effectIndex, device);
    applyWithUndo(snap);
    return snap;
  }

  async function setEffectSidechain(target: string, effectIndex: number, source: string | null) {
    try {
      applyWithUndo(await api.setEffectSidechain(target, effectIndex, source));
    } catch (e) { setError(String(e)); }
  }

  async function setAutomationSmoothing(
    target: string,
    laneParam: string,
    smoothingMs: number,
    laneIndex: number | null = null,
  ) {
    try {
      applyWithUndo(await api.setAutomationSmoothing(target, laneParam, smoothingMs, laneIndex));
    } catch (e) { setError(String(e)); }
  }

  async function setAutomationPointCurve(
    target: string,
    laneParam: string,
    pointIndex: number,
    curve: string,
    tension: number | null,
    laneIndex: number | null = null,
  ) {
    try {
      applyWithUndo(await api.setAutomationPointCurve(target, laneParam, pointIndex, curve, tension, laneIndex));
    } catch (e) { setError(String(e)); }
  }

  async function setAutomationMode(
    target: string,
    laneParam: string,
    mode: AutomationMode,
    laneIndex: number | null = null,
  ) {
    try {
      applyWithUndo(await api.setAutomationMode(target, laneParam, mode, laneIndex));
    } catch (e) { setError(String(e)); }
  }

  async function setAutomationLfo(
    target: string,
    laneParam: string,
    lfo: LaneLfo,
    laneIndex: number | null = null,
  ) {
    try {
      applyWithUndo(await api.setAutomationLfo(target, laneParam, lfo, laneIndex));
    } catch (e) { setError(String(e)); }
  }

  /**
   * Point edits on a timeline automation row. Every call is one undo entry; a
   * drag wraps its single commit in an undo gesture at the call site.
   */
  async function addAutomationPoint(
    target: string,
    laneParam: string,
    laneIndex: number | null,
    positionBeats: number,
    value: number,
  ) {
    try {
      applyWithUndo(await api.addAutomationPoint(target, laneParam, laneIndex, positionBeats, value));
    } catch (e) { setError(String(e)); }
  }

  async function removeAutomationPoint(
    target: string,
    laneParam: string,
    laneIndex: number | null,
    pointIndex: number,
  ) {
    try {
      applyWithUndo(await api.removeAutomationPoint(target, laneParam, laneIndex, pointIndex));
    } catch (e) { setError(String(e)); }
  }

  async function moveAutomationPoint(
    target: string,
    laneParam: string,
    laneIndex: number | null,
    pointIndex: number,
    positionBeats: number,
    value: number,
  ) {
    try {
      applyWithUndo(await api.moveAutomationPoint(target, laneParam, laneIndex, pointIndex, positionBeats, value));
    } catch (e) { setError(String(e)); }
  }

  async function setAutomationLaneMute(
    target: string,
    laneParam: string,
    laneIndex: number | null,
    muted: boolean,
  ) {
    try {
      applyWithUndo(await api.setAutomationLaneMute(target, laneParam, laneIndex, muted));
    } catch (e) { setError(String(e)); }
  }

  /**
   * Block gestures on an automation clip.
   *
   * Every call is one undo entry. A refusal reaches the backend, which answers
   * with the sentence the status bar shows; the snapshot is not applied, so the
   * block stays where it was.
   */
  async function moveAutomationClip(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
    to: BarBeat,
  ) {
    try {
      applyWithUndo(await api.moveAutomationClip(
        owner, laneParam, laneIndex, to.bar, to.beat, to.subdivision,
      ));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  async function resizeAutomationClip(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
    edge: "start" | "end",
    to: BarBeat,
  ) {
    try {
      applyWithUndo(await api.resizeAutomationClip(
        owner, laneParam, laneIndex, edge, to.bar, to.beat, to.subdivision,
      ));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  async function duplicateAutomationClip(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
    at: BarBeat,
  ) {
    try {
      applyWithUndo(await api.duplicateAutomationClip(
        owner, laneParam, laneIndex, at.bar, at.beat, at.subdivision,
      ));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  async function splitAutomationClip(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
    at: BarBeat,
  ) {
    try {
      applyWithUndo(await api.splitAutomationClip(
        owner, laneParam, laneIndex, at.bar, at.beat, at.subdivision,
      ));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  async function deleteAutomationClip(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
  ) {
    try {
      applyWithUndo(await api.deleteAutomationClip(owner, laneParam, laneIndex));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  /** Remove one lane, whole-timeline or clip; one undo entry on the backend. */
  async function removeAutomationLane(
    owner: string,
    laneParam: string,
    laneIndex: number | null,
  ) {
    try {
      applyWithUndo(await api.removeAutomationLane(owner, laneParam, laneIndex));
    } catch (e) { setError(String(e)); return String(e); }
    return null;
  }

  async function addBus(name: string, routes: string[]): Promise<string | null> {
    try {
      const snap = await api.addBus(name, routes);
      applyWithUndo(snap);
      return name;
    } catch (e) { setError(String(e)); return null; }
  }

  async function setBusRoutes(name: string, routes: string[]): Promise<boolean> {
    try {
      const snap = await api.setBusRoutes(name, routes);
      applyWithUndo(snap);
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  async function updateBusMix(
    name: string,
    volumeDb?: number,
    mute?: boolean,
    solo?: boolean,
  ) {
    try {
      const snap = await api.updateBusMix(name, volumeDb ?? null, mute ?? null, solo ?? null);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  /** Delete an unused bus. The backend rejects a referenced bus. */
  async function removeBus(name: string): Promise<boolean> {
    try {
      const snap = await api.removeBus(name, false);
      applyWithUndo(snap);
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  /** Delete a bus and explicitly re-route its references to master. */
  async function removeBusReroute(name: string): Promise<boolean> {
    try {
      const snap = await api.removeBus(name, true);
      applyWithUndo(snap);
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  return {
    updateMasterVolume,
    updateTrackMix,
    toggleMute,
    toggleSolo,
    addControlRoute,
    removeControlRoute,
    addSend,
    setSendAmount,
    setSendPreFader,
    setSendPan,
    presetOutputPairs,
    presetOutputPorts,
    setTrackOutputPort,
    setTrackChannelOverrides,
    setTrackOutput,
    setSendTarget,
    removeSend,
    addEffect,
    removeEffect,
    moveEffect,
    setEffectBypass,
    setEffectParam,
    setEffectParams,
    applyDevicePreset,
    setEffectSidechain,
    setAutomationSmoothing,
    setAutomationPointCurve,
    setAutomationMode,
    setAutomationLfo,
    addAutomationPoint,
    removeAutomationPoint,
    moveAutomationPoint,
    setAutomationLaneMute,
    moveAutomationClip,
    resizeAutomationClip,
    duplicateAutomationClip,
    splitAutomationClip,
    deleteAutomationClip,
    removeAutomationLane,
    addBus,
    setBusRoutes,
    updateBusMix,
    removeBus,
    removeBusReroute,
  };
}
