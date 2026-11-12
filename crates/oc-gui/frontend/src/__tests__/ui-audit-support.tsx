/**
 * Shared plumbing for the UI control audit: mount a surface on a fake backend,
 * enumerate every interactive control, and decide whether an interaction had an
 * observable effect.
 *
 * The audit walks the UI inventory defined by the test suite.
 * A control passes when it either changes something the user can see (DOM,
 * store, navigation) or reaches the backend; a disabled control passes only when
 * it says why it is disabled.
 */
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import type { JSX } from "solid-js";
import { invokeMock, makeSnapshot, openDialogMock, saveDialogMock, askDialogMock, TRANSPORT_STOPPED } from "./setup";
import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import { midiDialogStore } from "../stores/midiUi";
import { exportAudioStore } from "../stores/exportAudio";
import { bounceStore } from "../stores/bounce";
import { workspaceStore } from "../stores/workspace";
import { themeStore } from "../stores/theme";
import { gridDivision } from "../stores/grid";
import { i18n } from "../stores/i18n";
import { preferencesUi } from "../stores/preferencesUi";
import { makeAudioClip, makeTrack, makePattern } from "./setup";

/**
 * A project with everything the timeline can show: two tracks with clips and
 * phrases, a drum pattern, an audio clip and a bus send.
 */
export function makeTimelineProject() {
  const piano = makeTrack("Piano", 0);
  piano.preset = "Piano 1";
  piano.clips = [{
    pattern: "hook",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    },
  }];
  const audio = makeAudioClip({ track: "Drums" });
  const snapshot = makeSnapshot({
    tracks: [piano, makeTrack("Drums", 1)],
    patterns: [makePattern("hook"), makePattern("beat")],
    buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
    properties: {
      tempo: 128,
      time_signature: { numerator: 4, denominator: 4 },
      key: null,
      swing: null,
      tempo_map: [],
      time_sig_map: [],
      key_map: [],
      audio_clips: [audio],
    } as never,
  });
  snapshot.dsl_source = [
    '@project "Test Project"',
    "  tempo: 128",
    "  time_signature: 4/4",
    "",
    '@pattern hook | instrument: piano/acoustic_grand',
    "  C4 q mf",
    "",
    '@pattern beat | instrument: drums/acoustic_kit',
    "  kick on [1:1]",
    "",
    '@track Piano | instrument: piano/acoustic_grand',
    "  clip: hook [1:1:0 - 3:1:0]",
  ].join("\n");
  return snapshot;
}

/** Backend replies the audit does not care about; every command is at least answered. */
export function installAuditBackend(
  extra: (command: string, args: Record<string, unknown>) => unknown = () => undefined,
) {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    const handled = extra(command, typed);
    if (handled !== undefined) return handled;
    switch (command) {
      case "scan_plugins": return [];
      case "plugin_catalog": return { platform: "macos-aarch64", entries: [], installed: [] };
      case "take_startup_project": return null;
      case "get_transport_state": return TRANSPORT_STOPPED();
      case "transport_play": return { ...TRANSPORT_STOPPED(), playing: true, position_beats: 0 };
      case "transport_pause":
      case "transport_stop": return TRANSPORT_STOPPED();
      case "transport_seek":
        return { ...TRANSPORT_STOPPED(), position_beats: Number(typed.beat ?? 0) };
      case "get_undo_state":
        return { can_undo: true, can_redo: true, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      case "get_undo_history": return [];
      case "get_midi_config":
        return { input: null, monitor: false, active: false, audition_active: false, error: null };
      case "list_midi_ports": return { supported: true, inputs: [], outputs: [], error: null };
      case "get_midi_activity":
        return { running: false, events: 0, last: null, error: null, audition_active: false };
      case "begin_undo_gesture":
      case "end_undo_gesture": return undefined;
      case "soundfonts_list": return { dir: "/tmp/soundfonts", items: [] };
      case "list_sampler_assets": return [];
      case "list_library_patches": return { patches: [], categories: [] };
      case "list_patch_preferences": return { favorites: [] };
      case "audio_clip_peaks": return "";
      case "export_capabilities": return { mp3: true };
      case "preview_block": return [];
      case "preview_track": return null;
      case "preview_preset": return null;
      case "preview_pattern": return [];
      case "project_compatibility_report":
        return { compatible: true, installed_plugin_count: 0, resolved_plugins: [], issues: [] };
      default: return makeSnapshot();
    }
  });
  openDialogMock.mockReset();
  openDialogMock.mockResolvedValue(null);
  saveDialogMock.mockReset();
  saveDialogMock.mockResolvedValue(null);
  askDialogMock.mockReset();
  askDialogMock.mockResolvedValue(true);
}

/** Call once per test before mounting: clears the module-level store. */
export async function loadAuditProject(snapshot = makeSnapshot()) {
  installAuditBackend((command) => (command === "get_project" ? snapshot : undefined));
  // Module-level UI state survives a remount, so every iteration starts from the
  // same theme, language and panel layout; otherwise a language or theme click
  // would renumber the controls the next control is matched against.
  i18n.setLocale("en");
  themeStore.setTheme("default", "dark");
  panelStore.closeBottom();
  if (panelStore.aiOpen()) panelStore.toggleAi();
  midiDialogStore.closeImport();
  midiDialogStore.closeExport();
  workspaceStore.closeChooser();
  await store.initProject();
  // Transport position survives a project load; start every control run at 0.
  await store.stop();
  // Selections and block state are separate signals that survive a reload.
  store.setSelectedTrackIndex(null);
  store.setSelectedPresetName(null);
  store.setSelectedPatternName(null);
  store.setEditorSelection(null);
  store.setBlockSelection([]);
  store.setError(null);
  invokeMock.mockClear();
  openDialogMock.mockClear();
  saveDialogMock.mockClear();
  askDialogMock.mockClear();
}

/** Mount a surface and let its onMount effects settle. */
export async function mountSurface(surface: () => JSX.Element) {
  const view = render(surface);
  await settle();
  return view;
}

/** One macrotask, so promises resolved inside handlers run before assertions. */
export async function settle(rounds = 3) {
  for (let i = 0; i < rounds; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

export interface ControlRef {
  element: HTMLElement;
  /** Accessible name, visible text, or placeholder, in that order. */
  label: string;
  tag: string;
  role: string;
}

/** Roles that name a region rather than a control. */
const CONTAINER_ROLES = new Set(["group", "application", "region", "presentation", "img", "list", "toolbar"]);

const INTERACTIVE = [
  "button",
  "select",
  "input",
  "textarea",
  "[role='button']",
  "[role='menuitem']",
  "[role='option']",
  "[role='slider']",
  "[role='switch']",
  "[role='tab']",
  "[role='checkbox']",
  "[tabindex='0']",
].join(",");

/** Every control a pointer or keyboard can reach inside `root`. */
export function interactiveControls(root: ParentNode = document.body): ControlRef[] {
  return [...root.querySelectorAll<HTMLElement>(INTERACTIVE)]
    .filter((element) => !element.hasAttribute("disabled") && element.getAttribute("aria-disabled") !== "true")
    // A focusable container is a keyboard surface, not a control; clicking it is
    // a no-op by design and its text is the whole panel.
    .filter((element) => !CONTAINER_ROLES.has(element.getAttribute("role") ?? ""))
    .filter((element) => element.getAttribute("role") !== null
      || element.hasAttribute("aria-label")
      || !element.hasAttribute("tabindex"))
    .map((element) => ({
      element,
      label: labelOf(element),
      tag: element.tagName.toLowerCase(),
      role: element.getAttribute("role") ?? element.tagName.toLowerCase(),
    }));
}

export function labelOf(element: HTMLElement): string {
  const aria = element.getAttribute("aria-label");
  if (aria) return aria;
  const labelled = element.getAttribute("aria-labelledby");
  if (labelled) {
    const text = labelled
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" ")
      .trim();
    if (text) return text;
  }
  const text = (element.textContent ?? "").replace(/\s+/g, " ").trim();
  if (text) return text;
  const placeholder = element.getAttribute("placeholder");
  if (placeholder) return placeholder;
  const title = element.getAttribute("title");
  if (title) return title;
  return `<${element.tagName.toLowerCase()} class="${element.className}">`;
}

/** A cheap fingerprint of everything a click could plausibly change. */
export function observableState(exclude?: HTMLElement) {
  return {
    invokes: invokeMock.mock.calls.length,
    dialogs: openDialogMock.mock.calls.length + saveDialogMock.mock.calls.length + askDialogMock.mock.calls.length,
    html: document.body.innerHTML,
    text: document.body.textContent ?? "",
    dialogsOpen: document.querySelectorAll('[role="dialog"]').length,
    // A field that only feeds a dependent control still changes the surface:
    // the send target enables Add send, the bus name enables Add bus.
    controls: [...document.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>(
      "button, input, select, textarea",
    )].filter((element) => element !== exclude).map((element) => [
      element.tagName,
      element.disabled ? "1" : "0",
      element.getAttribute("aria-pressed") ?? "",
      element.getAttribute("aria-expanded") ?? "",
      element.getAttribute("aria-checked") ?? "",
      element.value ?? "",
      element.getAttribute("class") ?? "",
    ].join(":")).join("|"),
    // Surfaces live in several places: a toolbar button may open a panel, a
    // dialog store, or the workspace chooser without touching its own DOM.
    layout: JSON.stringify([
      panelStore.aiOpen(),
      panelStore.sidebarOpen(),
      panelStore.sidebarTab(),
      panelStore.bottomPanel(),
      midiDialogStore.importRequest(),
      midiDialogStore.exportRequest(),
      exportAudioStore.dialogRequest(),
      workspaceStore.chooserOpen(),
      store.error(),
      store.filePath(),
      store.project.name,
      themeStore.family().id,
      themeStore.variant().id,
      i18n.locale(),
      gridDivision(),
      document.documentElement.getAttribute("class"),
      document.documentElement.getAttribute("data-theme"),
      document.documentElement.getAttribute("style"),
    ]),
  };
}

export interface ControlOutcome {
  label: string;
  /** Why the interaction counted as a response; empty means nothing happened. */
  effect: string;
  error?: string;
}

/**
 * Drive one control the way a user would and report what changed.
 * A `select` changes to its next option; a text field receives a digit; a
 * button is clicked.
 */
export async function driveControl(control: ControlRef): Promise<ControlOutcome> {
  const { element } = control;
  const beforeState = observableState(element);
  let error: string | undefined;
  try {
    if (element.tagName === "SELECT") {
      const select = element as HTMLSelectElement;
      const options = [...select.options].filter((option) => !option.disabled);
      const target = options.find((option) => option.value !== select.value) ?? options[0];
      if (!target) return { label: control.label, effect: "", error: "select has no options" };
      select.value = target.value;
      fireEvent.change(select, { target: { value: target.value } });
    } else if (element.tagName === "INPUT") {
      const input = element as HTMLInputElement;
      if (input.type === "checkbox" || input.type === "radio") {
        fireEvent.click(input);
      } else if (input.type === "number") {
        const min = Number(input.min);
        const max = Number(input.max);
        const current = Number(input.value);
        const step = Number(input.step) || 1;
        let next = Number.isFinite(current) ? current + step : (Number.isFinite(min) ? min : 1);
        if (Number.isFinite(max) && next > max) next = Number.isFinite(min) ? min : max - step;
        if (Number.isFinite(min) && next < min) next = min;
        input.value = String(next);
        fireEvent.change(input, { target: { value: String(next) } });
      } else if (input.type === "color") {
        input.value = "#123456";
        fireEvent.change(input, { target: { value: "#123456" } });
      } else {
        input.value = `${input.value}x`;
        fireEvent.input(input, { target: { value: input.value } });
        fireEvent.change(input, { target: { value: input.value } });
      }
    } else if (element.tagName === "TEXTAREA") {
      const textarea = element as HTMLTextAreaElement;
      textarea.value = `${textarea.value} `;
      fireEvent.input(textarea, { target: { value: textarea.value } });
    } else {
      fireEvent.click(element);
    }
    await settle(2);
  } catch (cause) {
    error = String(cause);
  }
  let before = beforeState;
  let after = observableState(element);
  // A draft field is consumed by the action next to it; a select with a single
  // option has nothing to choose. Both are honest no-ops when clicked alone.
  const draft = element.tagName === "SELECT" || element.tagName === "INPUT" || element.tagName === "TEXTAREA";
  if (draft && sameState(before, after)) {
    const singleOption = element.tagName === "SELECT"
      && (element as HTMLSelectElement).options.length <= 1;
    if (singleOption) {
      return { label: control.label, effect: "single-option", error };
    }
    const row = element.closest(
      ".routing-section, .channel-send-add, .channel-strip, .device-control, .pianoroll-tool, label, form, .toolbar-group, .midi-monitor-row",
    );
    const submit = row?.querySelector<HTMLButtonElement>("button:not([disabled])");
    if (submit) {
      fireEvent.click(submit);
      await settle(2);
      if (!sameState(before, observableState(element))) {
        return { label: control.label, effect: "field-consumed", error };
      }
      after = observableState(element);
    } else {
      // A draft field can configure an action that is currently refused; the
      // refusal is the control's stated reason, so the field is not inert.
      const refused = row?.querySelector<HTMLButtonElement>("button[disabled]");
      const reason = row?.getAttribute("title")
        ?? refused?.getAttribute("title")
        ?? refused?.getAttribute("aria-label");
      if (refused && reason) {
        return { label: control.label, effect: "configures-disabled", error };
      }
    }
  }
  const effects: string[] = [];
  if (after.invokes > beforeState.invokes) effects.push("backend");
  if (after.dialogs > beforeState.dialogs) effects.push("dialog");
  if (after.html !== beforeState.html || after.text !== beforeState.text) effects.push("dom");
  if (after.dialogsOpen > beforeState.dialogsOpen) effects.push("dialog-open");
  if (after.layout !== beforeState.layout) effects.push("state");
  if (after.controls !== beforeState.controls) effects.push("controls");
  // Clicking the option that is already chosen is legitimately idempotent.
  if (!effects.length && isAlreadySelected(element)) effects.push("already-selected");
  if (error) effects.push("throw");
  return { label: control.label, effect: effects.join("+"), error };
}

function sameState(a: ReturnType<typeof observableState>, b: ReturnType<typeof observableState>): boolean {
  return a.invokes === b.invokes && a.dialogs === b.dialogs && a.html === b.html
    && a.text === b.text && a.dialogsOpen === b.dialogsOpen && a.layout === b.layout
    && a.controls === b.controls;
}

function isAlreadySelected(element: HTMLElement): boolean {
  return element.getAttribute("aria-pressed") === "true"
    || element.getAttribute("aria-checked") === "true"
    || element.getAttribute("aria-selected") === "true"
    || element.classList.contains("active");
}

/** A disabled control must carry a reason a user can find: title, or text nearby. */
export function disabledReason(element: HTMLElement): string | null {
  const title = element.getAttribute("title");
  if (title?.trim()) return title.trim();
  const described = element.getAttribute("aria-describedby");
  if (described) {
    const text = described
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" ")
      .trim();
    if (text) return text;
  }
  const aria = element.getAttribute("aria-label");
  if (aria && /unavailable|disabled|no |not |requires|select/i.test(aria)) return aria;
  // Hints sit beside the control, not inside it: the row, the strip or the
  // device panel that owns it. Walk up a few levels looking for one.
  const hintSelector =
    ".channel-strip-hint, .device-empty, .midi-hint, .effect-control-input-hint, .time-sig-hint";
  let node: HTMLElement | null = element;
  for (let depth = 0; depth < 4 && node; depth += 1) {
    const hint = node.querySelector?.(hintSelector)?.textContent?.trim();
    if (hint) return hint;
    node = node.parentElement;
  }
  return null;
}

/** All disabled controls inside the surface, with the reason each one gives. */
export function disabledControls(root: ParentNode = document.body): { label: string; reason: string | null }[] {
  const selector = "button[disabled], input[disabled], select[disabled], textarea[disabled], [aria-disabled='true']";
  return [...root.querySelectorAll<HTMLElement>(selector)].map((element) => ({
    label: labelOf(element),
    reason: disabledReason(element),
  }));
}

/** Unmount everything the previous control left behind, including portals. */
export function resetSurface() {
  cleanup();
  preferencesUi.close();
  document.body.innerHTML = "";
  exportAudioStore.close();
  bounceStore.close();
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

/**
 * Drive every enabled control of one surface, remounting it for each so one
 * click cannot change the controls the next click is matched against.
 * Returns one line per control that did nothing or threw.
 */
export async function sweepSurface(options: {
  /** Mount the surface, usually `() => mountSurface(() => <Surface />)`. */
  mount: () => Promise<{ container: HTMLElement }>;
  /** Reset the module state and load the fixture, run before every mount. */
  prepare: () => Promise<void>;
  /** A selector for the surface root; defaults to the whole container. */
  root?: string;
  /** Open a popover that holds the controls under test. */
  open?: (container: HTMLElement) => void;
}): Promise<string[]> {
  await options.prepare();
  const first = await options.mount();
  const scope = () => (options.root ? first.container.querySelector(options.root)! : first.container);
  const keys = controlKeys(interactiveControls(scope()));
  const failures: string[] = [];
  for (const key of keys) {
    resetSurface();
    await options.prepare();
    const view = await options.mount();
    options.open?.(view.container);
    await settle(1);
    const again = options.root ? view.container.querySelector(options.root)! : view.container;
    const controls = interactiveControls(again);
    const target = controls[controlKeys(controls).indexOf(key)] ?? controls[keys.indexOf(key)];
    if (!target) {
      failures.push(`${key}: disappeared on remount`);
      continue;
    }
    const outcome = await driveControl(target);
    if (outcome.error) failures.push(`${key}: threw ${outcome.error}`);
    else if (!outcome.effect) failures.push(`${key}: no observable effect`);
  }
  return failures;
}
