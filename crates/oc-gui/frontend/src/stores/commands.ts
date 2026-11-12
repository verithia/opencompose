/**
 * The command registry: one entry per user action, plus the key descriptor
 * every binding is compared through.
 *
 * A command owns its label, category, default binding and behaviour. The
 * palette, the shortcut editor, the toolbar list and the document key handler
 * all read this one table, so a rebinding cannot make the keyboard and the
 * palette disagree.
 */
import { helpUi } from "./helpUi";
import { createSignal } from "solid-js";
import { i18n, type I18nKey } from "./i18n";
import { store } from "./project";
import { panelStore } from "./panels";
import { recordStore } from "./record";
import { toggleAutomationWriteArm } from "./automationWrite";
import { clickStore } from "./click";
import { themeStore } from "./theme";
import { uiScaleStore } from "./uiScale";
import { openExportAudioDialog } from "./exportAudio";
import { openMidiExportDialog, openMidiImportDialog } from "./midiUi";
import { newProjectDialog, openAlignedStemsDialog, openFileDialog, saveAsFileDialog, saveFileDialog } from "./fileDialogs";
import { EXPORT_DIAGNOSTICS_EVENT, RENAME_SELECTION_EVENT } from "./appEvents";
import { overlayStore } from "./overlays";
import { uniqueName } from "../utils/naming";

// ── Binding descriptor ──────────────────────────────────────────────────────

/** A normalised key: `mod` is Cmd on macOS and Ctrl elsewhere. */
export interface Binding {
  key: string;
  mod: boolean;
  shift: boolean;
  alt: boolean;
}

/** Keys that are never a binding on their own. */
const MODIFIER_KEYS = new Set([
  "shift",
  "control",
  "meta",
  "alt",
  "altgraph",
  "capslock",
  "os",
  "contextmenu",
]);

const DISPLAY_KEYS: Record<string, string> = {
  space: "Space",
  escape: "Esc",
  tab: "Tab",
  enter: "Enter",
  delete: "Delete",
  backspace: "Backspace",
  arrowleft: "←",
  arrowright: "→",
  arrowup: "↑",
  arrowdown: "↓",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
};

const STORAGE_KEYS: Record<string, string> = {
  space: "Space",
  escape: "Escape",
  tab: "Tab",
  enter: "Enter",
  delete: "Delete",
  backspace: "Backspace",
  arrowleft: "ArrowLeft",
  arrowright: "ArrowRight",
  arrowup: "ArrowUp",
  arrowdown: "ArrowDown",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
};

/** Canonical key name for an event key or a binding token. */
export function normalizeKeyName(raw: string): string {
  const value = raw.trim();
  if (!value) return "";
  if (value === " " || /^space(bar)?$/i.test(value)) return "space";
  if (/^esc(ape)?$/i.test(value)) return "escape";
  if (value === "+") return "=";
  if (value === "_") return "-";
  if (/^[a-z]$/i.test(value)) return value.toLowerCase();
  return value.toLowerCase();
}

function storageKey(key: string): string {
  return STORAGE_KEYS[key] ?? (key.length === 1 ? key.toUpperCase() : key);
}

/** Display label for one key, platform-independent except for modifiers. */
export function displayKey(key: string): string {
  return DISPLAY_KEYS[key] ?? (key.length === 1 ? key.toUpperCase() : key);
}

/** Storage and canonical form: `Mod+Shift+Z`. */
export function bindingToString(binding: Binding): string {
  const parts: string[] = [];
  if (binding.mod) parts.push("Mod");
  if (binding.shift) parts.push("Shift");
  if (binding.alt) parts.push("Alt");
  parts.push(storageKey(binding.key));
  return parts.join("+");
}

/** Parse a stored binding; `null` for a string that names no usable key. */
export function parseBinding(value: string): Binding | null {
  const tokens = value.split("+").map((token) => token.trim()).filter(Boolean);
  if (tokens.length === 0) return null;
  let mod = false;
  let shift = false;
  let alt = false;
  const keyToken = tokens.pop()!;
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (lower === "mod" || lower === "cmd" || lower === "command" || lower === "ctrl" || lower === "control" || lower === "meta") mod = true;
    else if (lower === "shift") shift = true;
    else if (lower === "alt" || lower === "option") alt = true;
    else return null;
  }
  const key = normalizeKeyName(keyToken);
  if (!key || MODIFIER_KEYS.has(key)) return null;
  // A string that names only modifiers ("Mod", "Mod+Shift") binds nothing.
  if (key === "mod" || key === "cmd" || key === "command" || key === "ctrl" || key === "option") return null;
  return { key, mod, shift, alt };
}

/** Display form; the mod token names both platforms, like the menus do. */
export function formatBinding(binding: Binding): string {
  const parts: string[] = [];
  if (binding.mod) parts.push("Cmd/Ctrl");
  if (binding.shift) parts.push("Shift");
  if (binding.alt) parts.push("Alt");
  parts.push(displayKey(binding.key));
  return parts.join("+");
}

export function bindingsEqual(left: Binding, right: Binding): boolean {
  return left.key === right.key
    && left.mod === right.mod
    && left.shift === right.shift
    && left.alt === right.alt;
}

/** The binding an event describes, or null for a modifier-only press. */
export function bindingFromEvent(event: KeyboardEvent): Binding | null {
  let key = "";
  if (event.code === "Space" || event.key === " " || event.key === "Spacebar") key = "space";
  else key = normalizeKeyName(event.key);
  if (!key || MODIFIER_KEYS.has(key)) return null;
  // Punctuation and Space report a shift that the physical key does not need:
  // Mod+= and Mod++ both mean "scale up", so the binding stores neither.
  const shifted = key === "space" || key === "=" || key === "-" ? false : event.shiftKey;
  return {
    key,
    mod: event.metaKey || event.ctrlKey,
    shift: shifted,
    alt: event.altKey,
  };
}

// ── Command shape ───────────────────────────────────────────────────────────

export const COMMAND_CATEGORIES = ["transport", "file", "edit", "view", "track", "help"] as const;
export type CommandCategory = (typeof COMMAND_CATEGORIES)[number];

export const CATEGORY_LABEL_KEYS: Record<CommandCategory, I18nKey> = {
  transport: "command.category.transport",
  file: "command.category.file",
  edit: "command.category.edit",
  view: "command.category.view",
  track: "command.category.track",
  help: "command.category.help",
};

/** Which text fields keep a command from running. */
export type KeyGuard = "none" | "input" | "editable";

export interface Command {
  id: string;
  labelKey: I18nKey;
  category: CommandCategory;
  defaultBinding: Binding | null;
  /** Local handlers may return `false` to decline a press; module runs do not. */
  run: () => void | Promise<void> | boolean;
  enabled?: () => boolean;
  disabledReason?: () => string | null;
  /** Text-field guard; `editable` is the default for plain letter keys. */
  guard?: KeyGuard;
  /** A held key repeats; commands that toggle state opt out. */
  ignoreRepeat?: boolean;
  /** Letter commands accept a shifted press as well (Mod+Shift+N is New). */
  ignoreShift?: boolean;
  /** Commands that take any modifier state, such as Space for transport. */
  ignoreMod?: boolean;
  /** Some commands forward the event instead of consuming it (clip keys). */
  preventDefault?: boolean;
}

// ── Key context and local handlers ──────────────────────────────────────────

export interface KeyContext {
  inInput: boolean;
  inEditable: boolean;
  pianoRollActive: boolean;
  pianoRollGridFocused: boolean;
}

const NO_CONTEXT: KeyContext = {
  inInput: false,
  inEditable: false,
  pianoRollActive: false,
  pianoRollGridFocused: false,
};

let currentContext: KeyContext = NO_CONTEXT;
let currentEvent: KeyboardEvent | null = null;

interface LocalHandler {
  /** `false` means the surface does not own this press after all. */
  run: () => void | Promise<void> | boolean;
  enabled?: () => boolean;
  disabledReason?: () => string | null;
}

/** Commands a mounted component owns, such as a timeline-local split. */
const componentHandlers = new Map<string, LocalHandler>();
const [handlerVersion, setHandlerVersion] = createSignal(0);

/** Register or replace a command handler for as long as a component is mounted. */
export function registerCommand(
  id: string,
  run: () => void | Promise<void> | boolean,
  options?: { enabled?: () => boolean; disabledReason?: () => string | null },
): () => void {
  const handler: LocalHandler = { run, ...options };
  componentHandlers.set(id, handler);
  setHandlerVersion((version) => version + 1);
  return () => {
    if (componentHandlers.get(id) === handler) {
      componentHandlers.delete(id);
      setHandlerVersion((version) => version + 1);
    }
  };
}

export function hasCommandHandler(id: string): boolean {
  return componentHandlers.has(id);
}

/** Drop every component handler; the test harness resets between mounts. */
export function resetRegistryForTests(): void {
  componentHandlers.clear();
  setHandlerVersion((version) => version + 1);
}

/** Bumped when component handlers change; lists subscribe to re-render. */
export const commandHandlerVersion = handlerVersion;

// ── Helpers used by command runs ────────────────────────────────────────────

function focusTimeline() {
  const panel = document.querySelector<HTMLElement>(".timeline-panel");
  if (!panel) return;
  panel.focus({ preventScroll: true });
}

function forwardToPianoRoll(key: string, opts?: { ctrlKey?: boolean; metaKey?: boolean; shiftKey?: boolean }) {
  const grid = document.querySelector<HTMLElement>("[data-pianoroll-grid]");
  if (!grid) return;
  grid.focus({ preventScroll: true });
  const mod = opts?.ctrlKey || opts?.metaKey;
  grid.dispatchEvent(new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    ctrlKey: Boolean(mod),
    metaKey: Boolean(mod),
    shiftKey: Boolean(opts?.shiftKey),
  }));
}

function nextTrackName(): string {
  return uniqueName(
    `Track ${store.project.tracks.length + 1}`,
    store.project.tracks.map((track) => track.name),
  );
}

function nextPatternName(): string {
  return uniqueName("Pattern", store.project.patterns.map((pattern) => pattern.name));
}

function cycleTheme() {
  const flat = themeStore.catalog.flatMap((family) =>
    family.variants.map((variant) => `${family.id}/${variant.id}`),
  );
  const index = flat.indexOf(themeStore.id());
  themeStore.setTheme(flat[(index + 1) % flat.length]);
}

function needsHandler(id: string): (() => boolean) {
  return () => hasCommandHandler(id);
}

function handlerReason(id: string, key: I18nKey) {
  return () => (hasCommandHandler(id) ? null : i18n.t(key));
}

// ── The registry ────────────────────────────────────────────────────────────

export const COMMANDS: readonly Command[] = [
  // transport
  {
    id: "transport.playPause",
    labelKey: "command.transport.playPause",
    category: "transport",
    defaultBinding: { key: "space", mod: false, shift: false, alt: false },
    guard: "input",
    // A modified Space belongs to the platform or to another shortcut, so the
    // transport takes only the bare key.
    ignoreShift: false,
    ignoreMod: false,
    ignoreRepeat: true,
    run: () => {
      const command = panelStore.spaceCommand(
        panelStore.activeRegion(),
        panelStore.bottomMode(),
        store.selectedPresetName(),
        store.selectedPatternName(),
        store.auditionActive(),
      );
      if (command.kind === "audition-preset") void store.previewPreset(command.preset);
      else if (command.kind === "audition-pattern") void store.previewPattern(command.pattern);
      else void store.togglePlayPause();
    },
  },
  {
    id: "transport.stop",
    labelKey: "command.transport.stop",
    category: "transport",
    defaultBinding: null,
    run: () => void store.stop(),
  },
  {
    id: "transport.record",
    labelKey: "command.transport.record",
    category: "transport",
    defaultBinding: null,
    enabled: () => recordStore.armed() !== null || recordStore.recording(),
    disabledReason: () => i18n.t("command.recordNeedsArm"),
    run: () => {
      if (recordStore.recording()) void recordStore.stop();
      else void recordStore.start();
    },
  },
  {
    id: "transport.metronome",
    labelKey: "command.transport.metronome",
    category: "transport",
    defaultBinding: null,
    run: () => void clickStore.setEnabled(!clickStore.enabled()),
  },
  {
    id: "transport.loop",
    labelKey: "command.transport.loop",
    category: "transport",
    defaultBinding: { key: "l", mod: false, shift: false, alt: false },
    guard: "editable",
    ignoreShift: true,
    run: () => void store.toggleLoop(),
  },
  {
    id: "automation.write",
    labelKey: "command.automation.write",
    category: "transport",
    defaultBinding: null,
    ignoreRepeat: true,
    run: () => toggleAutomationWriteArm(),
  },

  // file
  {
    id: "file.new",
    labelKey: "command.file.new",
    category: "file",
    defaultBinding: { key: "n", mod: true, shift: false, alt: false },
    guard: "none",
    ignoreShift: true,
    run: () => void newProjectDialog(),
  },
  {
    id: "file.open",
    labelKey: "command.file.open",
    category: "file",
    defaultBinding: { key: "o", mod: true, shift: false, alt: false },
    guard: "none",
    ignoreShift: true,
    run: () => void openFileDialog(),
  },
  {
    id: "file.save",
    labelKey: "command.file.save",
    category: "file",
    defaultBinding: { key: "s", mod: true, shift: false, alt: false },
    guard: "none",
    run: () => void saveFileDialog(),
  },
  {
    id: "file.saveAs",
    labelKey: "command.file.saveAs",
    category: "file",
    defaultBinding: { key: "s", mod: true, shift: true, alt: false },
    guard: "none",
    run: () => void saveAsFileDialog(),
  },
  {
    id: "file.exportAudio",
    labelKey: "command.file.exportAudio",
    category: "file",
    defaultBinding: { key: "e", mod: true, shift: false, alt: false },
    guard: "none",
    run: () => openExportAudioDialog("mix"),
  },
  {
    id: "file.exportMidi",
    labelKey: "command.file.exportMidi",
    category: "file",
    defaultBinding: null,
    run: () => openMidiExportDialog(),
  },
  {
    id: "file.importMidi",
    labelKey: "command.file.importMidi",
    category: "file",
    defaultBinding: null,
    run: () => void openMidiImportDialog(),
  },
  {
    id: "file.importAlignedStems",
    labelKey: "command.file.importAlignedStems",
    category: "file",
    defaultBinding: null,
    run: () => void openAlignedStemsDialog(),
  },

  // edit
  {
    id: "edit.undo",
    labelKey: "command.edit.undo",
    category: "edit",
    defaultBinding: { key: "z", mod: true, shift: false, alt: false },
    guard: "none",
    run: () => void store.undo(),
  },
  {
    id: "edit.redo",
    labelKey: "command.edit.redo",
    category: "edit",
    defaultBinding: { key: "z", mod: true, shift: true, alt: false },
    guard: "none",
    run: () => void store.redo(),
  },
  {
    id: "edit.cut",
    labelKey: "command.edit.cut",
    category: "edit",
    defaultBinding: { key: "x", mod: true, shift: false, alt: false },
    guard: "editable",
    enabled: () => hasCommandHandler("edit.cut"),
    disabledReason: handlerReason("edit.cut", "command.needsTimeline"),
    run: () => {},
  },
  {
    id: "edit.copy",
    labelKey: "command.edit.copy",
    category: "edit",
    defaultBinding: { key: "c", mod: true, shift: false, alt: false },
    guard: "editable",
    enabled: () => hasCommandHandler("edit.copy"),
    disabledReason: handlerReason("edit.copy", "command.needsTimeline"),
    run: () => {},
  },
  {
    id: "edit.paste",
    labelKey: "command.edit.paste",
    category: "edit",
    defaultBinding: { key: "v", mod: true, shift: false, alt: false },
    guard: "editable",
    enabled: () => hasCommandHandler("edit.paste"),
    disabledReason: handlerReason("edit.paste", "command.needsTimeline"),
    run: () => {},
  },
  {
    id: "edit.delete",
    labelKey: "command.edit.delete",
    category: "edit",
    defaultBinding: { key: "delete", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive && !currentContext.pianoRollGridFocused) {
        forwardToPianoRoll("Delete");
      }
    },
  },
  {
    id: "edit.deleteBackspace",
    labelKey: "command.edit.deleteBackspace",
    category: "edit",
    defaultBinding: { key: "backspace", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive && !currentContext.pianoRollGridFocused) {
        forwardToPianoRoll("Backspace");
      }
    },
  },
  {
    id: "edit.duplicate",
    labelKey: "command.edit.duplicate",
    category: "edit",
    defaultBinding: { key: "d", mod: true, shift: false, alt: false },
    guard: "input",
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive) {
        if (!currentContext.pianoRollGridFocused) forwardToPianoRoll("d", { ctrlKey: true });
        return;
      }
      // Nothing in the arrangement claimed the press, so the selected track is
      // what the operator means: duplicate it.
      const index = store.selectedTrackIndex();
      if (index !== null) void store.duplicateTrack(index);
    },
  },
  {
    id: "edit.split",
    labelKey: "command.edit.split",
    category: "edit",
    defaultBinding: null,
    enabled: needsHandler("edit.split"),
    disabledReason: handlerReason("edit.split", "command.needsTimeline"),
    run: () => componentHandlers.get("edit.split")?.run(),
  },
  {
    id: "edit.selectAll",
    labelKey: "command.edit.selectAll",
    category: "edit",
    defaultBinding: { key: "a", mod: true, shift: false, alt: false },
    guard: "none",
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive) forwardToPianoRoll("a", { ctrlKey: true });
    },
  },
  {
    id: "edit.nudgeLeft",
    labelKey: "command.edit.nudgeLeft",
    category: "edit",
    defaultBinding: { key: "arrowleft", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive && !currentContext.pianoRollGridFocused) {
        forwardToPianoRoll("ArrowLeft", { shiftKey: Boolean(currentEvent?.shiftKey) });
      }
    },
  },
  {
    id: "edit.nudgeRight",
    labelKey: "command.edit.nudgeRight",
    category: "edit",
    defaultBinding: { key: "arrowright", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    preventDefault: false,
    run: () => {
      if (currentContext.pianoRollActive && !currentContext.pianoRollGridFocused) {
        forwardToPianoRoll("ArrowRight", { shiftKey: Boolean(currentEvent?.shiftKey) });
      }
    },
  },
  {
    id: "edit.addMarker",
    labelKey: "command.edit.addMarker",
    category: "edit",
    defaultBinding: { key: "t", mod: false, shift: false, alt: true },
    guard: "editable",
    run: () => void store.addMarkerAtPlayhead(),
  },
  {
    id: "edit.groupClips",
    labelKey: "command.edit.groupClips",
    category: "edit",
    defaultBinding: { key: "g", mod: true, shift: false, alt: false },
    guard: "editable",
    enabled: () => hasCommandHandler("edit.groupClips"),
    disabledReason: handlerReason("edit.groupClips", "command.needsTimeline"),
    run: () => {},
  },
  {
    id: "edit.ungroupClips",
    labelKey: "command.edit.ungroupClips",
    category: "edit",
    defaultBinding: { key: "g", mod: true, shift: true, alt: false },
    guard: "editable",
    enabled: () => hasCommandHandler("edit.ungroupClips"),
    disabledReason: handlerReason("edit.ungroupClips", "command.needsTimeline"),
    run: () => {},
  },

  // view
  {
    id: "view.toggleMixer",
    labelKey: "command.view.toggleMixer",
    category: "view",
    defaultBinding: { key: "m", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => panelStore.toggleBottom("mixer"),
  },
  {
    id: "view.togglePianoRoll",
    labelKey: "command.view.togglePianoRoll",
    category: "view",
    defaultBinding: { key: "r", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => {
      if (recordStore.armed() !== null || recordStore.recording()) {
        if (recordStore.recording()) void recordStore.stop();
        else void recordStore.start();
        return;
      }
      panelStore.toggleBottom("pianoroll");
    },
  },
  {
    id: "view.toggleClips",
    labelKey: "command.view.toggleClips",
    category: "view",
    defaultBinding: { key: "c", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => panelStore.toggleBottom("clips"),
  },
  {
    id: "view.togglePresets",
    labelKey: "command.view.togglePresets",
    category: "view",
    defaultBinding: { key: "p", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => panelStore.toggleBottom("presets"),
  },
  {
    id: "view.toggleDrumKit",
    labelKey: "command.view.toggleDrumKit",
    category: "view",
    defaultBinding: { key: "k", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => panelStore.toggleBottom("drumpads"),
  },
  {
    id: "view.toggleSamples",
    labelKey: "command.view.toggleSamples",
    category: "view",
    // No default binding: the free keys are taken by editing actions.
    defaultBinding: null,
    run: () => panelStore.toggleBottom("samples"),
  },
  {
    id: "view.toggleDsl",
    labelKey: "command.view.toggleDsl",
    category: "view",
    defaultBinding: { key: "d", mod: false, shift: false, alt: false },
    guard: "input",
    ignoreShift: true,
    run: () => panelStore.toggleSidebar("dsl"),
  },
  {
    id: "view.toggleAi",
    labelKey: "command.view.toggleAi",
    category: "view",
    defaultBinding: null,
    run: () => panelStore.toggleAi(),
  },
  {
    id: "view.toggleBrowser",
    labelKey: "command.view.toggleBrowser",
    category: "view",
    defaultBinding: null,
    run: () => panelStore.toggleSidebar("plugins"),
  },
  {
    id: "view.toggleTimeline",
    labelKey: "command.view.toggleTimeline",
    category: "view",
    defaultBinding: null,
    run: () => {
      panelStore.closeBottom();
      focusTimeline();
    },
  },
  {
    id: "view.zoomIn",
    labelKey: "command.view.zoomIn",
    category: "view",
    defaultBinding: null,
    enabled: needsHandler("view.zoomIn"),
    disabledReason: handlerReason("view.zoomIn", "command.needsTimeline"),
    run: () => componentHandlers.get("view.zoomIn")?.run(),
  },
  {
    id: "view.zoomOut",
    labelKey: "command.view.zoomOut",
    category: "view",
    defaultBinding: null,
    enabled: needsHandler("view.zoomOut"),
    disabledReason: handlerReason("view.zoomOut", "command.needsTimeline"),
    run: () => componentHandlers.get("view.zoomOut")?.run(),
  },
  {
    id: "view.zoomToFit",
    labelKey: "command.view.zoomToFit",
    category: "view",
    defaultBinding: null,
    enabled: needsHandler("view.zoomToFit"),
    disabledReason: handlerReason("view.zoomToFit", "command.needsTimeline"),
    run: () => componentHandlers.get("view.zoomToFit")?.run(),
  },
  {
    id: "view.followPlayhead",
    labelKey: "command.view.followPlayhead",
    category: "view",
    defaultBinding: null,
    enabled: needsHandler("view.followPlayhead"),
    disabledReason: handlerReason("view.followPlayhead", "command.needsTimeline"),
    run: () => componentHandlers.get("view.followPlayhead")?.run(),
  },
  {
    id: "view.toggleTheme",
    labelKey: "command.view.toggleTheme",
    category: "view",
    defaultBinding: null,
    run: () => cycleTheme(),
  },
  {
    id: "view.uiScaleUp",
    labelKey: "command.view.uiScaleUp",
    category: "view",
    defaultBinding: { key: "=", mod: true, shift: false, alt: false },
    // A view chord, not typing: the scale commands stay live while the
    // composer or any other text field has focus.
    guard: "none",
    run: () => uiScaleStore.adjust(1),
  },
  {
    id: "view.uiScaleDown",
    labelKey: "command.view.uiScaleDown",
    category: "view",
    defaultBinding: { key: "-", mod: true, shift: false, alt: false },
    // A view chord, not typing: the scale commands stay live while the
    // composer or any other text field has focus.
    guard: "none",
    run: () => uiScaleStore.adjust(-1),
  },
  {
    id: "view.uiScaleReset",
    labelKey: "command.view.uiScaleReset",
    category: "view",
    defaultBinding: { key: "0", mod: true, shift: false, alt: false },
    // A view chord, not typing: the scale commands stay live while the
    // composer or any other text field has focus.
    guard: "none",
    run: () => uiScaleStore.reset(),
  },
  {
    id: "view.localeEn",
    labelKey: "command.view.localeEn",
    category: "view",
    defaultBinding: null,
    run: () => i18n.setLocale("en"),
  },
  {
    id: "view.localeZhHans",
    labelKey: "command.view.localeZhHans",
    category: "view",
    defaultBinding: null,
    run: () => i18n.setLocale("zh-Hans"),
  },
  {
    id: "view.localeZhHant",
    labelKey: "command.view.localeZhHant",
    category: "view",
    defaultBinding: null,
    run: () => i18n.setLocale("zh-Hant"),
  },
  {
    id: "view.localeJa",
    labelKey: "command.view.localeJa",
    category: "view",
    defaultBinding: null,
    run: () => i18n.setLocale("ja"),
  },
  {
    id: "view.localeEs",
    labelKey: "command.view.localeEs",
    category: "view",
    defaultBinding: null,
    run: () => i18n.setLocale("es"),
  },

  // track and pattern
  {
    id: "edit.rename",
    labelKey: "command.edit.rename",
    category: "edit",
    defaultBinding: { key: "f2", mod: false, shift: false, alt: false },
    guard: "input",
    run: () => {
      window.dispatchEvent(new Event(RENAME_SELECTION_EVENT));
    },
  },
  {
    id: "track.add",
    labelKey: "command.track.add",
    category: "track",
    defaultBinding: null,
    run: () => void store.addTrack(nextTrackName(), "piano", "acoustic_grand"),
  },
  {
    id: "track.duplicate",
    labelKey: "command.track.duplicate",
    category: "track",
    defaultBinding: null,
    enabled: () => store.selectedTrackIndex() !== null,
    disabledReason: () => i18n.t("command.needsSelection"),
    run: () => {
      const index = store.selectedTrackIndex();
      if (index !== null) void store.duplicateTrack(index);
    },
  },
  {
    id: "track.remove",
    labelKey: "command.track.remove",
    category: "track",
    defaultBinding: null,
    enabled: () => store.selectedTrackIndex() !== null,
    disabledReason: () => i18n.t("command.needsSelection"),
    run: () => {
      const index = store.selectedTrackIndex();
      if (index !== null) void store.removeTrack(index);
    },
  },
  {
    id: "pattern.add",
    labelKey: "command.pattern.add",
    category: "track",
    defaultBinding: null,
    run: () => void store.addPattern(nextPatternName(), "piano", "piano"),
  },
  {
    id: "pattern.quantise",
    labelKey: "command.pattern.quantise",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.quantise"),
    disabledReason: handlerReason("pattern.quantise", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.quantise")?.run(),
  },
  {
    id: "pattern.humanise",
    labelKey: "command.pattern.humanise",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.humanise"),
    disabledReason: handlerReason("pattern.humanise", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.humanise")?.run(),
  },
  {
    id: "pattern.legato",
    labelKey: "command.pattern.legato",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.legato"),
    disabledReason: handlerReason("pattern.legato", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.legato")?.run(),
  },
  {
    id: "pattern.ramp",
    labelKey: "command.pattern.ramp",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.ramp"),
    disabledReason: handlerReason("pattern.ramp", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.ramp")?.run(),
  },
  {
    id: "pattern.scale",
    labelKey: "command.pattern.scale",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.scale"),
    disabledReason: handlerReason("pattern.scale", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.scale")?.run(),
  },
  {
    id: "pattern.compress",
    labelKey: "command.pattern.compress",
    category: "track",
    defaultBinding: null,
    enabled: needsHandler("pattern.compress"),
    disabledReason: handlerReason("pattern.compress", "command.needsPianoRoll"),
    run: () => componentHandlers.get("pattern.compress")?.run(),
  },

  // help
  {
    id: "help.manual",
    labelKey: "command.help.manual",
    category: "help",
    defaultBinding: null,
    run: () => helpUi.openManual(),
  },
  {
    id: "help.diagnostics",
    labelKey: "command.help.diagnostics",
    category: "help",
    defaultBinding: { key: "d", mod: true, shift: true, alt: false },
    guard: "none",
    run: () => {
      window.dispatchEvent(new Event(EXPORT_DIAGNOSTICS_EVENT));
    },
  },
  {
    id: "help.shortcuts",
    labelKey: "command.help.shortcuts",
    category: "help",
    defaultBinding: null,
    run: () => overlayStore.openShortcutEditor(),
  },
  {
    id: "command.palette.open",
    labelKey: "command.palette.open",
    category: "help",
    defaultBinding: { key: "p", mod: true, shift: true, alt: false },
    guard: "none",
    ignoreShift: true,
    run: () => overlayStore.openPalette(),
  },
];

const byId = new Map(COMMANDS.map((command) => [command.id, command]));

export function commandById(id: string): Command | undefined {
  return byId.get(id);
}

export function commandLabel(command: Command): string {
  return i18n.t(command.labelKey);
}

export function categoryLabel(category: CommandCategory): string {
  return i18n.t(CATEGORY_LABEL_KEYS[category]);
}

export function commandEnabled(command: Command): boolean {
  const local = componentHandlers.get(command.id);
  if (local) return local.enabled ? local.enabled() : true;
  return command.enabled ? command.enabled() : true;
}

export function commandDisabledReason(command: Command): string | null {
  const local = componentHandlers.get(command.id);
  if (local) {
    if (local.enabled && !local.enabled()) {
      return local.disabledReason?.() ?? command.disabledReason?.() ?? i18n.t("command.needsPianoRoll");
    }
    return null;
  }
  if (command.enabled && !command.enabled()) {
    return command.disabledReason?.() ?? i18n.t("command.needsPianoRoll");
  }
  return null;
}

/** Run a command by id; returns false when it is unknown or disabled. */
export async function runCommand(id: string): Promise<boolean> {
  const command = byId.get(id);
  if (!command || !commandEnabled(command)) return false;
  try {
    const local = componentHandlers.get(id);
    if (local) {
      const handled = await local.run();
      // A surface that declined (no selection, no clipboard) falls through to
      // the command's own run, which forwards to the surface that does own it.
      if (handled !== false) return true;
    }
    await command.run();
    return true;
  } catch (error) {
    store.setError(String(error));
    return false;
  }
}

/** Whether a command accepts a press in this text-field context. */
export function commandAllowsContext(command: Command, context: KeyContext): boolean {
  if (command.guard === "editable" && context.inEditable) return false;
  if (command.guard === "input" && context.inInput) return false;
  return true;
}

/** Hand the current press to command runs, which read its modifiers when forwarding. */
export function setDispatchContext(context: KeyContext, event: KeyboardEvent | null): void {
  currentContext = context;
  currentEvent = event;
}
