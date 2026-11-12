/**
 * Control audit — the transport surface (toolbar file actions, transport,
 * tempo and meter, render, MIDI actions, preferences).
 *
 * The sweep walks every control the surface renders and requires an observable
 * effect (backend call, dialog, DOM change) from each enabled one, and a stated
 * reason from each disabled one. The focused tests cover the defects the sweep
 * found.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, screen } from "@solidjs/testing-library";
import {
  disabledControls,
  driveControl,
  interactiveControls,
  labelOf,
  loadAuditProject,
  mountSurface,
  resetSurface,
  settle,
} from "./ui-audit-support";
import { invokeMock, openDialogMock, saveDialogMock, expectInvoked } from "./setup";
import { Toolbar } from "../components/Toolbar";
import App from "../App";
import { i18n } from "../stores/i18n";
import { exportAudioStore } from "../stores/exportAudio";

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject();
});

afterEach(() => {
  resetSurface();
  i18n.setLocale("en");
});

/** Mount the toolbar fresh, optionally opening a popover first. */
async function mountToolbar(open?: () => void) {
  const view = await mountSurface(() => <Toolbar />);
  open?.();
  await settle(1);
  return view;
}

/** Every enabled control must do something; every disabled one must say why. */
async function sweep(open?: () => void): Promise<string[]> {
  const first = await mountToolbar(open);
  const controls = interactiveControls(first.container.querySelector(".daw-toolbar")!);
  const keys = controlKeys(controls);
  const failures: string[] = [];
  for (let index = 0; index < keys.length; index += 1) {
    resetSurface();
    await loadAuditProject();
    const view = await mountToolbar(open);
    const again = interactiveControls(view.container.querySelector(".daw-toolbar")!);
    const target = again[controlKeys(again).indexOf(keys[index])] ?? again[index];
    if (!target) {
      failures.push(`${keys[index]}: disappeared on remount`);
      continue;
    }
    const outcome = await driveControl(target);
    if (outcome.error) failures.push(`${keys[index]}: threw ${outcome.error}`);
    else if (!outcome.effect) failures.push(`${keys[index]}: no observable effect`);
  }
  return failures;
}

/** A stable identity for a control across mounts: label plus occurrence. */
function controlKeys(controls: { label: string }[]): string[] {
  const seen = new Map<string, number>();
  return controls.map((control) => {
    const count = seen.get(control.label) ?? 0;
    seen.set(control.label, count + 1);
    return `${control.label}#${count}`;
  });
}

describe("transport inventory sweep", () => {
  it("every toolbar control responds or states why it cannot", async () => {
    const failures = await sweep();
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("every control inside Preferences responds or states why it cannot", async () => {
    const failures = await sweep(() => {
      fireEvent.click(screen.getByRole("button", { name: i18n.t("toolbar.preferences") }));
    });
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("every disabled control carries a reason", async () => {
    for (const open of [undefined, () => {
      fireEvent.click(screen.getByRole("button", { name: i18n.t("toolbar.preferences") }));
    }]) {
      resetSurface();
      await loadAuditProject();
      const view = await mountToolbar(open);
      const silent = disabledControls(view.container).filter((control) => !control.reason);
      expect(silent, `disabled without a reason: ${silent.map((c) => c.label).join(", ")}`).toEqual([]);
    }
  });
});

describe("transport defects", () => {
  it("redo accepts the shifted Z a browser reports", async () => {
    const view = await mountSurface(() => <App />);
    fireEvent.keyDown(view.container, { key: "Z", ctrlKey: true, shiftKey: true });
    await settle();
    expectInvoked("redo");
  });

  it("undo still accepts an unshifted z", async () => {
    const view = await mountSurface(() => <App />);
    fireEvent.keyDown(view.container, { key: "z", ctrlKey: true });
    await settle();
    expectInvoked("undo");
  });

  it("disables Apply and explains an out-of-range meter instead of ignoring the click", async () => {
    const view = await mountToolbar();
    fireEvent.click(view.container.querySelector(".time-sig-display")!);
    const field = view.container.querySelector<HTMLInputElement>(".time-sig-field input")!;
    fireEvent.input(field, { target: { value: "40" } });
    const submit = view.container.querySelector<HTMLButtonElement>(".time-sig-apply")!;
    expect(submit.disabled).toBe(true);
    expect(view.container.querySelector(".time-sig-hint")?.textContent).toMatch(/1 to 32/);
    fireEvent.submit(view.container.querySelector(".time-sig-popover")!);
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_time_signature")).toHaveLength(0);
  });

  it("closes the meter popover with Escape", async () => {
    const view = await mountToolbar();
    fireEvent.click(view.container.querySelector(".time-sig-display")!);
    expect(view.container.querySelector(".time-sig-popover")).toBeTruthy();
    fireEvent.keyDown(view.container.querySelector(".time-sig-popover")!, { key: "Escape" });
    await settle(1);
    expect(view.container.querySelector(".time-sig-popover")).toBeNull();
  });

  it("applies a valid meter from the same popover", async () => {
    const view = await mountToolbar();
    fireEvent.click(view.container.querySelector(".time-sig-display")!);
    fireEvent.input(view.container.querySelector<HTMLInputElement>(".time-sig-field input")!, {
      target: { value: "7" },
    });
    fireEvent.submit(view.container.querySelector(".time-sig-popover")!);
    await settle();
    expectInvoked("set_time_signature", { numerator: 7, denominator: 4 });
  });

  it("opens the audio export dialog from the render button instead of a native save panel", async () => {
    const view = await mountToolbar();
    fireEvent.click(
      view.container.querySelector<HTMLButtonElement>(
        `[aria-label="${i18n.t("toolbar.renderWav")}"]`,
      )!,
    );
    await settle();
    expect(exportAudioStore.dialogRequest()).toEqual({ scope: "mix" });
    expect(saveDialogMock.mock.calls).toHaveLength(0);
    expect(invokeMock.mock.calls.filter(([command]) => command === "render_audio")).toHaveLength(0);
  });

  it("opens the same dialog preset to the stem set from the stems button", async () => {
    const view = await mountToolbar();
    fireEvent.click(
      view.container.querySelector<HTMLButtonElement>(
        `[aria-label="${i18n.t("toolbar.renderStems")}"]`,
      )!,
    );
    await settle();
    expect(exportAudioStore.dialogRequest()).toEqual({ scope: "stems" });
    expect(openDialogMock.mock.calls).toHaveLength(0);
  });

  it("offers Export audio in the File menu beside Export MIDI", async () => {
    const view = await mountToolbar();
    fireEvent.click(
      view.container.querySelector(`[aria-label="${i18n.t("toolbar.fileMenu")}"]`)!,
    );
    fireEvent.click(screen.getByText(i18n.t("toolbar.exportAudio")));
    await settle();
    expect(exportAudioStore.dialogRequest()).toEqual({ scope: "mix" });
  });
});

describe("transport labels stay truthful", () => {
  it("names the tempo field so its range is discoverable", async () => {
    const view = await mountToolbar();
    const tempo = view.container.querySelector<HTMLInputElement>(".tempo-input")!;
    expect(labelOf(tempo)).toBe(i18n.t("toolbar.tempo"));
    expect(tempo.getAttribute("min")).toBe("20");
    expect(tempo.getAttribute("max")).toBe("999");
  });
});
