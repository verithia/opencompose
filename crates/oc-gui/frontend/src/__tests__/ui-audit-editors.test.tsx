/**
 * Control audit — the DSL editor and the piano roll toolbar.
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
import { invokeMock, expectInvoked, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import { DslEditor } from "../components/DslEditor";
import { PianoRoll } from "../components/PianoRoll";
import { i18n } from "../stores/i18n";

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject(makeTimelineProject());
});

afterEach(() => {
  resetSurface();
  i18n.setLocale("en");
});

const applyButton = () => screen.getByRole<HTMLButtonElement>("button", { name: i18n.t("dsl.apply") });

describe("DSL editor", () => {
  it("disables Apply with a reason while the draft matches the project", async () => {
    const view = await mountSurface(() => <DslEditor />);
    const button = view.container.querySelector<HTMLButtonElement>(".dsl-apply")!;
    expect(button.disabled).toBe(true);
    expect(button.getAttribute("title")).toBe(i18n.t("dsl.noChanges"));
    expect(view.container.querySelector(".dsl-editor-modified")).toBeNull();
  });

  it("applies typed DSL and clears the modified marker", async () => {
    const view = await mountSurface(() => <DslEditor />);
    const textarea = view.container.querySelector("textarea")!;
    fireEvent.input(textarea, { target: { value: '@project "Edited"\n  tempo: 90\n  time_signature: 4/4\n' } });
    await settle(1);
    expect(view.container.querySelector(".dsl-editor-modified")).toBeTruthy();
    expect(applyButton().disabled).toBe(false);
    fireEvent.click(applyButton());
    await settle();
    expectInvoked("update_dsl");
    expect((getLastDsl() ?? "")).toContain('@project "Edited"');
    await waitFor(() => expect(view.container.querySelector(".dsl-editor-modified")).toBeNull());
    expect(applyButton().disabled).toBe(true);
  });

  it("keeps an unsaved draft when the panel is unmounted", async () => {
    const first = await mountSurface(() => <DslEditor />);
    fireEvent.input(first.container.querySelector("textarea")!, {
      target: { value: '@project "Draft"\n  tempo: 100\n  time_signature: 4/4\n' },
    });
    await settle(1);
    resetSurface();
    const second = await mountSurface(() => <DslEditor />);
    const textarea = second.container.querySelector<HTMLTextAreaElement>("textarea")!;
    expect(textarea.value).toContain('@project "Draft"');
    expect(second.container.querySelector(".dsl-editor-modified")).toBeTruthy();
  });

  it("shows a refused apply as an alert and keeps the draft", async () => {
    const view = await mountSurface(() => <DslEditor />);
    fireEvent.input(view.container.querySelector("textarea")!, { target: { value: "not dsl" } });
    invokeMock.mockRejectedValueOnce(new Error("Parse error at line 1"));
    fireEvent.click(applyButton());
    const alert = await waitFor(() => screen.getByRole("alert"));
    expect(alert.textContent).toMatch(/Parse error/);
    expect(view.container.querySelector(".dsl-editor-modified")).toBeTruthy();
  });

  it("applies on Cmd+Enter as the tooltip promises", async () => {
    const view = await mountSurface(() => <DslEditor />);
    const textarea = view.container.querySelector("textarea")!;
    fireEvent.input(textarea, { target: { value: '@project "Keys"\n  tempo: 100\n  time_signature: 4/4\n' } });
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    await settle();
    expectInvoked("update_dsl");
  });
});

describe("piano roll sweep", () => {
  it("every toolbar control responds or states why it cannot", async () => {
    const failures: string[] = [];
    const mount = async () => {
      store.setSelectedTrackIndex(0);
      store.setSelectedPatternName("hook");
      panelStore.openBottom("pianoroll");
      return mountSurface(() => <PianoRoll />);
    };
    const first = await mount();
    const keys = interactiveControls(first.container.querySelector(".pianoroll-toolbar")!)
      .map((control) => control.label);
    for (const label of keys) {
      resetSurface();
      await loadAuditProject(makeTimelineProject());
      const view = await mount();
      const controls = interactiveControls(view.container.querySelector(".pianoroll-toolbar")!);
      const target = controls[keys.indexOf(label)];
      if (!target) {
        failures.push(`${label}: disappeared on remount`);
        continue;
      }
      const outcome = await driveControl(target);
      if (outcome.error) failures.push(`${label}: threw ${outcome.error}`);
      else if (!outcome.effect) failures.push(`${label}: no observable effect`);
    }
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("selects the tool it names", async () => {
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    const view = await mountSurface(() => <PianoRoll />);
    const draw = view.container.querySelector<HTMLButtonElement>('[aria-label="Draw tool"]')!;
    fireEvent.click(draw);
    await settle(1);
    expect(draw.className).toContain("active");
    const select = view.container.querySelector<HTMLButtonElement>('[aria-label="Select tool"]')!;
    fireEvent.click(select);
    await settle(1);
    expect(select.className).toContain("active");
  });

  it("changes the grid the note tools snap to", async () => {
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    const view = await mountSurface(() => <PianoRoll />);
    const grid = view.container.querySelector<HTMLSelectElement>(".pianoroll-toolbar select")!;
    fireEvent.change(grid, { target: { value: "8" } });
    await settle(1);
    const { gridDivision } = await import("../stores/grid");
    expect(gridDivision()).toBe(8);
  });

  it("previews the instrument when a piano key is played", async () => {
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    const view = await mountSurface(() => <PianoRoll />);
    const key = view.container.querySelector<HTMLElement>(".piano-key")!;
    fireEvent.click(key);
    await settle();
    expect(invokeMock.mock.calls.some(([command]) => command === "preview_track" || command === "preview_preset"))
      .toBe(true);
  });

  it("renders a labelled velocity lane", async () => {
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    const view = await mountSurface(() => <PianoRoll />);
    const lane = view.container.querySelector('[aria-label="Velocity lane"]');
    expect(lane).toBeTruthy();
  });
});

function getLastDsl(): string | undefined {
  const call = [...invokeMock.mock.calls].reverse()
    .find(([command]) => command === "update_dsl") as [string, { source?: string }] | undefined;
  return call?.[1]?.source;
}

// Keep the snapshot helper reachable for the shared fixture contract.
void makeSnapshot;
