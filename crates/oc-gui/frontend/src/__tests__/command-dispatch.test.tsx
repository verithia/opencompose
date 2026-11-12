/**
 * The document handler dispatches through the registry. A transport shortcut
 * still reaches its handler through that path, as does the palette trigger.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent } from "@solidjs/testing-library";
import App from "../App";
import { loadAuditProject, makeTimelineProject, mountSurface, resetSurface, settle } from "./ui-audit-support";
import { expectInvoked } from "./setup";
import { overlayStore } from "../stores/overlays";
import { i18n } from "../stores/i18n";

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject(makeTimelineProject());
});

afterEach(() => {
  resetSurface();
  overlayStore.closePalette();
});

describe("registry dispatch", () => {
  it("keeps the transport shortcut working after the refactor", async () => {
    const view = await mountSurface(() => <App />);
    fireEvent.pointerDown(view.container.querySelector(".split-top")!);
    fireEvent.keyDown(view.container, { key: " ", code: "Space" });
    await settle();
    expectInvoked("transport_play");
  });

  it("opens the command palette from its fixed binding", async () => {
    const view = await mountSurface(() => <App />);
    fireEvent.keyDown(view.container, { key: "P", metaKey: true, shiftKey: true });
    await settle();
    expect(overlayStore.paletteOpen()).toBe(true);
  });
});

describe("Cmd+D with a track and nothing else selected", () => {
  it("duplicates the selected track", async () => {
    const view = await mountSurface(() => <App />);
    fireEvent.pointerDown(view.container.querySelector(".split-top")!);
    fireEvent.keyDown(view.container, { key: "d", code: "KeyD", metaKey: true });
    await settle();
    expectInvoked("duplicate_track", { trackIndex: 0, name: "Piano 2" });
  });
});
