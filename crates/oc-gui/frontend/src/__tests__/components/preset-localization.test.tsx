import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { PresetEditor } from "../../components/PresetEditor";
import { PresetPicker } from "../../components/PresetPicker";
import { i18n, type I18nKey } from "../../stores/i18n";
import { presetCopy } from "../../stores/locales/presetCopy";
import { store } from "../../stores/project";
import { FALLBACK_DEVICES } from "../../devices/catalog";
import { chooseDropdown, createOwnedUndoFixture, invokeMock, makeSnapshot, selectPreset, TRANSPORT_STOPPED } from "../setup";

beforeEach(() => {
  i18n.setLocale("en");
  invokeMock.mockClear();
  const owned = createOwnedUndoFixture();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const ownership = owned.route(command, args);
    if (ownership) return ownership.value;
    command = owned.mutation(command, args);
    if (command === "scan_plugins") return [];
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "get_project" || command === "set_preset_param") return makeSnapshot();
    if (command === "set_loop_region" || command === "release_web_audio") return undefined;
    if (command === "devices_catalog") return { devices: FALLBACK_DEVICES };
    if (command === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    throw new Error(`Unexpected localized-preset fixture command: ${command}`);
  });
});
afterEach(() => { cleanup(); i18n.setLocale("en"); });

describe("localized preset controls", () => {
  it("changes the picker language in place and searches localized GM names without renaming the selected source", () => {
    const onPick = vi.fn();
    render(() => <PresetPicker onPick={onPick} onCancel={() => {}} existingNames={["Piano 1"]} />);
    expect(screen.getByText("New preset")).toBeVisible();
    i18n.setLocale("zh-Hans");
    expect(screen.getByText("新建预设")).toBeVisible();
    expect(screen.getByText("钢琴")).toBeVisible();
    fireEvent.input(screen.getByRole("textbox", { name: "搜索音色…" }), { target: { value: "三角钢琴" } });
    fireEvent.click(screen.getByText("三角钢琴"));
    expect(onPick).toHaveBeenCalledWith(expect.objectContaining({ id: "gm:0", program: 0, category: "Piano", name: "Acoustic Grand" }), "Piano 2");
  });

  it("searches canonical English names and program IDs while displaying Spanish labels", () => {
    i18n.setLocale("es");
    const onPick = vi.fn();
    render(() => <PresetPicker onPick={onPick} onCancel={() => {}} existingNames={[]} />);
    const search = screen.getByRole("textbox", { name: "Buscar sonidos…" });
    fireEvent.input(search, { target: { value: "Acoustic Grand" } });
    expect(screen.getByText("Piano de cola")).toBeVisible();
    fireEvent.input(search, { target: { value: "Acoustic Grand Piano" } });
    expect(screen.getByText("Piano de cola")).toBeVisible();
    fireEvent.input(search, { target: { value: "gm:127" } });
    fireEvent.click(screen.getByText("Disparo"));
    expect(onPick).toHaveBeenCalledWith(expect.objectContaining({ id: "gm:127", program: 127, name: "Gunshot" }), "SFX 1");
  });

  it("keeps plugin names and IDs while localizing the plugin category", async () => {
    i18n.setLocale("ja");
    invokeMock.mockImplementation(async (command: string) => command === "scan_plugins"
      ? [{ id: "org.example.synth", name: "Example Synth", vendor: "Example Audio", category: "Instrument", format: "Vst3" }]
      : undefined);
    const onPick = vi.fn();
    render(() => <PresetPicker onPick={onPick} onCancel={() => {}} existingNames={[]} />);
    fireEvent.click(await screen.findByText("プラグイン"));
    expect(screen.getByText("Example Audio")).toBeVisible();
    fireEvent.click(screen.getByText("Example Synth"));
    expect(onPick).toHaveBeenCalledWith(expect.objectContaining({ id: "vst3:org.example.synth", pluginId: "org.example.synth", kind: "vst3" }), "Example Synth 1");
  });

  it("localizes the scan failure while preserving the diagnostic", async () => {
    i18n.setLocale("zh-Hant");
    invokeMock.mockImplementation(async () => { throw "Scanner unavailable"; });
    render(() => <PresetPicker onPick={() => {}} onCancel={() => {}} existingNames={[]} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("外掛掃描失敗：Scanner unavailable");
  });

  it("retranslates existing validation messages without changing preset or parameter identifiers", async () => {
    await store.initProject();
    const { container } = render(() => <PresetEditor />);
    selectPreset(container, "Piano 1");
    const advanced = container.querySelector<HTMLDetailsElement>(".preset-advanced")!;
    advanced.open = true;
    expect(screen.getByText("Advanced instrument settings")).toBeVisible();
    fireEvent.input(screen.getByLabelText("CC number"), { target: { value: "128" } });
    fireEvent.click(screen.getByLabelText("Map CC parameter"));
    expect(screen.getByRole("alert")).toHaveTextContent("CC number must be 0-127.");
    i18n.setLocale("zh-Hans");
    expect(screen.getByText("乐器高级设置")).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("CC 编号必须在 0–127 之间。");
    expect(invokeMock.mock.calls.some(([command]) => command === "set_preset_param" || command === "set_preset_param_owned")).toBe(false);
    chooseDropdown(screen.getByLabelText("音符表现类型"), "颤音");
    fireEvent.click(screen.getByLabelText("映射音符表现"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
      token: expect.any(String),
      name: "Piano 1", paramName: "control_pressure_note_expression", paramValue: "vibrato",
    }));
  });

  it.each(i18n.locales)("ships its own complete preset copy in %s", (locale) => {
    for (const key of Object.keys(presetCopy.en) as I18nKey[]) expect(i18n.has(locale, key), key).toBe(true);
    for (let program = 0; program < 128; program++) expect(i18n.has(locale, `preset.program.${program}` as I18nKey)).toBe(true);
  });
});
