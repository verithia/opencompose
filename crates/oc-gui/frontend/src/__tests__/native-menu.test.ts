import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { createSignal } from "solid-js";
import { nativeCommandGroups, mountNativeMenu, runNativeCommand, type NativeMenuAdapter } from "../stores/nativeMenu";
import { registerCommand, resetRegistryForTests } from "../stores/commands";
import { dispatchKeydown, keyContext, readStoredBindings, resetAllBindings, setBinding } from "../stores/keymap";
import { i18n } from "../stores/i18n";
import { preferencesUi } from "../stores/preferencesUi";

const disposers: (() => void)[] = [];
beforeEach(() => { i18n.setLocale("en"); resetAllBindings(); resetRegistryForTests(); preferencesUi.close(); });
afterEach(() => {
  for (const dispose of disposers.splice(0)) dispose();
  document.body.innerHTML = "";
  preferencesUi.close();
});
function command(id: string) {
  return nativeCommandGroups(false).flatMap((group) => group.items).find((item) => item.id === id)!;
}
function fakeAdapter() {
  type Item = {id: string; text: string; enabled: boolean; action?: () => void; accelerator?: string;
    setText: Mock<(text: string) => Promise<void>>; setEnabled: Mock<(enabled: boolean) => Promise<void>>; close: Mock<() => Promise<void>>};
  const items = new Map<string, Item>();
  const predefined: string[] = [];
  let barItems: unknown[] = [];
  const make = (id: string, text: string, enabled = true) => {
    const item: Item = { id, text, enabled, close: vi.fn(async () => {}),
      setText: vi.fn(async (value: string) => { item.text = value; }),
      setEnabled: vi.fn(async (value: boolean) => { item.enabled = value; }) };
    items.set(id, item);
    return item;
  };
  const setAsAppMenu = vi.fn(async () => null);
  const setAsWindowsMenuForNSApp = vi.fn(async () => {});
  const setAsHelpMenuForNSApp = vi.fn(async () => {});
  const adapter: NativeMenuAdapter = {
    item: async (options) => Object.assign(make(options.id, options.text, options.enabled), options),
    predefined: async (id) => { predefined.push(id); return make(`predefined.${id}.${predefined.length}`, id); },
    submenu: async (id, text) => Object.assign(make(id, text), {setAsWindowsMenuForNSApp, setAsHelpMenuForNSApp}),
    menu: async (entries) => {
      barItems = entries;
      return Object.assign(make("bar", "bar"), {setAsAppMenu});
    },
  };
  return { adapter, items, predefined, setAsAppMenu, setAsWindowsMenuForNSApp, setAsHelpMenuForNSApp, barItems: () => barItems };
}

describe("native DAW command menus", () => {
  it("lets a focused range use project Undo without exposing transport or clipboard shortcuts", async () => {
    const undo = vi.fn();
    const play = vi.fn();
    registerCommand("edit.undo", undo);
    registerCommand("transport.playPause", play);
    const range = document.createElement("input");
    range.type = "range";
    document.body.append(range);
    range.focus();
    expect(keyContext().inEditable).toBe(true);
    expect(command("edit.undo").enabled).toBe(true);
    expect(command("edit.cut").enabled).toBe(false);
    expect(await runNativeCommand("edit.undo", false)).toBe(true);
    expect(undo).toHaveBeenCalledOnce();
    dispatchKeydown(new KeyboardEvent("keydown", { key: " ", bubbles: true }));
    expect(play).not.toHaveBeenCalled();
  });

  it.each(["text", "number", "search"])("keeps native text Undo owned by the %s input", async (type) => {
    const undo = vi.fn();
    registerCommand("edit.undo", undo);
    const input = document.createElement("input");
    input.type = type;
    document.body.append(input);
    input.focus();
    expect(command("edit.undo").enabled).toBe(false);
    expect(await runNativeCommand("edit.undo", false)).toBe(false);
    expect(undo).not.toHaveBeenCalled();
  });

  it.each(["q", ","])("keeps the native app shortcut Mod+%s out of DAW bindings", (key) => {
    const result = setBinding("file.save", {key,mod:true,alt:false,shift:false});
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("reserved");
    expect(readStoredBindings(JSON.stringify({version:1,bindings:{"file.save":`Mod+${key}`}}))).toEqual({});
  });

  it("uses registry categories and omits alternate bindings as duplicate actions", () => {
    expect(nativeCommandGroups(false).map((group) => group.id)).toEqual(["file", "edit", "view", "track", "transport", "help"]);
    const ids = nativeCommandGroups(false).flatMap((group) => group.items.map((item) => item.id));
    expect(ids).toContain("file.save");
    expect(ids).toContain("track.duplicate");
    expect(ids).not.toContain("edit.deleteBackspace");
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("shows current keymap hints and localized registry labels", () => {
    expect(setBinding("file.save", { key: "s", mod: true, shift: false, alt: true }).ok).toBe(true);
    expect(command("file.save").text).toContain("Alt");
    const english = command("file.save").text;
    i18n.setLocale("zh-Hans");
    expect(command("file.save").text).not.toBe(english);
  });

  it("rejects unknown, disabled and modal-blocked activations at execution time", async () => {
    const run = vi.fn();
    const [enabled, setEnabled] = createSignal(true);
    registerCommand("track.duplicate", run, { enabled });
    expect(command("track.duplicate").enabled).toBe(true);
    setEnabled(false);
    expect(await runNativeCommand("track.duplicate", false)).toBe(false);
    setEnabled(true);
    expect(await runNativeCommand("track.duplicate", true)).toBe(false);
    expect(await runNativeCommand("unknown.command", false)).toBe(false);
    expect(run).not.toHaveBeenCalled();
    expect(await runNativeCommand("track.duplicate", false)).toBe(true);
    expect(run).toHaveBeenCalledOnce();
  });

  it("leaves text editing to native responder actions", async () => {
    const run = vi.fn();
    registerCommand("edit.undo", run);
    const field = document.createElement("input");
    document.body.append(field);
    field.focus();
    expect(command("edit.undo").enabled).toBe(false);
    expect(await runNativeCommand("edit.undo", false)).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });

  it.each([
    ["macos", ["Minimize", "Maximize", "Fullscreen", "CloseWindow"]],
    ["windows", ["Minimize", "Maximize", "CloseWindow"]],
    ["linux", ["Minimize", "CloseWindow"]],
  ] as const)("only includes supported native Window actions on %s", async (platform, expected) => {
    const fake = fakeAdapter();
    disposers.push(mountNativeMenu({ adapter: async () => fake.adapter, platform, blocked: () => false, onQuit: vi.fn() }));
    await vi.waitFor(() => expect(fake.setAsAppMenu).toHaveBeenCalledOnce());
    const windowActions = fake.predefined.filter((item) => ["Minimize", "Maximize", "Fullscreen", "CloseWindow"].includes(item));
    expect(windowActions).toEqual(expected);
    expect(fake.predefined.includes("Services")).toBe(platform === "macos");
    expect(fake.setAsHelpMenuForNSApp).toHaveBeenCalledTimes(platform === "macos" ? 1 : 0);
  });

  it("installs native text/window actions and one owner per DAW shortcut", async () => {
    const fake = fakeAdapter();
    disposers.push(mountNativeMenu({ adapter: async () => fake.adapter, platform: "macos", blocked: () => false, onQuit: vi.fn() }));
    await vi.waitFor(() => expect(fake.setAsAppMenu).toHaveBeenCalledOnce());
    expect(fake.predefined).toEqual(expect.arrayContaining(["Copy", "Paste", "Cut", "SelectAll", "Undo", "Redo", "CloseWindow", "Services"]));
    expect(fake.predefined).not.toContain("Quit");
    expect(fake.items.get("oc.command.file.save")?.accelerator).toBeUndefined();
    expect(fake.items.get("oc.quit")?.accelerator).toBe("CmdOrCtrl+Q");
    expect(fake.setAsWindowsMenuForNSApp).toHaveBeenCalledOnce();
    expect(fake.setAsHelpMenuForNSApp).toHaveBeenCalledOnce();
    expect((fake.barItems().at(-1) as {id:string}).id).toBe("oc.help");
  });

  it("updates enabled state, locale and binding hints without replacing the menu", async () => {
    const fake = fakeAdapter();
    const [blocked, setBlocked] = createSignal(false);
    const [enabled, setEnabled] = createSignal(true);
    registerCommand("track.duplicate", vi.fn(), { enabled });
    disposers.push(mountNativeMenu({ adapter: async () => fake.adapter, platform: "linux", blocked, onQuit: vi.fn() }));
    await vi.waitFor(() => expect(fake.items.get("oc.command.track.duplicate")?.enabled).toBe(true));
    setEnabled(false);
    await vi.waitFor(() => expect(fake.items.get("oc.command.track.duplicate")?.enabled).toBe(false));
    setBlocked(true);
    await vi.waitFor(() => expect(fake.items.get("oc.command.file.save")?.enabled).toBe(false));
    expect(setBinding("file.save", {key:"s",mod:true,alt:true,shift:false}).ok).toBe(true);
    i18n.setLocale("es");
    await vi.waitFor(() => expect(fake.items.get("oc.file")?.text).toBe(i18n.t("command.category.file")));
    await vi.waitFor(() => expect(fake.items.get("oc.window")?.text).toBe("Ventana"));
    expect(fake.items.get("oc.text")?.text).toBe("Edición de texto");
    expect(fake.items.get("oc.about")?.text).toBe("Acerca de OpenCompose");
    expect(fake.items.get("oc.quit")?.text).toBe("Salir de OpenCompose");
    await vi.waitFor(() => expect(fake.items.get("oc.command.file.save")?.text).toContain("Alt"));
    expect(fake.setAsAppMenu).toHaveBeenCalledOnce();
    expect(fake.setAsWindowsMenuForNSApp).not.toHaveBeenCalled();
  });

  it("guards Quit and Preferences again when their callbacks run", async () => {
    const fake = fakeAdapter();
    const quit = vi.fn();
    const [blocked, setBlocked] = createSignal(false);
    disposers.push(mountNativeMenu({ adapter: async () => fake.adapter, platform: "linux", blocked, onQuit: quit }));
    await vi.waitFor(() => expect(fake.setAsAppMenu).toHaveBeenCalledOnce());
    setBlocked(true);
    fake.items.get("oc.quit")!.action!();
    fake.items.get("oc.preferences")!.action!();
    expect(quit).not.toHaveBeenCalled();
    expect(preferencesUi.open()).toBe(false);
    setBlocked(false);
    fake.items.get("oc.quit")!.action!();
    fake.items.get("oc.preferences")!.action!();
    expect(quit).toHaveBeenCalledOnce();
    expect(preferencesUi.open()).toBe(true);
  });

  it("does not build a menu after disposal while loading the adapter", async () => {
    const fake = fakeAdapter();
    let resolve!: (adapter: NativeMenuAdapter) => void;
    const dispose = mountNativeMenu({adapter: () => new Promise((done) => { resolve = done; }), blocked: () => false, onQuit: vi.fn()});
    dispose();
    resolve(fake.adapter);
    await Promise.resolve();
    await Promise.resolve();
    expect(fake.items.size).toBe(0);
  });

  it("releases all owned native resources once on disposal", async () => {
    const fake = fakeAdapter();
    const dispose = mountNativeMenu({adapter: async () => fake.adapter, blocked: () => false, onQuit: vi.fn()});
    await vi.waitFor(() => expect(fake.setAsAppMenu).toHaveBeenCalledOnce());
    dispose();
    for (const item of fake.items.values()) expect(item.close).toHaveBeenCalledOnce();
  });
});
