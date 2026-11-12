import { audioClipBounds } from "../utils/audioClipPlacement";
/**
 * One dispatch path for the three clip kinds.
 *
 * The timeline's menus, its Delete command and its drag commits address a clip
 * by its [`ClipKey`] and land here. Each call resolves the key against the clip
 * pool and forwards to the store method that owns that kind, so a new gesture
 * needs one entry here instead of three branches in the component.
 *
 * A refusal comes back as the sentence the backend produced; the caller shows
 * it verbatim. The frontend only invents a reason where no command exists (a
 * whole-timeline lane, a duplicate of an audio clip).
 */
import { clipInfoUi } from "./clipInfo";
import { COLOUR_PALETTE } from "../utils/colourPalette";
import type { ContextMenuItem } from "../types/contextMenu";
import {
  MIN_LOOP_QUARTERS,
  audioClipLoopDefault,
  audioClipLoopReason,
  fileName,
  findPattern,
  patternContentQuarters,
  isRestEvent,
  patternLengthQuarters,
  roundLoopSeconds,
} from "../utils/clipContent";
import { patternHitOnsets } from "../utils/drumkit";
import { ownerAutomationLanes } from "../utils/automationTarget";
import { CLIP_GAIN_MAX_DB, CLIP_GAIN_MIN_DB, formatClipGain } from "../utils/waveform";
import { createProjectClock } from "../utils/projectClock";
import { BLOCK_POSITION_QUARTERS } from "./grid";
import { withUndoGesture } from "./gesture";
import { gainEnvelopeWindow } from "./gainEnvelopeWindow";
import { i18n } from "./i18n";
import { bounceStore } from "./bounce";
import { panelStore } from "./panels";
import { store, type TimelineBlockRef } from "./project";
import {
  audioKey,
  automationKey,
  clipAt,
  isWholeTimeline,
  patternKey,
  positionText,
  type ClipRef,
} from "./clipPool";
import type {
  AudioClip,
  AutomationLane,
  BarBeat,
  Clip,
  Pattern,
  Phrase,
  TimeRange,
} from "../types/project";

/** Which edge a resize moves. */
export type ClipEdge = "start" | "end";

/** Where a move puts a clip: its new start and, optionally, another track. */
export interface ClipMove extends BarBeat {
  /** Destination track index, for a pattern clip that changes track. */
  trackIndex?: number;
  /** Destination track name, for an audio clip that changes track. */
  trackName?: string;
}

/** Gestures the capability matrix answers for. */
export type ClipAction = "move" | "resize" | "delete" | "copy" | "duplicate" | "split";

export interface ClipCapability {
  allowed: boolean;
  /** Why the gesture is refused; present exactly when `allowed` is false. */
  reason?: string;
}

/**
 * App hooks the menu cannot own itself: the rename dialog and the local audio
 * and automation picks the timeline keeps. The timeline installs them on mount.
 */
export interface ClipUiHooks {
  renamePattern: (pattern: string) => void;
  /** Drop a local pick once its clip is gone. */
  forget: (clip: ClipRef) => void;
}

let hooks: ClipUiHooks | null = null;

export function setClipUiHooks(next: ClipUiHooks | null): void {
  hooks = next;
}

const missing = () => i18n.t("clip.poolMissing");
const wholeTimeline = () => i18n.t("timeline.automation.wholeTimeline");
const splitHint = () => i18n.t("menu.clip.splitHint");

function clock() {
  return createProjectClock(store.project.properties);
}

function playhead(): number {
  return store.transport().position_beats;
}

function patternClip(ref: ClipRef): Clip | null {
  if (ref.trackIndex === null || ref.clipIndex === null) return null;
  return store.project.tracks[ref.trackIndex]?.clips[ref.clipIndex] ?? null;
}

function audioClip(ref: ClipRef): AudioClip | null {
  if (ref.audioIndex === null) return null;
  return (store.project.properties.audio_clips ?? [])[ref.audioIndex] ?? null;
}

function automationLane(ref: ClipRef): AutomationLane | null {
  if (ref.owner === null || ref.laneIndex === null) return null;
  return ownerAutomationLanes(store.project, ref.owner)[ref.laneIndex] ?? null;
}

function rangeQuarters(range: TimeRange): number {
  const current = clock();
  return current.quarterAt(range.end) - current.quarterAt(range.start);
}

/** Whether `beats` sits inside the range and can be split there. */
function canSplitAt(range: TimeRange | null | undefined, margin: number): boolean {
  if (!range) return false;
  const current = clock();
  const at = playhead();
  return at >= current.quarterAt(range.start) + margin
    && at <= current.quarterAt(range.end) - margin;
}

function splitPosition(): BarBeat {
  return clock().barBeatAt(playhead());
}

/** Capability matrix: what each kind offers, and why the rest is refused. */
export function clipCapability(key: string, action: ClipAction): ClipCapability {
  const ref = clipAt(key);
  if (!ref) return { allowed: false, reason: missing() };
  const reason = refusal(ref, action);
  return reason === null ? { allowed: true } : { allowed: false, reason };
}

function refusal(ref: ClipRef, action: ClipAction): string | null {
  if (isWholeTimeline(ref)) return wholeTimeline();
  if (ref.kind === "audio" && (action === "copy" || action === "duplicate")) {
    return i18n.t("clip.noDuplicate");
  }
  return null;
}

/** Pick a clip; a grouped pattern clip takes its whole group. */
export function selectClip(key: string): boolean {
  const ref = clipAt(key);
  if (!ref) return false;
  store.setSelectedPatternName(null);
  store.setEditorSelection(null);
  if (ref.kind === "pattern") {
    const clip = patternClip(ref);
    if (!clip) return false;
    const trackIndex = ref.trackIndex!;
    const clipIndex = ref.clipIndex!;
    const group = store.groupOfClip(trackIndex, clipIndex);
    const picks = group
      ? store.groupMembers(group).map((member) => ({
        kind: "clip" as const,
        trackIndex: member.trackIndex,
        index: member.clipIndex,
      }))
      : [{ kind: "clip" as const, trackIndex, index: clipIndex }];
    store.setBlockSelection(picks);
    store.setSelectedTrackIndex(trackIndex);
    store.setSelectedPatternName(clip.pattern);
    store.setEditorSelection({
      trackIndex,
      startBeat: clock().quarterAt(clip.time_range.start),
      patternName: clip.pattern,
    });
    return true;
  }
  store.setBlockSelection([]);
  const trackIndex = ref.kind === "audio"
    ? store.project.tracks.findIndex((track) => track.name === audioClip(ref)?.track)
    : ref.trackIndex;
  if (trackIndex !== null && trackIndex >= 0) store.setSelectedTrackIndex(trackIndex);
  return true;
}

/** Move a clip to `delta`; a grouped pattern clip moves its whole group. */
export async function moveClip(key: string, delta: ClipMove): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "move");
  if (blocked) return blocked;
  if (ref.kind === "pattern") {
    const trackIndex = ref.trackIndex!;
    const clipIndex = ref.clipIndex!;
    if (store.groupOfClip(trackIndex, clipIndex)) {
      return store.moveClipGroup(trackIndex, clipIndex, delta.bar, delta.beat, delta.subdivision);
    }
    if (delta.trackIndex !== undefined && delta.trackIndex !== trackIndex) {
      return store.moveClipToTrack(
        trackIndex, clipIndex, delta.trackIndex,
        delta.bar, delta.beat, delta.subdivision,
      );
    }
    return store.moveClip(trackIndex, clipIndex, delta.bar, delta.beat, delta.subdivision);
  }
  if (ref.kind === "audio") {
    return store.moveAudioClip(
      ref.audioIndex!, delta.bar, delta.beat, delta.subdivision, delta.trackName ?? null,
    );
  }
  const lane = automationLane(ref);
  if (!lane) return missing();
  return store.moveAutomationClip(ref.owner!, lane.param, ref.laneIndex, delta);
}

/** Move one edge to `position`, keeping the other edge where it is. */
export async function resizeClip(
  key: string,
  edge: ClipEdge,
  position: BarBeat,
): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "resize");
  if (blocked) return blocked;
  if (ref.kind === "pattern") {
    // The backend resizes a pattern clip from its end; a start drag has no command.
    if (edge === "start") return i18n.t("clip.resizeEndOnly");
    return store.resizeClip(
      ref.trackIndex!, ref.clipIndex!,
      position.bar, position.beat, position.subdivision,
    );
  }
  if (ref.kind === "audio") {
    const clip = audioClip(ref);
    if (!clip) return missing();
    const start = edge === "start" ? position : clip.time_range.start;
    const end = edge === "end" ? position : clip.time_range.end;
    return store.trimAudioClip(
      ref.audioIndex!,
      start.bar, start.beat, start.subdivision,
      end.bar, end.beat, end.subdivision, edge,
    );
  }
  const lane = automationLane(ref);
  if (!lane) return missing();
  return store.resizeAutomationClip(ref.owner!, lane.param, ref.laneIndex, edge, position);
}

/** Remove one clip through the command that owns its kind. */
/** Delete every clip the menu's selection covers, as one undo step. */
async function deleteMenuClips(clips: { trackIndex: number; clipIndex: number }[]) {
  const blocks = clips.map((clip) => ({
    kind: "clip" as const,
    trackIndex: clip.trackIndex,
    index: clip.clipIndex,
  }));
  await store.removeBlocks(blocks);
}

export async function deleteClip(key: string): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "delete");
  if (blocked) return blocked;
  let failure: string | null;
  if (ref.kind === "pattern") {
    failure = await store.removeClip(ref.trackIndex!, ref.clipIndex!);
  } else if (ref.kind === "audio") {
    failure = await store.removeAudioClip(ref.audioIndex!);
  } else {
    const lane = automationLane(ref);
    if (!lane) return missing();
    failure = await store.deleteAutomationClip(ref.owner!, lane.param, ref.laneIndex);
  }
  if (failure === null) {
    store.setBlockSelection([]);
    store.setEditorSelection(null);
    hooks?.forget(ref);
  }
  return failure;
}

/**
 * Remove one automation lane whether it is a clip or drives the whole timeline.
 *
 * The clip gestures refuse a whole-timeline lane because it has no edges to
 * grab; this is the lane's own delete, and the backend records one undo entry.
 */
export async function removeAutomationLane(key: string): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref || ref.kind !== "automation") return missing();
  const lane = automationLane(ref);
  if (!lane) return missing();
  const failure = await store.removeAutomationLane(ref.owner!, lane.param, ref.laneIndex);
  if (failure === null) {
    store.setBlockSelection([]);
    store.setEditorSelection(null);
    hooks?.forget(ref);
  }
  return failure;
}

/** Copy a clip to `at`, keeping its length and, when asked, changing track. */
export async function copyClip(key: string, at: ClipMove): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "copy");
  if (blocked) return blocked;
  if (ref.kind === "pattern") {
    const clip = patternClip(ref);
    if (!clip) return missing();
    const end = clock().barBeatAt(clock().quarterAt(at) + rangeQuarters(clip.time_range));
    return store.addClip(
      at.trackIndex ?? ref.trackIndex!, clip.pattern,
      at.bar, at.beat, at.subdivision,
      end.bar, end.beat, end.subdivision,
      clip.offset_quarters, clip.loop_quarters,
    );
  }
  const lane = automationLane(ref);
  if (!lane) return missing();
  const failure = await store.duplicateAutomationClip(
    ref.owner!, lane.param, ref.laneIndex, at,
  );
  // A copy inserts a lane, so the stored index may move under the local pick.
  if (failure === null) hooks?.forget(ref);
  return failure;
}

/** Duplicate a clip right after itself. */
export async function duplicateClip(key: string): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "duplicate");
  if (blocked) return blocked;
  if (ref.kind === "pattern") {
    const clip = patternClip(ref);
    if (!clip) return missing();
    const trackIndex = ref.trackIndex!;
    const start = clock().barBeatAt(clock().quarterAt(clip.time_range.end));
    const end = clock().barBeatAt(
      clock().quarterAt(clip.time_range.end) + rangeQuarters(clip.time_range),
    );
    const added = store.project.tracks[trackIndex]?.clips.length ?? 0;
    const failure = await store.addClip(
      trackIndex, clip.pattern,
      start.bar, start.beat, start.subdivision,
      end.bar, end.beat, end.subdivision,
      clip.offset_quarters, clip.loop_quarters,
    );
    if (failure === null) selectClip(patternKey(trackIndex, added));
    return failure;
  }
  const lane = automationLane(ref);
  const range = lane?.time_range;
  if (!lane || !range) return wholeTimeline();
  const failure = await store.duplicateAutomationClip(
    ref.owner!, lane.param, ref.laneIndex,
    clock().barBeatAt(clock().quarterAt(range.end)),
  );
  if (failure === null) hooks?.forget(ref);
  return failure;
}

/**
 * Split a clip at the playhead, when the playhead sits inside it.
 *
 * A pattern clip splits through one command, so one undo restores it and a
 * refusal leaves it in place. The backend carries the playback window into both
 * halves, which keeps a slipped or looped clip sounding as it did.
 */
export async function splitClip(key: string): Promise<string | null> {
  const ref = clipAt(key);
  if (!ref) return missing();
  const blocked = refusal(ref, "split");
  if (blocked) return blocked;
  if (ref.kind === "pattern") {
    const clip = patternClip(ref);
    if (!clip) return missing();
    if (!canSplitAt(clip.time_range, 0.01)) return splitHint();
    const at = splitPosition();
    return store.splitClip(
      ref.trackIndex!, ref.clipIndex!, at.bar, at.beat, at.subdivision,
    );
  }
  if (ref.kind === "audio") {
    const clip = audioClip(ref);
    if (!clip) return missing();
    const bounds = audioClipBounds(clock(), clip);
    if (playhead() <= bounds.startQuarter + 0.01 || playhead() >= bounds.endQuarter - 0.01) return splitHint();
    const at = splitPosition();
    return store.splitAudioClip(
      ref.audioIndex!, at.bar, at.beat, at.subdivision,
    );
  }
  const lane = automationLane(ref);
  if (!lane) return missing();
  if (!canSplitAt(lane.time_range, BLOCK_POSITION_QUARTERS)) {
    return i18n.t("timeline.automation.splitHint");
  }
  const at = splitPosition();
  const failure = await store.splitAutomationClip(
    ref.owner!, lane.param, ref.laneIndex, at,
  );
  if (failure === null) hooks?.forget(ref);
  return failure;
}

// ── Clipboard ──
// A copied block keeps its payload, so pasting after the source moved or was
// deleted still places the content the user copied.

function blockOf(selection: TimelineBlockRef): Clip | Phrase | Pattern | null {
  const track = store.project.tracks[selection.trackIndex];
  if (!track) return null;
  const blocks = selection.kind === "clip"
    ? track.clips
    : selection.kind === "phrase"
      ? track.phrases
      : track.patterns;
  return blocks[selection.index] ?? null;
}

export function copyBlockToClipboard(selection: TimelineBlockRef): void {
  const block = blockOf(selection);
  if (!block?.time_range) return;
  const durationBeats = Math.max(0.25, rangeQuarters(block.time_range));
  const base = {
    durationBeats,
    sourceTrackIndex: selection.trackIndex,
    sourceStart: block.time_range.start,
  };
  if (selection.kind === "clip") {
    const clip = block as Clip;
    store.setClipClipboard({
      ...base,
      kind: "clip",
      patternName: clip.pattern,
      offsetQuarters: clip.offset_quarters,
      loopQuarters: clip.loop_quarters,
    });
  } else if (selection.kind === "phrase") {
    store.setClipClipboard({
      ...base,
      kind: "phrase",
      phrase: JSON.parse(JSON.stringify(block)) as Phrase,
    });
  } else {
    store.setClipClipboard({
      ...base,
      kind: "pattern",
      pattern: JSON.parse(JSON.stringify(block)) as Pattern,
    });
  }
}

export function copyClipToClipboard(key: string): void {
  const ref = clipAt(key);
  if (!ref || ref.kind !== "pattern") return;
  copyBlockToClipboard({
    kind: "clip",
    trackIndex: ref.trackIndex!,
    index: ref.clipIndex!,
  });
}

/** Copy a clip, then remove it. */
export async function cutClip(key: string): Promise<string | null> {
  copyClipToClipboard(key);
  return deleteClip(key);
}

/** Place the copied block at the playhead, on its own track or the asked one. */
export async function pasteBlockAtPlayhead(targetTrackIndex?: number): Promise<void> {
  const clipboard = store.clipClipboard();
  if (!clipboard) return;
  const trackIndex = targetTrackIndex ?? clipboard.sourceTrackIndex;
  const track = store.project.tracks[trackIndex];
  if (!track) return;
  const current = clock();
  const playheadQuarter = Math.max(0, playhead());
  const start = current.barBeatAt(playheadQuarter);
  const end = current.barBeatAt(playheadQuarter + clipboard.durationBeats);
  const added = clipboard.kind === "clip"
    ? track.clips.length
    : clipboard.kind === "phrase"
      ? track.phrases.length
      : track.patterns.length;
  if (clipboard.kind === "clip") {
    const failure = await store.addClip(
      trackIndex, clipboard.patternName,
      start.bar, start.beat, start.subdivision,
      end.bar, end.beat, end.subdivision,
      clipboard.offsetQuarters, clipboard.loopQuarters,
    );
    if (failure === null) selectClip(patternKey(trackIndex, added));
    return;
  }
  if (clipboard.kind === "phrase") {
    await store.insertPhrase(
      trackIndex, clipboard.phrase,
      start.bar, start.beat, start.subdivision,
    );
  } else {
    await store.insertInlinePattern(
      trackIndex, clipboard.pattern,
      start.bar, start.beat, start.subdivision,
    );
  }
  store.setBlockSelection([{ kind: clipboard.kind, trackIndex, index: added }]);
}

// ── Menus ──

/** The eight theme accent colours a pattern can carry, in menu order. */
function menuClips(ref: ClipRef): { trackIndex: number; clipIndex: number }[] {
  const selection = store.blockSelection().filter((block) => block.kind === "clip");
  const inside = selection.some(
    (block) => block.trackIndex === ref.trackIndex && block.index === ref.clipIndex,
  );
  if (inside) {
    return selection.map((block) => ({ trackIndex: block.trackIndex, clipIndex: block.index }));
  }
  return [{ trackIndex: ref.trackIndex!, clipIndex: ref.clipIndex! }];
}

function groupedTogether(clips: { trackIndex: number; clipIndex: number }[]): boolean {
  if (clips.length < 2) return false;
  const first = store.groupOfClip(clips[0].trackIndex, clips[0].clipIndex);
  if (!first) return false;
  return clips.every(
    (clip) => store.groupOfClip(clip.trackIndex, clip.clipIndex) === first,
  );
}

function patternPreviewReason(ref: ClipRef): string | null {
  const track = ref.trackIndex === null ? null : store.project.tracks[ref.trackIndex];
  if (!track) return i18n.t("menu.clip.previewEmpty");
  if (track.mix.mute) return i18n.t("menu.clip.previewMuted");
  const clip = patternClip(ref);
  if (!clip) return i18n.t("menu.clip.previewNoLength");
  const current = clock();
  const start = current.quarterAt(clip.time_range.start);
  const end = current.quarterAt(clip.time_range.end);
  if (end <= start) return i18n.t("menu.clip.previewNoLength");
  const pattern = findPattern(clip.pattern);
  if (!pattern) return i18n.t("menu.clip.previewUnknownPattern");
  if (pattern.events.some((event) => !isRestEvent(event) && !event.muted)) return null;
  const window = {
    start, end, offset: clip.offset_quarters ?? 0, loop: clip.loop_quarters ?? 0,
    swing: pattern.swing ?? store.project.properties.swing,
  };
  return pattern.hits.some((hit) => !patternHitOnsets(hit, window).next().done)
    ? null : i18n.t("menu.clip.previewEmpty");
}

function patternMenu(ref: ClipRef): ContextMenuItem[] {
  const clip = patternClip(ref);
  if (!clip) return [];
  const trackIndex = ref.trackIndex!;
  const clipIndex = ref.clipIndex!;
  const key = ref.key;
  const rangeBeats = rangeQuarters(clip.time_range);
  const canSplit = canSplitAt(clip.time_range, 0.01);
  const preview = patternPreviewReason(ref);
  const pattern = findPattern(clip.pattern);
  const offset = clip.offset_quarters ?? 0;
  const declaredEnd = pattern?.bars != null ? patternLengthQuarters(pattern) : 0;
  const loopEnd = Math.max(patternContentQuarters(pattern), declaredEnd);
  const headroom = Math.min(Math.max(0, loopEnd - offset), rangeBeats);
  const loopBeats = clip.loop_quarters ?? 0;
  const looping = loopBeats > 0;
  const loopReason = rangeBeats <= MIN_LOOP_QUARTERS
    ? i18n.t("clip.loopNeedsLength")
    : headroom < MIN_LOOP_QUARTERS
      ? i18n.t("clip.loopNeedsContentPattern")
      : null;
  const declaredCycle = declaredEnd - offset;
  const loopDefault = declaredCycle >= MIN_LOOP_QUARTERS && declaredCycle < rangeBeats - 1e-9
    ? declaredCycle
    : Math.min(headroom, Math.max(MIN_LOOP_QUARTERS, rangeBeats / 2));
  const selection = menuClips(ref);
  const grouped = groupedTogether(selection);
  return [
    { type: "heading", label: ref.label },
    {
      label: i18n.t("menu.clip.preview"),
      disabled: preview !== null,
      hint: preview ?? undefined,
      onSelect: () => void store.previewBlock(
        trackIndex, clip.time_range.start, clip.time_range.end,
      ),
    },
    {
      // A pattern clip's editor is the piano roll; the inspector stays behind
      // "Inspect clip", which exists for the metadata table.
      label: i18n.t("menu.clip.openEditor"),
      onSelect: () => {
        selectClip(key);
        panelStore.openBottom("pianoroll");
      },
    },
    {
      label: i18n.t("menu.clip.rename"),
      onSelect: () => hooks?.renamePattern(clip.pattern),
    },
    {
      // A clip takes its pattern's colour, so the palette lives here too.
      type: "palette",
      label: i18n.t("menu.clip.colour"),
      colours: COLOUR_PALETTE.map((colour) => ({
        hex: colour.hex,
        label: i18n.t(colour.key),
      })),
      value: pattern?.color ?? null,
      onPick: (hex) => void store.setPatternColor(clip.pattern, hex),
      onCustom: (hex) => void store.setPatternColor(clip.pattern, hex),
      resetLabel: i18n.t("menu.clip.colourDefault"),
      onReset: () => void store.setPatternColor(clip.pattern, ""),
      resetDisabled: !pattern?.color,
    },
    { type: "separator" },
    {
      // Editing verbs and clip-content operations are grouped: fifteen flat rows
      // made the menu unreadable, and most of them have shortcuts anyway.
      type: "group",
      label: i18n.t("menu.clip.groupEdit"),
      items: [
        {
          label: i18n.t("menu.clip.copy"),
          shortcut: "Cmd/Ctrl+C",
          onSelect: () => copyClipToClipboard(key),
        },
        {
          label: i18n.t("menu.clip.cut"),
          shortcut: "Cmd/Ctrl+X",
          onSelect: () => void cutClip(key),
        },
        {
          label: i18n.t("menu.clip.paste"),
          shortcut: "Cmd/Ctrl+V",
          disabled: !store.clipClipboard(),
          hint: store.clipClipboard() ? undefined : i18n.t("menu.clip.pasteHint"),
          onSelect: () => void pasteBlockAtPlayhead(trackIndex),
        },
        {
          label: i18n.t("menu.clip.duplicate"),
          shortcut: "Cmd/Ctrl+D",
          onSelect: () => void duplicateClip(key),
        },
      ],
    },
    {
      type: "group",
      label: i18n.t("menu.clip.groupContent"),
      items: [
        {
          label: i18n.t("menu.clip.split"),
          disabled: !canSplit,
          hint: canSplit ? undefined : splitHint(),
          onSelect: () => void splitClip(key),
        },
        {
          label: i18n.t("menu.clip.resetSlip"),
          disabled: (clip.offset_quarters ?? 0) <= 1e-9,
          hint: i18n.t("menu.clip.slipHintPattern"),
          onSelect: () => {
            void withUndoGesture("project", () =>
              store.setPatternClipOffset(trackIndex, clipIndex, 0),
            );
          },
        },
        {
          label: looping ? i18n.t("menu.clip.clearLoop") : i18n.t("menu.clip.loop"),
          disabled: !looping && loopReason !== null,
          hint: looping
            ? i18n.t("menu.clip.clearLoopHint")
            : (loopReason ?? i18n.t("menu.clip.loopHintPattern")),
          onSelect: () => {
            const next = looping ? null : loopDefault;
            void withUndoGesture("project", () =>
              store.setPatternClipLoop(trackIndex, clipIndex, next),
            );
          },
        },
      ],
    },
    {
      type: "group",
      label: i18n.t("menu.clip.groupSelection"),
      items: [
        {
          label: i18n.t("menu.clip.group"),
          shortcut: "Cmd/Ctrl+G",
          disabled: selection.length < 2 || grouped,
          hint: store.groupBusy()
            ? i18n.t("clip.groupPending")
            : selection.length < 2
              ? i18n.t("menu.clip.groupHint")
              : grouped
                ? i18n.t("menu.clip.groupedHint")
                : undefined,
          onSelect: () => void store.groupClips(selection),
        },
        {
          label: i18n.t("menu.clip.ungroup"),
          shortcut: "Cmd/Ctrl+Shift+G",
          disabled: store.groupOfClip(trackIndex, clipIndex) === null,
          hint: store.groupBusy()
            ? i18n.t("clip.groupPending")
            : store.groupOfClip(trackIndex, clipIndex) === null
              ? i18n.t("menu.clip.ungroupHint")
              : undefined,
          onSelect: () => void store.ungroupClips(selection),
        },
      ],
    },
    { type: "separator" },
    {
      // The keyboard path deletes the whole selection, so the menu must too:
      // one menu Delete and one keyboard Delete cannot mean different things.
      label: selection.length > 1
        ? i18n.t("menu.clip.deleteMany", { count: selection.length })
        : i18n.t("menu.clip.delete"),
      shortcut: "Delete",
      danger: true,
      onSelect: () => void deleteMenuClips(selection),
    },
  ];
}

function audioMenu(ref: ClipRef): ContextMenuItem[] {
  const clip = audioClip(ref);
  if (!clip) return [];
  const index = ref.audioIndex!;
  const key = ref.key;
  const current = clock();
  const { startQuarter, endQuarter, durationSeconds: clipSeconds } = audioClipBounds(current, clip);
  const at = playhead();
  const canSplit = at > startQuarter + 0.01 && at < endQuarter - 0.01;
  const loop = clip.loop_seconds ?? 0;
  const loopReason = audioClipLoopReason(clip, clipSeconds);
  const hasEnvelope = (clip.gain_envelope?.length ?? 0) > 0;
  const document = store.documentIdentity();
  const revision = store.projectRevision;
  const clipName = clip.name;
  const targetsClip = () => store.documentIdentity() === document
    && store.project.properties.audio_clips?.[index]?.name === clipName;
  const trackIndex = store.project.tracks.findIndex((track) => track.name === clip.track);
  const previewReason = trackIndex < 0 || !clip.file.trim()
    ? i18n.t("menu.clip.previewEmpty")
    : store.project.tracks[trackIndex].mix.mute
      ? i18n.t("menu.clip.previewMuted")
      : clip.muted
        ? i18n.t("menu.clip.previewAudioMuted")
        : endQuarter <= startQuarter
          ? i18n.t("menu.clip.previewNoLength")
          : null;
  return [
    { type: "heading", label: ref.label || fileName(clip.file) },
    {
      label: i18n.t("menu.clip.preview"),
      disabled: previewReason !== null,
      hint: previewReason ?? undefined,
      onSelect: () => void store.previewBlock(
        trackIndex, clip.time_range.start, clip.time_range.end, clip.name,
      ),
    },
    {
      label: i18n.t("clipPlayback.open"),
      onSelect: () => gainEnvelopeWindow.open(index, "playback"),
    },
    {
      type: "slider",
      label: i18n.t("menu.clip.gain"),
      value: clip.gain_db,
      min: CLIP_GAIN_MIN_DB,
      max: CLIP_GAIN_MAX_DB,
      step: 0.1,
      format: (value) => formatClipGain(value),
      onCommit: (value) => {
        if (!targetsClip()) return;
        void withUndoGesture("project", () =>
          store.setAudioClipEnvelope(index, { gain_db: value }),
        );
      },
    },
    {
      label: hasEnvelope
        ? i18n.t("menu.clip.editGainEnvelope")
        : i18n.t("menu.clip.addGainEnvelope"),
      onSelect: () => {
        if (!targetsClip()) return;
        if (hasEnvelope) {
          gainEnvelopeWindow.open(index, "env");
          return;
        }
        void withUndoGesture("project", () =>
          store.setAudioClipEnvelope(index, {
            gain_envelope: [
              { position: 0, gain_db: 0 },
              { position: 1, gain_db: 0 },
            ],
          }),
        ).then((applied) => {
          if (applied && targetsClip()) gainEnvelopeWindow.open(index, "env");
        });
      },
    },
    { type: "separator" },
    {
      type: "group",
      label: i18n.t("menu.clip.groupContent"),
      items: [
    {
      label: i18n.t("menu.clip.resetSlip"),
      disabled: (clip.offset_seconds ?? 0) <= 1e-6,
      hint: i18n.t("menu.clip.slipHint"),
      onSelect: () => {
        if (!targetsClip()) return;
        void withUndoGesture("project", () => store.setAudioClipOffset(index, 0, revision));
      },
    },
    {
      label: loop > 0 ? i18n.t("menu.clip.clearLoop") : i18n.t("menu.clip.loop"),
      disabled: loop > 0 ? false : loopReason !== null,
      hint: loop > 0
        ? i18n.t("menu.clip.clearLoopHint")
        : (loopReason ?? i18n.t("menu.clip.loopHint")),
      onSelect: () => {
        if (!targetsClip()) return;
        const next = loop > 0
          ? null
          : roundLoopSeconds(audioClipLoopDefault(clip, clipSeconds));
        void withUndoGesture("project", () => store.setAudioClipLoop(index, next, revision));
      },
    },
    {
      label: i18n.t("menu.clip.split"),
      disabled: !canSplit,
      hint: canSplit ? undefined : splitHint(),
      onSelect: () => void splitClip(key),
    },
    {
      label: i18n.t("menu.clip.consolidate"),
      hint: i18n.t("menu.clip.consolidateHint"),
      onSelect: () => bounceStore.openConsolidate(clip.track, [index]),
    },
      ],
    },
    { type: "separator" },
    {
      label: i18n.t("menu.clip.delete"),
      shortcut: "Delete",
      danger: true,
      onSelect: () => void deleteClip(key),
    },
  ];
}

function automationMenu(ref: ClipRef): ContextMenuItem[] {
  const lane = automationLane(ref);
  if (!lane) return [];
  const key = ref.key;
  const whole = isWholeTimeline(ref);
  const reason = whole ? wholeTimeline() : undefined;
  const canSplit = canSplitAt(lane.time_range, BLOCK_POSITION_QUARTERS);
  const range = lane.time_range;
  const heading = range
    ? `${ref.label} · ${positionText(range.start)} – ${positionText(range.end)}`
    : ref.label;
  return [
    { type: "heading", label: heading },
    {
      label: i18n.t("timeline.automation.copy"),
      disabled: whole,
      hint: reason,
      onSelect: () => void copyClip(key, clock().barBeatAt(playhead())),
    },
    {
      label: i18n.t("menu.clip.duplicate"),
      disabled: whole,
      hint: reason,
      onSelect: () => void duplicateClip(key),
    },
    {
      label: i18n.t("menu.clip.split"),
      disabled: whole || !canSplit,
      hint: reason ?? i18n.t("timeline.automation.splitHint"),
      onSelect: () => void splitClip(key),
    },
    {
      label: i18n.t("menu.clip.delete"),
      danger: true,
      onSelect: () => void removeAutomationLane(key),
    },
  ];
}

/**
 * The whole menu for one clip, in the order the kind's actions take.
 *
 * An empty list means the key no longer resolves; the caller then opens no menu
 * rather than an empty one.
 */
export function clipMenuItems(key: string): ContextMenuItem[] {
  const ref = clipAt(key);
  if (!ref) return [];
  const items = ref.kind === "pattern" ? patternMenu(ref) : ref.kind === "audio" ? audioMenu(ref) : automationMenu(ref);
  // The read-only rows live in the block's hover tooltip: a menu that opens
  // with a table of metadata buries the actions underneath it. "Inspect" keeps
  // the full list one click away, and a source path stays copyable from there.
  if (items.length) items.unshift(
    { label: i18n.t("clip.info.inspect"), onSelect: () => {
      clipInfoUi.inspect(key);
      selectClip(key);
      panelStore.openDetail();
    } },
  );
  return items;
}

/** Key of the audio clip a menu or command addresses. */
export function audioClipKey(index: number): string {
  return audioKey(index);
}

/** Key of one automation lane, in the owner form the backend uses. */
export function automationClipKey(owner: string, laneIndex: number): string {
  return automationKey(owner, laneIndex);
}
