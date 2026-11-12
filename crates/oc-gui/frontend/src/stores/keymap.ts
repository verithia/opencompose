/**
 * User key overrides and binding resolution.
 *
 * Overrides are stored under one stable key so a build can read a keymap it
 * wrote months ago: `{ version: 1, bindings: { [commandId]: "Mod+Shift+Z" } }`.
 * Resolution is a pure function of a binding plus the command table, so the
 * palette, the editor and the document handler all agree on what a key runs.
 */
import { createSignal } from "solid-js";
import { i18n } from "./i18n";
import { panelStore } from "./panels";
import {
  COMMANDS,
  bindingFromEvent,
  bindingToString,
  bindingsEqual,
  commandAllowsContext,
  commandById,
  commandEnabled,
  commandLabel,
  formatBinding,
  parseBinding,
  runCommand,
  setDispatchContext,
  type Binding,
  type Command,
  type KeyContext,
} from "./commands";

export const KEYMAP_STORAGE_KEY = "opencompose.keymap";
export const KEYMAP_VERSION = 1;

/** Keys the app keeps for itself; a user binding never takes them. */
const RESERVED_KEYS = new Set(["escape", "tab"]);

export type BindResult =
  | { ok: true; binding: Binding }
  | { ok: false; reason: "reserved" | "conflict" | "unknown"; message: string; conflictId?: string };

/** Parse the stored payload; unreadable entries are dropped, never thrown. */
export function readStoredBindings(raw: string | null): Record<string, string> {
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const payload = parsed as { version?: unknown; bindings?: unknown };
    if (payload.version !== KEYMAP_VERSION) return {};
    if (typeof payload.bindings !== "object" || payload.bindings === null) return {};
    const bindings: Record<string, string> = {};
    for (const [id, value] of Object.entries(payload.bindings as Record<string, unknown>)) {
      if (typeof value !== "string") continue;
      const binding = parseBinding(value);
      if (!binding || !commandById(id) || isReservedBinding(binding)) continue;
      bindings[id] = bindingToString(binding);
    }
    return bindings;
  } catch {
    return {};
  }
}

function readInitialBindings(): Record<string, string> {
  if (typeof window === "undefined") return {};
  try {
    return readStoredBindings(window.localStorage.getItem(KEYMAP_STORAGE_KEY));
  } catch {
    return {};
  }
}

const [overrides, setOverrides] = createSignal<Record<string, string>>(readInitialBindings());

function persist(bindings: Record<string, string>) {
  try {
    window.localStorage.setItem(
      KEYMAP_STORAGE_KEY,
      JSON.stringify({ version: KEYMAP_VERSION, bindings }),
    );
  } catch {
    // Storage unavailable: the overrides still apply to this session.
  }
}

/** Every custom binding, keyed by command id. */
export const keymapOverrides = overrides;

/** The binding a command currently answers to, or null when unbound. */
export function effectiveBinding(
  commandId: string,
  bindings: Record<string, string> = overrides(),
): Binding | null {
  const command = commandById(commandId);
  if (!command) return null;
  const stored = bindings[commandId];
  if (stored) {
    const parsed = parseBinding(stored);
    if (parsed) return parsed;
  }
  return command.defaultBinding;
}

export function effectiveBindingString(
  commandId: string,
  bindings: Record<string, string> = overrides(),
): string | null {
  const binding = effectiveBinding(commandId, bindings);
  return binding ? bindingToString(binding) : null;
}

export function commandBindingLabel(commandId: string): string | null {
  const binding = effectiveBinding(commandId);
  return binding ? formatBinding(binding) : null;
}

/** Whether a binding is fixed by the app and cannot be assigned to a command. */
export function isReservedBinding(binding: Binding): boolean {
  if (RESERVED_KEYS.has(binding.key)) return true;
  // The native application menu owns these key equivalents on desktop.
  if (binding.mod && !binding.shift && !binding.alt && ["q", ","].includes(binding.key)) return true;
  if (binding.mod && binding.shift && binding.key === "p") return true;
  return false;
}

/** The command already using `binding`, ignoring `commandId` itself. */
export function bindingConflict(
  binding: Binding,
  commandId: string,
  commands: readonly Command[] = COMMANDS,
  bindings: Record<string, string> = overrides(),
): Command | null {
  for (const command of commands) {
    if (command.id === commandId) continue;
    const current = effectiveBinding(command.id, bindings);
    if (current && bindingsEqual(current, binding)) return command;
  }
  return null;
}

/** The command a key press runs, exactly then with shifted letters folded in. */
export function resolveBinding(
  binding: Binding,
  commands: readonly Command[] = COMMANDS,
  bindings: Record<string, string> = overrides(),
): Command | null {
  for (const command of commands) {
    const current = effectiveBinding(command.id, bindings);
    if (current && bindingsEqual(current, binding)) return command;
  }
  if (binding.shift) {
    // A letter shortcut keeps working with Shift held unless the command binds
    // the shifted form itself (Mod+Shift+Z is redo, Mod+Z is undo).
    const unshifted: Binding = { ...binding, shift: false };
    for (const command of commands) {
      if (!command.ignoreShift) continue;
      const current = effectiveBinding(command.id, bindings);
      if (current && bindingsEqual(current, unshifted)) return command;
    }
  }
  if (binding.mod) {
    // A command that opts out of the modifier check still matches the bare key.
    const bare: Binding = { ...binding, mod: false };
    for (const command of commands) {
      if (!command.ignoreMod) continue;
      const current = effectiveBinding(command.id, bindings);
      if (current && bindingsEqual(current, bare)) return command;
    }
  }
  return null;
}

export function setBinding(commandId: string, binding: Binding): BindResult {
  const command = commandById(commandId);
  if (!command) {
    return { ok: false, reason: "unknown", message: i18n.t("keymap.conflict", {
      binding: formatBinding(binding),
      command: commandId,
    }) };
  }
  if (isReservedBinding(binding)) {
    return {
      ok: false,
      reason: "reserved",
      message: i18n.t("keymap.reserved", { binding: formatBinding(binding) }),
    };
  }
  const other = bindingConflict(binding, commandId);
  if (other) {
    return {
      ok: false,
      reason: "conflict",
      message: i18n.t("keymap.conflict", {
        binding: formatBinding(binding),
        command: commandLabel(other),
      }),
      conflictId: other.id,
    };
  }
  const next = { ...overrides(), [commandId]: bindingToString(binding) };
  setOverrides(next);
  persist(next);
  return { ok: true, binding };
}

/** Drop one override; returns the default binding that comes back. */
export function resetBinding(commandId: string): Binding | null {
  const next = { ...overrides() };
  delete next[commandId];
  setOverrides(next);
  persist(next);
  return commandById(commandId)?.defaultBinding ?? null;
}

/** Drop every override; the defaults return for all commands. */
export function resetAllBindings(): void {
  setOverrides({});
  persist({});
}

/** Re-read localStorage, for a settings import or a test that seeds storage. */
export function reloadBindings(): void {
  setOverrides(readInitialBindings());
}

/** Native text undo belongs to text entry, rather than every focusable input. */
export function isTextEditingTarget(target: Element | null): boolean {
  if (!target) return false;
  if (target instanceof HTMLTextAreaElement) return true;
  if (target instanceof HTMLInputElement) {
    return ["text", "search", "email", "url", "tel", "password", "number"].includes(target.type);
  }
  if (!(target instanceof HTMLElement)) return false;
  const editable = target.closest("[contenteditable]");
  return editable ? ["", "true", "plaintext-only"].includes(editable.getAttribute("contenteditable") ?? "")
    : target.isContentEditable === true;
}

export function keyContext(): KeyContext {
  const active = document.activeElement as HTMLElement | null;
  const tag = active?.tagName?.toLowerCase();
  const inInput = tag === "input" || tag === "textarea" || tag === "select";
  const bottom = panelStore.bottomMode();
  return {
    inInput,
    inEditable: inInput || active?.isContentEditable === true,
    pianoRollActive: bottom === "pianoroll" || bottom === "detail",
    pianoRollGridFocused: active?.hasAttribute("data-pianoroll-grid") ?? false,
  };
}

/**
 * Dispatch one document key press through the registry. Returns true when a
 * command owns the key, so the caller keeps the browser default away from it.
 */
export function dispatchKeydown(event: KeyboardEvent): boolean {
  if (event.defaultPrevented) return false;
  const binding = bindingFromEvent(event);
  if (!binding) return false;
  // Space on a focused control is that control's press, not the transport's:
  // a keyboard user activating a button must not also start playback.
  if (binding.key === "space" && !binding.mod && !binding.alt && !binding.shift) {
    const active = document.activeElement as HTMLElement | null;
    if (
      active?.closest(
        "button, a[href], [role='button'], [role='menuitem'], [role='checkbox'], [role='switch'], [role='slider'], [role='combobox']",
      )
    ) {
      return false;
    }
  }
  // A text selection belongs to the webview: Mod+C / Mod+X must copy it, not
  // run the timeline's clip clipboard over it. The panel's transcript is the
  // main place this matters.
  if (
    binding.mod &&
    !binding.shift &&
    !binding.alt &&
    (binding.key === "c" || binding.key === "x") &&
    !keyContext().inEditable &&
    (document.getSelection()?.toString().trim().length ?? 0) > 0
  ) {
    // Copy the selection here rather than relying on the platform's key
    // equivalent: a webview without a matching menu item never receives it.
    if (binding.key === "c") {
      let copied = false;
      try {
        copied = document.execCommand?.("copy") ?? false;
      } catch {
        copied = false;
      }
      if (copied) {
        event.preventDefault();
        return true;
      }
    }
    return false;
  }
  const context = keyContext();
  const command = resolveBinding(binding);
  if (!command || !commandAllowsContext(command, context) || !commandEnabled(command)) return false;
  setDispatchContext(context, event);
  if (event.repeat && command.ignoreRepeat) {
    event.preventDefault();
    return true;
  }
  void runCommand(command.id);
  if (command.preventDefault !== false) event.preventDefault();
  return true;
}
