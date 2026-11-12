/**
 * Toolbar interaction tests.
 * Tests transport controls, tempo input, and button rendering.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent, within } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeSavedFile, expectInvoked, TRANSPORT_PLAYING, TRANSPORT_STOPPED, saveDialogMock } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { preferencesUi } from "../../stores/preferencesUi";
import { themeStore } from "../../stores/theme";
import { log } from "../../stores/log";
import { Toolbar, saveAsFileDialog } from "../../components/Toolbar";
import {
  automationWriteArmed,
  resetAutomationWriteForTests,
} from "../../stores/automationWrite";

const windowMock = vi.hoisted(() => ({
  setTitle: vi.fn(),
  close: vi.fn(),
  destroy: vi.fn(),
  onCloseRequested: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => windowMock }));

beforeEach(async () => {
  preferencesUi.close();
  i18n.setLocale("en");
  themeStore.setTheme("default", "dark");
  resetAutomationWriteForTests();
  invokeMock.mockClear();
  windowMock.setTitle.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    if (cmd === "transport_play") return TRANSPORT_PLAYING();
    if (cmd === "transport_stop") return TRANSPORT_STOPPED();
    if (cmd === "transport_pause") return TRANSPORT_STOPPED();
    return makeSnapshot();
  });
  await store.initProject();
});

describe("Toolbar", () => {
  it("keeps secondary transport controls in a dismissible accessible popover", () => {
    const view = render(() => <Toolbar />);
    expect(view.queryByLabelText(i18n.t("toolbar.countIn"))).toBeNull();
    expect(view.container.querySelector(".record-reason")).toBeNull();
    const trigger = view.getByRole("button", {name:i18n.t("toolbar.transportSettings")});
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    // The popover is portalled into the body so the scrolling strip cannot clip it.
    expect(document.body.querySelector(".transport-options-popover")).toBeTruthy();
    expect(within(document.body).getByLabelText(i18n.t("toolbar.clickLevel"))).toBeTruthy();
    fireEvent.keyDown(window, {key:"Escape"});
    expect(document.body.querySelector(".transport-options-popover")).toBeNull();
    expect(document.activeElement).toBe(trigger);
    fireEvent.click(trigger);
    fireEvent.pointerDown(document.body);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });
  it("localizes transport options and keeps the primary icon controls visible", () => {
    const view = render(() => <Toolbar />);
    for (const locale of ["en", "zh-Hans", "zh-Hant", "ja", "es"] as const) {
      i18n.setLocale(locale);
      const trigger = view.getByRole("button", {name:i18n.t("toolbar.transportSettings")});
      fireEvent.click(trigger);
      expect(within(document.body).getByLabelText(i18n.t("toolbar.countIn"))).toBeTruthy();
      expect(within(document.body).getByLabelText(i18n.t("toolbar.clickLevel"))).toBeTruthy();
      // stop + play + loop + record + click; automation write moved into the popover.
      expect(view.container.querySelectorAll(".transport-group > button.transport-btn")).toHaveLength(5);
      expect(view.container.querySelector(".transport-btn.loop svg")).toBeTruthy();
      fireEvent.click(trigger);
    }
  });

  it("leaves the wordmark to the timeline gutter", () => {
    const view = render(() => <Toolbar />);
    expect(view.container.querySelector(".brand")).toBeFalsy();
    expect(view.container.querySelector(".daw-toolbar .project-title")).toBeFalsy();
    expect(
      view.container.querySelector(".daw-toolbar")!.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);
  });

  it("blocks the toolbar drag region on the Preferences overlay", () => {
    const view = render(() => <Toolbar />);
    fireEvent.click(view.getByRole("button", { name: i18n.t("toolbar.preferences") }));
    expect(document.querySelector(".preferences-overlay"))
      .toHaveAttribute("data-tauri-drag-region", "false");
  });

  it("offers Save As for an existing project and uses the selected directory", async () => {
    invokeMock.mockResolvedValueOnce({ ...makeSnapshot(), file_path: "/old/project.ocp", dirty: true });
    await store.initProject();
    saveDialogMock.mockResolvedValueOnce("/new/project.ocp");
    invokeMock.mockImplementation(async (cmd: string) => cmd === "save_file" ? makeSavedFile("/new/project.ocp") : makeSnapshot());
    const view = render(() => <Toolbar />);
    fireEvent.click(view.container.querySelector('[aria-label="More file actions"]')!);
    fireEvent.click(view.getByText("Save As…"));
    await vi.waitFor(() => expectInvoked("save_file", { path: "/new/project.ocp" }));
    await vi.waitFor(() => expect(store.filePath()).toBe("/new/project.ocp"));
    expect(saveDialogMock).toHaveBeenLastCalledWith(expect.objectContaining({ defaultPath: "/old/project.ocp" }));
  });

  it("cancelling Save As or failing its dialog does not write", async () => {
    saveDialogMock.mockResolvedValueOnce(null);
    expect(await saveAsFileDialog()).toBe(false);
    saveDialogMock.mockRejectedValueOnce(new Error("dialog unavailable"));
    expect(await saveAsFileDialog()).toBe(false);
    expect(store.error()).toContain("dialog unavailable");
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "save_file")).toHaveLength(0);
  });

  it("updates the path and window title after Save As", async () => {
    invokeMock.mockResolvedValueOnce({ ...makeSnapshot(), file_path: "/old/project.ocp", dirty: true });
    await store.initProject();
    saveDialogMock.mockResolvedValueOnce("/new/project");
    invokeMock.mockImplementation(async (cmd: string) => cmd === "save_file" ? makeSavedFile("/new/project.ocp") : makeSnapshot());

    expect(await saveAsFileDialog()).toBe(true);

    expectInvoked("save_file", { path: "/new/project.ocp" });
    await vi.waitFor(() => expect(store.filePath()).toBe("/new/project.ocp"));
    expect(store.dirty()).toBe(false);
    await vi.waitFor(() => {
      expect(windowMock.setTitle).toHaveBeenLastCalledWith(expect.stringContaining("project.ocp"));
    });
    expect(windowMock.setTitle).toHaveBeenLastCalledWith(expect.stringContaining("Test Project"));
  });

  it("surfaces a failed Save As write and keeps the project dirty", async () => {
    invokeMock.mockResolvedValueOnce({ ...makeSnapshot(), file_path: "/old/project.ocp", dirty: true });
    await store.initProject();
    saveDialogMock.mockResolvedValueOnce("/new/project.ocp");
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "save_file") throw new Error("disk full");
      return makeSnapshot();
    });

    expect(await saveAsFileDialog()).toBe(false);

    expect(store.error()).toContain("disk full");
    expect(store.dirty()).toBe(true);
    expect(store.filePath()).toBe("/old/project.ocp");
  });

  it("changes the project time signature from the toolbar editor", async () => {
    const { container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".time-sig-display")!);

    const numerator = container.querySelector(".time-sig-popover input[type='number']") as HTMLInputElement;
    fireEvent.input(numerator, { target: { value: "7" } });
    fireEvent.change(container.querySelector(".time-sig-popover select")!, { target: { value: "8" } });

    const snapshot = makeSnapshot();
    snapshot.project.properties.time_signature = { numerator: 7, denominator: 8 };
    invokeMock.mockResolvedValueOnce(snapshot);
    fireEvent.submit(container.querySelector(".time-sig-popover")!);

    await vi.waitFor(() => {
      expectInvoked("set_time_signature", { numerator: 7, denominator: 8 });
    });
    await vi.waitFor(() => {
      expect(container.querySelector(".time-sig-display")?.textContent).toContain("7/8");
    });
    expect(container.querySelector(".time-sig-popover")).toBeFalsy();
  });

  it("rejects an invalid time signature without invoking the backend", async () => {
    const { container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".time-sig-display")!);

    const numerator = container.querySelector(".time-sig-popover input[type='number']") as HTMLInputElement;
    fireEvent.input(numerator, { target: { value: "0" } });
    fireEvent.submit(container.querySelector(".time-sig-popover")!);

    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_time_signature")).toHaveLength(0);
    expect(container.querySelector(".time-sig-popover")).toBeTruthy();
  });

  it("renders without crashing", () => {
    const { container } = render(() => <Toolbar />);
    expect(container.querySelector(".daw-toolbar")).toBeTruthy();
  });

  it("renders transport buttons (stop + play/pause)", () => {
    const { container } = render(() => <Toolbar />);
    const transportBtns = container.querySelectorAll(".transport-btn");
    expect(transportBtns.length).toBe(6); // stop + play + loop + record + click + settings
  });

  it("arms automation write from the transport settings", () => {
    const { container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".transport-options-toggle")!);
    const arm = document.body.querySelector<HTMLButtonElement>(".automation-write");
    expect(arm).toBeTruthy();
    expect(arm!.getAttribute("aria-label")).toBe(i18n.t("automation.write.arm"));
    expect(arm!.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(arm!);
    expect(automationWriteArmed()).toBe(true);
    expect(arm!.getAttribute("aria-pressed")).toBe("true");
    expect(arm!.classList.contains("active")).toBe(true);

    fireEvent.click(arm!);
    expect(automationWriteArmed()).toBe(false);
  });

  it("shows play icon when stopped", () => {
    const { container } = render(() => <Toolbar />);
    const playBtn = container.querySelector(".transport-btn.play");
    // Play button contains an SVG icon (triangle path)
    expect(playBtn?.querySelector("svg")).toBeTruthy();
  });

  it("clicking play triggers transport_play", async () => {
    const { container } = render(() => <Toolbar />);
    const playBtn = container.querySelector(".transport-btn.play") as HTMLElement;
    fireEvent.click(playBtn);

    await vi.waitFor(() => {
      expectInvoked("transport_play");
    });
  });

  it("clicking stop triggers transport_stop", async () => {
    const { container } = render(() => <Toolbar />);
    const stopBtn = container.querySelector(".transport-btn.stop") as HTMLElement;
    fireEvent.click(stopBtn);

    await vi.waitFor(() => {
      expectInvoked("transport_stop");
    });
  });

  it("renders tempo input with current value", () => {
    const { container } = render(() => <Toolbar />);
    const tempoInput = container.querySelector(".tempo-input") as HTMLInputElement;
    expect(tempoInput).toBeTruthy();
    expect(parseFloat(tempoInput.value)).toBe(128); // from makeSnapshot
  });

  it("changing tempo triggers set_tempo", async () => {
    const { container } = render(() => <Toolbar />);
    const tempoInput = container.querySelector(".tempo-input") as HTMLInputElement;

    invokeMock.mockResolvedValueOnce(makeSnapshot());
    fireEvent.change(tempoInput, { target: { value: "140" } });

    await vi.waitFor(() => {
      expectInvoked("set_tempo", { tempo: 140 });
    });
  });

  it("displays time signature", () => {
    const { container } = render(() => <Toolbar />);
    const timeSig = container.querySelector(".time-sig-display");
    expect(timeSig?.textContent).toContain("4/4");
  });

  it("displays position in bar:beat:sub format", () => {
    const { container } = render(() => <Toolbar />);
    const posDisplay = container.querySelector(".position-display");
    expect(posDisplay?.textContent).toMatch(/\d+:\d+:\d+/);
  });

  it("leaves the project title to the status bar", () => {
    const { container } = render(() => <Toolbar />);
    expect(container.querySelector(".project-title")).toBeFalsy();
  });

  it("leaves the dirty indicator to the status bar", async () => {
    // Make the project dirty via an operation
    invokeMock.mockResolvedValueOnce({ ...makeSnapshot(), dirty: true });
    await store.addTrack("X", "piano", "grand");

    const { container } = render(() => <Toolbar />);
    expect(container.querySelector(".dirty-indicator")).toBeFalsy();
  });

  it("renders AI toggle button", () => {
    const { container } = render(() => <Toolbar />);
    const text = container.textContent;
    expect(text).toContain("AI");
  });

  it("keeps Save As in the file menu", () => {
    const { container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector('[aria-label="More file actions"]')!);
    expect(container.textContent).toContain("Save As");
  });

  it("switches toolbar labels to Simplified Chinese", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;

    fireEvent.click(preferencesBtn);
    const localeSelect = container.querySelector(".locale-select") as HTMLSelectElement;

    fireEvent.change(localeSelect, { target: { value: "zh-Hans" } });

    expect(container.querySelector(".preferences-modal")).toBeTruthy();
    expect(container.textContent).toContain("主题");
    expect(container.textContent).toContain("变体");
    expect(container.textContent).toContain("语言");
    expect(container.querySelector('[aria-label="导出 WAV"]')).toBeTruthy();
  });

  it("keeps theme family, variant, and language controls in preferences", () => {
    const { baseElement: container } = render(() => <Toolbar />);

    expect(container.querySelector(".theme-select")).toBeFalsy();
    expect(container.querySelector(".locale-select")).toBeFalsy();

    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;
    fireEvent.click(preferencesBtn);

    expect(container.querySelector(".preferences-modal .theme-family-select")).toBeTruthy();
    expect(container.querySelector(".preferences-modal .theme-variant-select")).toBeTruthy();
    expect(container.querySelector(".preferences-modal .locale-select")).toBeTruthy();
  });

  it("scopes the preview grid to the selected theme family", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;

    fireEvent.click(preferencesBtn);

    expect(container.querySelectorAll(".theme-preview-card")).toHaveLength(2);
    const card = (label: string) => [...container.querySelectorAll<HTMLButtonElement>(".theme-preview-card")]
      .find((entry) => entry.textContent?.includes(label));
    expect(card("Dark")?.querySelector(".theme-preview-mini")).toBeTruthy();
    expect(card("Prism")).toBeFalsy();

    const familySelect = container.querySelector(".theme-family-select") as HTMLSelectElement;
    fireEvent.change(familySelect, { target: { value: "classic" } });

    await vi.waitFor(() => {
      expect(container.querySelectorAll(".theme-preview-card")).toHaveLength(9);
    });
    expect(card("Prism")?.querySelector(".theme-preview-mini")).toBeTruthy();
  });

  it("updates variant options when the family changes and applies the variant", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;
    fireEvent.click(preferencesBtn);

    const familySelect = container.querySelector(".theme-family-select") as HTMLSelectElement;
    fireEvent.change(familySelect, { target: { value: "catppuccin" } });

    const variantSelect = container.querySelector(".theme-variant-select") as HTMLSelectElement;
    await vi.waitFor(() => {
      expect(Array.from(variantSelect.options).map((option) => option.value)).toEqual([
        "latte",
        "frappe",
        "macchiato",
        "mocha",
      ]);
    });
    expect(themeStore.id()).toBe("catppuccin/latte");

    fireEvent.change(variantSelect, { target: { value: "mocha" } });

    expect(themeStore.id()).toBe("catppuccin/mocha");
    expect(themeStore.cssTheme()).toBe("catppuccin-mocha");
    expect(document.documentElement.dataset.theme).toBe("catppuccin-mocha");
    expect([...container.querySelectorAll(".theme-preview-card")].find((entry) => entry.textContent?.includes("Mocha"))?.classList.contains("active")).toBe(true);
    expect([...container.querySelectorAll(".theme-preview-card")].find((entry) => entry.textContent?.includes("Latte"))?.classList.contains("active")).toBe(false);
  });

  it("selection from the preview grid updates the active variant", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;
    fireEvent.click(preferencesBtn);

    const familySelect = container.querySelector(".theme-family-select") as HTMLSelectElement;
    fireEvent.change(familySelect, { target: { value: "classic" } });

    const prism = await vi.waitFor(() => {
      const card = [...container.querySelectorAll<HTMLButtonElement>(".theme-preview-card")]
        .find((entry) => entry.textContent?.includes("Prism"));
      expect(card).toBeTruthy();
      return card!;
    });
    fireEvent.click(prism);

    expect(themeStore.id()).toBe("classic/prism");
    expect(document.documentElement.dataset.theme).toBe("prism");
    expect(prism.classList.contains("active")).toBe(true);
  });

  it("shows shortcut keymapping in preferences", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const preferencesBtn = container.querySelector(".preferences-btn") as HTMLButtonElement;

    fireEvent.click(preferencesBtn);
    fireEvent.click(container.querySelector('[data-page="keymap"]') as HTMLElement);

    expect(container.querySelector(".shortcut-table")).toBeTruthy();
    expect(container.textContent).toContain("Space");
    expect(container.textContent).toContain("Cmd/Ctrl+S");
    expect(container.textContent).toContain("Piano Roll");
  });

  it("renders new/open/save file buttons", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const toolbarBtns = container.querySelectorAll(".toolbar-btn");
    // Should have at least: new, open, save, render, AI = 5+
    expect(toolbarBtns.length).toBeGreaterThanOrEqual(5);
  });

  it("dismisses preferences on an outside pointerdown", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
    expect(container.querySelector(".preferences-modal")).toBeTruthy();

    fireEvent.pointerDown(container.querySelector(".preferences-overlay") as HTMLElement);
    expect(container.querySelector(".preferences-modal")).toBeFalsy();
  });

  it("keeps preferences open for clicks inside it and on the tooltip layer", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
    const modal = container.querySelector(".preferences-modal") as HTMLElement;
    expect(modal).toBeTruthy();

    fireEvent.pointerDown(modal.querySelector("select") as HTMLElement);
    expect(container.querySelector(".preferences-modal")).toBeTruthy();

    const tooltip = document.createElement("div");
    tooltip.className = "oc-tooltip";
    document.body.appendChild(tooltip);
    fireEvent.pointerDown(tooltip);
    expect(container.querySelector(".preferences-modal")).toBeTruthy();
    tooltip.remove();
  });

  it("keeps the export and MIDI actions as compact icon buttons", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    for (const selector of [".render-btn", ".midi-toolbar-import", ".midi-toolbar-export"]) {
      expect(container.querySelector(selector)).toBeTruthy();
    }
    // The compact icon actions carry no text label; their names come from
    // aria-label and tooltips. Preferences and Browse name themselves.
    for (const selector of [".render-btn", ".midi-toolbar-import", ".midi-toolbar-export", ".dsl-btn"]) {
      expect(container.querySelector(`${selector} .toolbar-btn-label`)).toBeFalsy();
    }
    expect(container.querySelector(".preferences-btn .toolbar-btn-label")).toBeTruthy();
    expect(container.querySelector(".browse-btn .toolbar-btn-label")).toBeTruthy();
  });

  it("keeps the workspace switch, Save As, recovery and the MIDI import/export in the file menu", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector('[aria-label="More file actions"]')!);
    const items = [...container.querySelectorAll(".toolbar-menu-item")].map((item) => item.textContent?.trim());
    expect(items).toEqual([
      "Switch workspace…",
      "Save As…Cmd/Ctrl+Shift+S",
      "Recover autosave…",
      "Import MIDI…",
      "Import Aligned Stems…",
      "Export MIDI…",
      "Export audio…Cmd/Ctrl+E",
    ]);
  });

  it("gives every icon action a distinct accessible name", () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const names = [
      ".render-btn",
      ".midi-toolbar-import",
      ".midi-toolbar-export",
      ".dsl-btn",
      ".ai-btn",
      ".preferences-btn",
    ].map((selector) => container.querySelector(selector)!.getAttribute("aria-label"));
    expect(new Set(names).size).toBe(names.length);
  });

  it("exports diagnostics through the save dialog and reports the archive path", async () => {
    log.error("render", "boom");
    saveDialogMock.mockResolvedValueOnce("/tmp/opencompose-diagnostics-test.tar.gz");
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "export_diagnostics") return "/tmp/opencompose-diagnostics-test.tar.gz";
      if (cmd === "scan_plugins") return [];
      return makeSnapshot();
    });
    const { baseElement: container } = render(() => <Toolbar />);
    fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
    fireEvent.click(container.querySelector('[data-page="help"]') as HTMLElement);
    fireEvent.click(container.querySelector(".diagnostics-btn") as HTMLElement);

    await vi.waitFor(() => expectInvoked("export_diagnostics"));
    const call = invokeMock.mock.calls.filter(([cmd]) => cmd === "export_diagnostics").at(-1)!;
    const args = call[1] as { dest: string; logs: { level: string; message: string }[] };
    expect(args.dest).toBe("/tmp/opencompose-diagnostics-test.tar.gz");
    expect(args.logs.some((entry) => entry.level === "error" && entry.message === "boom")).toBe(true);
    expect(await within(container).findByText(/Diagnostics saved to \/tmp\/opencompose-diagnostics-test\.tar\.gz/)).toBeTruthy();
    await vi.waitFor(() =>
      expectInvoked("reveal_diagnostics", { path: "/tmp/opencompose-diagnostics-test.tar.gz" }),
    );
  });

  it("closes preferences on Escape and restores focus to its button", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    const button = container.querySelector(".preferences-btn") as HTMLButtonElement;
    fireEvent.click(button);
    const select = container.querySelector(".preferences-modal select") as HTMLSelectElement;
    select.focus();
    expect(container.querySelector(".preferences-modal")).toBeTruthy();

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(container.querySelector(".preferences-modal")).toBeFalsy();
    await vi.waitFor(() => expect(document.activeElement).toBe(button));
  });
});
