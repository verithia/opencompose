/**
 * Markers and the session loop region on the timeline ruler.
 *
 * The ruler owns the flags, the loop band and the commit-on-release drags; the
 * transport owns the Loop toggle and its refusal. Every case drives the real
 * components against the fake IPC layer, so it fails if the control stops
 * sending the command the backend needs.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import { Toolbar } from "../../components/Toolbar";
import type { LoopState } from "../../stores/api";

const MARKERS = [
  { name: "verse", position: { bar: 3, beat: 1, subdivision: 0 } },
  { name: "Verse 2", position: { bar: 5, beat: 3, subdivision: 0 } },
];

function stoppedTransport() {
  return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 16 };
}

/** One tick, enough for the store's fire-and-forget loop restore to land. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (
      cmd === "get_transport_state" ||
      cmd === "transport_seek" ||
      cmd === "transport_play" ||
      cmd === "transport_pause" ||
      cmd === "transport_stop"
    ) return stoppedTransport();
    if (cmd === "set_loop_region") {
      const args = invokeMock.mock.calls.at(-1)?.[1] as {
        startBeats: number | null;
        endBeats: number | null;
        enabled: boolean;
      };
      return {
        enabled: args?.enabled ?? false,
        start_beats: args?.startBeats ?? null,
        end_beats: args?.endBeats ?? null,
      } satisfies LoopState;
    }
    return makeSnapshot({ tracks: [makeTrack("Piano", 0)], markers: MARKERS });
  });
  await store.initProject();
  await flush();
});

afterEach(() => {
  // The region is remembered per project path; each case starts clean.
  if (typeof localStorage !== "undefined") localStorage.clear();
  closeContextMenu();
});

function markerAt(container: HTMLElement, name: string): HTMLElement {
  const node = container.querySelector<HTMLElement>(`[data-marker="${name}"]`);
  if (!node) throw new Error(`no marker flag for ${name}`);
  return node;
}

describe("timeline markers", () => {
  it("draws each flag at its beat and jumps the playhead on click", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const verse = markerAt(container, "verse");
    // Bar 3 beat 1 is 8 quarters in; the default zoom is 24 px/beat.
    expect(verse.style.left).toBe("192px");
    expect(verse.textContent).toContain("verse");
    // The second marker sits off the beat grid: bar 5 beat 3 = 18 quarters.
    expect(markerAt(container, "Verse 2").style.left).toBe("432px");

    fireEvent.pointerDown(verse, { button: 0, pointerId: 1 });
    fireEvent.pointerUp(verse, { pointerId: 1 });

    await vi.waitFor(() => {
      const seek = invokeMock.mock.calls.find(([cmd]) => cmd === "transport_seek");
      expect(seek, "a click must seek to the marker").toBeTruthy();
      expect(seek?.[1]).toEqual({ beat: 8 });
    });
  });

  it("seeks when the press shook by less than the drag threshold", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const verse = markerAt(container, "verse");

    // Three pixels of hand movement is still a click, not a marker move.
    fireEvent.pointerDown(verse, { button: 0, pointerId: 1, clientX: 192, clientY: 8 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 195, clientY: 8 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 195, clientY: 8 });

    await vi.waitFor(() => {
      const seek = invokeMock.mock.calls.find(([cmd]) => cmd === "transport_seek");
      expect(seek, "a shaky click still seeks").toBeTruthy();
      expect(seek?.[1]).toEqual({ beat: 8 });
    });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "move_marker")).toBe(false);
  });

  it("moves a marker once on release, so one drag is one undo entry", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const verse = markerAt(container, "verse");

    fireEvent.pointerDown(verse, { button: 0, pointerId: 1 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 144 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 288 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 288 });

    await vi.waitFor(() => {
      const moves = invokeMock.mock.calls.filter(([cmd]) => cmd === "move_marker");
      expect(moves).toHaveLength(1);
      expect(moves[0][1]).toEqual({
        name: "verse",
        position: { bar: 4, beat: 1, subdivision: 0 },
      });
    });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_seek")).toBe(false);
  });

  it("renames through the in-app dialog, not window.prompt", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    fireEvent.dblClick(markerAt(container, "verse"));

    const input = document.querySelector<HTMLInputElement>(".text-prompt-input");
    expect(input, "the app owns a rename dialog").toBeTruthy();
    expect(input?.value).toBe("verse");
    const ok = document.querySelector<HTMLButtonElement>(".text-prompt-btn.primary")!;
    expect(ok.disabled).toBe(false);
    fireEvent.input(input!, { target: { value: "   " } });
    expect(ok.disabled, "Add stays inactive until the name is non-empty").toBe(true);
    fireEvent.input(input!, { target: { value: "intro" } });
    expect(ok.disabled).toBe(false);
    fireEvent.submit(input!.closest("form")!);

    await vi.waitFor(() => {
      const rename = invokeMock.mock.calls.find(([cmd]) => cmd === "rename_marker");
      expect(rename?.[1]).toEqual({ name: "verse", newName: "intro" });
    });
  });

  it("shows a marker past the last audio", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_loop_region") return { enabled: false, start_beats: null, end_beats: null };
      if (cmd === "get_transport_state") return stoppedTransport();
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        markers: [{ name: "outro", position: { bar: 40, beat: 1, subdivision: 0 } }],
      });
    });
    await store.initProject();
    await flush();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const outro = container.querySelector<HTMLElement>("[data-marker='outro']");
    expect(outro, "a marker past the last audio is shown").toBeTruthy();
    // Bar 40 beat 1 is 156 quarters in: 39 bars × 4 beats × 24 px.
    expect(outro?.style.left).toBe("3744px");
  });

  it("deletes only after the confirmation", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const verse = markerAt(container, "verse");

    fireEvent.contextMenu(verse);
    const deleteItem = await vi.waitFor(() => {
      const node = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
        .find((item) => item.textContent?.includes(i18n.t("marker.delete")));
      expect(node).toBeTruthy();
      return node!;
    });
    fireEvent.click(deleteItem);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_marker")).toBe(false);

    const confirm = await vi.waitFor(() => {
      const node = [...document.querySelectorAll<HTMLElement>("[role='alertdialog'] button")]
        .find((button) => button.textContent === i18n.t("marker.delete"));
      expect(node, "deleting asks first").toBeTruthy();
      return node!;
    });
    fireEvent.click(confirm);

    await vi.waitFor(() => {
      const remove = invokeMock.mock.calls.find(([cmd]) => cmd === "remove_marker");
      expect(remove?.[1]).toEqual({ name: "verse" });
    });
  });
});

describe("session loop region", () => {
  it("disables Loop with the reason until a region exists", () => {
    const { container } = render(() => <Toolbar />);
    const loop = container.querySelector<HTMLButtonElement>("[aria-label='Loop']")!;
    expect(loop.disabled).toBe(true);
    expect(loop.getAttribute("title")).toBe(i18n.t("transport.loopNeedsRegion"));
    expect(i18n.t("transport.loopNeedsRegion")).toBe(
      "Drag in the ruler to set a loop region first.",
    );
  });

  it("shows the dragged region as a band and arms Loop on confirmation", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const strip = container.querySelector<HTMLElement>("[data-loop-strip]")!;

    fireEvent.pointerDown(strip, { button: 0, pointerId: 1, clientX: 0 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 96 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 96 });

    await vi.waitFor(() => {
      const drawn = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_loop_region").at(-1);
      expect(drawn?.[1]).toEqual({ startBeats: 0, endBeats: 4, enabled: false });
    });
    const band = container.querySelector<HTMLElement>("[data-loop-band]");
    expect(band, "the region is visible as soon as it exists").toBeTruthy();
    expect(band?.style.left).toBe("0px");
    expect(band?.style.width).toBe("96px");

    // The transport holds the region now; the next click arms it.
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_loop_region") {
        const args = invokeMock.mock.calls.at(-1)?.[1] as {
          startBeats: number | null;
          endBeats: number | null;
          enabled: boolean;
        };
        return { enabled: true, start_beats: args.startBeats, end_beats: args.endBeats };
      }
      if (cmd === "get_transport_state") return stoppedTransport();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], markers: MARKERS });
    });
    const toolbar = render(() => <Toolbar />);
    const loop = toolbar.container.querySelector<HTMLButtonElement>("[aria-label='Loop']")!;
    expect(loop.disabled).toBe(false);
    fireEvent.click(loop);

    await vi.waitFor(() => {
      expect(loop.getAttribute("aria-pressed")).toBe("true");
    });
    const armed = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_loop_region").at(-1);
    expect(armed?.[1]).toEqual({ startBeats: 0, endBeats: 4, enabled: true });
  });

  it("reads Loop armed while the transport confirms, then settles", async () => {
    let confirm: (state: LoopState) => void = () => {};
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "set_loop_region") {
        const args = invokeMock.mock.calls.at(-1)?.[1] as {
          startBeats: number | null;
          endBeats: number | null;
          enabled: boolean;
        };
        if (args.enabled === true) {
          return new Promise<LoopState>((resolve) => { confirm = resolve; });
        }
        return Promise.resolve({
          enabled: false,
          start_beats: args.startBeats,
          end_beats: args.endBeats,
        });
      }
      if (cmd === "get_transport_state") return Promise.resolve(stoppedTransport());
      return Promise.resolve(makeSnapshot({ tracks: [makeTrack("Piano", 0)], markers: MARKERS }));
    });
    await store.initProject();
    await store.setLoopRegion(0, 4);
    const { container } = render(() => <Toolbar />);
    const loop = container.querySelector<HTMLButtonElement>("[aria-label='Loop']")!;

    fireEvent.click(loop);
    await vi.waitFor(() => {
      expect(loop.getAttribute("aria-busy")).toBe("true");
      expect(loop.querySelector("svg")).toBeTruthy();
    });
    expect(loop.getAttribute("aria-pressed")).toBe("false");

    confirm({ enabled: true, start_beats: 0, end_beats: 4 });
    await vi.waitFor(() => {
      expect(loop.getAttribute("aria-busy")).toBe("false");
      expect(loop.getAttribute("aria-label")).toBe(i18n.t("transport.loop"));
      expect(loop.getAttribute("aria-pressed")).toBe("true");
    });
  });

  it("clearing the region turns Loop off", async () => {
    await store.setLoopRegion(0, 4);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_loop_region") {
        const args = invokeMock.mock.calls.at(-1)?.[1] as {
          startBeats: number | null;
          endBeats: number | null;
          enabled: boolean;
        };
        return { enabled: false, start_beats: args.startBeats, end_beats: args.endBeats };
      }
      return stoppedTransport();
    });
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const strip = container.querySelector<HTMLElement>("[data-loop-strip]")!;

    fireEvent.contextMenu(strip, { clientX: 10, clientY: 10 });
    const clear = await vi.waitFor(() => {
      const node = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
        .find((item) => item.textContent?.includes(i18n.t("loop.clear")));
      expect(node).toBeTruthy();
      return node!;
    });
    fireEvent.click(clear);

    await vi.waitFor(() => {
      expect(store.loopRegion()).toBeNull();
    });
    expect(container.querySelector("[data-loop-band]")).toBeNull();
  });
});
