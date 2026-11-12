/**
 * The workspace dialog over the app: a dialog taller than the viewport must
 * scroll from its own header, never put the header and close button above the
 * top of the scroll area. vitest runs with `css: false`, so the real stylesheet
 * is injected and the computed values are read from the rendered structure.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { invokeMock, makeSnapshot } from "./setup";
import { workspaceStore } from "../stores/workspace";
import { WorkspaceChooserDialog } from "../components/WorkspaceChooser";

let injected: HTMLStyleElement[] = [];

function injectStyles(...names: string[]) {
  const element = document.createElement("style");
  element.textContent = names
    .map((name) => readFileSync(`${process.cwd()}/src/styles/daw/${name}`, "utf8"))
    .join("\n");
  document.head.append(element);
  injected.push(element);
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string) => {
    switch (command) {
      case "workspace_current":
        return { path: null, name: null, detached: false, chosen: false, exists: true, projects: [], total_projects: 0 };
      case "workspaces_list": return [];
      default: return makeSnapshot();
    }
  });
});

afterEach(() => {
  cleanup();
  for (const element of injected) element.remove();
  injected = [];
});

describe("the workspace dialog's overlay", () => {
  it("centres with an auto margin so a tall dialog scrolls from its header", async () => {
    injectStyles("base.css", "panels.css");
    await workspaceStore.init();
    render(() => <WorkspaceChooserDialog onClose={() => {}} />);

    const overlay = document.querySelector(".workspace-overlay") as HTMLElement;
    const body = document.querySelector(".workspace-overlay-body") as HTMLElement;
    const dialog = document.querySelector(".workspace-chooser-dialog") as HTMLElement;
    const header = dialog.querySelector(".workspace-hero") as HTMLElement;
    const close = dialog.querySelector(".workspace-chooser-close") as HTMLElement;

    // The overlay is the scroller, and the body is its direct child: when the
    // dialog is taller than the viewport, the offset starts at its top instead
    // of centring the overflow above the reachable area.
    expect(overlay.firstElementChild).toBe(body);
    expect(getComputedStyle(overlay).overflowY).toBe("auto");
    expect(getComputedStyle(overlay).alignItems).toBe("flex-start");
    expect(getComputedStyle(body).marginTop).toBe("auto");
    // The header and its close button are inside the scrolling body, so they
    // move with the dialog and stay reachable.
    expect(body.contains(dialog)).toBe(true);
    expect(dialog.contains(header)).toBe(true);
    expect(header.contains(close)).toBe(true);
  });
});
