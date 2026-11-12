/**
 * Take lanes: the comp row and one draggable row per recorded pass.
 *
 * A take group is a set of audio clips sharing `take_group`. The comp row draws
 * the clips that are not muted; dragging across a take row asks the parent to
 * comp that range, which the backend turns into splits and mute flags.
 */
import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import type { AudioClip } from "../types/project";
import { store, type TakeSoloState } from "../stores/project";
import { i18n } from "../stores/i18n";
import { gridDivision, snapBlockQuarter } from "../stores/grid";
import { audioClipBounds } from "../utils/audioClipPlacement";
import { projectClock } from "../utils/projectClock";
import { beginPointerDrag } from "../utils/pointerDrag";

export interface IndexedAudioClip {
  clip: AudioClip;
  index: number;
}

export interface TakeGroup {
  group: string;
  takes: number[];
  clips: IndexedAudioClip[];
}

/** The newest take group on one track, or null when the track has none. */
export function takeGroupFor(clips: AudioClip[], track: string): TakeGroup | null {
  const indexed = clips
    .map((clip, index) => ({ clip, index }))
    .filter((entry) => entry.clip.track === track && entry.clip.take_group && entry.clip.take);
  if (indexed.length === 0) return null;
  const newest = new Map<string, number>();
  for (const entry of indexed) {
    const group = entry.clip.take_group!;
    newest.set(group, Math.max(newest.get(group) ?? 0, entry.clip.take!));
  }
  let group = "";
  let best = -1;
  for (const [name, take] of newest) {
    if (take > best) {
      best = take;
      group = name;
    }
  }
  const members = indexed.filter((entry) => entry.clip.take_group === group);
  const takes = [...new Set(members.map((entry) => entry.clip.take!))].sort((a, b) => a - b);
  return { group, takes, clips: members };
}

export interface TakeLanesProps {
  trackName: string;
  clips: AudioClip[];
  pxPerBeat: number;
  width: number;
  /** Take rows are shown only while the disclosure is open. */
  open: boolean;
  onComp: (track: string, take: number, startQuarter: number, endQuarter: number) => void;
}

/** One comp row plus one row per take, in timeline pixels. */
export function TakeLanes(props: TakeLanesProps) {
  const clock = () => projectClock(store.project.properties);
  const group = createMemo(() => takeGroupFor(props.clips, props.trackName));
  const [drag, setDrag] = createSignal<{ take: number; start: number; end: number } | null>(null);

  const leftOf = (clip: AudioClip) => audioClipBounds(clock(), clip).startQuarter * props.pxPerBeat;
  const widthOf = (clip: AudioClip) => {
    const bounds = audioClipBounds(clock(), clip);
    return Math.max((bounds.endQuarter - bounds.startQuarter) * props.pxPerBeat, 2);
  };

  const takeClips = (take: number) =>
    group()?.clips.filter((entry) => entry.clip.take === take) ?? [];
  const takeMuted = (take: number) => {
    const members = takeClips(take);
    return members.length > 0 && members.every((entry) => entry.clip.muted === true);
  };
  const compSegments = () => group()?.clips.filter((entry) => entry.clip.muted !== true) ?? [];
  const visible = () => group() !== null;

  let dragRow: HTMLElement | undefined;
  let stopDrag: (() => void) | null = null;
  onCleanup(() => stopDrag?.());

  const beginDrag = (event: PointerEvent, take: number) => {
    if (event.button !== 0 || !visible()) return;
    event.preventDefault();
    event.stopPropagation();
    const row = event.currentTarget as HTMLElement;
    dragRow = row;
    const start = snapBlockQuarter(
      Math.max(0, (event.clientX - row.getBoundingClientRect().left) / props.pxPerBeat),
      gridDivision(),
    );
    const session = { take, start, end: start };
    const move = (clientX: number) => {
      if (!dragRow) return;
      const at = snapBlockQuarter(
        Math.max(0, (clientX - dragRow.getBoundingClientRect().left) / props.pxPerBeat),
        gridDivision(),
      );
      setDrag((current) => current ? { ...current, end: at } : current);
    };
    // A comp writes splits and mutes across the group, so only a release may
    // commit one; a cancelled drag drops the marquee.
    const end = (committed: boolean) => {
      stopDrag = null;
      dragRow = undefined;
      const current = drag();
      setDrag(null);
      if (!committed || !current) return;
      const from = Math.min(current.start, current.end);
      const to = Math.max(current.start, current.end);
      if (to - from > 1e-9) {
        props.onComp(props.trackName, current.take, from, to);
      }
    };
    // Take the pointer before publishing the marquee: the helper cancels any
    // drag still running, and that drag must see its own state, not this one.
    stopDrag = beginPointerDrag({
      element: row,
      pointerId: event.pointerId,
      onMove: move,
      onEnd: end,
    });
    setDrag(session);
  };

  return (
    <Show when={visible()}>
      <div class="take-lanes" id={`take-lanes-${props.trackName}`} style={{ width: `${props.width}px` }}>
        <div class="take-comp-row">
          <For each={compSegments()}>
            {(entry) => (
              <div
                class="take-comp-segment"
                data-take={entry.clip.take}
                style={{ left: `${leftOf(entry.clip)}px`, width: `${widthOf(entry.clip)}px` }}
                title={i18n.t("take.compSegment", { take: entry.clip.take ?? 1 })}
              >
                {i18n.t("take.rowName", { take: entry.clip.take ?? 1 })}
              </div>
            )}
          </For>
          <Show when={drag()}>
            {(current) => (
              <div
                class="take-comp-marquee"
                style={{
                  left: `${Math.min(current().start, current().end) * props.pxPerBeat}px`,
                  width: `${Math.abs(current().end - current().start) * props.pxPerBeat}px`,
                }}
              />
            )}
          </Show>
        </div>
        <For each={group()?.takes ?? []}>
          {(take) => (
            <Show when={props.open}>
              <div
                class="take-row"
                classList={{ muted: takeMuted(take), dragging: drag()?.take === take }}
                data-take={take}
                aria-label={i18n.t("take.rowLabel", { take })}
                onPointerDown={(event) => beginDrag(event, take)}
              >
                <For each={takeClips(take)}>
                  {(entry) => (
                    <div
                      class="take-row-block"
                      classList={{ muted: entry.clip.muted === true }}
                      data-clip={entry.index}
                      style={{ left: `${leftOf(entry.clip)}px`, width: `${widthOf(entry.clip)}px` }}
                    />
                  )}
                </For>
              </div>
            </Show>
          )}
        </For>
      </div>
    </Show>
  );
}

export interface TakeHeadersProps {
  trackName: string;
  clips: AudioClip[];
  solo: TakeSoloState | null;
  open: boolean;
  onToggle: (track: string) => void;
  onMute: (track: string, take: number, muted: boolean) => void;
  onSolo: (track: string, take: number) => void;
}

/** The header column that matches {@link TakeLanes}: comp row plus M/S rows. */
export function TakeHeaders(props: TakeHeadersProps) {
  const group = createMemo(() => takeGroupFor(props.clips, props.trackName));
  const takeClips = (take: number) =>
    group()?.clips.filter((entry) => entry.clip.take === take) ?? [];
  const takeMuted = (take: number) => {
    const members = takeClips(take);
    return members.length > 0 && members.every((entry) => entry.clip.muted === true);
  };
  const soloed = (take: number) =>
    props.solo?.track === props.trackName && props.solo.take === take;
  return (
    <Show when={group()}>
      <div class="take-headers">
        <div class="take-comp-header">
          <button
            type="button"
            class="take-disclosure"
            classList={{ active: props.open }}
            aria-expanded={props.open}
            aria-controls={`take-lanes-${props.trackName}`}
            onClick={(event) => {
              event.stopPropagation();
              props.onToggle(props.trackName);
            }}
          >
            <span aria-hidden="true">{props.open ? "▾" : "▸"}</span>
            {i18n.t("take.toggle")}
          </button>
        </div>
        <Show when={props.open}>
          <For each={group()?.takes ?? []}>
            {(take) => (
              <div
                class="take-header-row"
                classList={{ muted: takeMuted(take), solo: soloed(take) }}
                data-take={take}
              >
                <span class="take-header-name">{i18n.t("take.rowName", { take })}</span>
                <div class="take-header-buttons">
                  <button
                    type="button"
                    class="track-btn"
                    classList={{ "mute-active": takeMuted(take) }}
                    aria-label={takeMuted(take)
                      ? i18n.t("take.unmute", { take })
                      : i18n.t("take.mute", { take })}
                    aria-pressed={takeMuted(take)}
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onMute(props.trackName, take, !takeMuted(take));
                    }}
                  >M</button>
                  <button
                    type="button"
                    class="track-btn"
                    classList={{ "solo-active": soloed(take) }}
                    aria-label={soloed(take)
                      ? i18n.t("take.unsolo", { take })
                      : i18n.t("take.solo", { take })}
                    aria-pressed={soloed(take)}
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onSolo(props.trackName, take);
                    }}
                  >S</button>
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </Show>
  );
}
