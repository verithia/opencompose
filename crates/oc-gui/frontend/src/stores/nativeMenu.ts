/**
 * Desktop menus are a view of the command registry. DAW shortcuts stay owned by
 * the webview keymap: menu labels show their current bindings without installing
 * a second native accelerator that could execute the same command twice.
 */
import { createEffect, createRoot, createSignal } from "solid-js";
import { isTauri } from "@tauri-apps/api/core";
import { COMMANDS, categoryLabel, commandAllowsContext, commandById, commandEnabled, commandLabel,
  commandHandlerVersion, runCommand, setDispatchContext, type CommandCategory } from "./commands";
import { commandBindingLabel, isTextEditingTarget, keyContext } from "./keymap";
import { i18n } from "./i18n";
import { preferencesUi } from "./preferencesUi";
import { log } from "./log";

export type NativePlatform = "macos" | "windows" | "linux";
// Navigator identifies the OS; the user-editable appearance data attribute does not.
const DESKTOP_PLATFORM: NativePlatform = typeof navigator !== "undefined" && /Mac/i.test(navigator.platform)
  ? "macos" : typeof navigator !== "undefined" && /Win/i.test(navigator.platform) ? "windows" : "linux";

/** Tauri predefined window actions have platform-specific support. */
export function nativeWindowActions(platform: NativePlatform): ("Minimize" | "Maximize" | "Fullscreen" | "CloseWindow")[] {
  if (platform === "macos") return ["Minimize", "Maximize", "Fullscreen", "CloseWindow"];
  if (platform === "windows") return ["Minimize", "Maximize", "CloseWindow"];
  return ["Minimize", "CloseWindow"];
}

export interface NativeCommandItem { id: string; text: string; enabled: boolean }
export interface NativeCommandGroup { id: CommandCategory; text: string; items: NativeCommandItem[] }

const GROUPS: CommandCategory[] = ["file", "edit", "view", "track", "transport", "help"];
// Backspace is a second binding for Delete, not another menu action.
const OMITTED = new Set(["edit.deleteBackspace"]);

const editorOwnsNativeCommand = (id: string, category: CommandCategory, inEditable: boolean) => {
  if (category !== "edit") return false;
  if (id === "edit.undo" || id === "edit.redo") return isTextEditingTarget(document.activeElement);
  return inEditable;
};

export function nativeCommandGroups(blocked: boolean): NativeCommandGroup[] {
  commandHandlerVersion();
  const context = keyContext();
  return GROUPS.map((category) => ({
    id: category,
    text: categoryLabel(category),
    items: COMMANDS.filter((command) => command.category === category && !OMITTED.has(command.id))
      .map((command) => {
        const binding = commandBindingLabel(command.id);
        return {
          id: command.id,
          text: `${commandLabel(command)}${binding ? `    ${binding}` : ""}`,
          enabled: !blocked && commandEnabled(command) && commandAllowsContext(command, context)
            && !editorOwnsNativeCommand(command.id, category, context.inEditable),
        };
      }),
  }));
}

/** Recheck at activation: a modal/selection may change after the menu opened. */
export async function runNativeCommand(id: string, blocked: boolean): Promise<boolean> {
  if (blocked || OMITTED.has(id)) return false;
  const command = commandById(id);
  const context = keyContext();
  if (!command || !commandAllowsContext(command, context)
    || editorOwnsNativeCommand(command.id, command.category, context.inEditable)) return false;
  setDispatchContext(context, null);
  return runCommand(id);
}

interface NativeItem {
  setText(text: string): Promise<void>;
  setEnabled(enabled: boolean): Promise<void>;
  close(): Promise<void>;
}
interface NativeResource { close(): Promise<void> }
interface NativeSubmenu extends NativeItem {
  setAsWindowsMenuForNSApp?(): Promise<void>;
  setAsHelpMenuForNSApp?(): Promise<void>;
}
interface NativeBar extends NativeResource { setAsAppMenu(): Promise<NativeResource | null> }
export interface NativeMenuAdapter {
  item(options: {id: string; text: string; enabled?: boolean; accelerator?: string; action: () => void}): Promise<NativeItem>;
  predefined(item: "Copy" | "Cut" | "Paste" | "SelectAll" | "Undo" | "Redo" | "Minimize" | "Maximize" | "Fullscreen" | "CloseWindow" | "Hide" | "HideOthers" | "ShowAll" | "Services" | "Separator"): Promise<NativeResource>;
  submenu(id: string, text: string, items: NativeResource[]): Promise<NativeSubmenu>;
  menu(items: NativeResource[]): Promise<NativeBar>;
}

async function loadAdapter(): Promise<NativeMenuAdapter> {
  const { Menu, MenuItem, PredefinedMenuItem, Submenu } = await import("@tauri-apps/api/menu");
  return {
    item: (options) => MenuItem.new(options),
    predefined: (item) => PredefinedMenuItem.new({item}),
    submenu: (id, text, items) => Submenu.new({id, text, items: items as Parameters<typeof Submenu.new>[0]["items"]}),
    menu: (items) => Menu.new({items: items as NonNullable<Parameters<typeof Menu.new>[0]>["items"]}),
  };
}

/** Mount once in the main window. The disposer also owns an in-flight setup. */
export function mountNativeMenu(options: {
  blocked: () => boolean;
  onQuit: () => void;
  /** Injectable platform/API for focused tests; production uses the desktop API. */
  adapter?: () => Promise<NativeMenuAdapter>;
  platform?: NativePlatform;
}): () => void {
  if (!options.adapter && !isTauri()) return () => {};
  let disposed = false;
  let disposeRoot: (() => void) | undefined;
  const resources: NativeResource[] = [];
  const own = <T extends NativeResource>(resource: T): T => { resources.push(resource); return resource; };
  const [focusVersion, setFocusVersion] = createSignal(0);
  const refreshFocus = () => setFocusVersion((version) => version + 1);
  document.addEventListener("focusin", refreshFocus);
  document.addEventListener("focusout", refreshFocus);

  void (async () => {
    let installed = false;
    try {
      const api = await (options.adapter ?? loadAdapter)();
      if (disposed) return;
      const platform = options.platform ?? DESKTOP_PLATFORM;
      const mac = platform === "macos";
      const groups = nativeCommandGroups(options.blocked());
      const commands = new Map<string, NativeItem>();
      const submenus = new Map<string, NativeSubmenu>();
      const makeItem = async (id: string, text: string, action: () => void, accelerator?: string) =>
        own(await api.item({ id: `oc.${id}`, text, enabled: !options.blocked(), action, accelerator }));
      const about = await makeItem("about", i18n.t("nativeMenu.about"), () => {
        if (!options.blocked()) preferencesUi.openOn("help");
      });
      const settings = await makeItem("preferences", i18n.t("preferences.title"), () => {
        if (!options.blocked()) preferencesUi.openOn("display");
      }, "CmdOrCtrl+,");
      const quit = await makeItem("quit", i18n.t("nativeMenu.quit"), () => {
        if (!options.blocked()) options.onQuit();
      }, "CmdOrCtrl+Q");
      const appItems: NativeResource[] = [about, settings];
      if (mac) {
        appItems.push(own(await api.predefined("Separator")));
        for (const item of ["Services", "Hide", "HideOthers", "ShowAll"] as const) {
          appItems.push(own(await api.predefined(item)));
        }
      }
      appItems.push(own(await api.predefined("Separator")), quit);
      const app = own(await api.submenu("oc.app", "OpenCompose", appItems));
      const barItems: NativeResource[] = [app];
      for (const group of groups) {
        const items: NativeResource[] = [];
        for (const command of group.items) {
          const item = own(await api.item({
            id: `oc.command.${command.id}`, text: command.text, enabled: command.enabled,
            action: () => { void runNativeCommand(command.id, options.blocked()); },
          }));
          commands.set(command.id, item);
          items.push(item);
        }
        if (group.id === "edit") {
          const textItems: NativeResource[] = [];
          for (const item of ["Undo", "Redo", "Cut", "Copy", "Paste", "SelectAll"] as const) {
            textItems.push(own(await api.predefined(item)));
          }
          const text = own(await api.submenu("oc.text", i18n.t("nativeMenu.textEditing"), textItems));
          submenus.set("text", text);
          items.push(own(await api.predefined("Separator")), text);
        }
        const submenu = own(await api.submenu(`oc.${group.id}`, group.text, items));
        submenus.set(group.id, submenu);
        barItems.push(submenu);
      }
      const windowItems: NativeResource[] = [];
      for (const item of nativeWindowActions(platform)) {
        windowItems.push(own(await api.predefined(item)));
      }
      const windowMenu = own(await api.submenu("oc.window", i18n.t("nativeMenu.window"), windowItems));
      submenus.set("window", windowMenu);
      // Help belongs last; Window stays immediately before it.
      barItems.splice(barItems.length - 1, 0, windowMenu);
      const bar = own(await api.menu(barItems));
      if (disposed) return;
      const previousMenu = await bar.setAsAppMenu();
      installed = true;
      if (previousMenu) await previousMenu.close();
      if (mac) {
        await windowMenu.setAsWindowsMenuForNSApp?.().catch((error) => log.warn("native-menu", String(error)));
        await submenus.get("help")?.setAsHelpMenuForNSApp?.().catch((error) => log.warn("native-menu", String(error)));
      }
      if (disposed) return;
      // Serial updates keep an older asynchronous state from overwriting a newer
      // locale, selection or keymap. Only changed properties cross IPC.
      let pending = Promise.resolve();
      const previous = new Map<string, string>();
      for (const group of groups) {
        previous.set(`group.${group.id}`, group.text);
        for (const item of group.items) {
          previous.set(item.id, item.text);
          previous.set(`enabled.${item.id}`, String(item.enabled));
        }
      }
      createRoot((dispose) => {
        disposeRoot = dispose;
        createEffect(() => {
          focusVersion();
          const state = nativeCommandGroups(options.blocked());
          const blocked = options.blocked();
          const updates: (() => Promise<void>)[] = [];
          const text = (id: string, item: NativeItem | undefined, value: string) => {
            if (!item || previous.get(id) === value) return;
            previous.set(id, value); updates.push(() => item.setText(value));
          };
          const enabled = (id: string, item: NativeItem, value: boolean) => {
            if (previous.get(id) === String(value)) return;
            previous.set(id, String(value)); updates.push(() => item.setEnabled(value));
          };
          for (const group of state) {
            text(`group.${group.id}`, submenus.get(group.id), group.text);
            for (const command of group.items) {
              const item = commands.get(command.id)!;
              text(command.id, item, command.text);
              enabled(`enabled.${command.id}`, item, command.enabled);
            }
          }
          text("window", windowMenu, i18n.t("nativeMenu.window"));
          text("text", submenus.get("text"), i18n.t("nativeMenu.textEditing"));
          text("about", about, i18n.t("nativeMenu.about"));
          text("preferences", settings, i18n.t("preferences.title"));
          text("quit", quit, i18n.t("nativeMenu.quit"));
          for (const [id, item] of [["about", about], ["preferences", settings], ["quit", quit]] as const) {
            enabled(id, item, !blocked);
          }
          pending = pending.then(async () => {
            if (disposed) return;
            for (const update of updates) await update();
          }).catch((error) => log.warn("native-menu", String(error)));
        });
      });
    } catch (error) {
      log.warn("native-menu", String(error));
    } finally {
      if (disposed || !installed) for (const resource of resources.splice(0).reverse()) void resource.close().catch(() => {});
    }
  })();
  return () => {
    disposed = true;
    disposeRoot?.();
    document.removeEventListener("focusin", refreshFocus);
    document.removeEventListener("focusout", refreshFocus);
    for (const resource of resources.splice(0).reverse()) void resource.close().catch(() => {});
  };
}
