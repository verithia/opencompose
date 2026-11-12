import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  chooseDropdown,
  expectInvoked,
  invokeMock,
  makeEffect,
  makePattern,
  makeSnapshot,
  makeTrack,
  askDialogMock,
} from "../setup";
import { store } from "../../stores/project";
import { panelStore } from "../../stores/panels";
import { TooltipLayer, placeTooltip, useTooltip, type TooltipContent } from "../../components/Tooltip";
import {
  ContextMenuLayer,
  closeContextMenu,
  openContextMenu,
  placeContextMenu,
} from "../../components/ContextMenu";
import { Timeline } from "../../components/Timeline";
import { Mixer } from "../../components/Mixer";
import { EffectsRack } from "../../components/EffectsRack";

function TooltipHost(props: { content: TooltipContent; delay?: number }) {
  const attachment = useTooltip(() => props.content, { delay: props.delay });
  return <button {...attachment}>Hover me</button>;
}

function menuItem(label: string): HTMLButtonElement | undefined {
  const find = () => [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
  const first = find();
  if (first) return first;
  for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
    if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
  }
  return find();
}

/** Open a menu group by its label before reading the rows inside it. */
function expandGroup(label: string): void {
  const group = [...document.querySelectorAll<HTMLElement>(".context-menu-group")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
  if (group && group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
}

function snapshotWithClip() {
  const snapshot = makeSnapshot({
    tracks: [{
      ...makeTrack("Piano", 0),
      clips: [{
        pattern: "lead",
        time_range: {
          start: { bar: 1, beat: 1, subdivision: 0 },
          end: { bar: 3, beat: 1, subdivision: 0 },
        },
      }],
    }],
    patterns: [makePattern("lead")],
  });
  snapshot.dsl_source = [
    '@project "Test Project"',
    "  tempo: 128",
    "  time_signature: 4/4",
    "",
    "@pattern lead",
    "  note C4 [1:1:0] 1/4",
    "",
    'track "Piano" {',
    "  clip: lead [1:1:0 - 3:1:0]",
    "}",
  ].join("\n");
  return snapshot;
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "devices_catalog") return { devices: [] };
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "transport_seek") {
      return { ...TRANSPORT_STOPPED(), position_beats: (args as { beat: number }).beat };
    }
    if (cmd === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    if (cmd === "update_dsl") {
      const source = (args as { source: string }).source;
      return { ...snapshotWithClip(), dsl_source: source };
    }
    return snapshotWithClip();
  });
  await store.initProject();
  closeContextMenu();
  panelStore.closeBottom();
});

afterEach(() => {
  vi.useRealTimers();
  closeContextMenu();
});

describe("Tooltip primitive", () => {
  it("shows after the delay and hides immediately on leave", () => {
    vi.useFakeTimers();
    const { getByRole } = render(() => <><TooltipHost content="Hello" /><TooltipLayer /></>);
    const button = getByRole("button", { name: "Hover me" });

    fireEvent.mouseEnter(button);
    expect(document.querySelector(".oc-tooltip")).toBeFalsy();
    vi.advanceTimersByTime(399);
    expect(document.querySelector(".oc-tooltip")).toBeFalsy();
    vi.advanceTimersByTime(1);
    expect(document.querySelector(".oc-tooltip")?.textContent).toContain("Hello");

    fireEvent.mouseLeave(button);
    expect(document.querySelector(".oc-tooltip")).toBeFalsy();
  });

  it("shows on keyboard focus without waiting and wires aria-describedby", () => {
    const { getByRole } = render(() => <><TooltipHost content="Focused" delay={400} /><TooltipLayer /></>);
    const button = getByRole("button", { name: "Hover me" });
    const describedBy = button.getAttribute("aria-describedby");

    fireEvent.focusIn(button);
    const tooltip = document.querySelector(".oc-tooltip");
    expect(tooltip?.textContent).toContain("Focused");
    expect(describedBy).toBeTruthy();
    expect(tooltip?.id).toBe(describedBy);
  });

  it("renders structured title and key/value rows", () => {
    vi.useFakeTimers();
    const content: TooltipContent = {
      title: "Piano",
      rows: [
        { label: "Preset", value: "Grand" },
        { label: "Notes", value: "12" },
      ],
    };
    const { getByRole } = render(() => <><TooltipHost content={content} /><TooltipLayer /></>);
    fireEvent.mouseEnter(getByRole("button", { name: "Hover me" }));
    vi.advanceTimersByTime(400);

    expect(document.querySelector(".oc-tooltip-title")?.textContent).toBe("Piano");
    expect([...document.querySelectorAll(".oc-tooltip-row")].map((row) => row.textContent)).toEqual([
      "PresetGrand",
      "Notes12",
    ]);
  });

  it("track headers and clip blocks show their structured summaries", () => {
    vi.useFakeTimers();
    const { container } = render(() => <><Timeline /><TooltipLayer /></>);

    fireEvent.mouseEnter(container.querySelector(".track-header") as HTMLElement);
    vi.advanceTimersByTime(400);
    expect(document.querySelector(".oc-tooltip-title")?.textContent).toBe("Piano");
    expect(document.querySelector(".oc-tooltip")?.textContent).toContain("Piano 1");
    expect(document.querySelector(".oc-tooltip")?.textContent).toContain("Phrases");
    fireEvent.mouseLeave(container.querySelector(".track-header") as HTMLElement);

    fireEvent.mouseEnter(container.querySelector(".clip-block") as HTMLElement);
    vi.advanceTimersByTime(400);
    expect(document.querySelector(".oc-tooltip-title")?.textContent).toBe("lead");
    expect(document.querySelector(".oc-tooltip")?.textContent).toContain("Double-click to edit");
  });

  it("flips above the anchor near the bottom edge and clamps near the right edge", () => {
    const flipped = placeTooltip(
      { left: 100, right: 180, top: 700, bottom: 720, width: 80, height: 20 },
      { width: 200, height: 80 },
      { width: 800, height: 768 },
    );
    expect(flipped.side).toBe("top");
    expect(flipped.y).toBe(612);
    expect(flipped.x).toBe(40);

    const clamped = placeTooltip(
      { left: 760, right: 800, top: 100, bottom: 120, width: 40, height: 20 },
      { width: 200, height: 80 },
      { width: 800, height: 768 },
    );
    expect(clamped.x).toBe(592);
  });

  it("clamps a tooltip taller than either side to the roomier side", () => {
    const tall = placeTooltip(
      { left: 100, right: 180, top: 2, bottom: 22, width: 80, height: 20 },
      { width: 200, height: 900 },
      { width: 800, height: 768 },
    );
    expect(tall.side).toBe("bottom");
    expect(tall.y).toBe(30);
    expect(tall.maxHeight).toBe(730);
  });
});

describe("Context menu primitive", () => {
  it("opens at the click point and closes on Escape or an outside pointer down", () => {
    render(() => <ContextMenuLayer />);
    openContextMenu({
      x: 120,
      y: 80,
      items: [{ label: "Action", onSelect: vi.fn() }],
    });

    const menu = document.querySelector(".context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.style.left).toBe("120px");
    expect(menu.style.top).toBe("80px");

    fireEvent.keyDown(window, { key: "Escape" });
    expect(document.querySelector(".context-menu")).toBeFalsy();

    openContextMenu({ x: 10, y: 10, items: [{ label: "Action", onSelect: vi.fn() }] });
    fireEvent.pointerDown(document.body);
    expect(document.querySelector(".context-menu")).toBeFalsy();
  });

  it("runs item callbacks, closes, and ignores disabled items", () => {
    const enabled = vi.fn();
    const disabled = vi.fn();
    render(() => <ContextMenuLayer />);
    openContextMenu({
      x: 10,
      y: 10,
      items: [
        { label: "Disabled", onSelect: disabled, disabled: true },
        { type: "separator" },
        { label: "Enabled", onSelect: enabled, shortcut: "Cmd+E" },
      ],
    });

    fireEvent.click(menuItem("Disabled")!);
    expect(disabled).not.toHaveBeenCalled();
    expect(document.querySelector(".context-menu")).toBeTruthy();

    fireEvent.click(menuItem("Enabled")!);
    expect(enabled).toHaveBeenCalledTimes(1);
    expect(document.querySelector(".context-menu")).toBeFalsy();
  });

  it("navigates with the keyboard and skips disabled items", () => {
    const first = vi.fn();
    const second = vi.fn();
    render(() => <ContextMenuLayer />);
    openContextMenu({
      x: 10,
      y: 10,
      items: [
        { label: "First", onSelect: first },
        { label: "Disabled", onSelect: vi.fn(), disabled: true },
        { label: "Second", onSelect: second },
      ],
    });

    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "Enter" });
    expect(second).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
  });

  it("renders a heading row and checked items, and keyboard navigation skips the heading", () => {
    const first = vi.fn();
    render(() => <ContextMenuLayer />);
    openContextMenu({
      x: 10,
      y: 10,
      items: [
        { type: "heading", label: "C4 · vel 84" },
        { label: "127 (FFF)", checked: false, onSelect: first },
        { label: "80 (MF)", checked: true, onSelect: vi.fn() },
      ],
    });

    expect(document.querySelector(".context-menu-heading")?.textContent).toBe("C4 · vel 84");
    const checked = [...document.querySelectorAll(".context-menu-item")]
      .find((item) => item.getAttribute("aria-checked") === "true");
    expect(checked?.textContent).toContain("80 (MF)");

    // Focus starts on the first action, not on the heading.
    fireEvent.keyDown(window, { key: "Enter" });
    expect(first).toHaveBeenCalledTimes(1);
  });

  it("clamps the menu rectangle into the viewport", () => {
    expect(placeContextMenu({ x: 900, y: 700 }, { width: 180, height: 120 }, { width: 800, height: 600 }))
      .toEqual({ x: 612, y: 472 });
  });
});

describe("Adopted context menus", () => {
  it("track header dispatches mute, duplicate, add track, and mixer commands", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const header = () => container.querySelector(".track-header") as HTMLElement;

    fireEvent.contextMenu(header(), { clientX: 100, clientY: 60 });
    expect(document.querySelector(".track-context-menu")).toBeTruthy();
    fireEvent.click(menuItem("Mute")!);
    await vi.waitFor(() => expectInvoked("set_track_mute", { trackIndex: 0, mute: true }));

    fireEvent.contextMenu(header(), { clientX: 100, clientY: 60 });
    fireEvent.click(menuItem("New track with this sound")!);
    await vi.waitFor(() => expectInvoked("duplicate_track", { trackIndex: 0, name: "Piano 2" }));

    // MIDI, adding a track and the mixer live in the "Track tools" group.
    const beforeAdd = invokeMock.mock.calls.filter(([cmd]) => cmd === "add_track").length;
    fireEvent.contextMenu(header(), { clientX: 100, clientY: 60 });
    expandGroup("Track tools");
    fireEvent.click(menuItem("Add track at end")!);
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "add_track").length).toBe(beforeAdd + 1));

    fireEvent.contextMenu(header(), { clientX: 100, clientY: 60 });
    expandGroup("Track tools");
    fireEvent.click(menuItem("Open mixer")!);
    expect(panelStore.bottomMode()).toBe("mixer");
  });

  it("clip menu opens the piano roll and dispatches duplicate, split, and delete", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const clip = () => container.querySelector(".clip-block") as HTMLElement;

    fireEvent.contextMenu(clip(), { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Open editor")!);
    // A pattern clip's editor is the piano roll, not the inspector.
    expect(panelStore.bottomMode()).toBe("pianoroll");

    fireEvent.contextMenu(clip(), { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Duplicate")!);
    await vi.waitFor(() => expectInvoked("add_clip", { trackIndex: 0, patternName: "lead" }));

    await store.seek(4);
    fireEvent.contextMenu(clip(), { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Split at playhead")!);
    await vi.waitFor(() =>
      expectInvoked("split_clip", { trackIndex: 0, clipIndex: 0, atBar: 2, atBeat: 1, atSub: 0 }));

    await store.initProject();
    const nextClip = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.contextMenu(nextClip, { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Delete")!);
    await vi.waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "remove_blocks");
      expect(call?.[1]).toEqual({ blocks: [{ kind: "clip", trackIndex: 0, index: 0 }] });
    });
  });

  it("clip menu names its clip and does not select it or retarget the editor", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const clip = container.querySelector(".clip-block") as HTMLElement;
    const lane = container.querySelector(".track-lane") as HTMLElement;

    fireEvent.click(lane);
    const before = {
      track: store.selectedTrackIndex(),
      pattern: store.selectedPatternName(),
      editor: store.editorSelection(),
    };
    expect(before.editor).toBe(null);

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    expect(document.querySelector(".clip-context-menu .context-menu-heading")?.textContent).toBe("lead");
    expect(clip.classList.contains("selected")).toBe(false);
    expect(store.selectedTrackIndex()).toBe(before.track);
    expect(store.selectedPatternName()).toBe(before.pattern);
    expect(store.editorSelection()).toBe(null);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_seek")).toBe(false);
  });

  it("track menu names its track without changing the selected track", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const header = container.querySelector(".track-header") as HTMLElement;
    store.setSelectedTrackIndex(1);

    fireEvent.contextMenu(header, { clientX: 100, clientY: 60 });
    expect(document.querySelector(".track-context-menu .context-menu-heading")?.textContent).toBe("Piano");
    expect(store.selectedTrackIndex()).toBe(1);

    fireEvent.click(menuItem("Mute")!);
    await vi.waitFor(() => expectInvoked("set_track_mute", { trackIndex: 0, mute: true }));
  });

  it("clip rename routes through the existing DSL rename path", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const clip = container.querySelector(".clip-block") as HTMLElement;

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Rename pattern")!);
    // The webview has no window.prompt, so the rename is an in-app dialog.
    const dialog = await screen.findByRole("dialog", { name: "New name" });
    fireEvent.input(dialog.querySelector("input")!, { target: { value: "lead 2" } });
    fireEvent.submit(dialog);
    await vi.waitFor(() => expectInvoked("rename_pattern", { patternName: "lead", newName: "lead 2" }));
  });

  it("effect slot dispatches bypass and remove but not disabled reorder", async () => {
    const effect = makeEffect("DynamicEq", []);
    const { container } = render(() => (
      <>
        <EffectsRack target="track:0" effects={[effect]} label="FX" />
        <ContextMenuLayer />
      </>
    ));
    const slot = container.querySelector(".effect-slot") as HTMLElement;

    fireEvent.contextMenu(slot, { clientX: 50, clientY: 50 });
    fireEvent.click(menuItem("Move up")!);
    expect(document.querySelector(".context-menu")).toBeTruthy();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_effect_param_owned")).toBe(false);

    fireEvent.click(menuItem("Bypass")!);
    await vi.waitFor(() => expectInvoked("set_effect_bypass_owned", {
      target: "track:0",
      effectIndex: 0,
      bypass: true,
    }));

    fireEvent.contextMenu(slot, { clientX: 50, clientY: 50 });
    fireEvent.click(menuItem("Remove effect")!);
    await vi.waitFor(() => expectInvoked("remove_effect", { target: "track:0", effectIndex: 0 }));
  });

  it("mixer strip dispatches solo/delete for tracks and delete-bus for buses", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "devices_catalog") return { devices: [] };
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_state") {
        return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      }
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
      });
    });
    await store.initProject();
    const { container, getByLabelText } = render(() => <><Mixer /><ContextMenuLayer /></>);
    const strip = container.querySelector(".node-mixer") as HTMLElement;

    fireEvent.contextMenu(strip, { clientX: 40, clientY: 40 });
    fireEvent.click(menuItem("Solo")!);
    await vi.waitFor(() => expectInvoked("set_track_solo", { trackIndex: 0, solo: true }));

    fireEvent.contextMenu(strip, { clientX: 40, clientY: 40 });
    // A track that holds clips asks before it goes.
    askDialogMock.mockResolvedValueOnce(true);
    fireEvent.click(menuItem("Delete track")!);
    await vi.waitFor(() => expectInvoked("remove_track", { trackIndex: 0 }));

    await store.initProject();
    chooseDropdown(getByLabelText("Channel"), "Bus: reverb_bus");
    fireEvent.contextMenu(strip, { clientX: 40, clientY: 40 });
    fireEvent.click(menuItem("Delete bus")!);
    await vi.waitFor(() => expectInvoked("remove_bus", { name: "reverb_bus" }));
  });
});
