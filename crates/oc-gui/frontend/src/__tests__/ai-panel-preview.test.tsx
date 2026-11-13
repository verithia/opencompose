/**
 * A preview the backend holds: the timeline and the piano roll mark the
 * subtrees the proposal changes. Nothing here re-renders a document: the
 * snapshot the backend returns is the document the store and the editors paint.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup } from "@solidjs/testing-library";
import { installAuditBackend, loadAuditProject, makeTimelineProject, mountSurface, resetSurface, settle } from "./ui-audit-support";
import { makeNote, makePattern, makeSnapshot, makeTrack } from "./setup";
import { store } from "../stores/project";
import { previewStore } from "../stores/preview";
import { i18n } from "../stores/i18n";
import { Timeline } from "../components/Timeline";
import { PianoRoll } from "../components/PianoRoll";
import type { PreviewHighlight, PreviewState } from "../stores/api";

beforeEach(() => {
  i18n.setLocale("en");
  previewStore.reset();
});

afterEach(() => {
  cleanup();
  previewStore.reset();
  resetSurface();
});

describe("preview highlighting", () => {
  it("marks the changed track and its clip in the timeline", async () => {
    const snapshot = makeTimelineProject();
    await loadAuditProject(snapshot);
    const highlight: PreviewHighlight = {
      tracks: ["Piano"],
      patterns: [],
      inline_patterns: [],
      presets: [],
      clips: [{ track: "Piano", kind: "pattern", name: "hook", base_index: 0, proposal_index: 0 }],
      effects: [],
      notes: [],
    };
    installAuditBackend((command) =>
      command === "preview_state"
        ? { side: "proposal", target_path: "track:Piano", highlight } satisfies PreviewState
        : undefined,
    );
    await previewStore.refresh();
    expect(previewStore.open()).toBe(true);

    await mountSurface(() => <Timeline />);
    const lanes = [...document.querySelectorAll(".track-lane")];
    expect(lanes).toHaveLength(2);
    // The first lane is Piano; only it carries the proposal's mark.
    expect(lanes[0].classList.contains("preview-changed")).toBe(true);
    expect(lanes[1].classList.contains("preview-changed")).toBe(false);
    expect(document.querySelectorAll(".phrase-block.preview-changed")).toHaveLength(1);
  });

  it("marks only the notes inside a changed bar of the open pattern", async () => {
    const pattern = makePattern("hook");
    pattern.hits = [];
    pattern.events = [
      makeNote({ start_beat: 0 }),
      makeNote({ start_beat: 4, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ];
    const track = makeTrack("Piano", 0);
    track.clips = [{
      pattern: "hook",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }];
    await loadAuditProject(makeSnapshot({ tracks: [track], patterns: [pattern] }));
    const highlight: PreviewHighlight = {
      tracks: [],
      patterns: ["hook"],
      inline_patterns: [],
      presets: [],
      clips: [],
      effects: [],
      notes: [{ pattern: "hook", start_bar: 2, end_bar: 2 }],
    };
    installAuditBackend((command) =>
      command === "preview_state"
        ? { side: "proposal", target_path: "pattern:hook", highlight } satisfies PreviewState
        : undefined,
    );
    await previewStore.refresh();

    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    await mountSurface(() => <PianoRoll />);
    await settle();

    const marked = [...document.querySelectorAll(".pianoroll-note.preview-changed")];
    expect(marked).toHaveLength(1);
    // The note on beat 4 is the one the proposal changed; the one at beat 0 is not.
    expect(Number(marked[0].getAttribute("style")?.match(/top:\s*(\d+)/)?.[1] ?? -1)).toBeGreaterThan(0);
  });

  it("clears the changed-side marks when no preview is open", async () => {
    await loadAuditProject(makeTimelineProject());
    previewStore.reset();
    await mountSurface(() => <Timeline />);
    expect(document.querySelectorAll(".preview-changed")).toHaveLength(0);
  });
});
