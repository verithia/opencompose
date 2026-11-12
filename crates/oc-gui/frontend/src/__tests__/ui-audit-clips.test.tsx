/**
 * Control audit — the timeline surface: track headers, clip blocks, the clip
 * context menu, audio clips and the grid/zoom controls.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import {
  driveControl,
  interactiveControls,
  loadAuditProject,
  makeTimelineProject,
  mountSurface,
  resetSurface,
  settle,
} from "./ui-audit-support";
import { invokeMock, expectInvoked } from "./setup";
import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import { Timeline } from "../components/Timeline";
import { closeContextMenu, ContextMenuLayer } from "../components/ContextMenu";
import { i18n } from "../stores/i18n";

beforeEach(async () => {
  i18n.setLocale("en");
  closeContextMenu();
  await loadAuditProject(makeTimelineProject());
});

afterEach(() => {
  closeContextMenu();
  resetSurface();
  i18n.setLocale("en");
});

function clipBlock(container: HTMLElement, name = "hook"): HTMLElement {
  const block = [...container.querySelectorAll<HTMLElement>(".clip-block")]
    .find((element) => element.textContent?.includes(name));
  if (!block) throw new Error(`no clip block for ${name}`);
  return block;
}

function menuLabels(): string[] {
  return [...document.querySelectorAll<HTMLElement>(".context-menu-item .context-menu-label")]
    .map((element) => element.textContent?.trim() ?? "");
}

/** Open a menu group before a test reads the rows inside it. */
function openGroup(label: string): void {
  const group = [...document.querySelectorAll<HTMLElement>(".context-menu-group")]
    .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === label);
  if (group && group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
}

function menuItem(label: string): HTMLElement {
  const item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === label);
  if (!item) throw new Error(`no menu item ${label}; menu has ${menuLabels().join(", ")}`);
  return item;
}

async function openClipMenu(container: HTMLElement, name = "hook") {
  fireEvent.contextMenu(clipBlock(container, name), { clientX: 40, clientY: 40 });
  await settle(1);
}

describe("timeline inventory sweep", () => {
  it("every timeline control responds or states why it cannot", async () => {
    const failures: string[] = [];
    const first = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const controls = interactiveControls(first.container);
    const labels = controls.map((control, index) => `${control.label}#${index}`);
    for (let index = 0; index < labels.length; index += 1) {
      resetSurface();
      await loadAuditProject(makeTimelineProject());
      const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
      const target = interactiveControls(view.container)[index];
      if (!target) {
        failures.push(`${labels[index]}: disappeared on remount`);
        continue;
      }
      const outcome = await driveControl(target);
      if (outcome.error) failures.push(`${labels[index]}: threw ${outcome.error}`);
      else if (!outcome.effect) failures.push(`${labels[index]}: no observable effect`);
    }
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("every disabled timeline control carries a reason", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const silent = [...view.container.querySelectorAll<HTMLElement>("[disabled]")];
    expect(silent.filter((element) => !element.getAttribute("aria-label"))).toEqual([]);
  });
});

describe("clip context menu", () => {
  it("lists every clip command and closes on Escape", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    expect(menuLabels()).toEqual([
      i18n.t("clip.info.inspect"),
      i18n.t("menu.clip.preview"),
      i18n.t("menu.clip.openEditor"),
      i18n.t("menu.clip.rename"),
      i18n.t("menu.clip.groupEdit"),
      i18n.t("menu.clip.copy"),
      i18n.t("menu.clip.cut"),
      i18n.t("menu.clip.paste"),
      i18n.t("menu.clip.duplicate"),
      i18n.t("menu.clip.groupContent"),
      i18n.t("menu.clip.split"),
      i18n.t("menu.clip.resetSlip"),
      i18n.t("menu.clip.loop"),
      i18n.t("menu.clip.groupSelection"),
      i18n.t("menu.clip.group"),
      i18n.t("menu.clip.ungroup"),
      i18n.t("menu.clip.delete"),
    ]);
    fireEvent.keyDown(window, { key: "Escape" });
    await settle(1);
    expect(document.querySelector(".context-menu-item")).toBeNull();
  });

  it("closes when the pointer goes elsewhere", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.pointerDown(document.body);
    await settle(1);
    expect(document.querySelector(".context-menu-item")).toBeNull();
  });

  it("names the reason Paste and Split are disabled", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    const paste = menuItem(i18n.t("menu.clip.paste"));
    expect(paste.getAttribute("aria-disabled")).toBe("true");
    expect(paste.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("menu.clip.pasteHint"));
    const split = menuItem(i18n.t("menu.clip.split"));
    expect(split.getAttribute("aria-disabled")).toBe("true");
    expect(split.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("menu.clip.splitHint"));
  });

  it("previews the block it names", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.preview")));
    await settle();
    expectInvoked("preview_block", { trackIndex: 0 });
  });

  it("opens the piano roll editor for a pattern clip", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.openEditor")));
    await settle();
    expect(panelStore.bottomPanel()).toBe("pianoroll");
  });

  it("copies a clip and enables Paste", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.copy")));
    await settle(1);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    const paste = menuItem(i18n.t("menu.clip.paste"));
    expect(paste.getAttribute("aria-disabled")).toBeNull();
    invokeMock.mockClear();
    fireEvent.click(paste);
    await settle();
    expectInvoked("add_clip", { trackIndex: 0, patternName: "hook" });
  });

  it("deletes the clip it names", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.delete")));
    await settle();
    // One command removes the whole clip selection, so the menu and the
    // Delete key cannot disagree.
    const call = invokeMock.mock.calls.find(([cmd]) => cmd === "remove_blocks");
    expect(call?.[1]).toEqual({ blocks: [{ kind: "clip", trackIndex: 0, index: 0 }] });
  });

  it("renames the pattern through an in-app dialog, not window.prompt", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const prompt = window.prompt;
    let prompted = false;
    window.prompt = () => { prompted = true; return "other"; };
    try {
      await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
      fireEvent.click(menuItem(i18n.t("menu.clip.rename")));
      await settle(1);
      const dialog = screen.getByRole("dialog", { name: i18n.t("menu.renamePrompt") });
      const input = dialog.querySelector<HTMLInputElement>("input")!;
      expect(input.value).toBe("hook");
      fireEvent.input(input, { target: { value: "verse" } });
      fireEvent.submit(dialog);
      await settle();
      expect(prompted).toBe(false);
      expectInvoked("rename_pattern", { patternName: "hook", newName: "verse" });
    } finally {
      window.prompt = prompt;
    }
  });

  it("cancels the rename dialog without touching the project", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.rename")));
    await settle(1);
    const dialog = screen.getByRole("dialog", { name: i18n.t("menu.renamePrompt") });
    fireEvent.click([...dialog.querySelectorAll("button")].find((b) => b.textContent === i18n.t("dialog.cancel"))!);
    await settle(1);
    expect(screen.queryByRole("dialog", { name: i18n.t("menu.renamePrompt") })).toBeNull();
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "update_dsl")).toHaveLength(0);
  });

  it("closes the rename dialog with Escape and with a click outside", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.rename")));
    await settle(1);
    const dialog = screen.getByRole("dialog", { name: i18n.t("menu.renamePrompt") });
    fireEvent.keyDown(dialog, { key: "Escape" });
    await settle(1);
    expect(screen.queryByRole("dialog", { name: i18n.t("menu.renamePrompt") })).toBeNull();

    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    fireEvent.click(menuItem(i18n.t("menu.clip.rename")));
    await settle(1);
    fireEvent.pointerDown(document.querySelector(".text-prompt-overlay")!);
    await settle(1);
    expect(screen.queryByRole("dialog", { name: i18n.t("menu.renamePrompt") })).toBeNull();
  });

  it("splits only when the playhead is inside the clip", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    expect(menuItem(i18n.t("menu.clip.split")).getAttribute("aria-disabled")).toBe("true");

    await store.seek(4);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    const split = menuItem(i18n.t("menu.clip.split"));
    expect(split.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(split);
    await settle(2);
    expectInvoked("split_clip", { trackIndex: 0, clipIndex: 0, atBar: 2, atBeat: 1, atSub: 0 });
    // One command, so one undo entry and no window where the clip is gone.
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_clip")).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "add_clip")).toBe(false);
  });
});

describe("muted track disables preview with a reason", () => {
  it("shows why preview cannot run on a muted track", async () => {
    resetSurface();
    const snapshot = makeTimelineProject();
    snapshot.project.tracks[0].mix.mute = true;
    await loadAuditProject(snapshot);
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    await openClipMenu(view.container);
  openGroup(i18n.t("menu.clip.groupEdit"));
  openGroup(i18n.t("menu.clip.groupContent"));
  openGroup(i18n.t("menu.clip.groupSelection"));
    const preview = menuItem(i18n.t("menu.clip.preview"));
    expect(preview.getAttribute("aria-disabled")).toBe("true");
    expect(preview.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("menu.clip.previewMuted"));
  });
});

describe("track header", () => {
  it("renames a track inline from the menu without a native prompt", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    fireEvent.contextMenu(view.container.querySelector(".track-header")!, { clientX: 30, clientY: 30 });
    await settle(1);
    fireEvent.click(menuItem(i18n.t("menu.track.rename")));
    await settle(1);
    const input = view.container.querySelector<HTMLInputElement>(".track-name-input")!;
    expect(input.value).toBe("Piano");
    fireEvent.input(input, { target: { value: "Keys" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await settle();
    expectInvoked("rename_track", { trackIndex: 0, newName: "Keys" });
  });

  it("adds a track from the add button", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const add = [...view.container.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes(i18n.t("timeline.addTrack")));
    expect(add).toBeTruthy();
    fireEvent.click(add!);
    await settle();
    expectInvoked("add_track");
  });
});

describe("audio clip menu", () => {
  it("deletes an audio clip and explains a split it cannot do", async () => {
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const block = view.container.querySelector<HTMLElement>(".audio-clip-block");
    expect(block).toBeTruthy();
    fireEvent.contextMenu(block!, { clientX: 20, clientY: 20 });
    await settle(1);
    openGroup(i18n.t("menu.clip.groupContent"));
    expect(menuLabels()).toEqual([
      i18n.t("clip.info.inspect"),
      i18n.t("menu.clip.preview"),
      i18n.t("clipPlayback.open"),
      i18n.t("menu.clip.addGainEnvelope"),
      i18n.t("menu.clip.groupContent"),
      i18n.t("menu.clip.resetSlip"),
      i18n.t("menu.clip.loop"),
      i18n.t("menu.clip.split"),
      i18n.t("menu.clip.consolidate"),
      i18n.t("menu.clip.delete"),
    ]);
    const split = menuItem(i18n.t("menu.clip.split"));
    expect(split.getAttribute("aria-disabled")).toBe("true");
    expect(split.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("menu.clip.splitHint"));
    fireEvent.click(menuItem(i18n.t("menu.clip.delete")));
    await settle();
    expectInvoked("remove_audio_clip", { clipIndex: 0 });
  });

  it("splits the audio clip at a playhead inside it", async () => {
    await store.seek(2);
    const view = await mountSurface(() => <><Timeline /><ContextMenuLayer /></>);
    const block = view.container.querySelector<HTMLElement>(".audio-clip-block")!;
    fireEvent.contextMenu(block, { clientX: 20, clientY: 20 });
    await settle(1);
    openGroup(i18n.t("menu.clip.groupContent"));
    fireEvent.click(menuItem(i18n.t("menu.clip.split")));
    await settle(2);
    expectInvoked("split_audio_clip");
  });
});

describe("mixer track context menu", () => {
  it("renames through the in-app dialog", async () => {
    const { Mixer } = await import("../components/Mixer");
    const view = await mountSurface(() => <><Mixer /><ContextMenuLayer /></>);
    const node = view.container.querySelector<HTMLElement>("[data-mixer-node]");
    expect(node).toBeTruthy();
    fireEvent.contextMenu(node!, { clientX: 30, clientY: 30 });
    await settle(1);
    fireEvent.click(menuItem(i18n.t("menu.track.rename")));
    await settle(1);
    const dialog = screen.getByRole("dialog", { name: i18n.t("menu.renamePrompt") });
    fireEvent.input(dialog.querySelector("input")!, { target: { value: "Keys" } });
    fireEvent.submit(dialog);
    await waitFor(() => expectInvoked("rename_track", { trackIndex: 0, newName: "Keys" }));
  });
});
